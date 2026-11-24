use super::*;

/// Start one stub agent per `(name, script)` in the hibernation sandbox and
/// return their ids once each has printed `helper=[`.
pub async fn start_stub_agents(
    client: &reqwest::Client,
    base: &str,
    bin: &std::path::Path,
    agents: &[(&str, &str)],
) -> Vec<String> {
    let mut ids = Vec::new();
    for (name, script) in agents {
        // Each stub is named `claude` (that is how the engine tells an agent
        // CLI), in a directory of its own.
        let dir = bin.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let stub = dir.join("claude");
        std::fs::write(&stub, script).unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
        let created: Value = client
            .post(format!("{base}/api/sessions"))
            .json(&json!({ "name": name, "command": [stub.to_string_lossy()] }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        ids.push(created["id"].as_str().unwrap().to_string());
    }
    for id in &ids {
        live_output_containing(client, base, id, &["helper=["]).await;
    }
    ids
}

/// [`hibernation_sandbox`] with a `claude` template that runs the stub, the
/// way a deployment's templates run Claude Code: what a wake resumes a
/// reported conversation through (WI-962).
pub fn conversation_sandbox() -> (tempfile::TempDir, Config, std::path::PathBuf) {
    let (tmp, mut cfg, stub) = hibernation_sandbox();
    cfg.session_templates.push(SessionTemplate {
        name: "Claude Code (test)".to_string(),
        description: "the stand-in claude".to_string(),
        command: Some(vec![stub.to_string_lossy().into_owned()]),
        cwd: None,
        env: vec![],
        default_name: None,
        match_repo_names: vec![],
        match_path_prefixes: vec![],
        tags: vec!["claude".to_string()],
        account: None,
        backend: "local".to_string(),
        host_capable: false,
        pod_capable: false,
    });
    (tmp, cfg, stub)
}

/// Stand in for a SIGKILL of the engine: end the session (so this engine's
/// exit hook has run and cannot race the next engine) and put back the
/// write-ahead record a killed engine would have left.
pub async fn lose_session(
    client: &reqwest::Client,
    base: &str,
    id: &str,
    record_path: &std::path::Path,
) {
    let record = std::fs::read_to_string(record_path).unwrap();
    client
        .post(format!("{base}/api/sessions/{id}/kill"))
        .send()
        .await
        .unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while record_path.exists() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the record outlived the kill"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    std::fs::write(record_path, record).unwrap();
}

pub async fn session_row(client: &reqwest::Client, base: &str, id: &str) -> Value {
    let listed: Value = client
        .get(format!("{base}/api/sessions"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let listed: Vec<Value> = serde_json::from_value(listed["sessions"].clone()).unwrap();
    listed
        .into_iter()
        .find(|s| s["id"] == id)
        .unwrap_or_else(|| panic!("{id} is not listed"))
}

/// One account holding one slot, and a `claude` stand-in that is just
/// `sleep 300`: every cap test below starts from this.
pub fn one_slot_sandbox() -> (tempfile::TempDir, Config, std::path::PathBuf) {
    let (tmp, mut cfg, stub) = hibernation_sandbox();
    std::fs::write(&stub, "#!/bin/sh\nprintf '> '\nexec sleep 300\n").unwrap();
    cfg.agent_onboarding = vogt_engine_server::claude_config::Onboarding {
        enabled: true,
        default_dir: Some(tmp.path().join("default-home")),
        settings_file: None,
        opencode_config: None,
        account_concurrency_cap: 1,
        default_account: None,
        accounts_root: None,
        accounts: vogt_engine_server::claude_config::parse_accounts(Some(&format!(
            "a={}",
            tmp.path().join("acct-a").display()
        ))),
        codex_policy: Ok(None),
    };
    (tmp, cfg, stub)
}

pub fn cap_client() -> reqwest::Client {
    reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap()
}

pub async fn start_on_account(
    client: &reqwest::Client,
    base: &str,
    stub: &std::path::Path,
    name: &str,
) -> reqwest::Response {
    client
        .post(format!("{base}/api/sessions"))
        .json(&json!({ "name": name, "command": [stub.to_string_lossy()], "account": "a" }))
        .send()
        .await
        .unwrap()
}

/// Start a session on the account, retrying `429` until `deadline`: a slot
/// is returned by an exit hook, which may trail the stop by a moment.
pub async fn start_when_free(
    client: &reqwest::Client,
    base: &str,
    stub: &std::path::Path,
    name: &str,
) -> Value {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let response = start_on_account(client, base, stub, name).await;
        if response.status() == StatusCode::OK {
            return response.json().await.unwrap();
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "{name} never got a slot: {}",
            response.status()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// A write-ahead record the previous engine left for a live session on
/// account `a`: the next boot recovers it, holding its slot, without waking it.
pub fn leave_live_record(state_dir: &std::path::Path, stub: &std::path::Path) -> uuid::Uuid {
    let lost = uuid::Uuid::new_v4();
    std::fs::create_dir_all(state_dir.join("sessions")).unwrap();
    std::fs::write(
        state_dir.join("sessions").join(format!("{lost}.json")),
        serde_json::to_vec(&json!({
            "version": 1,
            "id": lost,
            "name": "lost in a SIGKILL",
            "created_at": "2026-10-05T00:00:00Z",
            "command": [stub.to_string_lossy()],
            "account": "a",
            "conversation": { "agent": "claude", "id": lost },
        }))
        .unwrap(),
    )
    .unwrap();
    lost
}

/// Read frames until a text control frame of `kind` arrives; return it.
pub async fn next_control(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    kind: &str,
) -> Value {
    loop {
        let m = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .unwrap_or_else(|_| panic!("a {kind} frame arrives"))
            .unwrap()
            .unwrap();
        if let Message::Text(s) = m {
            let v: Value = serde_json::from_str(&s).unwrap();
            if v["type"].as_str() == Some(kind) {
                return v;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Only a person answers a permission prompt (WI-983)
// ---------------------------------------------------------------------------

/// A stand-in agent CLI showing one dialog: `question` over a numbered menu.
/// It prints `typed <bytes>` for anything that is not an arrow or Enter, and
/// `chose N` and exits on Enter, so a test can tell whether input reached it.
pub fn dialog_script(dir: &std::path::Path, name: &str, question: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::write(
        &path,
        format!(
            r#"import os, sys, tty
opts = ["Yes", "No"]
sel = 0
def draw():
    sys.stdout.write("\x1b[2J\x1b[H")
    sys.stdout.write("Bash command\r\n\r\n  rm -rf build\r\n\r\n{question}\r\n")
    for i, o in enumerate(opts):
        sys.stdout.write(("❯ " if i == sel else "  ") + f"{{i+1}}. {{o}}\r\n")
    sys.stdout.flush()
fd = sys.stdin.fileno()
tty.setraw(fd)
draw()
buf = b""
while True:
    buf += os.read(fd, 16)
    while buf:
        if buf.startswith(b"\x1b[B"):
            sel = min(sel + 1, len(opts) - 1); buf = buf[3:]; draw()
        elif buf.startswith(b"\x1b[A"):
            sel = max(sel - 1, 0); buf = buf[3:]; draw()
        elif buf.startswith(b"\r"):
            sys.stdout.write("\x1b[2J\x1b[H" + f"chose {{sel+1}}\r\n> ")
            sys.stdout.flush()
            os.read(fd, 1)
            sys.exit(0)
        elif buf.startswith(b"\x1b") and len(buf) < 3:
            break
        else:
            sys.stdout.write("\x1b[20;1Htyped " + repr(buf[:1]))
            sys.stdout.flush()
            buf = buf[1:]
"#
        ),
    )
    .unwrap();
    path
}

/// Start `script` as a session and wait until it reads as awaiting approval;
/// the dialog it shows, as the screen reports it.
pub async fn session_showing_dialog(
    client: &reqwest::Client,
    base: &str,
    name: &str,
    script: &std::path::Path,
) -> (String, Value) {
    let created: Value = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({ "name": name, "command": ["python3", script.to_string_lossy()] }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap().to_string();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let screen: Value = client
            .get(format!("{base}/api/sessions/{id}/screen"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if screen["activity"] == "awaiting-approval" {
            return (id, screen["approval"].clone());
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "never awaiting-approval: {screen}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

pub async fn wi983_front_door() -> (String, tempfile::TempDir, ServerGuard) {
    let core = stand_in_core_knowing(vec![
        (
            WI983_SESSION,
            "agent:session:ses_overseer",
            vec!["work.write"],
        ),
        (WI983_ENGINE, "agent:engine:3f2a", vec!["work.write"]),
        (WI983_POD, "agent:pod:vogt-dev", vec!["work.write"]),
        // An agent token holding `admin` is still an agent.
        (WI983_AGENT_ADMIN, "agent:vogt-sessions", vec!["admin"]),
        (WI983_PERSON, "human:ada", vec!["work.write"]),
    ])
    .await;
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = test_config();
    cfg.vogt_core_url = Some(core);
    cfg.vogt_core_token = Some(WI983_STACK.into());
    let (base, guard) = boot_with_config(cfg).await;
    (base, tmp, guard)
}

pub const WI983_STACK: &str = "stack-secret-for-wi983-1234567890";
pub const WI983_SESSION: &str = "session-token-wi983-1234567890";
pub const WI983_ENGINE: &str = "engine-token-wi983-1234567890";
pub const WI983_POD: &str = "pod-token-wi983-1234567890abcd";
pub const WI983_AGENT_ADMIN: &str = "agent-admin-wi983-1234567890";
pub const WI983_PERSON: &str = "person-token-wi983-1234567890";

pub fn client_for(token: &str) -> reqwest::Client {
    reqwest::Client::builder()
        .default_headers(auth_for(token))
        .build()
        .unwrap()
}

/// A permission dialog that closes on its answer and then reads one more
/// input from the terminal, so steering after the prompt has something to
/// land in (WI-4455).
pub fn dialog_then_steering_script(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::write(
        &path,
        r#"import os, sys, tty
fd = sys.stdin.fileno()
tty.setraw(fd)
def show(text):
    sys.stdout.write("\x1b[2J\x1b[H" + text.replace("\n", "\r\n"))
    sys.stdout.flush()
show("Bash command\n\n  rm -rf build\n\nDo you want to proceed?\n❯ 1. Yes\n  2. No\n")
while os.read(fd, 1) != b"\r":
    pass
show("chose 1\n")
line = os.read(fd, 64)
show("steered " + repr(line) + "\n")
"#,
    )
    .unwrap();
    path
}

/// What the stand-in chat provider will answer, in order, and every
/// request body it was sent.
#[derive(Default)]
pub struct ChatStub {
    pub replies: Vec<Value>,
    pub requests: Vec<Value>,
}

pub type ChatScript = Arc<Mutex<ChatStub>>;

/// A stand-in chat provider that answers `/chat/completions` with each
/// message pushed onto the returned script in turn (a `choices[0].message`),
/// then a plain "done".
pub async fn stand_in_chat() -> (String, ChatScript) {
    use axum::{extract::State, routing::post, Router};
    async fn chat(
        State(script): State<ChatScript>,
        axum::Json(body): axum::Json<Value>,
    ) -> axum::Json<Value> {
        let message = {
            let mut script = script.lock().unwrap();
            script.requests.push(body);
            if script.replies.is_empty() {
                json!({"role": "assistant", "content": "done"})
            } else {
                script.replies.remove(0)
            }
        };
        axum::Json(json!({"choices": [{"message": message}]}))
    }
    let script: ChatScript = Arc::default();
    let app = Router::new()
        .route("/chat/completions", post(chat))
        .with_state(Arc::clone(&script));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), script)
}

/// Ask the assistant, as `client`, to type "1" and Enter into `session`,
/// then approve the card it proposes. Returns the approval's response.
pub async fn approve_typing_one(
    client: &reqwest::Client,
    base: &str,
    script: &ChatScript,
    session: &str,
) -> reqwest::Response {
    script.lock().unwrap().replies.extend([
        json!({"role": "assistant", "content": null, "tool_calls": [{
            "id": "call_1",
            "type": "function",
            "function": {
                "name": "vogt_session_input",
                "arguments": json!({
                    "id": session,
                    "text": "1",
                    "submit": true,
                    "reason": "the session is asking and the caller chose 1",
                })
                .to_string(),
            },
        }]}),
        json!({"role": "assistant", "content": "turn over"}),
    ]);
    let proposed = client
        .post(format!("{base}/api/assistant/message"))
        .json(&json!({ "text": "type 1 and Enter into the asking session" }))
        .send()
        .await
        .unwrap();
    assert_eq!(proposed.status(), StatusCode::OK);
    let proposed: Value = proposed.json().await.unwrap();
    let card = &proposed["pending_action"];
    assert_eq!(card["kind"], "vogt_write", "{proposed}");
    assert_eq!(card["operation"], "session.input", "{proposed}");
    let card = card["id"].as_str().unwrap();
    client
        .post(format!("{base}/api/assistant/actions/{card}"))
        .json(&json!({ "approve": true }))
        .send()
        .await
        .unwrap()
}

// ── Quick chats (WI-1097) ────────────────────────────────────────────────

/// A test engine whose chats are driven by the scripted app-server stand-in
/// (`tests/fixtures/fake-codex-app-server.py`), reached through a symlink
/// named `codex` so the engine recognises the driver. `None` without python3.
pub type ChatEngine = (
    String,
    reqwest::Client,
    Arc<AppState>,
    ServerGuard,
    tempfile::TempDir,
);

pub async fn boot_chats() -> Option<ChatEngine> {
    boot_chats_with(|_| {}).await
}

pub async fn boot_chats_with(
    tune: impl FnOnce(&mut vogt_engine_server::chats::ChatPolicy),
) -> Option<ChatEngine> {
    if std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("skipping: the stand-in driver is written in python3");
        return None;
    }
    let bin = tempfile::tempdir().unwrap();
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/fake-codex-app-server.py")
        .canonicalize()
        .unwrap();
    let driver = bin.path().join("codex");
    std::os::unix::fs::symlink(&fixture, &driver).unwrap();
    let mut cfg = Config {
        chat: vogt_engine_server::chats::ChatPolicy {
            enabled: true,
            command: Some(vec![driver.to_string_lossy().into_owned()]),
            models: vec![
                vogt_engine_contract::ChatModel {
                    id: "grok-4.7".into(),
                    label: "Grok 4.7".into(),
                },
                vogt_engine_contract::ChatModel {
                    id: "deepseek-v4-pro".into(),
                    label: "DeepSeek V4 Pro".into(),
                },
            ],
            approval_timeout: Duration::from_secs(10),
            ..Default::default()
        },
        session_templates: vec![],
        ..test_config()
    };
    tune(&mut cfg.chat);
    let (base, state, guard) = boot_with_state(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    Some((base, client, state, guard, bin))
}

pub async fn chat_send(client: &reqwest::Client, base: &str, id: &str, text: &str) -> Value {
    let sent = client
        .post(format!("{base}/api/chats/{id}/messages"))
        .json(&json!({ "text": text, "wait_secs": 20 }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        sent.status(),
        StatusCode::OK,
        "{}",
        sent.text().await.unwrap()
    );
    sent.json().await.unwrap()
}

/// Wait until the chat shows a pending approval, and return it.
pub async fn pending_approval(client: &reqwest::Client, base: &str, id: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let chat: Value = client
                .get(format!("{base}/api/chats/{id}"))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            if let Some(first) = chat["approvals"].as_array().and_then(|a| a.first()) {
                break first.clone();
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("an approval should be raised")
}

pub fn texts(entries: &Value, kind: &str) -> Vec<String> {
    entries
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == kind)
        .map(|e| e["text"].as_str().unwrap_or("").to_string())
        .collect()
}

pub async fn runtime_stop(runtime: &Arc<vogt_engine_server::chats::ChatRuntime>, id: &str) {
    runtime
        .stop_for_test(uuid::Uuid::parse_str(id).unwrap())
        .await;
}

pub async fn wait_chat_idle(client: &reqwest::Client, base: &str, id: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let chat: Value = client
                .get(format!("{base}/api/chats/{id}"))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            if chat["state"] == "idle" {
                break chat;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the chat should go idle")
}

pub const WI1255_STACK: &str = "stack-secret-for-wi1255-1234567890";
pub const WI1255_WORKER: &str = "worker-token-wi1255-1234567890";
pub const WI1255_OVERSEER: &str = "overseer-token-wi1255-1234567890";
pub const WI1255_STRANGER: &str = "stranger-token-wi1255-1234567890";
pub const WI1255_PERSON: &str = "person-token-wi1255-1234567890";

/// A front door for the WI-1255 git-op tests: a repo whose main checkout two
/// sessions were launched in (the pool-worker shape), a linked worktree the
/// worker's work is staged in, and tokens for the worker, an oversight
/// session, an agent with no session, a person and vogt-core.
pub struct GitOpDoor {
    pub base: String,
    pub repo: std::path::PathBuf,
    pub work: std::path::PathBuf,
    pub worker: String,
    pub overseer: String,
    pub _tmp: tempfile::TempDir,
    pub _guard: ServerGuard,
}

pub async fn wi1255_front_door() -> GitOpDoor {
    let core = stand_in_core_knowing(vec![
        (
            WI1255_WORKER,
            "agent:session:ses_worker",
            vec!["work.write"],
        ),
        (
            WI1255_OVERSEER,
            "agent:session:ses_overseer",
            vec!["work.write"],
        ),
        (WI1255_STRANGER, "agent:vogt-sessions", vec!["admin"]),
        (WI1255_PERSON, "human:ada", vec!["work.write"]),
    ])
    .await;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let repo = root.join("repo");
    let work = root.join("repo-work");
    std::fs::create_dir(&repo).unwrap();
    let sh = |cmd: &str| {
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .current_dir(&repo)
            .output()
            .unwrap();
        assert!(out.status.success(), "{cmd}: {out:?}");
    };
    sh("git init -q -b main");
    sh("git config user.email t@t");
    sh("git config user.name t");
    std::fs::write(repo.join("a.txt"), "one\n").unwrap();
    sh("git add a.txt && git commit -q -m init");
    sh(&format!("git worktree add -q -b work {}", work.display()));

    let mut cfg = test_config();
    cfg.workspace_root = root.clone();
    cfg.default_cwd = repo.clone();
    cfg.vogt_core_url = Some(core);
    cfg.vogt_core_token = Some(WI1255_STACK.into());
    let (base, guard) = boot_with_config(cfg).await;

    let operator = client_for(TEST_TOKEN);
    let mut ids = Vec::new();
    for (name, vogt_id, role) in [
        ("worker", "ses_worker", "worker"),
        ("overseer", "ses_overseer", "oversight"),
    ] {
        let created: Value = operator
            .post(format!("{base}/api/sessions"))
            .json(&json!({
                "name": name,
                "role": role,
                "cwd": repo.to_string_lossy(),
                "env": [["VOGT_SESSION_ID", vogt_id]],
                "command": ["/bin/bash", "-c", "sleep 300"],
            }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        ids.push(created["id"].as_str().unwrap().to_string());
    }
    // A commit for a session needs it quiet, never mid-turn.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while session_row(&operator, &base, &ids[0]).await["activity"] != "idle" {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the worker never idled"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let overseer = ids.pop().unwrap();
    let worker = ids.pop().unwrap();
    GitOpDoor {
        base,
        repo,
        work,
        worker,
        overseer,
        _tmp: tmp,
        _guard: guard,
    }
}

impl GitOpDoor {
    /// Leave a change staged in the worker's linked worktree.
    pub fn stage_work(&self, file: &str) {
        std::fs::write(self.work.join(file), "work\n").unwrap();
        let out = std::process::Command::new("git")
            .args(["add", file])
            .current_dir(&self.work)
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
    }

    pub fn commit_count(&self, dir: &std::path::Path) -> usize {
        let out = std::process::Command::new("git")
            .args(["rev-list", "--count", "HEAD"])
            .current_dir(dir)
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().parse().unwrap()
    }

    pub async fn git_op(&self, token: &str, body: Value) -> (StatusCode, Value) {
        let res = client_for(token)
            .post(format!("{}/api/git/op", self.base))
            .json(&body)
            .send()
            .await
            .unwrap();
        let status = res.status();
        (status, res.json().await.unwrap_or(Value::Null))
    }

    pub fn commit_for_worker(&self, worktree: bool) -> Value {
        let mut body = json!({
            "op": "commit",
            "session": self.worker,
            "message": "worker change",
        });
        if worktree {
            body["worktree"] = json!(self.work.to_string_lossy());
        }
        body
    }
}

/// git in `dir` for the worktree tests, failing the test if git does.
pub fn worktree_git(dir: &std::path::Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A committed checkout at `repo` with a dirty edit, as the shared checkout is,
/// whose local `main` lags `origin/main` (WI-1630). The bare origin is a
/// sibling of the checkout, so the session start's fetch stays on disk.
pub fn worktree_checkout(repo: &std::path::Path) {
    let origin = repo.parent().unwrap().join("origin.git");
    worktree_git(
        repo.parent().unwrap(),
        &[
            "init",
            "--bare",
            "-q",
            "-b",
            "main",
            &origin.display().to_string(),
        ],
    );
    worktree_git(
        repo.parent().unwrap(),
        &[
            "clone",
            "-q",
            &origin.display().to_string(),
            &repo.display().to_string(),
        ],
    );
    worktree_git(repo, &["config", "user.email", "t@example"]);
    worktree_git(repo, &["config", "user.name", "t"]);
    std::fs::write(repo.join("f.txt"), "base\n").unwrap();
    worktree_git(repo, &["add", "f.txt"]);
    worktree_git(repo, &["commit", "-q", "-m", "base"]);
    worktree_git(repo, &["push", "-q", "origin", "main"]);
    std::fs::write(repo.join("f.txt"), "remote\n").unwrap();
    worktree_git(repo, &["add", "f.txt"]);
    worktree_git(repo, &["commit", "-q", "-m", "remote"]);
    worktree_git(repo, &["push", "-q", "origin", "main"]);
    worktree_git(repo, &["reset", "--hard", "HEAD~1"]);
    std::fs::write(repo.join("f.txt"), "dirty\n").unwrap();
}

pub fn has_branch(repo: &std::path::Path, branch: &str) -> bool {
    std::process::Command::new("git")
        .args([
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ])
        .current_dir(repo)
        .status()
        .unwrap()
        .success()
}

/// A stand-in Claude that takes its next turn from a file rather than the
/// PTY: each line appended to `turns.feed` starts one turn. An agent that
/// prompts itself leaves no person-input stamp, and writing the PTY from the
/// test would — the engine's own token counts as a person, which resets the
/// idle clock the budget stop waits on.
pub fn file_fed_claude(dir: &std::path::Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let stub = bin.join("claude");
    std::fs::write(dir.join("turns.feed"), "").unwrap();
    std::fs::write(
        &stub,
        format!(
            r#"#!/usr/bin/env bash
ready() {{
  printf '\033]0;claude: ready\007\033[2J\033[H'
  printf '╭──────────────────────╮\r\n│ › Ask the model…     │\r\n╰──────────────────────╯\r\n  model · stand-in\r\n'
}}
ready
n=0
seen=0
while true; do
  lines=$(wc -l < '{feed}')
  if [ "$lines" -gt "$seen" ]; then
    seen=$lines
    printf '\033]0;claude: working\007'
    printf '  ⣟  thinking…  (esc to interrupt)\r\n'; sleep 0.3
    n=$((n+1))
    printf '{{"type":"assistant","timestamp":"2026-10-13T00:%02d:00Z","message":{{"content":"turn %s"}}}}\n' "$n" "$n" >> "$(cat '{at}')"
    echo "$n" >> '{log}'
    ready
  fi
  sleep 0.1
done
"#,
            at = dir.join("transcript_at").display(),
            log = dir.join("turns.log").display(),
            feed = dir.join("turns.feed").display(),
        ),
    )
    .unwrap();
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    stub
}

/// A stand-in Claude TUI for the budget tests (WI-1281). It draws its idle
/// input box at rest, and per line of input runs one turn: about 1.5 s of `(esc to
/// interrupt)` spinner output (so the engine reads it running), then one
/// assistant line appended to the transcript named in `transcript_at` and a
/// line to `turns.log`. Nothing types into it but autopilot.
pub fn fake_claude_tui(dir: &std::path::Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let stub = bin.join("claude");
    std::fs::write(
        &stub,
        format!(
            r#"#!/usr/bin/env bash
ready() {{
  printf '\033]0;claude: ready\007\033[2J\033[H'
  printf '╭──────────────────────╮\r\n│ › Ask the model…     │\r\n╰──────────────────────╯\r\n  model · stand-in\r\n'
}}
ready
n=0
while IFS= read -r _; do
  printf '\033]0;claude: working\007'
  for _ in 1 2 3 4 5 6; do printf '  ⣟  thinking…  (esc to interrupt)\r\n'; sleep 0.25; done
  n=$((n+1))
  printf '{{"type":"assistant","timestamp":"2026-10-13T00:%02d:00Z","message":{{"usage":{{"input_tokens":200,"output_tokens":5}},"content":"turn %s"}}}}\n' "$n" "$n" >> "$(cat '{at}')"
  echo "$n" >> '{log}'
  ready
done
"#,
            at = dir.join("transcript_at").display(),
            log = dir.join("turns.log").display(),
        ),
    )
    .unwrap();
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    stub
}

/// A config for the budget tests: the economics tailer reads the sandbox's
/// `.claude`, autopilot re-prompts quickly, and the budget watcher ticks
/// slower than autopilot — the race an autopilot worker won in production,
/// where it was re-prompted before the watcher ever saw it at rest.
pub fn budget_sandbox(dir: &std::path::Path) -> Config {
    let mut cfg = test_config();
    cfg.default_cwd = dir.to_path_buf();
    cfg.workspace_root = dir.canonicalize().unwrap();
    cfg.state_dir = dir.join("state");
    cfg.economics_home = Some(dir.to_path_buf());
    cfg.economics_tick = Duration::from_millis(200);
    cfg.autopilot = vogt_engine_server::autopilot::Policy {
        nudge_after: Duration::from_millis(600),
        max_nudges: 40,
    };
    cfg.budget = vogt_engine_server::budget::Policy {
        soft_pct: 100,
        tick: Duration::from_millis(1_000),
        enabled: true,
    };
    std::fs::create_dir_all(dir.join(".claude/projects/k")).unwrap();
    cfg
}

pub async fn wait_hibernated(client: &reqwest::Client, base: &str, id: &str, secs: u64) -> Value {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    loop {
        let row = session_row(client, base, id).await;
        if row.get("hibernation").is_some_and(|h| !h.is_null()) {
            return row;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "never hibernated: {row}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

pub fn turns_run(dir: &std::path::Path) -> usize {
    std::fs::read_to_string(dir.join("turns.log"))
        .unwrap_or_default()
        .lines()
        .count()
}

/// Spawn a stub agent that prints `body`, then rests. The summary is read
/// from its own output, exactly as the overseer table and digest do.
pub async fn summary_of_a_stub_that_prints(body: &str) -> vogt_engine_contract::SessionSummary {
    let tmp = tempfile::tempdir().unwrap();
    let script = tmp.path().join("agent");
    std::fs::write(&script, body).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let spec = vogt_engine_contract::SessionSpec {
        name: "provider-error".into(),
        command: Some(vec![script.to_string_lossy().into_owned()]),
        ..Default::default()
    };
    let spawned = vogt_engine_server::pty::spawn(
        Uuid::new_v4(),
        &spec,
        vogt_engine_server::pty::SpawnDefaults {
            default_shell: "/bin/sh",
            auto_agent_auth: false,
            agent_auth_helper: std::path::Path::new("/usr/local/bin/vogt-agent-auth"),
            default_cwd: tmp.path(),
            scrollback_bytes: 64 * 1024,
            activity_idle_after_ms: 200,
            secret_broker: None,
            agent_auth_manifest: None,
            on_exit: None,
        },
        vogt_engine_server::events::EventBus::new(64),
        None,
    )
    .unwrap();
    let session = spawned.session;
    tokio::time::sleep(Duration::from_millis(800)).await;
    let summary = session.summary();
    session.kill().unwrap();
    summary
}

/// Spawn `body` as an executable named `bin` (the name decides whether the
/// engine takes it for an agent CLI) in a PTY.
pub fn spawn_osc_stub(
    bin: &str,
    body: &str,
) -> (
    std::sync::Arc<vogt_engine_server::pty::Session>,
    tempfile::TempDir,
) {
    let tmp = tempfile::tempdir().unwrap();
    let script = tmp.path().join(bin);
    std::fs::write(&script, body).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let spec = vogt_engine_contract::SessionSpec {
        name: "osc7501".into(),
        command: Some(vec![script.to_string_lossy().into_owned()]),
        ..Default::default()
    };
    let spawned = vogt_engine_server::pty::spawn(
        Uuid::new_v4(),
        &spec,
        vogt_engine_server::pty::SpawnDefaults {
            default_shell: "/bin/sh",
            auto_agent_auth: false,
            agent_auth_helper: std::path::Path::new("/usr/local/bin/vogt-agent-auth"),
            default_cwd: tmp.path(),
            scrollback_bytes: 64 * 1024,
            activity_idle_after_ms: 200,
            secret_broker: None,
            agent_auth_manifest: None,
            on_exit: None,
        },
        vogt_engine_server::events::EventBus::new(64),
        None,
    )
    .unwrap();
    (spawned.session, tmp)
}

/// Wait (bounded) until the session's ring holds `needle`.
pub async fn ring_has(session: &vogt_engine_server::pty::Session, needle: &str) -> String {
    let mut ring = String::new();
    for _ in 0..100 {
        ring = String::from_utf8_lossy(&session.tail(8192)).into_owned();
        if ring.contains(needle) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    ring
}

/// Spawn a fake agent named `codex` in its own PTY, the same shape a pod
/// session runs: a TUI that draws a composer and announces `codex: ready`.
pub fn spawn_fake_codex(
    dir: &std::path::Path,
    body: &str,
    bus: &vogt_engine_server::events::EventBus,
) -> Arc<vogt_engine_server::pty::Session> {
    spawn_fake_agent(dir, "codex", body, bus)
}

pub fn spawn_fake_agent(
    dir: &std::path::Path,
    name: &str,
    body: &str,
    bus: &vogt_engine_server::events::EventBus,
) -> Arc<vogt_engine_server::pty::Session> {
    let codex = dir.join(name);
    std::fs::write(&codex, body).unwrap();
    std::fs::set_permissions(&codex, std::fs::Permissions::from_mode(0o755)).unwrap();
    let spec = vogt_engine_contract::SessionSpec {
        name: "fake-codex".into(),
        command: Some(vec![codex.to_string_lossy().into_owned()]),
        prompt: Some("# task\n\nRun it.\n".into()),
        ..Default::default()
    };
    vogt_engine_server::pty::spawn(
        Uuid::new_v4(),
        &spec,
        vogt_engine_server::pty::SpawnDefaults {
            default_shell: "/bin/sh",
            auto_agent_auth: false,
            agent_auth_helper: std::path::Path::new("/usr/local/bin/vogt-agent-auth"),
            default_cwd: dir,
            scrollback_bytes: 64 * 1024,
            activity_idle_after_ms: 200,
            secret_broker: None,
            agent_auth_manifest: None,
            on_exit: None,
        },
        bus.clone(),
        None,
    )
    .unwrap()
    .session
}

pub fn first_turn_policy() -> vogt_engine_server::first_turn::Policy {
    vogt_engine_server::first_turn::Policy {
        ready_within: Duration::from_secs(5),
        turn_within: Duration::from_millis(500),
    }
}

/// Start a briefed agent session through `POST /api/sessions`, the path that
/// decides in `sessions.rs` whether the first-turn watchdog runs. The stub is
/// named `agent` in a directory of its own, which is how the engine tells an
/// agent CLI.
pub async fn start_briefed_agent(
    client: &reqwest::Client,
    base: &str,
    dir: &std::path::Path,
    agent: &str,
    script: &str,
    autopilot: bool,
) -> Uuid {
    std::fs::create_dir_all(dir).unwrap();
    let stub = dir.join(agent);
    std::fs::write(&stub, script).unwrap();
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    let res = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": format!("first-turn-{agent}"),
            "command": [stub.to_string_lossy()],
            "prompt": "# task\n\nRun `echo hello`.\n",
            "autopilot": autopilot,
        }))
        .send()
        .await
        .unwrap();
    let status = res.status();
    let created: Value = res.json().await.unwrap_or(Value::Null);
    assert_eq!(status, StatusCode::OK, "{created}");
    created["id"].as_str().unwrap().parse().unwrap()
}

pub fn first_turn_config() -> Config {
    Config {
        first_turn: first_turn_policy(),
        ..test_config()
    }
}

pub fn screen_text(session: &vogt_engine_server::pty::Session) -> String {
    session.render_screen(0).0.lines.join("\n")
}

/// What a stub that logs every line it reads for three seconds read: `[]` is
/// a bare Enter, anything else was typed text.
pub const LOG_READS: &str = "while read -t 3 -r line; do printf 'READ:[%s]\\n' \"$line\"; done\n\
                         printf 'READ-WINDOW-OVER\\n'\n\
                         exec sleep 30\n";

pub async fn reads_of(session: &vogt_engine_server::pty::Session) -> Vec<String> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !screen_text(session).contains("READ-WINDOW-OVER") {
        if tokio::time::Instant::now() >= deadline {
            // Killed first, so the failure reports instead of hanging on the child.
            let screen = screen_text(session);
            session.kill().unwrap();
            panic!("the stub never finished reading: {screen}");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    screen_text(session)
        .lines()
        .filter(|l| l.starts_with("READ:"))
        .map(str::to_string)
        .collect()
}

pub fn provider_request(name: &str, env_key: &str) -> Value {
    provider_request_with_secret(name, env_key, "PROVIDER_KEY")
}

pub fn provider_request_with_secret(name: &str, env_key: &str, secret_name: &str) -> Value {
    json!({
        "name": name,
        "base_url": "https://provider.example.test/v1",
        "wire_api": "chat",
        "env_key": env_key,
        "secret_project": "approved-project",
        "secret_name": secret_name,
        "models": ["test-model"]
    })
}

pub fn model_test_template(name: &str, agent: &str) -> SessionTemplate {
    SessionTemplate {
        name: name.into(),
        description: format!("pod-backed {agent}"),
        command: Some(vec![agent.into()]),
        cwd: None,
        env: vec![],
        backend: "pod".into(),
        host_capable: false,
        pod_capable: false,
        account: None,
        default_name: None,
        match_repo_names: vec![],
        match_path_prefixes: vec![],
        tags: vec![],
    }
}

pub fn write_model_registry(path: &std::path::Path) {
    std::fs::write(
        path,
        r#"
[[provider]]
name = "litellm-deepseek"
base_url = "http://litellm:4000/v1"
wire_api = "chat"
env_key = "DEEPSEEK_PROVIDER_KEY"
secret_project = "proj-a"
secret_name = "DEEPSEEK_SECRET"
models = ["deepseek/deepseek-v4-flash"]

[[provider]]
name = "litellm-grok"
base_url = "http://litellm:4000/v1"
wire_api = "chat"
env_key = "GROK_PROVIDER_KEY"
secret_project = "proj-a"
secret_name = "GROK_SECRET"
models = ["grok-4.7", "grok-4.6"]

[[provider]]
name = "litellm-claude"
base_url = "http://litellm:4000/v1"
wire_api = "chat"
env_key = "CLAUDE_PROVIDER_KEY"
secret_project = "proj-a"
secret_name = "CLAUDE_SECRET"
models = ["claude-org-haiku", "claude-org-sonnet", "claude-org-opus"]
"#,
    )
    .unwrap();
}

pub fn model_test_helper(path: &std::path::Path) {
    std::fs::write(
        path,
        "#!/bin/sh\n[ \"$1\" = get ] || exit 64\nprintf 'value-for-%s' \"$2\"\n",
    )
    .unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

pub fn model_test_manifest() -> Vec<vogt_engine_server::secret_broker::ManifestSecret> {
    vogt_engine_server::secret_broker::parse_manifest(
        "MANIFEST_DEEPSEEK proj-a DEEPSEEK_SECRET optional\n\
         MANIFEST_GROK proj-a GROK_SECRET optional\n\
         MANIFEST_CLAUDE proj-a CLAUDE_SECRET optional",
    )
    .unwrap()
}

/// Wait (bounded) until the session's activity is `want`; returns the last one seen.
pub async fn activity_reaches(
    session: &vogt_engine_server::pty::Session,
    want: vogt_engine_contract::ActivityState,
) -> vogt_engine_contract::ActivityState {
    for _ in 0..60 {
        if session.activity() == want {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    session.activity()
}

/// WI-1314 helpers: the session's blocked report, and waiting on the engine
/// to settle a session at rest.
pub async fn blocked_report(client: &reqwest::Client, base: &str, id: &str) -> Value {
    let summary: Value = client
        .get(format!("{base}/api/sessions/{id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    summary["summary"]
        .get("blocked")
        .cloned()
        .unwrap_or(Value::Null)
}

pub async fn report_blocked(client: &reqwest::Client, base: &str, id: &str) {
    let resp = client
        .post(format!("{base}/api/sessions/{id}/blocked"))
        .json(&json!({ "blocked": true, "reason": "push refused", "items": ["approve the push"] }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

pub async fn wait_for_activity(client: &reqwest::Client, base: &str, id: &str, want: &[&str]) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let screen: Value = client
            .get(format!("{base}/api/sessions/{id}/screen"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if want.iter().any(|w| screen["activity"] == *w) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "never {want:?}: {screen}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

pub async fn wait_for_unblocked(client: &reqwest::Client, base: &str, id: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let blocked = blocked_report(client, base, id).await;
        if blocked.is_null() {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "still blocked: {blocked}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// A shell session that prints a prompt, then on each line of input (or
/// each time `go` appears in `dir`) prints `what`'s output as one turn.
pub fn turn_script(body: &str) -> Value {
    json!([
        "/bin/sh",
        "-c",
        format!("printf 'ready> '; {body}; sleep 60")
    ])
}

pub const RESTING: &[&str] = &["idle", "waiting-for-input"];

/// WI-1576: a sandbox whose economics tailer reads Codex rollouts from
/// `<tmp>/.codex/sessions`, and the rollout file for `conversation`, holding
/// only its `session_meta` head.
pub fn codex_rollout_sandbox(
    conversation: &str,
    autopilot: vogt_engine_server::autopilot::Policy,
) -> (tempfile::TempDir, Config, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = test_config();
    cfg.economics_home = Some(tmp.path().to_path_buf());
    cfg.economics_tick = Duration::from_millis(300);
    cfg.autopilot = autopilot;
    let rollout = tmp
        .path()
        .join(".codex/sessions/2026/10/31")
        .join(format!("rollout-2026-10-31T00-00-00-{conversation}.jsonl"));
    std::fs::create_dir_all(rollout.parent().unwrap()).unwrap();
    std::fs::write(
        &rollout,
        format!(
            "{}\n",
            json!({"type": "session_meta", "payload": {"id": conversation, "cwd": "/tmp"}})
        ),
    )
    .unwrap();
    (tmp, cfg, rollout)
}

/// Append Codex `event_msg` entries to a rollout, the way Codex persists them.
pub fn append_rollout(rollout: &std::path::Path, payloads: &[Value]) {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(rollout)
        .unwrap();
    for payload in payloads {
        writeln!(
            file,
            "{}",
            json!({"timestamp": "2026-10-31T00:00:00Z", "type": "event_msg", "payload": payload})
        )
        .unwrap();
    }
}

/// The changes of `kind` for one session, from the start of the feed.
pub async fn changes_of(client: &reqwest::Client, base: &str, id: &str, kind: &str) -> Vec<Value> {
    let feed: Value = client
        .get(format!(
            "{base}/api/sessions/changes?since=0&ids={id}&limit=500"
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    feed["changes"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["kind"] == kind)
        .cloned()
        .collect()
}

/// Poll until the session has `want` changes of `kind`.
pub async fn await_changes(
    client: &reqwest::Client,
    base: &str,
    id: &str,
    kind: &str,
    want: usize,
) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        if changes_of(client, base, id, kind).await.len() >= want {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "session {id} never reached {want} {kind} change(s)"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

// ---- gh#1460: brokered GitHub tokens are validated before they are handed out ----

/// A fake `GET /user` that answers `status` and counts its hits.
pub async fn fake_github_api(
    status: u16,
) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let hits = std::sync::Arc::new(AtomicUsize::new(0));
    let seen = std::sync::Arc::clone(&hits);
    let app = axum::Router::new().route(
        "/user",
        axum::routing::get(move || {
            let seen = std::sync::Arc::clone(&seen);
            async move {
                seen.fetch_add(1, Ordering::SeqCst);
                axum::http::StatusCode::from_u16(status).unwrap()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}"), hits)
}

pub async fn github_broker_session(
    api: &str,
) -> (
    String,
    String,
    std::sync::Arc<vogt_engine_server::app::AppState>,
    (tempfile::TempDir, ServerGuard),
) {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = broker_config(&tmp, "GH_TOKEN vogt-dev-llm GH_TOKEN_SRC ondemand\n");
    let (base, state, guard) = boot_with_state(cfg).await;
    state
        .sessions
        .secret_broker()
        .set_github_check(vogt_engine_server::github_token_check::GithubTokenCheck::for_tests(api));
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let id = create_session_id(&client, &base, "github-broker").await;
    let token = state
        .sessions
        .secret_broker()
        .issue(id.parse::<uuid::Uuid>().unwrap());
    (base, token, state, (tmp, guard))
}

/// The launch wrapper's own path: a stale value is not exported and the
/// session is told why; a valid one is exported. The real `agent-auth.sh`
/// against the real engine route, with only the Infisical read faked.
pub async fn launch_with_github_token(api: &str) -> String {
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../deploy/agent-auth.sh");
    let (base, _token, _state, tmp) = github_broker_session(api).await;
    let _keep = tmp;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let launch = format!(
        "source '{}'; DEGRADE_ON_UNAVAILABLE=1; \
         resolve_secret() {{ printf -v \"$1\" '%s' ghp_launchvalue; PROJECT_OUTCOME[$3]=ok; }}; \
         ENGINE_AGENT_AUTH_SECRETS='GH_TOKEN vogt-dev-llm GH_TOKEN_SRC'; \
         load_manifest_secrets tok 2>/dev/null; \
         echo \"launch gh=[${{GH_TOKEN:-}}] why=[$AGENT_AUTH_UNAVAILABLE_REASON]\"; sleep 30",
        script.display()
    );
    let id = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "launch-gh",
            "command": ["/bin/bash", "-c", launch],
            "env": [["VOGT_ENGINE_BROKER_URL", base]],
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
    live_output_containing(&client, &base, &id, &["launch gh="]).await
}

/// A fake `GET /user` that rejects exactly the bearer `reject` and accepts
/// every other.
pub async fn fake_github_rejecting(reject: &'static str) -> String {
    let app = axum::Router::new().route(
        "/user",
        axum::routing::get(move |headers: axum::http::HeaderMap| async move {
            let bearer = headers
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default();
            if bearer == format!("Bearer {reject}") {
                axum::http::StatusCode::UNAUTHORIZED
            } else {
                axum::http::StatusCode::OK
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}")
}

pub async fn ask_check(base: &str, token: &str, body: &'static str) -> Value {
    let r = reqwest::Client::new()
        .post(format!("{base}/api/agent-auth/github-check/GH_TOKEN"))
        .bearer_auth(token)
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    r.json().await.unwrap()
}
