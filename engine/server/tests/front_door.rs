//! The front door, end to end, ported from `tests/test_front_door.py`.
//!
//! The Python original boots two real processes: the Python core (`vogt
//! serve`) and this engine, then reads the audit log through Vogt's own CLI.
//! Most tests here run against a stand-in core: a real HTTP server that
//! resolves each bearer the way the core does (its own token table, the stack
//! secret, a session minted by a password login), gates writes on the scopes
//! it holds, and writes an audit row per successful write. What is real is
//! the engine's door: the bearer gate, the capability gate, the identity
//! cache, the credential each caller is handed on, and the logout eviction.
//!
//! The two audit-row claims do not trust the stand-in's audit store. They boot
//! the real `vogt-core` binary the stack runs, write through the engine's door
//! to it, and read the audit table back with `vogt-core audit list`, the same
//! read the Python test makes. Like `rust_core.rs`, they need the binary built
//! first (`cargo build -p vogt-core` in `engine/`); a missing binary fails the
//! test rather than skipping it.

use std::{
    collections::{HashMap, HashSet},
    net::SocketAddr,
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use axum::{
    body::Bytes,
    extract::State,
    http::{header, HeaderMap, Method, StatusCode as AxumStatus, Uri},
    response::{IntoResponse, Response},
    routing::any,
    Json, Router,
};
use reqwest::StatusCode;
use serde_json::{json, Value};
use vogt_engine_server::{app::router, Config};

/// The optional static break-glass token. Full capability on the door, no
/// actor of its own at the core: its Vogt calls borrow the stack secret.
const BREAK_GLASS: &str = "front-door-primary-token-000000";
/// The stack secret, the one credential both halves share.
const STACK_SECRET: &str = "stack-secret-shared-by-both-halves-0000";
/// Core tokens for four actors, each with a scope set. Two actors so one
/// cannot pass for a proxy that hard-codes the name.
const ALPHA: &str = "alpha-core-token-aaaaaaaaaaaaaaaa";
const BETA: &str = "beta-core-token-bbbbbbbbbbbbbbbb";
/// Read only: the door derives no write capability from it.
const READER: &str = "reader-core-token-cccccccccccccccc";
/// Clears the door (`project.write` earns `vogt-write`) but the core refuses
/// a work write from it.
const PROJECTOR: &str = "projector-core-token-dddddddddddddd";
/// Ada's password, and the session a successful login mints for her.
const ADA_PASSWORD: &str = "correct horse battery staple";
const ADA_SESSION: &str = "adas-login-session-token-0987654321";

/// The stand-in core's request log entry: enough to see which credential
/// reached it, and on which path.
#[derive(Debug, Clone)]
struct Forwarded {
    method: String,
    path: String,
    bearer: Option<String>,
}

/// One row of the stand-in core's audit store.
#[derive(Debug, Clone)]
struct AuditRow {
    actor: String,
}

#[derive(Default)]
struct Core {
    forwarded: Vec<Forwarded>,
    /// Session secrets a password login has minted and no logout has revoked.
    sessions: HashSet<String>,
    /// Successful writes, keyed by operation.
    audit: Vec<(String, AuditRow)>,
}

type Shared = Arc<Mutex<Core>>;

/// Who the stand-in core says a bearer is.
#[derive(Debug, Clone)]
struct Who {
    name: &'static str,
    kind: &'static str,
    scopes: Vec<&'static str>,
}

fn static_identity(bearer: &str) -> Option<Who> {
    let (name, kind, scopes): (&'static str, &'static str, &[&'static str]) = match bearer {
        // The stack secret is the core's own identity; the core adopted it
        // at init as this front door's actor.
        STACK_SECRET => ("agent:vogt-engine", "agent", &["admin"]),
        ALPHA => ("agent:alpha", "agent", &["read", "work.write"]),
        BETA => ("agent:beta", "agent", &["read", "work.write"]),
        READER => ("agent:reader", "agent", &["read"]),
        PROJECTOR => ("agent:projector", "agent", &["read", "project.write"]),
        _ => return None,
    };
    Some(Who {
        name,
        kind,
        scopes: scopes.to_vec(),
    })
}

fn identify(core: &Core, bearer: &str) -> Option<Who> {
    if let Some(who) = static_identity(bearer) {
        return Some(who);
    }
    if core.sessions.contains(bearer) {
        return Some(Who {
            name: "human:ada",
            kind: "human",
            scopes: vec!["read", "work.write"],
        });
    }
    None
}

fn allows(who: &Who, scope: &str) -> bool {
    who.scopes.iter().any(|s| *s == "admin" || *s == scope)
}

fn unauthenticated() -> Response {
    (
        AxumStatus::UNAUTHORIZED,
        Json(json!({"error": {"code": "unauthenticated", "message": "the presented token is not valid"}})),
    )
        .into_response()
}

fn forbidden(message: &str) -> Response {
    (
        AxumStatus::FORBIDDEN,
        Json(json!({"error": {"code": "forbidden", "message": message}})),
    )
        .into_response()
}

async fn core_handler(
    State(core): State<Shared>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let bearer = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::to_owned);
    let mut core = core.lock().unwrap();
    core.forwarded.push(Forwarded {
        method: method.to_string(),
        path: uri.path().to_string(),
        bearer: bearer.clone(),
    });
    let who = bearer.as_deref().and_then(|b| identify(&core, b));
    let payload: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);

    match (method, uri.path()) {
        (Method::GET, "/api/auth/whoami") => match who {
            Some(w) => (
                AxumStatus::OK,
                Json(json!({
                    "identity_ref": w.name,
                    "kind": w.kind,
                    "display_name": w.name,
                    "scopes": w.scopes,
                    "token": null,
                })),
            )
                .into_response(),
            None => unauthenticated(),
        },
        (Method::POST, "/api/auth/login") => {
            let username = payload["username"].as_str().unwrap_or_default();
            let password = payload["password"].as_str().unwrap_or_default();
            if username.eq_ignore_ascii_case("ada") && password == ADA_PASSWORD {
                core.sessions.insert(ADA_SESSION.to_string());
                (
                    AxumStatus::OK,
                    Json(json!({"token": {"kind": "session"}, "secret": ADA_SESSION})),
                )
                    .into_response()
            } else {
                (
                    AxumStatus::UNAUTHORIZED,
                    Json(json!({"error": {"code": "unauthenticated", "message": "the username or password is not right"}})),
                )
                    .into_response()
            }
        }
        (Method::POST, "/api/auth/logout") => {
            if who.is_none() {
                return unauthenticated();
            }
            let revoked = bearer
                .as_deref()
                .is_some_and(|b| core.sessions.remove(b));
            (AxumStatus::OK, Json(json!({"revoked": revoked}))).into_response()
        }
        (Method::POST, "/api/labels") => {
            let Some(w) = who else {
                return unauthenticated();
            };
            if !allows(&w, "work.write") {
                return forbidden("forbidden: a label write needs the work.write scope");
            }
            core.audit.push((
                "label.create".to_string(),
                AuditRow {
                    actor: w.name.to_string(),
                },
            ));
            (AxumStatus::OK, Json(json!({"name": payload["name"]}))).into_response()
        }
        (Method::POST, "/api/work") => {
            let Some(w) = who else {
                return unauthenticated();
            };
            // The core's own gate on a work write, behind the door's.
            if !allows(&w, "work.write") {
                return forbidden(&format!(
                    "forbidden: a work write needs the work.write scope; this credential holds [{}]",
                    w.scopes.join(", ")
                ));
            }
            core.audit.push((
                "work.create".to_string(),
                AuditRow {
                    actor: w.name.to_string(),
                },
            ));
            (AxumStatus::OK, Json(json!({"id": "work-1"}))).into_response()
        }
        // The door's event follower polls this.
        (Method::GET, "/api/events") => {
            (AxumStatus::OK, Json(json!({"events": [], "next_cursor": 0}))).into_response()
        }
        (Method::GET, "/health/ready") => (
            AxumStatus::OK,
            Json(json!({"status": "ready", "declared_schema_version": 6, "observed_schema_version": 6})),
        )
            .into_response(),
        (_, path) => (AxumStatus::OK, Json(json!({"seen": path}))).into_response(),
    }
}

