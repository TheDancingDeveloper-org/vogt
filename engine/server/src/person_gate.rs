//! Only a person answers a permission prompt (WI-983).
//!
//! A Claude Code `permissions.ask` rule — and the same dialog in Codex and
//! opencode — means "a person decides". Every route that writes to a
//! session's terminal holds the `sessions` capability, and every `work.write`
//! token holds that, so without this gate an overseer could approve the
//! prompt of the session it drives, and a session could approve its own:
//! "ask" was a driver's gate, not a person's.
//!
//! The rule is checked here, in the engine, because only the engine can
//! read the dialog at the moment the input lands: whether the bytes resolve
//! a permission prompt is a fact about the screen, not about the request.
//! It is the WI-973 discipline (`api::require_core_identity`, the core's
//! `decide_grant`) applied to a dialog: the caller is decided by flags the
//! authentication gate set, never by `name`, which a core actor's
//! `identity_ref` can make anything.
//!
//! What is gated is input — an `/answer`, a raw `/input`, a WebSocket
//! keystroke — that lands while a **permission** dialog is on screen. A TUI
//! dialog is modal, so at that moment there is no other prompt the input
//! could be meant for. Startup gates (folder trust, external `CLAUDE.md`
//! imports) are not permission prompts and stay drivable, and input to a
//! session with no dialog showing is ordinary driving, untouched.
//!
//! Who counts as a person:
//!
//! - a caller the core resolved to an actor of kind `human`;
//! - vogt-core's own credential (the stack secret) only when it says the
//!   principal it is relaying is a person — it is the core's
//!   `session.answer`/`session.input` that calls here, and the core decides
//!   that from its own authenticated principal, refusing agents and the
//!   engine's credential itself;
//! - the break-glass token, an operator credential withheld from every
//!   session, unless the request says otherwise (the core says `false` when
//!   it relays an agent with it).
//!
//! Every session token (`agent:session:…`), engine-minted token
//! (`agent:engine:…`) and the pod token (`agent:pod:…`/`agent:vogt-sessions`)
//! resolves to an `agent` actor and is refused. As with WI-973, this is only
//! a boundary against a session that cannot read the engine's own
//! credentials — sessions running as a different uid (WI-982).

use crate::{approval::Detected, auth::AuthorizedIdentity, error::ApiError, pty::Session};

/// The dialog kinds only a person may answer: a tool call the permission
/// rules did not allow, and a read outside the working directories (also a
/// permission rule). Startup gates are not here.
pub fn needs_person(kind: &str) -> bool {
    matches!(kind, "permission" | "read-outside-cwd")
}

/// Whether this caller answers as a person. `asserted` is the request's
/// `person` field, honoured only from the two credentials that relay for
/// someone else.
pub fn is_person(identity: Option<&AuthorizedIdentity>, asserted: Option<bool>) -> bool {
    match identity {
        Some(identity) if identity.stack_secret => asserted.unwrap_or(false),
        Some(identity) if identity.break_glass => asserted.unwrap_or(true),
        Some(identity) => identity.person,
        None => false,
    }
}

/// How the input reached the terminal, for the audit line.
#[derive(Debug, Clone, Copy)]
pub enum Via {
    Answer,
    Input,
    Attach,
    /// An assistant `send_input` card, approved on screen (WI-983).
    Assistant,
}

impl Via {
    fn as_str(self) -> &'static str {
        match self {
            Via::Answer => "answer",
            Via::Input => "input",
            Via::Attach => "attach",
            Via::Assistant => "assistant",
        }
    }
}

/// The permission dialog showing on `session` now, if any, read fresh from
/// a render — the same read `/answer` aims by — rather than from the last
/// activity pass, which can lag a dialog that has just been answered.
pub fn permission_dialog(session: &Session) -> Option<Detected> {
    session
        .current_dialog()
        .filter(|dialog| needs_person(dialog.kind))
}

/// Refuse input from anyone but a person while a permission dialog is
/// showing, and write the audit line for a refusal.
///
/// A person's input is not rendered against: a keystroke from the PWA must
/// stay cheap, and a person may answer whatever is showing.
pub fn guard(
    session: &Session,
    identity: Option<&AuthorizedIdentity>,
    asserted: Option<bool>,
    via: Via,
) -> Result<(), ApiError> {
    let who = identity.map_or("unidentified", |i| i.name.as_str());
    guard_as(session, is_person(identity, asserted), who, via)
}

