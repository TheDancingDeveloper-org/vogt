use super::*;

/// WI-4522 review B1: a seeded shell pod holds an account slot like a
/// Claude pod, so the cap bounds how many pods share one refresh token.
/// At the cap the next one is refused, and a stopped one frees its slot.
#[tokio::test]
async fn a_shell_pod_takes_an_account_slot_and_frees_it() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let mut h = harness_cfg(false, Some(shell_pod_templates()), move |cfg| {
        cfg.agent_onboarding = default_account_onboarding(&root);
        cfg.agent_onboarding.account_concurrency_cap = 1;
    })
    .await;
    let start = |name: &str| {
        h.client
            .post(format!("{}/api/sessions", h.base))
            .json(&json!({ "name": name, "template": "Shell" }))
            .send()
    };
    let first = start("one").await.unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    let first: Value = first.json().await.unwrap();
    let second = start("two").await.unwrap();
    assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);

    let id = first["id"].as_str().unwrap();
    let gone = h
        .client
        .delete(format!("{}/api/sessions/{id}", h.base))
        .send()
        .await
        .unwrap();
    assert!(gone.status().is_success(), "{}", gone.status());
    let third = start("three").await.unwrap();
    assert_eq!(third.status(), StatusCode::OK);
    let _ = h.created.try_recv();
}

/// WI-4522: a Claude pod that names no account takes the default, not the
/// least-used one; a named account still wins.
#[tokio::test]
async fn a_claude_pod_without_an_account_takes_the_default() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let mut h = harness_cfg(
        false,
        Some(SessionTemplate::default_templates()),
        move |cfg| {
            cfg.agent_onboarding = default_account_onboarding(&root);
        },
    )
    .await;
    for (named, want) in [(None, "acct1"), (None, "acct1"), (Some("acct2"), "acct2")] {
        let mut body = json!({
            "name": "claude-pod",
            "template": "Claude Code (protected)",
            "placement": "pod",
        });
        if let Some(named) = named {
            body["account"] = json!(named);
        }
        let id = h.start_body(body).await;
        let (_, token) = h.created.recv().await.unwrap();
        let body: Value = h
            .bootstrap_as(id, &token, "claude")
            .await
            .json()
            .await
            .unwrap();
        let (account, login) = seeded_login(&body);
        assert_eq!(account, want, "{body}");
        assert!(login.contains(want), "{body}");
    }
}

