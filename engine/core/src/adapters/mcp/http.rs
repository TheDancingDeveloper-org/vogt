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
    /// `None` for a notification, whose 202 carries no body.
    pub body: Option<Value>,
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
    match Dispatcher::new(registry, grant, McpTransport::Http).handle(message) {
        Some(response) => json_response(response),
        None => McpHttpResponse {
            status: ACCEPTED,
            body: None,
        },
    }
}

fn json_response(body: Value) -> McpHttpResponse {
    McpHttpResponse {
        status: OK,
        body: Some(body),
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
        if !super::framing::exposed_over_mcp(registry, operation) {
            return false;
        }
        self.effective().contains(&operation.scope) && (!operation.mutating || self.writes_enabled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::operations::build_operations;
    use crate::registry::{OperationRegistry, Scope};

    fn registry() -> OperationRegistry {
        OperationRegistry::new(build_operations()).unwrap()
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
        let body = response.body.unwrap();
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
        let tools = response.body.unwrap()["result"]["tools"].clone();
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
        )
        .body
        .unwrap();
        let names: Vec<&str> = read_write["result"]["tools"]
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
        )
        .body
        .unwrap();
        let frozen_names: Vec<&str> = frozen["result"]["tools"]
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
        let body = response.body.unwrap();
        assert_eq!(response.status, OK);
        assert_eq!(body["result"]["isError"], true);
        assert!(body["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with("forbidden"));
    }
}
