//! MCP over streamable HTTP. Ports `adapters/mcp/http.py`.
//!
//! The same dispatcher as the stdio transport answers here; what this layer
//! adds is the HTTP envelope and the grant. A notification is a 202 with no
//! body, bad JSON is a 200 carrying a protocol error, and `tools/list` returns
//! exactly the tools this caller's grant permits — an ungranted tool is absent
//! rather than present and refusing. `tools/call` checks the same grant again,
//! so a tool hidden from the list cannot be reached by naming it.
//!
//! There is no HTTP framework in this crate, so mounting the route is a
//! function from a parsed body plus a grant to a response. Resolving the
//! request into a caller and a grant — the token, the session, the recorded
//! authorization — belongs to the front door that authenticates the request.

use serde_json::{json, Map, Value};

use super::framing::{Dispatcher, McpTransport, ToolGrant};

/// Where the route is mounted, matching the Python default.
pub const MCP_PATH: &str = "/mcp";

/// A notification. The streamable-HTTP shape is 202 with no body.
const ACCEPTED: u16 = 202;

/// Everything else, including a protocol error, is a 200 with a JSON body.
const OK: u16 = 200;

/// One HTTP response from the MCP route.
pub struct McpHttpResponse {
    pub status: u16,
    /// The body, and how to send it. `None` for a notification, whose 202
    /// carries nothing.
    pub body: Option<McpBody>,
    /// The authorization to record, set on a `tools/call` that reached an
    /// operation. The front door writes it; the route only decides it.
    pub decision: Option<AuthDecisionRecord>,
}

/// What the body is. A normal answer is JSON; a 500 is the bare text Starlette
/// sends, and encoding it as JSON would wrap it in quotes.
#[derive(Debug)]
pub enum McpBody {
    Json(Value),
    /// `text/plain; charset=utf-8`, exactly as written.
    Text(String),
}

/// Answer one request body with the streamable-HTTP envelope.
///
/// `body` is the raw request body. Anything that is not a JSON object comes
/// back as a 200 carrying an invalid-params error, never as a transport
/// failure — the client has to be able to read why.
pub fn handle_http<G: ToolGrant>(
    body: &str,
    registry: &crate::registry::OperationRegistry,
    grant: &G,
) -> McpHttpResponse {
    let message: Value = match serde_json::from_str(body) {
        Ok(message) => message,
        Err(_) => {
            return json_response(error(None, "body is not valid JSON"));
        }
    };
    let Value::Object(message) = message else {
        return json_response(error(None, "message must be a JSON object"));
    };
    respond(&message, registry, grant)
}

/// Answer one already-parsed message.
pub fn respond<G: ToolGrant>(
    message: &Map<String, Value>,
    registry: &crate::registry::OperationRegistry,
    grant: &G,
) -> McpHttpResponse {
    respond_recording(message, registry, grant, &mut ())
}

/// Answer one message, recording the authorization before the call runs.
///
/// `authorize()` writes its row and only then runs the operation, so a store
/// that refuses the write fails the request closed: the call never happens.
/// The recorder is invoked first for that reason. A recorder that cannot write
/// returns `Err`, and the route answers 500 without dispatching.
pub fn respond_recording<G: ToolGrant, R: AuthRecorder>(
    message: &Map<String, Value>,
    registry: &crate::registry::OperationRegistry,
    grant: &G,
    recorder: &mut R,
) -> McpHttpResponse {
    let decision = decision_for(message, registry, grant);
    if let Some(decision) = &decision {
        if let Err(failure) = recorder.record(decision) {
            // The store's own text stays server-side. A remote caller gets the
            // plain 500 Starlette would have sent, not a SQLite message and not
            // an id that failed to normalise.
            // Server-side only. The caller gets the plain text, never the
            // store's own words. `tracing` reaches a subscriber only once the
            // front door installs one; until then the line is a no-op.
            tracing::error!("mcp authorization could not be recorded: {failure}");
            return McpHttpResponse {
                status: 500,
                body: Some(McpBody::Text("Internal Server Error".to_owned())),
                decision: None,
            };
        }
    }
    let mut dispatcher = Dispatcher::new(registry, grant, McpTransport::Http);
    match dispatcher.handle(message) {
        Some(response) => McpHttpResponse {
            decision,
            ..json_response(response)
        },
        None => McpHttpResponse {
            status: ACCEPTED,
            body: None,
            decision: None,
        },
    }
}

