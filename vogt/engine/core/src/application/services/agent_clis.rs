//! Runtime-pinned agent CLIs, as the engine reports and moves them.
//!
//! Ports `src/vogt/application/services/agent_clis.py`. The engine is the only
//! place that knows what is installed and what a new session will run, so both
//! operations are a thin, audited pass-through: Vogt adds the reason and the
//! actor, and returns what the engine said.

use serde_json::{json, Value};

use super::{dispatch, Built};

use crate::application::context::{write_of, AppContext};
use crate::application::writes::{self, audited_action};
use crate::core::{py_repr, Clock, IdFactory};
use crate::errors::VogtError;

const AGENT_CLI_UPDATE: &str = "agent_cli.update";

/// Python `_NO_ENGINE`: the stated answer for the read and the refusal for the
/// write when no engine is configured.
const NO_ENGINE: &str = "no session engine is configured (VOGT_ENGINE_URL is unset)";

pub fn list_agent_clis_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    let upstream = match params.get("upstream") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(value)) => *value,
        Some(_) => {
            return Err(VogtError::InvalidRequest(
                "upstream must be a boolean".to_string(),
            ))
        }
    };
    dispatch!(built, list_agent_clis, upstream)
}

pub fn update_agent_cli_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    let tool = require(&params, "tool")?;
    let version = require(&params, "version")?;
    let reason = require(&params, "reason")?;
    dispatch!(built, update_agent_cli, (&tool, &version, &reason))
}

/// The pod's agent CLIs. An engine that is down or unconfigured is reported in
/// `engine` and yields an empty list, matching the Python service.
fn list_agent_clis<C, I>(ctx: &AppContext<C, I>, upstream: bool) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let Some(engine) = ctx.engine.as_ref() else {
        return Ok(list_result(Vec::new(), false, Some(NO_ENGINE)));
    };
    match engine.agent_clis(upstream) {
        Ok(payload) => Ok(list_result(
            rows(&payload),
            payload.get("installer_present").is_some_and(python_truthy),
            None,
        )),
        Err(VogtError::EngineUnavailable(text)) => Ok(list_result(Vec::new(), false, Some(&text))),
        Err(other) => Err(other),
    }
}

/// Make `version` of `tool` the one new sessions run, and record who asked.
///
/// The engine is asked first and its refusals are raised as they are; only a
/// move the engine confirms is audited (Python `agent_cli_update`).
fn update_agent_cli<C, I>(
    ctx: &AppContext<C, I>,
    args: (&str, &str, &str),
) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let (tool, version, reason) = args;
    let reason = writes::validate_reason(reason)?;
    let engine = ctx
        .engine
        .as_ref()
        .ok_or_else(|| VogtError::InvalidRequest(NO_ENGINE.to_string()))?;
    let payload = engine.update_agent_cli(tool, version)?;
    let tools = rows(&payload);
    let moved = tools.iter().find(|row| row["tool"] == tool);
    let active_version = moved.map_or(Value::Null, |row| row["active_version"].clone());
    let source = moved.map_or(Value::Null, |row| row["source"].clone());
    let result = json!({
        "tool": tool,
        "requested": version,
        "active_version": active_version,
        "source": source,
        "tools": tools,
    });
    let mut writing = write_of(ctx);
    audited_action(
        &mut writing,
        AGENT_CLI_UPDATE,
        &reason,
        "agent_cli",
        tool,
        &json!({
            "tool": tool,
            "requested": version,
            "active_version": result["active_version"],
            "source": result["source"],
        }),
        "agent_cli.updated",
        Some(&json!({
            "tool": tool,
            "requested": version,
            "active_version": result["active_version"],
        })),
    )?;
    Ok(result)
}

fn list_result(tools: Vec<Value>, installer_present: bool, engine: Option<&str>) -> Value {
    json!({
        "tools": tools,
        "installer_present": installer_present,
        "engine": engine,
    })
}

fn python_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64().is_some_and(|n| n != 0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(map) => !map.is_empty(),
    }
}

/// Python `_rows`: each object in `tools` read as an `AgentCliRow`, in the
/// model's field order with its defaults.
fn rows(payload: &Value) -> Vec<Value> {
    let Some(Value::Array(items)) = payload.get("tools") else {
        return Vec::new();
    };
    items
        .iter()
        .filter(|row| row.is_object())
        .map(|row| {
            let field = |name: &str| row.get(name).cloned().unwrap_or(Value::Null);
            json!({
                "tool": field("tool"),
                "package": field("package"),
                "binary": field("binary"),
                "kind": row.get("kind").cloned().unwrap_or_else(|| json!("npm")),
                "env_var": field("env_var"),
                "baked_version": field("baked_version"),
                "active_version": field("active_version"),
                "source": field("source"),
                "installed_versions": row.get("installed_versions").cloned().unwrap_or_else(|| json!([])),
                "upstream_latest": field("upstream_latest"),
                "update_available": field("update_available"),
            })
        })
        .collect()
}

