use super::*;

#[tokio::test]
async fn a_session_that_starts_waiting_for_input_wakes_a_phone() {
    // The headline notification case, and the one the drift watcher's unit tests do
    // not touch: `spawn_activity_watcher` reads the bus and turns a state
    // change into a push. Driven end to end because the routing is the
    // requirement — a watcher that stopped subscribing, a `notify` that lost
    // its kind, or a preference that stopped meaning what it says would each
    // leave a phone silent, and none of them is visible from inside the
    // heuristic that produced the state.
    let (log, push_base) = start_stand_in_push_service().await;
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    subscribe_for_kind(&client, &base, &push_base, "waiting_for_input").await;
    subscribe_for_kind(&client, &base, &push_base, "errored").await;

    let created: Value = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "asks-a-question",
            "command": ["/bin/sh", "-lc", "printf 'Continue? [y/N]'; sleep 30"],
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap().to_string();

    delivered_to(&log, "/waiting_for_input").await;
    // The session is alive and waiting, so nothing has errored — and the
    // device that only asked about errors must not have been woken.
    nothing_delivered_to(&log, "/errored");

    // Killing it here would error the session and push again, so every
    // assertion is made before the cleanup rather than after it.
    let _ = client
        .delete(format!("{base}/api/sessions/{id}"))
        .send()
        .await;
}

#[tokio::test]
async fn a_session_that_exits_badly_wakes_a_phone() {
    let (log, push_base) = start_stand_in_push_service().await;
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    subscribe_for_kind(&client, &base, &push_base, "waiting_for_input").await;
    subscribe_for_kind(&client, &base, &push_base, "errored").await;

    let created: Value = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "falls-over",
            "command": ["/bin/sh", "-lc", "printf 'it went wrong\\n'; exit 3"],
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap().to_string();

    delivered_to(&log, "/errored").await;
    // Nothing about that output looks like a prompt, so the other device
    // stays quiet: the two are routed by the state a session reached, not by
    // the fact that something happened to it.
    nothing_delivered_to(&log, "/waiting_for_input");

    let _ = client
        .delete(format!("{base}/api/sessions/{id}"))
        .send()
        .await;
}

