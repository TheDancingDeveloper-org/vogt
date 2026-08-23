//! Fail-closed approval gates and the steer queue for agent-task runs (#289).
//!
//! An agent task runs a CLI (`claude`, `codex`, …) in a PTY, unattended. Until
//! now there was no way to pause it for a human decision or to redirect it
//! short of killing the session. This module is the state those two abilities
//! stand on, kept deliberately free of the PTY, the clock, and the network so
//! the guarantee that matters can be tested as arithmetic:
//!
//! **A gate fails closed.** A gate that is interrupted, times out, or whose
//! session dies resolves to [`GateState::Blocked`] — never to an approval. The
//! only transition into "approved" is a person (or the audited `--auto-approve`
//! bypass) choosing an option while the gate is still open. Said as a type: the
//! one method that can produce [`GateState::Answered`] takes an actor and an
//! option; [`GateRecord::block`] can only ever write `Blocked`, and neither can
//! move a gate that is already resolved. `interrupted != approved` is therefore
//! not a rule the runtime remembers to apply — it is the only shape the data
//! can take.
//!
//! The runtime half (holding the PTY at the boundary, draining the steer queue
//! between rounds, killing a session whose gate blocked) lives in
//! `agent_tasks.rs`; this module is what it manipulates and what the unit tests
//! drive directly.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

/// The fail-closed deadline a gate uses when its spec names none. A gate that
/// nobody answers within this window blocks rather than waiting forever, so an
/// unattended run cannot sit paused indefinitely holding a session open.
pub const DEFAULT_GATE_TIMEOUT_MS: u64 = 30 * 60 * 1_000;

/// The actor recorded when the `--auto-approve` bypass resolves a gate. Named
/// rather than blank so the audit trail says *what* approved it, not just that
/// something did — a run that approved its own gates is a fact a reader must be
/// able to see months later.
pub const AUTO_APPROVE_ACTOR: &str = "auto-approve";

fn new_gate_id() -> Uuid {
    Uuid::new_v4()
}

/// One choice a gate offers. Choosing it delivers `input` to the PTY.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GateOption {
    /// What the option is called on screen and in the audit trail.
    pub label: String,
    /// The text written to the PTY when this option is chosen. The delivery
    /// path appends the Enter keystroke; a trailing newline is not stored here.
    /// May be empty — "just press Enter" is a legitimate answer to a prompt.
    #[serde(default)]
    pub input: String,
    /// The affirmative choice: what `--auto-approve` picks, and the only option
    /// whose selection counts as an approval. A gate with no `approve` option
    /// cannot be auto-approved and has no affirmative answer — it is a fork,
    /// not a yes/no. Exactly one option should carry it; the first one that
    /// does wins if several do.
    #[serde(default)]
    pub approve: bool,
}

/// A declared approval point on a task. Stable across the task's runs: the same
/// gate id identifies "the deploy gate" whether it opened yesterday or today,
/// so a client can remember how it was answered last time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GateSpec {
    #[serde(default = "new_gate_id")]
    pub id: Uuid,
    /// The question put to the human.
    pub question: String,
    /// The choices, in the order they are shown. Must be non-empty.
    pub options: Vec<GateOption>,
    /// Fail-closed deadline in milliseconds. `None` uses
    /// [`DEFAULT_GATE_TIMEOUT_MS`]. On expiry the gate resolves `blocked`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

impl GateSpec {
    /// The deadline this gate should use, its own or the default.
    pub fn timeout_ms(&self) -> u64 {
        self.timeout_ms.unwrap_or(DEFAULT_GATE_TIMEOUT_MS)
    }

    /// The index of the option `--auto-approve` selects: the first one flagged
    /// `approve`. `None` when the gate offers no affirmative choice, in which
    /// case the bypass cannot resolve it and it must fail closed like any
    /// unanswered gate.
    pub fn approve_index(&self) -> Option<usize> {
        self.options.iter().position(|opt| opt.approve)
    }

