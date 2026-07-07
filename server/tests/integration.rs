//! End-to-end integration tests: start the real Axum server on an OS-assigned
//! port, talk to it over HTTP + WebSocket the same way a client would.

use std::{os::unix::fs::PermissionsExt, time::Duration};

use base64::Engine as _;
use futures_util::{SinkExt, StreamExt};
use mydevenv2_contract::SessionDetail;
use mydevenv2_server::{app::router, Config};
use reqwest::StatusCode;
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;

const TEST_TOKEN: &str = "test-token-1234567890abcdef";

fn test_config() -> Config {
    Config {
        bind: "127.0.0.1:0".parse().unwrap(),
        token: TEST_TOKEN.to_string(),
        token_mutating_request_limit_per_minute: 600,
        extra_tokens: vec![],
        scrollback_bytes: 64 * 1024,
        default_shell: "/bin/bash".to_string(),
        default_cwd: std::env::temp_dir(),
        activity_idle_after_ms: 200,
        workspace_root: std::env::temp_dir(),
        gui_stream_url: None,
        state_dir: tempfile::tempdir().unwrap().keep(),
        fcm_service_account_json: None,
        vapid_subject: "mailto:test@example.invalid".to_string(),
        allowed_origins: vec![],
        auto_agent_auth: false,
        agent_auth_helper: "/usr/local/bin/mydevenv2-agent-auth".into(),
        session_templates: vec![],
    }
}

async fn boot() -> (String, tokio::task::JoinHandle<()>) {
    boot_with_config(test_config()).await
}

async fn boot_with_config(cfg: Config) -> (String, tokio::task::JoinHandle<()>) {
    let (router, _state) = router(cfg).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    // Tiny grace period so the listener is definitely accepting.
    tokio::time::sleep(Duration::from_millis(20)).await;
    (format!("http://{addr}"), handle)
}

fn auth() -> reqwest::header::HeaderMap {
    auth_for(TEST_TOKEN)
}

fn auth_for(token: &str) -> reqwest::header::HeaderMap {
    let mut h = reqwest::header::HeaderMap::new();
    h.insert(
        reqwest::header::AUTHORIZATION,
        format!("Bearer {token}").parse().unwrap(),
    );
    h
}

#[tokio::test]
async fn healthz_is_public() {
    let (base, _h) = boot().await;
    let res = reqwest::get(format!("{base}/healthz")).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["ok"], true);
}

#[tokio::test]
async fn readyz_is_public_and_returns_checks() {
    let (base, _h) = boot().await;
    let res = reqwest::get(format!("{base}/readyz")).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["ok"], true);
    let checks = body["checks"].as_array().expect("missing checks");
    assert!(!checks.is_empty(), "expected readiness checks");
    assert!(checks.iter().any(|check| check["name"] == "workspace_root"));
    assert!(checks.iter().any(|check| check["name"] == "state_dir"));
}

#[tokio::test]
async fn readyz_fails_when_workspace_root_disappears() {
    let workspace = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let mut cfg = test_config();
    cfg.default_cwd = workspace.path().to_path_buf();
    cfg.workspace_root = workspace.path().to_path_buf();
    cfg.state_dir = state_dir.path().to_path_buf();

    let (base, _h) = boot_with_config(cfg).await;
    workspace.close().unwrap();

    let res = reqwest::get(format!("{base}/readyz")).await.unwrap();
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["ok"], false);
    let checks = body["checks"].as_array().expect("missing checks");
    let workspace_check = checks
        .iter()
        .find(|check| check["name"] == "workspace_root")
        .expect("missing workspace_root check");
    assert_eq!(workspace_check["ok"], false);
}

#[tokio::test]
async fn config_endpoint_is_public_and_returns_shape() {
    let (base, _h) = boot().await;
    let res = reqwest::get(format!("{base}/api/config")).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body: Value = res.json().await.unwrap();
    assert!(
        body.get("gui_stream_url").is_some(),
        "missing gui_stream_url"
    );
    assert!(body["version"].as_str().is_some(), "missing version");
}

