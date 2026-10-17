//! A served instance for the HTTP tests that need the registry routes and MCP
//! on one listener. The two routers join one store, the way `serve` joins them,
//! so a token minted over `/api` is the one `/mcp` reads.

use std::net::SocketAddr;
use std::path::PathBuf;

use serde_json::Value;

use super::{app, mcp};
use crate::application::context::{build_context, Built};
use crate::config::VogtConfig;
use crate::core::SequentialIds;

/// A running instance. Dropping it removes the data directory.
pub struct Served {
    pub addr: SocketAddr,
    pub dir: PathBuf,
    _runtime: tokio::runtime::Runtime,
}

impl Drop for Served {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A fresh instance on a loopback port. `no_auth` is the loopback switch; without
/// it every `/api` and `/mcp` call needs a bearer token.
pub fn serve(no_auth: bool) -> Served {
    let dir = std::env::temp_dir().join(format!("vogt-served-{}", crate::core::fresh_id("t")));
    std::fs::create_dir_all(&dir).unwrap();
    crate::application::instance::init(&dir, &mut None, &mut None).unwrap();
    let built = build_context(
        VogtConfig {
            data_dir: dir.clone(),
            ..VogtConfig::default()
        },
        None,
        None,
        Some(SequentialIds::new(None).unwrap()),
        None,
        None,
        None,
        None,
    )
    .unwrap();
    let Built::SystemSequential(ctx) = &built else {
        panic!("a sequential test context builds a sequential router");
    };
    let router =
        app::router_system_sequential(app::AppState::joined(&dir, no_auth, true, &ctx.declared))
            .merge(mcp::router_system_sequential(mcp::McpState::joined(
                &dir,
                no_auth,
                true,
                &ctx.declared,
            )));
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let addr = listener.local_addr().unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.spawn(async move {
        let listener = tokio::net::TcpListener::from_std(listener).unwrap();
        axum::serve(
            listener,
            router.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    std::thread::sleep(std::time::Duration::from_millis(150));
    Served {
        addr,
        dir,
        _runtime: runtime,
    }
}

/// One request on a fresh connection. A body is sent as JSON.
pub fn send(
    addr: SocketAddr,
    method: &str,
    path: &str,
    bearer: Option<&str>,
    body: Option<&Value>,
) -> (u16, String) {
    use std::io::{Read, Write};
    let mut stream = std::net::TcpStream::connect(addr).unwrap();
    let auth = bearer
        .map(|token| format!("Authorization: Bearer {token}\r\n"))
        .unwrap_or_default();
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    let typed = if body.is_some() {
        "Content-Type: application/json\r\n"
    } else {
        ""
    };
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\n{auth}{typed}Content-Length: {}\r\nConnection: close\r\n\r\n{payload}",
        payload.len()
    )
    .unwrap();
    let mut buf = String::new();
    stream.read_to_string(&mut buf).unwrap();
    let (head, response) = buf.split_once("\r\n\r\n").unwrap();
    let status = head.split_whitespace().nth(1).unwrap().parse().unwrap();
    (status, response.to_string())
}

/// The first operator's token, from the install bootstrap. It is an admin token.
pub fn admin_secret(addr: SocketAddr) -> String {
    let (status, body) = send(
        addr,
        "POST",
        "/api/install/bootstrap",
        None,
        Some(&serde_json::json!({"display_name": "Ada Lovelace"})),
    );
    assert_eq!(status, 200, "{body}");
    let json: Value = serde_json::from_str(&body).unwrap();
    json["secret"].as_str().unwrap().to_string()
}

/// An agent actor holding one token with `scopes`, minted over `/api` by the
/// admin. Returns the token's secret.
pub fn mint_token(addr: SocketAddr, admin: &str, identity_ref: &str, scopes: &str) -> String {
    let (status, body) = send(
        addr,
        "POST",
        "/api/actors",
        Some(admin),
        Some(&serde_json::json!({
            "identity_ref": identity_ref,
            "kind": "agent",
            "display_name": identity_ref,
            "reason": "http test",
        })),
    );
    assert_eq!(status, 200, "{body}");
    let (status, body) = send(
        addr,
        "POST",
        "/api/tokens",
        Some(admin),
        Some(&serde_json::json!({
            "actor": identity_ref,
            "name": identity_ref,
            "scopes": scopes,
            "reason": "http test",
        })),
    );
    assert_eq!(status, 200, "{body}");
    let json: Value = serde_json::from_str(&body).unwrap();
    json["secret"].as_str().unwrap().to_string()
}
