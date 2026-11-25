// ---- Pod-backed sessions: the engine side of the session-agent stream ------
//
// The test plays the in-pod vogt-session-agent: it receives the bootstrap
// token the pod would carry, exchanges it, dials the stream and speaks the
// frames from the vogt-session-agent README.

use super::*;
use tokio::net::TcpStream;
use tokio_tungstenite::{tungstenite::client::IntoClientRequest, MaybeTlsStream, WebSocketStream};
use vogt_engine_contract::ActivityState;
use vogt_engine_server::pod_backend::{CreateFuture, GoneFuture, LaunchFuture, PodLauncher};
use vogt_session_agent::Frame;

type Agent = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// A mocked cluster. A pod occupies its name (and uid) from `create` until
/// `terminating` after its delete, as a real `Terminating` pod does, and a
/// second `create` under an occupied name is refused (409 AlreadyExists).
struct FakeLauncher {
    created: tokio::sync::mpsc::UnboundedSender<(Uuid, Vec<u8>)>,
    deleted: Mutex<Vec<Uuid>>,
    /// Every delete as the engine sent it: session id and the uid named.
    delete_uids: Mutex<Vec<(Uuid, Option<String>)>>,
    pods: Mutex<Vec<k8s_openapi::api::core::v1::Pod>>,
    /// What the mocked cluster lists.
    listed: Mutex<Vec<k8s_openapi::api::core::v1::Pod>>,
    /// Name -> uid of the pods that exist, `Terminating` ones included.
    cluster: Arc<Mutex<std::collections::HashMap<Uuid, String>>>,
    /// How long a deleted pod stays `Terminating`.
    terminating: Mutex<Duration>,
    fail: bool,
    /// A cluster policy refusal (a full ResourceQuota), reported as a
    /// `Conflict` the way the real launcher maps a 403 (WI-4232).
    refuse: Mutex<Option<String>>,
}

impl PodLauncher for FakeLauncher {
    fn create<'a>(
        &'a self,
        id: Uuid,
        token: &'a [u8],
        pod: k8s_openapi::api::core::v1::Pod,
    ) -> CreateFuture<'a> {
        Box::pin(async move {
            if let Some(message) = self.refuse.lock().unwrap().clone() {
                return Err(vogt_engine_server::error::ApiError::Conflict(message));
            }
            if self.fail {
                return Err(vogt_engine_server::error::ApiError::Internal(
                    "no cluster".into(),
                ));
            }
            let uid = format!("uid-{}", Uuid::new_v4());
            {
                let mut cluster = self.cluster.lock().unwrap();
                if cluster.contains_key(&id) {
                    return Err(vogt_engine_server::error::ApiError::Internal(
                        "creating session pod: AlreadyExists".into(),
                    ));
                }
                cluster.insert(id, uid.clone());
            }
            self.pods.lock().unwrap().push(pod);
            let _ = self.created.send((id, token.to_vec()));
            Ok(Some(uid))
        })
    }
    fn delete(&self, id: Uuid, uid: Option<String>) -> LaunchFuture<'_> {
        Box::pin(async move {
            self.delete_uids.lock().unwrap().push((id, uid.clone()));
            let held = self.cluster.lock().unwrap().get(&id).cloned();
            if let (Some(want), Some(held)) = (&uid, &held) {
                if want != held {
                    return Err(vogt_engine_server::error::ApiError::Internal(
                        "deleting session pod: uid precondition failed".into(),
                    ));
                }
            }
            self.deleted.lock().unwrap().push(id);
            if let Some(held) = held {
                let linger = *self.terminating.lock().unwrap();
                let cluster = Arc::clone(&self.cluster);
                tokio::spawn(async move {
                    tokio::time::sleep(linger).await;
                    let mut cluster = cluster.lock().unwrap();
                    if cluster.get(&id) == Some(&held) {
                        cluster.remove(&id);
                    }
                });
            }
            Ok(())
        })
    }
    fn gone(&self, id: Uuid, uid: Option<String>) -> GoneFuture<'_> {
        Box::pin(async move {
            let held = self.cluster.lock().unwrap().get(&id).cloned();
            Ok(match held {
                None => true,
                Some(held) => uid.is_some_and(|uid| uid != held),
            })
        })
    }
    fn list(&self) -> vogt_engine_server::pod_backend::ListFuture<'_> {
        Box::pin(async move { Ok(self.listed.lock().unwrap().clone()) })
    }
    fn log_tail(&self, _: Uuid) -> vogt_engine_server::pod_backend::LogFuture<'_> {
        Box::pin(async { Ok(String::new()) })
    }
}

impl FakeLauncher {
    fn new(
        created: tokio::sync::mpsc::UnboundedSender<(Uuid, Vec<u8>)>,
        listed: Vec<k8s_openapi::api::core::v1::Pod>,
        fail: bool,
    ) -> Arc<Self> {
        let cluster = listed
            .iter()
            .filter_map(|pod| {
                let id = pod.metadata.labels.as_ref()?.get("vogt.session")?;
                Some((id.parse().ok()?, pod.metadata.uid.clone()?))
            })
            .collect();
        Arc::new(Self {
            created,
            deleted: Mutex::new(Vec::new()),
            delete_uids: Mutex::new(Vec::new()),
            pods: Mutex::new(Vec::new()),
            listed: Mutex::new(listed),
            cluster: Arc::new(Mutex::new(cluster)),
            terminating: Mutex::new(Duration::ZERO),
            fail,
            refuse: Mutex::new(None),
        })
    }
}

