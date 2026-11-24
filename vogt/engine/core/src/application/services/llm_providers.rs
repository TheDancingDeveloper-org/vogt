//! LLM provider registry operations, delegated to the session engine.

use serde_json::{json, Value};

use super::{dispatch, Built};
use crate::application::context::{write_of, AppContext};
use crate::application::writes::{self, audited_action};
use crate::core::{Clock, IdFactory};
use crate::errors::VogtError;

const NO_ENGINE: &str = "no session engine is configured (VOGT_ENGINE_URL is unset)";

pub fn list_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    if !params.is_object() {
        return Err(VogtError::InvalidRequest(
            "parameters must be an object".into(),
        ));
    }
    dispatch!(built, list, ())
}

pub fn set_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    let reason = require(&params, "reason")?;
    dispatch!(built, set, (params, reason))
}

pub fn remove_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    let name = require(&params, "name")?;
    let reason = require(&params, "reason")?;
    dispatch!(built, remove, (name, reason))
}

fn list<C, I>(ctx: &AppContext<C, I>, _: ()) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let Some(engine) = ctx.engine.as_ref() else {
        return Ok(json!({"providers": [], "engine": NO_ENGINE}));
    };
    match engine.llm_providers() {
        Ok(value) => Ok(value),
        Err(VogtError::EngineUnavailable(e)) => Ok(json!({"providers": [], "engine": e})),
        Err(e) => Err(e),
    }
}

fn set<C, I>(ctx: &AppContext<C, I>, args: (Value, String)) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let (mut params, reason) = args;
    // The Python model supplies the public default.  The Rust core must do
    // the same before forwarding to the engine, whose wire request requires
    // this field.
    if let Value::Object(object) = &mut params {
        object
            .entry("wire_api".to_string())
            .or_insert_with(|| Value::String("chat".to_string()));
    }
    let engine = ctx
        .engine
        .as_ref()
        .ok_or_else(|| VogtError::InvalidRequest(NO_ENGINE.into()))?;
    let payload = engine.set_llm_provider(&params)?;
    let name = payload
        .get("provider")
        .and_then(|p| p.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let mut writing = write_of(ctx);
    audited_action(
        &mut writing,
        "llm_provider.set",
        &writes::validate_reason(&reason)?,
        "llm_provider",
        name,
        &json!({"name": name}),
        "llm_provider.updated",
        Some(&json!({"name": name})),
    )?;
    Ok(payload)
}

fn remove<C, I>(ctx: &AppContext<C, I>, args: (String, String)) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let (name, reason) = args;
    let engine = ctx
        .engine
        .as_ref()
        .ok_or_else(|| VogtError::InvalidRequest(NO_ENGINE.into()))?;
    let payload = engine.remove_llm_provider(&name)?;
    let mut writing = write_of(ctx);
    audited_action(
        &mut writing,
        "llm_provider.remove",
        &writes::validate_reason(&reason)?,
        "llm_provider",
        &name,
        &json!({"name": name}),
        "llm_provider.removed",
        Some(&json!({"name": name})),
    )?;
    Ok(payload)
}

fn require(params: &Value, field: &str) -> Result<String, VogtError> {
    params
        .get(field)
        .and_then(Value::as_str)
        .filter(|v| !v.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| VogtError::InvalidRequest(format!("{field} is required")))
}

#[cfg(test)]
mod tests {
    use super::{remove_op, set_op};
    use crate::application::context::Built;
    use crate::application::services::sessions_test::{opened, script_with};
    use crate::core::Moment;
    use crate::storage::interface::{AuditQuery, DeclaredStore, ReadView};
    use serde_json::json;
    use std::sync::Arc;

    #[test]
    fn provider_writes_are_audited_and_published_with_the_caller_reason() {
        let script = script_with(vec![
            (200, r#"{"provider":{"name":"demo"}}"#.to_string()),
            (200, r#"{"name":"demo","removed":true}"#.to_string()),
        ]);
        let built = opened(Some(Arc::clone(&script)));

        set_op(
            &built,
            json!({
                "name": "demo",
                "base_url": "https://provider.example.test",
                "env_key": "DEMO_KEY",
                "secret_project": "project",
                "secret_name": "key",
                "reason": "register demo provider"
            }),
        )
        .unwrap();
        remove_op(
            &built,
            json!({"name":"demo", "reason":"retire demo provider"}),
        )
        .unwrap();

        let Built::StepSequential(ctx) = &built else {
            unreachable!()
        };
        let view = ctx.declared.read().unwrap();
        let audits = view
            .list_audit(&AuditQuery {
                limit: 20,
                offset: 0,
                actor_id: None,
                operation: None,
                entity_id: None,
                project_id: None,
                since: Some(Moment::from_unix(0, 0)),
                until: None,
            })
            .unwrap();
        let provider_audits: Vec<_> = audits
            .iter()
            .filter(|audit| audit.entity_kind == "llm_provider")
            .collect();
        assert_eq!(provider_audits.len(), 2);
        assert_eq!(provider_audits[0].operation, "llm_provider.remove");
        assert_eq!(provider_audits[0].reason, "retire demo provider");
        assert_eq!(provider_audits[1].operation, "llm_provider.set");
        assert_eq!(provider_audits[1].reason, "register demo provider");

        let events: Vec<_> = view
            .list_events(0, 20, None)
            .unwrap()
            .into_iter()
            .filter(|event| event.entity_kind == "llm_provider")
            .collect();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].kind, "llm_provider.updated");
        assert_eq!(events[1].kind, "llm_provider.removed");
        assert!(events.iter().all(|event| event.audit_id.is_some()));
    }
}