/// `guard` for a caller already reduced to whether it is a person and its
/// name — the assistant's `Caller`, which no longer holds the identity.
pub fn guard_as(session: &Session, person: bool, who: &str, via: Via) -> Result<(), ApiError> {
    if person {
        return Ok(());
    }
    let Some(dialog) = permission_dialog(session) else {
        return Ok(());
    };
    Err(refuse(session, who, &dialog, via))
}

/// Write the audit line for a refused answer to `dialog` and return the
/// refusal. For a route that has already read the dialog itself (`/answer`
/// aims by it), so its refusal and `guard`'s cannot drift apart.
pub fn refuse(session: &Session, who: &str, dialog: &Detected, via: Via) -> ApiError {
    tracing::warn!(
        target: "vogt::audit",
        event = "session.permission_answer",
        outcome = "refused",
        via = via.as_str(),
        session_id = %session.id,
        principal = %who,
        kind = dialog.kind,
        question = %truncate(&dialog.question, 300),
        "refused an agent's answer to a permission prompt; only a person answers one"
    );
    refusal(who, dialog)
}

/// Record a person's answer to a permission prompt: who, which session,
/// which dialog, which option.
pub fn audit_answer(
    session: &Session,
    identity: Option<&AuthorizedIdentity>,
    asserted: Option<bool>,
    kind: &str,
    question: &str,
    option: u32,
    label: &str,
) {
    if !needs_person(kind) {
        return;
    }
    let who = identity.map_or("unidentified", |i| i.name.as_str());
    tracing::info!(
        target: "vogt::audit",
        event = "session.permission_answer",
        outcome = "answered",
        via = Via::Answer.as_str(),
        session_id = %session.id,
        principal = %who,
        relayed_person = asserted.unwrap_or(false),
        kind,
        question = %truncate(question, 300),
        option,
        label = %truncate(label, 200),
        "a person answered a permission prompt"
    );
}

/// The refusal an overseer reads. Starts `person required:` so the core can
/// tell it from any other 403 the engine gives.
pub fn refusal(who: &str, dialog: &Detected) -> ApiError {
    ApiError::Forbidden(format!(
        "person required: session is showing a {} prompt ({:?}) and only a person \
         answers one; {who} is not a person. Nothing was typed. Escalate it to a \
         person — session_report_blocked, or leave it for the Inbox — or stop the session",
        dialog.kind,
        truncate(&dialog.question, 120),
    ))
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let mut cut: String = text.chars().take(max).collect();
        cut.push('…');
        cut
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(person: bool, stack_secret: bool, break_glass: bool) -> AuthorizedIdentity {
        AuthorizedIdentity {
            name: "x".into(),
            capabilities: Vec::new(),
            scopes: Vec::new(),
            core_bearer: None,
            mutating_requests_per_minute: 0,
            stack_secret,
            break_glass,
            person,
        }
    }

    #[test]
    fn only_permission_kinds_need_a_person() {
        assert!(needs_person("permission"));
        assert!(needs_person("read-outside-cwd"));
        assert!(!needs_person("folder-trust"));
        assert!(!needs_person("external-imports"));
    }

    #[test]
    fn a_core_resolved_caller_is_a_person_only_by_kind() {
        let agent = identity(false, false, false);
        let human = identity(true, false, false);
        // A core-resolved caller cannot claim to be a person.
        assert!(!is_person(Some(&agent), Some(true)));
        assert!(is_person(Some(&human), None));
        assert!(!is_person(None, Some(true)));
    }

    #[test]
    fn the_stack_secret_relays_a_person_only_when_it_says_so() {
        let core = identity(false, true, false);
        assert!(!is_person(Some(&core), None));
        assert!(!is_person(Some(&core), Some(false)));
        assert!(is_person(Some(&core), Some(true)));
    }

    #[test]
    fn the_break_glass_token_is_an_operator_unless_told_otherwise() {
        let glass = identity(false, false, true);
        assert!(is_person(Some(&glass), None));
        assert!(!is_person(Some(&glass), Some(false)));
    }
}
