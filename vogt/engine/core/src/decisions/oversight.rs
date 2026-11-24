// What a session overseer should look at first. Ports `core/oversight.py`.
//
// The order is decided from facts the engine reported, with the reason in
// words — never a bare rank — so the table says why a row is at the top.
// Pure: the clock is a parameter, and nothing here reads storage or the
// engine. Every reason string is copied from the Python, because the two
// compare them.

/// Lower first. `stalled` sits above `running` because a turn that has printed
/// nothing for a long time is worth a look before one that is busy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Attention {
    Approval,
    Blocked,
    Waiting,
    Done,
    Stalled,
    Running,
    Idle,
    Hibernated,
    Exited,
    Unknown,
}

impl Attention {
    #[allow(dead_code)] // The overseer table sorts by this; the test does too.
    pub fn order(self) -> u8 {
        match self {
            Self::Approval => 0,
            Self::Blocked => 1,
            Self::Waiting => 2,
            Self::Done => 3,
            Self::Stalled => 4,
            Self::Running => 5,
            Self::Idle => 6,
            Self::Hibernated => 7,
            Self::Exited => 8,
            Self::Unknown => 9,
        }
    }

    /// The classes a person, or a driver acting for one, must act on.
    #[allow(dead_code)] // The sweep filter uses this; the test asserts it.
    pub fn needs_you(self) -> bool {
        matches!(self, Self::Approval | Self::Blocked | Self::Waiting)
    }

    /// The wire value, matching `oversight.py`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Approval => "approval",
            Self::Blocked => "blocked",
            Self::Waiting => "waiting",
            Self::Done => "done",
            Self::Stalled => "stalled",
            Self::Running => "running",
            Self::Idle => "idle",
            Self::Hibernated => "hibernated",
            Self::Exited => "exited",
            Self::Unknown => "unknown",
        }
    }

    /// `order` looked up by wire value, for sorting rows that only kept the name.
    pub fn order_of(name: &str) -> u8 {
        match name {
            "approval" => 0,
            "blocked" => 1,
            "waiting" => 2,
            "done" => 3,
            "stalled" => 4,
            "running" => 5,
            "idle" => 6,
            "hibernated" => 7,
            "exited" => 8,
            _ => 9,
        }
    }

    /// Whether a wire value is one of the classes a person must act on.
    pub fn needs_you_name(name: &str) -> bool {
        matches!(name, "approval" | "blocked" | "waiting")
    }
}

/// Startup gates an agent CLI stops at before any work, as words.
fn gate_words(kind: &str) -> Option<&'static str> {
    match kind {
        "folder-trust" => Some("folder trust"),
        "external-imports" => Some("external CLAUDE.md imports"),
        "read-outside-cwd" => Some("read outside the working directory"),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    pub attention: Attention,
    pub reason: String,
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
    last_output_at: Option<crate::core::Moment>,
    now: crate::core::Moment,
    stall_after_secs: i64,
    done_at: Option<&str>,
) -> Verdict {
    let verdict = |attention: Attention, reason: String| Verdict { attention, reason };
    if activity == Some("hibernated") {
        return verdict(
            Attention::Hibernated,
            "hibernated to free memory; wake it to continue".to_string(),
        );
    }
    if activity.is_none() || alive.is_none() {
        return verdict(
            Attention::Unknown,
            "the engine could not be asked".to_string(),
        );
    }
    if activity == Some("stopped") {
        return verdict(Attention::Exited, "stopped on request".to_string());
    }
    if alive == Some(false) {
        return verdict(
            Attention::Exited,
            format!("its process ended ({})", activity.unwrap_or_default()),
        );
    }
    if approval_question.is_some_and(|text| !text.is_empty())
        || activity == Some("awaiting-approval")
    {
        if let (Some(gate), Some(question)) = (
            approval_kind.and_then(gate_words),
            approval_question.filter(|text| !text.is_empty()),
        ) {
            return verdict(
                Attention::Approval,
                format!("stopped at a startup gate ({gate}): {question}"),
            );
        }
        return verdict(
            Attention::Approval,
            match approval_question.filter(|text| !text.is_empty()) {
                Some(question) => format!("asking for approval: {question}"),
                None => "showing a permission dialog".to_string(),
            },
        );
    }
    if let Some(blocker) = blocker.filter(|text| !text.is_empty()) {
        return verdict(
            Attention::Blocked,
            format!("blocked on a person: {blocker}"),
        );
    }
    if activity == Some("waiting-for-input") || (activity == Some("idle") && ready == Some(true)) {
        if done_at.is_some_and(|text| !text.is_empty()) {
            return verdict(
                Attention::Done,
                "said it is done (AUTOPILOT: DONE), at its prompt".to_string(),
            );
        }
        return verdict(
            Attention::Waiting,
            "at its prompt, waiting for the next instruction".to_string(),
        );
    }
    if activity == Some("running") {
        if let Some(last) = last_output_at {
            // Compared in f64 so a fraction is kept. Truncating to i64 would
            // call a turn stalled when its last output is half a second in the
            // future and the stall threshold is zero.
            let quiet = now.seconds_since(last);
            if quiet >= stall_after_secs as f64 {
                let minutes = (quiet / 60.0).floor() as i64;
                return verdict(
                    Attention::Stalled,
                    format!("running, but nothing printed for {minutes} min"),
                );
            }
        }
        return verdict(Attention::Running, "working".to_string());
    }
    verdict(
        Attention::Idle,
        "resting, not at a recognised prompt".to_string(),
    )
}

