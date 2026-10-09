//! Registry-generated routes. Ports `src/vogt/adapters/http/app.py`.
//!
//! One route per operation the registry exposes on HTTP, under `/api`. The
//! handler is not written per operation: the registry says the method, the
//! path and the summary, and this module dispatches to `Operation::run`. A
//! service that has not landed answers with the honest-unavailable error
//! rather than an empty success, in the same envelope every other failure
//! uses.

use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::State;
use axum::http::{Method, Request, StatusCode};
use axum::response::Response;
use axum::Router;

use crate::adapters::auth_gate::{self, Grant, Request as AuthRequest};
use crate::core::{local_principal, os_user, Clock, IdFactory, Principal};
use crate::errors::VogtError;
use crate::registry::{
    default_registry, validate, HttpMethod, Operation, OperationRegistry, Transport,
};
use crate::storage::interface::DeclaredStore;
use crate::storage::sqlite::declared::SqliteDeclaredStore;

/// The prefix every registry route lives under. The engine's front door
/// forwards `/api` untouched.
pub const API_PREFIX: &str = "/api";

/// What a route needs to answer. The registry is shared because every request
/// looks its operation up by method and path. The store is behind a mutex
/// because its clock and id factory are not shareable across tasks.
pub struct AppState<C, I> {
    pub registry: Arc<OperationRegistry>,
    store: Mutex<SqliteDeclaredStore<C, I>>,
    data_dir: std::path::PathBuf,
    /// Loaded once, at startup. A request must not call `load_config` itself:
    /// the file and the environment are process-wide, and reading them per
    /// request is where `VOGT_CONTRACT_VERSION` and `VOGT_SESSION_TTL_DAYS` got
    /// lost behind `VogtConfig::default()`.
    config: crate::config::VogtConfig,
    pub no_auth: bool,
    pub writes_enabled: bool,
}

impl<C: Clock, I: IdFactory> AppState<C, I> {
    pub fn new(
        data_dir: &std::path::Path,
        no_auth: bool,
        writes_enabled: bool,
        clock: C,
        ids: I,
    ) -> Self {
        Self {
            registry: Arc::new(default_registry()),
            store: Mutex::new(SqliteDeclaredStore::new(
                crate::storage::sqlite::declared_path(data_dir),
                clock,
                ids,
            )),
            data_dir: data_dir.to_path_buf(),
            config: loaded_config(data_dir),
            no_auth,
            writes_enabled,
        }
    }

    /// A state whose store shares the clock and id factory of an existing one.
    pub fn joined(
        data_dir: &std::path::Path,
        no_auth: bool,
        writes_enabled: bool,
        source: &SqliteDeclaredStore<C, I>,
    ) -> Self {
        Self {
            registry: Arc::new(default_registry()),
            store: Mutex::new(source.joined(crate::storage::sqlite::declared_path(data_dir))),
            data_dir: data_dir.to_path_buf(),
            config: loaded_config(data_dir),
            no_auth,
            writes_enabled,
        }
    }
}

/// The process config, with this route's data directory. Read once, when the
/// state is built. A config that cannot be read is fatal: falling back to the
/// defaults would silently turn `install_bootstrap_enabled` back on for an
/// operator who had switched it off. `serve` has already exited on the same
/// error, so a failure here means the state was built some other way. The MCP
/// route reads through this same function, so both surfaces share one config.
pub(crate) fn loaded_config(data_dir: &std::path::Path) -> crate::config::VogtConfig {
    let mut config = crate::config::load_config(&serde_json::Map::new())
        .expect("the configuration could not be read; refusing to serve on the defaults");
    config.data_dir = data_dir.to_path_buf();
    config
}

