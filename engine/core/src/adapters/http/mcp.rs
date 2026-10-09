//! The `/mcp` route. Ports the mounting half of `adapters/mcp/http.py`.
//!
//! The protocol lives in `adapters::mcp::framing`; this carries it over axum and
//! puts `adapters::auth_gate` in front of it. That is the same gate the `/api`
//! routes use. The scope grant and the recorder that used to live beside the
//! dispatcher duplicated it, so the route does not use them: every decision is
//! the gate's, recorded into the declared store, and a decision that cannot be
//! recorded fails closed.
//!
//! A body that does not parse is answered before the gate. A malformed message
//! is a protocol error even with no token, and there is nothing to record.
//! Every request that does parse is authenticated first, the way Python's
//! `resolve()` does, so an anonymous caller is refused before any method runs.
//! A `tools/call` that reaches an operation is then authorized against that
//! operation, and the gate writes the row before the call runs.
//!
//! No credential, or one that does not resolve, is a `401`. A live credential
//! that lacks the scope, or a write against a read-only instance, comes back as
//! the forbidden tool-result, because the model has to be able to read why and
//! try something else. A decision that cannot be recorded is the bare text
//! `500`, and the call does not run.

use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, StatusCode};
use axum::response::Response;
use axum::routing::post;
use axum::Router;
use serde_json::{Map, Value};

use crate::adapters::auth_gate::{self, Denial, Request as AuthRequest};
use crate::adapters::mcp::framing::{Dispatcher, McpTransport, ToolGrant};
use crate::adapters::mcp::http::MCP_PATH;
use crate::core::{IdFactory, Moment, SystemClock};
use crate::registry::{Operation, OperationRegistry, Transport};
use crate::storage::sqlite::declared::SqliteDeclaredStore;

/// What the route needs. The store is behind a mutex because its clock and id
/// factory are not shareable across tasks.
pub struct McpState<I> {
    store: Mutex<SqliteDeclaredStore<SystemClock, I>>,
    pub no_auth: bool,
    pub writes_enabled: bool,
}

impl<I: IdFactory> McpState<I> {
    pub fn new(data_dir: &std::path::Path, no_auth: bool, writes_enabled: bool, ids: I) -> Self {
        Self {
            store: Mutex::new(SqliteDeclaredStore::new(
                crate::storage::sqlite::declared_path(data_dir),
                SystemClock,
                ids,
            )),
            no_auth,
            writes_enabled,
        }
    }
}

/// The route, gated by the declared store.
pub fn router<I: IdFactory + Send + 'static>(state: McpState<I>) -> Router {
    Router::new()
        .route(MCP_PATH, post(handle))
        .with_state(Arc::new(state))
}

async fn handle<I: IdFactory>(
    State(state): State<Arc<McpState<I>>>,
    request: Request<Body>,
) -> Response {
    let presented = bearer(request.headers().get("authorization"));
    let body = axum::body::to_bytes(request.into_body(), usize::MAX)
        .await
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .unwrap_or_default();
    let registry = crate::registry::default_registry();
    let Ok(Value::Object(message)) = serde_json::from_str::<Value>(&body) else {
        // Not a message, so there is nothing to authorize and nothing to
        // record. The client still has to be able to read why.
        return json_response(
            StatusCode::OK,
            serde_json::json!({
                "jsonrpc": "2.0", "id": null,
                "error": {"code": -32602, "message": "body is not a JSON object"}
            }),
        );
    };

    let store = state.store.lock().expect("the store lock is not poisoned");
    // Authenticate before dispatch. Recorded against `status`, because this row
    // says who asked rather than which tool they named.
    if let Err(denial) = authorize(
        &store,
        &state,
        presented.as_deref(),
        status_operation(&registry),
    ) {
        return unauthenticated(&message, denial);
    }
    // A tool call that reaches an operation is authorized against it, and the
    // gate records that decision before the call runs.
    if let Some(operation) = called_operation(&message, &registry) {
        if let Err(denial) = authorize(&store, &state, presented.as_deref(), operation) {
            return refusal(&message, denial);
        }
    }
    drop(store);

    // Both checks passed, so the caller may do what the message asks. The
    // dispatcher still owns the protocol: a notification is a 202, a bad
    // argument is `-32602`, an unknown tool is `-32601`.
    let grant = Permitted {
        writes_enabled: state.writes_enabled,
    };
    let mut dispatcher = Dispatcher::new(&registry, &grant, McpTransport::Http);
    match dispatcher.handle(&message) {
        None => Response::builder()
            .status(StatusCode::ACCEPTED)
            .body(Body::empty())
            .expect("a fixed response builds"),
        Some(value) => json_response(StatusCode::OK, value),
    }
}