/// A front door with the stand-in core behind it, and the core's state.
struct Door {
    base: String,
    core: Shared,
}

impl Door {
    fn forwarded_with(&self, bearer: &str) -> Vec<Forwarded> {
        self.core
            .lock()
            .unwrap()
            .forwarded
            .iter()
            .filter(|f| f.bearer.as_deref() == Some(bearer))
            .cloned()
            .collect()
    }

    /// The core's audit rows for one operation, keyed by the actor that wrote
    /// them. The Python test reads the same thing through `vogt audit list`.
    fn audit_by_actor(&self, operation: &str) -> HashMap<String, AuditRow> {
        self.core
            .lock()
            .unwrap()
            .audit
            .iter()
            .filter(|(op, _)| op == operation)
            .map(|(_, row)| (row.actor.clone(), row.clone()))
            .collect()
    }

    fn audit_for(&self, operation: &str) -> usize {
        self.core
            .lock()
            .unwrap()
            .audit
            .iter()
            .filter(|(op, _)| op == operation)
            .count()
    }
}

async fn door() -> Door {
    let core: Shared = Arc::new(Mutex::new(Core::default()));
    let stand_in = Router::new()
        .route("/{*path}", any(core_handler))
        .with_state(Arc::clone(&core));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, stand_in).await;
    });

    Door {
        base: serve_engine(&format!("http://{addr}")).await,
        core,
    }
}