/// WI-3869: a pod Codex's rollout, shipped by the real
/// `vogt-session-agent` at the path contract, lands under the engine's
/// Codex home, and `session.last_reply` on a real `vogt-core` returns the
/// assistant text. A Codex-only session has no Claude root or account, so
/// this is also the write-back path P2b (WI-3868) opened.
#[tokio::test]
async fn a_pod_codex_rollout_reaches_session_last_reply() {
    // Fail before the harness and uploader start, not from inside the
    // joined observe future, where a missing binary hangs the test.
    let _core_binary = vogt_core_binary();
    let pod = tempfile::tempdir().unwrap();
    let pod_home = pod.path().to_path_buf();
    let engine_home = tempfile::tempdir().unwrap();
    let engine_root = engine_home.path().to_path_buf();
    let template = SessionTemplate {
        name: "pod".into(),
        description: "pod-backed Codex".into(),
        command: Some(vec!["codex".into()]),
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
    };
    let stored = engine_root.clone();
    let mut h = harness_cfg(false, Some(vec![template]), move |cfg| {
        // A fresh pod Codex pins its conversation to the engine's session
        // id, and the engine stores its rollout under its own Codex home.
        cfg.codex_session_id = true;
        cfg.codex_transcript_root = Some(stored);
    })
    .await;
    let id = h.start().await;
    let (_, token) = h.created.recv().await.unwrap();
    let body: Value = h
        .bootstrap_as(id, &token, "codex")
        .await
        .json()
        .await
        .unwrap();
    let session_token = body["session_token"].as_str().unwrap().to_string();
    let conversation = body["env"]["VOGT_SESSION_CONVERSATION_ID"]
        .as_str()
        .expect("a pinned Codex conversation")
        .to_string();
    assert_eq!(conversation, id.to_string());

    // The launch line the pod's session agent really generates, and the
    // argv Codex sees. The stand-in below names its rollout from that same
    // argv, as Codex does — the old test planted a file under a name it
    // already believed in, so it passed while the launch line carried no
    // conversation at all (WI-3873).
    let reply = "PONG from the pod Codex";
    let record = serde_json::to_string(&json!({
        "timestamp": "2026-11-09T00:00:00Z",
        "type": "response_item",
        "payload": {
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": reply}],
        },
    }))
    .unwrap();
    let mut agent = vogt_session_agent::Config {
        session_id: id.to_string(),
        engine_url: h.base.clone(),
        token: String::new(),
        cli: "codex".into(),
        command: None,
        cwd: Some("/work".into()),
        cols: 120,
        rows: 40,
        buffer_bytes: 64 * 1024,
        claude_config_dir: None,
        codex_home: Some(pod_home.clone()),
        workspace_root: None,
        model: None,
        effort: None,
        permission_mode: None,
        codex_approval_policy: "never".into(),
        account: None,
        conversation_id: Some(conversation.clone()),
        resume: false,
        claude: vogt_session_agent::ClaudeLaunch::default(),
        codex: vogt_session_agent::CodexLaunch::default(),
        opencode_mcp_config: None,
    };
    let line = vogt_session_agent::command_line(&agent).expect("a Codex launch line");
    let mut argv = shell_argv(&line);
    assert_eq!(argv.first().map(String::as_str), Some("codex"), "{line}");
    let pinned = argv
        .windows(2)
        .find(|pair| pair[0] == "--session-id")
        .map(|pair| pair[1].clone())
        .expect("the generated line pins the conversation");
    assert_eq!(pinned, conversation, "{line}");

    use std::os::unix::fs::PermissionsExt;
    let standin = pod.path().join("codex-standin");
    std::fs::write(
        &standin,
        format!(
            r#"#!/bin/sh
set -eu
home='{home}'
id=
while [ $# -gt 0 ]; do
  case "$1" in
    --session-id|resume) id="$2"; shift 2 ;;
    *) shift ;;
  esac
done
[ -n "$id" ] || exit 64
day="$(date -u +%Y/%m/%d)"
mkdir -p "$home/sessions/$day"
name="rollout-$(date -u +%Y-%m-%dT%H-%M-%S)-$id.jsonl"
printf '%s\n' '{record}' > "$home/sessions/$day/$name"
sleep 2
"#,
            home = pod_home.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&standin, std::fs::Permissions::from_mode(0o755)).unwrap();
    argv[0] = standin.to_string_lossy().into_owned();
    agent.command = Some(argv);
    agent.cwd = Some(pod.path().to_string_lossy().into_owned());
    agent.token = session_token;

    // The in-pod uploader for real: this session has no Claude root and no
    // account, and its rollout must still travel at the contract path.
    // `run` is not `Send`, so drive it and the assertions together on this
    // task rather than spawning it.
    let uploader = vogt_session_agent::run(agent);
    let observe = async {
        // The engine writes it under its own Codex home, under whatever
        // date directory the stand-in chose.
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let landed = loop {
            if let Some(path) = find_rollout(engine_root.as_path(), &conversation) {
                break path;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the pod's rollout never reached the engine under {}",
                engine_root.display()
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        };
        assert!(std::fs::read_to_string(&landed).unwrap().contains(reply));

        let core = boot_codex_core(&h.base, &engine_home.path().join("sessions")).await;
        let response = reqwest::Client::new()
            .get(format!("{}/api/sessions/last-reply", core.url))
            .query(&[("id", id.to_string()), ("n", "1".to_string())])
            .bearer_auth(CORE_TOKEN)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let answer: Value = response.json().await.unwrap();
        assert_eq!(answer["agent"], "codex", "{answer}");
        assert_eq!(answer["conversation_id"], conversation, "{answer}");
        assert_eq!(answer["messages"][0]["text"], reply, "{answer}");
    };
    let (uploaded, ()) = tokio::join!(uploader, observe);
    uploaded.expect("the session-agent ships the rollout and exits");
}

/// WI-4291: a pod opencode keeps its store (and its credentials) in the
/// pod. The real session agent reads the assistant text out of it and
/// ships only that; the engine keeps it, and `session.last_reply` on a
/// real `vogt-core` returns it. The engine's copy holds no credential,
/// tool payload or prompt from the store.
#[tokio::test]
async fn a_pod_opencode_reply_reaches_session_last_reply() {
    let _core_binary = vogt_core_binary();
    let pod = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let kept = state_dir.path().to_path_buf();
    let mut h = harness_cfg(false, Some(vec![opencode_pod_template()]), move |cfg| {
        cfg.state_dir = kept;
    })
    .await;
    let id = h.start().await;
    let (_, token) = h.created.recv().await.unwrap();
    let body: Value = h
        .bootstrap_as(id, &token, "opencode")
        .await
        .json()
        .await
        .unwrap();
    let session_token = body["session_token"].as_str().unwrap().to_string();
    let reply = "PONG from the pod opencode";
    let db = opencode_pod_store(pod.path(), id, reply).await;

    let shipped =
        vogt_session_agent::ship_opencode_replies(&opencode_agent(id, &h.base, session_token), &db)
            .await
            .expect("the agent builds its client");
    assert!(shipped, "the engine refused the pod's replies");

    // The engine's own read, as the core asks it.
    let replies: Value = h
        .client
        .get(format!("{}/api/sessions/{id}/replies?n=1", h.base))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(replies["agent"], "opencode", "{replies}");
    assert_eq!(replies["conversation_id"], "ses_podoc", "{replies}");
    assert_eq!(replies["replies"][0]["text"], reply, "{replies}");

    // Nothing from the store but that text is anywhere the engine keeps.
    let mut dirs = vec![state_dir.path().to_path_buf()];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                dirs.push(path);
                continue;
            }
            let text = String::from_utf8_lossy(&std::fs::read(&path).unwrap()).into_owned();
            for leaked in [OPENCODE_PROVIDER_KEY, OPENCODE_TOOL_SECRET, "ghp_TOOL"] {
                assert!(
                    !text.contains(leaked),
                    "{leaked} reached the engine in {}",
                    path.display()
                );
            }
        }
    }

    let core = boot_codex_core(&h.base, &pod.path().join("no-transcripts")).await;
    let response = reqwest::Client::new()
        .get(format!("{}/api/sessions/last-reply", core.url))
        .query(&[("id", id.to_string()), ("n", "1".to_string())])
        .bearer_auth(CORE_TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let answer: Value = response.json().await.unwrap();
    assert_eq!(answer["agent"], "opencode", "{answer}");
    assert_eq!(answer["conversation_id"], "ses_podoc", "{answer}");
    assert_eq!(answer["messages"][0]["text"], reply, "{answer}");
}

