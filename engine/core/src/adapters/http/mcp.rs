//! The `/mcp` route. Ports the mounting half of `adapters/mcp/http.py`.
//!
//! The streamable-HTTP endpoint is one route on the same port as the rest of
//! the API. Authentication and the recorded decision are the shared gate's,
//! the same one `/api` uses, so a disabled actor, a bad token and a missing
//! scope are refused in one place. What stays here is the part only MCP has:
//! the JSON-RPC framing, and narrowing `tools/list` to the scopes the token
//! holds.

use std::sync::{Arc, Mutex};

use parking_lot::Mutex as StoreMutex;

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
    store: StoreMutex<SqliteDeclaredStore<C, I>>,
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
            store: StoreMutex::new(SqliteDeclaredStore::new(
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
            store: StoreMutex::new(source.joined(crate::storage::sqlite::declared_path(data_dir))),
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

/// What authentication produced, carried back to the async task so the actor
/// can be named there. The access-log middleware reads the actor from the
/// request's task-local scope; a value set inside `spawn_blocking` is a
/// different task and the line would stay empty.
struct Authenticated<C, I> {
    request_store: SqliteDeclaredStore<C, I>,
    operation_clock: Arc<Mutex<C>>,
    grant: auth_gate::Grant,
    principal: crate::core::Principal,
    message: Map<String, Value>,
    /// `--no-auth` or writes enabled on the process. The console caller writes
    /// even when the server is read-only; a presented token stays bound by the
    /// switch. Captured here because the dispatch stage no longer holds state.
    writes_enabled: bool,
    config: crate::config::VogtConfig,
}

async fn handle<C: Clock + Clone + Send + 'static, I: IdFactory + Send + 'static>(
    State((state, build, clock_for)): State<Routed<C, I>>,
    request: Request<Body>,
) -> Response {
    let presented = bearer(request.headers().get("authorization"));
    // The same cap `/api` applies, and it is applied before anything else reads
    // the body: an unauthenticated caller must not be able to stream an
    // unbounded body into memory.
    let read = axum::body::to_bytes(request.into_body(), 1024 * 1024 + 1).await;
    let bytes = match read {
        Ok(bytes) if bytes.len() > 1024 * 1024 => {
            return super::app::mcp_payload_too_large();
        }
        Ok(bytes) => bytes,
        Err(_) => {
            return super::app::mcp_payload_too_large();
        }
    };
    let body = String::from_utf8_lossy(&bytes).into_owned();
    // Authentication is rusqlite. It runs off the async worker, and it returns
    // the identity rather than setting the actor: the access log reads the
    // task-local, which a blocking thread does not share.
    let admitted = tokio::task::spawn_blocking(move || {
        authenticate_message(state, clock_for, presented, body)
    })
    .await;
    let admitted = match admitted {
        Ok(admitted) => admitted,
        Err(_) => return plain_internal(),
    };
    let admitted = match admitted {
        Ok(admitted) => admitted,
        Err(response) => return *response,
    };
    // Python's `resolve` names the actor after authentication and before the
    // handler. The access-log middleware opened this task's request scope;
    // writing it here is what the line reads when the handler returns. A
    // refusal above never gets here, and stays unattributed.
    crate::observability::set_request_actor(Some(admitted.principal.identity_ref.clone()));
    let outcome = tokio::task::spawn_blocking(move || dispatch(admitted, build)).await;
    match outcome {
        Ok(response) => response,
        Err(_) => plain_internal(),
    }
}

fn plain_internal() -> Response {
    Response::builder()
        .status(StatusCode::INTERNAL_SERVER_ERROR)
        .header("content-type", "text/plain; charset=utf-8")
        .body(Body::from("Internal Server Error"))
        .expect("a fixed response builds")
}

/// Parse, authenticate and record the call. Invalid JSON and a refused
/// credential come back as the response the client sees, with no actor.
fn authenticate_message<C: Clock + Clone, I: IdFactory>(
    state: Arc<McpState<C, I>>,
    clock_for: ClockFor<C>,
    presented: Option<String>,
    body: String,
) -> Result<Authenticated<C, I>, Box<Response>> {
    let registry = crate::registry::default_registry();
    let Ok(Value::Object(message)) = serde_json::from_str::<Value>(&body) else {
        // Not a message, so there is nothing to authorize and nothing to
        // record. The client still has to be able to read why.
        return Err(Box::new(json_response(
            StatusCode::OK,
            serde_json::json!({
                "jsonrpc": "2.0", "id": null,
                "error": {"code": -32602, "message": "body is not valid JSON"}
            }),
        )));
    };

    // parking_lot's mutex does not poison: a panic under the lock releases
    // it, so one failed request cannot turn every later request into a 500.
    let store = state.store.lock();
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
    // request store's was, and then read once before the service sees it.
    // Python's per-call context reads its fresh step clock once while it is
    // being built (`context.py`), so the service's first stamp is one tick
    // past the hook's start rather than on it. A wall clock has nothing to
    // restart, and keeps the shared one.
    let operation_clock = clock_for(store.clock());
    operation_clock
        .lock()
        .expect("the clock lock is not poisoned")
        .now();
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
        Err(denial) => return Err(Box::new(unauthenticated(denial))),
    };
    let principal = match auth_gate::principal_for(&grant) {
        Ok(principal) => principal,
        // An authenticated grant whose actor has no identity must not run as
        // the local operator. The caller already authenticated, so this is the
        // plain 500 an unrecorded decision gets.
        Err(_) => {
            return Err(Box::new(unauthenticated(Denial::Unrecorded {
                failure: "the token's actor has no identity".to_string(),
            })));
        }
    };
    if let Some(operation) = called_operation(&message, &registry) {
        if let Err(denial) = record_call(&request_store, &state, &grant, operation, now) {
            return Err(Box::new(refusal(&message, operation, &grant, denial)));
        }
    }
    drop(store);
    Ok(Authenticated {
        request_store,
        operation_clock,
        grant,
        principal,
        message,
        writes_enabled: state.writes_enabled || state.no_auth,
        config: state.config.clone(),
    })
}

/// Run the tool. Blocking rusqlite and blocking engine HTTP stay off the
/// tokio workers; the actor was already written on the async task.
fn dispatch<C: Clock + Clone, I: IdFactory>(
    admitted: Authenticated<C, I>,
    build: ContextBuild<C, I>,
) -> Response {
    let registry = crate::registry::default_registry();
    let Authenticated {
        request_store,
        operation_clock,
        grant,
        principal,
        message,
        writes_enabled,
        config,
    } = admitted;
    let permitted = Permitted {
        scopes: grant.scopes.clone(),
        writes_enabled,
    };
    let built = context_for(
        &request_store,
        &config,
        &grant,
        principal,
        build,
        operation_clock,
    );
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
    config: &crate::config::VogtConfig,
    grant: &auth_gate::Grant,
    principal: crate::core::Principal,
    build: ContextBuild<C, I>,
    clock: Arc<Mutex<C>>,
) -> Option<crate::application::context::Built> {
    Some(build(
        config.clone(),
        Some(principal),
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

    use crate::adapters::http::test_harness;
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

    /// The MCP route over HTTP, on the same listener and store as `/api`, with
    /// tokens minted over `/api` the way an operator does it.
    fn rpc(addr: std::net::SocketAddr, token: &str, method: &str, params: Value) -> Value {
        let body =
            serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
        let (status, response) =
            test_harness::send(addr, "POST", MCP_PATH, Some(token), Some(&body));
        assert_eq!(status, 200, "{response}");
        serde_json::from_str(&response).unwrap()
    }

    fn tool_names(addr: std::net::SocketAddr, token: &str) -> Vec<String> {
        rpc(addr, token, "tools/list", serde_json::json!({}))["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn initialize_over_http_names_the_server() {
        let served = test_harness::serve(false);
        let admin = test_harness::admin_secret(served.addr);
        let reader = test_harness::mint_token(served.addr, &admin, "agent:reader", "read");
        let result = rpc(served.addr, &reader, "initialize", serde_json::json!({}));
        assert_eq!(result["result"]["serverInfo"]["name"], "vogt", "{result}");
        assert!(result["result"]["protocolVersion"].is_string(), "{result}");
    }

    /// The handshake negotiates down on HTTP as well as stdio. This is the
    /// transport Claude Code uses through the bridge.
    #[test]
    fn an_unknown_protocol_version_negotiates_down_over_http() {
        let served = test_harness::serve(false);
        let admin = test_harness::admin_secret(served.addr);
        let reader = test_harness::mint_token(served.addr, &admin, "agent:reader", "read");
        let response = rpc(
            served.addr,
            &reader,
            "initialize",
            serde_json::json!({"protocolVersion": "2999-01-01"}),
        );
        assert!(response.get("error").is_none(), "{response}");
        assert_eq!(
            response["result"]["protocolVersion"],
            crate::adapters::mcp::framing::SUPPORTED_PROTOCOL_VERSIONS[0]
        );
    }

    #[test]
    fn ungranted_tools_are_invisible_not_erroring() {
        let served = test_harness::serve(false);
        let admin = test_harness::admin_secret(served.addr);
        let reader = test_harness::mint_token(served.addr, &admin, "agent:reader", "read");
        let writer = test_harness::mint_token(served.addr, &admin, "agent:writer", "work.write");

        let reader_tools = tool_names(served.addr, &reader);
        let writer_tools = tool_names(served.addr, &writer);
        assert!(
            reader_tools.iter().any(|name| name == "status"),
            "{reader_tools:?}"
        );
        assert!(!reader_tools.iter().any(|name| name == "work_create"));
        assert!(writer_tools.iter().any(|name| name == "work_create"));
        assert!(
            !writer_tools.iter().any(|name| name == "token_issue"),
            "a writer does not see admin tools"
        );
    }

    #[test]
    fn calling_an_ungranted_tool_over_http_is_refused_and_recorded() {
        let served = test_harness::serve(false);
        let admin = test_harness::admin_secret(served.addr);
        let reader = test_harness::mint_token(served.addr, &admin, "agent:reader", "read");
        let response = rpc(
            served.addr,
            &reader,
            "tools/call",
            serde_json::json!({
                "name": "work_create",
                "arguments": {"kind": "bug", "title": "x", "reason": "test"},
            }),
        );
        assert_eq!(response["result"]["isError"], true, "{response}");
        assert!(
            response["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("forbidden"),
            "{response}"
        );
        let store = SqliteDeclaredStore::new(
            crate::storage::sqlite::declared_path(&served.dir),
            SystemClock,
            SequentialIds::new(None).unwrap(),
        );
        let denials = store
            .read()
            .unwrap()
            .list_auth_decisions(Some("deny"), 10)
            .unwrap();
        assert!(
            denials.iter().any(|d| d.transport == "mcp-http"),
            "{denials:?}"
        );
    }

    #[test]
    fn a_granted_tool_call_works_over_http() {
        let served = test_harness::serve(false);
        let admin = test_harness::admin_secret(served.addr);
        let writer = test_harness::mint_token(served.addr, &admin, "agent:writer", "work.write");
        let response = rpc(
            served.addr,
            &writer,
            "tools/call",
            serde_json::json!({
                "name": "work_create",
                "arguments": {"kind": "bug", "title": "Filed over HTTP", "reason": "test"},
            }),
        );
        assert_eq!(response["result"]["isError"], false, "{response}");
        assert!(
            response["result"]["structuredContent"]["item"]["ref"]
                .as_str()
                .is_some_and(|reference| reference.starts_with("WI-")),
            "{response}"
        );
    }

    /// The body cap is the same 1 MiB `/api` applies, and it is enforced before
    /// the body is parsed or authenticated.
    #[test]
    fn an_oversized_body_is_413() {
        let running = serve(true);
        let body = "x".repeat(1024 * 1024 + 1);
        let (status, response) = post(running.addr, &body);
        assert_eq!(status, 413, "{response}");
        let json: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(json["error"]["code"], "payload_too_large");
    }

    /// A panic while the store lock is held must not turn every later request
    /// into a 500. The lock does not poison, so the next request takes it.
    #[test]
    fn a_panic_under_the_store_lock_does_not_stick() {
        let dir =
            std::env::temp_dir().join(format!("vogt-mcp-lock-{}", crate::core::fresh_id("t")));
        std::fs::create_dir_all(&dir).unwrap();
        crate::application::instance::init(&dir, &mut None, &mut None).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let state = McpState::new(
            &dir,
            true,
            true,
            SystemClock,
            SequentialIds::new(None).unwrap(),
        );
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = state.store.lock();
            panic!("panic under the store lock");
        }));
        runtime.spawn(async move {
            let listener = tokio::net::TcpListener::from_std(listener).unwrap();
            axum::serve(listener, router_system_sequential(state))
                .await
                .unwrap();
        });
        std::thread::sleep(std::time::Duration::from_millis(100));
        let (status, response) = post(addr, r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#);
        assert_eq!(status, 200, "{response}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The actor is written on the async task, after authentication, inside
    /// the scope the access-log middleware opens. This layer opens that same
    /// scope and reads the actor once the handler has returned, which is when
    /// the access line is written. A value set inside `spawn_blocking` is a
    /// different task and would read back empty.
    #[test]
    fn an_authenticated_call_names_the_actor_the_access_line_reads() {
        let dir =
            std::env::temp_dir().join(format!("vogt-mcp-actor-{}", crate::core::fresh_id("t")));
        std::fs::create_dir_all(&dir).unwrap();
        crate::application::instance::init(&dir, &mut None, &mut None).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let state = McpState::new(
            &dir,
            true,
            true,
            SystemClock,
            SequentialIds::new(None).unwrap(),
        );
        let seen = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let slot = std::sync::Arc::clone(&seen);
        let router = router_system_sequential(state).layer(axum::middleware::from_fn(
            move |request: Request<Body>, next: axum::middleware::Next| {
                let slot = std::sync::Arc::clone(&slot);
                async move {
                    crate::observability::with_request_id("req-mcp-actor".to_string(), async move {
                        let response = next.run(request).await;
                        let actor = crate::observability::current_actor().unwrap_or_default();
                        *slot.lock().expect("the slot lock") = actor;
                        response
                    })
                    .await
                }
            },
        ));
        runtime.spawn(async move {
            let listener = tokio::net::TcpListener::from_std(listener).unwrap();
            axum::serve(listener, router).await.unwrap();
        });
        std::thread::sleep(std::time::Duration::from_millis(100));
        let (status, response) = post(addr, r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#);
        assert_eq!(status, 200, "{response}");
        let actor = seen.lock().expect("the slot lock").clone();
        assert!(
            !actor.is_empty(),
            "an authenticated MCP call left the access line with no actor"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