/// Run one request through the shared gate and record the decision.
fn authorize<I: IdFactory>(
    store: &SqliteDeclaredStore<SystemClock, I>,
    state: &McpState<I>,
    presented: Option<&str>,
    operation: &Operation,
) -> Result<(), Denial> {
    let now = Moment::from_unix(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs() as i64)
            .unwrap_or(0),
        0,
    );
    let decision_id = crate::core::fresh_id("aud");
    auth_gate::authorize(
        store,
        AuthRequest {
            operation,
            transport: Transport::Mcp,
            presented,
            no_auth: state.no_auth,
            writes_enabled: state.writes_enabled,
            now,
            decision_id: &decision_id,
        },
    )
    .map(|_| ())
}

/// The operation the authentication check is recorded against. It is a read, so
/// the check says whether the credential is live and never whether writes are
/// switched on.
fn status_operation(registry: &OperationRegistry) -> &Operation {
    registry
        .get("status")
        .expect("status is a registered operation")
}

/// The operation a `tools/call` names, when the call reaches one.
///
/// A call whose arguments are not an object, or whose name is not a tool the
/// MCP transport carries, never reaches an operation: it is a protocol error
/// and records no authorization. That is the order `http.py` uses.
fn called_operation<'a>(
    message: &Map<String, Value>,
    registry: &'a OperationRegistry,
) -> Option<&'a Operation> {
    if message.get("method").and_then(Value::as_str) != Some("tools/call") {
        return None;
    }
    // A notification, including a null id, is answered before the call.
    message.get("id").filter(|id| !id.is_null())?;
    let params = message.get("params").and_then(Value::as_object)?;
    let name = params.get("name").and_then(Value::as_str)?;
    if let Some(arguments) = params.get("arguments").filter(|value| !json_falsy(value)) {
        if !arguments.is_object() {
            return None;
        }
    }
    let operation = registry.by_mcp_tool(name).ok()?;
    registry
        .transports_for(operation.name)
        .contains(&Transport::Mcp)
        .then_some(operation)
}

/// Truthiness for a JSON value, matching `params or {}`: null, false, zero, an
/// empty string and an empty array are all absent.
fn json_falsy(value: &Value) -> bool {
    match value {
        Value::Null | Value::Bool(false) => true,
        Value::Number(number) => number.as_i64() == Some(0) || number.as_f64() == Some(0.0),
        Value::String(text) => text.is_empty(),
        Value::Array(items) => items.is_empty(),
        _ => false,
    }
}

/// A grant for a caller the gate has already allowed.
///
/// The gate decided the scope, so this allows every operation the MCP transport
/// carries. It still refuses a write when the instance is read-only, because
/// that switch is per request and the dispatcher is what turns it into the
/// tool-result the caller reads.
struct Permitted {
    writes_enabled: bool,
}

impl ToolGrant for Permitted {
    fn allows(&self, _registry: &OperationRegistry, operation: &Operation) -> bool {
        !operation.mutating || self.writes_enabled
    }

    fn writes_enabled(&self) -> bool {
        self.writes_enabled
    }
}

/// A missing or rejected credential. The request never became a caller, so this
/// is a `401` rather than a tool result.
fn unauthenticated(message: &Map<String, Value>, denial: Denial) -> Response {
    json_response(
        StatusCode::UNAUTHORIZED,
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": message.get("id").cloned().unwrap_or(Value::Null),
            "error": {"code": -32001, "message": denial.error().message()},
        }),
    )
}

/// A call the gate refused after the caller had authenticated.
fn refusal(message: &Map<String, Value>, denial: Denial) -> Response {
    match denial {
        Denial::Unrecorded { .. } => Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .header("content-type", "text/plain; charset=utf-8")
            .body(Body::from("Internal Server Error"))
            .expect("a fixed response builds"),
        Denial::Forbidden { .. } | Denial::WritesDisabled => json_response(
            StatusCode::OK,
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": message.get("id").cloned().unwrap_or(Value::Null),
                "result": {
                    "content": [{
                        "type": "text",
                        "text": format!("forbidden: {}", denial.error().message())
                    }],
                    "isError": true,
                },
            }),
        ),
        // The same credential just authenticated, so these do not arise on the
        // second check. Refused anyway: assuming they cannot is how a hole opens.
        Denial::NoBearer | Denial::Rejected { .. } => unauthenticated(message, denial),
    }
}

