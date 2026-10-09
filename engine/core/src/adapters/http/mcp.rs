//! The `/mcp` route. Ports the mounting half of `adapters/mcp/http.py`.
//!
//! The streamable-HTTP endpoint is one route on the same port as the rest of
//! the API. Authentication and the recorded decision are the shared gate's,
//! the same one `/api` uses, so a disabled actor, a bad token and a missing
//! scope are refused in one place. What stays here is the part only MCP has:
//! the JSON-RPC framing, and narrowing `tools/list` to the scopes the token
//! holds.

use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, StatusCode};
use axum::response::Response;
use axum::routing::post;
use axum::Router;
use serde_json::{Map, Value};

use crate::adapters::auth_gate::{self, Denial};
use crate::adapters::mcp::framing::{Dispatcher, McpTransport, ToolGrant};
use crate::adapters::mcp::http::MCP_PATH;
use crate::core::{Clock, IdFactory};
use crate::registry::{Operation, OperationRegistry, Transport};
use crate::storage::sqlite::declared::SqliteDeclaredStore;

/// What the route needs. The store is behind a mutex because its clock and id
/// factory are not shareable across tasks.
pub struct McpState<C, I> {
    store: Mutex<SqliteDeclaredStore<C, I>>,
    data_dir: std::path::PathBuf,
    /// Loaded once, at startup, the same way `/api` loads it. A request must not
    /// call `load_config` itself, and `VogtConfig::default()` is where
    /// `VOGT_CONTRACT_VERSION` and `VOGT_SESSION_TTL_DAYS` got lost.
    config: crate::config::VogtConfig,
    pub no_auth: bool,
    pub writes_enabled: bool,
}

impl<C: Clock, I: IdFactory> McpState<C, I> {
    pub fn new(
        data_dir: &std::path::Path,
        no_auth: bool,
        writes_enabled: bool,
        clock: C,
        ids: I,
    ) -> Self {
        Self {
            store: Mutex::new(SqliteDeclaredStore::new(
                crate::storage::sqlite::declared_path(data_dir),
                clock,
                ids,
            )),
            data_dir: data_dir.to_path_buf(),
            config: super::app::loaded_config(data_dir),
            no_auth,
            writes_enabled,
        }
    }

    /// A route over the context's store, so both draw the same clock and ids.
    pub fn joined(
        data_dir: &std::path::Path,
        no_auth: bool,
        writes_enabled: bool,
        source: &SqliteDeclaredStore<C, I>,
    ) -> Self {
        Self {
            store: Mutex::new(source.joined(crate::storage::sqlite::declared_path(data_dir))),
            data_dir: data_dir.to_path_buf(),
            config: super::app::loaded_config(data_dir),
            no_auth,
            writes_enabled,
        }
    }
}

/// The route, gated by the declared store.
///
/// One function per hook pair rather than one generic one. A tool call's
/// context has to be a `Built`, and `Built` only exists for these four pairs,
/// so each route names its pair. `serve` already matches on the same four.
///
/// The id factory is the store's own handle, so every route and the CLI count
/// from one sequence. The clock is not. Python builds a fresh step clock per
/// request, starting again at `VOGT_TEST_CLOCK_START`, and sharing the store's
/// would advance it four ticks a request and stamp later rows later. The step
/// routes therefore restart theirs; a wall clock has no start to restart from.
macro_rules! mcp_route {
    ($name:ident, $clock:ty, $ids:ty, $variant:ident, shared) => {
        mcp_route!(@build $name, $clock, $ids, $variant, clock_for::<$clock>);
    };
    ($name:ident, $clock:ty, $ids:ty, $variant:ident, restart) => {
        mcp_route!(@build $name, $clock, $ids, $variant, step_clock_for);
    };
    (@build $name:ident, $clock:ty, $ids:ty, $variant:ident, $clock_for:path) => {
        pub fn $name(state: McpState<$clock, $ids>) -> Router {
            fn build(
                config: crate::config::VogtConfig,
                principal: Option<crate::core::Principal>,
                clock: Arc<Mutex<$clock>>,
                ids: Arc<Mutex<$ids>>,
                token: Option<crate::core::Token>,
            ) -> crate::application::context::Built {
                crate::application::context::Built::$variant(
                    crate::application::context::context_on(config, principal, clock, ids, token),
                )
            }
            Router::new()
                .route(MCP_PATH, post(handle::<$clock, $ids>))
                .with_state((
                    Arc::new(state),
                    build as ContextBuild<$clock, $ids>,
                    $clock_for as ClockFor<$clock>,
                ))
        }
    };
}

