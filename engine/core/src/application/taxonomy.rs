//! Labels, actors, and the workflows transitions are checked against. Ports
//! the label, actor, and workflow half of
//! `src/vogt/application/services/taxonomy.py`.
//!
//! Initiatives live in `services::initiatives`; this module does not re-port
//! them.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::application::context::{AppContext, Built};
use crate::application::services::{context_parts, dispatch, next_id, now_of, write_context};
use crate::application::writes::{audited_write, WriteOutcome};
use crate::core::{py_repr, Actor, ActorKind, Clock, IdFactory, Label, Moment};
use crate::errors::VogtError;
use crate::storage::interface::{DeclaredStore, ReadView, WriteTxn};
use crate::storage::sqlite::declared::SqliteDeclaredStore;

const LABEL_CREATE: &str = "label.create";
const ACTOR_CREATE: &str = "actor.create";

const LABEL_CREATED_EVENT: &str = "label.created";
const ACTOR_CREATED_EVENT: &str = "actor.created";

const WORK_KINDS: [&str; 4] = ["feature", "bug", "chore", "question"];

fn parse<T: for<'de> serde::Deserialize<'de>>(params: Value) -> Result<T, VogtError> {
    serde_json::from_value(params).map_err(|err| VogtError::InvalidRequest(err.to_string()))
}

// --- label --------------------------------------------------------------------

#[derive(serde::Deserialize)]
struct CreateLabelParams {
    name: String,
    color: Option<String>,
    reason: String,
}

#[derive(serde::Deserialize)]
struct ListParams {
    #[serde(default = "default_limit")]
    limit: i64,
    #[serde(default)]
    offset: i64,
}

fn default_limit() -> i64 {
    100
}

struct NewLabel {
    id: String,
    name: String,
    color: Option<String>,
    now: Moment,
}

/// Define a tag, shared instance-wide and GitHub-label aligned.
pub fn create_label_op(ctx: &Built, params: Value) -> Result<Value, VogtError> {
    dispatch!(ctx, create_label, params)
}

fn create_label<C, I>(ctx: &AppContext<C, I>, params: Value) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let params = parse::<CreateLabelParams>(params)?;
    let (declared, _, principal, clock, ids, _) = context_parts(ctx);
    // Checked before the id is drawn. The write repeats the check, but a
    // duplicate refused here costs no id, so the next one stays where Python
    // puts it.
    if declared.read()?.label_by_name(&params.name)?.is_some() {
        return Err(VogtError::Conflict(format!(
            "a label named {} already exists",
            py_repr(&params.name)
        )));
    }
    let input = NewLabel {
        id: next_id(&ids, "lbl"),
        name: params.name,
        color: params.color,
        now: now_of(&clock),
    };
    let mut writing = write_context(declared, principal, Arc::clone(&clock), Arc::clone(&ids));
    let label = insert_label(&mut writing, &params.reason, input)?;
    Ok(json!({"label": label}))
}

fn insert_label<C: Clock + 'static, I: IdFactory + 'static>(
    writing: &mut crate::application::writes::WriteContext<'_, C, I, SqliteDeclaredStore<C, I>>,
    reason: &str,
    input: NewLabel,
) -> Result<Label, VogtError> {
    audited_write(writing, LABEL_CREATE, reason, |txn, _actor| {
        if txn.label_by_name(&input.name)?.is_some() {
            return Err(VogtError::Conflict(format!(
                "a label named {} already exists",
                py_repr(&input.name)
            )));
        }
        let label = Label {
            id: input.id.clone(),
            name: input.name.clone(),
            color: input.color.clone(),
            created_at: input.now,
        };
        txn.insert_label(&label)?;
        let payload = serde_json::to_value(&label).unwrap_or(Value::Null);
        Ok(WriteOutcome::new(
            label,
            "label",
            &input.id,
            payload,
            LABEL_CREATED_EVENT,
            json!({"name": input.name}),
        ))
    })
}

pub fn list_labels_op(ctx: &Built, params: Value) -> Result<Value, VogtError> {
    dispatch!(ctx, list_labels, params)
}