/// WI-4291: the reply channel carries a conversation id and assistant
/// text and nothing else. A message with any other field (a credential,
/// the environment, a tool payload) is refused and nothing is kept; so is
/// one without the pod's stream credential, and credential-shaped text
/// inside a reply is redacted before it is kept.
#[tokio::test]
async fn the_opencode_reply_channel_carries_no_credential_field() {
    let state_dir = tempfile::tempdir().unwrap();
    let kept = state_dir.path().to_path_buf();
    let mut h = harness_cfg(false, Some(vec![opencode_pod_template()]), move |cfg| {
        cfg.state_dir = kept;
    })
    .await;
    let id = h.start().await;
    let (_, token) = h.created.recv().await.unwrap();
    let body: Value = h
        .bootstrap_as(id, &token, "opencode")
        .await
        .json()
        .await
        .unwrap();
    let session_token = body["session_token"].as_str().unwrap().to_string();
    let url = format!(
        "{}{}",
        h.base,
        vogt_session_agent::opencode_store::REPLIES_ROUTE
    );
    let post = |bearer: Option<String>, body: Value| {
        let request = reqwest::Client::new().post(&url).json(&body);
        let request = match bearer {
            Some(bearer) => request.bearer_auth(bearer),
            None => request,
        };
        request.send()
    };
    for hostile in [
        json!({"conversation_id": "ses_podoc", "replies": [], "credentials": OPENCODE_PROVIDER_KEY}),
        json!({"conversation_id": "ses_podoc", "replies": [], "env": {"ANTHROPIC_API_KEY": OPENCODE_PROVIDER_KEY}}),
        json!({"conversation_id": "ses_podoc", "replies": [{"text": "hi", "tool": {"input": OPENCODE_TOOL_SECRET}}]}),
        json!({"conversation_id": "ses_podoc", "replies": [{"text": "hi", "key": OPENCODE_PROVIDER_KEY}]}),
        json!({"conversation_id": "../escape", "replies": [{"text": "hi"}]}),
    ] {
        let response = post(Some(session_token.clone()), hostile.clone())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{hostile}");
    }
    let fine = json!({"conversation_id": "ses_podoc", "replies": [{"text": "hi"}]});
    assert_eq!(
        post(None, fine.clone()).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        post(Some(TEST_TOKEN.into()), fine).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    let none: Value = h
        .client
        .get(format!("{}/api/sessions/{id}/replies", h.base))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(none["replies"], json!([]), "{none}");

    let printed = format!("the key is {OPENCODE_PROVIDER_KEY} and {OPENCODE_TOOL_SECRET}");
    let response = post(
        Some(session_token),
        json!({"conversation_id": "ses_podoc", "replies": [{"text": printed, "at": null}]}),
    )
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let kept: Value = h
        .client
        .get(format!("{}/api/sessions/{id}/replies", h.base))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let text = kept["replies"][0]["text"].as_str().unwrap();
    assert!(text.starts_with("the key is "), "{kept}");
    assert!(!text.contains(OPENCODE_PROVIDER_KEY), "{kept}");
    assert!(!text.contains("ghp_TOOL"), "{kept}");
}

/// WI-3873: a Codex session has no Claude account (the engine refuses one
/// for a non-Claude CLI), yet a wake must still seed the conversation's
/// rollout and set `VOGT_SESSION_RESUME=1`; otherwise the woken pod starts
/// a fresh conversation and the transcript is lost.
#[tokio::test]
async fn a_codex_bootstrap_seeds_its_rollout_without_an_account() {
    let engine_home = tempfile::tempdir().unwrap();
    let stored = engine_home.path().to_path_buf();
    let template = SessionTemplate {
        name: "pod".into(),
        description: "pod-backed Codex".into(),
        command: Some(vec!["codex".into()]),
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
    };
    let mut h = harness_cfg(false, Some(vec![template]), move |cfg| {
        cfg.codex_session_id = true;
        cfg.codex_transcript_root = Some(stored);
    })
    .await;
    let id = h.start().await;
    let (_, token) = h.created.recv().await.unwrap();

    // What a previous pod's write-back left in the engine's Codex home.
    let conversation = id.to_string();
    let transcript = b"{\"type\":\"session\"}\n";
    let relative = format!("sessions/2026/11/09/rollout-2026-11-09T00-00-00-{conversation}.jsonl");
    let rollout = engine_home.path().join(&relative);
    std::fs::create_dir_all(rollout.parent().unwrap()).unwrap();
    std::fs::write(&rollout, transcript).unwrap();

    let body: Value = h
        .bootstrap_as(id, &token, "codex")
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(body["env"]["VOGT_SESSION_CONVERSATION_ID"], conversation);
    assert_eq!(body["env"]["VOGT_SESSION_RESUME"], "1", "{body}");
    let seeded = body["bundle"][&relative]
        .as_str()
        .expect("the rollout is seeded into the woken pod");
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(seeded)
            .unwrap(),
        transcript
    );
}

/// WI-3355: the bootstrap response carries the session's brief, not its
/// name, so the pod's brief file holds the task.
#[tokio::test]
async fn a_bootstrap_carries_the_sessions_brief_text() {
    let mut h = harness(false).await;
    let id = h
        .start_body(json!({
            "name": "pod-session",
            "template": "pod",
            "prompt": "Fix the flaky forge test.\n\n## Task\nDo it.",
        }))
        .await;
    let (_, token) = h.created.recv().await.unwrap();
    let body: Value = h
        .bootstrap_as(id, &token, "claude")
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(
        body["brief"], "Fix the flaky forge test.\n\n## Task\nDo it.",
        "{body}"
    );
}

/// Without a core to mint from, a Claude pod still gets a config, with no
/// servers, so `--strict-mcp-config` keeps the account's own set out.
#[tokio::test]
async fn a_claude_bootstrap_with_no_core_gets_an_empty_mcp_config() {
    let mut h = harness(false).await;
    let id = h.start().await;
    let (_, token) = h.created.recv().await.unwrap();
    let body: Value = h
        .bootstrap_as(id, &token, "claude")
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(body["mcp_config"], json!({"mcpServers": {}}));
    assert!(body["env"].get("VOGT_HTTP_TOKEN").is_none(), "{body}");
}

