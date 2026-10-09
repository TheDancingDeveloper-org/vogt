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

use crate::adapters::auth_gate::Denial;
use crate::adapters::mcp::framing::{Dispatcher, McpTransport, ToolGrant};
use crate::adapters::mcp::http::MCP_PATH;
use crate::core::{Clock, IdFactory, SystemClock};
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
    // Authenticate first, the way `resolve()` does, and record it only when the
    // credential is refused. An authenticated initialize, ping or tools/list
    // writes no row: Python records none for those.
    let caller = match authenticate(&store, &state, presented.as_deref()) {
        Ok(caller) => caller,
        Err(denial) => return unauthenticated(&message, denial),
    };
    // A tool call that reaches an operation is authorized against it, and that
    // decision is recorded before the call runs.
    if let Some(operation) = called_operation(&message, &registry) {
        if let Err(denial) = authorize(&store, &state, &caller, operation) {
            return refusal(&message, operation, &caller, denial);
        }
    }
    drop(store);

    // Both checks passed. The dispatcher still owns the protocol: a notification
    // is a 202, a bad argument is `-32602`, an unknown tool is `-32601`. The
    // grant narrows `tools/list` to what this caller may invoke.
    let grant = Permitted {
        scopes: caller.scopes.clone(),
        writes_enabled: state.writes_enabled,
    };
    let mut dispatcher = Dispatcher::new(&registry, &grant, McpTransport::Http);
    match dispatcher.handle(&message) {
        None => Response::builder()
            .status(StatusCode::ACCEPTED)
            .header("content-type", "application/json")
            .body(Body::from("null"))
            .expect("a fixed response builds"),
        Some(value) => json_response(StatusCode::OK, value),
    }
}

/// Who a request resolved to. Scopes are the token's own, so the grant can
/// narrow `tools/list` the way `http.py` does.
struct Caller {
    scopes: Vec<String>,
    /// The token the credential resolved to. Absent on the no-auth path, which
    /// has no row to attribute.
    token: Option<crate::core::Token>,
}

/// Resolve the credential, recording a refusal only.
///
/// A refusal is recorded against `authenticate` on the `http` transport, which
/// is what `services/auth.py` writes: the row says the credential was refused,
/// not which method was asked for. An accepted credential records nothing here;
/// the per-operation row is `authorize`'s.
fn authenticate<I: IdFactory>(
    store: &SqliteDeclaredStore<SystemClock, I>,
    state: &McpState<I>,
    presented: Option<&str>,
) -> Result<Caller, Denial> {
    use crate::storage::interface::{DeclaredStore, ReadView};
    if state.no_auth {
        return Ok(Caller {
            scopes: vec!["admin".to_string()],
            token: None,
        });
    }
    let Some(secret) = presented else {
        record_refusal(store, "no_bearer_token", None)?;
        return Err(Denial::NoBearer);
    };
    let hashed = crate::auth::hash_token(secret);
    let found = store
        .read()
        .map_err(lookup_failed)?
        .token_by_hash(&hashed)
        .map_err(lookup_failed)?;
    let Some(token) = found else {
        record_refusal(store, "unknown_token", None)?;
        return Err(Denial::Rejected {
            code: "unknown_token",
            detail: "the presented token is not valid".to_string(),
        });
    };
    let now = store
        .clock()
        .lock()
        .expect("the clock lock is not poisoned")
        .now();
    let rejection = if token.revoked_at.is_some() {
        Some("revoked")
    } else if token.expires_at.is_some_and(|expires| expires <= now) {
        Some("expired")
    } else {
        None
    };
    if let Some(code) = rejection {
        record_refusal(store, code, Some(&token))?;
        return Err(Denial::Rejected {
            code: "rejected",
            detail: "the presented token is not valid".to_string(),
        });
    }
    // A disabled actor's token is not a live credential. The row names the
    // token; the caller only hears that it is not valid.
    let actor = store
        .read()
        .map_err(lookup_failed)?
        .actor_by_id(&token.actor_id)
        .map_err(lookup_failed)?;
    if actor.as_ref().is_none_or(|actor| actor.disabled) {
        record_refusal(store, "disabled_actor", Some(&token))?;
        return Err(Denial::Rejected {
            code: "disabled_actor",
            detail: "the presented token is not valid".to_string(),
        });
    }
    Ok(Caller {
        scopes: token.scopes.clone(),
        token: Some(token),
    })
}

