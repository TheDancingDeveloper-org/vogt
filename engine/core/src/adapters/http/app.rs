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
use crate::core::{local_principal, os_user, ActorKind, Clock, IdFactory, Principal};
use crate::errors::VogtError;
use crate::registry::{
    default_registry, validate, HttpMethod, Operation, OperationRegistry, Transport,
};
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
            no_auth,
            writes_enabled,
        }
    }
}

/// The router for the registry surface. Health routes stay on their own router
/// and are merged in by `serve`. Login is mounted by hand, ahead of the
/// fallback: the caller holds no credential yet, so it never enters the gate.
pub fn router<C: Clock + Send + 'static, I: IdFactory + Send + 'static>(
    state: AppState<C, I>,
) -> Router {
    Router::new()
        .route("/api/auth/login", axum::routing::post(login))
        .fallback(dispatch)
        .with_state(Arc::new(state))
}

/// `POST /api/auth/login`. Public, like Python's hand-mounted route. The body
/// is validated before anything else, and every refusal after that is the
/// service's own answer: one sentence at 401, or the throttled error at 429.
/// The decision rows are written inside `login_op`, with transport `http`.
async fn login<C: Clock, I: IdFactory>(
    State(state): State<Arc<AppState<C, I>>>,
    request: Request<Body>,
) -> Response {
    let body = axum::body::to_bytes(request.into_body(), 1024 * 1024)
        .await
        .unwrap_or_default();
    let params =
        match serde_json::from_slice::<serde_json::Value>(&body) {
            Ok(serde_json::Value::Object(object)) => serde_json::Value::Object(object),
            _ => return invalid_arguments(&VogtError::InvalidRequest(
                "invalid arguments for auth.login:\n1 validation error for request body\nbody\n  \
                 Input should be a valid JSON object"
                    .to_string(),
            )),
        };
    for field in ["username", "password"] {
        if params
            .get(field)
            .and_then(serde_json::Value::as_str)
            .is_none()
        {
            return invalid_arguments(&VogtError::InvalidRequest(format!(
                "invalid arguments for auth.login:\n1 validation error for LoginParams\n{field}\n  \
                 Field required"
            )));
        }
    }
    let built = context_for_login(&state);
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

async fn dispatch<C: Clock, I: IdFactory>(
    State(state): State<Arc<AppState<C, I>>>,
    request: Request<Body>,
) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_string();
    let query = request.uri().query().map(str::to_string);
    let presented = bearer(request.headers().get("authorization"));
    let body = axum::body::to_bytes(request.into_body(), 1024 * 1024)
        .await
        .unwrap_or_default();
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
    let params = match parse_params(operation, query.as_deref(), &body) {
        Ok(params) => params,
        Err(error) => return invalid_arguments(&error),
    };
    let params = match validate::prepare(operation.name, params) {
        Ok(params) => params,
        Err(error) => return invalid_arguments(&error),
    };
    // The gate records its decision before the operation runs, so a request
    // that will be refused never reaches a handler.
    let granted = {
        let store = state.store.lock().expect("the store lock is not poisoned");
        let now = store
            .clock()
            .lock()
            .expect("the clock lock is not poisoned")
            .now();
        auth_gate::authorize(
            &*store,
            AuthRequest {
                operation,
                transport: Transport::Http,
                presented: presented.as_deref(),
                no_auth: state.no_auth,
                writes_enabled: state.writes_enabled,
                now,
            },
        )
    };
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
    let built = context_for(&state, &grant);
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
    body: &[u8],
) -> Result<serde_json::Value, VogtError> {
    if operation.route.method == HttpMethod::Get {
        return Ok(query_object(query));
    }
    if body.is_empty() {
        return Ok(serde_json::Value::Null);
    }
    match serde_json::from_slice::<serde_json::Value>(body) {
        Ok(serde_json::Value::Object(object)) => Ok(serde_json::Value::Object(object)),
        _ => Err(VogtError::InvalidRequest(format!(
            "invalid arguments for {}:\n1 validation error for request body\nbody\n  Input should be a valid JSON object",
            operation.name
        ))),
    }
}

