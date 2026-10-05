//! Recognising an agent CLI's permission dialog on a session's screen.
//!
//! Claude Code and Codex stop and ask before running a command (or editing a
//! file) that their permission rules do not already allow, and Claude Code
//! denies the request on its own after a countdown. To a driver reading
//! `activity`, that dialog used to look exactly like an idle prompt
//! (`waiting-for-input`), so "approve me" and "done" were indistinguishable
//! and the request timed out unanswered — and the command it asked about had
//! often scrolled off the visible screen (WI-877).
//!
//! Detection runs on the **rendered** screen, not the raw byte tail: a
//! dialog that has been answered and redrawn over is gone from the screen,
//! while its text can linger in the raw tail for a long time. The raw tail is
//! used only as a cheap prefilter ([`mentions_approval`]) so the PTY reader
//! renders a screen only when a dialog may be showing.
//!
//! A dialog is recognised by two things together: a question line agents use
//! for permission ("Do you want to proceed?", "Do you want to make this edit
//! to …?", Codex's "Would you like to run the following command?" / "Allow
//! command?") and a numbered option line below it (`❯ 1. Yes`, `› 1. Yes,
//! proceed`). Either alone is ordinary output.

use once_cell::sync::Lazy;
use regex::Regex;

/// Characters a TUI draws its boxes and rules with, stripped from each line's
/// edges before matching.
const BORDER: &[char] = &[
    '│', '┃', '║', '|', '╭', '╮', '╰', '╯', '┌', '┐', '└', '┘', ' ', '\u{a0}',
];

/// Loose prefilter over the ANSI-stripped raw tail; whitespace-insensitive
/// because a TUI may position words with cursor moves rather than spaces.
static MENTIONS: Lazy<regex::bytes::Regex> = Lazy::new(|| {
    regex::bytes::Regex::new(
        r"(?i)do\s*you\s*want\s*to|would\s*you\s*like\s*to\s*(?:run|make|apply|allow)|allow\s*command|automatically\s*den|requires\s*approval",
    )
    .expect("approval prefilter compiles")
});

/// The question line of a permission dialog, matched on a border-stripped
/// line of the rendered screen.
static QUESTION: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?i)^(?:do you want to (?:proceed|make this edit|make these edits|create|run|allow|delete|overwrite|fetch|use|write|execute|apply)\b.*\?|would you like to (?:run|make|apply|allow)\b.*\?|allow (?:command|this command|edit|tool)\b.*\??|.*\brequires approval\b.*)$",
    )
    .expect("approval question regex compiles")
});

/// A numbered menu option, with or without the selection glyph.
static OPTION: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^(?:[❯›>▸]\s*)?1\.\s+\S").expect("option regex compiles"));

/// "…automatically deny this request in 90s", "auto-deny in 1m".
static DEADLINE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?i)auto(?:matically)?[\s-]*(?:den(?:y|ied|ies)|reject\w*)\D{0,60}?(\d{1,5})\s*(seconds?|secs?|s|minutes?|mins?|m)\b",
    )
    .expect("deadline regex compiles")
});

/// At most this many lines above the question are read for the command.
const MAX_CONTEXT_LINES: usize = 40;
/// The excerpt is cut at this many characters.
const MAX_EXCERPT_CHARS: usize = 4000;

/// A permission dialog read off the screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detected {
    /// The dialog's question line, as shown.
    pub question: String,
    /// What it asks about — the command, edit or tool call — with borders
    /// stripped, from the screen and the scrollback above it.
    pub command_excerpt: String,
    /// Seconds until the CLI answers "no" by itself, when it shows a
    /// countdown.
    pub deadline_seconds: Option<u32>,
}

/// How many scrollback lines are rendered above the screen when reading a
/// dialog, so a command taller than the screen is still read whole.
pub const SCROLLBACK_CONTEXT_LINES: usize = 200;

/// A dialog as a session holds it: what was detected, and when it was first
/// seen, so its countdown is anchored to the first sighting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pending {
    pub detected: Detected,
    pub detected_at: time::OffsetDateTime,
    pub deadline_at: Option<time::OffsetDateTime>,
}

impl Pending {
    pub fn new(detected: Detected, now: time::OffsetDateTime) -> Self {
        let deadline_at = detected
            .deadline_seconds
            .map(|s| now + time::Duration::seconds(i64::from(s)));
        Self {
            detected,
            detected_at: now,
            deadline_at,
        }
    }