/// The router for the registry surface. Health routes stay on their own router
/// and are merged in by `serve`. Login is mounted by hand, ahead of the
/// fallback: the caller holds no credential yet, so it never enters the gate.
/// One function per hook pair, the way the MCP route is split. A request's
/// context has to be a `Built`, which only exists for these four pairs, so each
/// router names its pair and builds the context on the store's own clock and id
/// factory rather than a fresh one.
macro_rules! api_route {
    ($name:ident, $clock:ty, $ids:ty, $variant:ident, shared) => {
        api_route!(@build $name, $clock, $ids, $variant, crate::adapters::http::mcp::clock_for::<$clock>);
    };
    ($name:ident, $clock:ty, $ids:ty, $variant:ident, restart) => {
        api_route!(@build $name, $clock, $ids, $variant, crate::adapters::http::mcp::step_clock_for);
    };
    (@build $name:ident, $clock:ty, $ids:ty, $variant:ident, $clock_for:path) => {
        pub fn $name(state: AppState<$clock, $ids>) -> Router {
            fn build(
                config: crate::config::VogtConfig,
                principal: Option<Principal>,
                clock: Arc<Mutex<$clock>>,
                ids: Arc<Mutex<$ids>>,
                token: Option<crate::core::Token>,
            ) -> crate::application::context::Built {
                crate::application::context::Built::$variant(
                    crate::application::context::context_on(config, principal, clock, ids, token),
                )
            }
            Router::new()
                .route(
                    "/api/auth/login",
                    axum::routing::post(login::<$clock, $ids>),
                )
                .route(
                    "/api/install/status",
                    axum::routing::get(install_status::<$clock, $ids>),
                )
                .route(
                    "/api/install/bootstrap",
                    axum::routing::post(install_bootstrap::<$clock, $ids>),
                )
                .fallback(dispatch::<$clock, $ids>)
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
    Option<Principal>,
    Arc<Mutex<C>>,
    Arc<Mutex<I>>,
    Option<crate::core::Token>,
) -> crate::application::context::Built;

type Routed<C, I> = (Arc<AppState<C, I>>, ContextBuild<C, I>, ClockFor<C>);

type ClockFor<C> = fn(&Arc<Mutex<C>>) -> Arc<Mutex<C>>;

api_route!(
    router_system_random,
    crate::application::context::SystemClock,
    crate::application::context::RandomIds,
    SystemRandom,
    shared
);
api_route!(
    router_system_sequential,
    crate::application::context::SystemClock,
    crate::core::SequentialIds,
    SystemSequential,
    shared
);
api_route!(
    router_step_random,
    crate::core::StepClock,
    crate::application::context::RandomIds,
    StepRandom,
    restart
);
api_route!(
    router_step_sequential,
    crate::core::StepClock,
    crate::core::SequentialIds,
    StepSequential,
    restart
);

/// `POST /api/auth/login`. Public, like Python's hand-mounted route. The body
/// is validated before anything else, and every refusal after that is the
/// service's own answer: one sentence at 401, or the throttled error at 429.
/// The decision rows are written inside `login_op`, with transport `http`.
async fn login<C: Clock, I: IdFactory>(
    State((state, build, clock_for)): State<Routed<C, I>>,
    request: Request<Body>,
) -> Response {
    // `text/plain` is a CORS simple request, so a missing or wrong content type
    // must be refused before the body is read. Otherwise any web page can POST a
    // username and burn its throttle window.
    if !json_content_type(request.headers().get("content-type")) {
        return invalid_arguments(&VogtError::InvalidRequest(
            "invalid arguments for auth.login:\n1 validation error for request body\nbody\n  \
             Input should be a valid JSON object"
                .to_string(),
        ));
    }
    let body = axum::body::to_bytes(request.into_body(), 1024 * 1024)
        .await
        .unwrap_or_default();
    let params = match login_params(&body) {
        Ok(params) => params,
        Err(error) => return invalid_arguments(&error),
    };
    let request_store = request_store(&state, clock_for);
    let built = context_for_login(&request_store, &state, build);
    let Some(built) = built else {
        return error_response(&VogtError::InvalidRequest(
            "the request context could not be built".to_string(),
        ));
    };
    match crate::application::services::auth::login_op(&built, params) {
        Ok(value) => json_response(StatusCode::OK, value),
        Err(error) => error_response(&error),
    }
}

/// `LoginParams` (`models.py`). Not a registry operation, so the shared
/// validator has no schema for it; the constraints are small enough to state.
/// `username` and `session_name` are `Name` (stripped, at least one character),
/// `password` is at least one character, and an extra field is forbidden.
/// Anything else is a 422 that never reaches `login_op`, so it counts no
/// throttle failure and writes no decision row.
fn login_params(body: &[u8]) -> Result<serde_json::Value, VogtError> {
    let object =
        match serde_json::from_slice::<serde_json::Value>(body) {
            Ok(serde_json::Value::Object(object)) => object,
            _ => return Err(VogtError::InvalidRequest(
                "invalid arguments for auth.login:\n1 validation error for request body\nbody\n  \
                 Input should be a valid JSON object"
                    .to_string(),
            )),
        };
    for key in object.keys() {
        if !["username", "password", "session_name"].contains(&key.as_str()) {
            return Err(VogtError::InvalidRequest(format!(
                "invalid arguments for auth.login:\n1 validation error for LoginParams\n{key}\n  \
                 Extra inputs are not permitted"
            )));
        }
    }
    let mut out = serde_json::Map::new();
    for field in ["username", "password"] {
        let Some(text) = object.get(field).and_then(serde_json::Value::as_str) else {
            return Err(VogtError::InvalidRequest(format!(
                "invalid arguments for auth.login:\n1 validation error for LoginParams\n{field}\n  \
                 Field required"
            )));
        };
        let text = if field == "username" {
            text.trim()
        } else {
            text
        };
        if text.is_empty() {
            return Err(VogtError::InvalidRequest(format!(
                "invalid arguments for auth.login:\n1 validation error for LoginParams\n{field}\n  \
                 String should have at least 1 character"
            )));
        }
        out.insert(
            field.to_string(),
            serde_json::Value::String(text.to_string()),
        );
    }
    let session_name = match object.get("session_name") {
        None => "browser session".to_string(),
        Some(serde_json::Value::String(name)) if !name.trim().is_empty() => name.trim().to_string(),
        Some(_) => {
            return Err(VogtError::InvalidRequest(
                "invalid arguments for auth.login:\n1 validation error for LoginParams\n\
                 session_name\n  Input should be a valid string"
                    .to_string(),
            ))
        }
    };
    out.insert(
        "session_name".to_string(),
        serde_json::Value::String(session_name),
    );
    Ok(serde_json::Value::Object(out))
}

fn json_content_type(header: Option<&axum::http::HeaderValue>) -> bool {
    header
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .eq_ignore_ascii_case("application/json")
        })
}

