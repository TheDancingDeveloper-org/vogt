//! What a session overseer should look at first (WI-915).
//!
//! Ports `src/vogt/core/oversight.py`. A driver overseeing many sessions wants
//! one table ordered by who needs it, and the reason in words — never a bare
//! rank. Pure: the clock is a parameter, and nothing here reads storage or the
//! engine.

use crate::core::Moment;

/// Lower first. `stalled` sits above `running` because a turn that has printed
/// nothing for a long time is worth a look before one that is busy.
pub fn order(attention: &str) -> u8 {
    match attention {
        "approval" => 0,
        "blocked" => 1,
        "waiting" => 2,
        "stalled" => 3,
        "running" => 4,
        "idle" => 5,
        "hibernated" => 6,
        "exited" => 7,
        _ => 8,
    }
}

/// The attention classes a person (or a driver acting for one) must act on.
pub fn needs_you(attention: &str) -> bool {
    matches!(attention, "approval" | "blocked" | "waiting")
}

/// Startup gates an agent CLI stops at before any work (WI-917), as words.
fn gate(kind: &str) -> Option<&'static str> {
    match kind {
        "folder-trust" => Some("folder trust"),
        "external-imports" => Some("external CLAUDE.md imports"),
        "read-outside-cwd" => Some("read outside the working directory"),
        _ => None,
    }
}

/// Where one session belongs in the oversight table, and why.
#[allow(clippy::too_many_arguments)]
pub fn classify(
    activity: Option<&str>,
    alive: Option<bool>,
    ready: Option<bool>,
    approval_question: Option<&str>,
    blocker: Option<&str>,
    approval_kind: Option<&str>,
    last_output_at: Option<Moment>,
    now: Moment,
    stall_after_seconds: i64,
) -> (&'static str, String) {
    if activity == Some("hibernated") {
        return (
            "hibernated",
            "hibernated to free memory; wake it to continue".to_string(),
        );
    }
    if activity.is_none() || alive.is_none() {
        return ("unknown", "the engine could not be asked".to_string());
    }
    if activity == Some("stopped") {
        return ("exited", "stopped on request".to_string());
    }
    if alive == Some(false) {
        return (
            "exited",
            format!("its process ended ({})", activity.unwrap_or("")),
        );
    }
    if approval_question.is_some() || activity == Some("awaiting-approval") {
        if let (Some(gate), Some(question)) = (gate(approval_kind.unwrap_or("")), approval_question)
        {
            return (
                "approval",
                format!("stopped at a startup gate ({gate}): {question}"),
            );
        }
        return (
            "approval",
            match approval_question {
                Some(question) => format!("asking for approval: {question}"),
                None => "showing a permission dialog".to_string(),
            },
        );
    }
    if let Some(blocker) = blocker {
        return ("blocked", format!("blocked on a person: {blocker}"));
    }
    if activity == Some("waiting-for-input") || (activity == Some("idle") && ready == Some(true)) {
        return (
            "waiting",
            "at its prompt, waiting for the next instruction".to_string(),
        );
    }
    if activity == Some("running") {
        if let Some(last) = last_output_at {
            let quiet = now.seconds_since(last) as i64;
            if quiet >= stall_after_seconds {
                return (
                    "stalled",
                    format!("running, but nothing printed for {} min", quiet / 60),
                );
            }
        }
        return ("running", "working".to_string());
    }
    ("idle", "resting, not at a recognised prompt".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(seconds: i64) -> Moment {
        Moment::from_unix(seconds, 0)
    }

    #[test]
    fn a_permission_dialog_outranks_everything() {
        let (attention, reason) = classify(
            Some("awaiting-approval"),
            Some(true),
            Some(false),
            Some("run cargo test?"),
            None,
            None,
            None,
            at(0),
            600,
        );
        assert_eq!(attention, "approval");
        assert!(reason.contains("run cargo test?"));
        assert!(order(attention) < order("blocked"));
    }

    #[test]
    fn a_quiet_turn_is_stalled() {
        let (attention, reason) = classify(
            Some("running"),
            Some(true),
            None,
            None,
            None,
            None,
            Some(at(0)),
            at(1200),
            600,
        );
        assert_eq!(attention, "stalled");
        assert_eq!(reason, "running, but nothing printed for 20 min");
    }

    #[test]
    fn an_unasked_engine_is_unknown() {
        let (attention, _) = classify(None, None, None, None, None, None, None, at(0), 600);
        assert_eq!(attention, "unknown");
        assert!(needs_you("waiting"));
        assert!(!needs_you("idle"));
    }
}