/// Serves the engine's front door in front of the core at `core_url` and
/// returns the door's base URL. The engine holds the stack secret, the same
/// credential the core adopted at init.
async fn serve_engine(core_url: &str) -> String {
    let mut cfg = base_config();
    cfg.vogt_core_url = Some(core_url.to_string());
    cfg.vogt_core_token = Some(STACK_SECRET.to_string());
    let (router, _state) = router(cfg).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    format!("http://{addr}")
}

/// The path to the `vogt-core` debug binary in this workspace's target
/// directory, as `rust_core.rs` finds it.
fn core_binary() -> PathBuf {
    let target = std::env::var("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../target"));
    let path = target.join("debug/vogt-core");
    assert!(
        path.is_file(),
        "vogt-core debug binary is missing at {}; build it with `cargo build -p vogt-core` before these tests",
        path.display()
    );
    path
}

/// A real `vogt-core` the way the stack runs it: initialised from a scratch
/// data dir with the stack secret as its bootstrap token, two agent actors
/// with tokens issued by the CLI, and `serve` on loopback.
struct RealCore {
    child: Child,
    url: String,
    data: PathBuf,
    /// The secrets `token issue` returned, keyed by actor name.
    tokens: HashMap<&'static str, String>,
    _dir: tempfile::TempDir,
}