#[tokio::test]
async fn status_endpoint_requires_auth_and_returns_shape() {
    let (base, _h) = boot().await;

    let unauth = reqwest::get(format!("{base}/api/status")).await.unwrap();
    assert_eq!(unauth.status(), StatusCode::UNAUTHORIZED);

    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let body: Value = client
        .get(format!("{base}/api/status"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert!(body["version"].as_str().is_some(), "missing version");
    assert!(
        body["session_count"].as_u64().is_some(),
        "missing session_count"
    );
    assert!(
        body["push_subscription_count"].as_u64().is_some(),
        "missing push_subscription_count"
    );
    assert!(
        body["gui_process_count"].as_u64().is_some(),
        "missing gui_process_count"
    );
    assert!(
        body["auth_broker"]["auto_agent_auth"].is_boolean(),
        "missing auth_broker.auto_agent_auth"
    );
    assert!(
        body["storage"]["workspace_root"].as_str().is_some(),
        "missing storage.workspace_root"
    );
}

#[tokio::test]
async fn push_subscribe_list_unsubscribe() {
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    // VAPID public key is reachable without auth and non-empty.
    let pk: Value = reqwest::get(format!("{base}/api/push/public-key"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(!pk["vapid_public_key"].as_str().unwrap_or("").is_empty());

    // Subscribe a fake FCM token.
    let r: Value = client
        .post(format!("{base}/api/push/subscribe"))
        .json(&json!({
            "kind": "fcm",
            "token": "fake-test-token-12345",
            "label": "test-device",
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = r["id"].as_str().unwrap().to_string();

    let list: Vec<Value> = client
        .get(format!("{base}/api/push/list"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(list.iter().any(|s| s["id"] == id));

    // Re-subscribing the same token is idempotent (same id).
    let r2: Value = client
        .post(format!("{base}/api/push/subscribe"))
        .json(&json!({"kind":"fcm","token":"fake-test-token-12345"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(r2["id"], r["id"]);

    let r: Value = client
        .post(format!("{base}/api/push/unsubscribe"))
        .json(&json!({"id": id}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(r["ok"], true);
}

#[tokio::test]
async fn mutating_requests_are_rate_limited_per_token() {
    let mut cfg = test_config();
    cfg.token_mutating_request_limit_per_minute = 2;

    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    for name in ["one", "two"] {
        let res = client
            .post(format!("{base}/api/sessions"))
            .json(&json!({ "name": name, "command": ["/bin/true"] }))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    let limited = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({ "name": "three", "command": ["/bin/true"] }))
        .send()
        .await
        .unwrap();
    assert_eq!(limited.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(limited.headers().get("retry-after").is_some());
}

#[tokio::test]
async fn agent_task_create_run_and_records_prompt_file() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = test_config();
    cfg.default_cwd = tmp.path().to_path_buf();
    cfg.workspace_root = tmp.path().canonicalize().unwrap();
    cfg.state_dir = tmp.path().join("state");

    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let created: Value = client
        .post(format!("{base}/api/agent-tasks"))
        .json(&json!({
            "name": "PX3 price monitor",
            "prompt": "Check Australian Hisense PX3 prices and notify only on a price drop.",
            "schedule": { "kind": "manual" },
            "command": ["/bin/sh", "-lc", "printf 'task:%s run:%s\\n' \"$MYDEVENV2_AGENT_TASK_ID\" \"$MYDEVENV2_AGENT_TASK_RUN_ID\""],
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let task_id = created["id"].as_str().unwrap().to_string();
    assert_eq!(created["name"], "PX3 price monitor");
    assert_eq!(created["notify_on_phrase"], "MYDEVENV2_NOTIFY:");

    let run: Value = client
        .post(format!("{base}/api/agent-tasks/{task_id}/run"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let prompt_file = run["prompt_file"].as_str().unwrap();
    let session_id = run["session_id"].as_str().unwrap().to_string();
    let prompt_text = std::fs::read_to_string(prompt_file).unwrap();
    assert!(prompt_text.contains("Check Australian Hisense PX3 prices"));
    assert!(prompt_text.contains("MYDEVENV2_NOTIFY:"));

    let detail: Value = client
        .get(format!("{base}/api/agent-tasks/{task_id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(detail["run_count"], 1);
    assert_eq!(detail["runs"][0]["session_id"], session_id);

    let detail_after_exit: Value = loop {
        tokio::time::sleep(Duration::from_millis(40)).await;
        let detail: Value = client
            .get(format!("{base}/api/agent-tasks/{task_id}"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if detail["runs"][0]["status"] == "completed" {
            break detail;
        }
    };
    assert_eq!(detail_after_exit["runs"][0]["status"], "completed");
    assert_eq!(detail_after_exit["runs"][0]["exit_code"], 0);
    assert!(detail_after_exit["runs"][0]["completed_at"]
        .as_str()
        .is_some());
    assert_eq!(
        detail_after_exit["runs"][0]["summary"],
        "Exited successfully"
    );

    let sessions: Vec<Value> = client
        .get(format!("{base}/api/sessions"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(sessions.iter().any(|s| s["id"] == session_id));
}

#[tokio::test]
async fn gui_launch_lists_and_kills() {
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let launched: Value = client
        .post(format!("{base}/api/gui/launch"))
        .json(&json!({ "command": ["sleep", "10"], "via_sway": false }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let pid = launched["pid"].as_u64().expect("pid in launch response");

    let procs: Vec<Value> = client
        .get(format!("{base}/api/gui/processes"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        procs.iter().any(|p| p["pid"].as_u64() == Some(pid)),
        "expected pid {pid} in {procs:?}"
    );

    let k = client
        .post(format!("{base}/api/gui/kill?pid={pid}"))
        .send()
        .await
        .unwrap();
    assert_eq!(k.status(), StatusCode::OK);
}

#[tokio::test]
async fn list_sessions_rejects_missing_auth() {
    let (base, _h) = boot().await;
    let res = reqwest::get(format!("{base}/api/sessions")).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn list_sessions_rejects_wrong_token() {
    let (base, _h) = boot().await;
    let client = reqwest::Client::new();
    let res = client
        .get(format!("{base}/api/sessions"))
        .header("Authorization", "Bearer wrong-token")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn create_list_and_kill_session() {
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let create: Value = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "test-shell",
            "command": ["/bin/cat"],
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = create["id"].as_str().unwrap().to_string();
    assert_eq!(create["name"], "test-shell");

    let list: Vec<Value> = client
        .get(format!("{base}/api/sessions"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(list.iter().any(|s| s["id"] == id));

    let kill = client
        .post(format!("{base}/api/sessions/{id}/kill"))
        .send()
        .await
        .unwrap();
    assert_eq!(kill.status(), StatusCode::OK);

    let del = client
        .delete(format!("{base}/api/sessions/{id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(del.status(), StatusCode::OK);
}

#[tokio::test]
async fn rename_session() {
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let id: String = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({ "name": "before", "command": ["/bin/cat"] }))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let r = client
        .patch(format!("{base}/api/sessions/{id}"))
        .json(&json!({ "name": "after" }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);

    let detail: Value = client
        .get(format!("{base}/api/sessions/{id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(detail["summary"]["name"], "after");

    let _ = client
        .delete(format!("{base}/api/sessions/{id}"))
        .send()
        .await;
}

#[tokio::test]
async fn create_session_trims_name() {
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let create: Value = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({ "name": "  trimmed shell  ", "command": ["/bin/cat"] }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(create["name"], "trimmed shell");

    let id = create["id"].as_str().unwrap();
    let _ = client
        .delete(format!("{base}/api/sessions/{id}"))
        .send()
        .await;
}

#[tokio::test]
async fn rename_session_trims_name() {
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let id: String = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({ "name": "before", "command": ["/bin/cat"] }))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let r = client
        .patch(format!("{base}/api/sessions/{id}"))
        .json(&json!({ "name": "  after trim  " }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);

    let detail: Value = client
        .get(format!("{base}/api/sessions/{id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(detail["summary"]["name"], "after trim");

    let _ = client
        .delete(format!("{base}/api/sessions/{id}"))
        .send()
        .await;
}

#[tokio::test]
async fn session_name_limit_is_enforced() {
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let long = "a".repeat(257);

    let create = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({ "name": long, "command": ["/bin/cat"] }))
        .send()
        .await
        .unwrap();
    assert_eq!(create.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn session_cwd_must_stay_under_workspace_root() {
    let tmp = tempfile::tempdir().unwrap();
    let workspace = tmp.path().join("workspace");
    let nested = workspace.join("nested");
    std::fs::create_dir_all(&nested).unwrap();
    let outside = tmp.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();

    let mut cfg = test_config();
    cfg.default_cwd = workspace.clone();
    cfg.workspace_root = workspace.canonicalize().unwrap();

    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let create: Value = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "cwd-test",
            "command": ["/bin/cat"],
            "cwd": nested.canonicalize().unwrap().to_string_lossy().into_owned()
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        create["cwd"],
        nested.canonicalize().unwrap().to_string_lossy().as_ref()
    );
    let id = create["id"].as_str().unwrap();
    let _ = client
        .delete(format!("{base}/api/sessions/{id}"))
        .send()
        .await;

    let rejected = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "bad-cwd",
            "command": ["/bin/cat"],
            "cwd": outside.canonicalize().unwrap().to_string_lossy().into_owned()
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(rejected.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn session_activity_becomes_idle_after_quiet_window() {
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let create: Value = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "idle-watch",
            "command": ["/bin/sh", "-lc", "printf ready; sleep 1"],
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = create["id"].as_str().unwrap().to_string();

    let detail: Value = loop {
        let detail: Value = client
            .get(format!("{base}/api/sessions/{id}"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if detail["summary"]["activity"] == "idle" {
            break detail;
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    };
    assert_eq!(detail["summary"]["activity"], "idle");

    let _ = client
        .delete(format!("{base}/api/sessions/{id}"))
        .send()
        .await;
}

#[tokio::test]
async fn get_session_returns_typed_detail_shape() {
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let created: Value = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({ "name": "detail-shape" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap();

    let detail: SessionDetail = client
        .get(format!("{base}/api/sessions/{id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(detail.summary.id.to_string(), id);
    assert_eq!(detail.summary.name, "detail-shape");
    assert!(detail.summary.created_at.contains('T'));

    let _ = client
        .delete(format!("{base}/api/sessions/{id}"))
        .send()
        .await;
}

#[tokio::test]
async fn create_session_accepts_scrollback_override() {
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let created: Value = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "scrollback-override",
            "command": ["/bin/sh", "-lc", "printf 'abcdefghijk'"],
            "scrollback_bytes": 8,
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap().to_string();

    let detail: SessionDetail = loop {
        let detail: SessionDetail = client
            .get(format!("{base}/api/sessions/{id}"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if detail.summary.exit_code.is_some() {
            break detail;
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    };

    let snapshot = base64::engine::general_purpose::STANDARD
        .decode(detail.scrollback_base64.as_bytes())
        .unwrap();
    assert_eq!(detail.summary.scrollback_bytes, 11);
    assert_eq!(snapshot, b"defghijk");
}

#[tokio::test]
async fn exited_sessions_are_archived_searchable_and_deletable() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = test_config();
    cfg.default_cwd = tmp.path().to_path_buf();
    cfg.workspace_root = tmp.path().canonicalize().unwrap();

    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let id: String = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "archive-me",
            "command": [
                "/bin/sh",
                "-lc",
                "printf 'history-needle path/with punctuation\\n'; exit 7",
            ],
        }))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let archived = loop {
        let sessions: Vec<Value> = client
            .get(format!("{base}/api/history/sessions?limit=20"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if let Some(session) = sessions.iter().find(|s| s["id"] == id) {
            break session.clone();
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "session was not archived; got {sessions:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };

    assert_eq!(archived["name"], "archive-me");
    assert_eq!(archived["exit_code"], 7);
    assert!(
        archived["scrollback_bytes"].as_i64().unwrap_or_default() > 0,
        "archive should record output bytes: {archived:?}"
    );

    let search: Vec<Value> = client
        .get(format!(
            "{base}/api/history/search?q=history-needle%20path%2Fwith"
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        search.iter().any(|hit| hit["session_id"] == id),
        "history search should find archived output; got {search:?}"
    );

    let del = client
        .delete(format!("{base}/api/history/{id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(del.status(), StatusCode::OK);

    let after_delete = client
        .get(format!("{base}/api/history/{id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(after_delete.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn archived_history_log_preview_and_download_work() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = test_config();
    cfg.default_cwd = tmp.path().to_path_buf();
    cfg.workspace_root = tmp.path().canonicalize().unwrap();

    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let id: String = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "replay-me",
            "command": [
                "/bin/sh",
                "-lc",
                "printf 'first line\\nsecond line\\nthird line\\n'; exit 0",
            ],
        }))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let session = client
            .get(format!("{base}/api/history/{id}"))
            .send()
            .await
            .unwrap();
        if session.status() == StatusCode::OK {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "session was not archived in time"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let preview: Value = client
        .get(format!("{base}/api/history/{id}/log?tail_bytes=12"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(preview["session_id"], id);
    assert_eq!(preview["truncated"], true);
    assert_eq!(preview["bytes"], 12);
    assert!(
        preview["text"]
            .as_str()
            .unwrap_or("")
            .contains("third line"),
        "preview should contain tail output: {preview:?}"
    );

    let download = client
        .get(format!("{base}/api/history/{id}/download"))
        .send()
        .await
        .unwrap();
    assert_eq!(download.status(), StatusCode::OK);
    let content_disposition = download
        .headers()
        .get(reqwest::header::CONTENT_DISPOSITION)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    assert!(
        content_disposition.contains("attachment"),
        "download should set attachment disposition: {content_disposition}"
    );
    let body = download.text().await.unwrap();
    assert!(body.contains("first line"));
    assert!(body.contains("third line"));
}

async fn ws_attach(
    base: &str,
    id: &str,
) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
    ws_attach_with_token(base, id, TEST_TOKEN).await
}

async fn ws_attach_with_token(
    base: &str,
    id: &str,
    token: &str,
) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
    let ws_url = base.replace("http://", "ws://");
    let url = format!("{ws_url}/api/sessions/{id}/attach");
    let (mut ws, _resp) = tokio_tungstenite::connect_async(url).await.unwrap();
    // First-frame auth (the legacy ?token= path still works but is deprecated).
    let auth = serde_json::json!({"type": "auth", "token": token}).to_string();
    ws.send(Message::Text(auth.into())).await.unwrap();
    ws
}

#[tokio::test]
async fn ws_attach_echoes_input_and_replays_on_reattach() {
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    // `cat` echoes whatever we send it on stdin to stdout.
    let id: String = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({ "name": "echo", "command": ["/bin/cat"] }))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let mut ws = ws_attach(&base, &id).await;

    // 1) snapshot-start text frame
    let m = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .expect("snapshot-start arrives")
        .unwrap()
        .unwrap();
    let s = match m {
        Message::Text(s) => s,
        other => panic!("expected text snapshot-start, got {other:?}"),
    };
    let v: Value = serde_json::from_str(&s).unwrap();
    assert_eq!(v["type"], "snapshot-start");

    // 2) snapshot-done (no binary frames in between since nothing has been written yet)
    let m = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let s = match m {
        Message::Text(s) => s,
        other => panic!("expected snapshot-done, got {other:?}"),
    };
    assert_eq!(
        serde_json::from_str::<Value>(&s).unwrap()["type"],
        "snapshot-done"
    );

    // 3) Write input → expect to see it echoed back.
    ws.send(Message::Binary(b"hello-mydevenv\n".to_vec().into()))
        .await
        .unwrap();

    let echoed = collect_binary_until(&mut ws, b"hello-mydevenv", Duration::from_secs(2)).await;
    assert!(
        echoed
            .windows(b"hello-mydevenv".len())
            .any(|w| w == b"hello-mydevenv"),
        "echo not seen; got {:?}",
        String::from_utf8_lossy(&echoed)
    );

    // Close first client.
    ws.close(None).await.ok();
    drop(ws);

    // 4) Reattach — snapshot must contain what we just echoed.
    let mut ws2 = ws_attach(&base, &id).await;
    let _start = ws2.next().await.unwrap().unwrap();
    let mut accumulated = Vec::new();
    loop {
        let m = tokio::time::timeout(Duration::from_secs(2), ws2.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        match m {
            Message::Binary(b) => accumulated.extend_from_slice(&b),
            Message::Text(s) => {
                let v: Value = serde_json::from_str(&s).unwrap();
                if v["type"] == "snapshot-done" {
                    break;
                }
            }
            _ => {}
        }
    }
    assert!(
        accumulated
            .windows(b"hello-mydevenv".len())
            .any(|w| w == b"hello-mydevenv"),
        "scrollback replay missing previous output; got {:?}",
        String::from_utf8_lossy(&accumulated)
    );

    let _ = client
        .delete(format!("{base}/api/sessions/{id}"))
        .send()
        .await;
}

#[tokio::test]
async fn default_session_uses_agent_auth_helper_when_enabled() {
    let tmp = tempfile::tempdir().unwrap();
    let helper = tmp.path().join("agent-auth");
    std::fs::write(
        &helper,
        "#!/bin/sh\n[ \"$1\" = shell ] || exit 64\nprintf 'agent-wrapper-ok\\n'\nexec /bin/cat\n",
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&helper).unwrap().permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&helper, permissions).unwrap();

    let mut cfg = test_config();
    cfg.default_cwd = tmp.path().to_path_buf();
    cfg.workspace_root = tmp.path().canonicalize().unwrap();
    cfg.auto_agent_auth = true;
    cfg.agent_auth_helper = helper;

    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let id = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({ "name": "authenticated-shell" }))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let mut ws = ws_attach(&base, &id).await;
    let output = collect_binary_until(&mut ws, b"agent-wrapper-ok", Duration::from_secs(2)).await;
    assert!(
        output
            .windows(b"agent-wrapper-ok".len())
            .any(|w| w == b"agent-wrapper-ok"),
        "agent auth helper output not seen; got {:?}",
        String::from_utf8_lossy(&output)
    );

    let _ = client
        .delete(format!("{base}/api/sessions/{id}"))
        .send()
        .await;
}

#[tokio::test]
async fn file_api_round_trip() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("hello.txt"), "first").unwrap();
    std::fs::create_dir(tmp.path().join("sub")).unwrap();
    std::fs::write(tmp.path().join("sub/nested.md"), "# nested").unwrap();

    let cfg = Config {
        bind: "127.0.0.1:0".parse().unwrap(),
        token: TEST_TOKEN.to_string(),
        token_mutating_request_limit_per_minute: 600,
        extra_tokens: vec![],
        scrollback_bytes: 64 * 1024,
        default_shell: "/bin/bash".to_string(),
        default_cwd: tmp.path().to_path_buf(),
        activity_idle_after_ms: 200,
        workspace_root: tmp.path().canonicalize().unwrap(),
        gui_stream_url: None,
        state_dir: tempfile::tempdir().unwrap().keep(),
        fcm_service_account_json: None,
        vapid_subject: "mailto:test@example.invalid".to_string(),
        allowed_origins: vec![],
        auto_agent_auth: false,
        agent_auth_helper: "/usr/local/bin/mydevenv2-agent-auth".into(),
        session_templates: vec![],
    };
    let (router, _state) = router(cfg).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    // List root
    let dir: Vec<Value> = client
        .get(format!("{base}/api/dir?path="))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let names: Vec<&str> = dir.iter().filter_map(|e| e["name"].as_str()).collect();
    assert!(names.contains(&"hello.txt"));
    assert!(names.contains(&"sub"));

    // Read existing file
    let r: Value = client
        .get(format!("{base}/api/files?path=hello.txt"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(r["content"], "first");

    // Write a new file
    let w = client
        .put(format!("{base}/api/files"))
        .json(&json!({ "path": "new.txt", "content": "fresh" }))
        .send()
        .await
        .unwrap();
    assert_eq!(w.status(), StatusCode::OK);
    let bytes_on_disk = std::fs::read_to_string(tmp.path().join("new.txt")).unwrap();
    assert_eq!(bytes_on_disk, "fresh");

    // Create a directory via higher-level file ops.
    let mkdir = client
        .post(format!("{base}/api/files/op"))
        .json(&json!({ "op": "mkdir", "path": "ops/deeper", "parents": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(mkdir.status(), StatusCode::OK);
    assert!(tmp.path().join("ops/deeper").is_dir());

    // Upload binary bytes via content_base64 (native client upload path).
    use base64::Engine as _;
    let raw: &[u8] = &[0x00, 0x01, 0xff, 0xfe, b'h', b'i'];
    let b64 = base64::engine::general_purpose::STANDARD.encode(raw);
    let wb = client
        .put(format!("{base}/api/files"))
        .json(&json!({ "path": "up/bin.dat", "content_base64": b64, "create_parents": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(wb.status(), StatusCode::OK);
    let on_disk = std::fs::read(tmp.path().join("up/bin.dat")).unwrap();
    assert_eq!(on_disk, raw);

    // Duplicate both a file and a directory tree.
    let dup_file = client
        .post(format!("{base}/api/files/op"))
        .json(&json!({
            "op": "duplicate",
            "from": "hello.txt",
            "to": "ops/hello-copy.txt",
            "create_parents": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(dup_file.status(), StatusCode::OK);
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("ops/hello-copy.txt")).unwrap(),
        "first"
    );

    let dup_dir = client
        .post(format!("{base}/api/files/op"))
        .json(&json!({
            "op": "duplicate",
            "from": "sub",
            "to": "sub-copy"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(dup_dir.status(), StatusCode::OK);
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("sub-copy/nested.md")).unwrap(),
        "# nested"
    );

    // Move/rename a file.
    let mv = client
        .post(format!("{base}/api/files/op"))
        .json(&json!({
            "op": "move",
            "from": "new.txt",
            "to": "ops/renamed.txt",
            "create_parents": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(mv.status(), StatusCode::OK);
    assert!(!tmp.path().join("new.txt").exists());
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("ops/renamed.txt")).unwrap(),
        "fresh"
    );

    // Delete a copied file and directory tree.
    let del_file = client
        .post(format!("{base}/api/files/op"))
        .json(&json!({ "op": "delete", "path": "ops/hello-copy.txt" }))
        .send()
        .await
        .unwrap();
    assert_eq!(del_file.status(), StatusCode::OK);
    assert!(!tmp.path().join("ops/hello-copy.txt").exists());

    let del_dir = client
        .post(format!("{base}/api/files/op"))
        .json(&json!({ "op": "delete", "path": "sub-copy", "recursive": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(del_dir.status(), StatusCode::OK);
    assert!(!tmp.path().join("sub-copy").exists());

    // A malformed base64 body is a 400, not a 500.
    let bad = client
        .put(format!("{base}/api/files"))
        .json(&json!({ "path": "bad.dat", "content_base64": "!!!notbase64!!!" }))
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), StatusCode::BAD_REQUEST);

    // Path-traversal rejected
    let escape = client
        .get(format!("{base}/api/files?path=../escape"))
        .send()
        .await
        .unwrap();
    assert_eq!(escape.status(), StatusCode::BAD_REQUEST);

    // Tree with depth 1 includes nested.md
    let tree: Vec<Value> = client
        .get(format!("{base}/api/tree?depth=1"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let sub = tree
        .iter()
        .find(|n| n["name"] == "sub")
        .expect("sub node present");
    let kids: Vec<&str> = sub["children"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|c| c["name"].as_str())
        .collect();
    assert!(kids.contains(&"nested.md"));

    // Search (skip if rg isn't installed; just don't fail the suite)
    let s = client
        .get(format!("{base}/api/search?q=nested"))
        .send()
        .await
        .unwrap();
    if s.status() == StatusCode::OK {
        let hits: Vec<Value> = s.json().await.unwrap();
        assert!(
            hits.iter().any(|h| h["path"]
                .as_str()
                .map(|p| p.ends_with("nested.md"))
                .unwrap_or(false)),
            "rg search should find nested; got {hits:?}"
        );
    }
}

#[tokio::test]
async fn file_name_search_returns_matching_paths() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir(tmp.path().join("docs")).unwrap();
    std::fs::write(tmp.path().join("README.md"), "root").unwrap();
    std::fs::write(tmp.path().join("docs").join("readme-notes.txt"), "nested").unwrap();
    std::fs::write(tmp.path().join("docs").join("other.txt"), "other").unwrap();

    let mut cfg = test_config();
    cfg.default_cwd = tmp.path().to_path_buf();
    cfg.workspace_root = tmp.path().canonicalize().unwrap();

    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let hits: Vec<Value> = client
        .get(format!("{base}/api/search/files?q=read"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert!(hits.iter().any(|hit| hit["path"] == "README.md"));
    assert!(hits
        .iter()
        .any(|hit| hit["path"] == "docs/readme-notes.txt"));
}

#[tokio::test]
async fn git_status_log_branch() {
    // Spin up a fresh git repo in a tempdir as the workspace, then drive
    // the git API across status, diff, staging, commit, and branch workflow.
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let sh = |cmd: &str| {
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .current_dir(repo)
            .output()
            .unwrap();
        assert!(out.status.success(), "{cmd}: {:?}", out);
    };
    sh("git init -q -b main");
    sh("git config user.email t@t");
    sh("git config user.name t");
    std::fs::write(repo.join("a.txt"), "one\n").unwrap();
    sh("git add a.txt && git commit -q -m 'init'");
    std::fs::write(repo.join("a.txt"), "one\ntwo\n").unwrap();
    std::fs::write(repo.join("b.txt"), "untracked\n").unwrap();

    let cfg = Config {
        bind: "127.0.0.1:0".parse().unwrap(),
        token: TEST_TOKEN.to_string(),
        token_mutating_request_limit_per_minute: 600,
        extra_tokens: vec![],
        scrollback_bytes: 64 * 1024,
        default_shell: "/bin/bash".to_string(),
        default_cwd: repo.to_path_buf(),
        activity_idle_after_ms: 200,
        workspace_root: repo.canonicalize().unwrap(),
        gui_stream_url: None,
        state_dir: tempfile::tempdir().unwrap().keep(),
        fcm_service_account_json: None,
        vapid_subject: "mailto:test@example.invalid".to_string(),
        allowed_origins: vec![],
        auto_agent_auth: false,
        agent_auth_helper: "/usr/local/bin/mydevenv2-agent-auth".into(),
        session_templates: vec![],
    };
    let (router, _state) = router(cfg).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let st: Value = client
        .get(format!("{base}/api/git/status"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(st["is_repo"], true);
    assert_eq!(st["branch"], "main");
    let entries = st["entries"].as_array().unwrap();
    let paths: Vec<&str> = entries.iter().filter_map(|e| e["path"].as_str()).collect();
    assert!(paths.contains(&"a.txt"));
    assert!(paths.contains(&"b.txt"));

    let log: Vec<Value> = client
        .get(format!("{base}/api/git/log?n=10"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(log.len(), 1);
    assert_eq!(log[0]["subject"], "init");

    let br: Value = client
        .get(format!("{base}/api/git/branch"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(br["current"], "main");

    let diff: Value = client
        .get(format!("{base}/api/git/diff?path=a.txt"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(diff["head"], "one\n");
    assert_eq!(diff["current"], "one\ntwo\n");

    let stage_a: Value = client
        .post(format!("{base}/api/git/op"))
        .json(&json!({
            "op": "stage",
            "path": "a.txt",
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(stage_a["ok"], true);

    let staged_status: Value = client
        .get(format!("{base}/api/git/status"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let staged_a = staged_status["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["path"] == "a.txt")
        .unwrap();
    assert_eq!(staged_a["kind"], "staged");
    assert_eq!(staged_a["index"], "M");
    assert_eq!(staged_a["worktree"], " ");

    let unstage_a: Value = client
        .post(format!("{base}/api/git/op"))
        .json(&json!({
            "op": "unstage",
            "path": "a.txt",
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(unstage_a["ok"], true);

    let discard_a: Value = client
        .post(format!("{base}/api/git/op"))
        .json(&json!({
            "op": "discard",
            "path": "a.txt",
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(discard_a["ok"], true);
    assert_eq!(
        std::fs::read_to_string(repo.join("a.txt")).unwrap(),
        "one\n"
    );

    let stage_b: Value = client
        .post(format!("{base}/api/git/op"))
        .json(&json!({
            "op": "stage",
            "path": "b.txt",
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(stage_b["ok"], true);

    let unstage_b: Value = client
        .post(format!("{base}/api/git/op"))
        .json(&json!({
            "op": "unstage",
            "path": "b.txt",
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(unstage_b["ok"], true);

    let discard_b: Value = client
        .post(format!("{base}/api/git/op"))
        .json(&json!({
            "op": "discard",
            "path": "b.txt",
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(discard_b["ok"], true);
    assert!(!repo.join("b.txt").exists());

    std::fs::write(repo.join("b.txt"), "tracked now\n").unwrap();
    let stage_commit_target: Value = client
        .post(format!("{base}/api/git/op"))
        .json(&json!({
            "op": "stage",
            "path": "b.txt",
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(stage_commit_target["ok"], true);

    let commit: Value = client
        .post(format!("{base}/api/git/op"))
        .json(&json!({
            "op": "commit",
            "message": "add b",
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(commit["ok"], true);
    assert!(commit["commit"].as_str().unwrap().len() >= 7);

    let clean_status: Value = client
        .get(format!("{base}/api/git/status"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(clean_status["entries"].as_array().unwrap().is_empty());

    let updated_log: Vec<Value> = client
        .get(format!("{base}/api/git/log?n=10"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(updated_log.len(), 2);
    assert_eq!(updated_log[0]["subject"], "add b");

    let create_branch: Value = client
        .post(format!("{base}/api/git/op"))
        .json(&json!({
            "op": "checkout",
            "branch": "feature/git-workflow",
            "create": true,
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(create_branch["ok"], true);
    assert_eq!(create_branch["branch"], "feature/git-workflow");

    let branch_after_create: Value = client
        .get(format!("{base}/api/git/branch"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(branch_after_create["current"], "feature/git-workflow");
    let all_branches = branch_after_create["all"].as_array().unwrap();
    assert!(all_branches
        .iter()
        .any(|branch| branch.as_str() == Some("feature/git-workflow")));

    let checkout_main: Value = client
        .post(format!("{base}/api/git/op"))
        .json(&json!({
            "op": "checkout",
            "branch": "main",
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(checkout_main["ok"], true);
    assert_eq!(checkout_main["branch"], "main");

    let branch_after_checkout: Value = client
        .get(format!("{base}/api/git/branch"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(branch_after_checkout["current"], "main");
}

#[tokio::test]
async fn git_endpoints_return_empty_for_non_repo_workspace() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir(tmp.path().join("plain-dir")).unwrap();

    let mut cfg = test_config();
    cfg.default_cwd = tmp.path().to_path_buf();
    cfg.workspace_root = tmp.path().canonicalize().unwrap();

    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let st: Value = client
        .get(format!("{base}/api/git/status"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(st["is_repo"], false);
    assert_eq!(st["branch"], "");
    assert!(st["entries"].as_array().unwrap().is_empty());

    let nested_status: Value = client
        .get(format!("{base}/api/git/status?repo=plain-dir"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(nested_status["repo"], "plain-dir");
    assert_eq!(nested_status["is_repo"], false);
    assert!(nested_status["entries"].as_array().unwrap().is_empty());

    let log: Vec<Value> = client
        .get(format!("{base}/api/git/log?n=10"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(log.is_empty());

    let br: Value = client
        .get(format!("{base}/api/git/branch"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(br["current"], "");
    assert!(br["all"].as_array().unwrap().is_empty());
}

async fn collect_binary_until(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    needle: &[u8],
    timeout: Duration,
) -> Vec<u8> {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut buf = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return buf;
        }
        let msg = match tokio::time::timeout(remaining, ws.next()).await {
            Ok(Some(Ok(m))) => m,
            _ => return buf,
        };
        if let Message::Binary(b) = msg {
            buf.extend_from_slice(&b);
            if buf.windows(needle.len()).any(|w| w == needle) {
                return buf;
            }
        }
    }
}