#[tokio::test]
async fn the_agent_task_notify_hook_wakes_a_phone() {
    // The third of the named notification events, and the only one that comes from a
    // task rather than from a session's state. A run's finding is asserted
    // elsewhere; that the finding also *interrupts somebody* is this, and it
    // is the half an unattended task exists for.
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = test_config();
    cfg.default_cwd = tmp.path().to_path_buf();
    cfg.workspace_root = tmp.path().canonicalize().unwrap();
    cfg.state_dir = tmp.path().join("state");

    let (log, push_base) = start_stand_in_push_service().await;
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    subscribe_for_kind(&client, &base, &push_base, "agent_task_notify").await;
    subscribe_for_kind(&client, &base, &push_base, "waiting_for_input").await;

    let created: Value = client
        .post(format!("{base}/api/agent-tasks"))
        .json(&json!({
            "name": "Nightly sweep",
            "prompt": "Look for unresolved internal references.",
            "schedule": { "kind": "manual" },
            // The sleep is not decoration: the watcher subscribes just after
            // the session is created, and a `printf` that finished first
            // would be a race rather than a test.
            "command": ["/bin/sh", "-lc",
                "sleep 0.3; printf 'VOGT_NOTIFY:two references are dangling\\n'"],
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let task_id = created["id"].as_str().unwrap().to_string();

    let run = client
        .post(format!("{base}/api/agent-tasks/{task_id}/run"))
        .send()
        .await
        .unwrap();
    assert_eq!(run.status(), StatusCode::OK);

    delivered_to(&log, "/agent_task_notify").await;
    // The task's session never asked a question, so the device watching for
    // that was not woken — the hook is routed as its own kind.
    nothing_delivered_to(&log, "/waiting_for_input");
}

/// WI-830: rows a previous engine process left unfinished (killed by a
/// restart it never drained) are closed out when the next one boots.
#[tokio::test]
async fn startup_closes_out_history_rows_left_unfinished_by_a_restart() {
    use vogt_engine_server::history::{ArchiveRecord, SessionHistory};

    let mut cfg = test_config();
    let state_dir = cfg.state_dir.clone();

    // The previous process: a provisional row and some output, then gone.
    let lost = uuid::Uuid::new_v4();
    {
        let history = SessionHistory::new(&state_dir).await.unwrap();
        history
            .archive_session(ArchiveRecord {
                id: lost,
                name: "lost-to-redeploy".into(),
                created_at: OffsetDateTime::now_utc() - time::Duration::minutes(5),
                ended_at: None,
                exit_code: None,
                cwd: None,
                command: None,
                scrollback_bytes: 0,
                end_reason: None,
                identity: None,
                economics: None,
            })
            .await
            .unwrap();
        std::fs::write(history.log_path(lost), b"working...\n").unwrap();
        // An engine from before `end_reason` existed: the next boot must
        // add the column to the existing table.
        sqlx::query("ALTER TABLE sessions DROP COLUMN end_reason")
            .execute(&history.pool)
            .await
            .unwrap();
        history.pool.close().await;
    }

    cfg.state_dir = state_dir.clone();
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let rows: Vec<Value> = client
        .get(format!("{base}/api/history/sessions?limit=20"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let row = rows.iter().find(|r| r["id"] == lost.to_string()).unwrap();
    assert!(row["ended_at"].is_string(), "{row:?}");
    assert!(row["exit_code"].is_null(), "the code is unknown: {row:?}");
    assert_eq!(row["end_reason"], "engine-restart", "{row:?}");

    // A session of this process is not touched by the reconcile. Its
    // provisional history row is written by a task spawned at launch, so it
    // is polled for rather than assumed to have landed within a fixed sleep:
    // on a loaded runner the spawn had not run 200 ms later and the row was
    // simply absent.
    let id = create_session_with(
        &client,
        &base,
        json!({ "name": "fresh", "command": ["/bin/cat"] }),
    )
    .await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let row = loop {
        let rows: Vec<Value> = client
            .get(format!("{base}/api/history/sessions?limit=20"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if let Some(row) = rows.iter().find(|r| r["id"] == id) {
            break row.clone();
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the fresh session never got its provisional history row: {rows:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert!(row["ended_at"].is_null(), "{row:?}");
    assert!(row["end_reason"].is_null(), "{row:?}");

    // A live child holds a blocking-pool waiter the test runtime would wait
    // on forever at shutdown.
    client
        .delete(format!("{base}/api/sessions/{id}"))
        .send()
        .await
        .unwrap();
}

#[tokio::test]
async fn a_hibernated_agent_frees_its_processes_and_wakes_into_the_same_conversation() {
    let (tmp, cfg, stub) = hibernation_sandbox();
    let state_dir = cfg.state_dir.clone();
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let created: Value = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "sleepy agent",
            "prompt": "## Task\n\nDo the thing.\n",
            "command": [stub.to_string_lossy()],
            "env": [["VOGT_HTTP_TOKEN", "first-secret"], ["KEEP", "kept"]],
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap().to_string();
    assert_eq!(created["conversation"]["agent"], "claude");
    assert_eq!(created["conversation"]["id"], id.as_str());
    let printed = live_output_containing(&client, &base, &id, &["helper=[", "> "]).await;
    assert!(
        printed.contains(&format!("arg=[--session-id]\r\narg=[{id}]")),
        "{printed:?}"
    );
    let helper: i32 = printed_value(&printed, "helper").parse().unwrap();
    assert!(pid_alive(helper));

    // The record exists from the start, and holds no secret.
    let record_path = state_dir.join("sessions").join(format!("{id}.json"));
    let record = std::fs::read_to_string(&record_path).unwrap();
    assert!(!record.contains("first-secret"), "{record}");
    assert!(record.contains("\"KEEP\""), "{record}");

    // A blocked report is part of the session, so hibernating must keep it.
    let blocked = client
        .post(format!("{base}/api/sessions/{id}/blocked"))
        .json(&json!({ "blocked": true, "reason": "needs the bot token", "items": ["create the bot"] }))
        .send()
        .await
        .unwrap();
    assert_eq!(blocked.status(), StatusCode::OK);

    // Hibernate: the whole tree goes, the session stays listed.
    let hibernated = client
        .post(format!("{base}/api/sessions/{id}/hibernate"))
        .json(&json!({ "reason": "idle for a test" }))
        .send()
        .await
        .unwrap();
    assert_eq!(hibernated.status(), StatusCode::OK);
    let hibernated: Value = hibernated.json().await.unwrap();
    assert_eq!(hibernated["activity"], "hibernated");
    assert_eq!(hibernated["alive"], false);
    assert_eq!(hibernated["hibernation"]["trigger"], "manual");
    assert_eq!(hibernated["hibernation"]["reason"], "idle for a test");
    assert_eq!(hibernated["hibernation"]["resumable"], true);
    assert_eq!(hibernated["blocked"]["reason"], "needs the bot token");
    assert_eq!(hibernated["blocked"]["items"], json!(["create the bot"]));
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!pid_alive(helper), "the agent's child should be gone");

    let listed: Value = client
        .get(format!("{base}/api/sessions"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let listed: Vec<Value> = serde_json::from_value(listed["sessions"].clone()).unwrap();
    let row = listed.iter().find(|s| s["id"] == id.as_str()).unwrap();
    assert_eq!(row["activity"], "hibernated");
    assert_eq!(row["blocked"]["reason"], "needs the bot token");

    // Its last screen is still readable, and reading it does not wake it.
    let screen: Value = client
        .get(format!("{base}/api/sessions/{id}/screen"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(screen["activity"], "hibernated");
    assert_eq!(screen["ready"], false);
    let lines = screen["lines"].as_array().unwrap();
    assert!(
        lines
            .iter()
            .any(|l| l.as_str().unwrap().contains("keep=[kept]")),
        "{screen}"
    );

    // Input is refused with the way to wake it.
    let typed = client
        .post(format!("{base}/api/sessions/{id}/input"))
        .json(&json!({ "text": "hello" }))
        .send()
        .await
        .unwrap();
    assert_eq!(typed.status(), StatusCode::CONFLICT);
    assert!(typed.text().await.unwrap().contains("wake"));

    // Wake under the same id, resuming the conversation, with a new token,
    // without being told to read the brief again. Restoring the blocked report
    // must not publish it again: the operator was already told, and a publish
    // is a push.
    let stream_res = client
        .get(format!("{base}/api/events"))
        .send()
        .await
        .unwrap();
    assert_eq!(stream_res.status(), StatusCode::OK);
    let mut stream = stream_res.bytes_stream();
    let woken = client
        .post(format!("{base}/api/sessions/{id}/wake"))
        .json(&json!({ "env": [["VOGT_HTTP_TOKEN", "second-secret"]] }))
        .send()
        .await
        .unwrap();
    assert_eq!(woken.status(), StatusCode::OK);
    let woken: Value = woken.json().await.unwrap();
    assert_eq!(woken["id"], id.as_str());
    assert_eq!(woken["alive"], true);
    assert_eq!(woken["blocked"]["reason"], "needs the bot token");
    assert_eq!(woken["blocked"]["items"], json!(["create the bot"]));
    let want = id.clone();
    let republished = tokio::time::timeout(Duration::from_millis(500), async {
        loop {
            let chunk = stream
                .next()
                .await
                .expect("the event stream ended")
                .expect("the event stream failed");
            let text = String::from_utf8_lossy(&chunk);
            for line in text.lines() {
                let Some(raw) = line.strip_prefix("data: ") else {
                    continue;
                };
                let Ok(event) = serde_json::from_str::<Value>(raw) else {
                    continue;
                };
                if event["type"] == "session-blocked" && event["id"] == want.as_str() {
                    return true;
                }
            }
        }
    })
    .await;
    assert!(
        republished.is_err(),
        "waking a blocked session must not republish session-blocked"
    );
    let printed = live_output_containing(&client, &base, &id, &["helper=["]).await;
    assert!(
        printed.contains(&format!("arg=[--resume]\r\narg=[{id}]")),
        "{printed:?}"
    );
    assert!(!printed.contains("--session-id"), "{printed:?}");
    assert!(
        !printed.contains("Vogt started this session"),
        "{printed:?}"
    );
    assert!(
        printed.contains("token=[second-secret] keep=[kept]"),
        "{printed:?}"
    );
    let record = std::fs::read_to_string(&record_path).unwrap();
    assert!(
        !record.contains("second-secret") && !record.contains("\"hibernation\""),
        "{record}"
    );

    // Killing it ends it: the record goes, so a restart will not bring it back.
    let killed = client
        .post(format!("{base}/api/sessions/{id}/kill"))
        .send()
        .await
        .unwrap();
    assert_eq!(killed.status(), StatusCode::OK);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while record_path.exists() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the record outlived the kill"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    drop(tmp);
}

/// A compacting wake (WI-2037) does not resume: it starts a new conversation
/// under a new id whose first prompt is the summary, the git facts and the
/// original brief; the next wake resumes that new conversation.
#[tokio::test]
async fn a_cold_wake_with_a_summary_starts_a_fresh_conversation() {
    let (tmp, cfg, stub) = hibernation_sandbox();
    let state_dir = cfg.state_dir.clone();
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let created: Value = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "cold agent",
            "prompt": "## Task\n\nDo the original thing.\n",
            "command": [stub.to_string_lossy()],
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap().to_string();
    live_output_containing(&client, &base, &id, &["helper=["]).await;
    let hibernate = |client: reqwest::Client, base: String, id: String| async move {
        let res = client
            .post(format!("{base}/api/sessions/{id}/hibernate"))
            .json(&json!({ "reason": "idle" }))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    };
    hibernate(client.clone(), base.clone(), id.clone()).await;

    // Too large a summary is refused before anything starts.
    let refused = client
        .post(format!("{base}/api/sessions/{id}/wake"))
        .json(&json!({ "fresh_prompt": "x".repeat(65 * 1024) }))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);

    let woken: Value = client
        .post(format!("{base}/api/sessions/{id}/wake"))
        .json(&json!({ "fresh_prompt": "# Resumed from a summary\n\nState: half done.\n" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let fresh = woken["conversation"]["id"].as_str().unwrap().to_string();
    assert_ne!(fresh, id, "a fresh conversation needs its own id");
    let printed = live_output_containing(&client, &base, &id, &["helper=["]).await;
    assert!(!printed.contains("--resume"), "{printed:?}");
    assert!(
        printed.contains(&format!("arg=[--session-id]\r\narg=[{fresh}]")),
        "{printed:?}"
    );
    let brief = std::fs::read_to_string(
        state_dir
            .join("agent-task-prompts")
            .join("sessions")
            .join(format!("{id}.md")),
    )
    .unwrap();
    assert!(brief.starts_with("# Resumed from a summary"), "{brief}");
    assert!(brief.contains("## Git (session directory)"), "{brief}");
    assert!(brief.contains("# Original brief"), "{brief}");
    assert!(brief.contains("Do the original thing."), "{brief}");

    // The next plain wake resumes the new conversation, not the old one.
    hibernate(client.clone(), base.clone(), id.clone()).await;
    let woken = client
        .post(format!("{base}/api/sessions/{id}/wake"))
        .send()
        .await
        .unwrap();
    assert_eq!(woken.status(), StatusCode::OK);
    let printed = live_output_containing(&client, &base, &id, &["helper=["]).await;
    assert!(
        printed.contains(&format!("arg=[--resume]\r\narg=[{fresh}]")),
        "{printed:?}"
    );
    drop(tmp);
}

/// A budget is part of the session, not of the process: hibernating and
/// waking under the same id must bring back the same limits (WI-1245 / C7).
#[tokio::test]
async fn a_sessions_budget_survives_hibernate_and_wake() {
    let (tmp, cfg, stub) = hibernation_sandbox();
    let _tmp = tmp;
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let created: Value = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "budgeted agent",
            "prompt": "## Task\n\nDo the thing.\n",
            "command": [stub.to_string_lossy()],
            "budget": { "turns": 120, "wall_clock_minutes": 90 },
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap().to_string();
    assert_eq!(created["budget"]["turns"], 120);
    assert_eq!(created["budget"]["wall_clock_minutes"], 90);
    assert!(created["budget"].get("context_tokens").is_none());

    let hibernated = client
        .post(format!("{base}/api/sessions/{id}/hibernate"))
        .json(&json!({ "reason": "budget round trip" }))
        .send()
        .await
        .unwrap();
    assert_eq!(hibernated.status(), StatusCode::OK);
    let hibernated: Value = hibernated.json().await.unwrap();
    assert_eq!(hibernated["activity"], "hibernated");
    assert_eq!(hibernated["budget"]["turns"], 120);
    assert_eq!(hibernated["budget"]["wall_clock_minutes"], 90);

    let listed: Value = client
        .get(format!("{base}/api/sessions"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let listed: Vec<Value> = serde_json::from_value(listed["sessions"].clone()).unwrap();
    let row = listed.iter().find(|s| s["id"] == id.as_str()).unwrap();
    assert_eq!(row["budget"]["turns"], 120);
    assert_eq!(row["budget"]["wall_clock_minutes"], 90);

    let woken = client
        .post(format!("{base}/api/sessions/{id}/wake"))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(woken.status(), StatusCode::OK);
    let woken: Value = woken.json().await.unwrap();
    assert_eq!(woken["id"], id.as_str());
    assert_eq!(woken["alive"], true);
    assert_eq!(woken["budget"]["turns"], 120);
    assert_eq!(woken["budget"]["wall_clock_minutes"], 90);
    assert!(woken["budget"].get("context_tokens").is_none());
}

#[tokio::test]
async fn a_shell_hibernates_only_when_asked_and_wakes_fresh() {
    let (_tmp, cfg, _stub) = hibernation_sandbox();
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let created: Value = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({ "name": "plain shell" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap().to_string();
    assert!(created.get("conversation").is_none());

    let refused = client
        .post(format!("{base}/api/sessions/{id}/hibernate"))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), StatusCode::CONFLICT);
    assert!(refused.text().await.unwrap().contains("allow_shell"));

    // WI-3878: `force` opens the not-attached-agent gate only; a shell still
    // needs `allow_shell` and no conversation to resume.
    let forced = client
        .post(format!("{base}/api/sessions/{id}/hibernate"))
        .json(&json!({ "force": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(forced.status(), StatusCode::CONFLICT);
    assert!(forced.text().await.unwrap().contains("allow_shell"));

    let hibernated: Value = client
        .post(format!("{base}/api/sessions/{id}/hibernate"))
        .json(&json!({ "allow_shell": true }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(hibernated["hibernation"]["resumable"], false);

    let woken: Value = client
        .post(format!("{base}/api/sessions/{id}/wake"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(woken["alive"], true);
    assert_eq!(woken["id"], id.as_str());
}

#[tokio::test]
async fn sessions_survive_an_engine_restart_as_hibernated() {
    let (_tmp, cfg, stub) = hibernation_sandbox();
    let state_dir = cfg.state_dir.clone();

    // A graceful shutdown hibernates every agent session it can.
    let (base, state, guard) = boot_with_state(cfg.clone()).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let created: Value = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({ "name": "survivor", "command": [stub.to_string_lossy()] }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap().to_string();
    live_output_containing(&client, &base, &id, &["helper=["]).await;
    state.sessions.hibernate_for_shutdown().await;
    drop(guard);

    // An engine that died without hibernating leaves its write-ahead record:
    // stand one in for a session the engine never got to.
    let lost = uuid::Uuid::new_v4();
    std::fs::write(
        state_dir.join("sessions").join(format!("{lost}.json")),
        serde_json::to_vec(&json!({
            "version": 1,
            "id": lost,
            "name": "lost in a SIGKILL",
            "created_at": "2026-10-05T00:00:00Z",
            "command": [stub.to_string_lossy()],
            "conversation": { "agent": "claude", "id": lost },
        }))
        .unwrap(),
    )
    .unwrap();

    let (base, _h) = boot_with_config(cfg).await;
    let listed: Value = client
        .get(format!("{base}/api/sessions"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let listed: Vec<Value> = serde_json::from_value(listed["sessions"].clone()).unwrap();
    let find = |want: &str| {
        listed
            .iter()
            .find(|s| s["id"] == want)
            .unwrap_or_else(|| panic!("{want} not listed in {listed:?}"))
            .clone()
    };
    let survivor = find(&id);
    assert_eq!(survivor["activity"], "hibernated");
    assert_eq!(survivor["hibernation"]["trigger"], "shutdown");
    let recovered = find(&lost.to_string());
    assert_eq!(recovered["hibernation"]["trigger"], "recovered");

    let woken: Value = client
        .post(format!("{base}/api/sessions/{id}/wake"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(woken["alive"], true);
    let printed = live_output_containing(&client, &base, &id, &["helper=["]).await;
    assert!(
        printed.contains(&format!("arg=[--resume]\r\narg=[{id}]")),
        "{printed:?}"
    );
}

#[tokio::test]
async fn attaching_to_a_hibernated_session_replays_its_screen_and_does_not_wake_it() {
    let (_tmp, cfg, stub) = hibernation_sandbox();
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let created: Value = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({ "name": "attach me", "command": [stub.to_string_lossy()] }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap().to_string();
    live_output_containing(&client, &base, &id, &["helper=["]).await;
    let status = client
        .post(format!("{base}/api/sessions/{id}/hibernate"))
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(status, StatusCode::OK);

    let ws_url = format!(
        "{}/api/sessions/{id}/attach",
        base.replacen("http://", "ws://", 1)
    );
    let (mut ws, _) = tokio_tungstenite::connect_async(ws_url).await.unwrap();
    ws.send(Message::Text(
        json!({ "type": "auth", "token": TEST_TOKEN })
            .to_string()
            .into(),
    ))
    .await
    .unwrap();
    let mut texts = Vec::new();
    let mut bytes = Vec::new();
    while let Some(Ok(msg)) = ws.next().await {
        match msg {
            Message::Text(t) => texts.push(t.to_string()),
            Message::Binary(b) => bytes.extend_from_slice(&b),
            Message::Close(_) => break,
            _ => {}
        }
    }
    assert!(
        texts.iter().any(|t| t.contains("snapshot-start")),
        "{texts:?}"
    );
    assert!(
        texts.last().unwrap().contains("\"hibernated\""),
        "{texts:?}"
    );
    assert!(String::from_utf8_lossy(&bytes).contains("helper=["));

    let listed: Value = client
        .get(format!("{base}/api/sessions"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let listed: Vec<Value> = serde_json::from_value(listed["sessions"].clone()).unwrap();
    let row = listed.iter().find(|s| s["id"] == id.as_str()).unwrap();
    assert_eq!(row["activity"], "hibernated", "attach must not wake it");
}

#[tokio::test]
async fn the_idle_policy_hibernates_only_what_no_exemption_covers() {
    use vogt_engine_server::hibernate_policy::{exemption, run_once, Policy};

    let (tmp, cfg, _stub) = hibernation_sandbox();
    let (base, state, _h) = boot_with_state(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let quiet = "#!/bin/sh\nsleep 300 &\nprintf 'helper=[%s]\\n> ' \"$!\"\nwait\n";
    // A tool call in progress: a shell below the agent.
    let tooling = "#!/bin/sh\nsh -c 'sleep 300' &\nprintf 'helper=[%s]\\n> ' \"$!\"\nwait\n";
    // A turn that keeps printing.
    let busy = "#!/bin/sh\nprintf 'helper=[0]\\n'\nwhile :; do printf .; sleep 0.02; done\n";
    // A turn whose only sign of life is a colour-cycled prompt (WI-1090):
    // the screen's text never changes, so activity reads idle, but its bytes
    // keep coming and a live turn must not be hibernated on that misreading.
    let shimmer = "#!/bin/sh\nprintf 'helper=[0]\\n> '\nwhile :; do printf '\\r\\033[7m>\\033[0m '; sleep 0.02; printf '\\r\\033[2m>\\033[0m '; sleep 0.02; done\n";
    let ids = start_stub_agents(
        &client,
        &base,
        &tmp.path().join("agents"),
        &[
            ("quiet", quiet),
            ("pinned", quiet),
            ("blocked", quiet),
            ("tooling", tooling),
            ("busy", busy),
            ("shimmer", shimmer),
        ],
    )
    .await;
    let [quiet_id, pinned, blocked, tooling_id, busy_id, shimmer_id] =
        [0, 1, 2, 3, 4, 5].map(|i| ids[i].clone());
    let ok = client
        .post(format!("{base}/api/sessions/{pinned}/keep-awake"))
        .json(&json!({ "keep_awake": true }))
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(ok, StatusCode::OK);
    let ok = client
        .post(format!("{base}/api/sessions/{blocked}/blocked"))
        .json(&json!({ "blocked": true, "reason": "needs the operator" }))
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(ok, StatusCode::OK);
    let why = |id: &str| {
        let session = state.sessions.get(id.parse().unwrap()).unwrap();
        exemption(&state.sessions, &session)
    };
    // Until the quiet ones settle at idle (200 ms here), the quiet one has
    // been quiet past the policy's 500 ms, and the busy one is seen running; a loaded runner can take longer than any fixed sleep.
    let settled = || {
        why(&quiet_id).is_none()
            && state
                .sessions
                .get(quiet_id.parse().unwrap())
                .unwrap()
                .quiet_for()
                >= Duration::from_millis(700)
            && why(&pinned).as_deref() == Some("pinned awake")
            && why(&blocked).as_deref() == Some("blocked on a person")
            && why(&tooling_id).is_some_and(|w| w.contains("shell is running below the agent"))
            && why(&busy_id).as_deref() == Some("a turn is running")
            && why(&shimmer_id).as_deref() == Some("output is still arriving")
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !settled() && std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(why(&quiet_id), None);
    assert_eq!(why(&pinned).as_deref(), Some("pinned awake"));
    assert_eq!(why(&blocked).as_deref(), Some("blocked on a person"));
    assert!(
        why(&tooling_id).is_some_and(|w| w.contains("shell is running below the agent")),
        "{:?}",
        why(&tooling_id)
    );
    assert_eq!(why(&busy_id).as_deref(), Some("a turn is running"));
    let shimmering = state.sessions.get(shimmer_id.parse().unwrap()).unwrap();
    assert_eq!(
        shimmering.activity(),
        vogt_engine_contract::ActivityState::Idle
    );
    assert_eq!(
        why(&shimmer_id).as_deref(),
        Some("output is still arriving")
    );

    run_once(
        &state.sessions,
        &Policy {
            idle_after: Some(Duration::from_millis(500)),
            memavailable_below: None,
            live_cap: None,
            done_after: None,
        },
    )
    .await;

    let listed: Value = client
        .get(format!("{base}/api/sessions"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let listed: Vec<Value> = serde_json::from_value(listed["sessions"].clone()).unwrap();
    let activity = |id: &str| {
        listed
            .iter()
            .find(|s| s["id"] == id)
            .map(|s| s["activity"].as_str().unwrap().to_string())
            .unwrap()
    };
    assert_eq!(activity(&quiet_id), "hibernated");
    let row = listed
        .iter()
        .find(|s| s["id"] == quiet_id.as_str())
        .unwrap();
    assert_eq!(row["hibernation"]["trigger"], "idle");
    for id in [&pinned, &blocked, &tooling_id, &busy_id, &shimmer_id] {
        assert_ne!(activity(id), "hibernated", "{id} should have been exempt");
    }
}

/// WI-4457: the idle cap is changed on a running engine and takes effect on the
/// next sweep with no restart; an idle worker over it hibernates, and an
/// oversight session quiet just as long is spared.
#[tokio::test]
async fn an_idle_cap_changed_live_hibernates_workers_and_spares_oversight() {
    use vogt_engine_server::hibernate_policy::run_once;

    let (tmp, cfg, stub) = hibernation_sandbox();
    let (base, state, _h) = boot_with_state(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let mut ids = Vec::new();
    for (name, role) in [("overseer", Some("oversight")), ("a worker", None)] {
        let mut body = json!({ "name": name, "command": [stub.to_string_lossy()] });
        if let Some(role) = role {
            body["role"] = json!(role);
        }
        let created: Value = client
            .post(format!("{base}/api/sessions"))
            .json(&body)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        ids.push(created["id"].as_str().unwrap().to_string());
    }
    for id in &ids {
        live_output_containing(&client, &base, id, &["helper=["]).await;
    }
    let (overseer, worker) = (ids[0].clone(), ids[1].clone());
    // Quiet for longer than the threshold the test is about to set.
    tokio::time::sleep(Duration::from_millis(1500)).await;

    // The boot policy is off: a sweep hibernates nothing.
    run_once(&state.sessions, &state.hibernation.get()).await;
    let activity = |listed: &[Value], id: &str| {
        listed
            .iter()
            .find(|s| s["id"] == id)
            .map(|s| s["activity"].as_str().unwrap().to_string())
            .unwrap()
    };
    let list = |client: reqwest::Client| {
        let base = base.clone();
        async move {
            let listed: Value = client
                .get(format!("{base}/api/sessions"))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            serde_json::from_value::<Vec<Value>>(listed["sessions"].clone()).unwrap()
        }
    };
    let listed = list(client.clone()).await;
    assert_ne!(activity(&listed, &worker), "hibernated");

    // Set the idle cap on the running engine.
    let put = client
        .put(format!("{base}/api/hibernation/policy"))
        .json(&json!({ "idle_after": 1 }))
        .send()
        .await
        .unwrap();
    assert_eq!(put.status(), StatusCode::OK);
    let after: Value = put.json().await.unwrap();
    assert_eq!(after["idle_after"], 1);
    let read: Value = client
        .get(format!("{base}/api/hibernation/policy"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        read["idle_after"], 1,
        "the change is what the next read sees"
    );
    assert!(
        tmp.path().join("state/hibernation-policy.json").is_file(),
        "an accepted change is saved, so it survives a restart"
    );

    // The next sweep reads the new cap: the worker goes, the overseer stays.
    run_once(&state.sessions, &state.hibernation.get()).await;
    let listed = list(client.clone()).await;
    assert_eq!(activity(&listed, &worker), "hibernated");
    let row = listed.iter().find(|s| s["id"] == worker.as_str()).unwrap();
    assert_eq!(row["hibernation"]["trigger"], "idle");
    assert_ne!(
        activity(&listed, &overseer),
        "hibernated",
        "an oversight session is spared the idle trigger"
    );

    // A cap the watcher cannot take live is refused, and nothing changes.
    let refused = client
        .put(format!("{base}/api/hibernation/policy"))
        .json(&json!({ "live_cap": 3 }))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        state.hibernation.get().idle_after,
        Some(Duration::from_secs(1))
    );
}

#[tokio::test]
async fn only_a_person_or_an_oversight_session_changes_the_hibernation_caps() {
    let (_tmp, mut cfg, stub) = hibernation_sandbox();
    // Every writer holds `sessions`; the policy route still refuses them.
    cfg.vogt_core_url = Some(
        stand_in_core_knowing(vec![
            (
                "worker-token-1234567890abcdef",
                "agent:session:ses_workerwork0000000000001",
                vec!["read", "work.write"],
            ),
            (
                "reader-token-1234567890abcdef",
                "agent:session:ses_readerread0000000000001",
                vec!["read"],
            ),
            (
                "person-token-1234567890abcdef",
                "human:ada",
                vec!["read", "work.write"],
            ),
            (
                "overseer-token-1234567890abcdef",
                "agent:session:ses_overseerover0000000001",
                vec!["read", "work.write"],
            ),
        ])
        .await,
    );
    let (base, _state, _h) = boot_with_state(cfg).await;
    let admin = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    // The oversight session carries the VOGT_SESSION_ID the stand-in core
    // names for the overseer token, so the engine can find its role.
    let overseer = admin
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "overseer",
            "command": [stub.to_string_lossy()],
            "role": "oversight",
            "env": [["VOGT_SESSION_ID", "ses_overseerover0000000001"]],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(overseer.status(), StatusCode::OK);
    let change = json!({ "idle_after": 1 });
    let put_as = |token: &'static str| {
        let base = base.clone();
        let change = change.clone();
        async move {
            reqwest::Client::builder()
                .default_headers(auth_for(token))
                .build()
                .unwrap()
                .put(format!("{base}/api/hibernation/policy"))
                .json(&change)
                .send()
                .await
                .unwrap()
                .status()
        }
    };
    for token in [
        "worker-token-1234567890abcdef",
        "reader-token-1234567890abcdef",
    ] {
        assert_eq!(
            put_as(token).await,
            StatusCode::FORBIDDEN,
            "{token} must not change the caps"
        );
    }
    // Reading the caps is open to any valid token, a reader's included.
    let read = reqwest::Client::new()
        .get(format!("{base}/api/hibernation/policy"))
        .headers(auth_for("reader-token-1234567890abcdef"))
        .send()
        .await
        .unwrap();
    assert_eq!(read.status(), StatusCode::OK, "reading the caps is open");
    assert_eq!(
        put_as("person-token-1234567890abcdef").await,
        StatusCode::OK
    );
    assert_eq!(
        put_as("overseer-token-1234567890abcdef").await,
        StatusCode::OK
    );
    // A worker's refusal left the caps as they were; the person's change stuck.
    let after: Value = reqwest::Client::new()
        .get(format!("{base}/api/hibernation/policy"))
        .headers(auth_for("person-token-1234567890abcdef"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(after["idle_after"], 1);
}

#[tokio::test]
async fn a_session_pinned_awake_wakes_by_itself_after_a_restart() {
    let (_tmp, cfg, stub) = hibernation_sandbox();
    let (base, state, guard) = boot_with_state(cfg.clone()).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let mut ids = Vec::new();
    for name in ["the driver", "a worker"] {
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
        live_output_containing(&client, &base, id, &["helper=["]).await;
    }
    client
        .post(format!("{base}/api/sessions/{}/keep-awake", ids[0]))
        .json(&json!({ "keep_awake": true }))
        .send()
        .await
        .unwrap();
    state.sessions.hibernate_for_shutdown().await;
    drop(guard);

    let (base, _h) = boot_with_config(cfg).await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
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
        let activity = |id: &str| {
            listed
                .iter()
                .find(|s| s["id"] == id)
                .map(|s| s["activity"].as_str().unwrap().to_string())
        };
        if activity(&ids[0]).as_deref() != Some("hibernated") {
            assert_eq!(
                activity(&ids[1]).as_deref(),
                Some("hibernated"),
                "only the pinned one wakes"
            );
            let driver = listed.iter().find(|s| s["id"] == ids[0].as_str()).unwrap();
            assert_eq!(driver["alive"], true);
            assert_eq!(driver["keep_awake"], true, "the pin survives the wake");
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the pinned session never woke"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test]
async fn an_oversight_session_is_pinned_listed_as_oversight_and_back_after_a_restart() {
    let (_tmp, cfg, stub) = hibernation_sandbox();
    let (base, state, guard) = boot_with_state(cfg.clone()).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    // One nominated at start, one nominated afterwards, one left a worker.
    let mut ids = Vec::new();
    for (name, role) in [
        ("overseer", Some("oversight")),
        ("promoted", None),
        ("a worker", None),
    ] {
        let mut body = json!({ "name": name, "command": [stub.to_string_lossy()] });
        if let Some(role) = role {
            body["role"] = json!(role);
        }
        let created: Value = client
            .post(format!("{base}/api/sessions"))
            .json(&body)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        ids.push(created["id"].as_str().unwrap().to_string());
    }
    for id in &ids {
        live_output_containing(&client, &base, id, &["helper=["]).await;
    }
    let promoted: Value = client
        .post(format!("{base}/api/sessions/{}/role", ids[1]))
        .json(&json!({ "role": "oversight" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(promoted["role"], "oversight");
    assert_eq!(
        promoted["keep_awake"], true,
        "oversight pins the session awake"
    );

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
    assert_eq!(row(&ids[0])["role"], "oversight");
    assert_eq!(row(&ids[0])["keep_awake"], true);
    assert!(
        row(&ids[2]).get("role").is_none(),
        "a worker carries no role on the wire"
    );
    assert!(row(&ids[2]).get("keep_awake").is_none());

    // Unknown roles are refused rather than read as a worker.
    let refused = client
        .post(format!("{base}/api/sessions/{}/role", ids[2]))
        .json(&json!({ "role": "boss" }))
        .send()
        .await
        .unwrap();
    assert!(refused.status().is_client_error());

    state.sessions.hibernate_for_shutdown().await;
    drop(guard);

    let (base, _h) = boot_with_config(cfg).await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
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
        let row = |id: &str| listed.iter().find(|s| s["id"] == id).cloned();
        let awake = |id: &str| row(id).is_some_and(|r| r["activity"] != "hibernated");
        if awake(&ids[0]) && awake(&ids[1]) {
            for id in &ids[..2] {
                let r = row(id).unwrap();
                assert_eq!(r["role"], "oversight", "the role survives the restart");
                assert_eq!(r["alive"], true);
            }
            assert_eq!(
                row(&ids[2]).unwrap()["activity"],
                "hibernated",
                "the worker waits to be woken"
            );
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the oversight sessions never woke"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // A woken overseer is told, at its prompt, that it was resumed (WI-962).
    for id in &ids[..2] {
        live_output_containing(
            &client,
            &base,
            id,
            &["[vogt] This oversight session was resumed after the engine restarted"],
        )
        .await;
    }
}

#[tokio::test]
async fn a_work_item_label_is_set_cleared_kept_in_history_and_survives_a_restart() {
    let (_tmp, cfg, stub) = hibernation_sandbox();
    let (base, state, guard) = boot_with_state(cfg.clone()).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    // One labelled at start, one labelled afterwards.
    let mut ids = Vec::new();
    for (name, work_item) in [("on WI-7", Some(" WI-7 ")), ("bound later", None)] {
        let mut body = json!({ "name": name, "command": [stub.to_string_lossy()] });
        if let Some(work_item) = work_item {
            body["work_item"] = json!(work_item);
        }
        let created: Value = client
            .post(format!("{base}/api/sessions"))
            .json(&body)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        ids.push(created["id"].as_str().unwrap().to_string());
    }
    for id in &ids {
        live_output_containing(&client, &base, id, &["helper=["]).await;
    }
    let set = |id: String, work_item: Value| {
        let client = client.clone();
        let base = base.clone();
        async move {
            client
                .post(format!("{base}/api/sessions/{id}/work-item"))
                .json(&json!({ "work_item": work_item }))
                .send()
                .await
                .unwrap()
        }
    };
    let bound: Value = set(ids[1].clone(), json!("WI-8"))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(bound["work_item"], "WI-8");

    // A label the engine will not keep is refused, not cut; the old one stays.
    let refused = set(ids[1].clone(), json!("x".repeat(201))).await;
    assert_eq!(refused.status(), reqwest::StatusCode::BAD_REQUEST);
    let missing = set(uuid::Uuid::new_v4().to_string(), json!("WI-1")).await;
    assert_eq!(missing.status(), reqwest::StatusCode::NOT_FOUND);

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
    assert_eq!(row(&ids[0])["work_item"], "WI-7", "trimmed at start");
    assert_eq!(row(&ids[1])["work_item"], "WI-8");

    // History knows it while the session runs; an unbind clears it there too.
    let cleared: Value = set(ids[0].clone(), Value::Null).await.json().await.unwrap();
    assert!(
        cleared.get("work_item").is_none(),
        "unbound carries no label"
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let first: Value = client
            .get(format!("{base}/api/history/{}", ids[0]))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let second: Value = client
            .get(format!("{base}/api/history/{}", ids[1]))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if second["work_item"] == "WI-8" && first.get("work_item").is_none() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "history never showed the work items: {first} {second}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // The record keeps it through a restart.
    state.sessions.hibernate_for_shutdown().await;
    drop(guard);
    let (base, _h) = boot_with_config(cfg).await;
    let listed: Value = client
        .get(format!("{base}/api/sessions"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let listed: Vec<Value> = serde_json::from_value(listed["sessions"].clone()).unwrap();
    let kept = listed.iter().find(|s| s["id"] == ids[1].as_str()).unwrap();
    assert_eq!(kept["activity"], "hibernated");
    assert_eq!(kept["work_item"], "WI-8", "the label survives the restart");
}

/// A graceful shutdown keeps an oversight shell too, with its screen, rather
/// than leaving it to the history drain (WI-962). It is not woken at boot.
#[tokio::test]
async fn a_shutdown_hibernates_an_oversight_shell_and_boot_leaves_it_hibernated() {
    let (_tmp, cfg, _stub) = conversation_sandbox();
    let (base, state, guard) = boot_with_state(cfg.clone()).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let created: Value = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "overseer shell",
            "role": "oversight",
            "command": ["/bin/bash", "-c", "echo overseeing; sleep 300"],
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap().to_string();
    live_output_containing(&client, &base, &id, &["overseeing"]).await;
    state.sessions.hibernate_for_shutdown().await;
    drop(guard);

    let (base, _h) = boot_with_config(cfg).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let row = session_row(&client, &base, &id).await;
    assert_eq!(row["activity"], "hibernated", "{row}");
    assert_eq!(row["hibernation"]["trigger"], "shutdown");
    assert_eq!(row["hibernation"]["resumable"], false);
    assert_eq!(row["role"], "oversight");
}

#[tokio::test]
async fn a_wake_takes_a_slot_and_is_refused_when_the_account_is_full() {
    let (_tmp, cfg, stub) = one_slot_sandbox();
    let (base, _h) = boot_with_config(cfg).await;
    let client = cap_client();
    let first: Value = start_on_account(&client, &base, &stub, "first")
        .await
        .json()
        .await
        .unwrap();
    let first = first["id"].as_str().unwrap().to_string();
    live_output_containing(&client, &base, &first, &["> "]).await;
    client
        .post(format!("{base}/api/sessions/{first}/hibernate"))
        .send()
        .await
        .unwrap();
    // Hibernating freed the slot; a second session takes it.
    let second = start_when_free(&client, &base, &stub, "second").await;
    // The wake needs a slot of its own and the account has none.
    let refused = client
        .post(format!("{base}/api/sessions/{first}/wake"))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), StatusCode::TOO_MANY_REQUESTS);
    // Once the second is gone the wake gets the slot, and holds it.
    client
        .post(format!(
            "{base}/api/sessions/{}/kill",
            second["id"].as_str().unwrap()
        ))
        .send()
        .await
        .unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let woken = client
            .post(format!("{base}/api/sessions/{first}/wake"))
            .send()
            .await
            .unwrap();
        if woken.status() == StatusCode::OK {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "wake never ran");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let full = start_on_account(&client, &base, &stub, "third").await;
    assert_eq!(full.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn a_failed_start_gives_its_slot_back() {
    let (tmp, cfg, stub) = one_slot_sandbox();
    let (base, _h) = boot_with_config(cfg).await;
    let client = cap_client();
    let missing = tmp.path().join("nowhere").join("claude");
    let failed = start_on_account(&client, &base, &missing, "broken").await;
    assert!(!failed.status().is_success(), "{}", failed.status());
    assert_ne!(failed.status(), StatusCode::TOO_MANY_REQUESTS);
    // The slot the failed start reserved is free for the next one.
    let ok = start_on_account(&client, &base, &stub, "fine").await;
    assert_eq!(ok.status(), StatusCode::OK);
}

#[tokio::test]
async fn a_recovered_session_holds_no_slot_until_it_is_woken() {
    let (_tmp, cfg, stub) = one_slot_sandbox();
    let lost = leave_live_record(&cfg.state_dir, &stub);
    let (base, _h) = boot_with_config(cfg).await;
    let client = cap_client();
    // The restart left the recovered session hibernated, so the slot is free.
    let fits = start_on_account(&client, &base, &stub, "fits").await;
    assert_eq!(fits.status(), StatusCode::OK);
    // The new session now holds the only slot, so the wake is refused (WI-4423).
    let woken = client
        .post(format!("{base}/api/sessions/{lost}/wake"))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(woken.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn removing_a_recovered_session_leaves_the_account_within_its_cap() {
    let (_tmp, cfg, stub) = one_slot_sandbox();
    let lost = leave_live_record(&cfg.state_dir, &stub);
    let (base, _h) = boot_with_config(cfg).await;
    let client = cap_client();
    let removed = client
        .delete(format!("{base}/api/sessions/{lost}"))
        .send()
        .await
        .unwrap();
    assert_eq!(removed.status(), StatusCode::OK);
    let ok = start_on_account(&client, &base, &stub, "fits").await;
    assert_eq!(ok.status(), StatusCode::OK);
    let full = start_on_account(&client, &base, &stub, "blocked").await;
    assert_eq!(full.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn stopping_a_recovered_session_leaves_the_account_within_its_cap() {
    let (_tmp, cfg, stub) = one_slot_sandbox();
    let lost = leave_live_record(&cfg.state_dir, &stub);
    let (base, state, _h) = boot_with_state(cfg).await;
    let client = cap_client();
    // Stopping through the registry, as an agent's stop request does.
    state.sessions.stop(lost, Default::default()).unwrap();
    let ok = start_on_account(&client, &base, &stub, "fits").await;
    assert_eq!(ok.status(), StatusCode::OK);
    let full = start_on_account(&client, &base, &stub, "blocked").await;
    assert_eq!(full.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn killing_a_recovered_session_through_the_endpoint_leaves_the_account_within_its_cap() {
    let (_tmp, cfg, stub) = one_slot_sandbox();
    let lost = leave_live_record(&cfg.state_dir, &stub);
    let (base, _h) = boot_with_config(cfg).await;
    let client = cap_client();
    let stopped = client
        .post(format!("{base}/api/sessions/{lost}/kill"))
        .send()
        .await
        .unwrap();
    assert_eq!(stopped.status(), StatusCode::OK);
    let ok = start_on_account(&client, &base, &stub, "fits").await;
    assert_eq!(ok.status(), StatusCode::OK);
    let full = start_on_account(&client, &base, &stub, "blocked").await;
    assert_eq!(full.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn stopping_a_live_session_frees_its_slot_by_registry_and_by_endpoint() {
    let (_tmp, cfg, stub) = one_slot_sandbox();
    let (base, state, _h) = boot_with_state(cfg).await;
    let client = cap_client();
    for (round, by_endpoint) in [("registry", false), ("endpoint", true)] {
        let started: Value = start_on_account(&client, &base, &stub, round)
            .await
            .json()
            .await
            .unwrap();
        let id = started["id"].as_str().unwrap().to_string();
        live_output_containing(&client, &base, &id, &["> "]).await;
        // Running `sleep 300`, it holds the only slot.
        let full = start_on_account(&client, &base, &stub, "blocked").await;
        assert_eq!(full.status(), StatusCode::TOO_MANY_REQUESTS, "{round}");
        let pid = state
            .sessions
            .get(id.parse().unwrap())
            .unwrap()
            .pid()
            .expect("a live session has a pid") as i32;
        if by_endpoint {
            let stopped = client
                .post(format!("{base}/api/sessions/{id}/kill"))
                .send()
                .await
                .unwrap();
            assert_eq!(stopped.status(), StatusCode::OK);
        } else {
            state
                .sessions
                .stop(id.parse().unwrap(), Default::default())
                .unwrap();
        }
        // The stop frees the slot at once, not when the exit is noticed.
        let ok = start_on_account(&client, &base, &stub, "next").await;
        assert_eq!(ok.status(), StatusCode::OK, "{round}");
        let next: Value = ok.json().await.unwrap();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while pid_alive(pid) {
            assert!(
                tokio::time::Instant::now() < deadline,
                "{round}: still alive"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        kill_session(&client, &base, next["id"].as_str().unwrap()).await;
        // Let the last exit return its slot before the next round.
        start_when_free(&client, &base, &stub, "drain").await;
        let drained = state.sessions.live_sessions();
        for s in drained {
            let _ = s.kill();
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// WI-926: a session started with a posture gets it, keeps it across a wake,
/// shows it on its summary, and every Claude launch carries the policy file.
#[tokio::test]
async fn a_permission_posture_reaches_the_agent_and_survives_a_wake() {
    let (tmp, mut cfg, stub) = hibernation_sandbox();
    let policy = tmp.path().join("policy.json");
    std::fs::write(&policy, br#"{"autoMode":{"allow":["$defaults"]}}"#).unwrap();
    cfg.agent_onboarding.settings_file = Some(policy.clone());
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let created: Value = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "trusted task",
            "command": [stub.to_string_lossy()],
            "permission_mode": "bypass",
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap().to_string();
    assert_eq!(created["permission_mode"], "bypass");
    let printed = live_output_containing(&client, &base, &id, &["helper=["]).await;
    assert!(
        printed.contains("arg=[--dangerously-skip-permissions]"),
        "{printed:?}"
    );
    assert!(
        printed.contains(&format!("arg=[--settings={}]", policy.display())),
        "{printed:?}"
    );

    client
        .post(format!("{base}/api/sessions/{id}/hibernate"))
        .send()
        .await
        .unwrap();
    // The record keeps the settings file from the first start. A wake reads
    // that, not whatever the deployment's config says now (WI-1250).
    let record_path = tmp
        .path()
        .join("state")
        .join("sessions")
        .join(format!("{id}.json"));
    let stored = std::fs::read_to_string(&record_path).unwrap();
    assert!(
        stored.contains(&format!("\"{}\"", policy.display())),
        "the original policy is what the record keeps: {stored}"
    );
    client
        .post(format!("{base}/api/sessions/{id}/wake"))
        .send()
        .await
        .unwrap();
    let printed =
        live_output_containing(&client, &base, &id, &["arg=[--resume]", "helper=["]).await;
    let woken = &printed[printed.rfind("arg=[--resume]").unwrap()..];
    assert!(
        woken.contains("--dangerously-skip-permissions"),
        "kept on wake: {woken:?}"
    );
    assert!(
        woken.contains(&format!("arg=[--settings={}]", policy.display())),
        "the settings file recorded at start: {woken:?}"
    );

    let refused = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({ "name": "shell", "permission_mode": "bypass" }))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
}

/// WI-1250: hibernate then wake keeps the posture the session was started
/// with, even when a later boot of the same state points the deployment at a
/// different settings file. Covers an oversight and a worker session, a model,
/// the brief's extra directory, and an oversight bypass that must neither
/// escalate nor drop back to the default. A record written before the posture
/// fields existed still wakes.
#[tokio::test]
async fn a_woken_session_keeps_the_posture_it_was_started_with() {
    let (tmp, mut cfg, claude) = hibernation_sandbox();
    let policy = tmp.path().join("policy.json");
    let replacement = tmp.path().join("replacement.json");
    std::fs::write(&policy, br#"{"autoMode":{"allow":["$defaults"]}}"#).unwrap();
    std::fs::write(&replacement, br#"{"autoMode":{"allow":["$defaults"]}}"#).unwrap();
    cfg.agent_onboarding.settings_file = Some(policy.clone());
    let state_dir = cfg.state_dir.clone();
    let (base, _h) = boot_with_config(cfg.clone()).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let start = |name: &str, command: &std::path::Path, role: &str| {
        let client = client.clone();
        let base = base.clone();
        let command = command.to_path_buf();
        let name = name.to_string();
        let role = role.to_string();
        async move {
            let created: Value = client
                .post(format!("{base}/api/sessions"))
                .json(&json!({
                    "name": name,
                    "command": [command.to_string_lossy()],
                    "permission_mode": "bypass",
                    "model": "test-model",
                    "role": role,
                    "prompt": "## Task\n\nKeep the posture.\n",
                }))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            created["id"].as_str().unwrap().to_string()
        }
    };
    let claude_id = start("claude oversight", &claude, "oversight").await;
    let worker_id = start("claude worker", &claude, "worker").await;

    for id in [&claude_id, &worker_id] {
        let printed = live_output_containing(&client, &base, id, &["helper=["]).await;
        assert!(
            printed.contains("arg=[--dangerously-skip-permissions]"),
            "{printed:?}"
        );
        assert!(
            printed.contains(&format!("arg=[--settings={}]", policy.display())),
            "{printed:?}"
        );
        assert!(
            printed.contains("arg=[--model]\r\narg=[test-model]"),
            "{printed:?}"
        );
        assert!(
            printed.contains("arg=[--add-dir="),
            "the brief's directory is on the first launch: {printed:?}"
        );
        client
            .post(format!("{base}/api/sessions/{id}/hibernate"))
            .send()
            .await
            .unwrap();
    }
    drop(_h);

    // A later boot of the same records, with the deployment now pointing at a
    // different policy. The woken argv must still be the original one.
    let mut later = cfg;
    later.agent_onboarding.settings_file = Some(replacement.clone());
    let (base, _h) = boot_with_config(later.clone()).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let brief_dir = state_dir.join("agent-task-prompts").join("sessions");
    for (id, role) in [(&claude_id, "oversight"), (&worker_id, "worker")] {
        let stored =
            std::fs::read_to_string(state_dir.join("sessions").join(format!("{id}.json"))).unwrap();
        // Only what the first launch carried is recorded (WI-2252).
        assert!(
            stored.contains(&format!("\"{}\"", policy.display())),
            "the record keeps the original policy it launched with: {stored}"
        );
        assert!(
            !stored.contains(&format!("\"{}\"", replacement.display())),
            "the replacement never lands on the record: {stored}"
        );
        let woken = client
            .post(format!("{base}/api/sessions/{id}/wake"))
            .send()
            .await
            .unwrap();
        assert_eq!(woken.status(), StatusCode::OK);
        let woken: Value = woken.json().await.unwrap();
        assert_eq!(woken["permission_mode"], "bypass", "{woken}");
        // A worker's role is omitted from the summary (`is_worker`); only an
        // oversight role is present, and a wake must not drop it.
        if role == "oversight" {
            assert_eq!(woken["role"], role, "{woken}");
        } else {
            assert!(woken["role"].is_null(), "{woken}");
        }
        let printed =
            live_output_containing(&client, &base, id, &["arg=[--resume]", "helper=["]).await;
        // `--model` is written before `--resume` (agent_cli::launch), so it is
        // not in the tail that starts at the resume flag. The posture flags
        // that follow `--resume` are what a wake must not quietly change.
        assert!(
            printed.contains("arg=[--model]\r\narg=[test-model]"),
            "the recorded model is re-applied on the wake: {printed:?}"
        );
        let tail = &printed[printed.rfind("arg=[--resume]").unwrap()..];
        assert!(
            tail.contains("--dangerously-skip-permissions"),
            "bypass is neither dropped nor widened: {tail:?}"
        );
        assert!(
            tail.contains(&format!("arg=[--settings={}]", policy.display())),
            "the recorded policy, not the deployment's current one: {tail:?}"
        );
        assert!(
            !tail.contains(&format!("arg=[--settings={}]", replacement.display())),
            "the replacement policy must not be applied: {tail:?}"
        );
        assert!(
            tail.contains(&format!("arg=[--add-dir={}]", brief_dir.display())),
            "the recorded directory, even though a wake has no new brief: {tail:?}"
        );
    }

    // A record from before the posture fields existed still wakes, and adds
    // no directory of its own. Both sessions are hibernated first — the second
    // engine is still running them — and the fields are stripped from the
    // record before the next boot. A wake of a record that still has them
    // would replay them, and hibernating afterwards would write those back.
    client
        .post(format!("{base}/api/sessions/{claude_id}/hibernate"))
        .send()
        .await
        .unwrap();
    client
        .post(format!("{base}/api/sessions/{worker_id}/hibernate"))
        .send()
        .await
        .unwrap();
    let legacy_id = claude_id.clone();
    drop(_h);
    let path = state_dir.join("sessions").join(format!("{legacy_id}.json"));
    let mut record: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    record.as_object_mut().unwrap().remove("settings_file");
    record.as_object_mut().unwrap().remove("add_dirs");
    std::fs::write(&path, serde_json::to_vec_pretty(&record).unwrap()).unwrap();
    let mut legacy_cfg = later;
    legacy_cfg.agent_onboarding.settings_file = Some(replacement.clone());
    let (base, _h) = boot_with_config(legacy_cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let woken = client
        .post(format!("{base}/api/sessions/{legacy_id}/wake"))
        .send()
        .await
        .unwrap();
    assert_eq!(woken.status(), StatusCode::OK, "{woken:?}");
    let printed =
        live_output_containing(&client, &base, &legacy_id, &["arg=[--resume]", "helper=["]).await;
    let tail = &printed[printed.rfind("arg=[--resume]").unwrap()..];
    // An oversight session with no recorded settings is not put on the
    // deployment's worker policy (WI-2252); a worker's would fall back to it.
    assert!(
        !tail.contains("arg=[--settings="),
        "an old oversight record gets no worker policy: {tail:?}"
    );
    assert!(
        !tail.contains("arg=[--add-dir="),
        "an old record adds no directory: {tail:?}"
    );
    assert!(
        tail.contains("--dangerously-skip-permissions"),
        "the recorded bypass still holds: {tail:?}"
    );
}

/// WI-2252: a prod restart woke both oversight sessions under the worker
/// driven policy, without the bypass a person had given them after the start.
/// A wake replays only the posture a person gave at the start: a bypass
/// granted then comes back; one the session merely showed on its screen (a
/// Shift+Tab, a hand-typed flag, or an agent printing the words) does not, and
/// the summary says `posture_lost_on_resume` instead. An overseer that started
/// as a plain shell is not handed the worker policy; a worker keeps the
/// default posture and the policy.
#[tokio::test]
async fn a_wake_replays_only_a_persons_posture_and_says_a_lost_bypass() {
    let (tmp, mut cfg, stub) = conversation_sandbox();
    let policy = tmp.path().join("policy.json");
    std::fs::write(&policy, br#"{"autoMode":{"allow":["$defaults"]}}"#).unwrap();
    cfg.agent_onboarding.settings_file = Some(policy.clone());
    // A `claude` that draws Claude Code's bypass footer when told to: what an
    // agent printing those words looks like to the engine.
    let forged_dir = tmp.path().join("forged");
    std::fs::create_dir_all(&forged_dir).unwrap();
    let forged = forged_dir.join("claude");
    std::fs::write(
        &forged,
        "#!/bin/sh\nfor a in \"$@\"; do printf 'arg=[%s]\\n' \"$a\"; done\n\
         printf '%s %s on\\n' bypass permissions\n\
         sleep 300 &\nprintf 'helper=[%s]\\n' \"$!\"\nwait\n",
    )
    .unwrap();
    std::fs::set_permissions(&forged, std::fs::Permissions::from_mode(0o755)).unwrap();
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let create = |body: Value| {
        let client = client.clone();
        let base = base.clone();
        async move {
            let created: Value = client
                .post(format!("{base}/api/sessions"))
                .json(&body)
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            created["id"].as_str().unwrap().to_string()
        }
    };
    let cycle = |id: String| {
        let client = client.clone();
        let base = base.clone();
        async move {
            let slept = client
                .post(format!("{base}/api/sessions/{id}/hibernate"))
                .send()
                .await
                .unwrap();
            assert_eq!(slept.status(), StatusCode::OK, "{:?}", slept.text().await);
            let woken = client
                .post(format!("{base}/api/sessions/{id}/wake"))
                .send()
                .await
                .unwrap();
            assert_eq!(woken.status(), StatusCode::OK);
            let woken: Value = woken.json().await.unwrap();
            let printed =
                live_output_containing(&client, &base, &id, &["arg=[--resume]", "helper=["]).await;
            let tail = printed[printed.rfind("arg=[--resume]").unwrap()..].to_string();
            (woken, tail)
        }
    };
    let settings = format!("arg=[--settings={}]", policy.display());

    // A person started this overseer with bypass: it comes back with it, and
    // with the policy its first launch carried. Nothing was lost.
    let granted = create(json!({
        "name": "granted overseer",
        "command": [stub.to_string_lossy()],
        "role": "oversight",
        "permission_mode": "bypass",
    }))
    .await;
    live_output_containing(&client, &base, &granted, &["helper=["]).await;
    let (woken, tail) = cycle(granted).await;
    assert_eq!(woken["permission_mode"], "bypass", "{woken}");
    assert!(woken.get("posture_lost_on_resume").is_none(), "{woken}");
    assert!(
        tail.contains("arg=[--dangerously-skip-permissions]"),
        "{tail:?}"
    );
    assert!(tail.contains(&settings), "{tail:?}");

    // A worker in the default posture whose agent prints the bypass footer:
    // the wake keeps the default and the policy, and only says it saw one.
    let worker = create(json!({
        "name": "worker",
        "command": [forged.to_string_lossy()],
    }))
    .await;
    live_output_containing(
        &client,
        &base,
        &worker,
        &["bypass permissions on", "helper=["],
    )
    .await;
    let (woken, tail) = cycle(worker).await;
    assert!(woken["permission_mode"].is_null(), "{woken}");
    assert_eq!(woken["posture_lost_on_resume"], true, "{woken}");
    assert!(
        !tail.contains("--dangerously-skip-permissions"),
        "an agent cannot escalate through a wake: {tail:?}"
    );
    assert!(tail.contains(&settings), "{tail:?}");

    // The incident: an overseer that began as a plain shell, a person typing
    // `claude` into it and granting bypass there. The wake resumes the
    // conversation through the template, without the worker policy, and says
    // the bypass is gone rather than replaying a grant it cannot verify.
    let typed = create(json!({ "name": "typed overseer", "role": "oversight" })).await;
    let conversation = "6c1f0d2e-5b7a-4e1c-9f3d-2a8b7c6d5e4f";
    client
        .post(format!("{base}/api/sessions/{typed}/conversation"))
        .json(&json!({ "agent": "claude", "id": conversation }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    client
        .post(format!("{base}/api/sessions/{typed}/input"))
        .json(&json!({ "text": "printf '%s %s on\\n' bypass permissions", "submit": true }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    live_output_containing(&client, &base, &typed, &["bypass permissions on"]).await;
    let (woken, tail) = cycle(typed.clone()).await;
    assert!(woken["permission_mode"].is_null(), "{woken}");
    assert_eq!(woken["posture_lost_on_resume"], true, "{woken}");
    assert!(
        tail.contains(&format!("arg=[{conversation}]")),
        "the conversation is resumed: {tail:?}"
    );
    assert!(!tail.contains("--dangerously-skip-permissions"), "{tail:?}");
    assert!(
        !tail.contains("arg=[--settings="),
        "an overseer that started without the worker policy is not given it: {tail:?}"
    );
    // The loss is still said after another hibernation, until a person
    // grants the bypass again: the woken screen no longer shows it.
    let (woken, _) = cycle(typed).await;
    assert_eq!(woken["posture_lost_on_resume"], true, "{woken}");
}

/// A session started with no `cwd` runs in the engine's default directory,
/// which on the pods is `~`, outside the workspace root. Hibernating it
/// recorded that directory and waking it replayed it as a requested `cwd`,
/// which was refused ("path escapes workspace_root"): every GUI or template
/// session hibernated for good. Found validating WI-912 on vogt-dev.
#[tokio::test]
async fn a_session_in_the_default_directory_outside_the_workspace_wakes() {
    let (tmp, mut cfg, stub) = hibernation_sandbox();
    let home = tmp.path().join("home");
    let workspace = tmp.path().join("ws");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    cfg.default_cwd = home.clone();
    cfg.workspace_root = workspace.canonicalize().unwrap();
    let (base, _state, _h) = boot_with_state(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let created: Value = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({ "name": "home", "command": [stub.to_string_lossy()] }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap().to_string();
    assert_eq!(created["cwd"], home.to_string_lossy().as_ref());
    live_output_containing(&client, &base, &id, &["helper=["]).await;
    let hibernated = client
        .post(format!("{base}/api/sessions/{id}/hibernate"))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        hibernated.status(),
        StatusCode::OK,
        "{:?}",
        hibernated.text().await
    );
    let woken = client
        .post(format!("{base}/api/sessions/{id}/wake"))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    let status = woken.status();
    let body: Value = woken.json().await.unwrap();
    assert_eq!(status, StatusCode::OK, "{body}");
    let row = wait_for_session_row(&client, &base, &id, |s| s["alive"] == json!(true)).await;
    assert_eq!(row["cwd"], home.to_string_lossy().as_ref(), "{row:?}");
}

/// WI-1251: a hibernated session whose work was handed to a successor is not
/// woken by a plain wake, and neither is one whose work item is already being
/// worked on by a live session in the same directory. `force` overrides both.
/// Two sessions in the same directory on different work items do not contend.
#[tokio::test]
async fn a_superseded_session_and_a_contended_work_item_are_not_woken() {
    let (tmp, cfg, stub) = hibernation_sandbox();
    let (base, state, _h) = boot_with_state(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let stub = stub.to_string_lossy().to_string();
    let start = |name: &str| {
        let (client, base, stub) = (client.clone(), base.clone(), stub.clone());
        let name = name.to_string();
        async move {
            let created: Value = client
                .post(format!("{base}/api/sessions"))
                .json(&json!({ "name": name, "command": [stub] }))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            created["id"].as_str().unwrap().to_string()
        }
    };
    let old = start("old").await;
    let successor = start("successor").await;
    live_output_containing(&client, &base, &old, &["helper=["]).await;

    let unknown = client
        .post(format!("{base}/api/sessions/{old}/hibernate"))
        .json(&json!({ "superseded_by": "not-a-session" }))
        .send()
        .await
        .unwrap();
    assert_eq!(unknown.status(), StatusCode::BAD_REQUEST);
    let self_named = client
        .post(format!("{base}/api/sessions/{old}/hibernate"))
        .json(&json!({ "superseded_by": old }))
        .send()
        .await
        .unwrap();
    assert_eq!(self_named.status(), StatusCode::BAD_REQUEST);

    let hibernated = client
        .post(format!("{base}/api/sessions/{old}/hibernate"))
        .json(&json!({ "superseded_by": successor }))
        .send()
        .await
        .unwrap();
    assert_eq!(hibernated.status(), StatusCode::OK);
    let row: Value = hibernated.json().await.unwrap();
    assert_eq!(row["hibernation"]["superseded_by"], successor);

    let refused = client
        .post(format!("{base}/api/sessions/{old}/wake"))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), StatusCode::CONFLICT);
    let why = refused.text().await.unwrap();
    assert!(why.contains(&format!("superseded by {successor}")), "{why}");

    let forced = client
        .post(format!("{base}/api/sessions/{old}/wake"))
        .json(&json!({ "force": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        forced.status(),
        StatusCode::OK,
        "{}",
        forced.text().await.unwrap_or_default()
    );
    // The wake returns before the process is up; hibernating before it is
    // alive is refused, so wait for output the way the first start did.
    live_output_containing(&client, &base, &old, &["helper=["]).await;
    let _ = client
        .post(format!("{base}/api/sessions/{old}/hibernate"))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    let list: Value = client
        .get(format!("{base}/api/sessions"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let again = list["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == old)
        .unwrap()
        .clone();
    assert_eq!(
        again["hibernation"]["superseded_by"], successor,
        "a later hibernation keeps the handoff: {again}"
    );

    // Declaring the handoff after the fact records it without a restart.
    let late = start("late").await;
    let _ = client
        .post(format!("{base}/api/sessions/{late}/hibernate"))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    let marked: Value = client
        .post(format!("{base}/api/sessions/{late}/hibernate"))
        .json(&json!({ "superseded_by": successor }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(marked["alive"], false);
    assert_eq!(marked["hibernation"]["superseded_by"], successor);

    // A successor that no longer exists does not block the wake forever.
    let orphan = start("orphan").await;
    live_output_containing(&client, &base, &orphan, &["helper=["]).await;
    let gone = client
        .post(format!("{base}/api/sessions/{orphan}/hibernate"))
        .json(&json!({ "superseded_by": successor }))
        .send()
        .await
        .unwrap();
    assert_eq!(gone.status(), StatusCode::OK);
    let removed = client
        .delete(format!("{base}/api/sessions/{successor}"))
        .send()
        .await
        .unwrap();
    assert_eq!(removed.status(), StatusCode::OK);
    let freed = client
        .post(format!("{base}/api/sessions/{orphan}/wake"))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        freed.status(),
        StatusCode::OK,
        "{}",
        freed.text().await.unwrap_or_default()
    );

    // Same directory, same work item: the live one blocks the wake.
    let bind = |id: &str, work_item: &str| {
        let (client, base) = (client.clone(), base.clone());
        let (id, work_item) = (id.to_string(), work_item.to_string());
        async move {
            client
                .post(format!("{base}/api/sessions/{id}/work-item"))
                .json(&json!({ "work_item": work_item }))
                .send()
                .await
                .unwrap()
        }
    };
    let first = start("first").await;
    let second = start("second").await;
    bind(&first, "WI-1251").await;
    bind(&second, "WI-1251").await;
    live_output_containing(&client, &base, &second, &["helper=["]).await;
    let _ = client
        .post(format!("{base}/api/sessions/{second}/hibernate"))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    let contended = client
        .post(format!("{base}/api/sessions/{second}/wake"))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(contended.status(), StatusCode::CONFLICT);
    let why = contended.text().await.unwrap();
    assert!(why.contains(&first) && why.contains("WI-1251"), "{why}");

    // A different work item in the same directory is not a contention.
    bind(&second, "WI-other").await;
    let woken = client
        .post(format!("{base}/api/sessions/{second}/wake"))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(woken.status(), StatusCode::OK, "{:?}", woken.text().await);

    // Boot wake skips a manually hibernated pinned session, a superseded one,
    // and one the budget watcher hibernated, and still wakes one the policy
    // hibernated.
    let manual = start("manual").await;
    let policy = start("policy").await;
    let handed = start("handed").await;
    let budgeted = start("budgeted").await;
    for id in [&manual, &policy, &handed, &budgeted] {
        let pinned = client
            .post(format!("{base}/api/sessions/{id}/keep-awake"))
            .json(&json!({ "keep_awake": true }))
            .send()
            .await
            .unwrap();
        assert_eq!(pinned.status(), StatusCode::OK);
    }
    let _ = client
        .post(format!("{base}/api/sessions/{manual}/hibernate"))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    let _ = client
        .post(format!("{base}/api/sessions/{handed}/hibernate"))
        .json(&json!({ "superseded_by": successor }))
        .send()
        .await
        .unwrap();
    state
        .sessions
        .hibernate(
            uuid::Uuid::parse_str(&policy).unwrap(),
            Some("idle".into()),
            vogt_engine_contract::HibernateTrigger::Idle,
            false,
            false,
            None,
        )
        .await
        .unwrap();
    state
        .sessions
        .hibernate(
            uuid::Uuid::parse_str(&budgeted).unwrap(),
            Some("budget".into()),
            vogt_engine_contract::HibernateTrigger::Budget,
            false,
            false,
            None,
        )
        .await
        .unwrap();
    let due = state.sessions.hibernated_keep_awake();
    assert!(
        due.contains(&uuid::Uuid::parse_str(&policy).unwrap()),
        "{due:?}"
    );
    assert!(
        !due.contains(&uuid::Uuid::parse_str(&manual).unwrap()),
        "{due:?}"
    );
    assert!(
        !due.contains(&uuid::Uuid::parse_str(&handed).unwrap()),
        "{due:?}"
    );
    assert!(
        !due.contains(&uuid::Uuid::parse_str(&budgeted).unwrap()),
        "{due:?}"
    );
    drop(tmp);
}

#[tokio::test]
async fn a_worktree_session_opens_in_its_worktree_and_wakes_back_into_it() {
    let (tmp, cfg, stub) = hibernation_sandbox();
    let repo = tmp.path().join("repo");
    worktree_checkout(&repo);
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let created = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "worktree agent",
            "command": [stub.to_string_lossy()],
            "cwd": repo.to_string_lossy(),
            "worktree": { "branch": "wi-1-worker", "base_ref": "main" },
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::OK);
    let created: Value = created.json().await.unwrap();
    let id = created["id"].as_str().unwrap().to_string();
    let worktree = tmp.path().canonicalize().unwrap().join("wi-1-worker");
    assert_eq!(created["cwd"], worktree.to_string_lossy().as_ref());
    assert_eq!(
        std::fs::read_to_string(worktree.join("f.txt")).unwrap(),
        "remote\n"
    );
    assert_eq!(
        worktree_git(&worktree, &["rev-parse", "--abbrev-ref", "HEAD"]).trim(),
        "wi-1-worker"
    );
    // The checkout it was cut from is untouched.
    assert_eq!(
        std::fs::read_to_string(repo.join("f.txt")).unwrap(),
        "dirty\n"
    );
    live_output_containing(&client, &base, &id, &["> "]).await;

    let hibernated = client
        .post(format!("{base}/api/sessions/{id}/hibernate"))
        .json(&json!({ "reason": "worktree wake test" }))
        .send()
        .await
        .unwrap();
    assert_eq!(hibernated.status(), StatusCode::OK);
    let woken = client
        .post(format!("{base}/api/sessions/{id}/wake"))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(woken.status(), StatusCode::OK);
    let woken: Value = woken.json().await.unwrap();
    // Back in the same worktree; no second one was cut.
    assert_eq!(woken["cwd"], worktree.to_string_lossy().as_ref());
    let listed = worktree_git(&repo, &["worktree", "list", "--porcelain"]);
    assert_eq!(listed.matches("worktree ").count(), 2, "{listed}");
    let _ = client
        .delete(format!("{base}/api/sessions/{id}"))
        .send()
        .await;
}

/// WI-1281: a Claude autopilot session is hibernated with trigger `budget`
/// at its turn limit, at a turn boundary, and not many turns later. Before,
/// autopilot re-prompted it each time it reached its prompt, and the watcher,
/// which waits for the prompt, rarely saw it there.
#[tokio::test]
async fn a_claude_autopilot_session_hibernates_at_its_turn_budget() {
    const LIMIT: usize = 3;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let stub = fake_claude_tui(dir);
    let (base, _h) = boot_with_config(budget_sandbox(dir)).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let created: Value = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "claude on a budget",
            "command": [stub.to_string_lossy()],
            "autopilot": true,
            "budget": { "turns": LIMIT, "wall_clock_minutes": 90 },
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap().to_string();
    assert_eq!(created["conversation"]["agent"], "claude", "{created}");
    let conversation = created["conversation"]["id"].as_str().unwrap();
    // The whole-session clock is not set unless asked for.
    assert!(created["budget"]
        .get("session_wall_clock_minutes")
        .is_none());
    std::fs::write(
        dir.join("transcript_at"),
        dir.join(format!(".claude/projects/k/{conversation}.jsonl"))
            .display()
            .to_string(),
    )
    .unwrap();

    let row = wait_hibernated(&client, &base, &id, 60).await;
    assert_eq!(row["hibernation"]["trigger"], "budget", "{row}");
    let reason = row["hibernation"]["reason"].as_str().unwrap_or_default();
    assert!(reason.contains("budget: turns"), "{reason}");
    let turns = turns_run(dir);
    // The watcher may first see the limit while the next turn is already
    // under way; it holds autopilot off and stops at that turn's end.
    assert!(
        (LIMIT..=LIMIT + 1).contains(&turns),
        "stopped after {turns} turns against a limit of {LIMIT}"
    );
    // Hibernated at the prompt, never mid-turn: every turn that began ended.
    let transcript =
        std::fs::read_to_string(dir.join(format!(".claude/projects/k/{conversation}.jsonl")))
            .unwrap();
    assert_eq!(transcript.lines().count(), turns);
}

/// WI-1522: an overseer over its context limit is never hibernated for it; the
/// engine types its compact command instead. The same limit on a worker
/// hibernates (the test above).
#[tokio::test]
async fn an_overseer_over_its_context_limit_is_not_hibernated() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let stub = fake_claude_tui(dir);
    let (base, _h) = boot_with_config(budget_sandbox(dir)).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let created: Value = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "overseer on a context budget",
            "command": [stub.to_string_lossy()],
            "autopilot": true,
            "role": "oversight",
            "budget": { "context_tokens": 1 },
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap().to_string();
    assert_eq!(created["role"], "oversight", "{created}");
    let conversation = created["conversation"]["id"].as_str().unwrap();
    std::fs::write(
        dir.join("transcript_at"),
        dir.join(format!(".claude/projects/k/{conversation}.jsonl"))
            .display()
            .to_string(),
    )
    .unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while turns_run(dir) < 3 {
        assert!(tokio::time::Instant::now() < deadline, "no turns ran");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    // Several watcher ticks past the limit.
    tokio::time::sleep(Duration::from_secs(4)).await;
    let row = session_row(&client, &base, &id).await;
    assert!(
        row.get("hibernation").is_none_or(|h| h.is_null()),
        "an overseer was hibernated for context: {row}"
    );
    assert!(row["economics"]["context_tokens"].as_i64().unwrap_or(0) > 1);
}

/// WI-1281: the whole-session wall clock banks running time across turns and
/// across a hibernation. A session hibernated with 55 s already counted
/// against a one-minute session clock is woken, runs a few short turns — none
/// near the per-turn limit — and is hibernated with trigger `budget` at a turn
/// boundary once the total passes the minute. The record it leaves carries
/// the total, so the next wake would resume from there.
#[tokio::test]
async fn a_claude_session_clock_survives_a_wake_and_hibernates_at_its_limit() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let stub = fake_claude_tui(dir);
    let cfg = budget_sandbox(dir);
    let id = uuid::Uuid::new_v4();
    let conversation = uuid::Uuid::new_v4();
    std::fs::write(
        dir.join("transcript_at"),
        dir.join(format!(".claude/projects/k/{conversation}.jsonl"))
            .display()
            .to_string(),
    )
    .unwrap();
    let sessions = cfg.state_dir.join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    std::fs::write(
        sessions.join(format!("{id}.json")),
        serde_json::to_vec(&json!({
            "version": 1,
            "id": id,
            "name": "claude on the clock",
            "created_at": "2026-10-13T00:00:00Z",
            "command": [stub.to_string_lossy()],
            "cwd": dir,
            "conversation": { "agent": "claude", "id": conversation },
            "autopilot": true,
            "budget": { "wall_clock_minutes": 90, "session_wall_clock_minutes": 1 },
            "budget_session_secs": 55,
            "hibernation": {
                "at": "2026-10-13T00:00:00Z",
                "trigger": "manual",
                "resumable": true,
            },
        }))
        .unwrap(),
    )
    .unwrap();
    let state_dir = cfg.state_dir.clone();
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let id = id.to_string();
    client
        .post(format!("{base}/api/sessions/{id}/wake"))
        .json(&json!({}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();

    let row = wait_hibernated(&client, &base, &id, 60).await;
    assert_eq!(row["hibernation"]["trigger"], "budget", "{row}");
    let reason = row["hibernation"]["reason"].as_str().unwrap_or_default();
    assert!(reason.contains("session wall clock"), "{reason}");
    // A few 1.5 s turns: 5 s of running on top of the banked 55 s, plus at
    // most the turn under way when the watcher saw it.
    let turns = turns_run(dir);
    assert!((2..=6).contains(&turns), "ran {turns} turns");
    let record: Value = serde_json::from_slice(
        &std::fs::read(state_dir.join(format!("sessions/{id}.json"))).unwrap(),
    )
    .unwrap();
    let banked = record["budget_session_secs"].as_u64().unwrap();
    assert!((60..120).contains(&banked), "banked {banked} s: {record}");
}

/// WI-1750: by default a hibernation keeps the worktree's build output (the
/// session is woken to carry on, and every engine shutdown hibernates every
/// live session); removing the hibernated session is its stop, and cleans.
#[tokio::test]
async fn hibernation_keeps_build_output_and_removal_cleans_it() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    let git = |dir: &std::path::Path, args: &[&str]| {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    };
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "seed@vogt.invalid"]);
    git(&repo, &["config", "user.name", "seed"]);
    std::fs::write(repo.join("README"), "r").unwrap();
    git(&repo, &["add", "README"]);
    git(&repo, &["commit", "-qm", "init"]);
    let linked = tmp.path().join("linked");
    git(&repo, &["worktree", "add", "-q", linked.to_str().unwrap()]);
    std::fs::write(linked.join("Cargo.toml"), "[package]\nname=\"t\"\n").unwrap();
    let target = linked.join("target");
    std::fs::create_dir(&target).unwrap();
    std::fs::write(target.join("out"), "built").unwrap();

    let mut cfg = test_config();
    cfg.default_cwd = tmp.path().to_path_buf();
    cfg.workspace_root = tmp.path().canonicalize().unwrap();
    cfg.state_dir = tmp.path().join("state");
    cfg.worktree_clean_on_stop = true;
    cfg.worktree_clean_on_hibernate = false;
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let cwd = linked.to_string_lossy().into_owned();
    let id = create_session_with(
        &client,
        &base,
        json!({ "name": "h", "command": ["/bin/cat"], "cwd": cwd }),
    )
    .await;

    let hibernated = client
        .post(format!("{base}/api/sessions/{id}/hibernate"))
        .json(&json!({ "allow_shell": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(hibernated.status(), StatusCode::OK);
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(
        target.join("out").exists(),
        "a hibernated session keeps its build output"
    );

    let removed = client
        .delete(format!("{base}/api/sessions/{id}"))
        .send()
        .await
        .unwrap();
    assert!(removed.status().is_success(), "{}", removed.status());
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    while target.exists() && std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(!target.exists(), "removing the hibernated session cleans");
    assert!(linked.join("Cargo.toml").exists());
}

/// WI-1812: a session launched through the real `agent-auth.sh` while the
/// secrets manager is unreachable still starts, says so in its terminal, and
/// shows in `/api/status`; once the secrets manager answers again, the next
/// session gets its credentials from the same engine, with no restart.
#[tokio::test]
async fn a_launch_with_the_secrets_manager_down_degrades_and_recovers_without_restart() {
    for tool in ["jq", "curl", "timeout"] {
        if std::process::Command::new(tool)
            .arg("--version")
            .output()
            .is_err()
        {
            eprintln!("skipping: the launch uses {tool}");
            return;
        }
    }
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../deploy/agent-auth.sh");
    let tmp = tempfile::tempdir().unwrap();
    let bin = tmp.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let write_exec = |name: &str, body: &str| {
        use std::os::unix::fs::PermissionsExt;
        let path = bin.join(name);
        std::fs::write(&path, body).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    };
    write_exec("vogt-mcp-bootstrap", "#!/bin/sh\nexit 0\n");
    // Down: the login hangs, as it did against an Infisical with no Redis.
    write_exec("infisical", "#!/bin/sh\nexec sleep 30\n");

    // A stand-in secrets-manager API that answers the one bulk read, and the
    // universal-auth login once `login_up` is set. The login is an API call,
    // not a CLI call (WI-3885), so "down" is this server hanging it.
    let api = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let api_url = format!("http://{}", api.local_addr().unwrap());
    let login_up = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let login_state = login_up.clone();
    tokio::spawn(async move {
        let app = axum::Router::new()
            .route(
                "/api/v4/secrets",
                axum::routing::get(|| async {
                    axum::Json(json!({ "secrets": [
                        { "secretKey": "ALPHA", "secretValue": "alpha-value" }
                    ]}))
                }),
            )
            .route(
                "/api/v1/auth/universal-auth/login",
                axum::routing::post(move || {
                    let login_up = login_state.clone();
                    async move {
                        if !login_up.load(std::sync::atomic::Ordering::SeqCst) {
                            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                        }
                        axum::Json(json!({ "accessToken": "tok-123" }))
                    }
                }),
            );
        axum::serve(api, app).await.unwrap()
    });

    let cfg = broker_config(&tmp, "ALPHA proj-a ALPHA\n");
    let (base, _state, _h) = boot_with_state(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let path = format!("{}:/usr/bin:/bin", bin.display());
    let start = |name: &'static str| {
        let client = client.clone();
        let base = base.clone();
        let script = script.display().to_string();
        let (path, api_url, bin) = (path.clone(), api_url.clone(), bin.clone());
        async move {
            client
                .post(format!("{base}/api/sessions"))
                .json(&json!({
                    "name": name,
                    "command": ["/bin/bash", script, "run", "--", "/bin/bash", "-c",
                        "echo \"granted=[$AGENT_AUTH_GRANTED] alpha=[${ALPHA:-}]\"; sleep 30"],
                    "env": [
                        ["VOGT_ENGINE_BROKER_URL", base],
                        ["PATH", path],
                        ["VOGT_MCP_BOOTSTRAP", bin.join("vogt-mcp-bootstrap").display().to_string()],
                        ["ENGINE_AGENT_AUTH_TIMEOUT", "1"],
                        ["ENGINE_AGENT_AUTH_SECRETS", "ALPHA proj-a ALPHA"],
                        ["INFISICAL_API_URL", format!("{api_url}/api")],
                        ["INFISICAL_CLIENT_ID", "id"],
                        ["INFISICAL_CLIENT_SECRET", "secret"],
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
                .to_string()
        }
    };

    let down = start("auth-down").await;
    live_output_containing(
        &client,
        &base,
        &down,
        &["agent auth unavailable", "granted=[] alpha=[]"],
    )
    .await;
    let mut status = Value::Null;
    for _ in 0..50 {
        status = client
            .get(format!("{base}/api/status"))
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap();
        if status["auth_broker"]["degraded_session_count"] == json!(1) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(status["auth_broker"]["degraded_session_count"], json!(1));
    assert_eq!(status["auth_broker"]["degraded"]["session_id"], json!(down));
    assert!(status["auth_broker"]["degraded"]["reason"]
        .as_str()
        .unwrap()
        .contains("timed out"));

    // The secrets manager is back: the next launch gets its credentials.
    login_up.store(true, std::sync::atomic::Ordering::SeqCst);
    let up = start("auth-up").await;
    live_output_containing(
        &client,
        &base,
        &up,
        &["granted=[ALPHA] alpha=[alpha-value]"],
    )
    .await;
}

/// WI-1523: a session done and quiet past `ENGINE_HIBERNATE_DONE_AFTER` is
/// hibernated with trigger `done`; one given input after its declaration,
/// and an overseer, are not.
#[tokio::test]
async fn the_done_policy_hibernates_a_quiet_done_session_only() {
    use vogt_engine_server::hibernate_policy::{exemption, run_once, Policy};

    let (tmp, cfg, _stub) = hibernation_sandbox();
    let (base, state, _h) = boot_with_state(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let quiet = "#!/bin/sh\nsleep 300 &\nprintf 'helper=[%s]\\n> ' \"$!\"\nwait\n";
    let ids = start_stub_agents(
        &client,
        &base,
        &tmp.path().join("agents"),
        &[("done", quiet), ("typed", quiet), ("overseer", quiet)],
    )
    .await;
    let [done_id, typed_id, overseer_id] = [0, 1, 2].map(|i| ids[i].parse::<uuid::Uuid>().unwrap());
    let long_ago = (time::OffsetDateTime::now_utc() - time::Duration::minutes(20))
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap();
    for id in [done_id, typed_id, overseer_id] {
        assert!(state.sessions.mark_done(id, long_ago.clone()));
    }
    state
        .sessions
        .set_role(overseer_id, vogt_engine_contract::SessionRole::Oversight)
        .unwrap();
    // The role pins it awake; unpin it so the role alone is what spares it.
    state
        .sessions
        .get(overseer_id)
        .unwrap()
        .set_keep_awake(false);
    // Input after the declaration: the agent was given more to do.
    state
        .sessions
        .get(typed_id)
        .unwrap()
        .write_input(b"\x1b")
        .unwrap();
    let why = |id: uuid::Uuid| exemption(&state.sessions, &state.sessions.get(id).unwrap());
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while [done_id, typed_id, overseer_id]
        .iter()
        .any(|id| why(*id).is_some())
        && std::time::Instant::now() < deadline
    {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    for id in [done_id, typed_id, overseer_id] {
        assert_eq!(why(id), None, "{id}");
    }

    run_once(
        &state.sessions,
        &Policy {
            done_after: Some(Duration::from_secs(600)),
            ..Policy::default()
        },
    )
    .await;

    let listed: Value = client
        .get(format!("{base}/api/sessions"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let listed: Vec<Value> = serde_json::from_value(listed["sessions"].clone()).unwrap();
    let row = |id: uuid::Uuid| {
        listed
            .iter()
            .find(|s| s["id"] == id.to_string().as_str())
            .unwrap()
            .clone()
    };
    assert_eq!(row(done_id)["activity"], "hibernated");
    assert_eq!(row(done_id)["hibernation"]["trigger"], "done");
    assert_ne!(row(typed_id)["activity"], "hibernated", "input since done");
    assert_ne!(row(overseer_id)["activity"], "hibernated", "an overseer");
}
