use super::*;

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

    let list: Value = client
        .get(format!("{base}/api/sessions"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let list: Vec<Value> = serde_json::from_value(list["sessions"].clone()).unwrap();
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

    // The child can exit before the PTY reader has drained what it wrote:
    // process exit and output accounting complete independently, exactly as
    // archival and indexing do in the history test below. Waiting only for
    // `exit_code` therefore observed a scrollback of 0 about one run in five
    // — a flake that only became visible once CI started running this suite
    // on every push. Poll until the accounting settles, on a deadline, so a
    // count that is genuinely wrong still fails rather than hanging.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let detail: SessionDetail = loop {
        let detail: SessionDetail = client
            .get(format!("{base}/api/sessions/{id}"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if detail.summary.exit_code.is_some() && detail.summary.scrollback_bytes == 11 {
            break detail;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "session never accounted for the 11 bytes it wrote; \
             scrollback_bytes={}, exit_code={:?}",
            detail.summary.scrollback_bytes,
            detail.summary.exit_code
        );
        tokio::time::sleep(Duration::from_millis(40)).await;
    };

    let snapshot = base64::engine::general_purpose::STANDARD
        .decode(detail.scrollback_base64.as_bytes())
        .unwrap();
    assert_eq!(detail.summary.scrollback_bytes, 11);
    assert_eq!(snapshot, b"defghijk");
}

#[tokio::test]
async fn session_child_receives_its_own_session_id() {
    // The session id is allocated before the spawn so the child can be told
    // which session it is: `VOGT_ENGINE_SESSION` is only the display name, which
    // is not unique and cannot identify a session.
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let created: Value = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "session-id-env",
            "command": ["/bin/sh", "-lc", "printf %s \"$VOGT_ENGINE_SESSION_ID\""],
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap().to_string();

    // The exit can be recorded before the last of the child's output is read
    // off the PTY, so keep reading until the id shows or the deadline passes.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let printed = loop {
        let detail: SessionDetail = client
            .get(format!("{base}/api/sessions/{id}"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let snapshot = base64::engine::general_purpose::STANDARD
            .decode(detail.scrollback_base64.as_bytes())
            .unwrap();
        let printed = String::from_utf8_lossy(&snapshot).into_owned();
        if printed.contains(&id)
            || (detail.summary.exit_code.is_some() && std::time::Instant::now() > deadline)
        {
            break printed;
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    };
    assert!(
        printed.contains(&id),
        "child should see its own session id; got {printed:?}"
    );
}

#[tokio::test]
async fn every_session_is_told_the_engine_url_and_a_caller_can_override_it() {
    // A session opened from the GUI never passes through vogt-core, which is
    // what used to set VOGT_ENGINE_URL; the engine now sets it itself, from
    // its bind address (a wildcard bind reads as loopback).
    let (base, _h) = boot_with_config(Config {
        bind: "0.0.0.0:48910".parse().unwrap(),
        ..test_config()
    })
    .await;
    let printed = session_output(
        &base,
        json!({
            "name": "engine-url-env",
            "command": ["/bin/sh", "-lc", "printf '<%s>' \"$VOGT_ENGINE_URL\""],
        }),
        "<http://127.0.0.1:48910>",
    )
    .await;
    assert!(
        printed.contains("<http://127.0.0.1:48910>"),
        "child should see the engine URL; got {printed:?}"
    );

    // The caller's value (vogt-core's view of the engine) wins.
    let printed = session_output(
        &base,
        json!({
            "name": "engine-url-env-override",
            "command": ["/bin/sh", "-lc", "printf '<%s>' \"$VOGT_ENGINE_URL\""],
            "env": [["VOGT_ENGINE_URL", "http://engine.example:8910"]],
        }),
        "<http://engine.example:8910>",
    )
    .await;
    assert!(
        printed.contains("<http://engine.example:8910>"),
        "a caller-supplied VOGT_ENGINE_URL should win; got {printed:?}"
    );
}

/// A *live* session's output is searchable before it exits, via the
/// on-demand bounded scan (`include_live`, on by default), and the hit is
/// marked `live: true`. `include_live=false` restores archive-only behaviour,
/// so the same still-running session is not found there.
#[tokio::test]
async fn live_session_output_is_searchable_before_exit() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = test_config();
    cfg.default_cwd = tmp.path().to_path_buf();
    cfg.workspace_root = tmp.path().canonicalize().unwrap();

    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    // Prints a unique needle then stays alive — never archived during the test.
    let id: String = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "live-search-me",
            "command": ["/bin/sh", "-lc", "printf 'live-needle-zzz here\\n'; sleep 30"],
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

    // The needle reaches scrollback shortly after spawn; the live scan then
    // finds it. Same deadline discipline as the archive-search test.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let hits: Vec<Value> = client
            .get(format!("{base}/api/history/search?q=live-needle-zzz"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if let Some(hit) = hits.iter().find(|hit| hit["session_id"] == id) {
            assert_eq!(
                hit["live"], true,
                "a live-session hit must be flagged: {hit:?}"
            );
            assert!(
                hit["match_snippet"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("live-needle-zzz"),
                "live snippet must preserve the matched output as data: {hit:?}"
            );
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "live search should find a running session's output; got {hits:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Archive-only search excludes the still-running session.
    let archive_only: Vec<Value> = client
        .get(format!(
            "{base}/api/history/search?q=live-needle-zzz&include_live=false"
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        !archive_only.iter().any(|hit| hit["session_id"] == id),
        "include_live=false must not return a live (unarchived) session: {archive_only:?}"
    );
}

/// WI-827/832/833 end to end: an agent CLI started with a brief is handed a
/// first prompt naming the brief file (so it begins the task rather than
/// opening idle), a fresh Claude Code launch is pinned to the engine's
/// session id as its conversation id, and the quiet defaults reach its env.
/// A stub named `claude` prints what it was given, standing in for the CLI.
#[tokio::test]
async fn an_agent_started_with_a_brief_is_told_to_read_it() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = test_config();
    cfg.default_cwd = tmp.path().to_path_buf();
    cfg.workspace_root = tmp.path().canonicalize().unwrap();
    cfg.state_dir = tmp.path().join("state");

    let bin = tmp.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let stub = bin.join("claude");
    std::fs::write(
        &stub,
        // One write, then a pause: the helper below stops reading once the
        // child has exited, so output must not trail the exit.
        "#!/bin/sh\nout=$(printf 'arg=[%s]\\n' \"$@\")\n\
         printf '%s\\nsuggest=[%s]\\n' \"$out\" \"$CLAUDE_CODE_ENABLE_PROMPT_SUGGESTION\"\n\
         sleep 0.3\n",
    )
    .unwrap();
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let created: Value = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "agent with a task",
            "prompt": "Context.\n\n## Task\n\nCheck the containers.\n",
            "command": [stub.to_string_lossy()],
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap().to_string();
    let prompt_path = tmp
        .path()
        .join("state")
        .join("agent-task-prompts")
        .join("sessions")
        .join(format!("{id}.md"));

    let printed = session_output_after_exit(&client, &base, &id, &["suggest=["])
        .await
        .printed;
    assert!(
        printed.contains(&format!("arg=[--session-id]\r\narg=[{id}]")),
        "a fresh claude launch should carry the session id; got {printed:?}"
    );
    assert!(
        printed.contains(&format!(
            "arg=[Vogt started this session with a brief in {}.",
            prompt_path.display()
        )),
        "the first prompt should name the brief file; got {printed:?}"
    );
    assert!(
        !printed.contains("Check the containers"),
        "the brief's text must not be argv; got {printed:?}"
    );
    assert!(
        printed.contains("suggest=[false]"),
        "prompt suggestions should be off; got {printed:?}"
    );
}

#[tokio::test]
async fn a_resume_for_a_shell_is_refused() {
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
    for (command, resume) in [
        (json!(["bash"]), "0f8fad5b-d9cb-469f-a165-70867728950e"),
        (json!(["claude"]), "--dangerously-skip-permissions"),
    ] {
        let status = client
            .post(format!("{base}/api/sessions"))
            .json(&json!({"name": "r", "command": command, "resume": resume}))
            .send()
            .await
            .unwrap()
            .status();
        assert_eq!(status, 400, "{command} resume {resume}");
    }
}

// ── a configuration that would hang refuses instead ───────────────────────

#[tokio::test]
async fn a_claude_route_on_the_openai_backend_refuses_with_a_named_reason() {
    // The recorded failure is that these proxy routes accept the request and
    // never answer. Under a 60-second client timeout that reads as "the
    // request took too long", which is a different sentence for the same
    // silence and sends an operator looking in the wrong place.
    let mut cfg = test_config();
    cfg.assistant_api_key = Some("sk-test".into());
    cfg.assistant_model = "claude-sonnet-4-5".into();
    let (base, _h) = boot_with_config(cfg).await;

    let res = reqwest::Client::new()
        .get(format!("{base}/api/assistant/history"))
        .headers(auth())
        .send()
        .await
        .unwrap();
    // Not 404: the assistant *is* provisioned, and reporting it absent would
    // send somebody looking for a missing API key.
    assert_ne!(res.status(), reqwest::StatusCode::NOT_FOUND);
    let body = res.text().await.unwrap();
    assert!(body.contains("claude-sonnet-4-5"), "{body}");
    assert!(body.contains("assistant_allow_claude_proxy"), "{body}");
}

// ── more than one client on one session, at the same time ─────────────────

#[tokio::test]
async fn two_clients_watch_one_session_at_once() {
    // "Multiple concurrent clients per session" is the conjunct, and the
    // existing multi-attach test closes the first socket before opening the
    // second — which exercises re-attachment, not concurrency. The difference
    // matters: a second attach that silently displaced the first would pass
    // that test and lose somebody's terminal.
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let id: String = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({ "name": "shared", "command": ["/bin/cat"] }))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let mut first = ws_attach(&base, &id).await;
    // Bounded, because the failure this is looking for is a *hang*: if the
    // server cannot accept a second socket while the first is attached, the
    // upgrade never completes and an unbounded await would take the suite
    // down with it rather than reporting.
    let mut second = tokio::time::timeout(Duration::from_secs(10), ws_attach(&base, &id))
        .await
        .expect("a second client must be able to attach while the first is attached");

    // Both are still attached, so both see the same output — the first is
    // asserted *after* the second connected, which is the whole point.
    first
        .send(Message::Binary(b"shared-line\n".to_vec().into()))
        .await
        .unwrap();

    let contains =
        |haystack: &[u8], needle: &[u8]| haystack.windows(needle.len()).any(|w| w == needle);
    let seen_by_first =
        collect_binary_until(&mut first, b"shared-line", Duration::from_secs(3)).await;
    let seen_by_second =
        collect_binary_until(&mut second, b"shared-line", Duration::from_secs(3)).await;
    assert!(
        contains(&seen_by_first, b"shared-line"),
        "the client that typed it must still be attached"
    );
    assert!(
        contains(&seen_by_second, b"shared-line"),
        "the second client must see output caused by the first — one PTY, two \
         watchers, which is what concurrent means here"
    );

    // And the second can type too: attaching is not read-only for whoever
    // arrived later.
    second
        .send(Message::Binary(b"from-the-second\n".to_vec().into()))
        .await
        .unwrap();
    let back = collect_binary_until(&mut first, b"from-the-second", Duration::from_secs(3)).await;
    assert!(
        contains(&back, b"from-the-second"),
        "the first client must see what the second typed"
    );

    // Close both sockets before the session goes. Every assertion above
    // passes without this and the test then hangs on teardown, which is worth
    // recording because a hang after the last assertion reads exactly like a
    // failure of the thing being tested — it had somebody looking for a
    // concurrency defect in the engine that was never there.
    //
    // The cause is ordinary refcounting, and it is why two sockets differ
    // from one: a socket's handler ends when its client disconnects or the
    // session's broadcast closes, and that broadcast cannot close while a
    // handler is holding the session alive. Killing the child is not enough;
    // the clients have to leave.
    let _ = first.close(None).await;
    let _ = second.close(None).await;
    let _ = client
        .delete(format!("{base}/api/sessions/{id}"))
        .send()
        .await;
}

#[tokio::test]
async fn bundle_route_refuses_an_oversize_body_unread_and_rate_limits_a_session() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = broker_config(&tmp, "RW proj rw ondemand,writable\n");
    let (base, state, _h) = boot_with_state(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let id = create_session_id(&client, &base, "bundle-limits").await;
    let session: uuid::Uuid = id.parse().unwrap();
    let token = state.sessions.secret_broker().issue(session);
    let plain = reqwest::Client::new();
    let url = format!("{base}/api/agent-auth/store/RW/bundle-file");

    // No token: nothing is counted or read.
    let r = plain.post(&url).body("x").send().await.unwrap();
    assert_eq!(r.status(), 401);

    // Past the 1 MiB payload bound (as base64 plus envelope): 413, whether the
    // length is declared up front or only found out while reading.
    let r = plain
        .post(&url)
        .bearer_auth(&token)
        .body(vec![b'x'; 2 * 1024 * 1024])
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 413);

    // Everything above counted. The window is 120 a minute for the session.
    let mut limited = None;
    for n in 0..130 {
        let r = plain
            .post(&url)
            .bearer_auth(&token)
            .body("not json")
            .send()
            .await
            .unwrap();
        if r.status() == 429 {
            limited = Some(n);
            assert!(r.headers().contains_key("retry-after"));
            break;
        }
        assert_eq!(r.status(), 400);
    }
    assert_eq!(
        limited,
        Some(119),
        "120 requests fit the window, the next is refused"
    );
}

/// WI-830: an exited child reads as exited and not alive, and stays that way
/// — the PTY reader's late drain must not flip it back to `running`/`idle`.
#[tokio::test]
async fn an_exited_session_reports_a_terminal_state_and_not_alive() {
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let ok = create_session_with(
        &client,
        &base,
        json!({ "name": "true-smoke", "command": ["/bin/sh", "-c", "printf 'bye\\n'; exit 0"] }),
    )
    .await;
    let failing = create_session_with(
        &client,
        &base,
        json!({ "name": "false-smoke", "command": ["/bin/sh", "-c", "printf 'oops\\n'; exit 3"] }),
    )
    .await;
    let live = create_session_with(
        &client,
        &base,
        json!({ "name": "cat", "command": ["/bin/cat"] }),
    )
    .await;

    // Wait for the whole terminal state, not just the exit code: the activity
    // can lag the exit code on a loaded host. Bounded by the helper's deadline.
    let row = wait_for_session_row(&client, &base, &ok, |s| {
        s["exit_code"] == json!(0) && s["activity"] == "exited" && s["alive"] == json!(false)
    })
    .await;
    // Then hold: nothing (the PTY reader's late drain, the idle window) may
    // move it off `exited`. Re-read the row over a span past the 200 ms idle
    // window instead of sleeping once.
    for _ in 0..12 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let later = wait_for_session_row(&client, &base, &ok, |_| true).await;
        assert_eq!(later["activity"], "exited", "{row:?} then {later:?}");
        assert_eq!(later["alive"], false, "{row:?} then {later:?}");
        assert_eq!(later["exit_code"], json!(0), "{row:?} then {later:?}");
    }

    let row = wait_for_session_row(&client, &base, &failing, |s| {
        s["exit_code"] == json!(3) && s["activity"] == "errored"
    })
    .await;
    assert_eq!(row["activity"], "errored", "{row:?}");
    assert_eq!(row["alive"], false, "{row:?}");

    let row = wait_for_session_row(&client, &base, &live, |_| true).await;
    assert_eq!(row["alive"], true, "{row:?}");
    assert!(row["exit_code"].is_null());

    // A stopped (killed) session is not alive either, and reads `stopped`
    // whatever its exit code (WI-913).
    client
        .post(format!("{base}/api/sessions/{live}/kill"))
        .send()
        .await
        .unwrap();
    let row = wait_for_session_row(&client, &base, &live, |s| {
        !s["exit_code"].is_null() && s["activity"] == "stopped"
    })
    .await;
    assert_eq!(row["alive"], false, "{row:?}");
    assert_eq!(row["activity"], "stopped", "{row:?}");

    // History records the exit and why it ended.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let rows: Vec<Value> = client
            .get(format!("{base}/api/history/sessions?limit=20"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if let Some(r) = rows
            .iter()
            .find(|r| r["id"] == ok && r["exit_code"] == json!(0))
        {
            assert!(r["ended_at"].is_string(), "{r:?}");
            assert_eq!(r["end_reason"], "exited", "{r:?}");
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "{rows:?}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// The economics tailer follows a live session's transcript and the session
/// row carries the reading; when the session ends, the in-process scrape
/// shows it. The registry is process-wide, so the assertion is that this
/// session's series appears, not an exact count.
#[tokio::test]
async fn the_economics_tailer_reports_a_live_transcript_and_the_scrape_records_the_exit() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = test_config();
    cfg.default_cwd = tmp.path().to_path_buf();
    cfg.workspace_root = tmp.path().canonicalize().unwrap();
    cfg.state_dir = tmp.path().join("state");
    cfg.economics_home = Some(tmp.path().to_path_buf());
    cfg.economics_tick = Duration::from_secs(1);

    let conversation = "6c1f0d2e-5b7a-4e1c-9f3d-2a8b7c6d5e4f";
    let transcript = tmp
        .path()
        .join(".claude/projects/cwd-key")
        .join(format!("{conversation}.jsonl"));
    std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    std::fs::write(
        &transcript,
        include_str!("../fixtures/economics/claude.jsonl"),
    )
    .unwrap();

    let (base, _guard) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let created: Value = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({ "name": "metered", "command": ["/bin/cat"] }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap().to_string();
    client
        .post(format!("{base}/api/sessions/{id}/conversation"))
        .json(&json!({ "agent": "claude", "id": conversation }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();

    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let row = loop {
        let row = session_row(&client, &base, &id).await;
        if row["economics"]["turns"] == 2 {
            break row;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the tailer never reported the transcript: {row}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    assert_eq!(row["economics"]["context_tokens"], 80);
    assert_eq!(row["economics"]["peak_context_tokens"], 175);
    assert_eq!(row["economics"]["compactions"], 1);
    assert_eq!(row["economics"]["usage_measured"], true);

    client
        .post(format!("{base}/api/sessions/{id}/kill"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let ended = loop {
        let row = session_row(&client, &base, &id).await;
        if !row["exit_code"].is_null() {
            break row;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the killed session never recorded an exit: {row}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert!(ended["exit_code"].is_number(), "{ended}");

    // The exit code is stored before the waiter observes the economics, and
    // that observation runs on the blocking pool. Under CI load the scrape
    // can land in the gap, so wait for the series rather than reading once.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let text = loop {
        let text = vogt_engine_server::metrics::metrics().render();
        if text.contains("vogt_session_turns_count{template=\"unknown\"}") {
            break text;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the exit was recorded but the scrape never showed the session's turns: {text}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert!(
        text.contains("vogt_session_peak_context_tokens_sum"),
        "{text}"
    );
    assert!(text.contains("vogt_session_compactions_bucket"), "{text}");
    drop(tmp);
}

/// The 2026-10-07 incident (WI-962): a `claude` typed by hand into a plain
/// shell. The engine saw a shell, the redeploy's SIGKILL left a record with
/// no conversation, boot dropped it, and History showed only `bash`. Now the
/// agent reports its conversation from inside the session, the record keeps
/// it, the session comes back hibernated and resumable, a wake starts the
/// agent on that conversation, and History shows and offers it.
#[tokio::test]
async fn a_claude_typed_into_a_shell_is_linked_kept_across_a_sigkill_and_resumed() {
    let (_tmp, cfg, _stub) = conversation_sandbox();
    let state_dir = cfg.state_dir.clone();
    let (base, guard) = boot_with_config(cfg.clone()).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let created: Value = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({ "name": "Oversight" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap().to_string();
    assert!(
        created.get("conversation").is_none(),
        "a shell, to start with"
    );

    let conversation = "6c1f0d2e-5b7a-4e1c-9f3d-2a8b7c6d5e4f";
    for bad in [
        json!({ "agent": "bash", "id": conversation }),
        json!({ "agent": "claude", "id": "../../etc/passwd" }),
    ] {
        let refused = client
            .post(format!("{base}/api/sessions/{id}/conversation"))
            .json(&bad)
            .send()
            .await
            .unwrap();
        assert_eq!(refused.status(), StatusCode::BAD_REQUEST, "{bad}");
    }
    let linked: Value = client
        .post(format!("{base}/api/sessions/{id}/conversation"))
        .json(&json!({ "agent": "claude", "id": conversation }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(linked["conversation"]["agent"], "claude");
    assert_eq!(linked["conversation"]["id"], conversation);

    // What a SIGKILL leaves behind is the write-ahead record: it now holds
    // the conversation.
    let record_path = state_dir.join("sessions").join(format!("{id}.json"));
    let record = std::fs::read_to_string(&record_path).unwrap();
    assert!(record.contains(conversation), "{record}");
    drop(record);

    // History knows what the session is, without waiting for it to end.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let row: Value = client
            .get(format!("{base}/api/history/{id}"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if row["conversation_id"] == conversation {
            assert_eq!(row["conversation_agent"], "claude");
            assert_eq!(row["role"], "worker");
            assert_eq!(row["resume_template"], "claude");
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "history never showed the conversation: {row}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // The engine dies without hibernating anything.
    lose_session(&client, &base, &id, &record_path).await;
    drop(guard);

    let (base, _h) = boot_with_config(cfg).await;
    let recovered = session_row(&client, &base, &id).await;
    assert_eq!(recovered["activity"], "hibernated", "{recovered}");
    assert_eq!(recovered["hibernation"]["trigger"], "recovered");
    assert_eq!(recovered["hibernation"]["resumable"], true);
    assert_eq!(recovered["conversation"]["id"], conversation);

    // Waking it starts the agent on its conversation, not an empty shell.
    let woken = client
        .post(format!("{base}/api/sessions/{id}/wake"))
        .send()
        .await
        .unwrap();
    assert_eq!(woken.status(), StatusCode::OK);
    let printed = live_output_containing(&client, &base, &id, &["helper=["]).await;
    assert!(
        printed.contains(&format!("arg=[--resume]\r\narg=[{conversation}]")),
        "{printed:?}"
    );

    // History still offers the resume after a restart.
    let rows: Vec<Value> = client
        .get(format!("{base}/api/history/sessions"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let row = rows.iter().find(|r| r["id"] == id.as_str()).unwrap();
    assert_eq!(row["conversation_id"], conversation);
    assert_eq!(row["resume_template"], "claude");
}

/// `vogt-claude-session-hook install`, which the entrypoint runs: both hooks
/// land in Claude Code's user settings once, beside what was there, and a
/// file that is not JSON is left alone.
#[test]
fn the_claude_session_hook_installs_itself_once_and_keeps_the_settings() {
    if std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("skipping: install is written in python3");
        return;
    }
    let hook = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../deploy/claude-session-hook.sh")
        .canonicalize()
        .unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let settings = tmp.path().join("settings.json");
    std::fs::write(
        &settings,
        r#"{"theme":"dark","hooks":{"Stop":[{"hooks":[{"type":"command","command":"x"}]}]}}"#,
    )
    .unwrap();
    for _ in 0..2 {
        let status = std::process::Command::new(&hook)
            .arg("install")
            .arg(&settings)
            .status()
            .unwrap();
        assert!(status.success());
    }
    let written: Value =
        serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
    assert_eq!(written["theme"], "dark");
    assert_eq!(written["hooks"]["Stop"][0]["hooks"][0]["command"], "x");
    for (event, arg) in [("SessionStart", "start"), ("SessionEnd", "end")] {
        let groups = written["hooks"][event].as_array().unwrap();
        assert_eq!(groups.len(), 1, "{event} added once: {written}");
        assert_eq!(
            groups[0]["hooks"][0]["command"],
            format!("{} {arg}", hook.display())
        );
    }
    let broken = tmp.path().join("broken.json");
    std::fs::write(&broken, "not json").unwrap();
    let status = std::process::Command::new(&hook)
        .arg("install")
        .arg(&broken)
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(std::fs::read_to_string(&broken).unwrap(), "not json");
}

#[tokio::test]
async fn a_policy_check_is_the_sessions_own_call() {
    // The route sits outside the bearer gate, like the conversation report,
    // because the caller is the session. What authenticates it is the broker
    // token the engine issued that session; anything else is refused before
    // the body is read.
    let (_tmp, cfg, _stub) = hibernation_sandbox();
    let (base, _h) = boot_with_config(cfg).await;
    let anonymous = reqwest::Client::new();
    for bearer in [None, Some("not-a-broker-token")] {
        let mut request = anonymous
            .post(format!("{base}/api/agent-auth/policy-check"))
            .json(&json!({ "tool_name": "Bash", "tool_input": { "command": "git push" } }));
        if let Some(bearer) = bearer {
            request = request.bearer_auth(bearer);
        }
        assert_eq!(
            request.send().await.unwrap().status(),
            StatusCode::UNAUTHORIZED
        );
    }
}

#[tokio::test]
async fn a_sessions_process_tree_is_measured_and_rides_on_its_summary() {
    use vogt_engine_server::resources::Sampler;

    let (tmp, cfg, _stub) = hibernation_sandbox();
    let (base, state, _h) = boot_with_state(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    // A tree of three: the stub, a child holding ~32 MiB, and a sleeper.
    let script = "#!/bin/sh\nsleep 300 &\n\
                  python3 -c 'import time; b=bytearray(32<<20); time.sleep(300)' &\n\
                  printf 'helper=[%s]\\n> ' \"$!\"\nwait\n";
    let ids = start_stub_agents(
        &client,
        &base,
        &tmp.path().join("agents"),
        &[("heavy", script)],
    )
    .await;
    let mut events = state.bus.subscribe();
    let mut sampler = Sampler::new(Some(16 << 20));
    // Until the child has allocated its 32 MiB; a loaded runner is slow to
    // start python.
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        sampler.run_once(&state).await;
        let session = state.sessions.get(ids[0].parse().unwrap()).unwrap();
        let rss = session.summary().resources.map_or(0, |r| r.rss_bytes);
        if rss >= 32 << 20 || std::time::Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    let listed: Value = client
        .get(format!("{base}/api/sessions"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let listed: Vec<Value> = serde_json::from_value(listed["sessions"].clone()).unwrap();
    let row = listed.iter().find(|s| s["id"] == ids[0].as_str()).unwrap();
    let resources = &row["resources"];
    assert!(
        resources["rss_bytes"].as_u64().unwrap() >= 32 << 20,
        "{resources}"
    );
    assert!(resources["processes"].as_u64().unwrap() >= 3, "{resources}");
    assert_eq!(resources["over_threshold"], true);
    let event = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Ok(ServerEvent::SessionResources { samples }) = events.recv().await {
                return samples;
            }
        }
    })
    .await
    .unwrap();
    assert!(event.iter().any(|s| s.id.to_string() == ids[0]));
}

/// WI-1255: a person's token keeps the Git panel and may commit for a
/// session; vogt-core's own credential (the stack secret, which
/// `session.commit_for` presents after its own check) may commit for a
/// session and nothing else. Neither commits a worker's work into the repo's
/// main checkout: without `worktree` the commit for a pool worker launched
/// there is refused, and nothing is committed.
#[tokio::test]
async fn a_person_and_the_core_commit_for_a_worker_but_never_in_the_main_checkout() {
    let door = wi1255_front_door().await;
    door.stage_work("w.txt");
    let repo_before = door.commit_count(&door.repo);
    let work_before = door.commit_count(&door.work);

    for token in [WI1255_PERSON, WI1255_STACK, TEST_TOKEN] {
        let (status, said) = door.git_op(token, door.commit_for_worker(false)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{token} {said}");
        assert!(
            said["error"].as_str().unwrap().contains("main checkout"),
            "{said}"
        );
    }
    assert_eq!(door.commit_count(&door.repo), repo_before);

    let (status, said) = door
        .git_op(WI1255_STACK, door.commit_for_worker(true))
        .await;
    assert_eq!(status, StatusCode::OK, "{said}");
    assert_eq!(door.commit_count(&door.work), work_before + 1);

    door.stage_work("p.txt");
    let (status, said) = door
        .git_op(WI1255_PERSON, door.commit_for_worker(true))
        .await;
    assert_eq!(status, StatusCode::OK, "{said}");
    assert_eq!(door.commit_count(&door.work), work_before + 2);

    // The core's credential is not a Git panel.
    let (status, said) = door
        .git_op(
            WI1255_STACK,
            json!({ "op": "stage", "repo": "repo", "path": "a.txt" }),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{said}");

    // A person's Git panel works as before.
    std::fs::write(door.repo.join("a.txt"), "one\ntwo\n").unwrap();
    let (status, said) = door
        .git_op(
            WI1255_PERSON,
            json!({ "op": "stage", "repo": "repo", "path": "a.txt" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{said}");
    let (status, said) = door
        .git_op(
            WI1255_PERSON,
            json!({ "op": "commit", "repo": "repo", "message": "by hand" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{said}");
    assert_eq!(door.commit_count(&door.repo), repo_before + 1);

    // A person may still nominate an overseer.
    let promoted = client_for(WI1255_PERSON)
        .post(format!("{}/api/sessions/{}/role", door.base, door.worker))
        .json(&json!({ "role": "oversight" }))
        .send()
        .await
        .unwrap();
    assert_eq!(promoted.status(), StatusCode::OK);
}

/// WI-4489: a child never outranks the session that made it. An agent token
/// may not name a parent or ask for the full MCP set, and its own unnamed
/// start is on the worker set. vogt-core (the stack secret) naming a worker
/// as the parent cannot get an oversight session or the full set either,
/// and an unnamed set under that parent is the worker set.
#[tokio::test]
async fn a_child_never_outranks_the_session_that_started_it() {
    let door = wi1255_front_door().await;
    let spec = |extra: Value| {
        let mut body = json!({
            "name": "child",
            "cwd": door.repo.to_string_lossy(),
            "command": ["/bin/bash", "-c", "sleep 300"],
        });
        for (k, v) in extra.as_object().unwrap() {
            body[k] = v.clone();
        }
        body
    };
    let post = |token: &'static str, body: Value| {
        let base = door.base.clone();
        async move {
            client_for(token)
                .post(format!("{base}/api/sessions"))
                .json(&body)
                .send()
                .await
                .unwrap()
        }
    };
    let worker = door.worker.to_string();

    for token in [WI1255_WORKER, WI1255_STRANGER] {
        for extra in [json!({"mcp_set": "full"}), json!({"started_by": worker})] {
            let res = post(token, spec(extra.clone())).await;
            assert_eq!(res.status(), StatusCode::FORBIDDEN, "{token} {extra}");
        }
        let res = post(token, spec(json!({}))).await;
        assert_eq!(res.status(), StatusCode::OK, "{token}");
        let row: Value = res.json().await.unwrap();
        assert_eq!(row["mcp_set"], "worker", "{token} {row}");
    }

    for extra in [
        json!({"started_by": worker, "mcp_set": "full"}),
        json!({"started_by": worker, "role": "oversight"}),
        json!({"started_by": uuid::Uuid::new_v4().to_string(), "mcp_set": "full"}),
    ] {
        let res = post(WI1255_STACK, spec(extra.clone())).await;
        assert_eq!(res.status(), StatusCode::FORBIDDEN, "{extra}");
        let said: Value = res.json().await.unwrap();
        assert!(
            said["error"]
                .as_str()
                .unwrap()
                .contains("child never gets more standing"),
            "{said}"
        );
    }
    let res = post(WI1255_STACK, spec(json!({"started_by": worker}))).await;
    assert_eq!(res.status(), StatusCode::OK);
    let row: Value = res.json().await.unwrap();
    assert_eq!(row["mcp_set"], "worker", "{row}");

    // A person's own start, with no parent, may still ask for the full set.
    let res = post(WI1255_PERSON, spec(json!({"mcp_set": "full"}))).await;
    assert_eq!(res.status(), StatusCode::OK);
}

/// WI-4186 review: Claude Code shows no turn-state title, so the watchdog has
/// no proof its first turn ran and is not started for it at all, autopilot
/// or not: nothing is typed or pressed into a Claude session.
#[tokio::test]
async fn a_claude_session_is_not_watched_so_nothing_is_typed_into_it() {
    let tmp = tempfile::tempdir().unwrap();
    let (base, state, _h) = boot_with_state(first_turn_config()).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    for autopilot in [false, true] {
        let id = start_briefed_agent(
            &client,
            &base,
            &tmp.path().join(format!("claude-{autopilot}")),
            "claude",
            &format!("#!/bin/bash\nprintf '> '\n{LOG_READS}"),
            autopilot,
        )
        .await;
        let session = state.sessions.get(id).unwrap();
        let reads = reads_of(&session).await;
        assert!(reads.is_empty(), "autopilot {autopilot}: {reads:?}");
        session.kill().unwrap();
    }
}