type ContextBuild<C, I> = fn(
    crate::config::VogtConfig,
    Option<crate::core::Principal>,
    Arc<Mutex<C>>,
    Arc<Mutex<I>>,
    Option<crate::core::Token>,
) -> crate::application::context::Built;

type Routed<C, I> = (Arc<McpState<C, I>>, ContextBuild<C, I>, ClockFor<C>);

type ClockFor<C> = fn(&Arc<Mutex<C>>) -> Arc<Mutex<C>>;

mcp_route!(
    router_system_random,
    crate::application::context::SystemClock,
    crate::application::context::RandomIds,
    SystemRandom,
    shared
);
mcp_route!(
    router_system_sequential,
    crate::application::context::SystemClock,
    crate::core::SequentialIds,
    SystemSequential,
    shared
);
mcp_route!(
    router_step_random,
    crate::core::StepClock,
    crate::application::context::RandomIds,
    StepRandom,
    restart
);
mcp_route!(
    router_step_sequential,
    crate::core::StepClock,
    crate::core::SequentialIds,
    StepSequential,
    restart
);

async fn handle<C: Clock + Clone, I: IdFactory>(
    State((state, build, clock_for)): State<Routed<C, I>>,
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
                "error": {"code": -32602, "message": "body is not valid JSON"}
            }),
        );
    };

    let store = state.store.lock().expect("the store lock is not poisoned");
    // The request's own store: the shared id factory, so the decision row and
    // the service count from one sequence, and a clock restarted for this
    // request, so both stamp from the same instant. The process store keeps its
    // clock, which the CLI also ticks.
    let request_store = SqliteDeclaredStore::shared(
        store.path().to_path_buf(),
        clock_for(store.clock()),
        Arc::clone(store.id_factory()),
        store.synchronous(),
    );
    // The operation context gets its own clock, restarted the same way the
    // request store's was. Python builds a fresh context per call
    // (`context.py`), so the service's rows start again at the hook's start
    // instead of continuing the clock the gate and the decision row already
    // ticked. A wall clock has nothing to restart, and keeps the shared one.
    let operation_clock = clock_for(store.clock());
    // One read of the request's clock. The gate's expiry check and the decision
    // row below both use it; reading again for the row would land it a tick
    // later than Python's.
    let now = request_store
        .clock()
        .lock()
        .expect("the clock lock is not poisoned")
        .now();
    // Authenticate through the shared gate. It records a refusal and nothing
    // for a live credential, so a ping or a tools/list writes no row. The one
    // row a tool call writes is recorded below.
    let grant = match authenticate(&request_store, &state, presented.as_deref(), now) {
        Ok(grant) => grant,
        Err(denial) => return unauthenticated(denial),
    };
    if let Some(operation) = called_operation(&message, &registry) {
        if let Err(denial) = record_call(&request_store, &state, &grant, operation, now) {
            return refusal(&message, operation, &grant, denial);
        }
    }
    drop(store);
    let permitted = Permitted {
        scopes: grant.scopes.clone(),
        writes_enabled: state.writes_enabled,
    };
    let built = context_for(&request_store, &state, &grant, build, operation_clock);
    let mut dispatcher = Dispatcher::new(&registry, &permitted, McpTransport::Http);
    if let Some(context) = built.as_ref() {
        dispatcher = dispatcher.with_context(context);
    }
    match dispatcher.handle(&message) {
        None => Response::builder()
            .status(StatusCode::ACCEPTED)
            .header("content-type", "application/json")
            .body(Body::from("null"))
            .expect("a fixed response builds"),
        Some(value) => json_response(StatusCode::OK, value),
    }
}

