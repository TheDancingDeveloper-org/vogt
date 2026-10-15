//! Codex app-server JSON-RPC for quick chat (WI-1148).
//!
//! A hand-written subset of the app-server protocol: one JSON object per line
//! on stdio. Types are pinned here rather than generated, because the engine
//! image does not ship `codex app-server generate-json-schema`.
//!
//! The thread id is the chat id. Approvals (`item/commandExecution/requestApproval`,
//! `item/fileChange/requestApproval`, `item/tool/requestUserInput`) are answered
//! by the person gate; the engine never auto-allows them.

use serde_json::{json, Value};
use uuid::Uuid;

/// Methods the engine sends.
pub const METHOD_INITIALIZE: &str = "initialize";
pub const METHOD_THREAD_START: &str = "thread/start";
pub const METHOD_THREAD_RESUME: &str = "thread/resume";
pub const METHOD_TURN_START: &str = "turn/start";
pub const METHOD_TURN_INTERRUPT: &str = "turn/interrupt";
pub const METHOD_THREAD_SETTINGS: &str = "thread/settings/update";

/// Approval requests the server sends that a person must answer.
pub const APPROVAL_METHODS: &[&str] = &[
    "item/commandExecution/requestApproval",
    "item/fileChange/requestApproval",
    "item/tool/requestUserInput",
];

/// LiteLLM endpoint WI-1147 pinned for the estate Codex fork.
pub const LITELLM_BASE: &str = "http://100.92.54.45:4000/v1";

/// One outbound JSON-RPC request.
pub fn request(id: &str, method: &str, params: Value) -> String {
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }).to_string()
}

/// Answer a server request (an approval).
pub fn response_ok(id: &str, result: Value) -> String {
    json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string()
}

pub fn response_err(id: &str, message: &str) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": -32000, "message": message }
    })
    .to_string()
}

/// `initialize` with the thread id the fork accepts on `thread/start` (WI-1126).
///
/// A caller-chosen `threadId` is an experimental field on the fork
/// (`thread/start.threadId requires experimentalApi capability`,
/// `app-server-protocol/src/experimental_api.rs`). The capability is the
/// `experimentalApi` boolean on `InitializeParams.capabilities`
/// (`schema/json/v1/InitializeParams.json`). Without it the fork rejects
/// `thread/start` and the following `turn/start` answers `thread not found`.
pub fn initialize(id: &str) -> String {
    request(
        id,
        METHOD_INITIALIZE,
        json!({
            "clientInfo": { "name": "vogt-engine", "version": "0" },
            "capabilities": { "experimentalApi": true }
        }),
    )
}

pub fn thread_start(id: &str, thread_id: Uuid, model: Option<&str>) -> String {
    let mut params = json!({ "threadId": thread_id.to_string(), "approvalPolicy": "on-request" });
    if let Some(model) = model {
        params["model"] = json!(model);
    }
    request(id, METHOD_THREAD_START, params)
}

pub fn thread_resume(id: &str, thread_id: Uuid) -> String {
    request(
        id,
        METHOD_THREAD_RESUME,
        json!({ "threadId": thread_id.to_string() }),
    )
}

pub fn turn_start(id: &str, thread_id: Uuid, text: &str) -> String {
    request(
        id,
        METHOD_TURN_START,
        json!({
            "threadId": thread_id.to_string(),
            "input": [{ "type": "text", "text": text }]
        }),
    )
}

pub fn turn_interrupt(id: &str, thread_id: Uuid) -> String {
    request(
        id,
        METHOD_TURN_INTERRUPT,
        json!({ "threadId": thread_id.to_string() }),
    )
}

pub fn settings_update(id: &str, thread_id: Uuid, model: &str) -> String {
    request(
        id,
        METHOD_THREAD_SETTINGS,
        json!({ "threadId": thread_id.to_string(), "model": model }),
    )
}

/// Allow or deny an approval request.
pub fn approval_decision(request_id: &str, allow: bool) -> String {
    if allow {
        response_ok(request_id, json!({ "decision": "accept" }))
    } else {
        response_ok(request_id, json!({ "decision": "decline" }))
    }
}