pub(super) struct Harness {
    pub(super) base: String,
    pub(super) state: Arc<AppState>,
    client: reqwest::Client,
    launcher: Arc<FakeLauncher>,
    pub(super) created: tokio::sync::mpsc::UnboundedReceiver<(Uuid, Vec<u8>)>,
    _guard: ServerGuard,
}

pub(super) async fn harness(fail: bool) -> Harness {
    harness_with(fail, None).await
}

/// `templates` replaces the harness's own `pod` template, e.g. with the
/// stock set a real engine boots with.
async fn harness_with(fail: bool, templates: Option<Vec<SessionTemplate>>) -> Harness {
    harness_cfg(fail, templates, |_| {}).await
}

/// [`harness_with`], with `tweak` applied to the engine's config first.
pub(super) async fn harness_cfg(
    fail: bool,
    templates: Option<Vec<SessionTemplate>>,
    tweak: impl FnOnce(&mut vogt_engine_server::config::Config),
) -> Harness {
    let mut cfg = test_config();
    tweak(&mut cfg);
    cfg.pod_backends_enabled = true;
    cfg.session_templates = templates.unwrap_or_else(|| {
        vec![SessionTemplate {
            name: "pod".into(),
            description: "pod-backed".into(),
            command: Some(vec!["/bin/cat".into()]),
            cwd: None,
            env: Vec::new(),
            account: None,
            default_name: None,
            match_repo_names: Vec::new(),
            match_path_prefixes: Vec::new(),
            tags: Vec::new(),
            backend: "pod".into(),
            host_capable: true,
            pod_capable: false,
        }]
    });
    let (base, state, guard) = boot_with_state(cfg).await;
    let (tx, created) = tokio::sync::mpsc::unbounded_channel();
    let launcher = FakeLauncher::new(tx, Vec::new(), fail);
    state.sessions.set_pod_launcher(launcher.clone());
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    Harness {
        base,
        state,
        client,
        launcher,
        created,
        _guard: guard,
    }
}

impl Harness {
    pub(super) async fn start(&self) -> Uuid {
        self.start_body(json!({ "name": "pod-session", "template": "pod" }))
            .await
    }

    pub(super) async fn start_body(&self, body: Value) -> Uuid {
        let response = self
            .client
            .post(format!("{}/api/sessions", self.base))
            .json(&body)
            .send()
            .await
            .unwrap();
        let status = response.status();
        let text = response.text().await.unwrap();
        let created: Value = serde_json::from_str(&text).expect("a JSON session reply");
        assert!(
            created["id"].is_string(),
            "start failed ({status}) for {body}: {text}"
        );
        created["id"].as_str().unwrap().parse().unwrap()
    }

    async fn bootstrap(&self, id: Uuid, bearer: &[u8]) -> reqwest::Response {
        self.bootstrap_as(id, bearer, "codex").await
    }

    async fn bootstrap_as(&self, id: Uuid, bearer: &[u8], cli: &str) -> reqwest::Response {
        self.bootstrap_with(id, bearer, cli, false).await
    }

    async fn bootstrap_with(
        &self,
        id: Uuid,
        bearer: &[u8],
        cli: &str,
        shell: bool,
    ) -> reqwest::Response {
        // The route sits outside the engine's own gate: the caller is the
        // pod, which holds only the one-shot token.
        reqwest::Client::new()
            .post(format!("{}/api/agent-auth/bootstrap", self.base))
            .bearer_auth(String::from_utf8_lossy(bearer))
            .json(&json!({
                "session_id": id.to_string(),
                "graft_repo": "vogt",
                "graft_line": "rust-2",
                "cli": cli,
                "shell": shell,
            }))
            .send()
            .await
            .unwrap()
    }

    async fn session_token(&mut self, id: Uuid) -> String {
        let (created_id, token) = self.created.recv().await.expect("a pod is created");
        assert_eq!(created_id, id);
        let res = self.bootstrap(id, &token).await;
        assert_eq!(res.status(), StatusCode::OK);
        res.json::<Value>().await.unwrap()["session_token"]
            .as_str()
            .unwrap()
            .to_string()
    }

    async fn connect(&self, id: Uuid, bearer: &str) -> Result<Agent, StatusCode> {
        let url = format!(
            "{}/api/agent-auth/sessions/{id}/agent",
            self.base.replace("http", "ws")
        );
        let mut req = url.into_client_request().unwrap();
        req.headers_mut()
            .insert("authorization", format!("Bearer {bearer}").parse().unwrap());
        match tokio_tungstenite::connect_async(req).await {
            Ok((ws, _)) => Ok(ws),
            Err(tokio_tungstenite::tungstenite::Error::Http(res)) => Err(res.status()),
            Err(e) => panic!("connect: {e}"),
        }
    }

