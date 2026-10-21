//! Did typed input land? Ports `src/vogt/core/delivery.py`.
//!
//! Judged from what the engine reported before the input and a few reads of
//! the screen after it. Evidence, not a promise.

#![allow(dead_code)]

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    Typed,
    Delivered,
    Queued,
    Wedged,
    Unconfirmed,
}

impl Delivery {
    /// The wire value, matching `delivery.py`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Typed => "typed",
            Self::Delivered => "delivered",
            Self::Queued => "queued",
            Self::Wedged => "wedged",
            Self::Unconfirmed => "unconfirmed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observation {
    pub activity: Option<String>,
    pub lines: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    pub delivery: Delivery,
    pub evidence: String,
}

use std::sync::LazyLock;

static QUEUED_HINT: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"(?i)queued messages?|message(?:s)? queued").unwrap());

/// Codex's prompt marker on its composer and on each echoed message.
pub const CODEX_PROMPT: &str = "\u{203a}";

/// Codex's footer row: two-space indent, then text with a " · " in it.
static CODEX_FOOTER: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^\s{2}\S.* \x{00b7} \S").unwrap());

/// How much of a message has to match the shown text.
const COMPOSER_MATCH: usize = 40;

/// Codex's marker for an error it shows in its transcript
/// (`■ unexpected status 422 Unprocessable Entity: Budget has been
/// exceeded! ...`), captured live on 2026-10-20 (WI-2027).
pub const CODEX_ERROR: &str = "\u{25a0}";

/// How a wedged session is recovered without losing its worktree or its
/// conversation: hibernation stops the CLI and waking resumes it.
pub const RESTART_HINT: &str = "restart it keeping its worktree and conversation: \
                                session_hibernate, then session_wake (or send the input \
                                again, which wakes it)";

/// Codex's footer row: the upper of the last two lines, as the hint line
/// under it can carry a " · " too ("⚠ 1 warning · f2 to view").
fn footer_index(rows: &[&str]) -> Option<usize> {
    (rows.len().saturating_sub(2)..rows.len()).find(|&i| CODEX_FOOTER.is_match(rows[i]))
}

/// Whitespace collapsed to single spaces, cut to `n` characters.
fn squashed(text: &str, n: usize) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(n)
        .collect()
}

/// The API error a Codex session is wedged behind, or None (WI-2027).
///
/// After a turn ends in a model-API error (a 422 for an exceeded budget),
/// Codex can echo each new message into its transcript as `› text` and
/// then start no turn: no `•` working line ever follows. The sign is the
/// last `■` error line with only message echoes after it, above the
/// composer. With `text`, one of those echoes has to be it.
pub fn codex_error_wedge(lines: &[String], text: Option<&str>) -> Option<String> {
    let mut rows: Vec<&str> = lines.iter().map(String::as_str).collect();
    while rows.last().is_some_and(|row| row.trim().is_empty()) {
        rows.pop();
    }
    let footer = footer_index(&rows)?;
    // Drop the composer: the prompt line above the footer and its wrap.
    let mut i = footer as isize - 1;
    while i >= 0 && rows[i as usize].trim().is_empty() {
        i -= 1;
    }
    while i >= 0 && rows[i as usize].starts_with("  ") && !rows[i as usize].trim().is_empty() {
        i -= 1;
    }
    if i < 0 || !rows[i as usize].starts_with(CODEX_PROMPT) {
        return None;
    }
    let body = &rows[..i as usize];
    let error = body.iter().rposition(|row| row.starts_with(CODEX_ERROR))?;
    let mut echoes: Vec<String> = Vec::new();
    for row in &body[error + 1..] {
        if let Some(echo) = row.strip_prefix(CODEX_PROMPT) {
            echoes.push(echo.trim().to_string());
        } else if !row.trim().is_empty() && !row.starts_with("  ") {
            // Anything else Codex drew (a `•` step, a new error): it moved.
            return None;
        } else if row.starts_with("  ") {
            if let Some(last) = echoes.last_mut() {
                *last = format!("{last} {}", row.trim());
            }
        }
    }
    if echoes.is_empty() {
        return None;
    }
    if let Some(text) = text {
        let wanted = squashed(text, COMPOSER_MATCH);
        let n = wanted.chars().count();
        if !echoes.iter().any(|echo| squashed(echo, n) == wanted) {
            return None;
        }
    }
    Some(
        body[error]
            .trim_start_matches(CODEX_ERROR)
            .trim()
            .to_string(),
    )
}