/// argv for `codex app-server --stdio`, from a resolved binary (or a command
/// whose first word is `codex`). TUI flags are not appended.
pub fn app_server_argv(binary: &str) -> Vec<String> {
    vec![binary.to_string(), "app-server".into(), "--stdio".into()]
}

/// `config.toml` for a per-chat `CODEX_HOME`. `env_key` is the one variable
/// the chat process is given (`ENGINE_CHAT_PROVIDER_KEY`), not the bootstrap
/// session key.
pub fn home_config(env_key: &str) -> String {
    format!(
        r#"[model_providers.litellm]
name = "litellm"
base_url = "{LITELLM_BASE}"
wire_api = "chat"
env_key = "{env_key}"
requires_openai_auth = false
supports_websockets = false
web_search = "disabled"

model_provider = "litellm"
model = "grok"
model_catalog_json = "vogt-model-catalog.json"
"#
    )
}

/// The router-group catalog WI-1147 writes, one ModelsResponse entry per
/// LiteLLM group that answers a chat completion (WI-1592). The fork sorts
/// by priority ascending and takes the first visible model as the default,
/// so priority is the index in router order and grok stays first. premium
/// is the router's opt-in group, hidden from the picker.
pub fn model_catalog() -> String {
    fn entry(slug: &str, priority: u8, visibility: &str) -> String {
        format!(
            r#"    {{
      "slug": "{slug}",
      "display_name": "{slug}",
      "description": "Estate LiteLLM {slug} group.",
      "supported_reasoning_levels": [],
      "shell_type": "unified_exec",
      "visibility": "{visibility}",
      "supported_in_api": true,
      "priority": {priority},
      "availability_nux": null,
      "upgrade": null,
      "model_messages": {{
        "instructions_template": "You are Codex, running through the estate LiteLLM router as {slug}.",
        "approvals": null,
        "collaboration_modes": null,
        "auto_review": null,
        "permissions": null,
        "multi_agent": null
      }},
      "default_reasoning_summary": "none",
      "support_verbosity": false,
      "default_verbosity": null,
      "apply_patch_tool_type": "freeform",
      "truncation_policy": {{"mode": "bytes", "limit": 10000}},
      "context_window": 256000,
      "effective_context_window_percent": 95,
      "experimental_supported_tools": []
    }}"#
        )
    }
    // Same groups, in the same order, as ROUTER_GROUPS in mcp-bootstrap.sh.
    // Priority is the index: the fork sorts ascending and defaults to the first.
    let groups = [
        "grok",
        "code",
        "small",
        "reasoning",
        "vision",
        "or-deepseek-flash",
        "or-gemini-flash",
        "or-glm-5-3",
        "qwen3-coder",
        "qwen3-8-27b",
        "gpt-oss-120b",
        "dlab-deepseek-v4-pro",
        "dlab-deepseek-v4-1-flash",
        "dlab-grok-4-5",
        "dlab-grok-4-6",
        "dlab-grok-4-7",
        "dlab-gemini-3-8-flash",
        "dlab-glm-5-3",
        "dlab-glm-5-3-flash",
        "dlab-glm-5-3-flashx",
        "dlab-kimi-k3",
        "premium",
    ];
    let body = groups
        .iter()
        .enumerate()
        .map(|(priority, slug)| {
            entry(
                slug,
                priority as u8,
                if *slug == "premium" { "hide" } else { "list" },
            )
        })
        .collect::<Vec<_>>()
        .join(",\n");
    format!("{{\n  \"models\": [\n{body}\n  ]\n}}\n")
}