fn bearer(header: Option<&axum::http::HeaderValue>) -> Option<String> {
    let text = header?.to_str().ok()?;
    let (scheme, secret) = text.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let secret = secret.trim();
    (!secret.is_empty()).then(|| secret.to_string())
}

fn json_response(status: StatusCode, body: Value) -> Response {
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("a fixed response builds")
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::core::FreshIds;
    use crate::storage::interface::{DeclaredStore, ReadView};

    struct Running {
        addr: std::net::SocketAddr,
        _runtime: tokio::runtime::Runtime,
        dir: std::path::PathBuf,
    }

    impl Drop for Running {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn serve(no_auth: bool) -> Running {
        let dir = std::env::temp_dir().join(format!("vogt-mcp-{}", crate::core::fresh_id("t")));
        std::fs::create_dir_all(&dir).unwrap();
        crate::application::instance::init(&dir, &mut None, &mut None).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let state = McpState::new(&dir, no_auth, true, FreshIds);
        runtime.spawn(async move {
            let listener = tokio::net::TcpListener::from_std(listener).unwrap();
            axum::serve(listener, router(state)).await.unwrap();
        });
        std::thread::sleep(std::time::Duration::from_millis(100));
        Running {
            addr,
            _runtime: runtime,
            dir,
        }
    }

    fn post(addr: std::net::SocketAddr, body: &str) -> (u16, String) {
        use std::io::{Read, Write};
        let mut stream = std::net::TcpStream::connect(addr).unwrap();
        write!(
            stream,
            "POST {MCP_PATH} HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        let mut buf = String::new();
        stream.read_to_string(&mut buf).unwrap();
        let (head, response_body) = buf.split_once("\r\n\r\n").unwrap();
        let status = head.split_whitespace().nth(1).unwrap().parse().unwrap();
        (status, response_body.to_string())
    }

    fn decisions(dir: &std::path::Path) -> Vec<crate::core::AuthDecision> {
        let store = SqliteDeclaredStore::new(
            crate::storage::sqlite::declared_path(dir),
            SystemClock,
            FreshIds,
        );
        store.read().unwrap().list_auth_decisions(None, 10).unwrap()
    }

    #[test]
    fn an_anonymous_tool_call_is_refused_and_recorded() {
        let running = serve(false);
        let (status, response) = post(
            running.addr,
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"status","arguments":{}}}"#,
        );
        assert_eq!(status, 401, "{response}");
        let json: Value = serde_json::from_str(&response).unwrap();
        assert!(json.get("error").is_some(), "{response}");

        let recorded = decisions(&running.dir);
        assert_eq!(recorded.len(), 1, "{recorded:?}");
        assert_eq!(recorded[0].decision, crate::core::AuthOutcome::Deny);
        assert_eq!(recorded[0].operation, "status");
        assert_eq!(recorded[0].transport, "mcp");
    }

    #[test]
    fn an_anonymous_ping_is_refused_too() {
        let running = serve(false);
        let (status, _) = post(running.addr, r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#);
        assert_eq!(status, 401);
        assert_eq!(decisions(&running.dir).len(), 1);
    }

    #[test]
    fn a_body_that_does_not_parse_records_nothing() {
        let running = serve(false);
        let (status, _) = post(running.addr, "not json");
        assert_eq!(status, 200);
        assert!(decisions(&running.dir).is_empty());
    }

    #[test]
    fn no_auth_answers_a_ping_and_records_the_allow() {
        let running = serve(true);
        let (status, response) = post(running.addr, r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#);
        assert_eq!(status, 200, "{response}");
        let json: Value = serde_json::from_str(&response).unwrap();
        assert!(json.get("result").is_some(), "{response}");
        let recorded = decisions(&running.dir);
        assert_eq!(recorded.len(), 1, "{recorded:?}");
        assert_eq!(recorded[0].decision, crate::core::AuthOutcome::Allow);
    }
}