/// `GET /api/install/status`. Public, like Python's hand-mounted route: it
/// answers a browser that holds no credential, and states one boolean that
/// caller could infer by trying to bootstrap anyway.
async fn install_status<C: Clock, I: IdFactory>(
    State((state, build, clock_for)): State<Routed<C, I>>,
) -> Response {
    let Some(built) = context_for_login(&request_store(&state, clock_for), &state, build) else {
        return error_response(&VogtError::InvalidRequest(
            "the request context could not be built".to_string(),
        ));
    };
    match crate::application::services::install::install_status_op(&built, serde_json::Value::Null)
    {
        Ok(value) => json_response(StatusCode::OK, value),
        Err(error) => error_response(&error),
    }
}

/// `POST /api/install/bootstrap`. Public, and validated before the service. A
/// body that is not JSON, or that fails `InstallBootstrapParams`, answers 422
/// and the store is never opened, so a bad request cannot race the one-shot
/// bootstrap or write a row.
async fn install_bootstrap<C: Clock, I: IdFactory>(
    State((state, build, clock_for)): State<Routed<C, I>>,
    request: Request<Body>,
) -> Response {
    if !json_content_type(request.headers().get("content-type")) {
        return invalid_arguments(&VogtError::InvalidRequest(
            "invalid arguments for install.bootstrap:\n1 validation error for request body\nbody\n  \
             Input should be a valid JSON object"
                .to_string(),
        ));
    }
    let body = axum::body::to_bytes(request.into_body(), 1024 * 1024 + 1).await;
    let body = match body {
        Ok(bytes) if bytes.len() > 1024 * 1024 => return payload_too_large(),
        Ok(bytes) => bytes,
        Err(_) => return payload_too_large(),
    };
    let params = match bootstrap_params(&body) {
        Ok(params) => params,
        Err(error) => return invalid_arguments(&error),
    };
    let Some(built) = context_for_login(&request_store(&state, clock_for), &state, build) else {
        return error_response(&VogtError::InvalidRequest(
            "the request context could not be built".to_string(),
        ));
    };
    match crate::application::services::install::install_bootstrap_op(&built, params) {
        Ok(value) => json_response(StatusCode::OK, value),
        Err(error) => error_response(&error),
    }
}

/// `InstallBootstrapParams` (`models.py`). `display_name` is required and every
/// field present must be a string; `Name` fields are stripped and at least one
/// character. Not a registry operation, so the shared validator has no schema.
fn bootstrap_params(body: &[u8]) -> Result<serde_json::Value, VogtError> {
    let object = match serde_json::from_slice::<serde_json::Value>(body) {
        Ok(serde_json::Value::Object(object)) => object,
        _ => {
            return Err(VogtError::InvalidRequest(
                "invalid arguments for install.bootstrap:\n1 validation error for request body\nbody\n  \
                 Input should be a valid JSON object"
                    .to_string(),
            ))
        }
    };
    for key in object.keys() {
        if ![
            "display_name",
            "identity_ref",
            "token_name",
            "username",
            "password",
        ]
        .contains(&key.as_str())
        {
            return Err(VogtError::InvalidRequest(format!(
                "invalid arguments for install.bootstrap:\n1 validation error for \
                 InstallBootstrapParams\n{key}\n  Extra inputs are not permitted"
            )));
        }
    }
    let mut out = serde_json::Map::new();
    // `token_name` is a plain string with a default, so an explicit null is a
    // type error rather than "use the default". The other three are `Name | None`,
    // where null and absence mean the same thing and the service fills in.
    for field in ["display_name", "identity_ref", "token_name", "username"] {
        let nullable = ["identity_ref", "username"].contains(&field);
        match object.get(field) {
            None => {}
            Some(serde_json::Value::Null) if nullable => {}
            Some(serde_json::Value::String(text)) if !text.trim().is_empty() => {
                out.insert(
                    field.to_string(),
                    serde_json::Value::String(text.trim().to_string()),
                );
            }
            Some(_) => {
                return Err(VogtError::InvalidRequest(format!(
                    "invalid arguments for install.bootstrap:\n1 validation error for \
                     InstallBootstrapParams\n{field}\n  Input should be a valid string"
                )))
            }
        }
    }
    if !out.contains_key("token_name") {
        out.insert(
            "token_name".to_string(),
            serde_json::Value::String("first-run browser token".to_string()),
        );
    }
    if !out.contains_key("display_name") {
        return Err(VogtError::InvalidRequest(
            "invalid arguments for install.bootstrap:\n1 validation error for \
             InstallBootstrapParams\ndisplay_name\n  Field required"
                .to_string(),
        ));
    }
    match object.get("password") {
        None | Some(serde_json::Value::Null) => {}
        Some(serde_json::Value::String(password)) => {
            out.insert(
                "password".to_string(),
                serde_json::Value::String(password.clone()),
            );
        }
        Some(_) => {
            return Err(VogtError::InvalidRequest(
                "invalid arguments for install.bootstrap:\n1 validation error for \
                 InstallBootstrapParams\npassword\n  Input should be a valid string"
                    .to_string(),
            ))
        }
    }
    Ok(serde_json::Value::Object(out))
}

