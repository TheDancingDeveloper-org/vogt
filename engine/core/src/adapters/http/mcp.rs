#![allow(dead_code)]
//! The `/mcp` route. Ports the mounting half of `adapters/mcp/http.py`.
//!
//! The behaviour lives in `adapters::mcp::http`; this only carries it over
//! axum. A notification is a 202 with no body, a normal answer is JSON, and a
//! 500 is the bare text the route already decided on.
//!
//! The grant and the recorder are fixed here, and both are wrong for
//! production: every caller gets the full grant, and the authorization is
//! recorded into a sink that writes nothing. Resolving the bearer token into a
//! caller and a `ScopeGrant`, and backing the recorder with the declared store,
//! is the shared authorization work (W7, R55-1). The seam is
//! `respond_recording`; this route is where its result becomes a response.

use axum::body::Body;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::post;
use axum::Router;

use crate::adapters::mcp::http::{
    respond_recording, AuthDecisionRecord, AuthRecorder, McpBody, MCP_PATH,
};

/// What the route records into. The default writes nothing; the shared
/// authorization replaces it with one backed by the declared store.
#[derive(Clone, Default)]
pub struct McpState<R> {
    pub recorder: R,
}

/// The route, with the given recorder and the full grant.
pub fn router<R>(state: McpState<R>) -> Router
where
    R: AuthRecorder + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(MCP_PATH, post(handle))
        .with_state(state)
}

async fn handle<R>(State(state): State<McpState<R>>, body: String) -> Response
where
    R: AuthRecorder + Clone,
{
    let registry = crate::registry::default_registry();
    let grant = crate::adapters::mcp::framing::FullGrant;
    let parsed = match serde_json::from_str::<serde_json::Value>(&body) {
        Ok(serde_json::Value::Object(message)) => message,
        _ => {
            return json_response(
                StatusCode::OK,
                serde_json::json!({
                    "jsonrpc": "2.0", "id": null,
                    "error": {"code": -32602, "message": "body is not a JSON object"}
                }),
            );
        }
    };
    let mut recorder = state.recorder.clone();
    let response = respond_recording(&parsed, &registry, &grant, &mut recorder);
    let status = StatusCode::from_u16(response.status).unwrap_or(StatusCode::OK);
    match response.body {
        None => Response::builder()
            .status(status)
            .body(Body::empty())
            .expect("a fixed response builds"),
        Some(McpBody::Json(value)) => json_response(status, value),
        // Starlette's 500 is text/plain. Sending it as JSON would quote it.
        Some(McpBody::Text(text)) => Response::builder()
            .status(status)
            .header("content-type", "text/plain; charset=utf-8")
            .body(Body::from(text))
            .expect("a fixed response builds"),
    }
}

fn json_response(status: StatusCode, body: serde_json::Value) -> Response {
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("a fixed response builds")
}

/// A recorder that keeps what it was given. Tests use it to see that a call
/// was authorized; production uses the store-backed one.
#[derive(Clone, Default)]
pub struct RecordingSink {
    pub decisions: std::sync::Arc<std::sync::Mutex<Vec<AuthDecisionRecord>>>,
}

impl AuthRecorder for RecordingSink {
    fn record(&mut self, decision: &AuthDecisionRecord) -> Result<(), String> {
        self.decisions
            .lock()
            .expect("the sink lock")
            .push(decision.clone());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn post(body: &str) -> (u16, String, String) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.spawn(async move {
            let listener = tokio::net::TcpListener::from_std(listener).unwrap();
            axum::serve(listener, router(McpState { recorder: () }))
                .await
                .unwrap();
        });
        std::thread::sleep(std::time::Duration::from_millis(100));
        let mut stream = std::net::TcpStream::connect(addr).unwrap();
        use std::io::{Read, Write};
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
        let content_type = head
            .lines()
            .find_map(|line| {
                line.split_once(": ")
                    .filter(|(name, _)| name.eq_ignore_ascii_case("content-type"))
            })
            .map(|(_, value)| value.to_string())
            .unwrap_or_default();
        (status, content_type, response_body.to_string())
    }

    #[test]
    fn a_ping_is_json_and_a_notification_is_empty() {
        let (status, content_type, body) = post(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#);
        assert_eq!(status, 200);
        assert_eq!(content_type, "application/json");
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert!(json.get("result").is_some(), "{body}");

        let (status, _, body) = post(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
        assert_eq!(status, 202);
        assert!(body.is_empty(), "{body}");
    }
}