/// A refused credential, recorded the way `services/auth.py` records it:
/// operation `authenticate`, transport `http`, and no scope.
fn record_refusal<I: IdFactory>(
    store: &SqliteDeclaredStore<SystemClock, I>,
    code: &str,
    token: Option<&crate::core::Token>,
) -> Result<(), Denial> {
    record(
        store,
        &decision(store, token, code, "authenticate", "http", None),
    )
    .map_err(|failure| Denial::Unrecorded { failure })
}

fn lookup_failed(error: crate::errors::VogtError) -> Denial {
    Denial::Unrecorded {
        failure: format!("the token could not be looked up: {error}"),
    }
}

/// Authorize one tool call and record it, with the transport Python uses.
fn authorize<I: IdFactory>(
    store: &SqliteDeclaredStore<SystemClock, I>,
    state: &McpState<I>,
    caller: &Caller,
    operation: &Operation,
) -> Result<(), Denial> {
    // The same question the shared gate asks, so the two cannot drift.
    let held: Vec<&str> = caller.scopes.iter().map(String::as_str).collect();
    let (permitted, reason) = crate::auth::allows(
        &held,
        state.writes_enabled,
        operation.scope.as_str(),
        operation.mutating,
    );
    record(
        store,
        &decision(
            store,
            caller.token.as_ref(),
            reason,
            operation.name,
            "mcp-http",
            Some(operation.scope.as_str()),
        ),
    )
    .map_err(|failure| Denial::Unrecorded { failure })?;
    if permitted {
        // The decision above says Allow only when permitted. `decision` reads the
        // reason, and `allows` returns `ok` exactly when it permits.
        Ok(())
    } else if reason == crate::auth::WRITES_DISABLED {
        Err(Denial::WritesDisabled)
    } else {
        Err(Denial::Forbidden {
            held: caller.scopes.clone(),
            needed: operation.scope.as_str().to_string(),
        })
    }
}

fn decision<I: IdFactory>(
    store: &SqliteDeclaredStore<SystemClock, I>,
    token: Option<&crate::core::Token>,
    reason: &str,
    operation: &str,
    transport: &str,
    scope: Option<&str>,
) -> crate::core::AuthDecision {
    // The store's own hooks, not a fresh clock and a fresh counter. The context
    // holds the same ones, so a second source would collide with it.
    let id = store
        .id_factory()
        .lock()
        .expect("the id factory lock is not poisoned")
        .next("aud");
    let at = store
        .clock()
        .lock()
        .expect("the clock lock is not poisoned")
        .now();
    crate::core::AuthDecision {
        id,
        at,
        decision: if reason == crate::auth::TOKEN_OK {
            crate::core::AuthOutcome::Allow
        } else {
            crate::core::AuthOutcome::Deny
        },
        reason_code: reason.to_string(),
        operation: operation.to_string(),
        scope: scope.map(str::to_string),
        actor_id: token.map(|token| token.actor_id.clone()),
        token_id: token.map(|token| token.id.clone()),
        identity_ref: token.and_then(|token| token.actor_identity_ref.clone()),
        transport: transport.to_string(),
        detail: None,
    }
}

fn record<I: IdFactory>(
    store: &SqliteDeclaredStore<SystemClock, I>,
    decision: &crate::core::AuthDecision,
) -> Result<(), String> {
    use crate::storage::interface::DeclaredStore;
    store
        .record_auth_decision(decision)
        .map_err(|error| error.to_string())
}