/// What one server line means for the chat transcript.
#[derive(Debug, Clone, PartialEq)]
pub enum CodexEvent {
    /// `initialize` answered and the thread may be started or resumed.
    Initialized,
    /// `initialize` was refused. The thread request must not be sent: the
    /// fork would reject `threadId` and the next turn would say the thread
    /// was never created.
    InitializeFailed(String),
    /// `thread/start` or `thread/resume` answered.
    ThreadReady,
    /// `thread/start` or `thread/resume` was refused. A later `turn/start`
    /// would answer `thread not found`; the chat records this instead.
    ThreadFailed(String),
    /// Assistant text.
    Assistant(String),
    /// A tool the agent is about to run.
    ToolCall {
        id: String,
        name: String,
        summary: String,
    },
    /// A tool finished.
    ToolResult {
        id: String,
        text: String,
        is_error: bool,
    },
    /// A turn ended. `error` is set when the turn failed.
    TurnEnd { error: Option<String> },
    /// The server asks a person to approve something.
    Approval {
        request_id: String,
        tool: String,
        summary: String,
    },
    /// Something to show, not a turn boundary.
    Notice(String),
    /// Ignore (a response we sent, a notification we do not render).
    Ignore,
}

fn str_field<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

/// Classify one stdout line. Malformed JSON is a notice, not a crash.
pub fn parse_line(line: &str) -> CodexEvent {
    let line = line.trim();
    if line.is_empty() {
        return CodexEvent::Ignore;
    }
    let Ok(msg) = serde_json::from_str::<Value>(line) else {
        return CodexEvent::Notice(line.chars().take(400).collect());
    };
    if let Some(method) = str_field(&msg, "method") {
        return parse_notification(&msg, method);
    }
    if msg.get("id").is_some() && (msg.get("result").is_some() || msg.get("error").is_some()) {
        return parse_response(&msg);
    }
    CodexEvent::Ignore
}

