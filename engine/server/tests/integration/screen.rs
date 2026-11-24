use super::*;

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

/// WI-1090: an idle agent's input box repaints its blinking cursor faster
/// than the quiet window, forever. Output that leaves the screen as it was
/// is not activity, so the session settles to `idle` and its screen reads
/// `ready`, while a TUI whose repaints do change the screen (a spinner)
/// stays `running`.
#[tokio::test]
async fn an_identical_repaint_is_not_activity_but_a_spinner_is() {
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    // Named `claude`, so the session has an agent whose prompt the engine reads.
    let fake = |name: &str, body: &str| {
        let sub = dir.path().join(name);
        std::fs::create_dir_all(&sub).unwrap();
        let path = sub.join("claude");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path.to_string_lossy().into_owned()
    };
    // The idle frame: a footer, the input box, the status bar.
    let frame = r"printf '\033[2J\033[H  done in 9m07s\r\n\342\225\255\342\224\200\342\224\200\342\225\256\r\n\342\224\202 \342\200\272 Ask the model\342\200\246 \342\224\202\r\n\342\225\260\342\224\200\342\224\200\342\225\257\r\n  model-1 \302\267 3 turns\r'";
    // Then the blink: the cursor cell flips between reverse video and dim
    // every 80 ms, under the suite's 200 ms quiet window.
    let idle = fake(
        "idle",
        &format!(
            "{frame}\nn=0\nwhile :; do n=$((n+1)); if [ $((n%2)) = 0 ]; then a='\\033[7m'; else a='\\033[2m'; fi\n\
             printf \"\\033[3;1H\\342\\224\\202 \\342\\200\\272 ${{a}}A\\033[0msk the model\\342\\200\\246 \\342\\224\\202\\033[K\\033[5;1H\"; sleep 0.08; done"
        ),
    );
    let spinning = fake(
        "spinning",
        &format!(
            "{frame}\nn=0\nwhile :; do n=$((n+1)); printf \"\\033[1;1H  thinking $n (esc to interrupt)\\033[K\\033[5;1H\"; sleep 0.08; done"
        ),
    );

    let start = |command: String| {
        let client = client.clone();
        let base = base.clone();
        async move {
            let created: Value = client
                .post(format!("{base}/api/sessions"))
                .json(&json!({ "name": "repaint", "command": [command] }))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            created["id"].as_str().unwrap().to_string()
        }
    };
    let idle_id = start(idle).await;
    let spinning_id = start(spinning).await;

    let screen = |id: String| {
        let client = client.clone();
        let base = base.clone();
        async move {
            client
                .get(format!("{base}/api/sessions/{id}/screen"))
                .send()
                .await
                .unwrap()
                .json::<Value>()
                .await
                .unwrap()
        }
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let settled = loop {
        let view = screen(idle_id.clone()).await;
        if view["activity"] == "idle" && view["ready"] == true {
            break view;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "a repainting idle prompt never settled: {view}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert!(
        settled["lines"][2]
            .as_str()
            .unwrap()
            .contains("Ask the model"),
        "{settled}"
    );
    // It is still repainting: its output clock keeps moving while it reads idle.
    let before = settled["last_output_at"].clone();
    tokio::time::sleep(Duration::from_millis(400)).await;
    let later = screen(idle_id.clone()).await;
    assert_ne!(later["last_output_at"], before, "{later}");
    assert_eq!(later["activity"], "idle", "{later}");
    assert_eq!(later["ready"], true, "{later}");

    let spinner = screen(spinning_id.clone()).await;
    assert_eq!(spinner["activity"], "running", "{spinner}");
    assert_eq!(spinner["ready"], false, "{spinner}");

    for id in [idle_id, spinning_id] {
        let _ = client
            .delete(format!("{base}/api/sessions/{id}"))
            .send()
            .await;
    }
}

#[tokio::test]
async fn the_activity_state_is_announced_on_the_server_wide_event_stream() {
    // This has two halves — the state is *derived from output heuristics*,
    // and it is *published on the server-wide SSE stream*. `activity.rs` owns
    // the first and asserts it four ways. The second was asserted by nothing:
    // the one activity test in this file polls `GET /api/sessions/{id}`, and
    // a bus publish that stopped happening would leave that test green and
    // every client on the stream showing a stale badge until it refreshed.
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    // Subscribe first. The stream is a live broadcast rather than a log, so a
    // reader that opens after the session has already spoken sees nothing and
    // would fail this test for the wrong reason.
    let stream_res = client
        .get(format!("{base}/api/events"))
        .send()
        .await
        .unwrap();
    assert_eq!(stream_res.status(), StatusCode::OK);
    assert_eq!(
        stream_res
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(|value| value.split(';').next().unwrap_or(value).trim().to_string()),
        Some("text/event-stream".to_string())
    );
    let mut stream = stream_res.bytes_stream();

    // A prompt the heuristics recognise, then a wait — so the state the
    // stream carries is one `activity.rs` derived from output rather than a
    // lifecycle state every session passes through.
    let created: Value = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "sse-activity",
            "command": ["/bin/sh", "-lc", "printf 'Continue? [y/N]'; sleep 30"],
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap().to_string();

    let want_id = id.clone();
    let event = event_matching(&mut stream, move |event| {
        event["type"] == "activity"
            && event["id"] == want_id.as_str()
            && event["state"] == "waiting-for-input"
    })
    .await;
    assert_eq!(
        event["state"], "waiting-for-input",
        "the first activity this session announced was not the state its \
         output implies: {event}"
    );

    // And the stream and the polled detail are the same fact, not two. A
    // stream that announced a state the session does not hold would be worse
    // than one that announced nothing.
    let detail: Value = client
        .get(format!("{base}/api/sessions/{id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(detail["summary"]["activity"], event["state"]);

    // Let go of the stream before the session goes: the handler holds a
    // subscriber until its client leaves.
    drop(stream);
    let _ = client
        .delete(format!("{base}/api/sessions/{id}"))
        .send()
        .await;
}

/// WI-831: `/screen` renders the visible grid with the documented shape.
#[tokio::test]
async fn session_screen_renders_the_visible_grid() {
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let id = create_session_with(
        &client,
        &base,
        json!({
            "name": "screen",
            "cols": 40,
            "rows": 5,
            "command": [
                "/bin/sh",
                "-c",
                "printf '\\033]0;my-title\\007\\033[2J\\033[1;1HIf\\033[1;4Hnothing\\033[1;12His   \\033[3;1HContinue? [y/N]'; sleep 30",
            ],
        }),
    )
    .await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let screen = loop {
        let resp = client
            .get(format!("{base}/api/sessions/{id}/screen"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let screen: Value = resp.json().await.unwrap();
        if screen["ready"] == true {
            break screen;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "screen never became ready: {screen:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };

    let keys: std::collections::BTreeSet<&str> = screen
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        [
            "activity",
            "alive",
            "cols",
            "cursor",
            "id",
            "last_output_at",
            "lines",
            "ready",
            "rows",
            "title",
            "turn_started_at",
        ]
        .into_iter()
        .collect(),
        "{screen:?}"
    );
    assert_eq!(screen["id"], id);
    assert_eq!(screen["cols"], 40);
    assert_eq!(screen["rows"], 5);
    assert_eq!(
        screen["lines"],
        json!(["If nothing is", "", "Continue? [y/N]", "", ""])
    );
    assert_eq!(screen["cursor"], json!({ "row": 2, "col": 15 }));
    assert_eq!(screen["title"], "my-title");
    assert_eq!(screen["activity"], "waiting-for-input");
    assert_eq!(screen["alive"], true);

    // Same gate as the other session reads, and 404 for an unknown id.
    let anon = reqwest::Client::new()
        .get(format!("{base}/api/sessions/{id}/screen"))
        .send()
        .await
        .unwrap();
    assert_eq!(anon.status(), StatusCode::UNAUTHORIZED);
    let missing = client
        .get(format!(
            "{base}/api/sessions/{}/screen",
            uuid::Uuid::new_v4()
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);

    client
        .delete(format!("{base}/api/sessions/{id}"))
        .send()
        .await
        .unwrap();
}

/// WI-3988 review: a narrow screen wraps a private key across lines, so a
/// line-at-a-time pass would keep the short body pieces and the END marker.
/// The served screen must mask the block from BEGIN to END.
#[tokio::test]
async fn session_screen_masks_a_key_block_wrapped_across_lines() {
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let id = create_session_with(
        &client,
        &base,
        json!({
            "name": "screen-wrapped-key",
            "cols": 40,
            "rows": 12,
            "command": [
                "/bin/sh",
                "-c",
                "printf '%s\\n' 'WIN_SSH_KEY=abcd1234' '-----BEGIN OPENSSH PRIVATE KEY-----' 'b3BlbnNzaC1rZXktdjEAAAAA' 'dGhlIHF1aWNrIGJyb3duIGZveA' '-----END OPENSSH PRIVATE KEY-----' 'key-block-done'; sleep 30",
            ],
        }),
    )
    .await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let screen: Value = client
            .get(format!("{base}/api/sessions/{id}/screen"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let lines = screen["lines"].to_string();
        if lines.contains("key-block-done") {
            for leaked in [
                "BEGIN OPENSSH PRIVATE KEY",
                "END OPENSSH PRIVATE KEY",
                "b3BlbnNzaC1rZXktdjEAAAAA",
                "dGhlIHF1aWNrIGJyb3duIGZveA",
                "abcd1234",
            ] {
                assert!(
                    !lines.contains(leaked),
                    "a printed key block survived: {lines}"
                );
            }
            assert!(lines.contains("[redacted]"), "{lines}");
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "screen never showed the end of the block: {screen:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    client
        .delete(format!("{base}/api/sessions/{id}"))
        .send()
        .await
        .unwrap();
}

/// WI-874: `/wait` answers as soon as the session is ready, on exit, and on
/// timeout; `any-change` returns on the next state change.
#[tokio::test]
async fn wait_returns_on_ready_exit_change_and_timeout() {
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let get = |url: String| {
        let client = client.clone();
        async move {
            let resp = client.get(url).send().await.unwrap();
            let status = resp.status();
            (status, resp.json::<Value>().await.unwrap_or(Value::Null))
        }
    };

    // A prompt appears after a moment: the wait blocks, then answers ready.
    let prompt = create_session_with(
        &client,
        &base,
        json!({
            "name": "wait-ready",
            "command": ["/bin/sh", "-c", "sleep 0.5; printf 'Continue? [y/N]'; sleep 30"],
        }),
    )
    .await;
    let (status, ready) = get(format!(
        "{base}/api/sessions/{prompt}/wait?until=ready&timeout_s=20"
    ))
    .await;
    assert_eq!(status, StatusCode::OK, "{ready:?}");
    assert_eq!(ready["outcome"], "ready", "{ready:?}");
    assert_eq!(ready["matched"], true);
    assert_eq!(ready["screen"]["ready"], true);
    assert!(ready["screen"]["lines"][0]
        .as_str()
        .unwrap()
        .contains("Continue?"));

    // Already ready: answers at once.
    let (_, again) = get(format!("{base}/api/sessions/{prompt}/wait?timeout_s=20")).await;
    assert_eq!(again["outcome"], "ready");
    assert!(again["waited_ms"].as_u64().unwrap() < 5000, "{again:?}");

    // Nothing happens: a short timeout says so.
    let (_, timed_out) = get(format!(
        "{base}/api/sessions/{prompt}/wait?until=exited&timeout_s=1"
    ))
    .await;
    assert_eq!(timed_out["outcome"], "timeout");
    assert_eq!(timed_out["matched"], false);

    // A child that exits ends a wait for it.
    let short = create_session_with(
        &client,
        &base,
        json!({ "name": "wait-exit", "command": ["/bin/sh", "-c", "sleep 0.5; exit 3"] }),
    )
    .await;
    let (_, exited) = get(format!(
        "{base}/api/sessions/{short}/wait?until=exited&timeout_s=20"
    ))
    .await;
    assert_eq!(exited["outcome"], "exited", "{exited:?}");
    assert_eq!(exited["matched"], true);
    assert_eq!(exited["screen"]["alive"], false);

    // A blocked report ends a wait for ready (a person must act first), and
    // is on the summary and the screen; clearing it is a change.
    let blocked = client
        .post(format!("{base}/api/sessions/{prompt}/blocked"))
        .json(&json!({ "blocked": true, "reason": "needs the bot token", "items": ["create the bot", " "] }))
        .send()
        .await
        .unwrap();
    assert_eq!(blocked.status(), StatusCode::OK);
    let summary: Value = blocked.json().await.unwrap();
    assert_eq!(summary["blocked"]["reason"], "needs the bot token");
    assert_eq!(summary["blocked"]["items"], json!(["create the bot"]));
    let (_, on_blocked) = get(format!(
        "{base}/api/sessions/{prompt}/wait?until=ready&timeout_s=5"
    ))
    .await;
    assert_eq!(on_blocked["outcome"], "blocked");
    assert_eq!(on_blocked["matched"], false);
    assert_eq!(
        on_blocked["screen"]["blocked"]["reason"],
        "needs the bot token"
    );
    let change = tokio::spawn(get(format!(
        "{base}/api/sessions/{prompt}/wait?until=any-change&timeout_s=20"
    )));
    tokio::time::sleep(Duration::from_millis(300)).await;
    let cleared = client
        .post(format!("{base}/api/sessions/{prompt}/blocked"))
        .json(&json!({ "blocked": false }))
        .send()
        .await
        .unwrap();
    assert_eq!(cleared.status(), StatusCode::OK);
    let cleared: Value = cleared.json().await.unwrap();
    assert!(cleared.get("blocked").is_none(), "{cleared:?}");
    let (_, changed) = change.await.unwrap();
    assert_eq!(changed["outcome"], "changed", "{changed:?}");

    // Bad input is refused.
    let (bad_until, _) = get(format!("{base}/api/sessions/{prompt}/wait?until=soon")).await;
    assert_eq!(bad_until, StatusCode::BAD_REQUEST);
    let (too_long, _) = get(format!("{base}/api/sessions/{prompt}/wait?timeout_s=601")).await;
    assert_eq!(too_long, StatusCode::BAD_REQUEST);
    let no_reason = client
        .post(format!("{base}/api/sessions/{prompt}/blocked"))
        .json(&json!({ "blocked": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(no_reason.status(), StatusCode::BAD_REQUEST);

    for id in [prompt, short] {
        client
            .delete(format!("{base}/api/sessions/{id}"))
            .send()
            .await
            .unwrap();
    }
}

/// WI-1246: every session event has a monotonic seq, `/changes` replays after
/// a cursor, and `/wait-any` blocks on the next matching change instead of a
/// sweep.
#[tokio::test]
async fn session_changes_are_sequenced_filtered_and_waited_on() {
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let prompt = create_session_with(
        &client,
        &base,
        json!({
            "name": "changes-ready",
            "command": ["/bin/sh", "-c", "sleep 0.3; printf 'Continue? [y/N]'; sleep 30"],
        }),
    )
    .await;

    // The prompt becomes a `ready` change, and the sequence only grows.
    let mut changes = Value::Null;
    for _ in 0..50 {
        changes = client
            .get(format!("{base}/api/sessions/changes?since=0"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if changes["changes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["session"] == prompt && c["kind"] == "ready")
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let rows = changes["changes"].as_array().unwrap();
    let seqs: Vec<u64> = rows.iter().map(|c| c["seq"].as_u64().unwrap()).collect();
    let mut ordered = seqs.clone();
    ordered.sort();
    assert_eq!(seqs, ordered, "changes come oldest first: {changes}");
    assert!(seqs.windows(2).all(|w| w[1] > w[0]), "{seqs:?}");
    let ready = rows
        .iter()
        .find(|c| c["session"] == prompt && c["kind"] == "ready")
        .unwrap();
    let ready_seq = ready["seq"].as_u64().unwrap();
    assert!(!ready["at"].as_str().unwrap().is_empty());
    assert!(ready["summary"].as_str().unwrap().len() <= 120);

    // After the cursor, that change is gone.
    let later: Value = client
        .get(format!("{base}/api/sessions/changes?since={ready_seq}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(later["changes"]
        .as_array()
        .unwrap()
        .iter()
        .all(|c| c["seq"].as_u64().unwrap() > ready_seq));
    assert_eq!(later["truncated"], false);

    // A cursor past the head is a restart, not the future: the sequence
    // begins again at 1, so the whole ring comes back flagged truncated
    // rather than an empty page the overseer would wait on forever.
    let restarted: Value = client
        .get(format!("{base}/api/sessions/changes?since=5000"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(restarted["truncated"], true, "{restarted}");
    assert!(
        restarted["changes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["session"] == prompt && c["kind"] == "ready"),
        "{restarted}"
    );
    let from_the_start: Value = client
        .get(format!(
            "{base}/api/sessions/wait-any?ids={prompt}&until=ready&cursor=5000&timeout_s=5"
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(from_the_start["outcome"], "changed", "{from_the_start}");
    assert_eq!(from_the_start["change"]["kind"], "ready");

    // Another session's change is not this one's.
    let other = create_session_with(
        &client,
        &base,
        json!({ "name": "changes-other", "command": ["/bin/sh", "-c", "sleep 30"] }),
    )
    .await;
    let mine: Value = client
        .get(format!("{base}/api/sessions/changes?since=0&ids={prompt}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(mine["changes"]
        .as_array()
        .unwrap()
        .iter()
        .all(|c| c["session"] == prompt));

    // A change already in the ring answers a wait at once.
    let buffered: Value = client
        .get(format!(
            "{base}/api/sessions/wait-any?ids={prompt}&until=ready&cursor=0&timeout_s=5"
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(buffered["outcome"], "changed", "{buffered}");
    assert_eq!(buffered["change"]["kind"], "ready");

    // Nothing pending: it blocks, then answers when the change lands.
    let head = buffered["next_cursor"].as_u64().unwrap();
    let waiting = tokio::spawn({
        let client = client.clone();
        let url = format!(
            "{base}/api/sessions/wait-any?ids={prompt}&until=blocked&cursor={head}&timeout_s=20"
        );
        async move {
            client
                .get(url)
                .send()
                .await
                .unwrap()
                .json::<Value>()
                .await
                .unwrap()
        }
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    let blocked = client
        .post(format!("{base}/api/sessions/{prompt}/blocked"))
        .json(&json!({ "blocked": true, "reason": "needs a token" }))
        .send()
        .await
        .unwrap();
    assert_eq!(blocked.status(), StatusCode::OK);
    let got = waiting.await.unwrap();
    assert_eq!(got["outcome"], "changed", "{got}");
    assert_eq!(got["change"]["kind"], "blocked");

    // And a wait with nothing coming says so.
    let cursor = got["next_cursor"].as_u64().unwrap();
    let timed: Value = client
        .get(format!(
            "{base}/api/sessions/wait-any?ids={other}&until=exited&cursor={cursor}&timeout_s=1"
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(timed["outcome"], "timeout", "{timed}");
    assert!(timed.get("change").is_none(), "{timed}");

    // Bad input is refused, the same ceiling as /wait.
    let too_long = client
        .get(format!("{base}/api/sessions/wait-any?timeout_s=601"))
        .send()
        .await
        .unwrap();
    assert_eq!(too_long.status(), StatusCode::BAD_REQUEST);
    let bad_until = client
        .get(format!("{base}/api/sessions/wait-any?until=soon"))
        .send()
        .await
        .unwrap();
    assert_eq!(bad_until.status(), StatusCode::BAD_REQUEST);

    for id in [prompt, other] {
        client
            .delete(format!("{base}/api/sessions/{id}"))
            .send()
            .await
            .unwrap();
    }
}

/// WI-1246: `Last-Event-ID` replays what the ring still holds, and the live
/// payload stays the raw event — the sequence rides on the SSE `id`.
#[tokio::test]
async fn the_event_stream_replays_after_a_last_event_id() {
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let id = create_session_with(
        &client,
        &base,
        json!({
            "name": "sse-replay",
            "command": ["/bin/sh", "-c", "printf 'Continue? [y/N]'; sleep 30"],
        }),
    )
    .await;

    // Read the stream until the activity event, and keep its SSE id. From
    // cursor 0, so an event published before this subscribed is replayed
    // rather than missed.
    let stream_res = client
        .get(format!("{base}/api/events"))
        .header("Last-Event-ID", "0")
        .send()
        .await
        .unwrap();
    let mut stream = stream_res.bytes_stream();
    let want = id.clone();
    let (event, sse_id) = sse_event(&mut stream, move |event| {
        event["type"] == "activity"
            && event["id"] == want.as_str()
            && event["state"] == "waiting-for-input"
    })
    .await;
    assert_eq!(event["state"], "waiting-for-input");
    let sse_id: u64 = sse_id.expect("the event carried an id").parse().unwrap();

    // A fresh stream that names the event before it gets this one replayed,
    // still as the raw event.
    let replay_res = client
        .get(format!("{base}/api/events"))
        .header("Last-Event-ID", (sse_id - 1).to_string())
        .send()
        .await
        .unwrap();
    let mut replay = replay_res.bytes_stream();
    let want = id.clone();
    let (replayed, replayed_id) = sse_event(&mut replay, move |event| {
        event["type"] == "activity" && event["id"] == want.as_str()
    })
    .await;
    assert_eq!(replayed["state"], "waiting-for-input");
    assert_eq!(replayed_id.unwrap().parse::<u64>().unwrap(), sse_id);

    // A cursor from a previous engine run, far above this run's sequence, is
    // told about the gap and still gets every new event live.
    let stale_res = client
        .get(format!("{base}/api/events"))
        .header("Last-Event-ID", "999999")
        .send()
        .await
        .unwrap();
    let mut stale = stale_res.bytes_stream();
    let (lagged, _) = sse_event(&mut stale, |event| event["type"] == "lagged").await;
    assert_eq!(lagged["type"], "lagged");

    client
        .delete(format!("{base}/api/sessions/{id}"))
        .send()
        .await
        .unwrap();
    let want = id.clone();
    let (killed, killed_id) = sse_event(&mut stale, move |event| {
        event["type"] == "session-killed" && event["id"] == want.as_str()
    })
    .await;
    assert_eq!(killed["type"], "session-killed");
    assert!(killed_id.unwrap().parse::<u64>().unwrap() < 999_999);
}

#[tokio::test]
async fn a_claude_session_starts_with_its_directory_trusted_and_its_brief_readable() {
    let (tmp, mut cfg, stub) = hibernation_sandbox();
    let claude_home = tmp.path().join("claude-home");
    cfg.agent_onboarding = vogt_engine_server::claude_config::Onboarding {
        enabled: true,
        default_dir: Some(claude_home.clone()),
        settings_file: None,
        opencode_config: None,
        accounts_root: None,
        accounts: Vec::new(),
        account_concurrency_cap: 8,
        default_account: None,
        codex_policy: Ok(None),
    };
    let state_dir = cfg.state_dir.clone();
    let workspace = cfg.workspace_root.clone();
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let created: Value = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "trusted",
            "prompt": "## Task\n\nDo it.\n",
            "command": [stub.to_string_lossy()],
            "cwd": workspace.to_string_lossy(),
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap().to_string();
    let printed = live_output_containing(&client, &base, &id, &["helper=["]).await;
    let brief_dir = state_dir.join("agent-task-prompts").join("sessions");
    assert!(
        printed.contains(&format!(
            "arg=[--add-dir={}]\r\narg=[Vogt started",
            brief_dir.display()
        )),
        "the brief's directory is added in the `=` form, right before the prompt: {printed:?}"
    );

    let config: Value =
        serde_json::from_slice(&std::fs::read(claude_home.join(".claude.json")).unwrap()).unwrap();
    let project = &config["projects"][workspace.to_string_lossy().as_ref()];
    assert_eq!(project["hasTrustDialogAccepted"], true, "{config}");
    assert_eq!(
        project["hasClaudeMdExternalIncludesApproved"], true,
        "{config}"
    );

    // A wake resumes and does not re-send the brief. It keeps the brief's
    // directory, recorded at the first start, so the resumed agent can still
    // re-read its brief without a permission prompt (WI-1250).
    client
        .post(format!("{base}/api/sessions/{id}/hibernate"))
        .send()
        .await
        .unwrap();
    client
        .post(format!("{base}/api/sessions/{id}/wake"))
        .send()
        .await
        .unwrap();
    let printed =
        live_output_containing(&client, &base, &id, &["arg=[--resume]", "helper=["]).await;
    let woken = &printed[printed.rfind("arg=[--resume]").unwrap()..];
    assert!(woken.contains("arg=[--add-dir="), "{woken:?}");
    assert!(!woken.contains("Vogt started"), "{woken:?}");
}

/// WI-2734: a discovered account starts a session with the configured
/// accounts present, under the same cap, and the same refusals as before
/// still hold for a name that is neither.
#[tokio::test]
async fn a_discovered_account_starts_a_session_alongside_configured_accounts() {
    let (tmp, mut cfg, _stub) = hibernation_sandbox();
    let acct2 = tmp.path().join("acct2");
    let root = tmp.path().join("accounts");
    let found = root.join("found");
    std::fs::create_dir_all(&found).unwrap();
    std::fs::write(found.join(".vogt-claude-profile"), "").unwrap();
    let workspace = cfg.workspace_root.clone();
    cfg.agent_onboarding = vogt_engine_server::claude_config::Onboarding {
        enabled: true,
        default_dir: Some(tmp.path().join("default-home")),
        settings_file: None,
        opencode_config: None,
        account_concurrency_cap: 8,
        default_account: None,
        accounts_root: Some(root.clone()),
        accounts: vogt_engine_server::claude_config::parse_accounts(Some(&format!(
            "acct2={}",
            acct2.display()
        ))),
        codex_policy: Ok(None),
    };
    let stub = tmp.path().join("bin").join("claude");
    std::fs::write(
        &stub,
        "#!/bin/sh\nprintf 'cfg=[%s]\\n' \"$CLAUDE_CONFIG_DIR\"\nsleep 300 &\nprintf '> '\nwait\n",
    )
    .unwrap();
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let started = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "discovered",
            "command": [stub.to_string_lossy()],
            "account": "found",
            "cwd": workspace.to_string_lossy(),
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(started.status(), StatusCode::OK);
    let created: Value = started.json().await.unwrap();
    assert_eq!(created["account"], "found", "{created}");
    let id = created["id"].as_str().unwrap().to_string();
    let want = format!("cfg=[{}]", found.canonicalize().unwrap().display());
    live_output_containing(&client, &base, &id, &[&want]).await;

    let unknown = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({ "name": "nope", "command": [stub.to_string_lossy()], "account": "nope" }))
        .send()
        .await
        .unwrap();
    assert_eq!(unknown.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn the_sweep_returns_every_session_with_its_screen_tail_in_one_call() {
    let (tmp, cfg, _stub) = hibernation_sandbox();
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let quiet = "#!/bin/sh\nsleep 300 &\nprintf 'line one\\n\\nhelper=[%s]\\n> ' \"$!\"\nwait\n";
    let ids = start_stub_agents(
        &client,
        &base,
        &tmp.path().join("agents"),
        &[("awake", quiet), ("asleep", quiet)],
    )
    .await;
    client
        .post(format!("{base}/api/sessions/{}/hibernate", ids[1]))
        .send()
        .await
        .unwrap();
    let ended: Value = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({ "name": "ended", "command": ["/bin/sh", "-c", "true"] }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    let rows: Vec<Value> = client
        .get(format!("{base}/api/sessions/sweep?screen_lines=2"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let ids_seen: Vec<&str> = rows
        .iter()
        .map(|r| r["summary"]["id"].as_str().unwrap())
        .collect();
    assert!(ids_seen.contains(&ids[0].as_str()) && ids_seen.contains(&ids[1].as_str()));
    assert!(
        !ids_seen.contains(&ended["id"].as_str().unwrap()),
        "exited is left out"
    );
    for row in &rows {
        let tail: Vec<&str> = row["screen_tail"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l.as_str().unwrap())
            .collect();
        assert_eq!(tail.len(), 2, "{row}");
        assert!(
            tail[0].starts_with("helper=["),
            "blank lines skipped: {tail:?}"
        );
    }
    let asleep = rows
        .iter()
        .find(|r| r["summary"]["id"] == ids[1].as_str())
        .unwrap();
    assert_eq!(asleep["summary"]["activity"], "hibernated");
    assert_eq!(
        client
            .get(format!("{base}/api/sessions/sweep?screen_lines=41"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
}

/// WI-913: a session stopped on request reads `stopped` with who and why;
/// one that dies by itself still reads `errored`.
#[tokio::test]
async fn a_requested_stop_is_stopped_and_a_crash_is_still_errored() {
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let start = |name: &'static str, command: Vec<&'static str>| {
        let client = client.clone();
        let base = base.clone();
        async move {
            let created: Value = client
                .post(format!("{base}/api/sessions"))
                .json(&json!({ "name": name, "command": command }))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            created["id"].as_str().unwrap().to_string()
        }
    };
    let reaped = start("reaped child", vec!["/bin/sh", "-c", "sleep 300"]).await;
    let crashed = start("crashing child", vec!["/bin/sh", "-c", "sleep 0.2; exit 3"]).await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    let killed = client
        .post(format!("{base}/api/sessions/{reaped}/kill"))
        .json(&json!({ "reason": "answer ingested", "by": "agent:session:ses_parent" }))
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(killed, StatusCode::OK);

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let listed: Value = client
            .get(format!("{base}/api/sessions"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let listed: Vec<Value> = serde_json::from_value(listed["sessions"].clone()).unwrap();
        let row = |id: &str| listed.iter().find(|s| s["id"] == id).unwrap().clone();
        let (r, c) = (row(&reaped), row(&crashed));
        if r["alive"] == false && c["alive"] == false {
            assert_eq!(r["activity"], "stopped", "{r}");
            assert_eq!(r["stop"]["reason"], "answer ingested");
            assert_eq!(r["stop"]["by"], "agent:session:ses_parent");
            assert!(r["stop"]["at"].as_str().is_some_and(|a| !a.is_empty()));
            assert_eq!(c["activity"], "errored", "a crash stays a crash: {c}");
            assert!(c.get("stop").is_none());
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "never exited: {r} {c}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// WI-927: a session's launch wrapper reports its stages to the engine, which
/// accepts the report once, from that session's own broker token only, and
/// keeps the timings as metrics — the real `report_launch` from
/// `agent-auth.sh`, not a hand-built request.
#[tokio::test]
async fn a_launch_report_from_the_wrapper_is_accepted_once_and_measured() {
    if std::process::Command::new("jq")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("skipping: the launch report is built with jq");
        return;
    }
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../deploy/agent-auth.sh");
    let tmp = tempfile::tempdir().unwrap();
    let cfg = broker_config(&tmp, "LATER proj later ondemand\n");
    let (base, state, _h) = boot_with_state(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let launch = format!(
        "source '{}'; begin_launch shell; launch_stage login 12 ok; \
         PROJECT_MODE[p-1]=bulk; PROJECT_MS[p-1]=40; PROJECT_OUTCOME[p-1]=ok; \
         PROJECT_NAMES[p-1]='GH:GITHUB_PAT:1 X:MISSING_ONE:0'; \
         launch_stage bootstrap 250 ok; report_launch ok; echo reported-$?; sleep 30",
        script.display()
    );
    let id = client
        .post(format!("{base}/api/sessions"))
        // The harness binds an ephemeral port, so the broker's advertised
        // loopback address (from the configured bind) is pointed at it here.
        .json(&json!({
            "name": "launch-report",
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
    live_output_containing(&client, &base, &id, &["reported-0"]).await;

    // Accepted: the session's one report is now claimed.
    let session = state.sessions.get(id.parse().unwrap()).unwrap();
    assert!(
        !session.mark_launch_reported(),
        "the wrapper's report should have been accepted"
    );
    let text = vogt_engine_server::metrics::metrics().render();
    for expected in [
        "vogt_session_launch_seconds_count{command=\"shell\",outcome=\"ok\"}",
        "vogt_session_launch_stage_seconds_count{stage=\"login\"}",
        "vogt_session_launch_stage_seconds_count{stage=\"secrets\"}",
        "vogt_session_launch_stage_seconds_count{stage=\"bootstrap\"}",
        "vogt_session_launch_secret_reads_total{mode=\"bulk\",outcome=\"ok\"}",
        "vogt_session_first_output_seconds_count{launcher=\"direct\"}",
        "vogt_session_starts_total{origin=\"api\",outcome=\"ok\"}",
    ] {
        assert!(text.contains(expected), "missing {expected} in\n{text}");
    }

    // Nobody else can report: no token, or a token the engine never issued.
    let anonymous = reqwest::Client::new();
    for bearer in [None, Some("not-a-broker-token")] {
        let mut request = anonymous
            .post(format!("{base}/api/agent-auth/launch-report"))
            .json(&json!({ "outcome": "ok", "total_ms": 1 }));
        if let Some(bearer) = bearer {
            request = request.bearer_auth(bearer);
        }
        assert_eq!(
            request.send().await.unwrap().status(),
            StatusCode::UNAUTHORIZED
        );
    }
}

/// WI-927: `/metrics` is served on its own listener, never on the API port.
#[tokio::test]
async fn metrics_are_served_on_their_own_listener_only() {
    let (base, _h) = boot().await;
    // The API port answers /metrics with the GUI shell, never the metrics.
    let on_api = reqwest::Client::new()
        .get(format!("{base}/metrics"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(
        !on_api.contains("vogt_session_"),
        "the API port must not serve /metrics"
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, vogt_engine_server::metrics::router())
            .await
            .unwrap()
    });
    let response = reqwest::get(format!("http://{addr}/metrics"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("text/plain; version=0.0.4"));
    let body = response.text().await.unwrap();
    assert!(
        body.contains("# TYPE vogt_session_first_output_seconds histogram"),
        "{body}"
    );
}

/// WI-934: `stopped` is keyed on the stop request, not on the CLI, so a
/// stopped opencode session reads `stopped` too, and one that dies on its
/// own still reads `errored`.
#[tokio::test]
async fn a_stopped_opencode_session_is_stopped_not_errored() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    // A stand-in named `opencode`, so the engine treats it as that CLI.
    let opencode = tmp.path().join("opencode");
    std::fs::write(
        &opencode,
        "#!/bin/sh\n[ \"$1\" = crash ] && { sleep 0.2; exit 7; }\nsleep 300\n",
    )
    .unwrap();
    std::fs::set_permissions(&opencode, std::fs::Permissions::from_mode(0o755)).unwrap();
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let mut ids = Vec::new();
    for args in [vec![], vec!["crash"]] {
        let mut command = vec![opencode.to_string_lossy().to_string()];
        command.extend(args.iter().map(|a| a.to_string()));
        let created: Value = client
            .post(format!("{base}/api/sessions"))
            .json(&json!({ "name": "opencode", "command": command }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        ids.push(created["id"].as_str().unwrap().to_string());
    }
    let (stopped, crashed) = (&ids[0], &ids[1]);
    tokio::time::sleep(Duration::from_millis(150)).await;
    let status = client
        .post(format!("{base}/api/sessions/{stopped}/kill"))
        .json(&json!({ "reason": "packet done", "by": "agent:session:ses_driver" }))
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(status, StatusCode::OK);
    let row = wait_for_session_row(&client, &base, stopped, |s| !s["exit_code"].is_null()).await;
    assert_eq!(row["activity"], "stopped", "{row:?}");
    assert_eq!(row["stop"]["reason"], "packet done", "{row:?}");
    let row = wait_for_session_row(&client, &base, crashed, |s| s["exit_code"] == json!(7)).await;
    assert_eq!(row["activity"], "errored", "{row:?}");
}

#[tokio::test]
async fn a_tool_approval_waits_for_a_person_and_a_read_does_not() {
    let Some((base, client, _state, _guard, _bin)) = boot_chats().await else {
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

    // A command waits on a card. Sent without waiting, so the test can answer.
    let sent = client
        .post(format!("{base}/api/chats/{id}/messages"))
        .json(&json!({ "text": "run:make test" }))
        .send()
        .await
        .unwrap();
    assert_eq!(sent.status(), StatusCode::OK);
    let approval = pending_approval(&client, &base, &id).await;
    assert_eq!(approval["tool_name"], "commandExecution");
    assert_eq!(approval["summary"], "make test");
    assert_eq!(approval["source"], "codex");
    let chat: Value = client
        .get(format!("{base}/api/chats/{id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(chat["state"], "awaiting-approval");
    assert!(
        texts(&chat["entries"], "tool-call").contains(&"make test".to_string()),
        "{chat}"
    );

    // An agent may not answer it: the break-glass token relaying a
    // non-person is refused, and nothing is decided.
    let approval_id = approval["id"].as_str().unwrap();
    let by_agent = client
        .post(format!("{base}/api/chats/{id}/approvals/{approval_id}"))
        .json(&json!({ "allow": true, "person": false }))
        .send()
        .await
        .unwrap();
    assert_eq!(by_agent.status(), StatusCode::FORBIDDEN);
    assert!(by_agent.text().await.unwrap().contains("person required"));

    // A person denies it; the agent is told, and the turn ends.
    let denied = client
        .post(format!("{base}/api/chats/{id}/approvals/{approval_id}"))
        .json(&json!({ "allow": false, "message": "not now" }))
        .send()
        .await
        .unwrap();
    assert_eq!(denied.status(), StatusCode::OK);
    let detail = wait_chat_idle(&client, &base, &id).await;
    let results: Vec<&Value> = detail["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "tool-result")
        .collect();
    assert_eq!(results.len(), 1, "{detail}");
    assert_eq!(results[0]["is_error"], true);
    let notes = texts(&detail["entries"], "approval");
    assert!(
        notes.iter().any(|n| n.contains("not now")),
        "the person's reason is recorded: {notes:?}"
    );

    // Allowed, it runs.
    client
        .post(format!("{base}/api/chats/{id}/messages"))
        .json(&json!({ "text": "run:ls" }))
        .send()
        .await
        .unwrap();
    let approval = pending_approval(&client, &base, &id).await;
    let approval_id = approval["id"].as_str().unwrap();
    client
        .post(format!("{base}/api/chats/{id}/approvals/{approval_id}"))
        .json(&json!({ "allow": true }))
        .send()
        .await
        .unwrap();
    let detail = wait_chat_idle(&client, &base, &id).await;
    assert!(
        texts(&detail["entries"], "tool-result").contains(&"ran ls".to_string()),
        "{detail}"
    );

    // A read of the chat's own directory runs unasked.
    let read = chat_send(&client, &base, &id, "read:notes.txt").await;
    assert!(
        texts(&read["entries"], "tool-result").contains(&"ran notes.txt".to_string()),
        "{read}"
    );
}

/// WI-4321: the engine answers an OSC 7501 capability query on the PTY's
/// input, keeps the query out of the replay ring, exposes the program's
/// report and bell in the summary, and drops both when the process exits.
/// It answers once: a second query gets nothing.
#[tokio::test]
async fn osc7501_query_is_answered_once_stripped_and_the_report_is_exposed_until_exit() {
    let reply = vogt_engine_server::osc7501::QUERY_REPLY;
    let body = format!(
        "#!/bin/sh\nstty raw -echo\nprintf '\\033]7501;?\\007'\n\
         reply=$(dd bs=1 count={} 2>/dev/null | od -An -tx1 | tr -d ' \\n')\n\
         printf 'REPLY=%s\\r\\n' \"$reply\"\n\
         printf '\\033]7501;?\\007'\n\
         extra=$(timeout --foreground 1 dd bs=1 count=1 2>/dev/null | od -An -tx1 | tr -d ' \\n')\n\
         printf 'EXTRA=[%s]\\r\\n' \"$extra\"\n\
         printf '\\033]7501;state=blocked:kind=permission:app=stub:msg=aGk=\\007'\n\
         printf '\\a'\nexec sleep 30\n",
        reply.len()
    );
    let (session, _tmp) = spawn_osc_stub("claude", &body);
    let ring = ring_has(&session, "EXTRA=[").await;
    let mut summary = session.summary();
    for _ in 0..100 {
        if summary.reported_status.is_some() && summary.attention_hint.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        summary = session.summary();
    }
    let status = summary
        .reported_status
        .clone()
        .unwrap_or_else(|| panic!("no reported status: {summary:?}"));
    assert_eq!(status.state, vogt_engine_contract::ReportedState::Blocked);
    assert_eq!(status.kind.as_deref(), Some("permission"));
    assert_eq!(status.app.as_deref(), Some("stub"));
    assert_eq!(status.msg.as_deref(), Some("hi"));
    assert_eq!(summary.attention_hint.as_ref().unwrap().source, "bell");

    let hex: String = reply.iter().map(|b| format!("{b:02x}")).collect();
    assert!(
        ring.contains(&format!("REPLY={hex}")),
        "the engine's reply did not reach the program's input: {ring:?}"
    );
    assert!(
        ring.contains("EXTRA=[]"),
        "a second query was answered: {ring:?}"
    );
    assert!(!ring.contains("7501;?"), "query left in the ring: {ring:?}");

    session.kill().unwrap();
    for _ in 0..100 {
        if !session.is_alive() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let after = session.summary();
    assert!(after.reported_status.is_none() && after.attention_hint.is_none());
}

/// A program that is not an agent CLI (a plain shell that cats a file holding
/// the query) gets no engine bytes on its input.
#[tokio::test]
async fn osc7501_query_from_a_plain_program_gets_no_reply() {
    let body = "#!/bin/sh\nstty raw -echo\nprintf '\\033]7501;?\\007'\n\
         got=$(timeout --foreground 1 dd bs=1 count=1 2>/dev/null | od -An -tx1 | tr -d ' \\n')\n\
         printf 'GOT=[%s]\\r\\n' \"$got\"\nexec sleep 30\n";
    let (session, _tmp) = spawn_osc_stub("agent", body);
    let ring = ring_has(&session, "GOT=[").await;
    assert!(
        ring.contains("GOT=[]"),
        "a plain program got a reply: {ring:?}"
    );
    session.kill().unwrap();
}

#[tokio::test]
async fn a_resting_session_whose_output_names_a_provider_failure_reads_as_blocked() {
    let summary = summary_of_a_stub_that_prints(
        "#!/bin/sh\nprintf 'Error: 402 Insufficient Balance\\n> '\nexec sleep 30\n",
    )
    .await;
    let blocked = summary
        .blocked
        .clone()
        .unwrap_or_else(|| panic!("provider failure left the session idle: {summary:?}"));
    assert!(
        blocked.reason.contains("Insufficient Balance"),
        "{blocked:?}"
    );
    assert!(blocked.items.is_empty());
}

#[tokio::test]
async fn ordinary_session_output_does_not_read_as_blocked() {
    let summary = summary_of_a_stub_that_prints(
        "#!/bin/sh\nprintf 'the 429 handler returns 402\\nreconnecting the client\\n> '\nexec sleep 30\n",
    )
    .await;
    assert!(summary.blocked.is_none(), "{summary:?}");
}

/// WI-4068 review: a briefed agent given its pointer at launch starts working
/// without ever resting at its composer. If that first turn finishes inside the
/// readiness window, the watchdog must not read the idle composer it leaves
/// behind as a prompt still queued: no Enter, no blocked report. Codex's own
/// `working` title is the evidence it acts on.
#[tokio::test]
async fn a_first_turn_seen_working_is_not_retried_when_it_finishes_inside_the_wait_window() {
    let tmp = tempfile::tempdir().unwrap();
    let bus = vogt_engine_server::events::EventBus::new(64);
    let session = spawn_fake_codex(
        tmp.path(),
        "#!/bin/bash\n\
         printf '\\033]0;codex: working\\007'\n\
         for i in 1 2 3 4 5; do printf 'working %s\\n' \"$i\"; sleep 0.1; done\n\
         printf '\\033]0;codex: ready\\007'\n\
         printf '> '\n\
         if read -t 2 -r line; then printf 'ENTER-RECEIVED:%s\\n' \"$line\"; fi\n\
         exec sleep 30\n",
        &bus,
    );
    vogt_engine_server::first_turn::watch(bus, Arc::clone(&session), first_turn_policy())
        .await
        .unwrap();
    assert!(
        session.blocked().is_none(),
        "the first turn ran, so nothing is blocked: {:?}",
        session.blocked()
    );
    let (screen, _) = session.render_screen(0);
    assert!(
        !screen
            .lines
            .iter()
            .any(|line| line.contains("ENTER-RECEIVED")),
        "the watchdog pressed Enter into a first turn that already ran: {:?}",
        screen.lines
    );
    session.kill().unwrap();
}

/// WI-4068 review: stock Codex sets no title, so a turn that starts at launch
/// and ends before the TUI is ever seen at its composer leaves only an advanced
/// turn start. The idle composer behind it must not be reported as a prompt
/// that never submitted.
#[tokio::test]
async fn a_first_turn_that_finishes_before_the_composer_is_seen_is_not_blocked() {
    let tmp = tempfile::tempdir().unwrap();
    let bus = vogt_engine_server::events::EventBus::new(64);
    let session = spawn_fake_codex(
        tmp.path(),
        "#!/bin/bash\n\
         printf 'booting\\n'\n\
         sleep 0.8\n\
         printf 'working\\n'\n\
         sleep 0.1\n\
         printf '> '\n\
         if read -t 2 -r line; then printf 'ENTER-RECEIVED:%s\\n' \"$line\"; fi\n\
         exec sleep 30\n",
        &bus,
    );
    vogt_engine_server::first_turn::watch(bus, Arc::clone(&session), first_turn_policy())
        .await
        .unwrap();
    assert!(
        session.turn_started_at() > Some(session.created_at),
        "the fixture must record a real turn start, not the spawn stamp"
    );
    assert!(
        session.blocked().is_none(),
        "the first turn ran, so nothing is blocked: {:?}",
        session.blocked()
    );
    let (screen, _) = session.render_screen(0);
    assert!(
        !screen
            .lines
            .iter()
            .any(|line| line.contains("ENTER-RECEIVED")),
        "the watchdog pressed Enter into a first turn that already ran: {:?}",
        screen.lines
    );
    session.kill().unwrap();
}

/// WI-4290: stopped sessions used to keep their whole output ring and live
/// vt100 grid in the registry forever, so the engine grew by about 1.5 MiB
/// per stopped session on vogt-dev. Run start/stop cycles and drive the
/// reaper with an injected clock (no sleeps decide anything): past the shed
/// age every stopped session holds at most a bounded tail and no grid, and
/// still shows its last screen; past the retention the registry is empty
/// again, and so is its transition-lock map.
#[tokio::test]
async fn stopped_sessions_shed_their_buffers_and_are_forgotten_after_retention() {
    const CYCLES: usize = 3;
    const PER_CYCLE: usize = 4;
    const OUTPUT_BYTES: u64 = 400 * 1024;
    let stopped = vogt_engine_server::config::StoppedRetention {
        shed_after: Duration::from_secs(60),
        keep_bytes: 16 * 1024,
        evict_after: Some(Duration::from_secs(3600)),
    };
    let (base, state, _h) = boot_with_state(Config {
        scrollback_bytes: 1024 * 1024,
        stopped_sessions: stopped,
        ..test_config()
    })
    .await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    for cycle in 0..CYCLES {
        let mut ids = Vec::new();
        for n in 0..PER_CYCLE {
            let marker = format!("STOPPED-MARKER-{cycle}-{n}");
            let script = format!(
                "head -c {OUTPUT_BYTES} /dev/zero | tr '\\0' x | fold -w 100; echo {marker}; exec cat"
            );
            let id = create_command_session(
                &client,
                &base,
                &format!("reap-{cycle}-{n}"),
                vec!["/bin/bash".into(), "-c".into(), script],
            )
            .await;
            ids.push((id.parse::<Uuid>().unwrap(), marker));
        }
        // Wait for each session's whole output, then stop it.
        for (id, marker) in &ids {
            let session = state.sessions.get(*id).unwrap();
            let deadline = tokio::time::Instant::now() + LOAD_DEADLINE;
            while !String::from_utf8_lossy(&session.snapshot_tail(256).0).contains(marker.as_str())
            {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "{marker} never printed"
                );
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            state.sessions.stop(*id, Default::default()).unwrap();
        }
        for (id, _) in &ids {
            let session = state.sessions.get(*id).unwrap();
            let deadline = tokio::time::Instant::now() + LOAD_DEADLINE;
            while session.is_alive() || session.ended_at().is_none() {
                assert!(tokio::time::Instant::now() < deadline, "{id} never exited");
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            // Before the shed: the full ring and the grid are still held.
            assert!(session.has_live_grid());
            assert!(session.retained_bytes() > stopped.keep_bytes);
        }
        // A refused hibernation of a stopped session takes and releases the
        // transition lock; its map entry must not outlive it.
        for (id, _) in &ids {
            let refused = state
                .sessions
                .hibernate(
                    *id,
                    None,
                    vogt_engine_contract::HibernateTrigger::Manual,
                    true,
                    true,
                    None,
                )
                .await;
            assert!(refused.is_err());
        }
        assert_eq!(state.sessions.transition_entries(), 0);

        let latest_end = ids
            .iter()
            .filter_map(|(id, _)| state.sessions.get(*id).unwrap().ended_at())
            .max()
            .unwrap();
        // Inside the shed age nothing is touched.
        let early = state.sessions.reap_stopped(latest_end);
        assert_eq!(early.evicted, 0);
        // Past it, every stopped session is shed, still listed and readable.
        let reaped = state.sessions.reap_stopped(latest_end + stopped.shed_after);
        assert_eq!(reaped.shed, PER_CYCLE, "cycle {cycle}");
        assert_eq!(reaped.evicted, 0);
        assert_eq!(state.sessions.len(), PER_CYCLE);
        let mut retained = 0;
        for (id, marker) in &ids {
            let session = state.sessions.get(*id).unwrap();
            assert!(!session.has_live_grid(), "{id} kept its grid");
            // The ring's tail plus the kept screen text, each bounded.
            assert!(
                session.retained_bytes() <= 2 * stopped.keep_bytes,
                "{id} retains {} bytes",
                session.retained_bytes()
            );
            retained += session.retained_bytes();
            let (screen, history) = session.render_screen(50);
            let shown = screen.lines.iter().chain(history.iter());
            assert!(
                shown.into_iter().any(|l| l.contains(marker.as_str())),
                "{id}'s kept screen lost its last output"
            );
            assert!(
                String::from_utf8_lossy(&session.snapshot().0).contains(marker.as_str()),
                "{id}'s kept tail lost its last output"
            );
        }
        assert!(retained <= PER_CYCLE * 2 * stopped.keep_bytes);
        // A second pass sheds nothing again.
        let again = state.sessions.reap_stopped(latest_end + stopped.shed_after);
        assert_eq!(again, Default::default());

        // Past the retention every stopped session is forgotten.
        let ttl = stopped.evict_after.unwrap();
        let forgotten = state.sessions.reap_stopped(latest_end + ttl);
        assert_eq!(forgotten.evicted, PER_CYCLE, "cycle {cycle}");
        assert!(state.sessions.is_empty(), "cycle {cycle} left sessions");
        assert_eq!(state.sessions.transition_entries(), 0);
        for (id, _) in &ids {
            assert!(state.sessions.get(*id).is_err());
        }
    }
}

/// WI-4322: a current report decides the activity, and without one (never
/// reported, or cleared) the quiet window decides as before. Both stubs print
/// once and go silent; only the report differs.
#[tokio::test]
async fn osc7501_report_decides_activity_and_no_report_falls_through() {
    use vogt_engine_contract::ActivityState::*;
    // Never reports: quiet, so the heuristic says idle.
    let (plain, _t1) = spawn_osc_stub("agent", "#!/bin/sh\nprintf 'hello\\r\\n'\nexec sleep 30\n");
    assert_eq!(activity_reaches(&plain, Idle).await, Idle);

    // Reports `working` then goes silent: the report holds it running well
    // past the quiet window (200 ms) that made the plain stub idle.
    let body = "#!/bin/sh\nprintf '\\033]7501;state=working:app=stub\\007'\nexec sleep 30\n";
    let (working, _t2) = spawn_osc_stub("agent", body);
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(working.activity(), Running);

    // Reports `working`, then `clear`: back to the heuristic, which is idle.
    let body = "#!/bin/sh\nprintf '\\033]7501;state=working:app=stub\\007'\nsleep 1\n\
                printf '\\033]7501;state=clear\\007'\nexec sleep 30\n";
    let (cleared, _t3) = spawn_osc_stub("agent", body);
    assert_eq!(activity_reaches(&cleared, Idle).await, Idle);
    assert!(cleared.summary().reported_status.is_none());
    for s in [plain, working, cleared] {
        s.kill().unwrap();
    }
}

/// WI-4322: a terminal state is never overridden by a report. The stub
/// reports `blocked` and exits; the session reads `exited`, with no dialog.
#[tokio::test]
async fn osc7501_report_never_overrides_a_terminal_state() {
    use vogt_engine_contract::ActivityState::*;
    let body = "#!/bin/sh\nprintf '\\033]7501;state=blocked:kind=permission:msg=aGk=\\007'\nsleep 1\nexit 0\n";
    let (session, _t) = spawn_osc_stub("agent", body);
    assert_eq!(
        activity_reaches(&session, AwaitingApproval).await,
        AwaitingApproval
    );
    assert_eq!(activity_reaches(&session, Exited).await, Exited);
    assert!(session.summary().approval.is_none());
}

/// WI-1314: a reply that reports blocked and signs off with the marker in
/// the same turn stays blocked; the marker is that turn's, not a later one.
#[tokio::test]
async fn a_done_marker_in_the_reporting_turn_leaves_it_blocked() {
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let go = dir.path().join("go");
    let id = create_session_with(
        &client,
        &base,
        json!({
            "name": "blocked-same-turn",
            "command": turn_script(&format!(
                "while [ ! -e {go} ]; do printf .; sleep 0.05; done; \
                 printf 'blocked on a person\\r\\nAUTOPILOT: DONE\\r\\n'",
                go = go.display()
            )),
        }),
    )
    .await;
    // Mid-turn (the dots keep it running): report, then sign off.
    wait_for_activity(&client, &base, &id, &["running"]).await;
    report_blocked(&client, &base, &id).await;
    assert_eq!(
        blocked_report(&client, &base, &id).await["reason"],
        "push refused"
    );
    std::fs::write(&go, b"").unwrap();
    live_output_containing(&client, &base, &id, &["AUTOPILOT: DONE"]).await;
    wait_for_activity(&client, &base, &id, RESTING).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        blocked_report(&client, &base, &id).await["reason"],
        "push refused"
    );
}