/// The context a tool call runs against, with the authenticated principal on it.
///
/// The principal comes off the grant: `kind` and `display_name` are the actor
/// row the gate already loaded, so an agent's token stays an agent instead of
/// being recorded as a person, and the token rides with it for `auth.whoami`.
///
/// The id factory is the store's own handle, so the service's writes continue
/// the sequence the decision row started. The clock is not: the gate and the
/// decision row already ticked the store's, and Python's per-call context
/// starts again at the hook's start, so the route hands in a fresh one.
fn context_for<C: Clock, I: IdFactory>(
    store: &SqliteDeclaredStore<C, I>,
    state: &McpState<C, I>,
    grant: &auth_gate::Grant,
    build: ContextBuild<C, I>,
    clock: Arc<Mutex<C>>,
) -> Option<crate::application::context::Built> {
    use crate::core::{local_principal, os_user, Principal};
    let config = state.config.clone();
    let principal = match &grant.identity_ref {
        Some(identity_ref) if !identity_ref.is_empty() => {
            Principal::new(identity_ref, grant.kind, &grant.display_name).ok()
        }
        _ => Some(local_principal(&os_user())),
    };
    Some(build(
        config,
        principal,
        clock,
        Arc::clone(store.id_factory()),
        grant.token.clone(),
    ))
}

/// Which clock a request ticks.
///
/// The step routes restart theirs at `VOGT_TEST_CLOCK_START`, so the decision
/// row, the entity row and the audit row land at the same three instants every
/// request. A wall clock has no start to restart from, and the request ticks
/// the one the store holds. The two are separate functions because only
/// `StepClock` has a start to restart from, and a generic one could not name it.
pub(crate) fn clock_for<C: Clock + Clone>(shared: &Arc<Mutex<C>>) -> Arc<Mutex<C>> {
    Arc::clone(shared)
}

pub(crate) fn step_clock_for(
    _shared: &Arc<Mutex<crate::core::StepClock>>,
) -> Arc<Mutex<crate::core::StepClock>> {
    use crate::core::{clock_from_env, CLOCK_ENV};
    // A value that is not a timestamp was already refused at startup, so this
    // failing means the environment changed under the process. Stamping from
    // the epoch would write rows the operator cannot reconcile.
    let clock = clock_from_env(std::env::var(CLOCK_ENV).ok().as_deref())
        .expect("VOGT_TEST_CLOCK_START changed after startup")
        .expect("VOGT_TEST_CLOCK_START was unset after startup");
    Arc::new(Mutex::new(clock))
}

/// Resolve the credential through the shared gate, which records a refusal and
/// nothing for a success.
fn authenticate<C: Clock, I: IdFactory>(
    store: &SqliteDeclaredStore<C, I>,
    state: &McpState<C, I>,
    presented: Option<&str>,
    now: crate::core::Moment,
) -> Result<auth_gate::Grant, Denial> {
    auth_gate::authenticate(
        store,
        auth_gate::Request {
            operation: status_operation(),
            transport: Transport::Mcp,
            presented,
            no_auth: state.no_auth,
            writes_enabled: state.writes_enabled,
            now,
        },
        state.config.session_ttl_days,
    )
}

/// The read the authentication check is made against. It never decides whether
/// writes are switched on; the tool call does that.
fn status_operation() -> &'static Operation {
    use std::sync::OnceLock;
    static REGISTRY: OnceLock<OperationRegistry> = OnceLock::new();
    REGISTRY
        .get_or_init(crate::registry::default_registry)
        .get("status")
        .expect("status is a registered operation")
}

/// Record the one decision a tool call makes, the way `services/auth.py` does.
///
/// The gate already said the credential is live and what scopes it holds, so
/// this asks the same scope question and writes the row itself: one `aut_` id,
/// transport `mcp-http`, the operation's scope. A no-auth caller has no token,
/// and the row names `local:<user>` instead.
fn record_call<C: Clock, I: IdFactory>(
    store: &SqliteDeclaredStore<C, I>,
    state: &McpState<C, I>,
    grant: &auth_gate::Grant,
    operation: &Operation,
    now: crate::core::Moment,
) -> Result<(), Denial> {
    use crate::storage::interface::DeclaredStore;
    let held: Vec<&str> = grant.scopes.iter().map(String::as_str).collect();
    // `--no-auth` is the local console: Python lets it write even with
    // `--read-only`, the way `/api` does. A presented token is still bound by
    // the switch.
    let writes_enabled = state.writes_enabled || state.no_auth;
    let (permitted, reason) = crate::auth::allows(
        &held,
        writes_enabled,
        operation.scope.as_str(),
        operation.mutating,
    );
    let id = store.next_id("aut");
    let local = state.no_auth;
    let decision = crate::core::AuthDecision {
        id,
        at: now,
        decision: if permitted {
            crate::core::AuthOutcome::Allow
        } else {
            crate::core::AuthOutcome::Deny
        },
        reason_code: reason.to_string(),
        operation: operation.name.to_string(),
        scope: Some(operation.scope.as_str().to_string()),
        actor_id: (!local).then(|| grant.actor_id.clone()),
        token_id: (!local).then(|| grant.token_id.clone()),
        identity_ref: if local {
            Some(crate::core::local_principal(&crate::core::os_user()).identity_ref)
        } else {
            grant.identity_ref.clone()
        },
        transport: "mcp-http".to_string(),
        detail: None,
    };
    store
        .record_auth_decision(&decision)
        .map_err(|error| Denial::Unrecorded {
            failure: error.to_string(),
        })?;
    if permitted {
        Ok(())
    } else if reason == crate::auth::WRITES_DISABLED {
        Err(Denial::WritesDisabled {
            operation: operation.name.to_string(),
        })
    } else {
        Err(Denial::Forbidden {
            operation: operation.name.to_string(),
            held: grant.scopes.clone(),
            needed: operation.scope.as_str().to_string(),
        })
    }
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

    fn writes_enabled(&self) -> bool {
        self.writes_enabled
    }
}