pub fn judge(
    submitted: bool,
    before: Option<&str>,
    after: &[Observation],
    pending: Option<&str>,
) -> Verdict {
    if !submitted {
        return Verdict {
            delivery: Delivery::Typed,
            evidence: "no Enter was pressed: the text is in the input, not sent".to_string(),
        };
    }
    if after
        .iter()
        .any(|seen| seen.lines.iter().any(|line| QUEUED_HINT.is_match(line)))
    {
        return Verdict {
            delivery: Delivery::Queued,
            evidence: "the agent shows a queued-message hint: it will take the input when its \
                       current turn ends"
                .to_string(),
        };
    }
    if let Some(last) = after.last() {
        if !matches!(
            last.activity.as_deref(),
            Some("running") | Some("awaiting-approval")
        ) {
            if let Some(error) = codex_error_wedge(&last.lines, pending) {
                let error: String = error.chars().take(120).collect();
                return Verdict {
                    delivery: Delivery::Wedged,
                    evidence: format!(
                        "Codex echoed the input but started no turn after an API error \
                         ({error}); it will not take input again — {RESTART_HINT}"
                    ),
                };
            }
        }
    }
    if before == Some("running") {
        return Verdict {
            delivery: Delivery::Queued,
            evidence: "a turn was already running when it was sent; agent CLIs queue input behind \
                       the running turn"
                .to_string(),
        };
    }
    for seen in after {
        if seen.activity.as_deref() == Some("running") {
            return Verdict {
                delivery: Delivery::Delivered,
                evidence: "the agent started a turn after it".to_string(),
            };
        }
        if seen.activity.as_deref() == Some("awaiting-approval") {
            return Verdict {
                delivery: Delivery::Delivered,
                evidence: "the agent took it and is now asking for approval".to_string(),
            };
        }
    }
    Verdict {
        delivery: Delivery::Unconfirmed,
        evidence: "sent, but no turn started and no queued hint showed while Vogt watched; read \
                   session_screen"
            .to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seen(activity: &str, line: &str) -> Observation {
        Observation {
            activity: (!activity.is_empty()).then(|| activity.to_string()),
            lines: vec![line.to_string()],
        }
    }

    #[test]
    fn the_four_verdicts() {
        assert_eq!(judge(false, None, &[], None).delivery, Delivery::Typed);
        assert_eq!(
            judge(
                true,
                Some("idle"),
                &[seen("", "Press up to edit queued messages")],
                None
            )
            .delivery,
            Delivery::Queued
        );
        assert_eq!(
            judge(true, Some("running"), &[], None).delivery,
            Delivery::Queued
        );
        assert_eq!(
            judge(true, Some("idle"), &[seen("running", "")], None).delivery,
            Delivery::Delivered
        );
        assert_eq!(
            judge(true, Some("idle"), &[seen("awaiting-approval", "")], None).delivery,
            Delivery::Delivered
        );
        assert_eq!(
            judge(true, Some("idle"), &[], None).delivery,
            Delivery::Unconfirmed
        );
    }

    /// A Codex screen after two model-API 422s, as the session log of
    /// 2026-10-20 shows it (WI-2027): each message is echoed under the error
    /// and no `•` step ever follows.
    const ERR: &str = "\u{25a0} unexpected status 422 Unprocessable Entity: Budget has been \
                       exceeded! Key=vogt-sessions";
    const NUDGE: &str = "The LiteLLM key budget has been removed. Continue WI-1262.";

    fn wedged_screen() -> Vec<String> {
        let p = CODEX_PROMPT;
        vec![
            "\u{2022} Now the frontend adoption fix.".to_string(),
            ERR.to_string(),
            String::new(),
            format!("{p} Autopilot: carry on with the next unblocked item in scope."),
            String::new(),
            ERR.to_string(),
            String::new(),
            format!("{p} {NUDGE}"),
            String::new(),
            format!("{p} Ask Codex to do anything"),
            String::new(),
            "  deepseek-flash \u{00b7} ~/Working/Active/WI-1262".to_string(),
            "  ? for shortcuts".to_string(),
        ]
    }

    #[test]
    fn an_echo_after_an_api_error_with_no_turn_is_wedged() {
        let screen = wedged_screen();
        assert!(codex_error_wedge(&screen, Some(NUDGE)).is_some());
        assert!(codex_error_wedge(&screen, None).is_some(), "an Enter alone");
        assert!(codex_error_wedge(&screen, Some("something else")).is_none());
        let seen = Observation {
            activity: Some("waiting-for-input".to_string()),
            lines: screen,
        };
        let found = judge(true, Some("waiting-for-input"), &[seen], Some(NUDGE));
        assert_eq!(found.delivery, Delivery::Wedged);
        assert!(found.evidence.contains("session_hibernate"));
        assert!(found.evidence.contains("422"));
    }

    #[test]
    fn an_error_that_codex_moved_past_is_not_a_wedge() {
        let screen = wedged_screen();
        let mut moved = screen[..8].to_vec();
        moved.push("\u{2022} Explored".to_string());
        moved.extend_from_slice(&screen[8..]);
        assert!(codex_error_wedge(&moved, None).is_none());
        let mut bare = vec![ERR.to_string(), String::new()];
        bare.extend_from_slice(&screen[9..]);
        assert!(
            codex_error_wedge(&bare, None).is_none(),
            "an error with nothing sent since"
        );
        let running = Observation {
            activity: Some("running".to_string()),
            lines: screen,
        };
        let found = judge(true, Some("waiting-for-input"), &[running], None);
        assert_eq!(found.delivery, Delivery::Delivered);
    }
}