impl Drop for RealCore {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Runs a `vogt-core` verb with the global flags first, as the admin CLI
/// tests and the Python harness both do.
fn core_cli(data: &Path, args: &[&str]) -> Output {
    Command::new(core_binary())
        .arg("--json")
        .arg("--data-dir")
        .arg(data)
        .args(args)
        .output()
        .expect("run vogt-core")
}

/// The parsed JSON stdout of a successful verb.
fn core_json(output: &Output, what: &str) -> Value {
    assert!(
        output.status.success(),
        "{what} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "{what}: stdout is not JSON ({error}): {}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

async fn boot_real_core() -> RealCore {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("instance");
    let stack_secret = dir.path().join("stack-secret");
    std::fs::write(&stack_secret, STACK_SECRET).unwrap();

    let init = Command::new(core_binary())
        .arg("--json")
        .arg("--data-dir")
        .arg(&data)
        .arg("init")
        .env("VOGT_BOOTSTRAP_CORE_TOKEN_FILE", &stack_secret)
        .output()
        .expect("run vogt-core init");
    core_json(&init, "vogt-core init");

    let mut tokens = HashMap::new();
    for actor in ["alpha", "beta"] {
        let identity = format!("agent:{actor}");
        let created = core_cli(
            &data,
            &[
                "actor",
                "create",
                "--identity-ref",
                &identity,
                "--kind",
                "agent",
                "--display-name",
                actor,
                "--reason",
                "front door test",
            ],
        );
        core_json(&created, "actor create");
        let issued = core_cli(
            &data,
            &[
                "token",
                "issue",
                "--actor",
                &identity,
                "--name",
                actor,
                "--scopes",
                "read,work.write",
                "--reason",
                "front door test",
            ],
        );
        let secret = core_json(&issued, "token issue")["secret"]
            .as_str()
            .expect("token issue returns the secret once")
            .to_string();
        tokens.insert(actor, secret);
    }

    // The core announces the port it was given, not the one the kernel
    // chose, so pick a free port first.
    let port = {
        let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        probe.local_addr().unwrap().port()
    };
    let child = Command::new(core_binary())
        .arg("--data-dir")
        .arg(&data)
        .args(["serve", "--host", "127.0.0.1", "--port", &port.to_string()])
        .env("VOGT_BOOTSTRAP_CORE_TOKEN_FILE", &stack_secret)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn vogt-core serve");
    let url = format!("http://127.0.0.1:{port}");
    wait_ready(&url).await;
    RealCore {
        child,
        url,
        data,
        tokens,
        _dir: dir,
    }
}

async fn wait_ready(url: &str) {
    let client = reqwest::Client::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if client
            .get(format!("{url}/health/ready"))
            .send()
            .await
            .map(|response| response.status().is_success())
            .unwrap_or(false)
        {
            return;
        }
        if Instant::now() > deadline {
            panic!("vogt-core never became ready at {url}");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// The real core's `label.create` audit rows, keyed by the actor that wrote
/// them. The same read the Python test makes through `vogt audit list`.
fn real_label_audit(core: &RealCore) -> HashMap<String, Value> {
    let trail = core_json(
        &core_cli(
            &core.data,
            &[
                "audit",
                "list",
                "--operation",
                "label.create",
                "--limit",
                "20",
            ],
        ),
        "audit list",
    );
    trail["records"]
        .as_array()
        .expect("audit list returns records")
        .iter()
        .map(|record| {
            let actor = record["actor_identity_ref"].as_str().unwrap().to_string();
            (actor, record.clone())
        })
        .collect()
}

/// POST to the door, optionally with a bearer. Returns the status and the
/// body as text, so a test can assert on either.
async fn post(base: &str, path: &str, token: Option<&str>, body: Value) -> (StatusCode, String) {
    let mut request = reqwest::Client::new()
        .post(format!("{base}{path}"))
        .json(&body);
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    let answer = request.send().await.unwrap();
    (answer.status(), answer.text().await.unwrap())
}

async fn get(base: &str, path: &str, token: &str) -> (StatusCode, String) {
    let answer = reqwest::Client::new()
        .get(format!("{base}{path}"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    (answer.status(), answer.text().await.unwrap())
}

/// The same configuration the door tests in `vogt_core.rs` boot with: every
/// field the engine reads, none of them the point of these tests.
fn base_config() -> Config {
    Config {
        bind: "127.0.0.1:0".parse().unwrap(),
        token: Some(BREAK_GLASS.to_string()),
        token_mutating_request_limit_per_minute: 600,
        scrollback_bytes: 64 * 1024,
        default_shell: "/bin/bash".to_string(),
        default_cwd: std::env::temp_dir(),
        activity_idle_after_ms: 200,
        idle_stall_after_ms: 10 * 60 * 1_000,
        workspace_root: std::env::temp_dir(),
        gui_stream_url: None,
        gui_stream_verified: false,
        ws_query_token_allowed: false,
        push_allow_insecure_endpoints: false,
        state_dir: tempfile::tempdir().unwrap().keep(),
        fcm_service_account_json: None,
        vapid_subject: "mailto:test@example.invalid".to_string(),
        allowed_origins: vec![],
        auto_agent_auth: false,
        agent_auth_helper: "/usr/local/bin/vogt-agent-auth".into(),
        codex_session_id: false,
        agent_auth_secrets: vec![],
        agent_grant_projects: vec![],
        session_templates: vec![],
        assistant_api_key: None,
        assistant_base_url: "https://api.example.com/v1".into(),
        assistant_model: "gpt-5.4-mini".into(),
        assistant_max_tool_calls: 8,
        assistant_starts_per_hour: 3,
        assistant_allow_claude_proxy: false,
        assistant_reasoning_effort: None,
        assistant_profiles: vec![],
        assistant_default_profile: None,
        assistant_log_retention_days: 30,
        history_retention_days: 30,
        history_live_scan_bytes: 256 * 1024,
        assistant_stt_base_urls: vec![],
        assistant_stt_api_key: None,
        assistant_stt_model: "whisper-1".into(),
        assistant_stt_language: "en".into(),
        assistant_tts_base_urls: vec![],
        assistant_tts_api_key: None,
        assistant_tts_model: "tts-1".into(),
        assistant_tts_voice: "alloy".into(),
        assistant_tts_format: "mp3".into(),
        assistant_speech_attempt_timeout_ms: 30_000,
        public_url: None,
        vogt_core_url: None,
        vogt_import_root: None,
        vogt_engine_state_dir: None,
        vogt_core_token: None,
        agent_clis: vogt_engine_server::agent_clis::AgentCliPaths::default(),
        hibernation: vogt_engine_server::hibernate_policy::Policy::default(),
        session_list_retain_after: None,
        assistant_call: vogt_engine_server::call::CallPolicy::default(),
        autopilot: vogt_engine_server::autopilot::Policy::default(),
        budget: vogt_engine_server::budget::Policy::default(),
        agent_onboarding: vogt_engine_server::claude_config::Onboarding::default(),
        session_rss_warn_bytes: None,
        worktree_clean_on_hibernate: false,
        disk_warn_percent: 85,
        metrics_bind: None,
        chat: Default::default(),
        economics_tick: std::time::Duration::ZERO,
        economics_home: None,
    }
}

// -- the five end-to-end claims, as test_front_door.py states them ----------

/// The front door's whole claim: a caller presenting a core credential to the
/// door writes as *that* actor, not as a shared proxy identity. Read back from
/// the real core's audit table.
#[tokio::test]
async fn a_write_through_the_front_door_is_audited_as_the_caller() {
    let core = boot_real_core().await;
    let base = serve_engine(&core.url).await;
    for actor in ["alpha", "beta"] {
        let (status, body) = post(
            &base,
            "/api/vogt/labels",
            Some(&core.tokens[actor]),
            json!({"name": format!("{actor}-was-here"), "reason": "front door test"}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{actor}: {body}");
    }

    let by_actor = real_label_audit(&core);
    assert!(
        by_actor.contains_key("agent:alpha") && by_actor.contains_key("agent:beta"),
        "each write is attributed to the actor whose credential was presented; \
         the log says {:?}",
        by_actor.keys().collect::<Vec<_>>()
    );
    assert_eq!(by_actor["agent:alpha"]["reason"], "front door test");
}

/// The one shared identity left, and it is named: the static engine token has
/// no actor of its own, so the door lends it the stack secret's. Read back
/// from the real core's audit table.
#[tokio::test]
async fn the_break_glass_token_writes_as_the_stack_secrets_actor() {
    let core = boot_real_core().await;
    let base = serve_engine(&core.url).await;
    let (status, body) = post(
        &base,
        "/api/vogt/labels",
        Some(BREAK_GLASS),
        json!({"name": "break-glass-was-here", "reason": "front door test"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let by_actor = real_label_audit(&core);
    assert!(
        by_actor.contains_key("agent:vogt-engine"),
        "{:?}",
        by_actor.keys().collect::<Vec<_>>()
    );
}

/// Two gates, neither substituting for the other. A read-only credential is
/// refused at the door before the core is asked; a `project.write` credential
/// clears the door and is refused by the core. A refusal at either writes
/// nothing.
#[tokio::test]
async fn the_front_doors_gate_and_the_cores_gate_both_stand() {
    let door = door().await;
    let work = json!({
        "kind": "bug",
        "title": "nope",
        "project": "alpha",
        "reason": "front door test",
    });

    let (reader, body) = post(&door.base, "/api/vogt/work", Some(READER), work.clone()).await;
    assert_eq!(
        reader,
        StatusCode::FORBIDDEN,
        "the front door refuses a credential without `vogt-write`: {body}"
    );
    assert!(body.contains("capability"), "{body}");
    assert!(
        door.forwarded_with(READER)
            .iter()
            .all(|f| f.method == "GET" && f.path == "/api/auth/whoami"),
        "the refused write never reached the core"
    );

    let (projector, body) = post(&door.base, "/api/vogt/work", Some(PROJECTOR), work).await;
    assert_eq!(
        projector,
        StatusCode::FORBIDDEN,
        "the core refuses a work write from a project.write credential that cleared the door: {body}"
    );
    assert!(body.contains("work.write"), "{body}");
    assert!(
        door.forwarded_with(PROJECTOR)
            .iter()
            .any(|f| f.method == "POST" && f.path == "/api/work"),
        "the projector cleared the door, so the core must have been asked"
    );

    assert_eq!(
        door.audit_for("work.create"),
        0,
        "a refusal at either gate writes nothing"
    );
}

/// A bearer the core does not know is refused at the door.
#[tokio::test]
async fn an_unknown_credential_is_refused_at_the_door() {
    let door = door().await;
    let (status, body) = post(
        &door.base,
        "/api/vogt/labels",
        Some("nobody-issued-this-token-0000000000"),
        json!({"name": "never", "reason": "front door test"}),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert!(body.contains("not valid"), "{body}");
}

/// The browser's path, end to end: an open login through the door mints a
/// session at the core; the door resolves that session; a write with it is
/// audited to the person; a logout ends it, and the very next request is
/// refused at the door.
#[tokio::test]
async fn a_person_signs_in_with_a_password_and_writes_as_themselves() {
    let door = door().await;
    let (refused, body) = post(
        &door.base,
        "/api/auth/login",
        None,
        json!({"username": "ada", "password": "not it"}),
    )
    .await;
    assert_eq!(refused, StatusCode::UNAUTHORIZED, "{body}");

    let (status, body) = post(
        &door.base,
        "/api/auth/login",
        None,
        json!({"username": "Ada", "password": ADA_PASSWORD}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let session: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(session["token"]["kind"], "session");
    let secret = session["secret"].as_str().unwrap().to_string();

    let (status, body) = get(&door.base, "/api/auth/check", &secret).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let check: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(check["identity"]["name"], "human:ada");

    let (status, body) = post(
        &door.base,
        "/api/vogt/labels",
        Some(&secret),
        json!({"name": "ada-was-here", "reason": "front door test"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(door
        .audit_by_actor("label.create")
        .contains_key("human:ada"));

    let (status, body) = post(
        &door.base,
        "/api/vogt/auth/logout",
        Some(&secret),
        json!({"reason": "front door test"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let logout: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(logout["revoked"], true, "{body}");

    // Refused at the door on the very next request: the logout evicted the
    // door's cache and the core no longer knows the session.
    let (status, body) = get(&door.base, "/api/auth/check", &secret).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
}
