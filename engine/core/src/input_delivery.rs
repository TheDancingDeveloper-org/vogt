//! Did typed input land? The acknowledgement `session.input` gives (WI-918).
//!
//! Ports `src/vogt/core/delivery.py`. Writing bytes to a PTY always "succeeds";
//! what a driver needs to know is what the agent did with them. Judged from
//! what the engine reported before the input and from a few quick reads of the
//! screen after it — evidence, not a promise. Pure: observations are passed in.

use regex::Regex;
use std::sync::LazyLock;

/// Claude Code's hint under its input box while messages wait behind a turn,
/// and Codex's queued-message line.
static QUEUED_HINT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)queued messages?|message(?:s)? queued").expect("static"));

/// One read of the session after the input: its activity and screen.
pub struct Observation<'a> {
    pub activity: Option<&'a str>,
    pub lines: &'a [String],
}

/// What became of the input, from the activity before it and the reads after
/// it, in order.
pub fn judge(
    submitted: bool,
    before: Option<&str>,
    after: &[Observation<'_>],
) -> (&'static str, String) {
    if !submitted {
        return (
            "typed",
            "no Enter was pressed: the text is in the input, not sent".to_string(),
        );
    }
    for seen in after {
        if seen.lines.iter().any(|line| QUEUED_HINT.is_match(line)) {
            return (
                "queued",
                "the agent shows a queued-message hint: it will take the input when its current \
                 turn ends"
                    .to_string(),
            );
        }
    }
    if before == Some("running") {
        return (
            "queued",
            "a turn was already running when it was sent; agent CLIs queue input behind the \
             running turn"
                .to_string(),
        );
    }
    for seen in after {
        if seen.activity == Some("running") {
            return ("delivered", "the agent started a turn after it".to_string());
        }
        if seen.activity == Some("awaiting-approval") {
            return (
                "delivered",
                "the agent took it and is now asking for approval".to_string(),
            );
        }
    }
    (
        "unconfirmed",
        "sent, but no turn started and no queued hint showed while Vogt watched; read \
         session_screen"
            .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unsent_line_is_typed() {
        let (delivery, evidence) = judge(false, None, &[]);
        assert_eq!(delivery, "typed");
        assert!(evidence.contains("no Enter"));
    }

    #[test]
    fn a_queued_hint_wins_over_a_running_turn() {
        let lines = vec!["Press up to edit queued messages".to_string()];
        let (delivery, _) = judge(
            true,
            Some("running"),
            &[Observation {
                activity: Some("running"),
                lines: &lines,
            }],
        );
        assert_eq!(delivery, "queued");
    }

    #[test]
    fn a_turn_that_starts_is_delivered() {
        let (delivery, evidence) = judge(
            true,
            Some("idle"),
            &[Observation {
                activity: Some("running"),
                lines: &[],
            }],
        );
        assert_eq!(delivery, "delivered");
        assert!(evidence.contains("started a turn"));
    }

    #[test]
    fn nothing_seen_stays_unconfirmed() {
        let (delivery, _) = judge(true, Some("idle"), &[]);
        assert_eq!(delivery, "unconfirmed");
    }
}
