//! The front door, tested against a stand-in core (NFR-D11).
//!
//! The stand-in is a real HTTP server rather than a mocked client, because
//! what these tests are about is what crosses the wire: which credential the
//! core is handed, which headers survive the hop, and what a caller is told
//! when the core is not there at all. A mock of our own proxy would agree
//! with us by construction.

use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    extract::State,
    http::{header, HeaderMap, StatusCode as AxumStatus, Uri},
    response::IntoResponse,
    routing::any,
    Json, Router,
};
use mydevenv2_server::{
    app::router,
    auth::{ScopedTokenConfig, TokenCapability},
    Config,
};
use reqwest::StatusCode;
use serde_json::{json, Value};

const TEST_TOKEN: &str = "test-token-1234567890abcdef";
const CORE_TOKEN: &str = "core-token-abcdef1234567890";
const READ_ONLY_TOKEN: &str = "read-only-token-0987654321fedcba";

/// What the stand-in core saw. One request's worth is all these tests need.
#[derive(Debug, Clone, Default)]
struct Seen {
    path: String,
    query: Option<String>,
    authorization: Option<String>,
    method: String,
}

type Log = Arc<Mutex<Vec<Seen>>>;

async fn core_handler(
    State(log): State<Log>,
    method: axum::http::Method,
    uri: Uri,
    headers: HeaderMap,
) -> impl IntoResponse {
    log.lock().unwrap().push(Seen {
        path: uri.path().to_string(),
        query: uri.query().map(str::to_owned),
        authorization: headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned),
        method: method.to_string(),
    });
    match uri.path() {
        "/health/ready" => (
            AxumStatus::OK,
            Json(json!({
                "status": "ready",
                "declared_schema_version": 6,
                "observed_schema_version": 6,
            })),
        )
            .into_response(),
        "/ui/" => (AxumStatus::OK, "<title>Vogt</title>").into_response(),
        _ => (AxumStatus::OK, Json(json!({"seen": uri.path()}))).into_response(),
    }
}

/// A core that answers, and a handle on what it was asked.
async fn stand_in_core() -> (String, Log) {
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    let app = Router::new()
        .route("/{*path}", any(core_handler))
        .with_state(Arc::clone(&log));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    (format!("http://{addr}"), log)
}

fn base_config() -> Config {
    Config {
        bind: "127.0.0.1:0".parse().unwrap(),
        token: TEST_TOKEN.to_string(),
        token_mutating_request_limit_per_minute: 600,
        extra_tokens: vec![ScopedTokenConfig {
            name: "read-only".to_string(),
            token: READ_ONLY_TOKEN.to_string(),
            // Every capability except the one the write path needs, so a
            // refusal here is about `vogt-write` and not about being unknown.
            capabilities: vec![TokenCapability::Sessions],
            mutating_requests_per_minute: 600,
        }],
        scrollback_bytes: 64 * 1024,
        default_shell: "/bin/bash".to_string(),
        default_cwd: std::env::temp_dir(),
        activity_idle_after_ms: 200,
        idle_stall_after_ms: 10 * 60 * 1_000,
        workspace_root: std::env::temp_dir(),
        gui_stream_url: None,
        state_dir: tempfile::tempdir().unwrap().keep(),
        fcm_service_account_json: None,
        vapid_subject: "mailto:test@example.invalid".to_string(),
        allowed_origins: vec![],
        auto_agent_auth: false,
        agent_auth_helper: "/usr/local/bin/mydevenv2-agent-auth".into(),
        session_templates: vec![],
        assistant_api_key: None,
        assistant_base_url: "https://api.theclawbay.com/v1".into(),
        assistant_model: "gpt-5.4-mini".into(),
        assistant_auto_type: false,
        assistant_max_tool_calls: 8,
        assistant_reasoning_effort: None,
        contextkeeper_url: None,
        contextkeeper_token: None,
        vogt_core_url: None,
        vogt_core_token: None,
    }
}

async fn boot(cfg: Config) -> String {
    let (router, _state) = router(cfg).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    format!("http://{addr}")
}

/// A front door with a core behind it, and the core's request log.
async fn front_door() -> (String, Log) {
    let (core_url, log) = stand_in_core().await;
    let mut cfg = base_config();
    cfg.vogt_core_url = Some(core_url);
    cfg.vogt_core_token = Some(CORE_TOKEN.to_string());
    (boot(cfg).await, log)
}

fn bearer(token: &str) -> reqwest::header::HeaderMap {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::AUTHORIZATION,
        format!("Bearer {token}").parse().unwrap(),
    );
    headers
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
}