/// WI-3243: a pod session has no broker token; its policy hook presents
/// the stream credential it was bootstrapped with, and is checked against
/// its own policy. Nothing else opens the route.
#[tokio::test]
async fn a_pod_sessions_policy_hook_authenticates_with_its_stream_credential() {
    let mut h = harness(false).await;
    let created: Value = h
        .client
        .post(format!("{}/api/sessions", h.base))
        .json(&json!({
            "name": "policed",
            "template": "pod",
            "policy": {"merge_deploys_prod": true},
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id: Uuid = created["id"].as_str().unwrap().parse().unwrap();
    let stream_token = h.session_token(id).await;

    let check = |bearer: String, command: &'static str| {
        let url = format!("{}/api/agent-auth/policy-check", h.base);
        async move {
            reqwest::Client::new()
                .post(url)
                .bearer_auth(bearer)
                .json(&json!({"tool_name": "Bash", "tool_input": {"command": command}}))
                .send()
                .await
                .unwrap()
        }
    };
    let denied = check(stream_token.clone(), "gh pr merge 5").await;
    assert_eq!(denied.status(), StatusCode::OK);
    assert_eq!(denied.json::<Value>().await.unwrap()["decision"], "deny");
    let allowed = check(stream_token.clone(), "ls").await;
    assert_eq!(allowed.json::<Value>().await.unwrap()["decision"], "allow");
    // WI-4046: printing the environment is refused whatever the project
    // policy says, and the reason does not repeat the variable.
    let dumped: Value = check(stream_token.clone(), "printenv VOGT_HTTP_TOKEN")
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(dumped["decision"], "deny");
    assert!(!dumped["reason"]
        .as_str()
        .unwrap()
        .contains("VOGT_HTTP_TOKEN"));
    let safe = check(stream_token.clone(), "printenv HOME").await;
    assert_eq!(safe.json::<Value>().await.unwrap()["decision"], "allow");
    // A made-up credential and a near miss are refused.
    for bearer in ["not-a-token".to_string(), format!("{stream_token}x")] {
        assert_eq!(
            check(bearer, "gh pr merge 5").await.status(),
            StatusCode::UNAUTHORIZED
        );
    }
}

/// WI-4046: the environment guard holds a session that has no project
/// policy at all.
#[tokio::test]
async fn the_environment_guard_holds_a_session_without_a_policy() {
    let mut h = harness(false).await;
    let created: Value = h
        .client
        .post(format!("{}/api/sessions", h.base))
        .json(&json!({"name": "unpoliced", "template": "pod"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id: Uuid = created["id"].as_str().unwrap().parse().unwrap();
    let stream_token = h.session_token(id).await;
    for (command, decision) in [("env", "deny"), ("ls", "allow"), ("env FOO=1 ls", "allow")] {
        let body: Value = reqwest::Client::new()
            .post(format!("{}/api/agent-auth/policy-check", h.base))
            .bearer_auth(&stream_token)
            .json(&json!({"tool_name": "Bash", "tool_input": {"command": command}}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(body["decision"], decision, "{command}");
    }
}

/// Runs the real `deploy/session-entrypoint.sh` against the real bootstrap
/// route. The CLI agent and `graft` are fakes that record what they get.
#[tokio::test]
async fn real_entrypoint_bootstraps_against_the_real_route() {
    use std::os::unix::fs::PermissionsExt;
    // WI-4271: the entrypoint clones only with a source token the
    // bootstrap response carries (WI-4213), so the project's graft source
    // is served by a minting provider, as in production.
    let mut h = core_harness(false).await;
    install_minter(&h, FakeMinter::new(Ok("minted-graft-token-SECRET-1234")));
    // WI-3883 review: a caller's own variable must reach the agent and
    // must not be exported into the shell that clones with the minted
    // source token.
    let id = h
        .start_body(json!({
            "name": "pod-entrypoint-env",
            "template": "pod",
            "env": [["CARGO_BUILD_JOBS", "4"]],
            "source": graft_source(),
        }))
        .await;
    let (_, token) = h.created.recv().await.unwrap();
    let token = String::from_utf8_lossy(&token).into_owned();

    let tmp = tempfile::tempdir().unwrap();
    let bin = tmp.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let script = |name: &str, body: &str| {
        let path = bin.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    };
    let graft_log = tmp.path().join("graft.args");
    let graft_env = tmp.path().join("graft.env");
    let agent_log = tmp.path().join("agent.env");
    script(
            "graft",
            &format!(
                "printf '%s\\n' \"$*\" >>{g}\ncase \" $* \" in *\" clone \"*) env >{e} ;; esac\nwhile [ \"$1\" != clone ]; do shift; done\nmkdir -p \"$3\"",
                g = graft_log.display(),
                e = graft_env.display()
            ),
        );
    std::os::unix::fs::symlink(bin.join("graft"), bin.join("g")).unwrap();
    script(
        "vogt-session-agent",
        &format!(
            "printf 'argc=%s\\n' \"$#\" >{a}\nenv >>{a}\necho agent-ran",
            a = agent_log.display()
        ),
    );
    let work = tmp.path().join("work");
    let token_file = tmp.path().join("run").join("session-token");
    // What the kubelet mounts from the per-session Secret.
    let bootstrap_file = tmp.path().join("bootstrap-token");
    std::fs::write(&bootstrap_file, &token).unwrap();
    let run = |tmp_dir: std::path::PathBuf| {
        let mut cmd = std::process::Command::new("sh");
        cmd.arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../deploy/session-entrypoint.sh"
            ))
            .env_clear()
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .env("HOME", tmp.path())
            .env("TMPDIR", tmp_dir)
            .env("CLAUDE_CONFIG_DIR", tmp.path().join("claude"))
            .env("VOGT_ENGINE_URL", &h.base)
            .env("VOGT_SESSION_ID", id.to_string())
            .env("VOGT_BOOTSTRAP_TOKEN_FILE", &bootstrap_file)
            .env(
                "VOGT_SOURCE_JSON",
                r#"{"location":{"kind":"graft","endpoint":"","repo":"vogt"},"ref":"rust-2","landing":"line"}"#,
            )
            .env("VOGT_CLI", "claude")
            .env("VOGT_CLONE_DIR", &work)
            .env("VOGT_WORK_DIR", &work)
            .env("VOGT_SESSION_TOKEN_FILE", &token_file);
        tokio::task::spawn_blocking(move || cmd.output().unwrap())
    };
    let tmp_a = tmp.path().join("tmp-a");
    std::fs::create_dir(&tmp_a).unwrap();
    let out = run(tmp_a).await.unwrap();
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "entrypoint failed: {all}");
    assert!(all.contains("agent-ran"), "agent was not exec'd: {all}");

    // The session token is on disk, owner-only, and nowhere else.
    let mode = std::fs::metadata(&token_file).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
    let session_token = std::fs::read_to_string(&token_file).unwrap();
    assert!(!session_token.trim().is_empty());
    assert!(!all.contains(&token) && !all.contains(session_token.trim()));

    // The agent got its contract in env, no secrets, and no arguments.
    let env = std::fs::read_to_string(&agent_log).unwrap();
    assert!(env.starts_with("argc=0\n"), "agent args: {env}");
    assert!(!env.contains(&token), "bootstrap token reached the agent");
    assert!(!env.contains(session_token.trim()), "session token in env");
    assert!(!env.contains("VOGT_BOOTSTRAP_TOKEN"));
    for want in [
        format!("VOGT_SESSION_ID={id}"),
        "VOGT_CLI=claude".into(),
        format!("VOGT_SESSION_TOKEN_FILE={}", token_file.display()),
        format!("VOGT_SESSION_CWD={}", work.display()),
    ] {
        assert!(env.lines().any(|l| l == want), "agent env lacks {want}");
    }
    assert!(
        env.lines().any(|l| l.starts_with("VOGT_SOURCE_JSON=")),
        "agent env lacks its source: {env}"
    );
    // The caller's own variable applies to the agent's process only: the
    // clone (which spends the source token) ran without it.
    assert!(
        env.lines().any(|l| l == "CARGO_BUILD_JOBS=4"),
        "agent env lacks the caller's variable: {env}"
    );
    let graft_env = std::fs::read_to_string(&graft_env).unwrap();
    assert!(
        !graft_env
            .lines()
            .any(|l| l.starts_with("CARGO_BUILD_JOBS=")),
        "the caller's env was exported before the clone: {graft_env}"
    );
    assert!(work.join(".vogt/brief.md").exists());

    // `graft` cloned once, with the minted token file, on the requested
    // line; any later calls only read the clone for its git view.
    let graft = std::fs::read_to_string(&graft_log).unwrap();
    assert_eq!(graft.matches(" clone ").count(), 1, "{graft}");
    assert_eq!(
        graft.lines().next().unwrap_or_default(),
        format!(
            "--token-file {} clone vogt {} --line rust-2",
            tmp.path()
                .join(format!(".config/graft/sessions/{id}/trial-1.token"))
                .display(),
            work.display()
        )
    );
    assert!(!graft.contains(&token));
    assert!(!graft.contains("minted-graft-token-SECRET-1234"));

    // The bootstrap was consumed: replaying it is refused.
    let tmp_b = tmp.path().join("tmp-b");
    std::fs::create_dir(&tmp_b).unwrap();
    let again = run(tmp_b).await.unwrap();
    assert!(!again.status.success());
    let err = String::from_utf8_lossy(&again.stderr);
    assert!(err.contains("bootstrap failed"), "{err}");
    assert!(!err.contains(&token));
}

#[tokio::test]
async fn bootstrap_without_a_json_body_says_what_it_needs() {
    let mut h = harness(false).await;
    let id = h.start().await;
    let (_, token) = h.created.recv().await.unwrap();
    let res = reqwest::Client::new()
        .post(format!("{}/api/agent-auth/bootstrap", h.base))
        .bearer_auth(String::from_utf8_lossy(&token))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let body = res.text().await.unwrap();
    assert!(body.contains("JSON body"), "{body}");
    assert!(body.contains("session_id"), "{body}");
    // A rejected body does not burn the one-shot token.
    assert_eq!(h.bootstrap(id, &token).await.status(), StatusCode::OK);
}

#[tokio::test]
async fn bad_and_reused_tokens_are_refused() {
    let mut h = harness(false).await;
    let id = h.start().await;
    let (created_id, token) = h.created.recv().await.unwrap();
    assert_eq!(created_id, id);

    assert_eq!(
        h.bootstrap(id, b"wrong").await.status(),
        StatusCode::UNAUTHORIZED
    );
    let ok = h.bootstrap(id, &token).await;
    assert_eq!(ok.status(), StatusCode::OK);
    let session_token = ok.json::<Value>().await.unwrap()["session_token"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        h.bootstrap(id, &token).await.status(),
        StatusCode::UNAUTHORIZED
    );

    // The stream takes the session token, not the bootstrap one, and not
    // a session token for some other session.
    let bootstrap = String::from_utf8_lossy(&token).into_owned();
    assert_eq!(
        h.connect(id, &bootstrap).await.err(),
        Some(StatusCode::UNAUTHORIZED)
    );
    assert_eq!(
        h.connect(id, "nope").await.err(),
        Some(StatusCode::UNAUTHORIZED)
    );
    assert_eq!(
        h.connect(Uuid::new_v4(), &session_token).await.err(),
        Some(StatusCode::UNAUTHORIZED)
    );
    assert!(h.connect(id, &session_token).await.is_ok());
}

#[tokio::test]
async fn the_requested_command_reaches_the_pod_env() {
    let mut h = harness(false).await;
    let created: Value = h
        .client
        .post(format!("{}/api/sessions", h.base))
        .json(&json!({
            "name": "pod-shell",
            "template": "pod",
            "command": ["bash", "-l"]
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(created["id"].is_string(), "{created}");
    h.created.recv().await.expect("a pod is created");
    let pod = h.launcher.pods.lock().unwrap().pop().expect("pod created");
    let container = &pod.spec.unwrap().containers[0];
    assert!(container.command.is_none());
    let raw = container
        .env
        .as_ref()
        .unwrap()
        .iter()
        .find(|v| v.name == "VOGT_SESSION_COMMAND")
        .and_then(|v| v.value.clone())
        .expect("VOGT_SESSION_COMMAND is set");
    assert_eq!(
        vogt_session_agent::parse_command(Some(&raw)),
        Some(vec!["bash".to_string(), "-l".to_string()])
    );
}

/// The smoke harness's exact request against the stock `Pod` template (no
/// command of its own): the argv in the body must still reach the pod.
#[tokio::test]
async fn the_stock_pod_template_keeps_the_requested_command() {
    let mut h = harness_with(false, Some(SessionTemplate::default_templates())).await;
    let body = r#"{"name":"smoke-x","template":"pod","command":["bash","-l"]}"#;
    let created: Value = h
        .client
        .post(format!("{}/api/sessions", h.base))
        .header("content-type", "application/json")
        .body(body)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(created["id"].is_string(), "{created}");
    h.created.recv().await.expect("a pod is created");
    let pod = h.launcher.pods.lock().unwrap().pop().expect("pod created");
    let container = &pod.spec.unwrap().containers[0];
    let raw = container
        .env
        .as_ref()
        .unwrap()
        .iter()
        .find(|v| v.name == "VOGT_SESSION_COMMAND")
        .and_then(|v| v.value.clone())
        .expect("VOGT_SESSION_COMMAND is set");
    assert_eq!(
        vogt_session_agent::parse_command(Some(&raw)),
        Some(vec!["bash".to_string(), "-l".to_string()])
    );
}

#[tokio::test]
async fn output_before_attach_is_replayed_to_a_later_attach() {
    let mut h = harness(false).await;
    let id = h.start().await;
    let token = h.session_token(id).await;
    let mut agent = h.dial(id, &token, 0).await;
    assert_eq!(take_skip_resize(&mut agent).await, Frame::Ack { offset: 0 });
    put(
        &mut agent,
        Frame::Output {
            offset: 0,
            data: b"early bytes".to_vec(),
        },
    )
    .await;
    assert_eq!(take(&mut agent).await, Frame::Ack { offset: 11 });
    // Only now does a viewer attach: it must still be sent the bytes, and
    // bytes the agent sends afterwards fan out to it live.
    let mut viewer = ws_attach_with_token(&h.base, &id.to_string(), TEST_TOKEN).await;
    let mut seen = Vec::new();
    attach_collect(&mut viewer, &mut seen, "early bytes").await;
    put(
        &mut agent,
        Frame::Output {
            offset: 11,
            data: b" and live".to_vec(),
        },
    )
    .await;
    attach_collect(&mut viewer, &mut seen, "and live").await;
    // End the session so the pod's background task finishes.
    put(&mut agent, Frame::Exited { status: Some(0) }).await;
    tokio::time::timeout(Duration::from_secs(10), async {
        while !h.launcher.deleted.lock().unwrap().contains(&id) {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("the pod is deleted after exit");
}

#[tokio::test]
async fn output_input_resize_and_stop_round_trip() {
    let mut h = harness(false).await;
    let id = h.start().await;
    let token = h.session_token(id).await;
    let mut agent = h.dial(id, &token, 0).await;
    assert_eq!(take_skip_resize(&mut agent).await, Frame::Ack { offset: 0 });

    // Output reaches the session's subscribers and is acknowledged.
    put(
        &mut agent,
        Frame::Output {
            offset: 0,
            data: b"hello pod".to_vec(),
        },
    )
    .await;
    assert_eq!(take(&mut agent).await, Frame::Ack { offset: 9 });
    live_output_containing(&h.client, &h.base, &id.to_string(), &["hello pod"]).await;

    // Input, resize and stop go to the agent.
    let typed = h
        .client
        .post(format!("{}/api/sessions/{id}/input", h.base))
        .json(&json!({ "text": "ls" }))
        .send()
        .await
        .unwrap();
    assert_eq!(typed.status(), StatusCode::OK);
    match take(&mut agent).await {
        Frame::Input { data } => assert!(String::from_utf8_lossy(&data).contains("ls")),
        other => panic!("expected input, got {other:?}"),
    }
    h.state.sessions.get(id).unwrap().resize(100, 30).unwrap();
    assert_eq!(
        take(&mut agent).await,
        Frame::Resize {
            cols: 100,
            rows: 30
        }
    );

    // The kill salvages the checkout before it signals the child, and
    // waits for the agent's answer, so it is sent alongside.
    let kill = tokio::spawn({
        let client = h.client.clone();
        let url = format!("{}/api/sessions/{id}/kill", h.base);
        async move { client.post(url).send().await.unwrap() }
    });
    assert_eq!(take(&mut agent).await, Frame::Salvage);
    answer_salvage(&mut agent).await;
    assert_eq!(
        take(&mut agent).await,
        Frame::Signal {
            signal: "kill".into()
        }
    );
    assert_eq!(kill.await.unwrap().status(), StatusCode::OK);

    // The agent reports the child's exit; the session ends with it and
    // the pod is removed.
    put(&mut agent, Frame::Exited { status: Some(7) }).await;
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if h.detail(id).await.summary.exit_code == Some(7) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("the session records the agent's exit status");
    tokio::time::timeout(Duration::from_secs(10), async {
        while !h.launcher.deleted.lock().unwrap().contains(&id) {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("the pod is deleted after exit");
}

#[tokio::test]
async fn excessive_gap_is_rejected_without_poisoning_reconnect() {
    let mut harness = harness(false).await;
    let id = harness.start().await;
    let token = harness.session_token(id).await;
    let mut agent = harness.dial(id, &token, 0).await;
    assert_eq!(take_skip_resize(&mut agent).await, Frame::Ack { offset: 0 });
    put(
        &mut agent,
        Frame::Gap {
            offset: 0,
            lost: u64::MAX,
        },
    )
    .await;
    assert!(matches!(take(&mut agent).await, Frame::Error { code, .. } if code == "invalid_gap"));
    drop(agent);
    let mut agent = harness.dial(id, &token, 0).await;
    assert_eq!(take_skip_resize(&mut agent).await, Frame::Ack { offset: 0 });
    put(
        &mut agent,
        Frame::Output {
            offset: 0,
            data: b"alive".to_vec(),
        },
    )
    .await;
    assert_eq!(take(&mut agent).await, Frame::Ack { offset: 5 });
    put(&mut agent, Frame::Exited { status: Some(0) }).await;
    tokio::time::timeout(Duration::from_secs(10), async {
        while harness.detail(id).await.summary.exit_code.is_none() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the agent exit is processed before runtime shutdown");
}

#[tokio::test]
async fn reconnect_replays_exactly_the_unacked_bytes() {
    let mut h = harness(false).await;
    let id = h.start().await;
    let token = h.session_token(id).await;

    let mut agent = h.dial(id, &token, 0).await;
    assert_eq!(take_skip_resize(&mut agent).await, Frame::Ack { offset: 0 });
    put(
        &mut agent,
        Frame::Output {
            offset: 0,
            data: b"AAAA".to_vec(),
        },
    )
    .await;
    assert_eq!(take(&mut agent).await, Frame::Ack { offset: 4 });
    drop(agent);

    // The link drops. The agent redials believing only 2 bytes were
    // acknowledged, so it replays from 2: the overlap must not repeat.
    let mut agent = h.dial(id, &token, 2).await;
    assert_eq!(take_skip_resize(&mut agent).await, Frame::Ack { offset: 4 });
    put(
        &mut agent,
        Frame::Output {
            offset: 2,
            data: b"AABBBB".to_vec(),
        },
    )
    .await;
    assert_eq!(take(&mut agent).await, Frame::Ack { offset: 8 });
    live_output_containing(&h.client, &h.base, &id.to_string(), &["AAAABBBB"]).await;
    assert_eq!(h.scrollback(id).await, b"AAAABBBB");

    // A resend of bytes already held changes nothing.
    put(
        &mut agent,
        Frame::Output {
            offset: 4,
            data: b"BBBB".to_vec(),
        },
    )
    .await;
    assert_eq!(take(&mut agent).await, Frame::Ack { offset: 8 });
    assert_eq!(h.scrollback(id).await, b"AAAABBBB");

    // A hole is refused, not papered over.
    put(
        &mut agent,
        Frame::Output {
            offset: 100,
            data: b"X".to_vec(),
        },
    )
    .await;
    match take_skip_resize(&mut agent).await {
        Frame::Error { code, .. } => assert_eq!(code, "output_gap"),
        other => panic!("expected an error frame, got {other:?}"),
    }
    assert_eq!(
        take(&mut agent).await,
        Frame::Resync { offset: 8 },
        "a refused hole names where the stream ends"
    );
    assert_eq!(h.scrollback(id).await, b"AAAABBBB");

    // The agent had trimmed 8..100 from its buffer before the engine
    // acknowledged it: it says so, and the output goes on after the loss.
    put(
        &mut agent,
        Frame::Gap {
            offset: 8,
            lost: 92,
        },
    )
    .await;
    assert_eq!(take(&mut agent).await, Frame::Ack { offset: 100 });
    put(
        &mut agent,
        Frame::Output {
            offset: 100,
            data: b"CC".to_vec(),
        },
    )
    .await;
    assert_eq!(take(&mut agent).await, Frame::Ack { offset: 102 });
    live_output_containing(
        &h.client,
        &h.base,
        &id.to_string(),
        &["AAAABBBB", "[92 bytes lost]", "CC"],
    )
    .await;

    // A resume cursor past the stream (now at 102) is refused.
    let mut stale = h.dial(id, &token, 999).await;
    match take_skip_resize(&mut stale).await {
        Frame::Error { code, .. } => assert_eq!(code, "resume_beyond_stream"),
        other => panic!("expected an error frame, got {other:?}"),
    }
    // End the session so the test's runtime can wind down.
    put(&mut agent, Frame::Exited { status: Some(0) }).await;
}

#[tokio::test]
async fn stopping_before_the_agent_attaches_ends_the_session() {
    let mut h = harness(false).await;
    let id = h.start().await;
    let _ = h.created.recv().await.unwrap();
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
    .expect("the session ends without an agent");
}

#[tokio::test]
async fn stopping_a_session_revokes_its_graft_token_at_graft() {
    let graft = FakeGraft::default();
    let (minter, _dir) = real_minter(graft.clone()).await;
    let mut h = core_harness(false).await;
    install_minter(&h, minter);
    let id = h
        .start_with(
            json!({ "source": graft_source(), "session_ref": "ses_00000000000000000000000001" }),
        )
        .await;
    let (_, boot) = h.created.recv().await.unwrap();
    let body: Value = h.bootstrap(id, &boot).await.json().await.unwrap();
    let token = body["source_token"].as_str().unwrap().to_string();
    assert_eq!(token, "fake-minted-1");
    // Minted with the engine's mint credential, for this session.
    {
        let g = graft.0.lock().unwrap();
        assert_eq!(
            g.minted[0].0.as_deref(),
            Some("Bearer engine-mint-credential")
        );
        assert_eq!(
            g.minted[0].1.session.as_deref(),
            Some("ses_00000000000000000000000001"),
            "the mint binds the core session id when the start named one"
        );
        assert!(
            g.revokes.is_empty(),
            "nothing is revoked while the session runs"
        );
    }

    kill_and_wait_exit(&h, id).await;
    until("the revoke", || !graft.0.lock().unwrap().revokes.is_empty()).await;
    let g = graft.0.lock().unwrap();
    assert_eq!(g.revokes.len(), 1);
    // The engine revoked the minted token with its mint credential.
    assert_eq!(
        g.revokes[0].0.as_deref(),
        Some("Bearer engine-mint-credential")
    );
    assert_eq!(g.revokes[0].1, token);
    assert!(g.revoked.contains(&token), "graft now refuses the token");
}

#[tokio::test]
async fn a_failed_revoke_is_audited_and_the_stop_still_completes() {
    const SECRET: &str = "fake-minted-1";
    let logs = global_logs();
    let graft = FakeGraft::default();
    graft.0.lock().unwrap().revoke_fails = Some(tonic::Code::Internal);
    let (minter, _dir) = real_minter(graft.clone()).await;
    let mut h = core_harness(false).await;
    install_minter(&h, minter);
    let id = h.start_with(json!({ "source": graft_source() })).await;
    let _ = h.created.recv().await.unwrap();

    // The stop completes: the session exits and its pod is deleted.
    kill_and_wait_exit(&h, id).await;
    until("the pod delete", || {
        h.launcher.deleted.lock().unwrap().contains(&id)
    })
    .await;
    until("the revoke attempt", || {
        !graft.0.lock().unwrap().revokes.is_empty()
    })
    .await;
    until("the audit line", || {
        String::from_utf8_lossy(&logs.lock().unwrap()).contains("could not revoke")
    })
    .await;
    let logged = String::from_utf8(logs.lock().unwrap().clone()).unwrap();
    assert!(logged.contains("vogt::audit"), "{logged}");
    assert!(logged.contains(&id.to_string()), "{logged}");
    assert!(!logged.contains(SECRET), "graft token in the logs");
    assert!(
        !logged.contains("engine-mint-credential"),
        "mint token in the logs"
    );
    assert!(graft.0.lock().unwrap().revoked.is_empty());
}

#[tokio::test]
async fn a_pod_create_that_fails_after_the_mint_still_revokes_the_token() {
    let graft = FakeGraft::default();
    let (minter, _dir) = real_minter(graft.clone()).await;
    let h = core_harness(true).await;
    install_minter(&h, minter);
    let _id = h.start_with(json!({ "source": graft_source() })).await;
    until("the revoke", || !graft.0.lock().unwrap().revokes.is_empty()).await;
    assert_eq!(graft.0.lock().unwrap().revokes[0].1, "fake-minted-1");
}

/// WI-4230: the engine cannot tell whose id a `session_ref` is, so only
/// vogt-core may name one.
#[tokio::test]
async fn a_caller_other_than_vogt_core_cannot_name_a_session_ref() {
    let h = harness_cfg(false, None, |cfg| {
        cfg.vogt_core_token = Some(GRAFT_CORE_TOKEN.into())
    })
    .await;
    let status = post_with_session_ref(&h, "ses_01ARZ3NDEKTSV4RRFFQ69G5FAV").await;
    assert_eq!(status, reqwest::StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn vogt_core_cannot_name_a_malformed_session_ref() {
    let h = core_harness(false).await;
    for bad in [
        "ses_x",
        "../../etc/x",
        "ses_01ARZ3NDEKTSV4RRFFQ69G5FAV/../x",
        "ses_01ARZ3NDEKTSV4RRFFQ69G5FAI",
        "ses_01ARZ3NDEKTSV4RRFFQ69G5FAVV",
        "01ARZ3NDEKTSV4RRFFQ69G5FAV",
    ] {
        assert_eq!(
            post_with_session_ref(&h, bad).await,
            reqwest::StatusCode::BAD_REQUEST,
            "{bad}"
        );
    }
}

#[tokio::test]
async fn a_pod_gets_its_own_scoped_graft_token_that_never_leaks() {
    const SECRET: &str = "minted-graft-token-SECRET-1234";
    let logs = global_logs();
    let mut h = core_harness(false).await;
    let minter = FakeMinter::new(Ok(SECRET));
    install_minter(&h, minter.clone());
    let id = h
        .start_with(json!({
            "work_item": "WI-3229",
            "session_ref": "ses_01ARZ3NDEKTSV4RRFFQ69G5FAV",
            "source": graft_source(),
            "budget": { "session_wall_clock_minutes": 60 },
        }))
        .await;
    let (created_id, token) = h.created.recv().await.expect("a pod is created");
    assert_eq!(created_id, id);

    // The token was minted for this session, for its repo and line only,
    // living for the session budget plus slack.
    let grants = minter.grants.lock().unwrap().clone();
    assert_eq!(grants.len(), 1);
    // Bound to the core's session id: that is the id the worker's brief
    // names and the id its commits carry.
    assert_eq!(grants[0].session, "ses_01ARZ3NDEKTSV4RRFFQ69G5FAV");
    assert_eq!(grants[0].work_item.as_deref(), Some("WI-3229"));
    assert_eq!(grants[0].ttl_s, 60 * 60 + 30 * 60);
    assert_eq!(
        grants[0].scopes(),
        [
            "repo:vogt:read",
            "repo:vogt:write",
            "repo:vogt:drafts",
            "repo:vogt:land:rust-2"
        ]
    );

    // Not in the pod spec, however it is spelt.
    let pod = h.launcher.pods.lock().unwrap().pop().expect("pod created");
    assert!(!serde_json::to_string(&pod).unwrap().contains(SECRET));

    // Held for the bootstrap request, and delivered there once.
    let boot = h.bootstrap(id, &token).await;
    assert_eq!(boot.status(), StatusCode::OK);
    let body: Value = boot.json().await.unwrap();
    assert_eq!(body["source_token"], SECRET);
    assert_eq!(body["graft_session"], "ses_01ARZ3NDEKTSV4RRFFQ69G5FAV");
    assert!(!body["env"].to_string().contains(SECRET));
    assert!(!body["bundle"].to_string().contains(SECRET));
    assert_eq!(
        h.bootstrap(id, &token).await.status(),
        StatusCode::UNAUTHORIZED
    );

    // Not in the session's API view, its output, or any log line.
    assert!(!serde_json::to_string(&h.detail(id).await)
        .unwrap()
        .contains(SECRET));
    let out = String::from_utf8_lossy(&h.scrollback(id).await).into_owned();
    assert!(!out.contains(SECRET));
    let logged = String::from_utf8(logs.lock().unwrap().clone()).unwrap();
    assert!(
        logged.contains("minted the session's graft token"),
        "{logged}"
    );
    // The line is this session's, not some sibling test's.
    assert!(logged.contains(&id.to_string()), "{logged}");
    assert!(!logged.contains(SECRET), "graft token in the logs");
}