fn json_response(body: Value) -> McpHttpResponse {
    McpHttpResponse {
        status: OK,
        body: Some(McpBody::Json(body)),
        decision: None,
    }
}

fn error(id: Option<Value>, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {"code": -32602, "message": message},
    })
}

/// The caller's grant, narrowed to the scopes it holds.
///
/// Scope implication follows `core/auth.py`: `admin` implies every scope, and
/// each write scope implies `read`. Nothing else implies anything —
/// `work.write` does not grant `project.write`. A mutating operation
/// additionally needs writes enabled, so a server started read-only shows no
/// writes no matter what the token says. Operations the MCP transport does not
/// carry are excluded before either check.
pub struct ScopeGrant {
    scopes: Vec<crate::registry::Scope>,
    writes_enabled: bool,
}

impl ScopeGrant {
    pub fn new(scopes: Vec<crate::registry::Scope>, writes_enabled: bool) -> Self {
        Self {
            scopes,
            writes_enabled,
        }
    }

    fn effective(&self) -> Vec<crate::registry::Scope> {
        use crate::registry::Scope;
        let mut granted = Vec::new();
        for scope in &self.scopes {
            let implied: &[Scope] = match scope {
                Scope::Admin => &[
                    Scope::Read,
                    Scope::WorkWrite,
                    Scope::ProjectWrite,
                    Scope::Admin,
                    Scope::Writeback,
                ],
                Scope::ProjectWrite => &[Scope::ProjectWrite, Scope::Read],
                Scope::WorkWrite => &[Scope::WorkWrite, Scope::Read],
                Scope::Writeback => &[Scope::Writeback, Scope::Read],
                Scope::Read => &[Scope::Read],
            };
            for scope in implied {
                if !granted.contains(scope) {
                    granted.push(*scope);
                }
            }
        }
        granted
    }
}

impl ToolGrant for ScopeGrant {
    fn allows(
        &self,
        registry: &crate::registry::OperationRegistry,
        operation: &crate::registry::Operation,
    ) -> bool {
        super::framing::exposed_over_mcp(registry, operation) && self.permitted(operation)
    }

    fn writes_enabled(&self) -> bool {
        self.writes_enabled
    }

    fn denial(&self, operation: &crate::registry::Operation) -> String {
        if operation.mutating && !self.writes_enabled {
            format!(
                "{} is a write, and this server was started read-only",
                operation.name
            )
        } else {
            // `sorted(frozenset(scopes))`: deduplicated and alphabetical, so
            // the order the token was issued in never reaches the message.
            let mut held_scopes: Vec<&str> =
                self.scopes.iter().map(|scope| scope.as_str()).collect();
            held_scopes.sort_unstable();
            held_scopes.dedup();
            let held = held_scopes.join(", ");
            format!(
                "{} requires the {} scope; this token holds {}",
                operation.name,
                super::framing::python_repr(operation.scope.as_str()),
                if held.is_empty() { "nothing" } else { &held }
            )
        }
    }
}

impl ScopeGrant {
    /// The scope and the writes gate, without the transport exclusion. An
    /// operation the transport does not carry is an unknown tool, not a
    /// refusal, so the two checks stay separate.
    fn permitted(&self, operation: &crate::registry::Operation) -> bool {
        self.effective().contains(&operation.scope) && (!operation.mutating || self.writes_enabled)
    }
}