// -- /api/vogt: the front door's token in, the core's token out -------------

#[tokio::test]
async fn a_vogt_read_reaches_the_core_under_its_own_prefix() {
    let (base, log) = front_door().await;
    let res = client()
        .get(format!("{base}/api/vogt/status"))
        .headers(bearer(TEST_TOKEN))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["seen"], "/api/status");

    let seen = log.lock().unwrap().last().cloned().unwrap();
    assert_eq!(seen.path, "/api/status");
    assert_eq!(seen.method, "GET");
}

#[tokio::test]
async fn the_core_is_handed_the_core_token_not_the_callers() {
    let (base, log) = front_door().await;
    client()
        .get(format!("{base}/api/vogt/status"))
        .headers(bearer(TEST_TOKEN))
        .send()
        .await
        .unwrap();

    let seen = log.lock().unwrap().last().cloned().unwrap();
    assert_eq!(
        seen.authorization.as_deref(),
        Some("Bearer core-token-abcdef1234567890")
    );
    assert!(
        !seen.authorization.unwrap().contains(TEST_TOKEN),
        "the front-door token must not reach the core: it would only be refused, \
         and the point of the swap is that the core sees a real actor (FR-S9)"
    );
}

#[tokio::test]
async fn a_query_string_survives_the_hop() {
    let (base, log) = front_door().await;
    client()
        .get(format!("{base}/api/vogt/backlog?project=vogt&limit=5"))
        .headers(bearer(TEST_TOKEN))
        .send()
        .await
        .unwrap();

    let seen = log.lock().unwrap().last().cloned().unwrap();
    assert_eq!(seen.path, "/api/backlog");
    assert_eq!(seen.query.as_deref(), Some("project=vogt&limit=5"));
}

#[tokio::test]
async fn an_unauthenticated_caller_never_reaches_the_core() {
    let (base, log) = front_door().await;
    let res = client()
        .get(format!("{base}/api/vogt/status"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    assert!(
        log.lock().unwrap().is_empty(),
        "the request was refused, so nothing should have been forwarded"
    );
}

#[tokio::test]
async fn a_write_needs_the_vogt_write_capability() {
    let (base, log) = front_door().await;
    let refused = client()
        .post(format!("{base}/api/vogt/work"))
        .headers(bearer(READ_ONLY_TOKEN))
        .json(&json!({"kind": "bug", "title": "x", "reason": "test"}))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), StatusCode::FORBIDDEN);
    assert!(log.lock().unwrap().is_empty());

    let allowed = client()
        .post(format!("{base}/api/vogt/work"))
        .headers(bearer(TEST_TOKEN))
        .json(&json!({"kind": "bug", "title": "x", "reason": "test"}))
        .send()
        .await
        .unwrap();
    assert_eq!(allowed.status(), StatusCode::OK);
    let seen = log.lock().unwrap().last().cloned().unwrap();
    assert_eq!(seen.method, "POST");
    assert_eq!(seen.path, "/api/work");
}