    async fn dial(&self, id: Uuid, token: &str, cursor: u64) -> Agent {
        let mut ws = self.connect(id, token).await.expect("agent connects");
        put(
            &mut ws,
            Frame::Hello {
                session_id: id.to_string(),
            },
        )
        .await;
        put(&mut ws, Frame::Resume { cursor }).await;
        ws
    }

    async fn detail(&self, id: Uuid) -> SessionDetail {
        self.client
            .get(format!("{}/api/sessions/{id}", self.base))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    }

    async fn scrollback(&self, id: Uuid) -> Vec<u8> {
        base64::engine::general_purpose::STANDARD
            .decode(self.detail(id).await.scrollback_base64)
            .unwrap()
    }
}

async fn put(ws: &mut Agent, frame: Frame) {
    ws.send(Message::Text(serde_json::to_string(&frame).unwrap().into()))
        .await
        .unwrap();
}

/// Plays the agent's half of a stop's salvage (WI-4132): every stop of an
/// attached pod asks for a `Salvage` first and waits for `Salvaged`
/// before it signals the child, so an agent that ignores it stalls the
/// stop for the engine's whole salvage timeout.
async fn answer_salvage(ws: &mut Agent) {
    put(
        ws,
        Frame::Salvaged {
            ok: true,
            files: 0,
            reason: None,
        },
    )
    .await;
}

async fn take(ws: &mut Agent) -> Frame {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match ws.next().await.expect("stream open").expect("frame") {
                Message::Text(t) => return serde_json::from_str(&t).unwrap(),
                Message::Close(_) => panic!("engine closed the stream"),
                _ => {}
            }
        }
    })
    .await
    .expect("a frame within 10s")
}

/// A vogt-core stand-in that mints `minted-for-<id>` at
/// `POST /api/sessions/token`.
async fn mock_core() -> String {
    use axum::{routing::post, Router};
    async fn token(axum::Json(body): axum::Json<Value>) -> axum::Json<Value> {
        let id = body["engine_session_id"].as_str().unwrap_or_default();
        axum::Json(json!({
            "engine_session_id": id,
            "actor": format!("agent:engine:{id}"),
            "token": if body["revoke"] == json!(true) { Value::Null } else { json!(format!("minted-for-{id}")) },
            "revoked": 0,
        }))
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(
            listener,
            Router::new().route("/api/sessions/token", post(token)),
        )
        .await;
    });
    format!("http://{addr}")
}

/// Two signed-in accounts, `acct1` the deployment default (WI-4522).
fn default_account_onboarding(
    root: &std::path::Path,
) -> vogt_engine_server::claude_config::Onboarding {
    let mut accounts = Vec::new();
    for name in ["acct1", "acct2"] {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(".credentials.json"),
            format!(r#"{{"claudeAiOauth":{{"who":"{name}"}}}}"#),
        )
        .unwrap();
        accounts.push(format!("{name}={}", dir.display()));
    }
    vogt_engine_server::claude_config::Onboarding {
        enabled: true,
        default_dir: None,
        settings_file: None,
        opencode_config: None,
        account_concurrency_cap: 8,
        default_account: Some("acct1".into()),
        accounts_root: None,
        accounts: vogt_engine_server::claude_config::parse_accounts(Some(&accounts.join(","))),
        codex_policy: Ok(None),
    }
}

/// The bootstrap body's account and the login its bundle carries.
fn seeded_login(body: &Value) -> (String, String) {
    let account = body["env"]["VOGT_SESSION_ACCOUNT"]
        .as_str()
        .unwrap_or("")
        .to_string();
    let login = body["bundle"]
        .get(".credentials.json")
        .and_then(Value::as_str)
        .map(|b| {
            let bytes =
                base64::Engine::decode(&base64::engine::general_purpose::STANDARD, b).unwrap();
            String::from_utf8(bytes).unwrap()
        })
        .unwrap_or_default();
    (account, login)
}

/// A pod template running no agent, tagged `shell` when `shell`.
fn shell_pod_templates() -> Vec<SessionTemplate> {
    ["Shell", "raw"]
        .into_iter()
        .map(|name| SessionTemplate {
            name: name.into(),
            description: "pod-backed".into(),
            command: Some(vec!["/bin/cat".into()]),
            cwd: None,
            env: Vec::new(),
            account: None,
            default_name: None,
            match_repo_names: Vec::new(),
            match_path_prefixes: Vec::new(),
            tags: if name == "Shell" {
                vec!["shell".into()]
            } else {
                Vec::new()
            },
            backend: "pod".into(),
            host_capable: true,
            pod_capable: false,
        })
        .collect()
}

/// The core token this test's own `vogt-core` adopts at `init`.
const CORE_TOKEN: &str = "pod-codex-core-token-1234567890";

/// A real `vogt-core`, pointed at this engine and at the transcript roots a
/// deployment serving pod Codex replies sets.
struct CoreGuard {
    child: std::process::Child,
    url: String,
    _dir: tempfile::TempDir,
}