fn require(params: &Value, field: &str) -> Result<String, VogtError> {
    match params.get(field).and_then(Value::as_str) {
        Some(text) if !text.trim().is_empty() => Ok(text.to_string()),
        _ => Err(VogtError::InvalidRequest(format!(
            "{field} is required ({})",
            py_repr(field)
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::engine::EngineClient;
    use crate::application::instance;
    use crate::core::{ActorKind, Moment, Principal, StepClock};
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    fn context(answers: Vec<Vec<u8>>) -> (std::path::PathBuf, Built) {
        let dir = std::env::temp_dir().join(format!("vogt-clis-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut clock = None;
        let mut ids = None;
        instance::init(&dir, &mut clock, &mut ids).unwrap();
        let built = crate::application::context::build_context(
            crate::config::VogtConfig {
                data_dir: dir.clone(),
                ..crate::config::VogtConfig::default()
            },
            Some(Principal::new("local:ada", ActorKind::Human, "Ada").unwrap()),
            Some(StepClock::new(Moment::from_unix(1_700_000_000, 0))),
            None,
            None,
            Some(scripted(answers)),
            None,
            None,
        )
        .unwrap();
        (dir, built)
    }

    /// An engine that answers each GET from `script`, last entry first.
    fn scripted(script: Vec<Vec<u8>>) -> EngineClient {
        let remaining = Arc::new(Mutex::new(script));
        EngineClient::new(
            "http://engine",
            None,
            Some(std::sync::Arc::new(move |_path, _, _body, _method| {
                let answer = remaining
                    .lock()
                    .unwrap()
                    .pop()
                    .unwrap_or_else(|| b"{}".to_vec());
                (200, answer)
            })),
        )
    }

    #[test]
    fn listing_without_an_engine_says_so_instead_of_claiming_none() {
        let dir = std::env::temp_dir().join(format!("vogt-clis-none-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut clock = None;
        let mut ids = None;
        instance::init(&dir, &mut clock, &mut ids).unwrap();
        let built = crate::application::context::build_context(
            crate::config::VogtConfig {
                data_dir: dir.clone(),
                ..crate::config::VogtConfig::default()
            },
            Some(Principal::new("local:ada", ActorKind::Human, "Ada").unwrap()),
            Some(StepClock::new(Moment::from_unix(1_700_000_000, 0))),
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        let result = list_agent_clis_op(&built, json!({})).unwrap();
        assert_eq!(result["tools"], json!([]));
        assert!(result["engine"]
            .as_str()
            .unwrap()
            .contains("VOGT_ENGINE_URL"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_confirmed_move_is_audited_with_the_engines_answer() {
        let moved = br#"{"root":"/opt","installer_present":true,"tools":[{"tool":"claude-code","package":"@anthropic-ai/claude-code","binary":"claude","env_var":"VOGT_CLAUDE_CODE_VERSION","baked_version":"2.1.258","active_version":"2.1.261","source":"runtime","installed_versions":["2.1.261"]}]}"#;
        let (dir, built) = context(vec![moved.to_vec()]);
        let result = update_agent_cli_op(
            &built,
            json!({"tool": "claude-code", "version": "2.1.261", "reason": "a model users want"}),
        )
        .unwrap();
        assert_eq!(result["tool"], "claude-code");
        assert_eq!(result["requested"], "2.1.261");
        assert_eq!(result["active_version"], "2.1.261");
        assert_eq!(result["source"], "runtime");
        assert_eq!(result["tools"][0]["installed_versions"], json!(["2.1.261"]));
        assert_eq!(result["tools"][0]["kind"], "npm");
        use crate::storage::interface::{DeclaredStore, ReadView};
        let view = crate::with_ctx!(&built, |ctx| ctx.declared.read()).unwrap();
        let audit: Vec<_> = view
            .list_audit(&crate::storage::interface::AuditQuery {
                limit: 20,
                offset: 0,
                actor_id: None,
                operation: Some("agent_cli.update".to_string()),
                entity_id: None,
                project_id: None,
                since: None,
                until: None,
            })
            .unwrap();
        assert_eq!(audit.len(), 1);
        assert_eq!(audit[0].entity_id, "claude-code");
        assert_eq!(audit[0].reason, "a model users want");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_move_needs_an_engine() {
        let dir = std::env::temp_dir().join(format!("vogt-clis-noengine-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut clock = None;
        let mut ids = None;
        instance::init(&dir, &mut clock, &mut ids).unwrap();
        let built = crate::application::context::build_context(
            crate::config::VogtConfig {
                data_dir: dir.clone(),
                ..crate::config::VogtConfig::default()
            },
            Some(Principal::new("local:ada", ActorKind::Human, "Ada").unwrap()),
            Some(StepClock::new(Moment::from_unix(1_700_000_000, 0))),
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        let error = update_agent_cli_op(
            &built,
            json!({"tool": "claude-code", "version": "2.1.261", "reason": "why"}),
        )
        .unwrap_err();
        assert!(
            matches!(&error, VogtError::InvalidRequest(text) if text.contains("VOGT_ENGINE_URL")),
            "{error}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
