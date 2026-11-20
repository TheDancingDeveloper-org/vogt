//! Agent-auth manifest, as the engine reports it (admin, read-only).
//!
//! Ports `src/vogt/application/services/agent_auth.py`. The engine owns the
//! manifest, so both operations are pass-through reads: Vogt adds nothing and
//! returns what the engine said. Neither carries a secret value.

use serde_json::{json, Value};

use super::{dispatch, Built};

use crate::application::context::AppContext;
use crate::core::{Clock, IdFactory};
use crate::errors::VogtError;

/// Python `_NO_ENGINE`: the stated answer for the read, the refusal for the check.
const NO_ENGINE: &str = "no session engine is configured (VOGT_ENGINE_URL is unset)";

pub fn manifest_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    if !params.is_object() {
        return Err(VogtError::InvalidRequest(
            "parameters must be an object".into(),
        ));
    }
    dispatch!(built, manifest, ())
}

pub fn policy_check_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    let named = params
        .get("var")
        .and_then(Value::as_str)
        .is_some_and(|v| !v.trim().is_empty());
    if !named {
        return Err(VogtError::InvalidRequest("var is required".into()));
    }
    dispatch!(built, policy_check, params)
}

fn manifest<C, I>(ctx: &AppContext<C, I>, _: ()) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let Some(engine) = ctx.engine.as_ref() else {
        return Ok(json!({"entries": [], "engine": NO_ENGINE}));
    };
    match engine.agent_auth_manifest() {
        Ok(value) => Ok(value),
        Err(VogtError::EngineUnavailable(e)) => Ok(json!({"entries": [], "engine": e})),
        Err(e) => Err(e),
    }
}

fn policy_check<C, I>(ctx: &AppContext<C, I>, params: Value) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let engine = ctx
        .engine
        .as_ref()
        .ok_or_else(|| VogtError::InvalidRequest(NO_ENGINE.into()))?;
    engine.agent_auth_policy_check(&params)
}

#[cfg(test)]
mod tests {
    use super::policy_check_op;
    use crate::application::services::sessions_test::{opened, script_with};
    use crate::errors::VogtError;
    use serde_json::json;
    use std::sync::Arc;

    #[test]
    fn a_check_without_a_var_is_refused_before_the_engine_is_asked() {
        let script = script_with(vec![]);
        let built = opened(Some(Arc::clone(&script)));
        let error = policy_check_op(&built, json!({"template": "x"})).unwrap_err();
        assert!(matches!(error, VogtError::InvalidRequest(ref m) if m == "var is required"));
    }
}