#[cfg(test)]
mod oversight_tests {
    use super::*;

    fn now() -> crate::core::Moment {
        crate::core::from_iso("2026-10-09T03:00:00Z").unwrap()
    }

    fn classify_running(quiet_for_secs: i64) -> Verdict {
        let moment = now();
        classify(
            Some("running"),
            Some(true),
            Some(false),
            None,
            None,
            None,
            Some(crate::core::Moment::from_unix(
                moment.unix_seconds() - quiet_for_secs,
                0,
            )),
            moment,
            600,
            None,
        )
    }

    #[test]
    fn each_session_lands_where_a_driver_should_look() {
        // tests/test_oversight.py, the parametrised table plus the reason
        // assertions that follow it.
        let moment = now();
        let ago = |minutes: i64| {
            Some(crate::core::Moment::from_unix(
                moment.unix_seconds() - minutes * 60,
                0,
            ))
        };
        type Row<'a> = (
            &'a str,
            Option<&'a str>,
            bool,
            bool,
            Option<&'a str>,
            Attention,
        );
        let cases: &[Row] = &[
            (
                "awaiting-approval",
                Some("Run rm?"),
                true,
                false,
                None,
                Attention::Approval,
            ),
            (
                "running",
                None,
                true,
                false,
                Some("needs a token"),
                Attention::Blocked,
            ),
            (
                "waiting-for-input",
                None,
                true,
                false,
                None,
                Attention::Waiting,
            ),
            ("idle", None, true, true, None, Attention::Waiting),
            ("idle", None, true, false, None, Attention::Idle),
            ("running", None, true, false, None, Attention::Running),
            (
                "hibernated",
                None,
                false,
                false,
                None,
                Attention::Hibernated,
            ),
            ("errored", None, false, false, None, Attention::Exited),
        ];
        for (activity, question, alive, ready, blocker, want) in cases {
            let found = classify(
                Some(activity),
                Some(*alive),
                Some(*ready),
                question.as_deref(),
                blocker.as_deref(),
                None,
                ago(0),
                moment,
                600,
                None,
            );
            assert_eq!(found.attention, *want, "{activity}");
        }
        let stalled = classify(
            Some("running"),
            Some(true),
            Some(false),
            None,
            None,
            None,
            ago(25),
            moment,
            600,
            None,
        );
        assert_eq!(stalled.attention, Attention::Stalled);
        assert_eq!(stalled.reason, "running, but nothing printed for 25 min");

        let dialog = classify(
            Some("awaiting-approval"),
            Some(true),
            Some(false),
            Some("Run rm?"),
            Some("x"),
            None,
            ago(0),
            moment,
            600,
            None,
        );
        assert_eq!(dialog.attention, Attention::Approval);
        assert!(dialog.reason.contains("Run rm?"));

        // An empty question is not a question. Python treats "" as false.
        let empty = classify(
            Some("running"),
            Some(true),
            Some(false),
            Some(""),
            None,
            None,
            ago(0),
            moment,
            600,
            None,
        );
        assert_eq!(empty.attention, Attention::Running);
    }

    #[test]
    fn the_order_puts_what_needs_a_person_first() {
        let ranked = [
            Attention::Idle,
            Attention::Approval,
            Attention::Running,
            Attention::Blocked,
            Attention::Unknown,
            Attention::Waiting,
            Attention::Done,
            Attention::Stalled,
        ];
        let mut sorted = ranked.to_vec();
        sorted.sort_by_key(|attention| attention.order());
        assert_eq!(
            sorted,
            [
                Attention::Approval,
                Attention::Blocked,
                Attention::Waiting,
                Attention::Done,
                Attention::Stalled,
                Attention::Running,
                Attention::Idle,
                Attention::Unknown,
            ]
        );
        assert!(Attention::Approval.needs_you());
        assert!(!Attention::Stalled.needs_you());
    }

    #[test]
    fn a_quiet_turn_is_stalled_and_a_gate_is_named() {
        assert_eq!(classify_running(600).attention, Attention::Stalled);
        assert_eq!(
            classify_running(600).reason,
            "running, but nothing printed for 10 min"
        );
        assert_eq!(classify_running(540).attention, Attention::Running);

        let gate = classify(
            Some("awaiting-approval"),
            Some(true),
            None,
            Some("trust this folder?"),
            None,
            Some("folder-trust"),
            None,
            now(),
            600,
            None,
        );
        assert_eq!(gate.attention, Attention::Approval);
        assert_eq!(
            gate.reason,
            "stopped at a startup gate (folder trust): trust this folder?"
        );
    }

    #[test]
    fn an_unasked_engine_is_unknown_and_a_dead_process_has_exited() {
        let unknown = classify(
            None,
            Some(true),
            None,
            None,
            None,
            None,
            None,
            now(),
            600,
            None,
        );
        assert_eq!(unknown.attention, Attention::Unknown);

        let ended = classify(
            Some("crashed"),
            Some(false),
            None,
            None,
            None,
            None,
            None,
            now(),
            600,
            None,
        );
        assert_eq!(ended.reason, "its process ended (crashed)");
    }
}