async fn dispatch<C: Clock, I: IdFactory>(
    State((state, build, clock_for)): State<Routed<C, I>>,
    request: Request<Body>,
) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_string();
    let query = request.uri().query().map(str::to_string);
    let presented = bearer(request.headers().get("authorization"));
    let content_type = request.headers().get("content-type").cloned();
    // Read one byte past the limit so an oversized body is distinguishable from
    // one that fits. `unwrap_or_default` used to turn a too-large body into an
    // empty one, and the caller was told their parameters were wrong.
    let read = axum::body::to_bytes(request.into_body(), 1024 * 1024 + 1).await;
    let body = match read {
        Ok(bytes) if bytes.len() > 1024 * 1024 => return payload_too_large(),
        Ok(bytes) => bytes,
        Err(err) if err.to_string().contains("length limit") => return payload_too_large(),
        Err(_) => return payload_too_large(),
    };
    let Some(operation_path) = path.strip_prefix(API_PREFIX) else {
        return not_found();
    };
    let wanted = match method {
        Method::GET => HttpMethod::Get,
        Method::POST => HttpMethod::Post,
        Method::PATCH => HttpMethod::Patch,
        Method::DELETE => HttpMethod::Delete,
        _ => return method_not_allowed(),
    };
    let found = state.registry.for_transport(Transport::Http);
    let Some(operation) = found.into_iter().find(|operation| {
        operation.route.method == wanted && operation.route.path == operation_path
    }) else {
        return not_found();
    };
    // Parse, then validate, and only then the gate. Both are the caller's
    // mistake, so both answer 422 and write no auth row. The check is the shared
    // one `Operation::run` applies, not one of this adapter's own.
    let params = match parse_params(operation, query.as_deref(), &content_type, &body) {
        Ok(params) => params,
        Err(error) => return invalid_arguments(&error),
    };
    let params = match validate::prepare(operation.name, params) {
        Ok(params) => params,
        Err(error) => return invalid_arguments(&error),
    };
    // The gate and the operation read one clock. On the step routes that clock is
    // fresh for this request and restarts at the hook's start (`step_clock_for`),
    // the same helper `/mcp` uses. `--no-auth` is Python's `local()`, which reads
    // no clock at all, so the operation's own first read stays at the start.
    let request_store = request_store(&state, clock_for);
    let now = if state.no_auth {
        crate::core::Moment::from_unix(0, 0)
    } else {
        request_store.now()
    };
    let granted = auth_gate::authorize(
        &request_store,
        AuthRequest {
            operation,
            transport: Transport::Http,
            presented: presented.as_deref(),
            no_auth: state.no_auth,
            writes_enabled: state.writes_enabled,
            now,
        },
        state.config.session_ttl_days,
    );
    let grant = match granted {
        Ok(grant) => grant,
        Err(denial) => {
            if matches!(denial, auth_gate::Denial::Unrecorded { .. }) {
                // A decision that could not be recorded must not describe the store
                // failure. The client gets the same plain 500 `/mcp` gives.
                return Response::builder()
                    .status(StatusCode::INTERNAL_SERVER_ERROR)
                    .header("content-type", "text/plain; charset=utf-8")
                    .body(Body::from("Internal Server Error"))
                    .expect("a fixed response builds");
            }
            return error_response(&denial.error());
        }
    };
    let built = context_for(&request_store, &state, &grant, build);
    match operation.run(built.as_ref(), params) {
        Ok(value) => json_response(StatusCode::OK, value),
        // Not ported is not the caller's fault, so it is not a 400. The shared
        // error taxonomy has no 501, and adding one would change every adapter.
        Err(VogtError::InvalidRequest(message)) if message.contains("has not been ported") => {
            json_response(
                StatusCode::NOT_IMPLEMENTED,
                serde_json::json!({"error": {"code": "not_implemented", "message": message}}),
            )
        }
        Err(error) => error_response(&error),
    }
}