impl Drop for CoreGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The `vogt-core` debug binary these tests drive. Assert it before a
/// session starts: a missing binary discovered only inside the joined
/// observe future leaves the pod uploader running, so the test hangs to
/// its timeout instead of failing with this message (WI-3873).
fn vogt_core_binary() -> std::path::PathBuf {
    let binary = std::env::var("CARGO_TARGET_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../target"))
        .join("debug/vogt-core");
    assert!(
            binary.is_file(),
            "vogt-core debug binary is missing at {}; build it with `cargo build -p vogt-core` before these tests",
            binary.display()
        );
    binary
}

async fn boot_codex_core(engine_url: &str, transcript_root: &std::path::Path) -> CoreGuard {
    let binary = vogt_core_binary();
    let dir = tempfile::tempdir().unwrap();
    let core_token = dir.path().join("core-token");
    std::fs::write(&core_token, CORE_TOKEN).unwrap();
    let engine_token = dir.path().join("engine-token");
    std::fs::write(&engine_token, TEST_TOKEN).unwrap();
    let init = std::process::Command::new(&binary)
        .arg("init")
        .arg("--data-dir")
        .arg(dir.path())
        .env("VOGT_BOOTSTRAP_CORE_TOKEN_FILE", &core_token)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();
    assert!(
        init.status.success(),
        "vogt-core init failed: {}\n{}",
        init.status,
        String::from_utf8_lossy(&init.stderr)
    );
    // `serve` announces the port it was given, so port 0 is useless here;
    // ask the kernel for a free one instead.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
        .to_string();
    let child = std::process::Command::new(&binary)
        .args(["serve", "--host", "127.0.0.1", "--port", &port])
        .arg("--data-dir")
        .arg(dir.path())
        .env("VOGT_BOOTSTRAP_CORE_TOKEN_FILE", &core_token)
        .env("VOGT_ENGINE_URL", engine_url)
        .env("VOGT_ENGINE_TOKEN_FILE", &engine_token)
        // What a deployment that serves pod Codex replies sets: the root
        // the engine writes the pod's rollout back under.
        .env(
            "VOGT_SESSION_TRANSCRIPT_ROOTS",
            json!({ "codex": transcript_root.to_string_lossy() }).to_string(),
        )
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let url = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        if client
            .get(format!("{url}/health/ready"))
            .send()
            .await
            .map(|response| response.status().is_success())
            .unwrap_or(false)
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "vogt-core never became ready at {url}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    CoreGuard {
        child,
        url,
        _dir: dir,
    }
}

/// The argv a shell line runs, one word per line: `set --` applies the
/// line's own quoting, so the test reads the same argv Codex would.
fn shell_argv(line: &str) -> Vec<String> {
    let out = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(format!("set -- {line}; printf '%s\\n' \"$@\""))
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}

/// The `rollout-*-<conversation>.jsonl` under `root/sessions`, wherever
/// the agent put its date directories.
fn find_rollout(root: &std::path::Path, conversation: &str) -> Option<std::path::PathBuf> {
    let suffix = format!("-{conversation}.jsonl");
    fn walk(dir: &std::path::Path, suffix: &str, depth: usize) -> Option<std::path::PathBuf> {
        if depth == 0 {
            return None;
        }
        for entry in std::fs::read_dir(dir).ok()? {
            let path = entry.ok()?.path();
            if path.is_dir() {
                if let Some(found) = walk(&path, suffix, depth - 1) {
                    return Some(found);
                }
            } else if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("rollout-") && name.ends_with(suffix))
            {
                return Some(path);
            }
        }
        None
    }
    walk(&root.join("sessions"), &suffix, 6)
}

/// A pod's opencode store, as opencode lays it out, with an account row
/// holding a provider key and a tool call carrying a secret: what must
/// never leave the pod (WI-4291).
async fn opencode_pod_store(
    dir: &std::path::Path,
    engine_id: Uuid,
    reply: &str,
) -> std::path::PathBuf {
    use sqlx::{
        sqlite::{SqliteConnectOptions, SqliteJournalMode},
        ConnectOptions,
    };
    let db = dir.join("opencode.db");
    // opencode runs its store in WAL mode.
    let mut conn = SqliteConnectOptions::new()
        .filename(&db)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .connect()
        .await
        .unwrap();
    let brief = json!({"type": "text", "text": format!("Vogt started this session with a brief in /s/sessions/{engine_id}.md")}).to_string();
    let tool = json!({"type": "tool", "tool": "bash", "state": {"input": {"command": format!("echo {OPENCODE_TOOL_SECRET}")}, "output": OPENCODE_TOOL_SECRET}}).to_string();
    let text = json!({"type": "text", "text": reply}).to_string();
    let account = json!({"provider": "anthropic", "key": OPENCODE_PROVIDER_KEY}).to_string();
    let statements: Vec<(&str, Vec<String>)> = vec![
            ("CREATE TABLE session (id TEXT PRIMARY KEY, directory TEXT NOT NULL, parent_id TEXT, time_created INTEGER NOT NULL)", vec![]),
            ("CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT NOT NULL, time_created INTEGER NOT NULL, data TEXT NOT NULL)", vec![]),
            ("CREATE TABLE part (id TEXT PRIMARY KEY, message_id TEXT NOT NULL, session_id TEXT NOT NULL, time_created INTEGER NOT NULL, data TEXT NOT NULL)", vec![]),
            ("CREATE TABLE account (id TEXT PRIMARY KEY, data TEXT NOT NULL)", vec![]),
            ("INSERT INTO account VALUES ('acc', ?1)", vec![account]),
            ("INSERT INTO session VALUES ('ses_podoc', '/work', NULL, 1000)", vec![]),
            ("INSERT INTO message VALUES ('m1', 'ses_podoc', 1000, '{\"role\":\"user\"}')", vec![]),
            ("INSERT INTO message VALUES ('m2', 'ses_podoc', 2000, '{\"role\":\"assistant\"}')", vec![]),
            ("INSERT INTO message VALUES ('m3', 'ses_podoc', 3000, '{\"role\":\"assistant\"}')", vec![]),
            ("INSERT INTO part VALUES ('p1', 'm1', 'ses_podoc', 1, ?1)", vec![brief]),
            ("INSERT INTO part VALUES ('p2', 'm2', 'ses_podoc', 1, ?1)", vec![tool]),
            ("INSERT INTO part VALUES ('p3', 'm3', 'ses_podoc', 1, ?1)", vec![text]),
        ];
    for (sql, binds) in statements {
        let mut q = sqlx::query(sql);
        for b in binds {
            q = q.bind(b);
        }
        q.execute(&mut conn).await.unwrap();
    }
    db
}

