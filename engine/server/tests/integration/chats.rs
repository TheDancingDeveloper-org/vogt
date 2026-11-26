use super::*;

/// WI-1355: a Codex MCP tool approval is a person's to answer. The fork
/// (codex-real 0.162.0+fork.ca04973b4, PTY bytes captured off it) asks
/// "Allow the echo MCP server to run tool …?" in its elicitation form; prod
/// saw it with approval null, activity idle and an agent's typed prompt
/// answering it. Replayed through a real session, the engine must report the
/// approval, hand a wait back as needing a person, and refuse agent input —
/// whether the title says `awaiting approval` or, as on prod, still `working`.
#[tokio::test]
async fn a_codex_mcp_tool_approval_needs_a_person() {
    let (base, tmp, _guard) = wi983_front_door().await;
    let operator = client_for(TEST_TOKEN);
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/codex-approvals/mcp-120.pty");
    let script = tmp.path().join("replay.py");
    std::fs::write(
        &script,
        r#"import os, sys, tty
fixture, title, typed = sys.argv[1:4]
data = open(fixture, "rb").read()
data = data.replace(b"codex: awaiting approval", title.encode())
fd = sys.stdin.fileno()
tty.setraw(fd)
os.write(sys.stdout.fileno(), data)
while True:
    got = os.read(fd, 64)
    if not got:
        break
    with open(typed, "ab") as f:
        f.write(got)
"#,
    )
    .unwrap();

    for title in ["codex: awaiting approval", "codex: working"] {
        let typed = tmp.path().join(format!("typed-{}", title.len()));
        let created: Value = operator
            .post(format!("{base}/api/sessions"))
            .json(&json!({
                "name": "codex-mcp",
                "cols": 120,
                "rows": 40,
                "command": [
                    "python3",
                    script.to_string_lossy(),
                    fixture.to_string_lossy(),
                    title,
                    typed.to_string_lossy(),
                ],
            }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let id = created["id"].as_str().unwrap().to_string();

        // A wait comes back as soon as the dialog shows, not at its timeout.
        let waited: Value = operator
            .get(format!(
                "{base}/api/sessions/{id}/wait?until=ready&timeout_s=20"
            ))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(waited["outcome"], "awaiting-approval", "{title}: {waited}");
        let screen = &waited["screen"];
        assert_eq!(screen["activity"], "awaiting-approval", "{title}: {screen}");
        assert_eq!(screen["ready"], false, "{title}: {screen}");
        let approval = &screen["approval"];
        assert_eq!(approval["kind"], "permission", "{title}: {approval}");
        assert_eq!(
            approval["question"], "Allow the echo MCP server to run tool \"echo\"?",
            "{title}: read off the screen, not the title: {approval}"
        );
        let labels: Vec<&str> = approval["options"]
            .as_array()
            .unwrap()
            .iter()
            .map(|o| o["label"].as_str().unwrap())
            .collect();
        assert_eq!(
            labels,
            ["Allow", "Allow for this session", "Always allow", "Cancel"],
            "{title}"
        );

        // An agent's prompt that happens to start with a digit, a bare
        // option number and Enter are each an answer to the form: refused.
        let agent = client_for(WI983_SESSION);
        for text in ["Gitguard test (1) run board_list", "1", "\r"] {
            let sent = agent
                .post(format!("{base}/api/sessions/{id}/input"))
                .json(&json!({ "text": text, "submit": true, "person": true }))
                .send()
                .await
                .unwrap();
            assert_eq!(sent.status(), StatusCode::FORBIDDEN, "{title} {text:?}");
            let said: Value = sent.json().await.unwrap();
            assert!(
                said["error"].as_str().unwrap().contains("person required"),
                "{said}"
            );
        }
        let answered = agent
            .post(format!("{base}/api/sessions/{id}/answer"))
            .json(&json!({ "option": 1, "person": true }))
            .send()
            .await
            .unwrap();
        assert_eq!(answered.status(), StatusCode::FORBIDDEN, "{title}");

        tokio::time::sleep(Duration::from_millis(300)).await;
        let typed = std::fs::read(&typed).unwrap_or_default();
        assert!(
            !typed.iter().any(|b| matches!(b, b'1' | b'\r' | b'G')),
            "{title}: agent keys reached the form: {typed:?}"
        );
        let _ = operator
            .post(format!("{base}/api/sessions/{id}/kill"))
            .send()
            .await;
    }
}

/// WI-1641: on the codex driver the first message must wait for the thread.
/// The stand-in (`tests/fixtures/fake-codex-app-server.py`) answers
/// `thread/start` late and refuses a `turn/start` for a thread it has not
/// started, as the fork does; a chat that sends the turn early dies with
/// `thread not found` instead of answering.
#[tokio::test]
async fn a_codex_chat_holds_its_first_turn_until_the_thread_is_ready() {
    if std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("skipping: the stand-in app-server is written in python3");
        return;
    }
    let bin = tempfile::tempdir().unwrap();
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/fake-codex-app-server.py")
        .canonicalize()
        .unwrap();
    let driver = bin.path().join("codex");
    std::os::unix::fs::symlink(&fixture, &driver).unwrap();
    let cfg = Config {
        chat: vogt_engine_server::chats::ChatPolicy {
            enabled: true,
            command: Some(vec![driver.to_string_lossy().into_owned()]),
            approval_timeout: Duration::from_secs(10),
            ..Default::default()
        },
        session_templates: vec![],
        ..test_config()
    };
    let (base, _state, _guard) = boot_with_state(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let created = client
        .post(format!("{base}/api/chats"))
        .json(&json!({ "driver": "codex" }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::OK);
    let created: Value = created.json().await.unwrap();
    let id = created["chat"]["id"].as_str().unwrap().to_string();

    let sent = chat_send(&client, &base, &id, "ping").await;
    let chat: Value = client
        .get(format!("{base}/api/chats/{id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let entries = &chat["entries"];
    assert_eq!(texts(entries, "assistant"), vec!["pong"], "{sent} {chat}");
    assert!(texts(entries, "error").is_empty(), "{chat}");
}

#[tokio::test]
async fn codex_asks_go_to_a_person_and_errors_are_retryable() {
    let Some((base, client, state, _guard, _bin)) = boot_chats().await else {
        return;
    };
    let created: Value = client
        .post(format!("{base}/api/chats"))
        .json(&json!({}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["chat"]["id"].as_str().unwrap().to_string();
    let dir = state.config.state_dir.join("chats").join(&id);

    // A host change goes to a person, as a `codex` card.
    client
        .post(format!("{base}/api/chats/{id}/messages"))
        .json(&json!({ "text": "hostask" }))
        .send()
        .await
        .unwrap();
    let approval = pending_approval(&client, &base, &id).await;
    assert_eq!(approval["source"], "codex");
    assert_eq!(approval["summary"], "systemctl restart nginx");
    let approval_id = approval["id"].as_str().unwrap();
    client
        .post(format!("{base}/api/chats/{id}/approvals/{approval_id}"))
        .json(&json!({ "allow": false }))
        .send()
        .await
        .unwrap();
    wait_chat_idle(&client, &base, &id).await;
    let responses = std::fs::read_to_string(dir.join("responses.jsonl")).unwrap();
    assert!(
        responses.contains("\"decision\": \"decline\""),
        "{responses}"
    );

    // A provider error is a failed reply the person can send again.
    let failed = chat_send(&client, &base, &id, "fail").await;
    let errors: Vec<&Value> = failed["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "error")
        .collect();
    assert_eq!(errors.len(), 1, "{failed}");
    assert_eq!(errors[0]["retryable"], true);
    assert!(errors[0]["text"].as_str().unwrap().contains("400"));

    // Stop ends a running turn as a stop, not an error.
    client
        .post(format!("{base}/api/chats/{id}/messages"))
        .json(&json!({ "text": "slow" }))
        .send()
        .await
        .unwrap();
    let stopped = client
        .post(format!("{base}/api/chats/{id}/interrupt"))
        .send()
        .await
        .unwrap();
    assert_eq!(stopped.status(), StatusCode::OK);
    let detail = wait_chat_idle(&client, &base, &id).await;
    let notices = texts(&detail["entries"], "notice");
    assert!(notices.contains(&"Stopped.".to_string()), "{detail}");
}

#[tokio::test]
async fn a_promoted_codex_chat_continues_in_a_pod_session_and_takes_no_more_messages() {
    if std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("skipping: the stand-in app-server is written in python3");
        return;
    }
    let bin = tempfile::tempdir().unwrap();
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/fake-codex-app-server.py")
        .canonicalize()
        .unwrap();
    let driver = bin.path().join("codex");
    std::os::unix::fs::symlink(&fixture, &driver).unwrap();
    // Codex is pod-only, so a chat is promoted through the template the
    // deployment configured for it, never the chat's own host argv.
    let mut h = pod_stream::harness_cfg(
        false,
        Some(vec![SessionTemplate {
            name: "codex-pod".into(),
            description: "pod-backed Codex chat driver".into(),
            command: Some(vec![driver.to_string_lossy().into_owned()]),
            cwd: None,
            env: Vec::new(),
            account: None,
            default_name: None,
            match_repo_names: Vec::new(),
            match_path_prefixes: Vec::new(),
            tags: Vec::new(),
            backend: "pod".into(),
            host_capable: false,
            pod_capable: true,
        }]),
        |cfg| {
            cfg.chat = vogt_engine_server::chats::ChatPolicy {
                enabled: true,
                template: Some("codex-pod".into()),
                approval_timeout: Duration::from_secs(10),
                ..Default::default()
            };
        },
    )
    .await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let created: Value = client
        .post(format!("{}/api/chats", h.base))
        .json(&json!({ "driver": "codex", "message": "plan the fix" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["chat"]["id"].as_str().unwrap().to_string();
    wait_chat_idle(&client, &h.base, &id).await;
    let promoted = client
        .post(format!("{}/api/chats/{id}/promote", h.base))
        .json(&json!({ "name": "fix it" }))
        .send()
        .await
        .unwrap();
    assert_eq!(promoted.status(), StatusCode::OK);
    let promoted: Value = promoted.json().await.unwrap();
    let session = promoted["session"]["id"].as_str().unwrap();
    assert_eq!(promoted["chat"]["promoted_session"], session);
    assert_eq!(promoted["session"]["name"], "fix it");
    // The session runs the same conversation, by the chat's id.
    assert_eq!(
        promoted["session"]["conversation"]["id"],
        id.as_str(),
        "{promoted}"
    );
    assert_eq!(promoted["session"]["conversation"]["agent"], "codex");
    assert!(
        promoted["chat"]["live"] == false,
        "the chat's own process is gone"
    );
    // And it runs in a pod, because the driver's template is pod-backed.
    let (pod, _token) = tokio::time::timeout(Duration::from_secs(5), h.created.recv())
        .await
        .expect("the promoted session creates a pod")
        .expect("the pod is created");
    assert_eq!(pod.to_string(), session);

    let refused = client
        .post(format!("{}/api/chats/{id}/messages", h.base))
        .json(&json!({ "text": "more" }))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), StatusCode::CONFLICT);
}

/// Codex is pod-only: a chat launched by a bare `ENGINE_CHAT_COMMAND` has no
/// template to place a session in, so promotion refuses with the setting to
/// configure instead of a host-placed start's placement error — and it
/// refuses before stopping the chat's process, so the chat still works.
#[tokio::test]
async fn a_codex_chat_without_a_pod_template_refuses_promotion_and_keeps_its_process() {
    let Some((base, client, _state, _guard, _bin)) = boot_chats().await else {
        return;
    };
    let created: Value = client
        .post(format!("{base}/api/chats"))
        .json(&json!({ "driver": "codex", "message": "plan the fix" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["chat"]["id"].as_str().unwrap().to_string();
    wait_chat_idle(&client, &base, &id).await;
    let promoted = client
        .post(format!("{base}/api/chats/{id}/promote"))
        .json(&json!({ "name": "fix it" }))
        .send()
        .await
        .unwrap();
    assert_eq!(promoted.status(), StatusCode::CONFLICT);
    let body = promoted.text().await.unwrap();
    assert!(body.contains("ENGINE_CHAT_TEMPLATE"), "{body}");
    let detail: Value = client
        .get(format!("{base}/api/chats/{id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(detail.get("promoted_session").is_none(), "{detail}");
    // The process was not stopped: the chat takes another turn.
    let sent = chat_send(&client, &base, &id, "hello again").await;
    assert!(sent["entries"].is_array(), "{sent}");
}

#[tokio::test]
async fn chats_need_the_sessions_capability_and_404_when_off() {
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    // The test config turns chats off: every route is absent, and the
    // config does not advertise them.
    let off = client
        .get(format!("{base}/api/chats"))
        .send()
        .await
        .unwrap();
    assert_eq!(off.status(), StatusCode::NOT_FOUND);
    let config: Value = reqwest::get(format!("{base}/api/config"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(config.get("chat").is_none(), "{config}");
    // Unauthenticated: refused before anything else.
    let anonymous = reqwest::get(format!("{base}/api/chats")).await.unwrap();
    assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn chat_processes_are_capped_and_an_idle_one_makes_room() {
    let Some((base, client, _state, _guard, _bin)) = boot_chats_with(|chat| {
        chat.max_per_creator = 1;
        chat.max_processes = 4;
    })
    .await
    else {
        return;
    };
    let mut ids = Vec::new();
    for _ in 0..2 {
        let created: Value = client
            .post(format!("{base}/api/chats"))
            .json(&json!({}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        ids.push(created["chat"]["id"].as_str().unwrap().to_string());
    }
    chat_send(&client, &base, &ids[0], "one").await;
    // The second chat's launch stops the first, which was idle.
    chat_send(&client, &base, &ids[1], "two").await;
    let first: Value = client
        .get(format!("{base}/api/chats/{}", ids[0]))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(first["live"], false, "{first}");

    // A busy chat is not stopped for another: that one is refused, by name.
    client
        .post(format!("{base}/api/chats/{}/messages", ids[1]))
        .json(&json!({ "text": "slow" }))
        .send()
        .await
        .unwrap();
    let refused = client
        .post(format!("{base}/api/chats/{}/messages", ids[0]))
        .json(&json!({ "text": "again" }))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), StatusCode::CONFLICT);
    assert!(refused.text().await.unwrap().contains("already working"));
    client
        .post(format!("{base}/api/chats/{}/interrupt", ids[1]))
        .send()
        .await
        .unwrap();
}

/// A Codex turn whose title says `working` while its screen has gone quiet
/// stays `running` (WI-1598) — and the activity watcher must then wait for
/// the next window, not loop straight back. On prod (v0.7.15) it looped
/// without an await, pinning a runtime worker at 100% and starving terminal
/// input and every request queued behind it (WI-1644). The engine here runs
/// on a single-threaded runtime, so a spinning watcher hangs the runtime
/// outright: the watchdog thread turns that into a failure, not a hung suite.
///
/// The session is spawned through `pty::spawn` directly: this is a regression
/// test of the watcher, and a Codex *session* started over the API would be
/// refused by the pod-only placement gate before a PTY existed.
#[test]
fn a_quiet_working_codex_session_does_not_starve_the_runtime() {
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let tmp = tempfile::tempdir().unwrap();
            let codex = tmp.path().join("codex");
            std::fs::write(
                &codex,
                "#!/bin/sh\nprintf '\\033]0;codex: working\\007Working (0s)\n'\nexec sleep 30\n",
            )
            .unwrap();
            std::fs::set_permissions(&codex, std::os::unix::fs::PermissionsExt::from_mode(0o755))
                .unwrap();
            let spec = vogt_engine_contract::SessionSpec {
                name: "quiet-codex".into(),
                command: Some(vec![codex.to_string_lossy().into_owned()]),
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
                    tier: None,
                    on_exit: None,
                },
                vogt_engine_server::events::EventBus::new(64),
                None,
            )
            .unwrap();
            let session = spawned.session;
            // Several quiet windows (idle_after_ms is 200 here).
            tokio::time::sleep(Duration::from_millis(1_500)).await;
            assert_eq!(
                session.activity(),
                vogt_engine_contract::ActivityState::Running
            );
            session.kill().unwrap();
        });
        let _ = done_tx.send(());
    });
    done_rx
        .recv_timeout(Duration::from_secs(20))
        .expect("the engine runtime stalled: the activity watcher is spinning");
}

/// WI-4186: the cause. A pod session is silent while the pod is scheduled and
/// the entrypoint boots, so it goes idle before its CLI draws anything, and
/// that first draw was stamped as a turn start. The watchdog took the stamp
/// for a started first turn and returned, so a pod Codex whose launch prompt
/// sat in the composer was never given its Enter: with autopilot off it never
/// ran (the WI-3619 bench), with autopilot on only the 60-second nudge started
/// it. The stub stays silent past the quiet window, then waits at its composer
/// for an Enter. (Codex is pod-only, so `POST /api/sessions` cannot start it
/// in-process; the gate itself is `first_turn::watches`, unit-tested.)
#[tokio::test]
async fn a_codex_silent_until_its_first_draw_still_gets_its_first_turn() {
    let tmp = tempfile::tempdir().unwrap();
    let bus = vogt_engine_server::events::EventBus::new(64);
    let session = spawn_fake_codex(
        tmp.path(),
        "#!/bin/sh\n\
         sleep 0.8\n\
         printf 'OpenAI Codex\\n'\n\
         sleep 0.1\n\
         printf '\\033]0;codex: ready\\007'\n\
         printf '> '\n\
         read _line\n\
         printf '\\033]0;codex: working\\007'\n\
         printf 'TURN STARTED\\n'\n\
         exec sleep 30\n",
        &bus,
    );
    tokio::time::timeout(
        Duration::from_secs(10),
        vogt_engine_server::first_turn::watch(bus, Arc::clone(&session), first_turn_policy()),
    )
    .await
    .expect("the watch ends")
    .unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    let mut screen = screen_text(&session);
    while !screen.contains("TURN STARTED") && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(100)).await;
        screen = screen_text(&session);
    }
    let blocked = session.blocked();
    // Killed before any assertion, so a failure reports instead of hanging on
    // a child still waiting for its Enter.
    session.kill().unwrap();
    assert!(
        screen.contains("TURN STARTED"),
        "the first turn never started: the boot draw was taken for a turn and \
         the watchdog never pressed Enter; screen: {screen}"
    );
    assert!(blocked.is_none(), "{blocked:?}");
}

/// WI-4186 review: a Codex that took its launch prompt but is slow to show a
/// running turn sits at an empty composer past the turn window. The watchdog
/// may press Enter there, which an empty composer ignores, but it never types
/// a prompt into the session.
#[tokio::test]
async fn a_codex_slow_to_show_its_turn_is_never_typed_a_second_prompt() {
    let tmp = tempfile::tempdir().unwrap();
    let bus = vogt_engine_server::events::EventBus::new(64);
    let session = spawn_fake_codex(
        tmp.path(),
        &format!("#!/bin/bash\nprintf '\\033]0;codex: ready\\007'\nprintf '> '\n{LOG_READS}"),
        &bus,
    );
    vogt_engine_server::first_turn::watch(bus, Arc::clone(&session), first_turn_policy());
    let reads = reads_of(&session).await;
    assert!(
        reads.iter().all(|r| r == "READ:[]"),
        "typed into a live session: {reads:?}"
    );
    assert!(reads.len() <= 1, "more than one Enter: {reads:?}");
    session.kill().unwrap();
}

/// WI-1576: a Codex session's readiness and done come from the rollout's
/// `task_complete`, not a screen render: exactly one `ready` per turn id,
/// however often the tailer reads, and a final message ending in
/// `AUTOPILOT: DONE` is one `done` change with autopilot turned off.
#[tokio::test]
async fn a_codex_rollout_task_complete_is_one_ready_per_turn_and_its_done_marker_one_done() {
    let conversation = "7d2e1f3a-6c8b-4f2d-8a4e-3b9c8d7e6f50";
    let (_tmp, cfg, rollout) = codex_rollout_sandbox(
        conversation,
        vogt_engine_server::autopilot::Policy {
            nudge_after: Duration::from_secs(600),
            max_nudges: 10,
        },
    );
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    // The stand-in draws nothing, so no screen could read as ready: every
    // `ready` below is the rollout's.
    let created: Value = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({ "name": "codex-rollout", "command": ["/bin/cat"], "autopilot": true }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap().to_string();
    client
        .post(format!("{base}/api/sessions/{id}/conversation"))
        .json(&json!({ "agent": "codex", "id": conversation }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();

    append_rollout(
        &rollout,
        &[
            json!({"type": "task_started", "turn_id": "turn-1"}),
            json!({"type": "agent_message", "message": "working"}),
            json!({"type": "task_complete", "turn_id": "turn-1", "last_agent_message": "first item landed"}),
        ],
    );
    await_changes(&client, &base, &id, "ready", 1).await;

    append_rollout(
        &rollout,
        &[
            json!({"type": "task_started", "turn_id": "turn-2"}),
            json!({
                "type": "task_complete", "turn_id": "turn-2",
                "last_agent_message": "Nothing unblocked is left.\n\nAUTOPILOT: DONE\n"
            }),
        ],
    );
    await_changes(&client, &base, &id, "done", 1).await;

    // Sit past several ticks: nothing repeats.
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    let ready = changes_of(&client, &base, &id, "ready").await;
    assert_eq!(ready.len(), 2, "one ready per turn: {ready:?}");
    let done = changes_of(&client, &base, &id, "done").await;
    assert_eq!(done.len(), 1, "exactly one done: {done:?}");
    let detail: Value = client
        .get(format!("{base}/api/sessions/{id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        detail["summary"].get("autopilot").is_none(),
        "autopilot is off: {}",
        detail["summary"]
    );
}

/// WI-4511: with a 15 s economics tick, a prompted turn's `ready` still
/// follows its `task_complete` within about a second, and the rollout is
/// read when it grows rather than on the tick.
#[tokio::test]
async fn a_codex_ready_follows_task_complete_inside_the_economics_tick() {
    let conversation = "9f4a3b5c-8e0d-4b4f-8c6a-5d1e0f9a8b72";
    let (_tmp, mut cfg, rollout) = codex_rollout_sandbox(
        conversation,
        vogt_engine_server::autopilot::Policy {
            nudge_after: Duration::from_secs(600),
            max_nudges: 10,
        },
    );
    cfg.economics_tick = Duration::from_secs(15);
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let created: Value = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({ "name": "codex-fast-ready", "command": ["/bin/cat"] }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap().to_string();
    client
        .post(format!("{base}/api/sessions/{id}/conversation"))
        .json(&json!({ "agent": "codex", "id": conversation }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    // Let the first read settle the rollout; the next 15 s tick is far off.
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    for turn in ["turn-1", "turn-2"] {
        let before = changes_of(&client, &base, &id, "ready").await.len();
        append_rollout(
            &rollout,
            &[
                json!({"type": "task_started", "turn_id": turn}),
                json!({"type": "task_complete", "turn_id": turn, "last_agent_message": "ok"}),
            ],
        );
        let deadline = tokio::time::Instant::now() + Duration::from_millis(1_500);
        while changes_of(&client, &base, &id, "ready").await.len() <= before {
            assert!(
                tokio::time::Instant::now() < deadline,
                "ready for {turn} did not follow task_complete within 1.5 s"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
    tokio::time::sleep(Duration::from_millis(1_200)).await;
    let ready = changes_of(&client, &base, &id, "ready").await;
    assert_eq!(ready.len(), 2, "one ready per turn: {ready:?}");
}

/// WI-1576: a Codex session whose `/goal` is active continues idle turns by
/// itself, so autopilot does not nudge it while it rests at its prompt. Once
/// the goal completes, the same prompt is nudged.
#[tokio::test]
async fn a_codex_session_with_an_active_goal_is_not_nudged() {
    use std::os::unix::fs::PermissionsExt;
    let conversation = "8e3f2a4b-7d9c-4a3e-9b5f-4c0d9e8f7a61";
    let (tmp, cfg, rollout) = codex_rollout_sandbox(
        conversation,
        vogt_engine_server::autopilot::Policy {
            nudge_after: Duration::from_millis(400),
            max_nudges: 10,
        },
    );
    let log = tmp.path().join("input.log");
    let go = tmp.path().join("go");
    // Not named `codex`: a host launch of a binary called codex is refused, and
    // the conversation below is what makes this session a Codex one.
    let codex = tmp.path().join("fake-agent");
    // The prompt appears only once the rollout is being read, so the screen
    // cannot be nudged before the goal is known.
    std::fs::write(
        &codex,
        format!(
            "#!/usr/bin/env bash\nwhile [ ! -e '{}' ]; do sleep 0.05; done\nprintf '\\r\\n> \\r\\n'\nwhile IFS= read -r line; do echo x >> '{}'; done\n",
            go.display(),
            log.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&codex, std::fs::Permissions::from_mode(0o755)).unwrap();

    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let created: Value = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "codex-goal",
            "command": [codex.display().to_string()],
            "autopilot": true,
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(created["id"].is_string(), "session not created: {created}");
    let id = created["id"].as_str().unwrap().to_string();
    client
        .post(format!("{base}/api/sessions/{id}/conversation"))
        .json(&json!({ "agent": "codex", "id": conversation }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    append_rollout(
        &rollout,
        &[
            json!({"type": "thread_goal_updated", "goal": {"status": "active", "objective": "ship it"}}),
            json!({"type": "task_started", "turn_id": "turn-1"}),
            json!({"type": "task_complete", "turn_id": "turn-1", "last_agent_message": "step one done"}),
        ],
    );
    // The goal is active, so the ended turn is not the session at rest: no
    // `ready` is published for it.
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    assert!(
        changes_of(&client, &base, &id, "ready").await.is_empty(),
        "task_complete under an active goal is not ready"
    );
    std::fs::write(&go, "").unwrap();

    // Well past `nudge_after`, at its prompt: no nudge while the goal drives.
    tokio::time::sleep(Duration::from_millis(2_500)).await;
    assert_eq!(
        std::fs::read_to_string(&log).unwrap_or_default(),
        "",
        "an active goal is not nudged"
    );

    // The goal completes; the same prompt is now autopilot's to drive.
    append_rollout(
        &rollout,
        &[
            json!({"type": "thread_goal_updated", "goal": {"status": "complete", "objective": "ship it"}}),
        ],
    );
    await_changes(&client, &base, &id, "ready", 1).await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while std::fs::read_to_string(&log).unwrap_or_default().is_empty() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "a codex session with no active goal is nudged"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// WI-1576: an `AUTOPILOT: DONE` final message under an active `/goal` is held
/// with its ready, and the goal ending yields exactly one `done` (and one
/// `ready`) with autopilot turned off.
#[tokio::test]
async fn a_done_marker_under_an_active_goal_is_one_done_after_the_goal_ends() {
    let conversation = "5c1d0e2f-4a6b-4d1c-8e3d-1a7b6c5d4e39";
    let (_tmp, cfg, rollout) = codex_rollout_sandbox(
        conversation,
        vogt_engine_server::autopilot::Policy {
            nudge_after: Duration::from_secs(600),
            max_nudges: 10,
        },
    );
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let created: Value = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({ "name": "codex-goal-done", "command": ["/bin/cat"], "autopilot": true }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap().to_string();
    client
        .post(format!("{base}/api/sessions/{id}/conversation"))
        .json(&json!({ "agent": "codex", "id": conversation }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    // Codex stamps `completed_at`; a turn that ended after the session began
    // is live even when the first read already holds it.
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    append_rollout(
        &rollout,
        &[
            json!({"type": "thread_goal_updated", "goal": {"status": "active", "objective": "ship it"}}),
            json!({"type": "task_started", "turn_id": "turn-1"}),
            json!({
                "type": "task_complete", "turn_id": "turn-1", "completed_at": now,
                "last_agent_message": "Nothing unblocked is left.\n\nAUTOPILOT: DONE\n"
            }),
        ],
    );
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    assert!(changes_of(&client, &base, &id, "done").await.is_empty());
    assert!(changes_of(&client, &base, &id, "ready").await.is_empty());

    append_rollout(
        &rollout,
        &[
            json!({"type": "thread_goal_updated", "goal": {"status": "complete", "objective": "ship it"}}),
        ],
    );
    await_changes(&client, &base, &id, "done", 1).await;
    await_changes(&client, &base, &id, "ready", 1).await;
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    assert_eq!(changes_of(&client, &base, &id, "done").await.len(), 1);
    assert_eq!(changes_of(&client, &base, &id, "ready").await.len(), 1);
    let detail: Value = client
        .get(format!("{base}/api/sessions/{id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        detail["summary"].get("autopilot").is_none(),
        "autopilot is off: {}",
        detail["summary"]
    );
}