    /// Reject a gate a client could not meaningfully answer before it is ever
    /// opened: a gate with no question or no options is a bug in the task
    /// definition, not a decision anyone can take.
    pub fn validate(&self) -> Result<(), String> {
        if self.question.trim().is_empty() {
            return Err("gate question must not be empty".into());
        }
        if self.options.is_empty() {
            return Err("gate must offer at least one option".into());
        }
        if self.options.iter().any(|opt| opt.label.trim().is_empty()) {
            return Err("every gate option must have a label".into());
        }
        Ok(())
    }
}

/// Where a gate is in its life. The variant is the whole of the fail-closed
/// guarantee: `Answered` is reachable only through [`GateRecord::answer`], and
/// `Blocked` only through [`GateRecord::block`], and neither runs on a gate
/// that has already left `Open`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum GateState {
    /// Held at the prompt boundary, waiting for an answer.
    Open,
    /// A person or the audited bypass chose an option.
    Answered {
        option_index: usize,
        option_label: String,
        /// The chosen option's `approve` flag — whether this resolution was an
        /// approval or merely a selection (e.g. "Skip").
        approved: bool,
        /// Who answered. [`AUTO_APPROVE_ACTOR`] for the bypass.
        actor: String,
        /// Whether the `--auto-approve` bypass produced this answer, as opposed
        /// to a person. Surfaced on its own so the audit trail can be filtered
        /// to "gates a human actually saw".
        auto: bool,
    },
    /// Failed closed: interrupted, timed out, or its session died. Carries the
    /// reason so the audit trail can say which.
    Blocked { reason: String },
}

impl GateState {
    pub fn is_open(&self) -> bool {
        matches!(self, GateState::Open)
    }

    pub fn is_resolved(&self) -> bool {
        !self.is_open()
    }
}

/// A gate instance on a run: the audit record and the live view a client
/// renders, in one. Persisted on the run so a gate's whole history survives a
/// restart and a sweep can read what a run stopped for.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GateRecord {
    pub id: Uuid,
    pub question: String,
    pub options: Vec<GateOption>,
    #[serde(flatten)]
    pub state: GateState,
    #[serde(with = "time::serde::rfc3339")]
    pub opened_at: OffsetDateTime,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub resolved_at: Option<OffsetDateTime>,
}

impl GateRecord {
    /// Open a gate from its spec at `now`. The record starts [`GateState::Open`]
    /// — the only state anything may transition *out* of.
    pub fn open(spec: &GateSpec, now: OffsetDateTime) -> Self {
        Self {
            id: spec.id,
            question: spec.question.clone(),
            options: spec.options.clone(),
            state: GateState::Open,
            opened_at: now,
            resolved_at: None,
        }
    }

    /// Resolve the gate by an actor choosing an option — the one and only path
    /// to `Answered`, and therefore the one and only path to an approval.
    ///
    /// Refuses a resolved gate (first writer wins, so a late answer cannot
    /// overturn a block) and an out-of-range option. `auto` records whether the
    /// audited bypass produced this rather than a person.
    pub fn answer(
        &mut self,
        option_index: usize,
        actor: &str,
        auto: bool,
        now: OffsetDateTime,
    ) -> Result<(), GateError> {
        if self.state.is_resolved() {
            return Err(GateError::AlreadyResolved);
        }
        let option = self
            .options
            .get(option_index)
            .ok_or(GateError::UnknownOption)?;
        self.state = GateState::Answered {
            option_index,
            option_label: option.label.clone(),
            approved: option.approve,
            actor: actor.to_string(),
            auto,
        };
        self.resolved_at = Some(now);
        Ok(())
    }

    /// Fail the gate closed. Only ever writes `Blocked`, and only from `Open`:
    /// a gate a person already answered is left as it was (its answer stands),
    /// and a second block is a no-op. Returns whether this call is the one that
    /// blocked it, so the caller can emit the event exactly once.
    pub fn block(&mut self, reason: &str, now: OffsetDateTime) -> bool {
        if self.state.is_resolved() {
            return false;
        }
        self.state = GateState::Blocked {
            reason: reason.to_string(),
        };
        self.resolved_at = Some(now);
        true
    }

    /// The PTY input the chosen option should deliver, if this gate was
    /// answered. `None` for an open or blocked gate — a blocked gate delivers
    /// nothing, which is the point of failing closed.
    pub fn answered_input(&self) -> Option<&str> {
        match &self.state {
            GateState::Answered { option_index, .. } => self
                .options
                .get(*option_index)
                .map(|opt| opt.input.as_str()),
            _ => None,
        }
    }
}

