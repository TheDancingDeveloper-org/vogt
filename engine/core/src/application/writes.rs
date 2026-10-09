//! The transactional write path. Ports `src/vogt/application/writes.py`.
//!
//! Two functions, and the difference between them is where the operation's
//! effect lands rather than whether it is accountable.
//!
//! `audited_write` is used by every declared write. It opens the transaction,
//! resolves the acting actor, runs the caller's body, and lands the audit row
//! and the event row inside the same transaction as the entity change.
//!
//! `audited_action` covers the mutating operations a principal invokes whose
//! effect lands in the observed store. It is called after the effect, because
//! the two stores cannot share a transaction.

use serde_json::{json, Value};

use std::rc::Rc;

use crate::core::{Actor, Clock, IdFactory};
use crate::decisions::digest_of;
use crate::errors::VogtError;
use crate::storage::interface::{DeclaredStore, WriteTxn};

/// Emitted when a principal is seen for the first time and auto-registered.
pub const ACTOR_AUTO_REGISTER: &str = "actor.auto_register";

/// What a write produced, and what the audit and event rows should say.
pub struct WriteOutcome<T> {
    pub result: T,
    pub entity_kind: String,
    pub entity_id: String,
    pub payload: Value,
    pub event_kind: String,
    pub summary: Value,
}

impl<T> WriteOutcome<T> {
    pub fn new(
        result: T,
        entity_kind: &str,
        entity_id: &str,
        payload: Value,
        event_kind: &str,
        summary: Value,
    ) -> Self {
        Self {
            result,
            entity_kind: entity_kind.to_string(),
            entity_id: entity_id.to_string(),
            payload,
            event_kind: event_kind.to_string(),
            summary,
        }
    }
}

/// Reject a blank reason before it reaches the store.
///
/// The database has a CHECK for this too. Both exist on purpose: the CHECK
/// guarantees no blank reason can ever be stored by any code path, and this
/// guarantees the caller gets a useful error instead of an integrity one.
pub fn validate_reason(reason: &str) -> Result<String, VogtError> {
    let cleaned = reason.trim();
    if cleaned.is_empty() {
        Err(VogtError::MissingReason(
            "a non-empty reason is required on every write".to_string(),
        ))
    } else {
        Ok(cleaned.to_string())
    }
}

/// The slice of a context an audited write needs. The full `AppContext` holds
/// stores and optional clients; this is what the write path reads.
///
/// The principal fields are private. They come from the context's principal,
/// through [`WriteContext::new`], and a service must not be able to set them
/// from request data.
pub struct WriteContext<'a, C, I, D> {
    declared: &'a D,
    principal_identity_ref: &'a str,
    principal_kind: crate::core::ActorKind,
    principal_display_name: &'a str,
    /// Shared with the stores, so a body that draws an id continues the count
    /// the store itself has already advanced.
    clock: Rc<std::cell::RefCell<C>>,
    ids: Rc<std::cell::RefCell<I>>,
}

impl<'a, C, I, D> WriteContext<'a, C, I, D> {
    pub(crate) fn new(
        declared: &'a D,
        principal: &'a crate::core::Principal,
        clock: Rc<std::cell::RefCell<C>>,
        ids: Rc<std::cell::RefCell<I>>,
    ) -> Self {
        Self {
            declared,
            principal_identity_ref: &principal.identity_ref,
            principal_kind: principal.kind,
            principal_display_name: &principal.display_name,
            clock,
            ids,
        }
    }
}

/// Resolve the acting principal to an Actor row, creating it if new.
///
/// Auto-registration is itself a declared write: it lands its own audit row
/// and its own event, inside the caller's transaction, so an actor never
/// appears in the audit trail without an explanation of where it came from.
pub fn ensure_actor<C, I, T>(
    txn: &mut T,
    identity_ref: &str,
    kind: crate::core::ActorKind,
    display_name: &str,
    clock: &Rc<std::cell::RefCell<C>>,
    ids: &Rc<std::cell::RefCell<I>>,
) -> Result<Actor, VogtError>
where
    C: Clock,
    I: IdFactory,
    T: WriteTxn,
{
    if let Some(existing) = txn.actor_by_identity(identity_ref)? {
        return Ok(existing);
    }
    let now = clock.borrow_mut().now();
    let actor = Actor {
        id: ids.borrow_mut().next("act"),
        kind,
        display_name: display_name.to_string(),
        identity_ref: identity_ref.to_string(),
        disabled: false,
        created_at: now,
    };
    txn.insert_actor(&actor)?;
    let record = txn.append_audit(
        &actor,
        ACTOR_AUTO_REGISTER,
        "actor",
        &actor.id,
        &format!(
            "first authenticated use by {}; auto-registered so the write it accompanies can be attributed",
            actor.identity_ref
        ),
        &digest_of(&actor_payload(&actor)),
        now,
    )?;
    txn.append_event(
        ACTOR_AUTO_REGISTER,
        "actor",
        &actor.id,
        Some(&actor.id),
        Some(&record.id),
        &json!({
            "identity_ref": actor.identity_ref,
            "kind": actor.kind.as_str(),
        }),
        now,
    )?;
    Ok(actor)
}

