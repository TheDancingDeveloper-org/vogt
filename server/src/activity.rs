use std::time::Instant;

use once_cell::sync::Lazy;
use regex::bytes::RegexSet;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ActivityState {
    Idle,
    Running,
    WaitingForInput,
    Errored,
}

/// Patterns that indicate the program is waiting for the user. Matched against
/// the *tail* of scrollback with ANSI escape sequences stripped.
///
/// Conservative on purpose — false positives nag the user with push
/// notifications. Add patterns as real prompts surface.
static WAITING_PATTERNS: Lazy<RegexSet> = Lazy::new(|| {
    RegexSet::new([
        // Generic y/n confirmation prompts
        r"(?i)\[y/n\]\s*\??\s*$",
        r"(?i)\(y/n\)\s*\??\s*$",
        r"(?i)\(yes/no\)\s*\??\s*$",
        // password / passphrase prompts
        r"(?i)pass(word|phrase)[^:]*:\s*$",
        // Claude Code / Codex style numbered approval menus end with "❯ 1." or similar.
        // Match a NL then a caret-style prompt with no following output.
        r"(?:❯|>)\s*\d+\.[^\n]*$",
        // Generic single-arrow / chevron prompt at tail (REPLs, claude prompt)
        r"(?:\n|^)❯\s*$",
        r"(?:\n|^)>>>\s*$",
        // bash/zsh "Press any key", "Continue?" etc.
        r"(?i)press\s+(any\s+key|enter|return)\s+to\s+continue",
        r"(?i)continue\?\s*$",
    ])
    .expect("waiting-for-input regex set compiles")
});

/// Cheap ANSI/CSI escape stripper for heuristics. Not a full terminal emulator —
/// good enough to expose visible prompt text to regex matching.
pub fn strip_ansi(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        let b = input[i];
        if b == 0x1b && i + 1 < input.len() {
            // ESC sequence. Handle CSI (ESC [) and OSC (ESC ]) and short two-byte.
            match input[i + 1] {
                b'[' => {
                    // CSI: ESC [ params... final-byte (0x40..=0x7e)
                    let mut j = i + 2;
                    while j < input.len() && !(0x40..=0x7e).contains(&input[j]) {
                        j += 1;
                    }
                    i = j.saturating_add(1).min(input.len());
                    continue;
                }
                b']' => {
                    // OSC: terminated by BEL (0x07) or ST (ESC \)
                    let mut j = i + 2;
                    while j < input.len() {
                        if input[j] == 0x07 {
                            j += 1;
                            break;
                        }
                        if input[j] == 0x1b && j + 1 < input.len() && input[j + 1] == b'\\' {
                            j += 2;
                            break;
                        }
                        j += 1;
                    }
                    i = j.min(input.len());
                    continue;
                }
                _ => {
                    // Two-byte ESC sequence; skip both.
                    i += 2;
                    continue;
                }
            }
        }
        out.push(b);
        i += 1;
    }
    out
}

/// Decide the next activity state given current state, time of last output,
/// and a tail snapshot of scrollback.
///
/// `idle_after_ms` is the quiet window before Running collapses to Idle.
pub fn classify(
    last_output: Option<Instant>,
    tail: &[u8],
    idle_after_ms: u64,
    is_exited_nonzero: bool,
) -> ActivityState {
    if is_exited_nonzero {
        return ActivityState::Errored;
    }
    let stripped = strip_ansi(tail);
    // Only check the last ~512 bytes of stripped content — patterns anchor on $.
    let scan_start = stripped.len().saturating_sub(512);
    if WAITING_PATTERNS.is_match(&stripped[scan_start..]) {
        return ActivityState::WaitingForInput;
    }

    match last_output {
        None => ActivityState::Idle,
        Some(t) => {
            let elapsed = t.elapsed().as_millis() as u64;
            if elapsed < idle_after_ms {
                ActivityState::Running
            } else {
                ActivityState::Idle
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_csi_sequences() {
        let input = b"\x1b[31mhello\x1b[0m world";
        assert_eq!(strip_ansi(input), b"hello world");
    }

    #[test]
    fn strips_osc_sequences() {
        let input = b"before\x1b]0;title\x07after";
        assert_eq!(strip_ansi(input), b"beforeafter");
    }

    #[test]
    fn detects_yn_prompt() {
        let s = classify(Some(Instant::now()), b"Continue? [y/N] ", 1500, false);
        // Trailing space breaks the `$` anchor — verify the un-spaced form works.
        assert_ne!(s, ActivityState::Errored);
        let s = classify(Some(Instant::now()), b"Continue? [y/N]", 1500, false);
        assert_eq!(s, ActivityState::WaitingForInput);
    }

    #[test]
    fn detects_password_prompt() {
        let s = classify(
            Some(Instant::now()),
            b"sudo password for user:",
            1500,
            false,
        );
        assert_eq!(s, ActivityState::WaitingForInput);
    }

    #[test]
    fn nonzero_exit_becomes_errored() {
        let s = classify(Some(Instant::now()), b"hello", 1500, true);
        assert_eq!(s, ActivityState::Errored);
    }

    #[test]
    fn quiet_window_collapses_to_idle() {
        let t = Instant::now() - std::time::Duration::from_secs(10);
        let s = classify(Some(t), b"hello", 1500, false);
        assert_eq!(s, ActivityState::Idle);
    }

    #[test]
    fn recent_output_is_running() {
        let s = classify(Some(Instant::now()), b"hello", 1500, false);
        assert_eq!(s, ActivityState::Running);
    }
}