#[tokio::test]
async fn a_read_needs_no_capability_beyond_a_valid_token() {
    let (base, _log) = front_door().await;
    let res = client()
        .get(format!("{base}/api/vogt/backlog"))
        .headers(bearer(READ_ONLY_TOKEN))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

// -- /mcp: the caller's own credential, untouched ---------------------------

#[tokio::test]
async fn mcp_forwards_the_callers_credential_unchanged() {
    let (base, log) = front_door().await;
    // Not a token this engine knows. That is the point: an MCP client holds a
    // token minted by the core and bound to an actor.
    let res = client()
        .post(format!("{base}/mcp"))
        .headers(bearer("an-agents-own-core-token"))
        .json(&json!({"jsonrpc": "2.0", "method": "tools/list", "id": 1}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let seen = log.lock().unwrap().last().cloned().unwrap();
    assert_eq!(seen.path, "/mcp");
    assert_eq!(
        seen.authorization.as_deref(),
        Some("Bearer an-agents-own-core-token"),
        "rewriting an agent's credential would replace a real actor with a shared one"
    );
}

// -- /ui-legacy: the vanilla GUI keeps serving (FR-U9) ----------------------

#[tokio::test]
async fn the_legacy_gui_is_served_from_the_front_door() {
    let (base, log) = front_door().await;
    let res = client()
        .get(format!("{base}/ui-legacy"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert!(res.text().await.unwrap().contains("<title>Vogt</title>"));

    let seen = log.lock().unwrap().last().cloned().unwrap();
    assert_eq!(
        seen.path, "/ui/",
        "a bare /ui-legacy must map to the core's /ui/, not to a redirect \
         pointing at a path this front door does not serve"
    );
    assert!(
        seen.authorization.is_none(),
        "static assets carry no token — there has to be a page on which to enter one"
    );
}

#[tokio::test]
async fn a_legacy_gui_asset_keeps_its_path() {
    let (base, log) = front_door().await;
    client()
        .get(format!("{base}/ui-legacy/app.js"))
        .send()
        .await
        .unwrap();
    assert_eq!(log.lock().unwrap().last().unwrap().path, "/ui/app.js");
}

// -- an absent core costs Vogt features, never sessions (FR-E9, FR-U21) -----

#[tokio::test]
async fn with_no_core_configured_the_vogt_routes_refuse_with_a_reason() {
    let base = boot(base_config()).await;
    for path in ["/api/vogt/status", "/mcp", "/ui-legacy"] {
        let res = client()
            .get(format!("{base}{path}"))
            .headers(bearer(TEST_TOKEN))
            .send()
            .await
            .unwrap();
        assert_eq!(
            res.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "{path} should refuse"
        );
        let body: Value = res.json().await.unwrap();
        let message = body["error"]["message"].as_str().unwrap_or_default();
        assert!(
            message.contains("VOGT_CORE_URL"),
            "{path} refused without naming why: {message:?}"
        );
    }
}

#[tokio::test]
async fn with_no_core_configured_the_engine_is_still_ready() {
    let base = boot(base_config()).await;
    let res = client().get(format!("{base}/readyz")).send().await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["ok"], true);
    let check = find_check(&body, "vogt_core");
    assert_eq!(check["ok"], true);
    assert_eq!(check["detail"], "not configured");
    assert_eq!(check["fatal"], false);
}

#[tokio::test]
async fn a_reachable_core_is_reported_with_its_schema_state() {
    let (base, _log) = front_door().await;
    let res = client().get(format!("{base}/readyz")).send().await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body: Value = res.json().await.unwrap();
    let check = find_check(&body, "vogt_core");
    assert_eq!(check["ok"], true);
    assert!(
        check["detail"].as_str().unwrap().contains("schema"),
        "the probe reads /health/ready so it can say which schema is applied: {:?}",
        check["detail"]
    );
}

#[tokio::test]
async fn an_unreachable_core_is_reported_without_declaring_this_pod_unready() {
    let mut cfg = base_config();
    // A port nothing is listening on. Reserved-for-testing address space, so
    // this cannot accidentally reach a real service.
    cfg.vogt_core_url = Some("http://127.0.0.1:1".to_string());
    cfg.vogt_core_token = Some(CORE_TOKEN.to_string());
    let base = boot(cfg).await;

    let res = client().get(format!("{base}/readyz")).send().await.unwrap();
    assert_eq!(
        res.status(),
        StatusCode::OK,
        "restarting the engine would not revive the core, and would kill every \
         live session doing it (FR-E9)"
    );
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["ok"], true);
    let check = find_check(&body, "vogt_core");
    assert_eq!(check["ok"], false, "the outage is still reported honestly");
    assert_eq!(check["fatal"], false);

    // And the sessions API — the thing an absent core must not cost — answers.
    let sessions = client()
        .get(format!("{base}/api/sessions"))
        .headers(bearer(TEST_TOKEN))
        .send()
        .await
        .unwrap();
    assert_eq!(sessions.status(), StatusCode::OK);
}

#[tokio::test]
async fn an_unreachable_core_answers_502_and_says_so() {
    let mut cfg = base_config();
    cfg.vogt_core_url = Some("http://127.0.0.1:1".to_string());
    let base = boot(cfg).await;

    let res = client()
        .get(format!("{base}/api/vogt/status"))
        .headers(bearer(TEST_TOKEN))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_GATEWAY);
    let body: Value = res.json().await.unwrap();
    let message = body["error"]["message"].as_str().unwrap_or_default();
    assert!(message.contains("vogt-core"), "{message:?}");
    assert!(
        !message.contains("127.0.0.1:1"),
        "a client is told what went wrong, not where the core lives: {message:?}"
    );
}

fn find_check<'a>(body: &'a Value, name: &str) -> &'a Value {
    body["checks"]
        .as_array()
        .expect("readiness answers with checks")
        .iter()
        .find(|check| check["name"] == name)
        .unwrap_or_else(|| panic!("no {name} check in {body}"))
}