/// The operation a `tools/call` names, when the message is well-formed enough
/// to be one and the name is an MCP-exposed operation. Anything else is the
/// dispatcher's to answer, and it records nothing.
fn called_operation<'a>(
    message: &Map<String, Value>,
    registry: &'a OperationRegistry,
) -> Option<&'a Operation> {
    if message.get("method").and_then(Value::as_str) != Some("tools/call") {
        return None;
    }
    if message.get("id").is_none_or(Value::is_null) {
        return None;
    }
    let params = message.get("params").filter(|value| !json_falsy(value))?;
    let params = params.as_object()?;
    let name = params.get("name").and_then(Value::as_str)?;
    if let Some(arguments) = params.get("arguments") {
        if !json_falsy(arguments) && !arguments.is_object() {
            return None;
        }
    }
    let operation = registry.by_mcp_tool(name).ok()?;
    registry
        .transports_for(operation.name)
        .contains(&Transport::Mcp)
        .then_some(operation)
}

fn json_falsy(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Bool(flag) => !flag,
        Value::Number(number) => number.as_f64().is_some_and(|value| value == 0.0),
        Value::String(text) => text.is_empty(),
        Value::Array(items) => items.is_empty(),
        Value::Object(fields) => fields.is_empty(),
    }
}

/// A refused credential. Python's envelope, not a JSON-RPC error: the request
/// never reached the protocol.
fn unauthenticated(message: &Map<String, Value>, denial: Denial) -> Response {
    if matches!(denial, Denial::Unrecorded { .. }) {
        // The decision could not be recorded, so the request does not run and
        // the client hears a plain failure rather than the store's own text.
        return Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .header("content-type", "text/plain; charset=utf-8")
            .body(Body::from("Internal Server Error"))
            .expect("a fixed response builds");
    }
    let _ = message;
    json_response(
        StatusCode::UNAUTHORIZED,
        serde_json::json!({
            "error": {"code": "unauthenticated", "message": denial.error().to_string()}
        }),
    )
}

/// A tool call the caller may not make. The text names the operation, the scope
/// it needs and the scopes the token holds, matching `core/auth.py`.
fn refusal(
    message: &Map<String, Value>,
    operation: &Operation,
    caller: &Caller,
    denial: Denial,
) -> Response {
    if matches!(denial, Denial::Unrecorded { .. }) {
        return unauthenticated(message, denial);
    }
    let text = if matches!(denial, Denial::WritesDisabled) {
        format!(
            "{} is a write, and this server was started read-only",
            operation.name
        )
    } else {
        // Deduplicated and alphabetical, so the order the token was issued in
        // never reaches the message.
        let mut held: Vec<&str> = caller.scopes.iter().map(String::as_str).collect();
        held.sort_unstable();
        held.dedup();
        let held = held.join(", ");
        format!(
            "{} requires the {} scope; this token holds {}",
            operation.name,
            serde_json::to_string(operation.scope.as_str()).unwrap_or_else(|_| "\"?\"".to_string()),
            if held.is_empty() { "nothing" } else { &held }
        )
    };
    json_response(
        StatusCode::OK,
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": message.get("id").cloned().unwrap_or(Value::Null),
            "result": {"content": [{"type": "text", "text": text}], "isError": true}
        }),
    )
}

/// What this caller may invoke. The scope decision is the same one the shared
/// gate makes; the dispatcher uses it to narrow `tools/list`.
struct Permitted {
    scopes: Vec<String>,
    writes_enabled: bool,
}

impl ToolGrant for Permitted {
    fn allows(&self, _registry: &OperationRegistry, operation: &Operation) -> bool {
        let held: Vec<&str> = self.scopes.iter().map(String::as_str).collect();
        crate::auth::allows(
            &held,
            self.writes_enabled,
            operation.scope.as_str(),
            operation.mutating,
        )
        .0
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
        // A refused credential is recorded as the authentication, not as the
        // method that was asked for.
        assert_eq!(recorded[0].operation, "authenticate");
        assert_eq!(recorded[0].reason_code, "no_bearer_token");
        assert_eq!(recorded[0].transport, "http");
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
    fn no_auth_answers_a_ping_and_records_nothing() {
        let running = serve(true);
        let (status, response) = post(running.addr, r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#);
        assert_eq!(status, 200, "{response}");
        let json: Value = serde_json::from_str(&response).unwrap();
        assert!(json.get("result").is_some(), "{response}");
        // An authenticated ping is not a tool call, so it writes no row.
        assert!(decisions(&running.dir).is_empty());
    }
}