fn parse_notification(msg: &Value, method: &str) -> CodexEvent {
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    if APPROVAL_METHODS.contains(&method) {
        let request_id = str_field(msg, "id").unwrap_or("").to_string();
        let tool = method.split('/').nth(1).unwrap_or("tool").to_string();
        let summary = params
            .get("command")
            .or_else(|| params.get("reason"))
            .or_else(|| params.get("question"))
            .and_then(|v| v.as_str())
            .unwrap_or(method)
            .chars()
            .take(600)
            .collect();
        return CodexEvent::Approval {
            request_id,
            tool,
            summary,
        };
    }
    match method {
        "item/agentMessage/delta" | "item/agentMessage/completed" => {
            let text = params
                .get("delta")
                .or_else(|| params.get("text"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if text.is_empty() {
                CodexEvent::Ignore
            } else {
                CodexEvent::Assistant(text)
            }
        }
        "item/commandExecution/started" | "item/fileChange/started" | "item/tool/started" => {
            let id = str_field(&params, "itemId")
                .or_else(|| str_field(&params, "id"))
                .unwrap_or("")
                .to_string();
            let name = method.split('/').nth(1).unwrap_or("tool").to_string();
            let summary = params
                .get("command")
                .or_else(|| params.get("summary"))
                .and_then(Value::as_str)
                .unwrap_or(&name)
                .to_string();
            CodexEvent::ToolCall { id, name, summary }
        }
        "item/commandExecution/completed" | "item/fileChange/completed" | "item/tool/completed" => {
            let id = str_field(&params, "itemId")
                .or_else(|| str_field(&params, "id"))
                .unwrap_or("")
                .to_string();
            let is_error = params
                .get("status")
                .and_then(Value::as_str)
                .is_some_and(|s| s == "failed" || s == "error");
            let text = params
                .get("output")
                .or_else(|| params.get("text"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            CodexEvent::ToolResult { id, text, is_error }
        }
        "turn/completed" => {
            let error = params
                .get("error")
                .and_then(|e| {
                    e.get("message")
                        .and_then(Value::as_str)
                        .or_else(|| e.as_str())
                })
                .map(str::to_string);
            CodexEvent::TurnEnd { error }
        }
        "turn/started" | "thread/started" => CodexEvent::Ignore,
        other => CodexEvent::Notice(other.to_string()),
    }
}

fn parse_response(msg: &Value) -> CodexEvent {
    if let Some(err) = msg.get("error") {
        let message = err
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("codex request failed");
        // Only `turn/start` failing ends the in-flight turn. `set_model` and
        // `interrupt` send their own requests while a turn can be running, and
        // reading their rejection as a turn boundary would drop the real turn
        // (ids are `turn-`, `model-`, `stop-`, `init`, `start`, `resume`).
        let id = str_field(msg, "id").unwrap_or("");
        if id.starts_with("turn-") {
            return CodexEvent::TurnEnd {
                error: Some(message.to_string()),
            };
        }
        // `thread/start` and `thread/resume` share the `start`/`resume` ids the
        // handshake sends. Their refusal means the thread was never created, so
        // the follow-on `turn/start` (`thread not found`) is the same failure.
        if id == "start" || id == "resume" {
            return CodexEvent::ThreadFailed(message.to_string());
        }
        if id == "init" {
            return CodexEvent::InitializeFailed(message.to_string());
        }
        return CodexEvent::Notice(message.to_string());
    }
    let result = msg.get("result").cloned().unwrap_or(Value::Null);
    // initialize / thread/start / thread/resume all look like a successful
    // result with no turn. The caller treats ThreadReady and Initialized the
    // same for the transcript: nothing to append.
    if result.get("threadId").is_some() || result.get("thread").is_some() {
        return CodexEvent::ThreadReady;
    }
    if result.get("userAgent").is_some()
        || result.get("capabilities").is_some()
        || result.is_object()
    {
        return CodexEvent::Initialized;
    }
    CodexEvent::Ignore
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialize_declares_experimental_api_before_the_thread() {
        // Recorded from the fork's app-server (WI-1632). Without
        // capabilities.experimentalApi, thread/start.threadId is rejected with
        // "thread/start.threadId requires experimentalApi capability" and the
        // turn that follows answers "thread not found".
        let id = Uuid::nil();
        let init: Value = serde_json::from_str(&initialize("init")).unwrap();
        assert_eq!(init["method"], "initialize");
        assert_eq!(init["params"]["capabilities"]["experimentalApi"], true);

        let start: Value = serde_json::from_str(&thread_start("start", id, Some("grok"))).unwrap();
        assert_eq!(start["method"], "thread/start");
        assert_eq!(start["params"]["threadId"], id.to_string());

        let initialized = r#"{"jsonrpc":"2.0","id":"init","result":{"userAgent":"0.153.4-vogt.1","capabilities":{"experimentalApi":true}}}"#;
        assert_eq!(parse_line(initialized), CodexEvent::Initialized);

        let ready =
            format!(r#"{{"jsonrpc":"2.0","id":"start","result":{{"thread":{{"id":"{id}"}}}}}}"#);
        assert_eq!(parse_line(&ready), CodexEvent::ThreadReady);

        let rejected = r#"{"jsonrpc":"2.0","id":"start","error":{"code":-32600,"message":"thread/start.threadId requires experimentalApi capability"}}"#;
        assert_eq!(
            parse_line(rejected),
            CodexEvent::ThreadFailed(
                "thread/start.threadId requires experimentalApi capability".into()
            )
        );
        let missing = format!(
            r#"{{"jsonrpc":"2.0","id":"turn-1","error":{{"code":-32600,"message":"thread not found: {id}"}}}}"#
        );
        assert_eq!(
            parse_line(&missing),
            CodexEvent::TurnEnd {
                error: Some(format!("thread not found: {id}"))
            }
        );
        let handshake = r#"{"jsonrpc":"2.0","id":"init","error":{"message":"initialize refused"}}"#;
        assert_eq!(
            parse_line(handshake),
            CodexEvent::InitializeFailed("initialize refused".into())
        );
    }

    #[test]
    fn turn_start_names_the_chat_thread() {
        let id = Uuid::nil();
        let line = turn_start("t1", id, "hello");
        let v: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["method"], "turn/start");
        assert_eq!(v["params"]["threadId"], id.to_string());
        assert_eq!(v["params"]["input"][0]["text"], "hello");
    }

    #[test]
    fn approval_is_not_auto_allowed() {
        let line = r#"{"jsonrpc":"2.0","id":"a1","method":"item/commandExecution/requestApproval","params":{"command":"rm -rf /"}}"#;
        match parse_line(line) {
            CodexEvent::Approval {
                request_id,
                tool,
                summary,
            } => {
                assert_eq!(request_id, "a1");
                assert_eq!(tool, "commandExecution");
                assert!(summary.contains("rm -rf"));
            }
            other => panic!("expected approval, got {other:?}"),
        }
        let decision: Value = serde_json::from_str(&approval_decision("a1", false)).unwrap();
        assert_eq!(decision["result"]["decision"], "decline");
    }

    #[test]
    fn turn_completed_ends_the_turn() {
        let line = r#"{"jsonrpc":"2.0","method":"turn/completed","params":{}}"#;
        assert_eq!(parse_line(line), CodexEvent::TurnEnd { error: None });
    }

    #[test]
    fn only_a_turn_error_ends_the_turn() {
        let turn = r#"{"jsonrpc":"2.0","id":"turn-1","error":{"message":"rejected"}}"#;
        assert_eq!(
            parse_line(turn),
            CodexEvent::TurnEnd {
                error: Some("rejected".into())
            }
        );
        let model = r#"{"jsonrpc":"2.0","id":"model-1","error":{"message":"unknown model"}}"#;
        assert_eq!(
            parse_line(model),
            CodexEvent::Notice("unknown model".into())
        );
        let stop = r#"{"jsonrpc":"2.0","id":"stop-1","error":{"message":"no turn"}}"#;
        assert_eq!(parse_line(stop), CodexEvent::Notice("no turn".into()));
    }

    #[test]
    fn home_config_uses_the_chat_key_name() {
        let cfg = home_config("ENGINE_CHAT_PROVIDER_KEY");
        assert!(cfg.contains("env_key = \"ENGINE_CHAT_PROVIDER_KEY\""));
        assert!(cfg.contains(LITELLM_BASE));
        assert!(!cfg.contains("HOMELAB_LITELLM_SESSION_KEY"));
        let catalog: Value = serde_json::from_str(&model_catalog()).unwrap();
        let models = catalog["models"].as_array().unwrap();
        let mut sorted = models.clone();
        sorted.sort_by_key(|model| model["priority"].as_i64().unwrap());
        assert_eq!(
            sorted
                .iter()
                .map(|model| model["slug"].as_str().unwrap())
                .collect::<Vec<_>>(),
            [
                "grok",
                "code",
                "small",
                "reasoning",
                "vision",
                "or-deepseek-flash",
                "or-gemini-flash",
                "or-glm-5-3",
                "qwen3-coder",
                "qwen3-8-27b",
                "gpt-oss-120b",
                "dlab-deepseek-v4-pro",
                "dlab-deepseek-v4-1-flash",
                "dlab-grok-4-5",
                "dlab-grok-4-6",
                "dlab-grok-4-7",
                "dlab-gemini-3-8-flash",
                "dlab-glm-5-3",
                "dlab-glm-5-3-flash",
                "dlab-glm-5-3-flashx",
                "dlab-kimi-k3",
                "premium",
            ]
        );
        // The fork defaults to the first visible model after an ascending sort.
        assert_eq!(sorted[0]["slug"], "grok");
        assert_eq!(sorted[0]["visibility"], "list");
        assert_eq!(sorted.last().unwrap()["slug"], "premium");
        assert_eq!(sorted.last().unwrap()["visibility"], "hide");
        assert!(sorted
            .iter()
            .all(|model| model["apply_patch_tool_type"] == "freeform"
                && model.get("tool_mode").is_none()));
    }

    #[test]
    fn argv_is_app_server_stdio() {
        assert_eq!(
            app_server_argv("codex"),
            vec!["codex", "app-server", "--stdio"]
        );
    }
}