/// A missing or rejected credential. The request never became a caller, so this
/// is a `401` with the product envelope rather than a JSON-RPC error.
fn unauthenticated(denial: Denial) -> Response {
    if matches!(denial, Denial::Unrecorded { .. }) {
        // The decision could not be recorded, so the request does not run and
        // the client hears a plain failure rather than the store's own text.
        return Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .header("content-type", "text/plain; charset=utf-8")
            .body(Body::from("Internal Server Error"))
            .expect("a fixed response builds");
    }
    json_response(
        StatusCode::UNAUTHORIZED,
        serde_json::json!({
            "error": {"code": "unauthenticated", "message": denial.error().message()}
        }),
    )
}

/// A call the gate refused after the caller had authenticated. The text names
/// the operation, the scope it needs and the scopes the token holds.
fn refusal(
    message: &Map<String, Value>,
    operation: &Operation,
    grant: &auth_gate::Grant,
    denial: Denial,
) -> Response {
    if matches!(denial, Denial::Unrecorded { .. }) {
        return unauthenticated(denial);
    }
    let text = if matches!(denial, Denial::WritesDisabled { .. }) {
        format!(
            "forbidden: {} is a write, and this server was started read-only",
            operation.name
        )
    } else {
        // Deduplicated and alphabetical, so the order the token was issued in
        // never reaches the message.
        let mut held: Vec<&str> = grant.scopes.iter().map(String::as_str).collect();
        held.sort_unstable();
        held.dedup();
        let held = held.join(", ");
        format!(
            "forbidden: {} requires the {} scope; this token holds {}",
            operation.name,
            crate::core::py_repr(operation.scope.as_str()),
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

    use crate::application::context::SystemClock;
    use crate::core::SequentialIds;
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
        let state = McpState::new(
            &dir,
            no_auth,
            true,
            SystemClock,
            SequentialIds::new(None).unwrap(),
        );
        runtime.spawn(async move {
            let listener = tokio::net::TcpListener::from_std(listener).unwrap();
            axum::serve(listener, router_system_sequential(state))
                .await
                .unwrap();
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
            SequentialIds::new(None).unwrap(),
        );
        store.read().unwrap().list_auth_decisions(None, 10).unwrap()
    }

    #[test]
    fn an_anonymous_tool_call_is_refused_and_records_nothing() {
        let running = serve(false);
        let (status, response) = post(
            running.addr,
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"status","arguments":{}}}"#,
        );
        assert_eq!(status, 401, "{response}");
        let json: Value = serde_json::from_str(&response).unwrap();
        assert_eq!(json["error"]["code"], "unauthenticated", "{response}");
        // A missing bearer is not a token, so Python records no row for it.
        assert!(decisions(&running.dir).is_empty(), "{response}");
    }

    #[test]
    fn an_anonymous_ping_is_refused_too() {
        let running = serve(false);
        let (status, _) = post(running.addr, r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#);
        assert_eq!(status, 401);
        assert!(decisions(&running.dir).is_empty());
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
        // A ping is not a tool call, so it writes no row. The one token_valid
        // row a no-auth session writes is for the tool it calls.
        assert!(decisions(&running.dir).is_empty());
    }
}