fn list_labels<C, I>(ctx: &AppContext<C, I>, params: Value) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let params = parse::<ListParams>(or_default_object(params))?;
    let (declared, _, _, _, _, _) = context_parts(ctx);
    let labels = declared.read()?.list_labels(params.limit, params.offset)?;
    Ok(json!({"labels": labels}))
}

fn or_default_object(params: Value) -> Value {
    if params.is_null() {
        json!({})
    } else {
        params
    }
}

// --- actor --------------------------------------------------------------------

#[derive(serde::Deserialize)]
struct CreateActorParams {
    kind: String,
    display_name: String,
    identity_ref: String,
    reason: String,
}

struct NewActor {
    id: String,
    kind: String,
    display_name: String,
    identity_ref: String,
    now: Moment,
}

/// Register a human or agent so work can be assigned to it.
///
/// This creates no credential and grants no access. An actor here is somebody
/// work can be attributed to; the tokens that let them act are issued
/// separately, and the acting principal is never taken from a parameter.
pub fn create_actor_op(ctx: &Built, params: Value) -> Result<Value, VogtError> {
    dispatch!(ctx, create_actor, params)
}

fn create_actor<C, I>(ctx: &AppContext<C, I>, params: Value) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let params = parse::<CreateActorParams>(params)?;
    if params.kind != "human" && params.kind != "agent" {
        return Err(VogtError::InvalidRequest(format!(
            "actor kind must be 'human' or 'agent', not {}",
            py_repr(&params.kind)
        )));
    }
    let (declared, _, principal, clock, ids, _) = context_parts(ctx);
    if declared
        .read()?
        .actor_by_identity(&params.identity_ref)?
        .is_some()
    {
        return Err(VogtError::Conflict(format!(
            "an actor with identity {} already exists",
            py_repr(&params.identity_ref)
        )));
    }
    let input = NewActor {
        id: next_id(&ids, "act"),
        kind: params.kind,
        display_name: params.display_name,
        identity_ref: params.identity_ref,
        now: now_of(&clock),
    };
    let mut writing = write_context(declared, principal, Arc::clone(&clock), Arc::clone(&ids));
    let actor = insert_actor(&mut writing, &params.reason, input)?;
    Ok(json!({"actor": actor}))
}

fn insert_actor<C: Clock + 'static, I: IdFactory + 'static>(
    writing: &mut crate::application::writes::WriteContext<'_, C, I, SqliteDeclaredStore<C, I>>,
    reason: &str,
    input: NewActor,
) -> Result<Actor, VogtError> {
    audited_write(writing, ACTOR_CREATE, reason, |txn, _actor| {
        if txn.actor_by_identity(&input.identity_ref)?.is_some() {
            return Err(VogtError::Conflict(format!(
                "an actor with identity {} already exists",
                py_repr(&input.identity_ref)
            )));
        }
        let kind = match input.kind.as_str() {
            "agent" => ActorKind::Agent,
            _ => ActorKind::Human,
        };
        let actor = Actor {
            id: input.id.clone(),
            kind,
            display_name: input.display_name.clone(),
            identity_ref: input.identity_ref.clone(),
            disabled: false,
            created_at: input.now,
        };
        txn.insert_actor(&actor)?;
        let payload = serde_json::to_value(&actor).unwrap_or(Value::Null);
        Ok(WriteOutcome::new(
            actor,
            "actor",
            &input.id,
            payload,
            ACTOR_CREATED_EVENT,
            json!({"identity_ref": input.identity_ref, "kind": input.kind}),
        ))
    })
}

pub fn list_actors_op(ctx: &Built, params: Value) -> Result<Value, VogtError> {
    dispatch!(ctx, list_actors, params)
}

fn list_actors<C, I>(ctx: &AppContext<C, I>, params: Value) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let params = parse::<ListParams>(or_default_object(params))?;
    let (declared, _, _, _, _, _) = context_parts(ctx);
    let actors = declared.read()?.list_actors(params.limit, params.offset)?;
    Ok(json!({"actors": actors}))
}

// --- workflow -----------------------------------------------------------------