const OPENCODE_PROVIDER_KEY: &str = "sk-ant-oat01-PODSECRETPODSECRETPODSECRET";
const OPENCODE_TOOL_SECRET: &str = "GITHUB_TOKEN=ghp_TOOLPAYLOADTOOLPAYLOAD0123456789";

fn opencode_pod_template() -> SessionTemplate {
    SessionTemplate {
        name: "pod".into(),
        description: "pod-backed opencode".into(),
        command: Some(vec!["opencode".into()]),
        cwd: None,
        env: Vec::new(),
        account: None,
        default_name: None,
        match_repo_names: Vec::new(),
        match_path_prefixes: Vec::new(),
        tags: Vec::new(),
        backend: "pod".into(),
        host_capable: false,
        pod_capable: false,
    }
}

fn opencode_agent(id: Uuid, base: &str, token: String) -> vogt_session_agent::Config {
    vogt_session_agent::Config {
        session_id: id.to_string(),
        engine_url: base.to_string(),
        token,
        cli: "opencode".into(),
        command: None,
        cwd: Some("/work".into()),
        cols: 120,
        rows: 40,
        buffer_bytes: 64 * 1024,
        claude_config_dir: None,
        codex_home: None,
        workspace_root: None,
        model: None,
        effort: None,
        permission_mode: None,
        codex_approval_policy: "never".into(),
        account: None,
        conversation_id: None,
        resume: false,
        claude: vogt_session_agent::ClaudeLaunch::default(),
        codex: vogt_session_agent::CodexLaunch::default(),
        opencode_mcp_config: None,
    }
}

/// Read attach frames (binary output and text alike) until `needle` has
/// appeared in what the viewer was sent.
async fn attach_collect(
    ws: &mut tokio_tungstenite::WebSocketStream<MaybeTlsStream<TcpStream>>,
    seen: &mut Vec<u8>,
    needle: &str,
) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !String::from_utf8_lossy(seen).contains(needle) {
            match ws.next().await.expect("attach stays open").expect("frame") {
                Message::Binary(b) => seen.extend_from_slice(&b),
                Message::Text(t) => seen.extend_from_slice(t.as_bytes()),
                _ => {}
            }
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "the viewer never saw {needle:?}; got {:?}",
            String::from_utf8_lossy(seen)
        )
    });
}

use vogt_engine_server::error::ApiError;
use vogt_engine_server::graft_mint::{
    GraftMinter, GraftToken, MintFuture, RevokeFuture, SessionGrant,
};

/// A graft minter that records the grants it is asked for.
struct FakeMinter {
    reply: Result<&'static str, &'static str>,
    grants: Mutex<Vec<SessionGrant>>,
    revoked: Mutex<Vec<Uuid>>,
}
impl FakeMinter {
    fn new(reply: Result<&'static str, &'static str>) -> Arc<Self> {
        Arc::new(Self {
            reply,
            grants: Mutex::new(Vec::new()),
            revoked: Mutex::new(Vec::new()),
        })
    }
}
impl GraftMinter for FakeMinter {
    fn mint<'a>(&'a self, grant: &'a SessionGrant) -> MintFuture<'a> {
        Box::pin(async move {
            self.grants.lock().unwrap().push(grant.clone());
            self.reply
                .map(GraftToken::new)
                .map_err(|e| ApiError::BadGateway(e.into()))
        })
    }
    fn revoke<'a>(&'a self, session: Uuid, _token: &'a GraftToken) -> RevokeFuture<'a> {
        Box::pin(async move {
            self.revoked.lock().unwrap().push(session);
            Ok(())
        })
    }
}

// ---- a fake graft AdminService (MintToken + RevokeToken) over real gRPC

