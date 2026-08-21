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
/// Holds one capability, and not the ones the refusals below are about — so a
/// rejection means "not granted this", never "not a token I know".
const HISTORY_ONLY_TOKEN: &str = "history-only-token-13579086420abc";

/// Two more front-door tokens, each with a core token of its own, so the
/// mapping has two actors to tell apart rather than one to confirm (FR-S9).
const PAIRED_TOKEN: &str = "paired-front-door-token-1234567890";
const PAIRED_CORE_TOKEN: &str = "core-token-for-the-paired-actor";
const OTHER_PAIRED_TOKEN: &str = "other-paired-front-door-token-0987654321";
const OTHER_PAIRED_CORE_TOKEN: &str = "core-token-for-the-other-actor";

/// What the stand-in core saw. One request's worth is all these tests need.
#[derive(Debug, Clone, Default)]
struct Seen {
    path: String,
    query: Option<String>,
    authorization: Option<String>,
    method: String,
    /// What the door said about where clients arrive (FR-A8). Recorded so a
    /// test can tell "the door stated it" from "the caller did".
    public_url: Option<String>,
    api_path: Option<String>,
    /// The correlation id the door stated (#139). Recorded so a test can
    /// assert the core and the door name the same request.
    request_id: Option<String>,
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
        public_url: headers
            .get("x-vogt-public-url")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned),
        api_path: headers
            .get("x-vogt-api-path")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned),
        request_id: headers
            .get("x-request-id")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned),
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
        extra_tokens: vec![
            ScopedTokenConfig {
                name: "history-only".to_string(),
                token: HISTORY_ONLY_TOKEN.to_string(),
                capabilities: vec![TokenCapability::HistoryWrite],
                mutating_requests_per_minute: 600,
                vogt_core_token_file: None,
                vogt_core_token: None,
            },
            ScopedTokenConfig {
                name: "read-only".to_string(),
                token: READ_ONLY_TOKEN.to_string(),
                // Every capability except the one the write path needs, so a
                // refusal here is about `vogt-write` and not about being unknown.
                capabilities: vec![TokenCapability::Sessions],
                mutating_requests_per_minute: 600,
                // No pairing: this one exercises the fallback.
                vogt_core_token_file: None,
                vogt_core_token: None,
            },
            ScopedTokenConfig {
                name: "paired".to_string(),
                token: PAIRED_TOKEN.to_string(),
                capabilities: vec![TokenCapability::VogtWrite],
                mutating_requests_per_minute: 600,
                vogt_core_token_file: None,
                vogt_core_token: Some(PAIRED_CORE_TOKEN.to_string()),
            },
            ScopedTokenConfig {
                name: "other-paired".to_string(),
                token: OTHER_PAIRED_TOKEN.to_string(),
                capabilities: vec![TokenCapability::VogtWrite],
                mutating_requests_per_minute: 600,
                vogt_core_token_file: None,
                vogt_core_token: Some(OTHER_PAIRED_CORE_TOKEN.to_string()),
            },
        ],
        scrollback_bytes: 64 * 1024,
        default_shell: "/bin/bash".to_string(),
        default_cwd: std::env::temp_dir(),
        activity_idle_after_ms: 200,
        idle_stall_after_ms: 10 * 60 * 1_000,
        workspace_root: std::env::temp_dir(),
        gui_stream_url: None,
        gui_stream_verified: false,
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
        assistant_max_tool_calls: 8,
        assistant_allow_claude_proxy: false,
        assistant_reasoning_effort: None,
        assistant_profiles: vec![],
        assistant_default_profile: None,
        assistant_log_retention_days: 30,
        assistant_stt_base_urls: vec![],
        assistant_stt_api_key: None,
        assistant_stt_model: "whisper-1".into(),
        assistant_tts_base_urls: vec![],
        assistant_tts_api_key: None,
        assistant_tts_model: "tts-1".into(),
        assistant_tts_voice: "alloy".into(),
        assistant_speech_attempt_timeout_ms: 30_000,
        public_url: None,
        vogt_core_url: None,
        vogt_import_root: None,
        vogt_engine_state_dir: None,
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