/// Read the caller's parameters. A read takes them from the query string and a
/// write from the JSON body; an absent body is the empty object the validator
/// fills defaults into. Anything that is not JSON, or not an object, is the
/// same failure the validator reports for a value of the wrong shape.
fn parse_params(
    operation: &Operation,
    query: Option<&str>,
    content_type: &Option<axum::http::HeaderValue>,
    body: &[u8],
) -> Result<serde_json::Value, VogtError> {
    if operation.route.method == HttpMethod::Get {
        return Ok(query_object(operation.name, query));
    }
    // A write with no body is the empty object the validator fills defaults into.
    // A write that carries a body must say it is JSON: `text/plain` is a CORS
    // simple request, and accepting it lets any web page write to a `--no-auth`
    // server.
    if body.is_empty() {
        return Ok(serde_json::Value::Null);
    }
    if !json_content_type(content_type.as_ref()) {
        return Err(VogtError::InvalidRequest(format!(
            "invalid arguments for {}:\n1 validation error for request body\nbody\n  Input should be a valid JSON object",
            operation.name
        )));
    }
    match serde_json::from_slice::<serde_json::Value>(body) {
        Ok(serde_json::Value::Object(object)) => Ok(serde_json::Value::Object(object)),
        _ => Err(VogtError::InvalidRequest(format!(
            "invalid arguments for {}:\n1 validation error for request body\nbody\n  Input should be a valid JSON object",
            operation.name
        ))),
    }
}

/// Query parameters as one flat object. A repeated key keeps its last value,
/// which is what FastAPI does for a scalar field; building an array made
/// `?limit=5&limit=10` a 422. Each value is coerced to the type the operation's
/// schema declares, because a query string only ever carries text and the
/// validator does not coerce: `limit=5` has to arrive as a number, not the
/// string `"5"`. Percent-encoding is decoded on bytes, never by slicing the
/// string, so a `%` followed by a non-boundary byte is a bad query rather than
/// a panic.
fn query_object(operation: &str, query: Option<&str>) -> serde_json::Value {
    let schema = crate::registry::params_schema_for(operation);
    let mut object = serde_json::Map::new();
    for pair in query
        .unwrap_or("")
        .split('&')
        .filter(|pair| !pair.is_empty())
    {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let key = percent_decode(key);
        let decoded = percent_decode(value);
        let coerced = coerce_query(&schema, &key, &decoded);
        // A repeated key on a list field is the list. FastAPI collects
        // `?sources=a&sources=b` into `["a", "b"]`; keeping only the last value
        // hands the validator a string and it reports `list_type`. A scalar keeps
        // the last value, because a list of one is not that scalar.
        let repeats = schema
            .and_then(|schema| schema.pointer(&format!("/properties/{key}/type")))
            .and_then(serde_json::Value::as_str)
            == Some("array");
        match object.get_mut(&key) {
            Some(existing) if repeats => match existing {
                serde_json::Value::Array(items) => items.push(coerced),
                single => {
                    let first = single.take();
                    *single = serde_json::Value::Array(vec![first, coerced]);
                }
            },
            _ => {
                object.insert(key, coerced);
            }
        }
    }
    serde_json::Value::Object(object)
}

/// The schema's declared type for one field, unwrapping a nullable union. The
/// query string has no way to say which type it meant, so a value that parses as
/// that type is sent as that type and anything else stays a string for the
/// validator to reject.
fn coerce_query(schema: &Option<&serde_json::Value>, field: &str, text: &str) -> serde_json::Value {
    let kind = schema
        .and_then(|schema| schema.pointer(&format!("/properties/{field}/type")))
        .or_else(|| schema.and_then(|schema| schema.pointer(&format!("/properties/{field}/anyOf"))))
        .and_then(json_type);
    match kind {
        Some("integer") => text
            .parse::<i64>()
            .map_or(serde_json::Value::String(text.to_string()), |n| {
                serde_json::json!(n)
            }),
        Some("number") => text
            .parse::<f64>()
            .map_or(serde_json::Value::String(text.to_string()), |n| {
                serde_json::json!(n)
            }),
        Some("boolean") => match text {
            "true" | "True" | "1" => serde_json::Value::Bool(true),
            "false" | "False" | "0" => serde_json::Value::Bool(false),
            _ => serde_json::Value::String(text.to_string()),
        },
        _ => serde_json::Value::String(text.to_string()),
    }
}

fn json_type(value: &serde_json::Value) -> Option<&str> {
    value.as_str().or_else(|| {
        value.as_array().and_then(|options| {
            options.iter().find_map(|option| {
                option
                    .get("type")
                    .and_then(serde_json::Value::as_str)
                    .filter(|kind| *kind != "null")
            })
        })
    })
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => out.push(b' '),
            b'%' if index + 2 < bytes.len() => {
                let hex = [bytes[index + 1], bytes[index + 2]];
                match std::str::from_utf8(&hex)
                    .ok()
                    .and_then(|hex| u8::from_str_radix(hex, 16).ok())
                {
                    Some(byte) => out.push(byte),
                    None => out.extend_from_slice(&bytes[index..=index + 2]),
                }
                index += 2;
            }
            byte => out.push(byte),
        }
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A store for one request: the process store's id factory, so every route and
/// the CLI count from one sequence, and a clock of the route's choosing. The
/// gate stamps its decision row from the store it is given, so the decision and
/// the service only share an instant when they share this store. The process
/// store keeps its own clock, which the CLI also ticks.
fn request_store<C: Clock, I: IdFactory>(
    state: &AppState<C, I>,
    clock_for: ClockFor<C>,
) -> SqliteDeclaredStore<C, I> {
    let store = state.store.lock().expect("the store lock is not poisoned");
    SqliteDeclaredStore::shared(
        store.path().to_path_buf(),
        clock_for(store.clock()),
        Arc::clone(store.id_factory()),
        store.synchronous(),
    )
}