/// Query parameters as one flat object. A repeated key becomes a list, which is
/// how a caller passes an array field; everything else is a string and the
/// validator decides whether that string is acceptable. Percent-encoding is
/// decoded because that is what the query string is.
fn query_object(query: Option<&str>) -> serde_json::Value {
    let mut object = serde_json::Map::new();
    for pair in query
        .unwrap_or("")
        .split('&')
        .filter(|pair| !pair.is_empty())
    {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let key = percent_decode(key);
        let value = serde_json::Value::String(percent_decode(value));
        match object.get_mut(&key) {
            Some(serde_json::Value::Array(items)) => items.push(value),
            Some(existing) => {
                let first = existing.take();
                *existing = serde_json::Value::Array(vec![first, value]);
            }
            None => {
                object.insert(key, value);
            }
        }
    }
    serde_json::Value::Object(object)
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => out.push(b' '),
            b'%' if index + 2 < bytes.len() => {
                let hex = &text[index + 1..index + 3];
                match u8::from_str_radix(hex, 16) {
                    Ok(byte) => out.push(byte),
                    Err(_) => out.extend_from_slice(&bytes[index..index + 3]),
                }
                index += 2;
            }
            byte => out.push(byte),
        }
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The context a ported service runs in. The grant names the caller; the data
/// directory is the one the route's own store was opened on.
fn context_for<C: Clock, I: IdFactory>(
    state: &AppState<C, I>,
    grant: &Grant,
) -> Option<crate::application::context::Built> {
    let config = crate::config::VogtConfig {
        data_dir: state.data_dir.clone(),
        ..crate::config::VogtConfig::default()
    };
    let principal = match &grant.identity_ref {
        Some(identity_ref) if !identity_ref.is_empty() => {
            Principal::new(identity_ref, ActorKind::Human, identity_ref).ok()
        }
        _ => Some(local_principal(&os_user())),
    };
    crate::application::context::build_context(
        config, principal, None, None, None, None, None, None,
    )
    .ok()
}

/// The context a login runs in. Nobody is authenticated yet, so there is no
/// principal and no token; `login_op` names the actor itself once the password
/// matches. The data directory is the route's own store.
fn context_for_login<C: Clock, I: IdFactory>(
    state: &AppState<C, I>,
) -> Option<crate::application::context::Built> {
    let config = crate::config::VogtConfig {
        data_dir: state.data_dir.clone(),
        ..crate::config::VogtConfig::default()
    };
    crate::application::context::build_context(config, None, None, None, None, None, None, None)
        .ok()
}

/// Python's 422 envelope. The validator reports one error as a single
/// `InvalidRequest`, so the detail carries that whole message; the code and the
/// fixed message are what `tests/test_http.py` asserts.
fn invalid_arguments(error: &VogtError) -> Response {
    json_response(
        StatusCode::UNPROCESSABLE_ENTITY,
        serde_json::json!({
            "error": {
                "code": "invalid_arguments",
                "message": "request does not match the operation's parameters",
                "detail": [{"loc": ["body"], "msg": error.message(), "type": "value_error"}],
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
            true,
            crate::core::SystemClock,
            crate::core::SequentialIds::new(None).unwrap(),
        );
        runtime.spawn(async move {
            let listener = tokio::net::TcpListener::from_std(listener).unwrap();
            axum::serve(listener, router(state)).await.unwrap();
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
        let mut stream = std::net::TcpStream::connect(addr).unwrap();
        write!(
            stream,
            "POST {path} HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
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
    fn a_body_missing_required_fields_is_rejected_before_auth() {
        let running = serve(false);
        let (status, body) = post_body(running.addr, "/api/work", "{}");
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
        let (status, body) = post_body(running.addr, "/api/auth/login", "{}");
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(status, 422, "{body}");
        assert_eq!(json["error"]["code"], "invalid_arguments");
    }

    #[test]
    fn login_refuses_an_unknown_user_with_the_one_sentence() {
        let running = serve(true);
        let (status, body) = post_body(
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