/// What the core was asked *on behalf of a client*.
///
/// The front door also follows the core's event feed in the background
/// (FR-U10), so a bare "the core saw nothing" assertion would be racing a
/// poll that has nothing to do with the request under test.
fn proxied(log: &Log) -> Vec<Seen> {
    log.lock()
        .unwrap()
        .iter()
        .filter(|seen| seen.path != "/api/events")
        .cloned()
        .collect()
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
        proxied(&log).is_empty(),
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
    assert!(proxied(&log).is_empty());

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

// -- each front-door token is its own actor at the core (FR-S9) -------------

/// A front door whose only core token is the one paired with each front-door
/// token: no deployment-wide fallback to fall back to.
async fn front_door_without_a_fallback() -> (String, Log) {
    let (core_url, log) = stand_in_core().await;
    let mut cfg = base_config();
    cfg.vogt_core_url = Some(core_url);
    cfg.vogt_core_token = None;
    (boot(cfg).await, log)
}

/// The whole point of the mapping: the core can tell the two callers apart,
/// because it is handed two different credentials rather than one shared one.
#[tokio::test]
async fn two_front_door_tokens_reach_the_core_as_two_actors() {
    let (base, log) = front_door().await;

    for token in [PAIRED_TOKEN, OTHER_PAIRED_TOKEN] {
        let res = client()
            .get(format!("{base}/api/vogt/status"))
            .headers(bearer(token))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    let seen = log.lock().unwrap().clone();
    let credentials: Vec<Option<String>> = seen
        .iter()
        .rev()
        .take(2)
        .rev()
        .map(|s| s.authorization.clone())
        .collect();
    assert_eq!(
        credentials,
        vec![
            Some(format!("Bearer {PAIRED_CORE_TOKEN}")),
            Some(format!("Bearer {OTHER_PAIRED_CORE_TOKEN}")),
        ],
        "each front-door token must reach the core as the actor it is paired \
         with — one shared credential is what FR-S9 replaces"
    );
    for credential in credentials.into_iter().flatten() {
        assert!(
            !credential.contains(PAIRED_TOKEN) && !credential.contains(OTHER_PAIRED_TOKEN),
            "a front-door token still must not reach the core: {credential}"
        );
    }
}

/// The M9 deployment — one configured core token, no pairings — keeps working.
#[tokio::test]
async fn a_token_with_no_pairing_of_its_own_uses_the_configured_fallback() {
    let (base, log) = front_door().await;
    let res = client()
        .get(format!("{base}/api/vogt/backlog"))
        .headers(bearer(READ_ONLY_TOKEN))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let seen = log.lock().unwrap().last().cloned().unwrap();
    assert_eq!(
        seen.authorization.as_deref(),
        Some(format!("Bearer {CORE_TOKEN}").as_str()),
        "an unpaired token falls back to the single configured core token, so a \
         deployment that has not provisioned pairings is unaffected"
    );
}

/// A pairing is enough on its own: no deployment-wide token is required for a
/// front door whose every token brings its own actor.
#[tokio::test]
async fn a_pairing_needs_no_fallback_beside_it() {
    let (base, log) = front_door_without_a_fallback().await;
    let res = client()
        .get(format!("{base}/api/vogt/status"))
        .headers(bearer(PAIRED_TOKEN))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        log.lock().unwrap().last().unwrap().authorization.as_deref(),
        Some(format!("Bearer {PAIRED_CORE_TOKEN}").as_str())
    );
}

/// Nothing to inject means nothing is forwarded. Sending the request on
/// without a credential would return the core's 401 and read as the caller's
/// mistake; the refusal names the pairing that was never provisioned.
#[tokio::test]
async fn no_pairing_and_no_fallback_is_refused_by_name() {
    let (base, log) = front_door_without_a_fallback().await;
    let res = client()
        .get(format!("{base}/api/vogt/status"))
        .headers(bearer(READ_ONLY_TOKEN))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);

    let body: Value = res.json().await.unwrap();
    let message = body["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("read-only"),
        "the refusal names the front-door token that has no pairing: {message:?}"
    );
    assert!(
        message.contains("vogt_core_token"),
        "and names the setting that would fix it: {message:?}"
    );
    assert!(
        !message.contains(READ_ONLY_TOKEN),
        "the token's *name* is not its value: {message:?}"
    );
    assert!(
        log.lock().unwrap().is_empty(),
        "an unauthenticated request must never be forwarded to the core"
    );
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

/// The actor mapping is a property of `/api/vogt` alone. Even when the caller
/// presents a front-door token that *has* a pairing, `/mcp` forwards what it
/// was given: swapping in the paired token here would replace whichever actor
/// the MCP client actually is with the browser's.
#[tokio::test]
async fn mcp_ignores_the_pairing_even_when_the_caller_has_one() {
    let (base, log) = front_door().await;
    client()
        .post(format!("{base}/mcp"))
        .headers(bearer(PAIRED_TOKEN))
        .json(&json!({"jsonrpc": "2.0", "method": "tools/list", "id": 1}))
        .send()
        .await
        .unwrap();

    let seen = log.lock().unwrap().last().cloned().unwrap();
    assert_eq!(
        seen.authorization.as_deref(),
        Some(format!("Bearer {PAIRED_TOKEN}").as_str()),
        "/mcp is a pass-through; the core decides what that credential is worth"
    );
}

/// And `/mcp` does not depend on there being any core token configured at all
/// — it never needed one, and the refusal added for a missing pairing belongs
/// to `/api/vogt`.
#[tokio::test]
async fn mcp_works_with_no_core_token_configured_at_all() {
    let (base, log) = front_door_without_a_fallback().await;
    let res = client()
        .post(format!("{base}/mcp"))
        .headers(bearer("an-agents-own-core-token"))
        .json(&json!({"jsonrpc": "2.0", "method": "tools/list", "id": 1}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        log.lock().unwrap().last().unwrap().authorization.as_deref(),
        Some("Bearer an-agents-own-core-token")
    );
}

// -- /ui-legacy: the vanilla GUI keeps serving (FR-U9) ----------------------

#[tokio::test]
async fn the_legacy_gui_is_served_from_the_front_door() {
    let (base, log) = front_door().await;
    let res = client()
        .get(format!("{base}/ui-legacy/"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert!(res.text().await.unwrap().contains("<title>Vogt</title>"));

    let seen = log.lock().unwrap().last().cloned().unwrap();
    assert_eq!(seen.path, "/ui/");
    assert!(
        seen.authorization.is_none(),
        "static assets carry no token — there has to be a page on which to enter one"
    );
}

#[tokio::test]
async fn the_legacy_gui_redirects_to_its_directory() {
    let (base, _log) = front_door().await;
    let res = client()
        .get(format!("{base}/ui-legacy"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::PERMANENT_REDIRECT);
    assert_eq!(
        res.headers()[reqwest::header::LOCATION],
        "/ui-legacy/",
        "index.html links its assets relatively, so the page has to be served \
         from a path a browser will resolve them against"
    );
}

/// A wildcard segment matches at least one character, so every prefix needs
/// its bare form, its trailing-slash form *and* its wildcard form. Missing
/// the middle one sent `/ui-legacy/` to the PWA's catch-all, which answered
/// 404 with the engine's own "web bundle not present" placeholder — a
/// convincing wrong answer, and the reason this test enumerates the shapes.
#[tokio::test]
async fn a_trailing_slash_still_reaches_the_core() {
    let (base, log) = front_door().await;
    for (front, upstream) in [
        ("/api/vogt/", "/api/"),
        ("/mcp/", "/mcp/"),
        ("/ui-legacy/", "/ui/"),
    ] {
        let res = client()
            .get(format!("{base}{front}"))
            .headers(bearer(TEST_TOKEN))
            .send()
            .await
            .unwrap();
        assert_eq!(
            res.status(),
            StatusCode::OK,
            "{front} did not reach the core"
        );
        assert_eq!(
            log.lock().unwrap().last().unwrap().path,
            upstream,
            "{front} reached the wrong upstream path"
        );
    }
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
    // A credential to inject, so this test is about the hop failing and not
    // about the front door having nothing to present.
    cfg.vogt_core_token = Some(CORE_TOKEN.to_string());
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

// -- what a client can learn before it asks (FR-U21) -----------------------

#[tokio::test]
async fn the_public_config_says_whether_a_core_is_configured() {
    let (base, _log) = front_door().await;
    let res = client()
        .get(format!("{base}/api/config"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["vogt"]["configured"], true);
    assert_eq!(body["vogt"]["api_prefix"], "/api/vogt");
    assert_eq!(body["vogt"]["legacy_gui_prefix"], "/ui-legacy");
    assert!(
        !body.to_string().contains(CORE_TOKEN),
        "this endpoint is unauthenticated; presence only, never a credential"
    );
}

#[tokio::test]
async fn the_public_config_says_when_there_is_no_core() {
    let base = boot(base_config()).await;
    let body: Value = client()
        .get(format!("{base}/api/config"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        body["vogt"]["configured"], false,
        "a client that must provoke a 503 to discover this cannot render an \
         honest absent state; it renders a tab that appears and then errors"
    );
}

// -- the core's changes arrive on this server's stream (FR-U10) ------------

/// A stand-in core with an event feed the test drives.
async fn stand_in_core_with_events(
    events: Arc<Mutex<Vec<Value>>>,
) -> (String, Arc<Mutex<Vec<i64>>>) {
    let cursors: Arc<Mutex<Vec<i64>>> = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&cursors);
    let feed = Arc::clone(&events);
    let app = Router::new().route(
        "/{*path}",
        any(move |uri: Uri| {
            let feed = Arc::clone(&feed);
            let seen = Arc::clone(&seen);
            async move {
                let query = uri.query().unwrap_or_default().to_string();
                let after: i64 = query
                    .split('&')
                    .find_map(|pair| pair.strip_prefix("after="))
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(0);
                let wants_none = query.contains("limit=1");
                let rows = feed.lock().unwrap().clone();
                let head = rows
                    .iter()
                    .filter_map(|row| row.get("seq").and_then(|v| v.as_i64()))
                    .max()
                    .unwrap_or(0);
                let visible: Vec<Value> = if wants_none {
                    Vec::new()
                } else {
                    rows.into_iter()
                        .filter(|row| row.get("seq").and_then(|v| v.as_i64()).unwrap_or(0) > after)
                        .collect()
                };
                // Recorded *after* the answer is computed. Recording first
                // opens a window in which a test that waits for this signal
                // can change the feed before the handler has read it — which
                // is exactly the race this comment is paying for.
                seen.lock().unwrap().push(after);
                Json(json!({ "events": visible, "next_cursor": head }))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    (format!("http://{addr}"), cursors)
}

fn core_event(seq: i64, kind: &str) -> Value {
    json!({
        "seq": seq,
        "kind": kind,
        "entity_kind": "work_item",
        "entity_id": "wrk_01J8",
    })
}

#[tokio::test]
async fn the_core_s_changes_are_republished_on_this_server_s_stream() {
    let feed = Arc::new(Mutex::new(vec![core_event(1, "work.created")]));
    let (core_url, cursors) = stand_in_core_with_events(Arc::clone(&feed)).await;

    let mut cfg = base_config();
    cfg.vogt_core_url = Some(core_url);
    cfg.vogt_core_token = Some(CORE_TOKEN.to_string());
    let (router, state) = mydevenv2_server::app::router(cfg).await;
    drop(router);

    let mut stream = state.bus.subscribe();

    // Wait until the follower has taken its starting cursor before making
    // anything happen. Without this the test races the boot read and ends up
    // asserting that history *is* replayed — the opposite of the design.
    for _ in 0..200 {
        if !cursors.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        !cursors.lock().unwrap().is_empty(),
        "the follower must actually have asked the core"
    );

    // Now something happens in the core, after the follower started.
    feed.lock()
        .unwrap()
        .push(core_event(2, "work.transitioned"));

    let event = tokio::time::timeout(Duration::from_secs(20), stream.recv())
        .await
        .expect("the follower should publish within a poll interval")
        .expect("the bus should deliver");

    let json = serde_json::to_value(&event).unwrap();
    assert_eq!(json["type"], "vogt-changed");
    assert_eq!(json["kind"], "work.transitioned");
    assert_eq!(json["seq"], 2);
}

#[tokio::test]
async fn the_follower_does_not_replay_history_at_boot() {
    // An estate with a past. Replaying it into a live UI would be
    // indistinguishable from everything changing at once.
    let feed = Arc::new(Mutex::new(vec![
        core_event(1, "work.created"),
        core_event(2, "work.transitioned"),
        core_event(3, "drift.opened"),
    ]));
    let (core_url, _cursors) = stand_in_core_with_events(Arc::clone(&feed)).await;

    let mut cfg = base_config();
    cfg.vogt_core_url = Some(core_url);
    cfg.vogt_core_token = Some(CORE_TOKEN.to_string());
    let (router, state) = mydevenv2_server::app::router(cfg).await;
    drop(router);

    let mut stream = state.bus.subscribe();
    let nothing = tokio::time::timeout(Duration::from_secs(8), stream.recv()).await;
    assert!(
        nothing.is_err(),
        "three events existed before the follower started; none is news"
    );
}

#[tokio::test]
async fn no_core_token_means_no_follower_and_no_401_every_five_seconds() {
    let feed = Arc::new(Mutex::new(vec![core_event(1, "work.created")]));
    let (core_url, cursors) = stand_in_core_with_events(feed).await;

    let mut cfg = base_config();
    cfg.vogt_core_url = Some(core_url);
    cfg.vogt_core_token = None;
    for token in &mut cfg.extra_tokens {
        token.vogt_core_token = None;
        token.vogt_core_token_file = None;
    }
    let (router, _state) = mydevenv2_server::app::router(cfg).await;
    drop(router);

    tokio::time::sleep(Duration::from_secs(7)).await;
    assert!(
        cursors.lock().unwrap().is_empty(),
        "a feed that cannot be read must not be asked once a second forever"
    );
}

// -- the two halves agree about where the estate is (FR-E3) ----------------

#[tokio::test]
async fn readiness_reports_a_workspace_the_core_imports_outside_of() {
    let workspace = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();

    let mut cfg = base_config();
    cfg.workspace_root = workspace.path().to_path_buf();
    cfg.default_cwd = workspace.path().to_path_buf();
    // Through config, not `std::env`: the value is read once at load, and two
    // tests mutating one process's environment in parallel is a race that
    // fails whichever of them the scheduler picks second.
    cfg.vogt_import_root = Some(elsewhere.path().to_path_buf());
    let base = boot(cfg).await;

    let body: Value = client()
        .get(format!("{base}/readyz"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let check = find_check(&body, "workspace_agreement");
    assert_eq!(check["ok"], false);
    assert!(
        check["detail"].as_str().unwrap().contains("invisible"),
        "a disagreement has to say what it costs, not just that it exists: {:?}",
        check["detail"]
    );
    assert_eq!(
        check["fatal"], false,
        "some projects being invisible is a bad answer, not a dead server — \
         failing readiness here would take the terminals down with it"
    );
    assert_eq!(
        body["ok"], true,
        "and the container is still ready, for the same reason"
    );
}

#[tokio::test]
async fn readiness_says_nothing_when_there_is_nothing_to_compare() {
    let base = boot(base_config()).await;
    let body: Value = client()
        .get(format!("{base}/readyz"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let check = find_check(&body, "workspace_agreement");
    assert_eq!(check["ok"], true);
    assert!(check["detail"]
        .as_str()
        .unwrap()
        .contains("nothing to compare"));
}

#[tokio::test]
async fn a_sibling_directory_is_not_inside_the_workspace() {
    // `/srv/work` and `/srv/workspace` are two unrelated trees that a textual
    // prefix says are nested. The first version of this check compared
    // strings, and both of its tests used unrelated temporary directories, so
    // neither noticed.
    let parent = tempfile::tempdir().unwrap();
    let workspace = parent.path().join("work");
    let sibling = parent.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(&sibling).unwrap();

    let mut cfg = base_config();
    cfg.workspace_root = workspace.clone();
    cfg.default_cwd = workspace;
    cfg.vogt_import_root = Some(sibling);
    let base = boot(cfg).await;

    let body: Value = client()
        .get(format!("{base}/readyz"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        find_check(&body, "workspace_agreement")["ok"],
        false,
        "a name that starts the same is not a directory that contains it"
    );
}

// ── The backup covers the engine, or says it does not (NFR-I6) ────────────

#[tokio::test]
async fn readiness_reports_a_backup_that_would_miss_the_engines_state() {
    let state = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();

    let mut cfg = base_config();
    cfg.state_dir = state.path().to_path_buf();
    // Through config rather than `std::env`, for the reason the workspace
    // pair above gives: one process, two tests, one environment.
    cfg.vogt_engine_state_dir = Some(elsewhere.path().to_path_buf());
    let base = boot(cfg).await;

    let body: Value = client()
        .get(format!("{base}/readyz"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let check = find_check(&body, "backup_agreement");
    assert_eq!(check["ok"], false);
    let detail = check["detail"].as_str().unwrap();
    assert!(
        detail.contains("session history"),
        "a disagreement has to name what the backup would silently omit: \
         {detail:?}"
    );
    assert_eq!(
        check["fatal"], false,
        "a wrong backup path is a bad answer to a question nobody is asking \
         yet; killing the pod over it would take the terminals with it"
    );
    assert_eq!(body["ok"], true);
}

#[tokio::test]
async fn readiness_confirms_the_backup_covers_this_servers_state() {
    let state = tempfile::tempdir().unwrap();

    let mut cfg = base_config();
    cfg.state_dir = state.path().to_path_buf();
    cfg.vogt_engine_state_dir = Some(state.path().to_path_buf());
    let base = boot(cfg).await;

    let body: Value = client()
        .get(format!("{base}/readyz"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let check = find_check(&body, "backup_agreement");
    assert_eq!(check["ok"], true);
    assert!(check["detail"].as_str().unwrap().contains("covers"));
}

#[tokio::test]
async fn a_single_half_deployment_is_not_called_misconfigured() {
    // No core beside this engine, so nothing names its state directory. The
    // check has to stay quiet: every engine that ever ran alone would
    // otherwise report a disagreement with a configuration that does not
    // exist.
    let base = boot(base_config()).await;
    let body: Value = client()
        .get(format!("{base}/readyz"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let check = find_check(&body, "backup_agreement");
    assert_eq!(check["ok"], true);
    assert!(
        check["detail"]
            .as_str()
            .unwrap()
            .contains("covered no engine state"),
        "and it should say what a backup taken here would claim"
    );
}

// ── NFR-D11: one port, two protocols, and the front end on it ─────────────

#[tokio::test]
async fn the_port_that_serves_mcp_also_answers_plain_http_health() {
    // The requirement is a deployment claim: one published port, so a
    // healthcheck and an MCP client reach the same place. `personal-vogt`'s
    // probe speaks plain HTTP for exactly this reason — nothing outside a
    // real MCP client sends `initialize`, and a probe that did would pin a
    // protocol version to a container's liveness.
    let (base, _log) = front_door().await;

    let mcp = client()
        .post(format!("{base}/mcp"))
        .headers(bearer("an-agents-own-core-token"))
        .json(&json!({"jsonrpc": "2.0", "method": "tools/list", "id": 1}))
        .send()
        .await
        .unwrap();
    assert_eq!(mcp.status(), StatusCode::OK);

    // Same origin, no credential, no JSON-RPC.
    let health = client()
        .get(format!("{base}/healthz"))
        .send()
        .await
        .unwrap();
    assert_eq!(health.status(), StatusCode::OK);
    let body: Value = health.json().await.unwrap();
    assert_eq!(body["ok"], true);
}

#[tokio::test]
async fn the_engine_serves_the_front_end_from_that_same_port() {
    // NFR-D11's first clause. `assets.rs` embeds `web/dist/` with rust-embed,
    // so the bundle is compiled in rather than mounted — this asserts the
    // route exists and answers HTML, which is what makes "one published
    // port" true of the *product* and not only of its API.
    //
    // The test suite builds against a placeholder `web/dist/`, so what is
    // asserted is the serving, not the contents: a real bundle differs from
    // this one in every byte except the shape.
    let (base, _log) = front_door().await;
    let res = client().get(format!("{base}/")).send().await.unwrap();
    assert_eq!(
        res.status(),
        StatusCode::OK,
        "the engine serves the PWA at its root; a 404 here means the front \
         door has an API and no front end"
    );
    let content_type = res
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert!(
        content_type.starts_with("text/html"),
        "the root is a document, not JSON: {content_type}"
    );
}

// -- the front door's identity (r10: FR-A7, FR-A8, FR-A9, MERGE §5.3) ------

const DOOR_URL: &str = "https://vogt-dev.example.com";

async fn front_door_with_a_public_url() -> (String, Log) {
    let (core_url, log) = stand_in_core().await;
    let mut cfg = base_config();
    cfg.vogt_core_url = Some(core_url);
    cfg.vogt_core_token = Some(CORE_TOKEN.to_string());
    cfg.public_url = Some(DOOR_URL.to_string());
    (boot(cfg).await, log)
}

/// #24: every probe was answered by the PWA catch-all, at 200, with HTML.
///
/// The status is the part that mattered. A 404 would have been a correct
/// answer to a path that is not served; a 200 carrying `index.html` cannot be
/// told from success, so `vogt-mcp-remote` parsed a web page as JSON and died
/// at launch while `/mcp` on the same host answered perfectly.
#[tokio::test]
async fn every_probe_reaches_the_core_rather_than_the_pwa() {
    let (base, log) = front_door_with_a_public_url().await;
    let client = reqwest::Client::new();

    for path in [
        "/version",
        "/connection-info",
        "/health/ready",
        "/health/live",
    ] {
        let response = client.get(format!("{base}{path}")).send().await.unwrap();
        assert_eq!(response.status(), 200, "{path}");
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        assert!(
            content_type.contains("json"),
            "{path} answered {content_type}, which is the PWA, not the core"
        );
    }

    let seen: Vec<String> = proxied(&log).iter().map(|s| s.path.clone()).collect();
    for path in [
        "/version",
        "/connection-info",
        "/health/ready",
        "/health/live",
    ] {
        assert!(
            seen.contains(&path.to_string()),
            "{path} never reached the core"
        );
    }
}

/// A probe carries no credential, and must not acquire one on the way.
#[tokio::test]
async fn a_probe_needs_no_token_and_is_not_given_one() {
    let (base, log) = front_door_with_a_public_url().await;
    let response = reqwest::Client::new()
        .get(format!("{base}/version"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);

    let probe = proxied(&log)
        .into_iter()
        .find(|s| s.path == "/version")
        .expect("the probe reached the core");
    assert_eq!(
        probe.authorization, None,
        "a probe was handed a credential it never asked for"
    );
}

/// #34: the same failure as #24, one namespace over.
///
/// The probes above were fixed by registering them. That is per-path, and
/// `/api` is not a subtree anyone registered — it is a list of routes, so
/// every path *not* on the list was still claimed by the SPA catch-all and
/// answered `200 text/html`. `/api/openapi.json` reads as "the spec is served
/// here and my parser is broken", which is a worse answer than any error.
///
/// Asserted as a property over `MACHINE_NAMESPACES` and a set of shapes,
/// rather than over the two paths in the report: the guarantee is "nothing
/// under a machine namespace is ever HTML", and it was previously expressed
/// only as a comment about route ordering — one registration away from
/// silently regressing.
#[tokio::test]
async fn nothing_under_a_machine_namespace_is_ever_answered_by_the_pwa() {
    let (base, _log) = front_door().await;

    // Shapes a client actually produces: the bare namespace, its trailing
    // slash, a spec probe, a typo, a deep path, and something that looks like
    // a document — the last because a `.html` suffix is exactly what would
    // tempt a static handler to answer.
    let shapes = [
        "",
        "/",
        "/openapi.json",
        "/nonexistent-xyz",
        "/deeply/nested/unknown/path",
        "/index.html",
    ];

    for namespace in mydevenv2_server::app::MACHINE_NAMESPACES {
        for shape in shapes {
            let path = format!("{namespace}{shape}");
            // Both credentials states: an anonymous prober and a client that
            // holds a real token must each be told something machine-readable.
            for headers in [reqwest::header::HeaderMap::new(), bearer(TEST_TOKEN)] {
                let response = client()
                    .get(format!("{base}{path}"))
                    .headers(headers)
                    .send()
                    .await
                    .unwrap();
                let content_type = response
                    .headers()
                    .get(reqwest::header::CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or_default()
                    .to_string();
                assert!(
                    !content_type.starts_with("text/html"),
                    "{path} answered {} {content_type} — that is the PWA \
                     claiming a machine namespace",
                    response.status()
                );
            }
        }
    }
}

/// The other half of the property: what the catch-all is *for* still works.
///
/// A fix that owned `/api` by taking the whole tree from the front end would
/// pass the test above and break every deep link, so both halves are asserted
/// together — and the shape of the refusal is the engine's ordinary error
/// body, not a new one only this route speaks.
#[tokio::test]
async fn an_unknown_api_path_is_a_json_404_and_a_deep_link_is_still_the_pwa() {
    let (base, _log) = front_door().await;

    let unknown = client()
        .get(format!("{base}/api/openapi.json"))
        .send()
        .await
        .unwrap();
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
    let body: Value = unknown.json().await.unwrap();
    assert_eq!(
        body["error"], "not found",
        "the engine's own error shape, so a client that parses one parses all"
    );

    // A registered route still refuses on its own terms: 401 means "exists,
    // bring a token", which a 404 for everything would have flattened away.
    let registered = client()
        .get(format!("{base}/api/status"))
        .send()
        .await
        .unwrap();
    assert_eq!(registered.status(), StatusCode::UNAUTHORIZED);

    // And the routed subtree keeps owning itself.
    let vogt = client()
        .get(format!("{base}/api/vogt/nonexistent"))
        .send()
        .await
        .unwrap();
    assert_eq!(vogt.status(), StatusCode::UNAUTHORIZED);

    // The SPA deep link — the thing the catch-all exists for. Asserted as
    // "the asset handler answered", because whether that is `index.html` or
    // its own bundle-missing notice depends on the `web/dist/` the suite was
    // built against, and the routing is what this test is about.
    let deep_link = client()
        .get(format!("{base}/totally-bogus"))
        .send()
        .await
        .unwrap();
    let status = deep_link.status();
    let content_type = deep_link
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let body = deep_link.text().await.unwrap();
    if content_type.starts_with("text/html") {
        assert_eq!(
            status,
            StatusCode::OK,
            "a front-end deep link is served, not refused"
        );
    } else {
        assert!(
            body.contains("web bundle not present"),
            "an unknown non-API path must reach the asset handler; it \
             answered {status} {content_type}: {body}"
        );
    }
}

/// #26: the core cannot know this door's address, so the door states it.
#[tokio::test]
async fn the_door_states_where_clients_arrive() {
    let (base, log) = front_door_with_a_public_url().await;
    let response = reqwest::Client::new()
        .get(format!("{base}/api/vogt/connect"))
        .bearer_auth(TEST_TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);

    let seen = proxied(&log)
        .into_iter()
        .find(|s| s.path == "/api/connect")
        .expect("the request reached the core");
    assert_eq!(seen.public_url.as_deref(), Some(DOOR_URL));
    assert_eq!(
        seen.api_path.as_deref(),
        Some("/api/vogt"),
        "the door must state its own mount point, not the core's"
    );
}

/// The gate on this door's side: a caller must never state it.
///
/// `connect` renders a configuration meant to be pasted beside a token, so a
/// caller who could choose the address in it would have a phishing primitive.
/// The core also refuses unless configured as `fronted`, but the door must
/// not forward the claim in the first place — two gates, because this one is
/// reachable by anyone who can reach the door.
#[tokio::test]
async fn a_caller_cannot_state_where_clients_arrive() {
    let (base, log) = front_door_with_a_public_url().await;
    let response = reqwest::Client::new()
        .get(format!("{base}/api/vogt/connect"))
        .bearer_auth(TEST_TOKEN)
        .header("x-vogt-public-url", "https://attacker.example")
        .header("x-vogt-api-path", "/api")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);

    let seen = proxied(&log)
        .into_iter()
        .find(|s| s.path == "/api/connect")
        .expect("the request reached the core");
    assert_eq!(
        seen.public_url.as_deref(),
        Some(DOOR_URL),
        "a caller's claimed address reached the core"
    );
    assert_eq!(seen.api_path.as_deref(), Some("/api/vogt"));
}

/// An exposure value carries no default (NFR-D2), so an unconfigured door
/// states nothing and the core keeps answering for itself.
#[tokio::test]
async fn a_door_with_no_public_url_states_nothing() {
    let (base, log) = front_door().await;
    let response = reqwest::Client::new()
        .get(format!("{base}/api/vogt/connect"))
        .bearer_auth(TEST_TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);

    let seen = proxied(&log)
        .into_iter()
        .find(|s| s.path == "/api/connect")
        .expect("the request reached the core");
    assert_eq!(seen.public_url, None);
    assert_eq!(seen.api_path, None);
}

// -- one request, two runtimes, one identifier (#139, NFR-OB3) -------------

#[tokio::test]
async fn the_core_is_told_which_request_this_is_and_the_caller_is_told_too() {
    let (base, log) = front_door().await;
    let res = client()
        .get(format!("{base}/api/vogt/status"))
        .headers(bearer(TEST_TOKEN))
        .send()
        .await
        .unwrap();

    let answered = res
        .headers()
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
        .expect("every answer carries the id of the request it answers");
    let seen = proxied(&log).last().cloned().unwrap();
    assert_eq!(
        seen.request_id.as_deref(),
        Some(answered.as_str()),
        "the door's log line and the core's must name the same request, or a \
         slow request cannot be followed across the two"
    );
}

#[tokio::test]
async fn a_callers_own_request_id_is_carried_through_to_the_core() {
    let (base, log) = front_door().await;
    let mut headers = bearer(TEST_TOKEN);
    headers.insert("x-request-id", "caller-chosen-id-42".parse().unwrap());
    let res = client()
        .get(format!("{base}/api/vogt/status"))
        .headers(headers)
        .send()
        .await
        .unwrap();

    assert_eq!(
        res.headers().get("x-request-id").unwrap(),
        "caller-chosen-id-42"
    );
    let seen = proxied(&log).last().cloned().unwrap();
    assert_eq!(seen.request_id.as_deref(), Some("caller-chosen-id-42"));
}

#[tokio::test]
async fn an_unusable_request_id_is_replaced_before_it_reaches_a_log() {
    let (base, log) = front_door().await;
    let mut headers = bearer(TEST_TOKEN);
    // A value shaped to forge a second log line if it were ever written out
    // unchecked. The request is fine; only the label is unusable.
    headers.insert("x-request-id", "forged status=200".parse().unwrap());
    let res = client()
        .get(format!("{base}/api/vogt/status"))
        .headers(headers)
        .send()
        .await
        .unwrap();

    let answered = res.headers().get("x-request-id").unwrap().to_str().unwrap();
    assert_ne!(answered, "forged status=200");
    let seen = proxied(&log).last().cloned().unwrap();
    assert_eq!(seen.request_id.as_deref(), Some(answered));
}