/// The context a ported service runs in. The grant names the caller; the data
/// directory is the one the route's own store was opened on. The id factory is
/// that store's handle, so the service's writes continue the sequence the
/// decision row started. The clock is the route's choice: a step clock restarts
/// at the hook's start each request, a wall clock is the store's.
fn context_for<C: Clock, I: IdFactory>(
    store: &SqliteDeclaredStore<C, I>,
    state: &AppState<C, I>,
    grant: &Grant,
    build: ContextBuild<C, I>,
) -> Option<crate::application::context::Built> {
    let principal = match &grant.identity_ref {
        Some(identity_ref) if !identity_ref.is_empty() => {
            Principal::new(identity_ref, grant.kind, &grant.display_name).ok()
        }
        _ => Some(local_principal(&os_user())),
    };
    Some(build(
        state.config.clone(),
        principal,
        Arc::clone(store.clock()),
        Arc::clone(store.id_factory()),
        grant.token.clone(),
    ))
}

/// The context a login runs in. Nobody is authenticated yet, so there is no
/// principal and no token; `login_op` names the actor itself once the password
/// matches. The data directory and the id factory are the route's own store;
/// the clock restarts per request the way the other route's does.
fn context_for_login<C: Clock, I: IdFactory>(
    store: &SqliteDeclaredStore<C, I>,
    state: &AppState<C, I>,
    build: ContextBuild<C, I>,
) -> Option<crate::application::context::Built> {
    Some(build(
        state.config.clone(),
        None,
        Arc::clone(store.clock()),
        Arc::clone(store.id_factory()),
        None,
    ))
}

fn payload_too_large() -> Response {
    json_response(
        StatusCode::PAYLOAD_TOO_LARGE,
        serde_json::json!({"error": {"code": "payload_too_large", "message": "the request body exceeds 1 MiB"}}),
    )
}