/// Publish the state machines transitions are checked against.
///
/// An agent that can read the machine can pick a legal next state instead of
/// guessing and handling a rejection.
pub fn list_workflows_op(ctx: &Built, params: Value) -> Result<Value, VogtError> {
    dispatch!(ctx, list_workflows, params)
}

fn list_workflows<C, I>(ctx: &AppContext<C, I>, _params: Value) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    Ok(json!({"workflows": views(ctx)?}))
}

fn views<C: Clock, I: IdFactory>(ctx: &AppContext<C, I>) -> Result<Vec<Value>, VogtError> {
    let (declared, _, _, _, _, _) = context_parts(ctx);
    let view = declared.read()?;
    WORK_KINDS
        .iter()
        .map(|kind| {
            let workflow = view.workflow_for(kind)?;
            let mut states = workflow.states();
            states.sort();
            let transitions: serde_json::Map<String, Value> = workflow
                .transitions
                .into_iter()
                .map(|(source, targets)| (source, json!(targets)))
                .collect();
            Ok(json!({
                "kind": workflow.kind,
                "initial_state": workflow.initial_state,
                "states": states,
                "transitions": Value::Object(transitions),
            }))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Principal, SequentialIds, StepClock};

    fn moment() -> Moment {
        Moment::from_unix(1_700_000_000, 0)
    }

    fn unique() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        COUNTER.fetch_add(1, Ordering::Relaxed)
    }

    fn context() -> Built {
        let dir =
            std::env::temp_dir().join(format!("vogt-taxonomy-{}-{}", std::process::id(), unique()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let config = crate::config::VogtConfig {
            data_dir: dir,
            ..crate::config::VogtConfig::default()
        };
        let principal = Principal::new("local:test-user", ActorKind::Human, "Test").unwrap();
        let built = crate::application::context::build_context(
            config,
            Some(principal.clone()),
            Some(StepClock::new(moment())),
            Some(SequentialIds::new(None).unwrap()),
            None,
            None,
            None,
            None,
        )
        .unwrap();
        let Built::StepSequential(ctx) = &built else {
            unreachable!()
        };
        ctx.declared.migrate().unwrap();
        ctx.declared.bootstrap(&principal).unwrap();
        built
    }

    #[test]
    fn a_duplicate_label_name_is_a_conflict() {
        let ctx = context();
        create_label_op(&ctx, json!({"name": "bug", "reason": "a reason"})).unwrap();
        let error =
            create_label_op(&ctx, json!({"name": "bug", "reason": "a reason"})).unwrap_err();
        assert!(
            matches!(error, VogtError::Conflict(ref message) if message == "a label named 'bug' already exists"),
            "{error}"
        );
    }

    #[test]
    fn an_actor_kind_outside_human_or_agent_is_refused() {
        let ctx = context();
        let error = create_actor_op(
            &ctx,
            json!({"kind": "robot", "display_name": "R", "identity_ref": "x:r", "reason": "a reason"}),
        )
        .unwrap_err();
        assert!(
            matches!(error, VogtError::InvalidRequest(ref message) if message.contains("actor kind must be 'human' or 'agent'")),
            "{error}"
        );
        create_actor_op(
            &ctx,
            json!({"kind": "agent", "display_name": "R", "identity_ref": "x:r", "reason": "a reason"}),
        )
        .unwrap();
        let again = create_actor_op(
            &ctx,
            json!({"kind": "agent", "display_name": "R", "identity_ref": "x:r", "reason": "a reason"}),
        )
        .unwrap_err();
        assert!(matches!(again, VogtError::Conflict(_)), "{again}");
    }

    #[test]
    fn workflows_cover_the_four_work_kinds() {
        let ctx = context();
        let result = list_workflows_op(&ctx, Value::Null).unwrap();
        let kinds: Vec<&str> = result["workflows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["kind"].as_str().unwrap())
            .collect();
        assert_eq!(kinds, ["feature", "bug", "chore", "question"]);
        assert!(result["workflows"][0]["states"].as_array().unwrap().len() > 1);
    }
}