    /// Whether `other` is this same dialog, re-read (its countdown moved).
    pub fn is_same(&self, other: &Detected) -> bool {
        self.detected.question == other.question
            && self.detected.command_excerpt == other.command_excerpt
    }

    pub fn to_wire(&self, now: time::OffsetDateTime) -> vogt_engine_contract::ApprovalPrompt {
        let deadline_seconds = self
            .deadline_at
            .map(|at| (at - now).whole_seconds().clamp(0, i64::from(u32::MAX)) as u32);
        vogt_engine_contract::ApprovalPrompt {
            question: self.detected.question.clone(),
            command_excerpt: self.detected.command_excerpt.clone(),
            deadline_seconds,
            deadline_at: self.deadline_at.map(rfc3339),
            detected_at: rfc3339(self.detected_at),
        }
    }
}

fn rfc3339(ts: time::OffsetDateTime) -> String {
    ts.format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| ts.to_string())
}

/// Whether an ANSI-stripped tail may contain a permission dialog. Cheap; a
/// `true` only earns a screen render.
pub fn mentions_approval(stripped_tail: &[u8]) -> bool {
    MENTIONS.is_match(stripped_tail)
}

fn strip(line: &str) -> &str {
    line.trim_matches(BORDER)
}

fn is_rule(line: &str) -> bool {
    let t = line.trim();
    t.chars().count() >= 3
        && t.chars()
            .all(|c| matches!(c, '─' | '━' | '═' | '-' | '╌' | '┄' | '╭' | '╮' | '┌' | '┐'))
}