/// Python's 422 envelope. The validator reports one error as a single
/// `InvalidRequest`, so the detail carries that whole message; the code and the
/// fixed message are what `tests/test_http.py` asserts.
fn invalid_arguments(error: &VogtError) -> Response {
    // Python's `_jsonable_errors` writes one entry per rejected field, and every
    // part of `loc` is a string, index included (`app.py:143`). A failure that
    // carries no structured report falls back to the whole-body entry.
    let detail = match crate::errors::take_validation(error) {
        Some(report) => report
            .errors
            .iter()
            .map(|problem| {
                serde_json::json!({
                    "loc": problem.loc.iter().map(crate::errors::Loc::as_text).collect::<Vec<_>>(),
                    "msg": problem.msg,
                    "type": problem.error_type,
                })
            })
            .collect::<Vec<_>>(),
        None => vec![serde_json::json!({
            "loc": ["body"],
            "msg": error.message(),
            "type": "value_error",
        })],
    };
    json_response(
        StatusCode::UNPROCESSABLE_ENTITY,
        serde_json::json!({
            "error": {
                "code": "invalid_arguments",
                "message": "request does not match the operation's parameters",
                "detail": detail,
            }
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
    if secret.is_empty() {
        None
    } else {
        Some(secret.to_string())
    }
}

fn error_response(error: &VogtError) -> Response {
    let status =
        StatusCode::from_u16(error.http_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    json_response(
        status,
        serde_json::json!({"error": {"code": error.code(), "message": error.message()}}),
    )
}

fn not_found() -> Response {
    error_response(&VogtError::NotFound("no such operation".to_string()))
}

fn method_not_allowed() -> Response {
    error_response(&VogtError::InvalidRequest(
        "this route does not accept that method".to_string(),
    ))
}

fn json_response(status: StatusCode, body: serde_json::Value) -> Response {
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("a fixed response builds")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

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
        serve_with(no_auth, true)
    }

    fn serve_with(no_auth: bool, writes_enabled: bool) -> Running {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("vogt-http-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        crate::application::instance::init(&dir, &mut None, &mut None).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let state = AppState::new(
            &dir,
            no_auth,
            writes_enabled,
            crate::application::context::SystemClock,
            crate::core::SequentialIds::new(None).unwrap(),
        );
        runtime.spawn(async move {
            let listener = tokio::net::TcpListener::from_std(listener).unwrap();
            axum::serve(listener, router_system_sequential(state))
                .await
                .unwrap();
        });
        std::thread::sleep(std::time::Duration::from_millis(150));
        Running {
            addr,
            _runtime: runtime,
            dir,
        }
    }

    fn request(
        addr: std::net::SocketAddr,
        method: &str,
        path: &str,
        bearer: Option<&str>,
    ) -> (u16, String) {
        let mut stream = std::net::TcpStream::connect(addr).unwrap();
        let auth = bearer
            .map(|token| format!("Authorization: Bearer {token}\r\n"))
            .unwrap_or_default();
        write!(
            stream,
            "{method} {path} HTTP/1.1\r\nHost: localhost\r\n{auth}Connection: close\r\n\r\n"
        )
        .unwrap();
        let mut buf = String::new();
        stream.read_to_string(&mut buf).unwrap();
        let (head, body) = buf.split_once("\r\n\r\n").unwrap();
        let status = head.split_whitespace().nth(1).unwrap().parse().unwrap();
        (status, body.to_string())
    }

    fn post_body(addr: std::net::SocketAddr, path: &str, body: &str) -> (u16, String) {
        post_typed(addr, path, body, None)
    }

    fn post_json(addr: std::net::SocketAddr, path: &str, body: &str) -> (u16, String) {
        post_typed(addr, path, body, Some("application/json"))
    }

    fn post_typed(
        addr: std::net::SocketAddr,
        path: &str,
        body: &str,
        content_type: Option<&str>,
    ) -> (u16, String) {
        let mut stream = std::net::TcpStream::connect(addr).unwrap();
        let typed = content_type
            .map(|value| format!("Content-Type: {value}\r\n"))
            .unwrap_or_default();
        write!(
            stream,
            "POST {path} HTTP/1.1\r\nHost: localhost\r\n{typed}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        let mut buf = String::new();
        stream.read_to_string(&mut buf).unwrap();
        let (head, response) = buf.split_once("\r\n\r\n").unwrap();
        let status = head.split_whitespace().nth(1).unwrap().parse().unwrap();
        (status, response.to_string())
    }

    #[test]
    fn a_local_caller_may_write_on_a_read_only_server() {
        // `--no-auth` is the operator at the console. Python lets that caller
        // write even when the server refuses tokens, and records nothing.
        let running = serve_with(true, false);
        let (status, body) = post_json(
            running.addr,
            "/api/labels",
            r#"{"name":"bug","reason":"r"}"#,
        );
        assert_eq!(status, 200, "{body}");
    }

    #[test]
    fn a_repeated_query_key_is_reported_per_field() {
        // The refusal names the field, not the whole body, and it names it once
        // even though the key was given twice.
        let running = serve(true);
        let (status, body) = request(running.addr, "GET", "/api/work?sources=a&sources=b", None);
        assert_eq!(status, 422, "{body}");
        assert!(body.contains("\"loc\":[\"sources\"]"), "{body}");
    }

    #[test]
    fn a_repeated_scalar_keeps_its_last_value() {
        // A scalar field cannot be a list of one, so the last value stands alone.
        let running = serve(true);
        let (status, body) = request(running.addr, "GET", "/api/labels?limit=0&limit=5", None);
        assert_eq!(status, 200, "{body}");
    }

    #[test]
    fn a_numeric_query_reaches_the_operation() {
        let running = serve(true);
        let (status, body) = request(running.addr, "GET", "/api/labels?limit=5", None);
        assert_eq!(status, 200, "{body}");
    }

    #[test]
    fn a_non_json_body_is_rejected_before_it_writes() {
        let running = serve(true);
        let (status, _) = post_body(
            running.addr,
            "/api/labels",
            r#"{"name":"bug","reason":"r"}"#,
        );
        assert_eq!(status, 422);
    }

    #[test]
    fn a_malformed_percent_escape_is_a_bad_query_not_a_panic() {
        let running = serve(true);
        let (status, _) = request(running.addr, "GET", "/api/labels?bogus=%a%C3%A9", None);
        assert!(status != 500, "a bad escape must not drop the connection");
    }

    #[test]
    fn install_status_is_public_and_open_on_an_empty_store() {
        let running = serve(false);
        let (status, body) = request(running.addr, "GET", "/api/install/status", None);
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(status, 200, "{body}");
        assert_eq!(json["install_mode"], true);
    }

    #[test]
    fn a_bad_bootstrap_body_is_rejected_with_no_side_effects() {
        let running = serve(false);
        let (status, _) = post_body(
            running.addr,
            "/api/install/bootstrap",
            r#"{"display_name":"Ada"}"#,
        );
        assert_eq!(
            status, 422,
            "a body without a content type never reaches the service"
        );
        let (status, _) = post_json(running.addr, "/api/install/bootstrap", "{}");
        assert_eq!(status, 422);
        // `token_name` is a plain string with a default, so an explicit null is a
        // type error. Only the fields typed as optional treat null as "not given".
        let (status, _) = post_json(
            running.addr,
            "/api/install/bootstrap",
            r#"{"display_name":"Ada","token_name":null}"#,
        );
        assert_eq!(status, 422);
        // An explicit null is "not given", not a type error. The service then
        // decides it cannot derive an identity, which is its own answer.
        let (status, body) = post_json(
            running.addr,
            "/api/install/bootstrap",
            r#"{"display_name":"!!!","identity_ref":null}"#,
        );
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_ne!(status, 422, "{body}");
        assert_ne!(json["error"]["code"], "invalid_arguments");
        let (status, body) = request(running.addr, "GET", "/api/install/status", None);
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            json["install_mode"], true,
            "a rejected body must not close install mode"
        );
        let _ = status;
    }

    #[test]
    fn bootstrap_names_the_first_operator_exactly_once() {
        let running = serve(false);
        let (status, body) = post_json(
            running.addr,
            "/api/install/bootstrap",
            r#"{"display_name":"Ada Lovelace"}"#,
        );
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(status, 200, "{body}");
        assert!(
            json["secret"].as_str().is_some_and(|s| !s.is_empty()),
            "{body}"
        );
        assert_eq!(json["token"]["name"], "first-run browser token", "{body}");
        let (status, body) = post_json(
            running.addr,
            "/api/install/bootstrap",
            r#"{"display_name":"Grace Hopper"}"#,
        );
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_ne!(status, 200, "{body}");
        assert_eq!(json["error"]["code"], "install_closed");
    }

    #[test]
    fn a_body_missing_required_fields_is_rejected_before_auth() {
        let running = serve(false);
        let (status, body) = post_json(running.addr, "/api/work", "{}");
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(status, 422, "{body}");
        assert_eq!(json["error"]["code"], "invalid_arguments");
    }

    #[test]
    fn no_token_is_refused_before_the_operation_runs() {
        let running = serve(false);
        let (status, body) = request(running.addr, "GET", "/api/registry", None);
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(status, 401, "{body}");
        assert_eq!(json["error"]["code"], "unauthenticated");
        assert_eq!(json["error"]["message"], "no bearer token presented");
    }

    #[test]
    fn a_bad_token_is_refused_too() {
        let running = serve(false);
        let (status, body) = request(running.addr, "GET", "/api/status", Some("not-a-real-token"));
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(status, 401, "{body}");
        assert_eq!(json["error"]["code"], "unauthenticated");
    }

    #[test]
    fn no_auth_reaches_the_operation_and_an_unported_one_says_so() {
        let running = serve(true);
        let (status, body) = request(running.addr, "GET", "/api/work/get?ref=WI-1", None);
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(status, 501, "{body}");
        assert_eq!(json["error"]["code"], "not_implemented");
        assert!(json["error"]["message"]
            .as_str()
            .unwrap()
            .contains("work.get"));
    }

    #[test]
    fn no_auth_serves_a_ported_operation() {
        let running = serve(true);
        let (status, body) = request(running.addr, "GET", "/api/registry", None);
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(status, 200, "{body}");
        assert!(json["operations"]
            .as_array()
            .is_some_and(|ops| !ops.is_empty()));
    }

    #[test]
    fn a_path_outside_the_registry_is_not_found() {
        let running = serve(false);
        let (status, body) = request(running.addr, "GET", "/api/no-such-thing", None);
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(status, 404);
        assert_eq!(json["error"]["code"], "not_found");
    }

    #[test]
    fn a_local_only_operation_is_not_on_http() {
        let running = serve(false);
        let (status, _) = request(running.addr, "GET", "/api/instance/init", None);
        assert_eq!(status, 404, "init is local-only");
    }

    #[test]
    fn login_is_public_and_validates_before_anything_else() {
        let running = serve(false);
        let (status, body) = post_json(running.addr, "/api/auth/login", "{}");
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(status, 422, "{body}");
        assert_eq!(json["error"]["code"], "invalid_arguments");
    }

    #[test]
    fn login_rejects_a_body_that_is_not_json_before_the_throttle() {
        let running = serve(true);
        // No content type at all. This is the cross-site case: a plain form post
        // must not reach the service, because five of them would lock the
        // username out.
        let (status, _) = post_body(
            running.addr,
            "/api/auth/login",
            r#"{"username":"alice","password":"whatever"}"#,
        );
        assert_eq!(status, 422);
        let (status, _) = post_json(
            running.addr,
            "/api/auth/login",
            r#"{"username":"alice","password":"x","extra":1}"#,
        );
        assert_eq!(status, 422);
        let (status, _) = post_json(
            running.addr,
            "/api/auth/login",
            r#"{"username":"alice","password":""}"#,
        );
        assert_eq!(status, 422);
    }

    #[test]
    fn login_refuses_an_unknown_user_with_the_one_sentence() {
        let running = serve(true);
        let (status, body) = post_json(
            running.addr,
            "/api/auth/login",
            r#"{"username":"nobody","password":"whatever"}"#,
        );
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(status, 401, "{body}");
        assert_eq!(json["error"]["code"], "unauthenticated");
        assert_eq!(
            json["error"]["message"],
            "the username or password is not right"
        );
    }
}