#[derive(Clone, PartialEq, prost::Message)]
struct MintReq {
    #[prost(string, tag = "1")]
    subject: String,
    #[prost(string, repeated, tag = "2")]
    scopes: Vec<String>,
    #[prost(uint64, tag = "3")]
    ttl_s: u64,
    #[prost(string, optional, tag = "4")]
    session: Option<String>,
    #[prost(string, optional, tag = "5")]
    work_item: Option<String>,
}
#[derive(Clone, PartialEq, prost::Message)]
struct TokenMsg {
    #[prost(string, tag = "1")]
    token: String,
}
#[derive(Clone, PartialEq, prost::Message)]
struct RevokeReq {
    #[prost(string, tag = "1")]
    token: String,
}
#[derive(Clone, PartialEq, prost::Message)]
struct EmptyMsg {}

#[derive(Default)]
struct GraftState {
    /// Refuse every revoke with this code.
    revoke_fails: Option<tonic::Code>,
    minted: Vec<(Option<String>, MintReq)>,
    /// (bearer the call carried, token asked to be revoked)
    revokes: Vec<(Option<String>, String)>,
    revoked: std::collections::HashSet<String>,
}

#[derive(Clone, Default)]
struct FakeGraft(Arc<Mutex<GraftState>>);

impl tonic::server::NamedService for FakeGraft {
    const NAME: &'static str = "graft.v1.AdminService";
}
struct MintCall(FakeGraft, Option<String>);
impl tonic::server::UnaryService<MintReq> for MintCall {
    type Response = TokenMsg;
    type Future = std::future::Ready<std::result::Result<tonic::Response<TokenMsg>, tonic::Status>>;
    fn call(&mut self, req: tonic::Request<MintReq>) -> Self::Future {
        let mut g = self.0 .0.lock().unwrap();
        let token = format!("fake-minted-{}", g.minted.len() + 1);
        g.minted.push((self.1.clone(), req.into_inner()));
        std::future::ready(Ok(tonic::Response::new(TokenMsg { token })))
    }
}
struct RevokeCall(FakeGraft, Option<String>);
impl tonic::server::UnaryService<RevokeReq> for RevokeCall {
    type Response = EmptyMsg;
    type Future = std::future::Ready<std::result::Result<tonic::Response<EmptyMsg>, tonic::Status>>;
    fn call(&mut self, req: tonic::Request<RevokeReq>) -> Self::Future {
        let mut g = self.0 .0.lock().unwrap();
        let token = req.into_inner().token;
        g.revokes.push((self.1.clone(), token.clone()));
        std::future::ready(match g.revoke_fails {
            Some(code) => Err(tonic::Status::new(code, "revoke refused")),
            None => {
                g.revoked.insert(token);
                Ok(tonic::Response::new(EmptyMsg {}))
            }
        })
    }
}
impl<B> tower::Service<tonic::codegen::http::Request<B>> for FakeGraft
where
    B: tonic::codegen::Body + Send + 'static,
    B::Error: Into<tonic::codegen::StdError> + Send + 'static,
{
    type Response = tonic::codegen::http::Response<tonic::body::Body>;
    type Error = std::convert::Infallible;
    type Future = std::pin::Pin<
        Box<
            dyn std::future::Future<Output = std::result::Result<Self::Response, Self::Error>>
                + Send,
        >,
    >;
    fn poll_ready(
        &mut self,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::result::Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }
    fn call(&mut self, req: tonic::codegen::http::Request<B>) -> Self::Future {
        let this = self.clone();
        let bearer = req
            .headers()
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let path = req.uri().path().to_owned();
        Box::pin(async move {
            Ok(match path.as_str() {
                "/graft.v1.AdminService/MintToken" => tonic::server::Grpc::new(
                    tonic_prost::ProstCodec::<TokenMsg, MintReq>::default(),
                )
                .unary(MintCall(this, bearer), req)
                .await,
                "/graft.v1.AdminService/RevokeToken" => {
                    tonic::server::Grpc::new(
                        tonic_prost::ProstCodec::<EmptyMsg, RevokeReq>::default(),
                    )
                    .unary(RevokeCall(this, bearer), req)
                    .await
                }
                _ => tonic::Status::unimplemented("not faked").into_http(),
            })
        })
    }
}

/// Serve `graft` on a loopback port; returns its URL.
async fn serve_graft(graft: FakeGraft) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(graft)
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
            .await
            .unwrap();
    });
    format!("http://{addr}")
}

/// Serve the session's graft source through a provider that mints with
/// `minter`, its credentials kept under the engine's state dir.
fn install_minter(h: &Harness, minter: Arc<dyn GraftMinter>) {
    let state_dir = h.state.sessions.state_dir().to_path_buf();
    h.state.sessions.install_source_provider(Arc::new(
        vogt_engine_server::graft_provider::GraftProvider::with_minter(&state_dir, minter),
    ));
}

/// The real gRPC minter, pointed at a fake graft, with a mint token file.
async fn real_minter(graft: FakeGraft) -> (Arc<dyn GraftMinter>, tempfile::TempDir) {
    let url = serve_graft(graft).await;
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("graft-mint-token");
    std::fs::write(&file, "engine-mint-credential\n").unwrap();
    (
        Arc::new(vogt_engine_server::graft_mint::GrpcGraftMinter::new(
            url, file,
        )),
        dir,
    )
}