/// Why a gate answer was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateError {
    /// The gate has already been answered or blocked.
    AlreadyResolved,
    /// The option index is out of range for this gate.
    UnknownOption,
}

impl std::fmt::Display for GateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GateError::AlreadyResolved => f.write_str("gate is already resolved"),
            GateError::UnknownOption => f.write_str("no such gate option"),
        }
    }
}

/// One queued steering instruction, delivered to the PTY at the next prompt
/// boundary. Pure data: the byte translation and the actual write live in the
/// runtime, but the ordering guarantee (`interrupt` first) is expressed by
/// [`steer_delivery_bytes`], which is tested here.
#[derive(Debug, Clone)]
pub struct SteerItem {
    pub text: String,
    /// Send the CLI's cancel (Ctrl-C) before the text.
    pub interrupt: bool,
    pub actor: String,
    pub reason: Option<String>,
}

/// The ETX byte a PTY line discipline turns into SIGINT — the CLI's cancel.
pub const CANCEL_BYTE: u8 = 0x03;

/// The exact bytes a steer delivers, in order: the cancel first when
/// `interrupt` is set, then the text with an Enter appended so the CLI reads a
/// completed line. Factored out of the runtime so the "cancel first" ordering
/// is a property a unit test pins rather than a sequence of writes a reader has
/// to trust.
pub fn steer_delivery_bytes(item: &SteerItem) -> Vec<u8> {
    let mut out = Vec::with_capacity(item.text.len() + 2);
    if item.interrupt {
        out.push(CANCEL_BYTE);
    }
    out.extend_from_slice(item.text.as_bytes());
    out.push(b'\r');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts() -> OffsetDateTime {
        OffsetDateTime::now_utc()
    }

    fn deploy_gate() -> GateSpec {
        GateSpec {
            id: Uuid::new_v4(),
            question: "Deploy to prod?".into(),
            options: vec![
                GateOption {
                    label: "Approve".into(),
                    input: "yes".into(),
                    approve: true,
                },
                GateOption {
                    label: "Hold".into(),
                    input: "no".into(),
                    approve: false,
                },
            ],
            timeout_ms: None,
        }
    }

    #[test]
    fn a_gate_opens_held_and_unresolved() {
        let gate = GateRecord::open(&deploy_gate(), ts());
        assert!(gate.state.is_open());
        assert!(!gate.state.is_resolved());
        assert!(gate.resolved_at.is_none());
        // Nothing is delivered while it is held.
        assert!(gate.answered_input().is_none());
    }

    #[test]
    fn answering_resolves_to_the_chosen_option_and_its_input() {
        let mut gate = GateRecord::open(&deploy_gate(), ts());
        gate.answer(0, "sprooty", false, ts()).unwrap();
        match &gate.state {
            GateState::Answered {
                option_index,
                option_label,
                approved,
                actor,
                auto,
            } => {
                assert_eq!(*option_index, 0);
                assert_eq!(option_label, "Approve");
                assert!(*approved);
                assert_eq!(actor, "sprooty");
                assert!(!auto);
            }
            other => panic!("expected Answered, got {other:?}"),
        }
        assert_eq!(gate.answered_input(), Some("yes"));
        assert!(gate.resolved_at.is_some());
    }

    #[test]
    fn a_non_approve_option_resolves_without_approving() {
        // Choosing "Hold" is a real answer — the gate is resolved, the run is
        // not left paused — but it is not an approval.
        let mut gate = GateRecord::open(&deploy_gate(), ts());
        gate.answer(1, "sprooty", false, ts()).unwrap();
        match &gate.state {
            GateState::Answered { approved, .. } => assert!(!approved),
            other => panic!("expected Answered, got {other:?}"),
        }
    }

    #[test]
    fn interrupt_blocks_and_never_approves() {
        // The load-bearing test: an interrupt on an open gate can only ever
        // produce Blocked. There is no argument to `block` that yields an
        // approval, and no other method the runtime could call.
        let mut gate = GateRecord::open(&deploy_gate(), ts());
        assert!(gate.block("interrupted", ts()));
        match &gate.state {
            GateState::Blocked { reason } => assert_eq!(reason, "interrupted"),
            other => panic!("expected Blocked, got {other:?}"),
        }
        assert!(gate.answered_input().is_none());
    }

    #[test]
    fn a_block_after_an_answer_is_a_no_op_and_the_answer_stands() {
        // A race between a human answer and a session death must not un-approve
        // what a person legitimately approved: first writer wins.
        let mut gate = GateRecord::open(&deploy_gate(), ts());
        gate.answer(0, "sprooty", false, ts()).unwrap();
        assert!(!gate.block("session died", ts()));
        match &gate.state {
            GateState::Answered { actor, .. } => assert_eq!(actor, "sprooty"),
            other => panic!("the human answer must stand, got {other:?}"),
        }
    }

    #[test]
    fn an_answer_after_a_block_is_refused() {
        // The other side of the race: once blocked, a late answer cannot
        // overturn it. This is what keeps a timed-out gate from being approved
        // by an answer that arrives a moment too late.
        let mut gate = GateRecord::open(&deploy_gate(), ts());
        gate.block("timed out", ts());
        assert_eq!(
            gate.answer(0, "sprooty", false, ts()),
            Err(GateError::AlreadyResolved)
        );
        match &gate.state {
            GateState::Blocked { reason } => assert_eq!(reason, "timed out"),
            other => panic!("expected Blocked to stand, got {other:?}"),
        }
    }

    #[test]
    fn an_unknown_option_is_refused() {
        let mut gate = GateRecord::open(&deploy_gate(), ts());
        assert_eq!(
            gate.answer(9, "sprooty", false, ts()),
            Err(GateError::UnknownOption)
        );
        // The gate is still open — a bad answer changed nothing.
        assert!(gate.state.is_open());
    }

    #[test]
    fn auto_approve_selects_the_approve_option() {
        let spec = deploy_gate();
        assert_eq!(spec.approve_index(), Some(0));
        let mut gate = GateRecord::open(&spec, ts());
        gate.answer(
            spec.approve_index().unwrap(),
            AUTO_APPROVE_ACTOR,
            true,
            ts(),
        )
        .unwrap();
        match &gate.state {
            GateState::Answered {
                actor,
                auto,
                approved,
                ..
            } => {
                assert_eq!(actor, AUTO_APPROVE_ACTOR);
                assert!(auto);
                assert!(approved);
            }
            other => panic!("expected an audited auto-answer, got {other:?}"),
        }
    }

    #[test]
    fn a_gate_with_no_approve_option_cannot_be_auto_approved() {
        // A fork with no affirmative choice must fail closed under the bypass,
        // exactly like an unanswered gate — the bypass is not "pick anything".
        let mut spec = deploy_gate();
        for opt in &mut spec.options {
            opt.approve = false;
        }
        assert_eq!(spec.approve_index(), None);
    }

    #[test]
    fn spec_validation_rejects_empty_gates() {
        let mut spec = deploy_gate();
        spec.question = "  ".into();
        assert!(spec.validate().is_err());

        let mut spec = deploy_gate();
        spec.options.clear();
        assert!(spec.validate().is_err());
    }

    #[test]
    fn steer_delivery_sends_cancel_before_text_when_interrupting() {
        let plain = SteerItem {
            text: "focus on the failing test".into(),
            interrupt: false,
            actor: "operator".into(),
            reason: None,
        };
        assert_eq!(steer_delivery_bytes(&plain), b"focus on the failing test\r");

        let interrupt = SteerItem {
            text: "stop".into(),
            interrupt: true,
            actor: "operator".into(),
            reason: None,
        };
        let bytes = steer_delivery_bytes(&interrupt);
        assert_eq!(bytes[0], CANCEL_BYTE, "cancel must be delivered first");
        assert_eq!(&bytes[1..], b"stop\r");
    }

    #[test]
    fn a_gate_uses_its_own_timeout_then_the_default() {
        let mut spec = deploy_gate();
        assert_eq!(spec.timeout_ms(), DEFAULT_GATE_TIMEOUT_MS);
        spec.timeout_ms = Some(1234);
        assert_eq!(spec.timeout_ms(), 1234);
    }
}