/// One recorded authorization, matching the row `authorize()` writes.
///
/// `services/auth.py` records an allow or a deny for every `tools/call` that
/// reaches a real operation, with transport `mcp-http`, before the call runs.
/// The route has no store of its own, so it hands the decision to whoever
/// mounted it; dropping it would leave the audit trail short exactly where a
/// refusal is most worth recording.
#[derive(Clone)]
pub struct AuthDecisionRecord {
    pub decision: &'static str,
    pub reason_code: &'static str,
    pub operation: String,
    pub scope: String,
    pub transport: &'static str,
}

pub const MCP_HTTP_TRANSPORT: &str = "mcp-http";

/// Where an authorization is written. The front door supplies one backed by the
/// declared store; the default records nothing, which is what a test wants.
pub trait AuthRecorder {
    fn record(&mut self, decision: &AuthDecisionRecord) -> Result<(), String>;
}

impl AuthRecorder for () {
    fn record(&mut self, _decision: &AuthDecisionRecord) -> Result<(), String> {
        Ok(())
    }
}

/// The decision for this message, or `None` when `authorize()` would not have
/// been reached.
///
/// Python checks the arguments before it authorizes, so a call whose arguments
/// are not an object is a `-32602` and records nothing — even when the tool
/// exists and the grant would have allowed it.
fn decision_for<G: ToolGrant>(
    message: &Map<String, Value>,
    registry: &crate::registry::OperationRegistry,
    grant: &G,
) -> Option<AuthDecisionRecord> {
    if message.get("method").and_then(Value::as_str) != Some("tools/call") {
        return None;
    }
    // A notification is answered before the call, so it is never authorized and
    // never recorded. That includes a null id: Python's `raw_id is None` holds
    // for both an absent key and a JSON null.
    message.get("id").filter(|id| !id.is_null())?;
    let params = message.get("params").and_then(Value::as_object)?;
    let name = params.get("name").and_then(Value::as_str)?;
    if let Some(arguments) = params
        .get("arguments")
        .filter(|value| !super::framing::json_falsy(value))
    {
        if !arguments.is_object() {
            return None;
        }
    }
    authorize(registry, grant, name)
}