async fn kill_and_wait_exit(h: &Harness, id: Uuid) {
    let kill = h
        .client
        .post(format!("{}/api/sessions/{id}/kill", h.base))
        .send()
        .await
        .unwrap();
    assert_eq!(kill.status(), StatusCode::OK);
    tokio::time::timeout(Duration::from_secs(10), async {
        while h.detail(id).await.summary.exit_code != Some(137) {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("the session ends");
}

async fn until<F: Fn() -> bool>(what: &str, f: F) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !f() {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
}

#[derive(Clone, Default)]
struct LogCapture(Arc<Mutex<Vec<u8>>>);
impl std::io::Write for LogCapture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogCapture {
    type Writer = LogCapture;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// One process-wide capture for log assertions. A thread-local subscriber
/// alone loses events under parallel tests: `tracing`'s callsite interest
/// and max-level cache are global, so a sibling installing or dropping its
/// own subscriber can cache this callsite as "never" while it runs.
fn global_logs() -> Arc<Mutex<Vec<u8>>> {
    static LOGS: std::sync::OnceLock<Arc<Mutex<Vec<u8>>>> = std::sync::OnceLock::new();
    LOGS.get_or_init(|| {
        let logs = Arc::new(Mutex::new(Vec::new()));
        let _ = tracing::subscriber::set_global_default(
            tracing_subscriber::fmt()
                .with_writer(LogCapture(logs.clone()))
                .with_ansi(false)
                .with_max_level(tracing::Level::TRACE)
                .finish(),
        );
        logs
    })
    .clone()
}

/// A source is named by vogt-core alone (`require_source_authority`), so
/// the graft tests start as the stack secret rather than break-glass.
const GRAFT_CORE_TOKEN: &str = "stack-secret-for-graft-mint-1234567890";

async fn core_harness(fail: bool) -> Harness {
    let mut h = harness_cfg(fail, None, |cfg| {
        cfg.vogt_core_token = Some(GRAFT_CORE_TOKEN.into())
    })
    .await;
    h.client = client_for(GRAFT_CORE_TOKEN);
    h
}

/// A project-owned graft source, the one case that earns a session token.
fn graft_source() -> Value {
    json!({
        "location": {"kind": "graft", "endpoint": "http://graft.test:17888", "repo": "vogt"},
        "ref": "rust-2",
        "landing": "line"
    })
}

impl Harness {
    async fn start_with(&self, extra: Value) -> Uuid {
        let mut body = json!({ "name": "pod-session", "template": "pod" });
        body.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let response = self
            .client
            .post(format!("{}/api/sessions", self.base))
            .json(&body)
            .send()
            .await
            .unwrap();
        let status = response.status();
        let text = response.text().await.unwrap();
        let created: Value = serde_json::from_str(&text).expect("a JSON session reply");
        assert!(
            created["id"].is_string(),
            "start failed ({status}) for {body}: {text}"
        );
        created["id"].as_str().unwrap().parse().unwrap()
    }
}

async fn post_with_session_ref(h: &Harness, session_ref: &str) -> reqwest::StatusCode {
    h.client
        .post(format!("{}/api/sessions", h.base))
        .json(&json!({
            "name": "pod-session", "template": "pod", "session_ref": session_ref,
        }))
        .send()
        .await
        .unwrap()
        .status()
}

// ---- host-backed sessions: the graft credential's lifecycle (WI-3986)

/// The config home host credentials are delivered under in these tests. It
/// reaches the engine through `host_placement.graft_config_home`, not the
/// process environment, which parallel tests share (WI-4216).
fn host_config_home() -> std::path::PathBuf {
    static HOME: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    HOME.get_or_init(|| tempfile::tempdir().unwrap().keep())
        .clone()
}

fn host_token_path(session_ref: &str) -> std::path::PathBuf {
    host_config_home()
        .join("graft/sessions")
        .join(session_ref)
        .join("trial-1.token")
}

/// A harness whose `pod` template runs `script` on the host, with the
/// session's environment written to `out`.
async fn host_harness(out: &std::path::Path, command: Option<&str>) -> Harness {
    let script = format!("env > {}; exec cat", out.display());
    let template = SessionTemplate {
        name: "pod".into(),
        description: "host-backed".into(),
        command: Some(match command {
            Some(c) => vec![c.into()],
            None => vec!["/bin/sh".into(), "-c".into(), script],
        }),
        cwd: None,
        env: Vec::new(),
        account: None,
        default_name: None,
        match_repo_names: Vec::new(),
        match_path_prefixes: Vec::new(),
        tags: Vec::new(),
        backend: "pod".into(),
        host_capable: true,
        pod_capable: false,
    };
    let mut h = harness_cfg(false, Some(vec![template]), |cfg| {
        cfg.vogt_core_token = Some(GRAFT_CORE_TOKEN.into());
        // These start Vogt workers on the host, which a worker is refused
        // unless the transition override is set (WI-4428).
        cfg.host_placement.allow_workers = true;
        cfg.host_placement.graft_config_home = Some(host_config_home());
    })
    .await;
    h.client = client_for(GRAFT_CORE_TOKEN);
    h
}

async fn host_env(out: &std::path::Path) -> String {
    until("the session environment", || {
        std::fs::read_to_string(out).is_ok_and(|t| t.contains("PATH="))
    })
    .await;
    std::fs::read_to_string(out).unwrap()
}

// ---- Hibernation, wake and restart re-adoption (WI-3158, WI-3205) ------

/// A harness whose template runs Claude on a named account, so the session
/// has a conversation to resume and an account bundle the engine holds.
async fn claude_harness() -> (Harness, std::path::PathBuf) {
    let accounts = tempfile::tempdir().unwrap().keep();
    let mut cfg = test_config();
    cfg.pod_backends_enabled = true;
    let mut onboarding = vogt_engine_server::claude_config::Onboarding::default();
    for name in ["acct", "other"] {
        let dir = accounts.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        onboarding
            .accounts
            .push(vogt_engine_server::claude_config::ClaudeAccount {
                name: name.into(),
                dir,
            });
    }
    onboarding.account_concurrency_cap = 8;
    cfg.agent_onboarding = onboarding;
    cfg.session_templates = vec![SessionTemplate {
        name: "pod".into(),
        description: "pod-backed claude".into(),
        command: Some(vec!["claude".into()]),
        cwd: None,
        env: Vec::new(),
        account: Some("acct".into()),
        default_name: None,
        match_repo_names: Vec::new(),
        match_path_prefixes: Vec::new(),
        tags: Vec::new(),
        backend: "pod".into(),
        host_capable: false,
        pod_capable: false,
    }];
    let (base, state, guard) = boot_with_state(cfg).await;
    let (tx, created) = tokio::sync::mpsc::unbounded_channel();
    let launcher = FakeLauncher::new(tx, Vec::new(), false);
    state.sessions.set_pod_launcher(launcher.clone());
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    (
        Harness {
            base,
            state,
            client,
            launcher,
            created,
            _guard: guard,
        },
        accounts,
    )
}

fn transcript_path(conversation: &str) -> String {
    format!("projects/-work/{conversation}.jsonl")
}

async fn conversation_of(h: &Harness, id: Uuid) -> String {
    h.state
        .sessions
        .summary_of(id)
        .and_then(|s| s.conversation)
        .expect("a claude pod session has a conversation")
        .id
}

/// Plays the agent through a hibernation: stores the newer transcript when
/// asked to flush, confirms, answers the salvage, then reports the child's
/// exit when told to stop.
async fn agent_hibernates(
    h: &Harness,
    agent: &mut Agent,
    account: &str,
    conversation: &str,
    offset: u64,
    newer: &[u8],
) {
    loop {
        match take(agent).await {
            Frame::Flush => {
                h.state
                    .sessions
                    .secret_broker()
                    .append_transcript(
                        account,
                        conversation,
                        &transcript_path(conversation),
                        offset,
                        newer,
                    )
                    .expect("write-back accepted");
                put(agent, Frame::Flushed { ok: true }).await;
            }
            Frame::Salvage => answer_salvage(agent).await,
            Frame::Signal { signal } if signal == "kill" => {
                put(agent, Frame::Exited { status: Some(0) }).await;
                return;
            }
            _ => {}
        }
    }
}

/// An engine that restarted: a registry over the same state directory
/// and history whose process memory holds nothing of the old one.
fn restarted(h: &Harness, launcher: Arc<FakeLauncher>) -> Arc<vogt_engine_server::SessionRegistry> {
    let registry = Arc::new(vogt_engine_server::SessionRegistry::new(
        Arc::clone(&h.state.config),
        vogt_engine_server::events::EventBus::new(64),
        h.state.history.clone(),
    ));
    registry.set_pod_launcher(launcher);
    registry
}

/// Wait for the registry's session to have ended, so its reader thread is
/// gone before the test's runtime is dropped.
async fn ended(registry: &vogt_engine_server::SessionRegistry, id: Uuid) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while registry.get(id).is_ok_and(|s| s.is_alive()) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the session ends");
}

fn running_pod(id: Uuid) -> k8s_openapi::api::core::v1::Pod {
    running_pod_of(id, vogt_engine_server::pod_backend::DEFAULT_ENGINE_ID)
}

fn running_pod_of(id: Uuid, engine: &str) -> k8s_openapi::api::core::v1::Pod {
    let mut pod = k8s_openapi::api::core::v1::Pod::default();
    pod.metadata.uid = Some(format!("uid-adopted-{id}"));
    pod.metadata.labels = Some(std::collections::BTreeMap::from([
        ("vogt.session".to_string(), id.to_string()),
        ("vogt.engine".to_string(), engine.to_string()),
    ]));
    pod.status = Some(k8s_openapi::api::core::v1::PodStatus {
        phase: Some("Running".into()),
        ..Default::default()
    });
    pod
}

async fn log_len(h: &Harness, id: Uuid, at_least: u64) {
    let path = h.state.history.as_ref().unwrap().log_path(id);
    tokio::time::timeout(Duration::from_secs(10), async {
        while std::fs::metadata(&path).map_or(0, |m| m.len()) < at_least {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the output reaches the log");
}

fn fake_launcher(listed: Vec<k8s_openapi::api::core::v1::Pod>) -> Arc<FakeLauncher> {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    FakeLauncher::new(tx, listed, false)
}
mod bootstrap;
mod session;
mod stream;
