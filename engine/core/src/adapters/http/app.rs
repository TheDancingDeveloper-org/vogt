//! Registry-generated routes. Ports `src/vogt/adapters/http/app.py`.
//!
//! One route per operation the registry exposes on HTTP, under `/api`. The
//! handler is not written per operation: the registry says the method, the
//! path and the summary, and this module dispatches to `Operation::run`. A
//! service that has not landed answers with the honest-unavailable error
//! rather than an empty success, in the same envelope every other failure
//! uses.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::State;
use axum::http::{Method, Request, StatusCode};
use axum::response::Response;
use axum::Router;

use crate::errors::VogtError;
use crate::registry::{default_registry, HttpMethod, OperationRegistry};

/// The prefix every registry route lives under. The engine's front door
/// forwards `/api` untouched.
pub const API_PREFIX: &str = "/api";

/// What a route needs to answer. The registry is shared because every request
/// looks its operation up by method and path.
#[derive(Clone)]
pub struct AppState {
    pub registry: Arc<OperationRegistry>,
}

impl AppState {
    pub fn from_default() -> Self {
        Self {
            registry: Arc::new(default_registry()),
        }
    }
}

/// The router for the registry surface. Health routes stay on their own router
/// and are merged in by `serve`.
pub fn router(state: AppState) -> Router {
    Router::new().fallback(dispatch).with_state(Arc::new(state))
}

async fn dispatch(State(state): State<Arc<AppState>>, request: Request<Body>) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_string();
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
    let found = state
        .registry
        .for_transport(crate::registry::Transport::Http);
    let Some(operation) = found.into_iter().find(|operation| {
        operation.route.method == wanted && operation.route.path == operation_path
    }) else {
        return not_found();
    };
    match operation.run() {
        Ok(()) => json_response(
            StatusCode::OK,
            serde_json::json!({"operation": operation.name, "available": true}),
        ),
        Err(error) => error_response(&error),
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

    fn serve() -> (std::net::SocketAddr, tokio::runtime::Runtime) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.spawn(async move {
            let listener = tokio::net::TcpListener::from_std(listener).unwrap();
            axum::serve(listener, router(AppState::from_default()))
                .await
                .unwrap();
        });
        std::thread::sleep(std::time::Duration::from_millis(100));
        (addr, runtime)
    }

    fn get(addr: std::net::SocketAddr, path: &str) -> (u16, String) {
        let mut stream = std::net::TcpStream::connect(addr).unwrap();
        use std::io::{Read, Write};
        write!(
            stream,
            "GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        let mut buf = String::new();
        stream.read_to_string(&mut buf).unwrap();
        let (head, body) = buf.split_once("\r\n\r\n").unwrap();
        let status = head.split_whitespace().nth(1).unwrap().parse().unwrap();
        (status, body.to_string())
    }

    #[test]
    fn an_unported_operation_says_so_instead_of_succeeding() {
        let (addr, _runtime) = serve();
        let (status, body) = get(addr, "/api/status");
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(status, 400);
        assert_eq!(json["error"]["code"], "invalid_request");
        assert!(json["error"]["message"]
            .as_str()
            .unwrap()
            .contains("status"));
    }

    #[test]
    fn registry_dump_is_available() {
        let (addr, _runtime) = serve();
        let (status, body) = get(addr, "/api/registry");
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(status, 200, "{body}");
        assert_eq!(json["operation"], "registry.dump");
        assert_eq!(json["available"], true);
    }

    #[test]
    fn a_path_outside_the_registry_is_not_found() {
        let (addr, _runtime) = serve();
        let (status, body) = get(addr, "/api/no-such-thing");
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(status, 404);
        assert_eq!(json["error"]["code"], "not_found");
    }

    #[test]
    fn a_local_only_operation_is_not_on_http() {
        let (addr, _runtime) = serve();
        let (status, _) = get(addr, "/api/instance/init");
        assert_eq!(status, 404, "init is local-only");
    }
}
