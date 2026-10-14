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

/// What an engine that cannot be asked looks like: an empty tool list, never an
/// error, so an outage does not read as "no agent CLIs".
const UNAVAILABLE: &str = "the session engine did not answer";

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
        return Ok(unavailable(None));
    };
    match engine.agent_clis(upstream) {
        Ok(payload) => Ok(with_engine_field(payload)),
        Err(VogtError::EngineUnavailable(_)) => Ok(unavailable(Some(UNAVAILABLE))),
        Err(other) => Err(other),
    }
}

/// Make `version` of `tool` the one new sessions run, and record who asked.
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
    if tool.trim().is_empty() || version.trim().is_empty() {
        return Err(VogtError::InvalidRequest(
            "a tool and a version are both required".to_string(),
        ));
    }
    let engine = ctx.engine.as_ref().ok_or_else(|| {
        VogtError::EngineUnavailable(
            "no session engine is configured, so no agent CLI can be moved \
             (set VOGT_ENGINE_URL)"
                .to_string(),
        )
    })?;
    let payload = engine.update_agent_cli(tool.trim(), version.trim())?;
    let mut writing = write_of(ctx);
    audited_action(
        &mut writing,
        AGENT_CLI_UPDATE,
        &reason,
        "agent_cli",
        tool.trim(),
        &json!({
            "tool": tool.trim(),
            "version": version.trim(),
            "active_version": payload.get("active_version").cloned().unwrap_or(Value::Null),
        }),
        "agent_cli.updated",
        Some(&payload),
    )?;
    Ok(payload)
}

/// The engine's payload, with the field the result model always carries.
fn with_engine_field(mut payload: Value) -> Value {
    if let Some(object) = payload.as_object_mut() {
        object.entry("engine".to_string()).or_insert(Value::Null);
        if !object.contains_key("tools") {
            object.insert("tools".to_string(), Value::Array(Vec::new()));
        }
        object
            .entry("installer_present".to_string())
            .or_insert(Value::Bool(false));
    }
    payload
}

fn unavailable(detail: Option<&str>) -> Value {
    json!({
        "tools": [],
        "installer_present": false,
        "engine": detail.map(str::to_string),
    })
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
        assert!(result["engine"].is_null());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn updating_a_cli_records_who_asked_and_returns_the_engine() {
        let (_dir, built) = context(vec![
            br#"{"tool":"claude","active_version":"2.0.0"}"#.to_vec()
        ]);
        let result = update_agent_cli_op(
            &built,
            json!({"tool": "claude", "version": "2.0.0", "reason": "pinned"}),
        )
        .unwrap();
        assert_eq!(result["active_version"], "2.0.0");
    }
}
