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

use crate::adapters::auth_gate::{self, Request as AuthRequest};
use crate::core::{Clock, IdFactory};
use crate::errors::VogtError;
use crate::registry::{default_registry, HttpMethod, Operation, OperationRegistry, Transport};
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
            no_auth,
            writes_enabled,
        }
    }
}

/// The router for the registry surface. Health routes stay on their own router
/// and are merged in by `serve`.
pub fn router<C: Clock + Send + 'static, I: IdFactory + Send + 'static>(
    state: AppState<C, I>,
) -> Router {
    Router::new().fallback(dispatch).with_state(Arc::new(state))
}

async fn dispatch<C: Clock, I: IdFactory>(
    State(state): State<Arc<AppState<C, I>>>,
    request: Request<Body>,
) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_string();
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
    // The body is checked before the gate. A malformed request is the caller's
    // mistake and answers 422 with no auth row, rather than a 401 or a 403.
    if let Err(rejection) = validate_body(operation, &body) {
        return invalid_arguments(&rejection);
    }
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
    if let Err(denial) = granted {
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
    match operation.run(None, serde_json::Value::Null) {
        Ok(_value) => json_response(
            StatusCode::OK,
            serde_json::json!({"operation": operation.name}),
        ),
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

/// Why a body was rejected, in the shape Python's 422 detail carries.
struct InvalidField {
    loc: &'static str,
    message: String,
}

/// Check the body against the operation's parameter schema before the gate runs.
///
/// A read takes its parameters from the query string, so its body is not
/// checked. A write must be a JSON object carrying every field the schema marks
/// required. This is the part of validation that decides whether the request is
/// well-formed at all; the finer type checks land with the service.
fn validate_body(operation: &Operation, body: &[u8]) -> Result<(), InvalidField> {
    if !operation.mutating {
        return Ok(());
    }
    if body.is_empty() {
        return Err(InvalidField {
            loc: "body",
            message: "a request body is required".to_string(),
        });
    }
    let parsed: serde_json::Value = serde_json::from_slice(body).map_err(|_| InvalidField {
        loc: "body",
        message: "the request body is not valid JSON".to_string(),
    })?;
    let Some(object) = parsed.as_object() else {
        return Err(InvalidField {
            loc: "body",
            message: "the request body must be a JSON object".to_string(),
        });
    };
    let required = crate::registry::params_schema_for(operation.name)
        .and_then(|schema| schema.get("required"))
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();
    for field in required {
        let Some(name) = field.as_str() else { continue };
        if !object.contains_key(name) {
            return Err(InvalidField {
                loc: "body",
                message: format!("missing required field '{name}'"),
            });
        }
    }
    Ok(())
}

fn invalid_arguments(rejection: &InvalidField) -> Response {
    json_response(
        StatusCode::UNPROCESSABLE_ENTITY,
        serde_json::json!({
            "error": {
                "code": "invalid_arguments",
                "message": "request does not match the operation's parameters",
                "detail": [{"loc": [rejection.loc], "msg": rejection.message, "type": "missing"}],
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
        let (status, body) = request(running.addr, "GET", "/api/work/get", None);
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
        assert_eq!(json["operation"], "registry.dump");
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
}