/// Find a live permission dialog on the screen.
///
/// `visible` is the screen top to bottom; `scrollback` is the history above
/// it, oldest first (may be empty). The question must be on the visible
/// screen, with a numbered option within the next eight lines; the command
/// excerpt may reach up into the scrollback.
pub fn detect(visible: &[String], scrollback: &[String]) -> Option<Detected> {
    let q_visible = visible
        .iter()
        .rposition(|line| QUESTION.is_match(strip(line).trim()))?;
    let after = &visible[q_visible + 1..];
    let opt_rel = after
        .iter()
        .take(8)
        .position(|line| OPTION.is_match(strip(line).trim()))?;

    let all: Vec<&str> = scrollback
        .iter()
        .chain(visible.iter())
        .map(String::as_str)
        .collect();
    let q = scrollback.len() + q_visible;
    let opt = q + 1 + opt_rel;
    let question = strip(all[q]).trim().to_string();

    // Codex draws what it asks about *between* the question and the options;
    // Claude Code draws it *above* the question, under the dialog's title.
    let between: Vec<&str> = all[q + 1..opt]
        .iter()
        .map(|l| strip(l).trim_end())
        .filter(|l| !l.trim().is_empty())
        .collect();
    let body: Vec<&str> = if !between.is_empty() {
        between
    } else {
        let mut start = q;
        while start > 0 && q - start < MAX_CONTEXT_LINES && !is_rule(all[start - 1]) {
            start -= 1;
        }
        all[start..q]
            .iter()
            .map(|l| strip(l).trim_end())
            .filter(|l| !l.trim().is_empty())
            .collect()
    };
    let mut command_excerpt = body.join("\n");
    if command_excerpt.chars().count() > MAX_EXCERPT_CHARS {
        command_excerpt = command_excerpt.chars().take(MAX_EXCERPT_CHARS).collect();
        command_excerpt.push('…');
    }

    let deadline_seconds = all[q.saturating_sub(MAX_CONTEXT_LINES)..]
        .iter()
        .rev()
        .find_map(|line| DEADLINE.captures(line))
        .and_then(|c| {
            let n: u32 = c.get(1)?.as_str().parse().ok()?;
            let unit = c.get(2)?.as_str().to_ascii_lowercase();
            Some(if unit.starts_with('m') {
                n.saturating_mul(60)
            } else {
                n
            })
        });

    Some(Detected {
        question,
        command_excerpt,
        deadline_seconds,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &str) -> Vec<String> {
        text.lines().map(str::to_string).collect()
    }

    #[test]
    fn claude_code_bash_dialog_with_a_countdown() {
        let screen = lines(
            "● I'll clean up the containers.\n\
             ╭──────────────────────────────────────────────╮\n\
             │ Bash command                                 │\n\
             │                                              │\n\
             │   sh -c 'docker stop x && docker rm -f x'    │\n\
             │   Stop and remove the test container         │\n\
             │                                              │\n\
             │ Do you want to proceed?                      │\n\
             │ ❯ 1. Yes                                     │\n\
             │   2. Yes, and don't ask again for sh         │\n\
             │   3. No, and tell Claude what to do (esc)    │\n\
             ╰──────────────────────────────────────────────╯\n\
             Claude will automatically deny this request in 87s",
        );
        let d = detect(&screen, &[]).expect("dialog detected");
        assert_eq!(d.question, "Do you want to proceed?");
        assert!(d.command_excerpt.contains("docker rm -f x"), "{d:?}");
        assert!(d.command_excerpt.starts_with("Bash command"), "{d:?}");
        assert!(!d.command_excerpt.contains("clean up"), "{d:?}");
        assert_eq!(d.deadline_seconds, Some(87));
    }

    #[test]
    fn the_command_can_reach_up_into_the_scrollback() {
        let scrollback = lines(
            "older output\n\
             ────────────────────────\n\
             Bash command\n\
             \n\
             rm -rf /tmp/build-cache &&",
        );
        let screen = lines(
            "  docker system prune -f\n\
             \n\
             Do you want to proceed?\n\
             ❯ 1. Yes\n\
             2. No",
        );
        let d = detect(&screen, &scrollback).expect("dialog detected");
        assert!(
            d.command_excerpt.contains("rm -rf /tmp/build-cache"),
            "{d:?}"
        );
        assert!(
            d.command_excerpt.contains("docker system prune -f"),
            "{d:?}"
        );
        assert!(!d.command_excerpt.contains("older output"), "{d:?}");
        assert_eq!(d.deadline_seconds, None);
    }

    #[test]
    fn codex_exec_approval() {
        let screen = lines(
            "Would you like to run the following command?\n\
             \n\
             Reason: clean the build directory\n\
             \n\
             $ rm -rf target\n\
             \n\
             › 1. Yes, proceed (y)\n\
               2. Yes, and don't ask again for this command (a)\n\
               3. No, and tell Codex what to do differently (esc)",
        );
        let d = detect(&screen, &[]).expect("dialog detected");
        assert_eq!(d.question, "Would you like to run the following command?");
        assert!(d.command_excerpt.contains("$ rm -rf target"), "{d:?}");
        assert!(d.command_excerpt.contains("Reason:"), "{d:?}");
    }

    #[test]
    fn edit_dialog_and_minute_deadlines() {
        let screen = lines(
            "Edit file src/main.rs\n\
             Do you want to make this edit to main.rs?\n\
             ❯ 1. Yes\n\
               2. No\n\
             auto-deny in 2m",
        );
        let d = detect(&screen, &[]).expect("dialog detected");
        assert_eq!(d.deadline_seconds, Some(120));
    }

    #[test]
    fn a_question_without_a_menu_or_a_menu_without_a_question_is_not_a_dialog() {
        // Prose that asks the question, with no menu under it.
        assert_eq!(
            detect(&lines("Do you want to proceed? I can wait.\n> "), &[]),
            None
        );
        // An ordinary numbered list.
        assert_eq!(detect(&lines("Steps:\n1. build\n2. test\n> "), &[]), None);
        // An idle Claude prompt.
        assert_eq!(detect(&lines("● Done.\n╭────╮\n│ >  │\n╰────╯"), &[]), None);
    }

    #[test]
    fn a_pending_dialog_counts_down_from_its_first_sighting() {
        let found = Detected {
            question: "Do you want to proceed?".into(),
            command_excerpt: "rm -rf x".into(),
            deadline_seconds: Some(90),
        };
        let seen = time::OffsetDateTime::UNIX_EPOCH;
        let pending = Pending::new(found.clone(), seen);
        let later = pending.to_wire(seen + time::Duration::seconds(30));
        assert_eq!(later.deadline_seconds, Some(60));
        assert!(later.deadline_at.is_some());
        let gone = pending.to_wire(seen + time::Duration::seconds(300));
        assert_eq!(gone.deadline_seconds, Some(0));
        // The countdown ticking is the same dialog; a different command is not.
        let ticked = Detected {
            deadline_seconds: Some(80),
            ..found.clone()
        };
        assert!(pending.is_same(&ticked));
        let other = Detected {
            command_excerpt: "ls".into(),
            ..found
        };
        assert!(!pending.is_same(&other));
    }

    #[test]
    fn the_prefilter_is_whitespace_insensitive() {
        assert!(mentions_approval(b"Doyouwanttoproceed?"));
        assert!(mentions_approval(b"will automatically deny this request"));
        assert!(!mentions_approval(b"compiling vogt v0.7.4"));
    }
}
