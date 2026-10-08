use std::time::Instant;

use once_cell::sync::Lazy;
use regex::bytes::RegexSet;
pub use vogt_engine_contract::ActivityState;

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

/// Phrases indicating a transient, retryable failure (rate limiting or
/// upstream overload) rather than a real error. Matched anywhere in the
/// scanned tail, not anchored — these show up mid-line in API error bodies.
static RATE_LIMIT_PATTERNS: Lazy<RegexSet> = Lazy::new(|| {
    RegexSet::new([
        r"(?i)\b429\b",
        r"(?i)rate.?limit",
        r"(?i)\boverloaded\b",
        r"(?i)too many requests",
    ])
    .expect("rate-limit regex set compiles")
});

/// True if the (ANSI-stripped) tail contains a transient-failure phrase such
/// as a 429 / rate-limit / overload message.
pub fn is_rate_limited(stripped_tail: &[u8]) -> bool {
    RATE_LIMIT_PATTERNS.is_match(stripped_tail)
}

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

/// When a session's output last changed what its screen shows (WI-1090).
///
/// Output recency is how `classify` tells a turn at work from one at rest,
/// and output that repaints an identical frame fooled it: Klaudia's idle
/// input box redraws its blinking cursor twice a second, so an idle Klaudia
/// read `running` for ten hours and was never nudged. This clock moves only
/// when the rendered screen (its text and title, see
/// `screen::Terminal::fingerprint`) differs from the last time it was looked
/// at, so an identical repaint counts the same as silence.
///
/// Fingerprinting reads the whole grid, so it is not done for every chunk:
/// output within `interval` of the last change is only noted as pending, and
/// is settled the next time the clock is read or a chunk arrives after the
/// interval. A pending change is stamped with the time of the latest output,
/// so the clock is never early and is late by at most about one interval.
#[derive(Debug, Default)]
pub struct ChangeClock {
    fingerprint: Option<u64>,
    pending: Option<Instant>,
    changed: Option<Instant>,
}

impl ChangeClock {
    /// Note output at `at`. True when it should be settled now, because the
    /// last change is at least `interval` old (or there was none).
    pub fn output(&mut self, at: Instant, interval: std::time::Duration) -> bool {
        self.pending = Some(at);
        !self
            .changed
            .is_some_and(|changed| at.saturating_duration_since(changed) < interval)
    }

    /// When the screen last changed, as far as has been settled. Output still
    /// pending is newer than this, and no older than one interval after it.
    pub fn changed(&self) -> Option<Instant> {
        self.changed
    }

    /// Compare pending output against the screen, and return when it last
    /// changed. `fingerprint` is called only when output is pending; `None`
    /// (no grid to look at) counts every output as a change.
    pub fn settle(&mut self, fingerprint: impl FnOnce() -> Option<u64>) -> Option<Instant> {
        if let Some(at) = self.pending.take() {
            match fingerprint() {
                Some(now) if self.fingerprint == Some(now) => {}
                now => {
                    self.fingerprint = now;
                    self.changed = Some(at);
                }
            }
        }
        self.changed
    }
}

/// The terminal state an exit code maps to: `exited` for 0, `errored` for
/// anything else. `None` while the child is still running.
pub fn exit_state(exit_code: Option<i32>) -> Option<ActivityState> {
    match exit_code {
        None => None,
        Some(0) => Some(ActivityState::Exited),
        Some(_) => Some(ActivityState::Errored),
    }
}

/// The terminal state of an exited child, knowing whether a stop was asked
/// for (WI-913): a requested stop is `stopped` whatever the code — killing
/// an agent makes it exit non-zero or by signal, and that is the request
/// working, not a crash. A signal from anywhere else (the OOM killer, a
/// person's `kill -9` outside the engine) stays `errored`: it can be a real
/// failure, and nothing recorded says otherwise.
pub fn terminal_state(exit_code: Option<i32>, stop_requested: bool) -> Option<ActivityState> {
    match exit_state(exit_code) {
        Some(_) if stop_requested => Some(ActivityState::Stopped),
        other => other,
    }
}