/// `Actor.model_dump(mode="json")`: the fields, in declaration order, with the
/// moment rendered the way the Python model renders one.
fn actor_payload(actor: &Actor) -> Value {
    json!({
        "id": actor.id,
        "kind": actor.kind.as_str(),
        "display_name": actor.display_name,
        "identity_ref": actor.identity_ref,
        "disabled": actor.disabled,
        "created_at": actor.created_at.to_json(),
    })
}

/// Run one declared write atomically, audited and evented.
///
/// `body` receives the transaction, the resolved actor, and the shared clock
/// and id factory, so it draws after `ensure_actor` exactly as a Python body
/// does. The audit and event rows are appended after it returns and committed
/// with it; a `Err` from the body drops the transaction, which rolls it back.
pub fn audited_write<C, I, D, T, F>(
    ctx: &mut WriteContext<'_, C, I, D>,
    operation: &str,
    reason: &str,
    body: F,
) -> Result<T, VogtError>
where
    C: Clock,
    I: IdFactory,
    D: DeclaredStore,
    F: FnOnce(
        &mut D::Write<'_>,
        &Actor,
        &Rc<std::cell::RefCell<C>>,
        &Rc<std::cell::RefCell<I>>,
    ) -> Result<WriteOutcome<T>, VogtError>,
{
    let cleaned = validate_reason(reason)?;
    let mut txn = ctx.declared.write()?;
    let actor = ensure_actor(
        &mut txn,
        ctx.principal_identity_ref,
        ctx.principal_kind,
        ctx.principal_display_name,
        &ctx.clock,
        &ctx.ids,
    )?;
    let outcome = body(&mut txn, &actor, &ctx.clock, &ctx.ids)?;
    let now = ctx.clock.borrow_mut().now();
    let record = txn.append_audit(
        &actor,
        operation,
        &outcome.entity_kind,
        &outcome.entity_id,
        &cleaned,
        &digest_of(&outcome.payload),
        now,
    )?;
    txn.append_event(
        &outcome.event_kind,
        &outcome.entity_kind,
        &outcome.entity_id,
        Some(&actor.id),
        Some(&record.id),
        &outcome.summary,
        now,
    )?;
    let result = outcome.result;
    txn.commit()?;
    Ok(result)
}

/// Record a mutating operation whose effect landed outside the declared store.
///
/// Called after the effect, not around it. The audit row and its event still
/// share one transaction. The argument count matches `audited_action` in
/// writes.py.
#[allow(clippy::too_many_arguments)]
pub fn audited_action<C, I, D>(
    ctx: &mut WriteContext<'_, C, I, D>,
    operation: &str,
    reason: &str,
    entity_kind: &str,
    entity_id: &str,
    outcome: &Value,
    event_kind: &str,
    summary: Option<&Value>,
) -> Result<(), VogtError>
where
    C: Clock,
    I: IdFactory,
    D: DeclaredStore,
{
    let cleaned = validate_reason(reason)?;
    let mut txn = ctx.declared.write()?;
    let actor = ensure_actor(
        &mut txn,
        ctx.principal_identity_ref,
        ctx.principal_kind,
        ctx.principal_display_name,
        &ctx.clock,
        &ctx.ids,
    )?;
    let now = ctx.clock.borrow_mut().now();
    let record = txn.append_audit(
        &actor,
        operation,
        entity_kind,
        entity_id,
        &cleaned,
        &digest_of(outcome),
        now,
    )?;
    txn.append_event(
        event_kind,
        entity_kind,
        entity_id,
        Some(&actor.id),
        Some(&record.id),
        summary.unwrap_or(outcome),
        now,
    )?;
    txn.commit()
}