/// The decision `authorize()` would record for this call, or `None` when the
/// call never reached an operation (bad arguments, an unknown tool).
pub fn authorize<G: ToolGrant>(
    registry: &crate::registry::OperationRegistry,
    grant: &G,
    name: &str,
) -> Option<AuthDecisionRecord> {
    let operation = registry.by_mcp_tool(name).ok()?;
    if !super::framing::exposed_over_mcp(registry, operation) {
        return None;
    }
    // The same order as `Grant.allows`: the writes gate first, then the
    // scope. A write the token has no scope for is `missing_scope` when writes
    // are enabled, not `writes_disabled`.
    let (allowed, reason_code) = if !grant.allows(registry, operation) {
        (
            false,
            if operation.mutating && !grant.writes_enabled() {
                "writes_disabled"
            } else {
                "missing_scope"
            },
        )
    } else {
        (true, "token_valid")
    };
    Some(AuthDecisionRecord {
        decision: if allowed { "allow" } else { "deny" },
        reason_code,
        operation: operation.name.to_owned(),
        scope: operation.scope.as_str().to_owned(),
        transport: MCP_HTTP_TRANSPORT,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::operations::build_operations;
    use crate::registry::{OperationRegistry, Scope};

    fn registry() -> OperationRegistry {
        OperationRegistry::new(build_operations()).unwrap()
    }

    fn json_body(response: &McpHttpResponse) -> &Value {
        match &response.body {
            Some(McpBody::Json(value)) => value,
            _ => panic!("expected a JSON body"),
        }
    }

    fn message(value: Value) -> Map<String, Value> {
        match value {
            Value::Object(map) => map,
            _ => unreachable!(),
        }
    }

    #[test]
    fn bad_json_is_a_200_carrying_the_error() {
        let response = handle_http("not json", &registry(), &ScopeGrant::new(vec![], false));
        assert_eq!(response.status, OK);
        let body = json_body(&response);
        assert_eq!(body["error"]["code"], -32602);
        assert!(body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("valid JSON"));
    }

    #[test]
    fn a_notification_is_a_202_with_no_body() {
        let response = respond(
            &message(json!({"jsonrpc": "2.0", "method": "notifications/initialized"})),
            &registry(),
            &ScopeGrant::new(vec![Scope::Read], false),
        );
        assert_eq!(response.status, ACCEPTED);
        assert!(response.body.is_none());
    }

    #[test]
    fn tools_list_hides_what_the_grant_does_not_allow() {
        let response = respond(
            &message(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"})),
            &registry(),
            &ScopeGrant::new(vec![Scope::Read], false),
        );
        let tools = json_body(&response)["result"]["tools"].clone();
        let names: Vec<&str> = tools
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"work_list"), "{names:?}");
        assert!(
            !names.contains(&"work_create"),
            "a read-only grant does not see a write"
        );
        assert!(!names.contains(&"init"), "local-only stays invisible");
        assert!(
            !names.contains(&"session_token"),
            "http-only stays invisible"
        );
    }

    #[test]
    fn a_write_scope_implies_read_and_a_read_only_server_hides_writes() {
        let read_write = respond(
            &message(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"})),
            &registry(),
            &ScopeGrant::new(vec![Scope::WorkWrite], true),
        );
        let names: Vec<&str> = json_body(&read_write)["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"work_list"), "work.write implies read");
        assert!(names.contains(&"work_create"));

        let frozen = respond(
            &message(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"})),
            &registry(),
            &ScopeGrant::new(vec![Scope::Admin], false),
        );
        let frozen_names: Vec<&str> = json_body(&frozen)["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        assert!(frozen_names.contains(&"work_list"));
        assert!(
            !frozen_names.contains(&"work_create"),
            "writes disabled hides a write even from admin"
        );
    }

    #[test]
    fn a_call_outside_the_grant_is_forbidden() {
        let response = respond(
            &message(json!({
                "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": {"name": "work_create", "arguments": {}}
            })),
            &registry(),
            &ScopeGrant::new(vec![Scope::Read], false),
        );
        let body = json_body(&response);
        assert_eq!(response.status, OK);
        assert_eq!(body["result"]["isError"], true);
        let text = body["result"]["content"][0]["text"].as_str().unwrap();
        assert!(
            text.contains("work.create is a write, and this server was started read-only"),
            "{text}"
        );
        let decision = response.decision.unwrap();
        assert_eq!(decision.decision, "deny");
        assert_eq!(decision.reason_code, "writes_disabled");
        assert_eq!(decision.transport, "mcp-http");
    }

    #[test]
    fn a_missing_scope_names_what_the_token_holds() {
        let response = respond(
            &message(json!({
                "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": {"name": "work_list", "arguments": {}}
            })),
            &registry(),
            &ScopeGrant::new(vec![], true),
        );
        let text = json_body(&response)["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(text.contains("requires the 'read' scope"), "{text}");
        assert!(text.contains("holds nothing"), "{text}");
        assert_eq!(response.decision.unwrap().reason_code, "missing_scope");
    }

    #[test]
    fn an_allowed_call_records_an_allow() {
        let response = respond(
            &message(json!({
                "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": {"name": "work_list"}
            })),
            &registry(),
            &ScopeGrant::new(vec![Scope::Read], false),
        );
        let decision = response.decision.unwrap();
        assert_eq!(decision.decision, "allow");
        assert_eq!(decision.reason_code, "token_valid");
        assert_eq!(decision.operation, "work.list");
    }

    #[test]
    fn a_null_id_is_a_notification_and_a_bad_id_is_echoed_as_null() {
        let silent = respond(
            &message(json!({"jsonrpc": "2.0", "id": null, "method": "ping"})),
            &registry(),
            &ScopeGrant::new(vec![Scope::Read], false),
        );
        assert_eq!(silent.status, ACCEPTED);
        assert!(silent.body.is_none());

        let answered = respond(
            &message(json!({"jsonrpc": "2.0", "id": 1.5, "method": "ping"})),
            &registry(),
            &ScopeGrant::new(vec![Scope::Read], false),
        );
        assert_eq!(json_body(&answered)["id"], Value::Null);

        let object_id = respond(
            &message(json!({"jsonrpc": "2.0", "id": {"x": 1}, "method": "nope"})),
            &registry(),
            &ScopeGrant::new(vec![Scope::Read], false),
        );
        let body = json_body(&object_id);
        assert_eq!(body["id"], Value::Null);
        assert_eq!(body["error"]["code"], -32601);
    }

    #[test]
    fn a_write_without_the_scope_is_missing_scope_when_writes_are_on() {
        let response = respond(
            &message(json!({
                "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": {"name": "work_create", "arguments": {}}
            })),
            &registry(),
            &ScopeGrant::new(vec![Scope::Read], true),
        );
        assert_eq!(response.decision.unwrap().reason_code, "missing_scope");
    }

    #[test]
    fn bad_arguments_record_nothing() {
        let response = respond(
            &message(json!({
                "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": {"name": "work_list", "arguments": [1]}
            })),
            &registry(),
            &ScopeGrant::new(vec![Scope::Read], false),
        );
        assert_eq!(json_body(&response)["error"]["code"], -32602);
        assert!(response.decision.is_none());
    }

    #[test]
    fn the_held_scopes_are_sorted_and_deduplicated() {
        let response = respond(
            &message(json!({
                "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": {"name": "work_create"}
            })),
            &registry(),
            &ScopeGrant::new(
                vec![Scope::Writeback, Scope::ProjectWrite, Scope::ProjectWrite],
                true,
            ),
        );
        let text = json_body(&response)["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(text.contains("holds project.write, writeback"), "{text}");
        let _ = response;
    }

    #[test]
    fn a_float_zero_argument_is_falsy_and_still_recorded() {
        let response = respond(
            &message(json!({
                "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": {"name": "work_list", "arguments": 0.0}
            })),
            &registry(),
            &ScopeGrant::new(vec![Scope::Read], false),
        );
        assert!(json_body(&response).get("error").is_none());
        assert!(
            response.decision.is_some(),
            "0.0 is falsy, so the call runs and is recorded"
        );
    }

    #[test]
    fn a_notification_records_nothing_even_when_the_store_fails() {
        struct Refusing;
        impl AuthRecorder for Refusing {
            fn record(&mut self, _: &AuthDecisionRecord) -> Result<(), String> {
                Err("down".to_owned())
            }
        }
        let response = respond_recording(
            &message(
                json!({"jsonrpc": "2.0", "method": "tools/call", "params": {"name": "work_list"}}),
            ),
            &registry(),
            &ScopeGrant::new(vec![Scope::Read], false),
            &mut Refusing,
        );
        assert_eq!(response.status, ACCEPTED);
        assert!(response.decision.is_none());
    }

    #[test]
    fn a_recorder_that_fails_stops_the_call() {
        struct Refusing;
        impl AuthRecorder for Refusing {
            fn record(&mut self, _: &AuthDecisionRecord) -> Result<(), String> {
                Err("the store refused the decision".to_owned())
            }
        }
        let response = respond_recording(
            &message(json!({
                "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": {"name": "work_list"}
            })),
            &registry(),
            &ScopeGrant::new(vec![Scope::Read], false),
            &mut Refusing,
        );
        assert_eq!(response.status, 500);
        assert!(response.decision.is_none());
    }

    #[test]
    fn an_unknown_tool_is_a_protocol_error_and_records_nothing() {
        let response = respond(
            &message(json!({
                "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": {"name": "session_token"}
            })),
            &registry(),
            &ScopeGrant::new(vec![Scope::Admin], true),
        );
        let body = json_body(&response);
        assert_eq!(body["error"]["code"], -32601);
        assert!(body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("'session_token'"));
        assert!(response.decision.is_none());
    }
}