/// Decide the next activity state given time of last output, a tail snapshot
/// of scrollback, and the child's exit code if it has exited.
///
/// `idle_after_ms` is the quiet window before Running collapses to Idle. An
/// exited child is always `exited`/`errored`, whatever the tail says.
pub fn classify(
    last_output: Option<Instant>,
    tail: &[u8],
    idle_after_ms: u64,
    exit_code: Option<i32>,
) -> ActivityState {
    if let Some(state) = exit_state(exit_code) {
        return state;
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
        let s = classify(Some(Instant::now()), b"Continue? [y/N] ", 1500, None);
        // Trailing space breaks the `$` anchor — verify the un-spaced form works.
        assert_ne!(s, ActivityState::Errored);
        let s = classify(Some(Instant::now()), b"Continue? [y/N]", 1500, None);
        assert_eq!(s, ActivityState::WaitingForInput);
    }

    #[test]
    fn detects_password_prompt() {
        let s = classify(Some(Instant::now()), b"sudo password for user:", 1500, None);
        assert_eq!(s, ActivityState::WaitingForInput);
    }

    #[test]
    fn nonzero_exit_becomes_errored() {
        let s = classify(Some(Instant::now()), b"hello", 1500, Some(1));
        assert_eq!(s, ActivityState::Errored);
    }

    #[test]
    fn zero_exit_is_exited_even_with_a_prompt_in_the_tail() {
        let s = classify(Some(Instant::now()), b"Continue? [y/N]", 1500, Some(0));
        assert_eq!(s, ActivityState::Exited);
        assert_eq!(exit_state(None), None);
        // WI-913: a requested stop is `stopped` whatever the code; anything
        // else non-zero stays `errored`, and a live child has no terminal state.
        assert_eq!(
            terminal_state(Some(137), true),
            Some(ActivityState::Stopped)
        );
        assert_eq!(terminal_state(Some(0), true), Some(ActivityState::Stopped));
        assert_eq!(
            terminal_state(Some(137), false),
            Some(ActivityState::Errored)
        );
        assert_eq!(terminal_state(None, true), None);
        assert!(ActivityState::Stopped.is_terminal());
    }

    #[test]
    fn quiet_window_collapses_to_idle() {
        let t = Instant::now() - std::time::Duration::from_secs(10);
        let s = classify(Some(t), b"hello", 1500, None);
        assert_eq!(s, ActivityState::Idle);
    }

    #[test]
    fn detects_rate_limit_phrases() {
        assert!(is_rate_limited(b"Error: 429 Too Many Requests"));
        assert!(is_rate_limited(
            b"upstream error: rate_limited, retry later"
        ));
        assert!(is_rate_limited(b"model overloaded, please retry"));
        assert!(!is_rate_limited(b"hello world"));
    }

    #[test]
    fn recent_output_is_running() {
        let s = classify(Some(Instant::now()), b"hello", 1500, None);
        assert_eq!(s, ActivityState::Running);
    }
}

#[cfg(test)]
mod change_clock_tests {
    use super::ChangeClock;
    use std::time::{Duration, Instant};

    #[test]
    fn only_output_that_changes_the_screen_moves_the_clock() {
        let interval = Duration::from_millis(100);
        let t0 = Instant::now();
        let at = |ms| t0 + Duration::from_millis(ms);
        let mut clock = ChangeClock::default();
        assert_eq!(clock.settle(|| unreachable!("nothing pending")), None);

        // The first frame is a change.
        assert!(clock.output(at(0), interval));
        assert_eq!(clock.settle(|| Some(1)), Some(at(0)));
        // The same frame repainted, however often, is not.
        for ms in [500, 1000, 1500] {
            assert!(clock.output(at(ms), interval));
            assert_eq!(clock.settle(|| Some(1)), Some(at(0)));
        }
        // A different one is, stamped when it arrived.
        assert!(clock.output(at(2000), interval));
        assert_eq!(clock.settle(|| Some(2)), Some(at(2000)));
        // Output right after a change is only noted; settling later stamps
        // it with the latest output, never earlier.
        assert!(!clock.output(at(2050), interval));
        assert!(!clock.output(at(2080), interval));
        assert_eq!(clock.changed(), Some(at(2000)));
        assert_eq!(clock.settle(|| Some(3)), Some(at(2080)));
        // With no grid to look at, every output counts.
        assert!(clock.output(at(3000), interval));
        assert_eq!(clock.settle(|| None), Some(at(3000)));
        assert!(clock.output(at(4000), interval));
        assert_eq!(clock.settle(|| None), Some(at(4000)));
    }
}
