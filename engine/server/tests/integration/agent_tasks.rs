use super::*;

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
            "command": ["/bin/sh", "-lc", "printf 'task:%s run:%s\\n' \"$VOGT_ENGINE_AGENT_TASK_ID\" \"$VOGT_ENGINE_AGENT_TASK_RUN_ID\""],
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let task_id = created["id"].as_str().unwrap().to_string();
    assert_eq!(created["name"], "PX3 price monitor");
    assert_eq!(created["notify_on_phrase"], "VOGT_NOTIFY:");

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
    assert!(prompt_text.contains("VOGT_NOTIFY:"));

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

    let sessions: Value = client
        .get(format!("{base}/api/sessions"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let sessions: Vec<Value> = serde_json::from_value(sessions["sessions"].clone()).unwrap();
    assert!(sessions.iter().any(|s| s["id"] == session_id));
}

/// A bound task's subject and its recorded findings, both halves in one run.
///
/// The binding reaches the run — the command prints the two environment
/// variables back, so a run that was told nothing would print a blank line —
/// and the notify phrase becomes a *recorded* finding on the run rather than
/// only a push. The whole point of the requirement is that a finding
/// survives the notification, so the assertion is that it is still there
/// afterwards, on the task, where a sweep can collect it.
#[tokio::test]
async fn a_bound_task_carries_its_subject_and_records_what_it_reported() {
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
            "name": "Nightly dependency audit",
            "prompt": "Look for unresolved internal references.",
            "schedule": { "kind": "manual" },
            "vogt_project": "vogt",
            "vogt_work_item": "WI-7",
            // The sleep is not decoration: the watcher subscribes just after
            // the session is created, and a `printf` that finished first
            // would be a race rather than a test.
            "command": ["/bin/sh", "-lc",
                "sleep 0.3; printf 'VOGT_NOTIFY:bound to %s in %s\\n' \"$VOGT_WORK_ITEM\" \"$VOGT_PROJECT\""],
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let task_id = created["id"].as_str().unwrap().to_string();
    assert_eq!(created["vogt_project"], "vogt");
    assert_eq!(created["vogt_work_item"], "WI-7");

    let run: Value = client
        .post(format!("{base}/api/agent-tasks/{task_id}/run"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let prompt_text = std::fs::read_to_string(run["prompt_file"].as_str().unwrap()).unwrap();
    assert!(
        prompt_text.contains("Vogt subject: WI-7 (project vogt)"),
        "the run's own prompt must name what it is about: {prompt_text}"
    );

    let detail = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let detail: Value = client
                .get(format!("{base}/api/agent-tasks/{task_id}"))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            if detail["runs"][0]["findings"]
                .as_array()
                .is_some_and(|f| !f.is_empty())
            {
                break detail;
            }
        }
    })
    .await
    .expect("the notify phrase should have produced a finding");

    let finding = &detail["runs"][0]["findings"][0];
    assert_eq!(finding["text"], "bound to WI-7 in vogt");
    assert_eq!(finding["source"], "notify-phrase");
    assert!(finding["at"].as_str().is_some());
}

/// An unbound task is the engine's own business, and says nothing about Vogt.
#[tokio::test]
async fn an_unbound_task_names_no_vogt_subject() {
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
            "name": "Unbound",
            "prompt": "Do something for its own sake.",
            "schedule": { "kind": "manual" },
            "command": ["/bin/sh", "-lc", "true"],
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(created.get("vogt_project").is_none());
    assert!(created.get("vogt_work_item").is_none());

    let run: Value = client
        .post(format!(
            "{base}/api/agent-tasks/{}/run",
            created["id"].as_str().unwrap()
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let prompt_text = std::fs::read_to_string(run["prompt_file"].as_str().unwrap()).unwrap();
    assert!(!prompt_text.contains("Vogt subject"));
}

/// The fake-agent is registered as a session preset, so a client can
/// discover it the same way it discovers the real agent templates.
#[tokio::test]
async fn fake_agent_is_registered_as_a_session_preset() {
    let (base, _h) = boot().await;
    let cfg: Value = reqwest::get(format!("{base}/api/config"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let templates = cfg["session_templates"]
        .as_array()
        .expect("config advertises session templates");
    assert!(
        templates.iter().any(|t| t["name"] == "Fake Agent (test)"),
        "the synthetic agent preset should be listed: {templates:?}"
    );
}

/// Fake-agent edit+commit, driven all the way through the engine's agent-task path.
///
/// The run's working tree is a real git repo; the fake-agent edits a file and
/// commits it with the checkpoint trailers, and the trailer carrying the run
/// id must equal the run the engine actually started. This is the seam the
/// future git story needs: a checkpoint a run made, traceable back
/// to that run, produced without a real agent CLI.
#[tokio::test]
async fn fake_agent_edit_commit_scenario_leaves_a_trailered_commit() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    // A committable repo with an identity, so the commit does not depend on the
    // CI user's git config.
    for args in [
        vec!["init", "-q"],
        vec!["config", "user.email", "seed@vogt.invalid"],
        vec!["config", "user.name", "seed"],
    ] {
        let status = std::process::Command::new("git")
            .args(&args)
            .current_dir(&repo)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }

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
            "name": "fake edit+commit",
            "prompt": "Make a checkpoint.",
            "schedule": { "kind": "manual" },
            "command": fake_agent_command("edit+commit"),
            "cwd": repo.to_string_lossy(),
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let task_id = created["id"].as_str().unwrap().to_string();

    let run: Value = client
        .post(format!("{base}/api/agent-tasks/{task_id}/run"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let run_id = run["id"].as_str().unwrap().to_string();

    let detail = wait_for_run_finish(&client, &base, &task_id).await;
    assert_eq!(detail["runs"][0]["status"], "completed");

    // The engine set VOGT_ENGINE_AGENT_TASK_RUN_ID for the run; the fake-agent
    // wrote it into a `Vogt-Run` trailer on the commit it made.
    let trailer = std::process::Command::new("git")
        .args(["log", "-1", "--pretty=%(trailers:key=Vogt-Run,valueonly)"])
        .current_dir(&repo)
        .output()
        .unwrap();
    let trailer = String::from_utf8_lossy(&trailer.stdout).trim().to_string();
    assert_eq!(
        trailer, run_id,
        "the checkpoint commit should carry the engine's run id as a trailer"
    );
}

/// Fake-agent findings, driven through the engine's phrase watcher.
///
/// The fake-agent prints a `VOGT_NOTIFY:` line; the engine records the text
/// after it as a durable finding on the run. No push service, no real
/// agent — just the parse-and-record path typed outcomes build on.
#[tokio::test]
async fn fake_agent_findings_scenario_is_recorded_on_the_run() {
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
            "name": "fake findings",
            "prompt": "Report something.",
            "schedule": { "kind": "manual" },
            "command": fake_agent_command("findings"),
            "env": [["FAKE_AGENT_NOTIFY_TEXT", "the price dropped"]],
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let task_id = created["id"].as_str().unwrap().to_string();

    client
        .post(format!("{base}/api/agent-tasks/{task_id}/run"))
        .send()
        .await
        .unwrap();

    let detail = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let detail: Value = client
                .get(format!("{base}/api/agent-tasks/{task_id}"))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            if detail["runs"][0]["findings"]
                .as_array()
                .is_some_and(|f| !f.is_empty())
            {
                break detail;
            }
        }
    })
    .await
    .expect("the VOGT_NOTIFY line should have produced a finding");

    let finding = &detail["runs"][0]["findings"][0];
    assert_eq!(finding["text"], "the price dropped");
    assert_eq!(finding["source"], "notify-phrase");
}

/// Fake-agent outcome: a chosen non-zero exit is surfaced as an errored run.
#[tokio::test]
async fn fake_agent_outcome_scenario_surfaces_the_exit_code() {
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
            "name": "fake outcome",
            "prompt": "Exit non-zero.",
            "schedule": { "kind": "manual" },
            "command": fake_agent_command("outcome"),
            "env": [["FAKE_AGENT_EXIT_CODE", "7"]],
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let task_id = created["id"].as_str().unwrap().to_string();

    client
        .post(format!("{base}/api/agent-tasks/{task_id}/run"))
        .send()
        .await
        .unwrap();

    let detail = wait_for_run_finish(&client, &base, &task_id).await;
    assert_eq!(detail["runs"][0]["status"], "errored");
    assert_eq!(detail["runs"][0]["exit_code"], 7);
    assert_eq!(detail["runs"][0]["summary"], "Exited with status 7");
}

/// A `work.transitioned` core event starts a bound, audited run.
///
/// The engine subscribes to its own bus — the one the core-event follower
/// feeds — matches the enabled `work-transition` trigger, and starts a NORMAL
/// run. The run is bound to the item that transitioned (its `VOGT_WORK_ITEM`
/// reaches the child and its prompt names it), and its audit names the trigger
/// and the event that fired it, so a `why` could say "ran because WI-7 entered
/// ready at seq 4102".
#[tokio::test]
async fn a_work_transition_event_starts_a_bound_and_audited_run() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = test_config();
    cfg.default_cwd = tmp.path().to_path_buf();
    cfg.workspace_root = tmp.path().canonicalize().unwrap();
    cfg.state_dir = tmp.path().join("state");
    let (base, state, _h) = boot_with_state(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let created: Value = client
        .post(format!("{base}/api/agent-tasks"))
        .json(&json!({
            "name": "ready-watcher",
            "prompt": "A work item reached ready.",
            "schedule": { "kind": "manual" },
            "command": ["/bin/sh", "-lc",
                "sleep 0.2; printf 'bound=%s\\n' \"$VOGT_WORK_ITEM\""],
            "triggers": [
                { "enabled": true, "kind": "work-transition", "to_state": "ready" }
            ],
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let task_id = created["id"].as_str().unwrap().to_string();
    // The trigger round-trips on the task.
    assert_eq!(created["triggers"][0]["kind"], "work-transition");
    assert_eq!(created["triggers"][0]["to_state"], "ready");
    assert_eq!(created["concurrency"], 1);

    // A transition to a *different* state does not fire the trigger.
    publish_core_event(
        &state,
        "work.transitioned",
        "wi_9",
        4100,
        json!({"ref": "WI-9", "from": "open", "to": "in_progress"}),
    );
    // The matching transition does.
    publish_core_event(
        &state,
        "work.transitioned",
        "wi_7",
        4102,
        json!({"ref": "WI-7", "from": "review", "to": "ready"}),
    );

    let detail = wait_for_run_count(&client, &base, &task_id, 1).await;
    // Only the matching event started a run.
    assert_eq!(detail["runs"].as_array().unwrap().len(), 1);
    let run = &detail["runs"][0];
    assert_eq!(run["trigger"], "event");
    // The audit names the trigger and the exact event that fired it.
    assert_eq!(run["trigger_detail"]["trigger_kind"], "work-transition");
    assert_eq!(run["trigger_detail"]["event_kind"], "work.transitioned");
    assert_eq!(run["trigger_detail"]["event_id"], "wi_7");
    assert_eq!(run["trigger_detail"]["event_seq"], 4102);
    assert_eq!(run["trigger_detail"]["description"], "WI-7 entered ready");

    // The run is bound to the item that transitioned: its prompt names it.
    let prompt = std::fs::read_to_string(run["prompt_file"].as_str().unwrap()).unwrap();
    assert!(
        prompt.contains("Vogt subject: WI-7"),
        "the triggered run must be bound to the item that fired it: {prompt}"
    );
}

/// Drift, observation, and forge-PR-check events each start the right
/// run, driven by synthetic core events.
#[tokio::test]
async fn drift_observation_and_forge_events_each_start_a_run() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = test_config();
    cfg.default_cwd = tmp.path().to_path_buf();
    cfg.workspace_root = tmp.path().canonicalize().unwrap();
    cfg.state_dir = tmp.path().join("state");
    let (base, state, _h) = boot_with_state(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    // Each case: the trigger to arm, the event that should fire it, and the
    // audit description the run should carry.
    struct Case {
        name: &'static str,
        trigger: Value,
        event_kind: &'static str,
        entity_id: &'static str,
        summary: Value,
        expect_desc: &'static str,
    }
    let cases = [
        Case {
            name: "drift-watcher",
            trigger: json!({"enabled": true, "kind": "drift-proposed", "project": "vogt"}),
            event_kind: "drift.raised",
            entity_id: "d_1",
            summary: json!({"kind": "coupling", "summary": "x leaks into y", "project": "vogt"}),
            expect_desc: "drift raised: x leaks into y",
        },
        Case {
            name: "issue-watcher",
            trigger: json!({"enabled": true, "kind": "observation-new", "observation_kind": "forge.issue"}),
            event_kind: "observation.new",
            entity_id: "obs_1",
            summary: json!({"kind": "forge.issue", "project": "vogt", "subject": "#42"}),
            expect_desc: "new forge.issue: #42",
        },
        Case {
            name: "checks-watcher",
            trigger: json!({"enabled": true, "kind": "forge-pr-checks", "status": "red"}),
            event_kind: "forge.pr.checks",
            entity_id: "pr_12",
            summary: json!({"status": "red", "work_item": "WI-7", "pr": "#12"}),
            expect_desc: "PR #12 checks red",
        },
    ];

    let mut seq = 5000;
    for case in cases {
        let created: Value = client
            .post(format!("{base}/api/agent-tasks"))
            .json(&json!({
                "name": case.name,
                "prompt": "React.",
                "schedule": { "kind": "manual" },
                "command": ["/bin/sh", "-lc", "true"],
                "triggers": [case.trigger],
            }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let task_id = created["id"].as_str().unwrap().to_string();

        seq += 1;
        publish_core_event(
            &state,
            case.event_kind,
            case.entity_id,
            seq,
            case.summary.clone(),
        );

        let detail = wait_for_run_count(&client, &base, &task_id, 1).await;
        let run = &detail["runs"][0];
        assert_eq!(
            run["trigger"], "event",
            "{} should be event-triggered",
            case.name
        );
        assert_eq!(run["trigger_detail"]["event_kind"], case.event_kind);
        assert_eq!(run["trigger_detail"]["description"], case.expect_desc);
    }
}

/// The per-task concurrency cap holds, and a fire it cannot honour is
/// dropped rather than retried (no storm).
#[tokio::test]
async fn the_concurrency_cap_holds_and_a_blocked_fire_does_not_storm() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = test_config();
    cfg.default_cwd = tmp.path().to_path_buf();
    cfg.workspace_root = tmp.path().canonicalize().unwrap();
    cfg.state_dir = tmp.path().join("state");
    let (base, state, _h) = boot_with_state(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    // A task capped at one run, whose run sleeps long enough to still be in
    // flight when the second event arrives.
    let created: Value = client
        .post(format!("{base}/api/agent-tasks"))
        .json(&json!({
            "name": "capped",
            "prompt": "Only one at a time.",
            "schedule": { "kind": "manual" },
            "concurrency": 1,
            "command": ["/bin/sh", "-lc", "sleep 3"],
            "triggers": [
                { "enabled": true, "kind": "work-transition", "to_state": "ready" }
            ],
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let task_id = created["id"].as_str().unwrap().to_string();

    // First fire starts a run.
    publish_core_event(
        &state,
        "work.transitioned",
        "wi_7",
        6001,
        json!({"ref": "WI-7", "to": "ready"}),
    );
    let detail = wait_for_run_count(&client, &base, &task_id, 1).await;
    assert_eq!(detail["runs"][0]["status"], "running");

    // Second fire while the first run is still in flight: the cap is full, so
    // it is dropped, not queued and not retried.
    publish_core_event(
        &state,
        "work.transitioned",
        "wi_8",
        6002,
        json!({"ref": "WI-8", "to": "ready"}),
    );

    // Give the watcher ample time to (not) start a second run.
    tokio::time::sleep(Duration::from_millis(600)).await;
    let detail: Value = client
        .get(format!("{base}/api/agent-tasks/{task_id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        detail["runs"].as_array().unwrap().len(),
        1,
        "the capped task must not start an overlapping run, and must not spin: {:?}",
        detail["runs"]
    );
    assert_eq!(detail["run_count"], 1);
}

/// An `api` fire is refused unless the task has an `api` trigger armed,
/// and records `api` when it is.
#[tokio::test]
async fn an_api_fire_needs_an_api_trigger_and_records_api() {
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

    // No api trigger yet: a programmatic fire is refused.
    let created: Value = client
        .post(format!("{base}/api/agent-tasks"))
        .json(&json!({
            "name": "api-task",
            "prompt": "Fire me from a script.",
            "schedule": { "kind": "manual" },
            "command": ["/bin/sh", "-lc", "true"],
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let task_id = created["id"].as_str().unwrap().to_string();

    let refused = client
        .post(format!("{base}/api/agent-tasks/{task_id}/run?trigger=api"))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), StatusCode::CONFLICT);

    // Arm an api trigger, then the fire records `api`.
    client
        .patch(format!("{base}/api/agent-tasks/{task_id}"))
        .json(&json!({ "triggers": [{ "enabled": true, "kind": "api" }] }))
        .send()
        .await
        .unwrap();

    let run: Value = client
        .post(format!("{base}/api/agent-tasks/{task_id}/run?trigger=api"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(run["trigger"], "api");
    assert_eq!(run["trigger_detail"]["trigger_kind"], "api");

    // Let the api run's session exit so the (concurrency-capped) task is free
    // to run again — otherwise the manual fire below would race it and 409.
    wait_for_run_finish(&client, &base, &task_id).await;

    // A plain human Run Now on the same task still records `manual`.
    let manual: Value = client
        .post(format!("{base}/api/agent-tasks/{task_id}/run"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(manual["trigger"], "manual");
}

#[tokio::test]
async fn task_prompt_artifact_cleanup_prunes_old_runs_and_orphans() {
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
            "name": "artifact cleanup",
            "prompt": "Keep the latest prompt only.",
            "schedule": { "kind": "manual" },
            "command": ["/bin/sh", "-lc", "printf 'done\\n'"],
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let task_id = created["id"].as_str().unwrap().to_string();

    let mut prompt_files: Vec<String> = Vec::new();
    for _ in 0..2 {
        let run_deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let detail: Value = client
                .get(format!("{base}/api/agent-tasks/{task_id}"))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            let latest_running = detail["runs"]
                .as_array()
                .and_then(|runs| runs.last())
                .map(|run| run["status"] == "running")
                .unwrap_or(false);
            if !latest_running {
                break;
            }
            assert!(
                tokio::time::Instant::now() < run_deadline,
                "prior task run did not finish in time"
            );
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
        let run: Value = client
            .post(format!("{base}/api/agent-tasks/{task_id}/run"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        prompt_files.push(run["prompt_file"].as_str().unwrap().to_string());
    }

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let detail: Value = client
            .get(format!("{base}/api/agent-tasks/{task_id}"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let all_finished = detail["runs"]
            .as_array()
            .map(|runs| runs.iter().all(|run| run["status"] != "running"))
            .unwrap_or(false);
        if all_finished {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "task runs did not finish in time"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let cleanup_runs: Value = client
        .post(format!("{base}/api/agent-tasks/artifacts/cleanup"))
        .json(&json!({ "keep_latest_runs_per_task": 1 }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(cleanup_runs["removed_prompt_file_count"], 1);
    assert!(!std::path::Path::new(&prompt_files[0]).exists());
    assert!(std::path::Path::new(&prompt_files[1]).exists());

    let deleted = client
        .delete(format!("{base}/api/agent-tasks/{task_id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::OK);

    let orphan_dir = tmp
        .path()
        .join("state")
        .join("agent-task-prompts")
        .join("orphan-task");
    std::fs::create_dir_all(&orphan_dir).unwrap();
    std::fs::write(orphan_dir.join("stale.md"), "stale").unwrap();

    let cleanup_orphans: Value = client
        .post(format!("{base}/api/agent-tasks/artifacts/cleanup"))
        .json(&json!({ "keep_latest_runs_per_task": 1 }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(cleanup_orphans["removed_task_dir_count"], 1);
    let task_dir = tmp
        .path()
        .join("state")
        .join("agent-task-prompts")
        .join(task_id);
    assert!(!task_dir.exists());
    assert!(!orphan_dir.exists());
}

/// A session's brief lands in a file, and the child is told where it is —
/// never handed the text. Vogt writes the work item's brief this way because
/// it is a separate process and the file belongs on the engine's state dir.
#[tokio::test]
async fn session_prompt_is_written_to_a_file_the_child_is_pointed_at() {
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

    let brief = "Fix the flaky forge test.\n\nWhy: it blocks the release.";
    let created: Value = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "work item 42",
            "prompt": brief,
            "command": ["/bin/sh", "-lc",
                "printf 'file=[%s]\\n' \"$VOGT_ENGINE_AGENT_TASK_PROMPT_FILE\"; \
                 cat \"$VOGT_ENGINE_AGENT_TASK_PROMPT_FILE\""],
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
    assert_eq!(std::fs::read_to_string(&prompt_path).unwrap(), brief);

    let path_line = format!("file=[{}]", prompt_path.display());
    let printed = session_output_after_exit(
        &client,
        &base,
        &id,
        &[&path_line, "Fix the flaky forge test."],
    )
    .await
    .printed;
    assert!(
        printed.contains(&path_line),
        "child should be told the prompt file path; got {printed:?}"
    );
    assert!(
        printed.contains("Fix the flaky forge test."),
        "child should be able to read the brief; got {printed:?}"
    );
}

#[tokio::test]
async fn a_session_without_a_prompt_gets_no_file_and_no_variable() {
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
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "plain terminal",
            // Whitespace is not a brief: this must behave exactly like a
            // request that omits the field.
            "prompt": "   ",
            "command": ["/bin/sh", "-lc",
                "printf 'file=[%s]\\n' \"$VOGT_ENGINE_AGENT_TASK_PROMPT_FILE\""],
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap().to_string();

    let printed = session_output_after_exit(&client, &base, &id, &["file=[]"])
        .await
        .printed;
    assert!(
        printed.contains("file=[]"),
        "no brief means no variable; got {printed:?}"
    );
    let sessions_dir = tmp
        .path()
        .join("state")
        .join("agent-task-prompts")
        .join("sessions");
    assert!(
        !sessions_dir.join(format!("{id}.md")).exists(),
        "no brief means no prompt file"
    );
}

#[tokio::test]
async fn deleting_a_session_forgets_its_prompt_file() {
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
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "work item 43",
            "prompt": "Land the migration.",
            "command": ["/bin/cat"],
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
    assert!(prompt_path.exists());

    // Killing keeps the session inspectable, and its brief with it.
    let killed = client
        .post(format!("{base}/api/sessions/{id}/kill"))
        .send()
        .await
        .unwrap();
    assert_eq!(killed.status(), StatusCode::OK);
    assert!(
        prompt_path.exists(),
        "a killed session is still inspectable; its brief stays"
    );

    let deleted = client
        .delete(format!("{base}/api/sessions/{id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::OK);
    assert!(
        !prompt_path.exists(),
        "forgetting the session forgets its brief"
    );
}

/// Prompt files whose session the registry no longer knows — the ones a crash
/// or a restart leaves behind — are collected by the same artifact cleanup
/// endpoint that prunes task run prompts.
#[tokio::test]
async fn artifact_cleanup_collects_prompts_of_sessions_the_registry_forgot() {
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
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "work item 44",
            "prompt": "Keep me: my session still exists.",
            "command": ["/bin/true"],
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap().to_string();

    let sessions_dir = tmp
        .path()
        .join("state")
        .join("agent-task-prompts")
        .join("sessions");
    let live_prompt = sessions_dir.join(format!("{id}.md"));
    // Stands in for a prompt file written before a restart: the session id is
    // well formed, but the registry has never heard of it.
    let stale_prompt = sessions_dir.join(format!("{}.md", uuid::Uuid::new_v4()));
    std::fs::write(&stale_prompt, "brief of a session that no longer exists").unwrap();

    let status: Value = client
        .get(format!("{base}/api/status"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(status["agent_tasks"]["session_prompt_file_count"], 2);
    assert!(
        status["git_sha"].as_str().is_some(),
        "/api/status is missing git_sha"
    );

    let cleanup: Value = client
        .post(format!("{base}/api/agent-tasks/artifacts/cleanup"))
        .json(&json!({ "keep_latest_runs_per_task": 10 }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(cleanup["removed_session_prompt_file_count"], 1);
    // The sessions directory is not a task directory and must never be swept
    // as an orphan one.
    assert_eq!(cleanup["removed_task_dir_count"], 0);
    assert!(!stale_prompt.exists());
    assert!(
        live_prompt.exists(),
        "a brief whose session the registry still holds must survive cleanup"
    );
}

#[tokio::test]
async fn warm_attach_over_budget_is_a_bounded_reset() {
    // F1 (WI-125) inverts the old `warm_attach_is_not_narrowed_by_a_tail_hint`.
    // The tail hint is now the replay budget on *every* path. A warm reattach
    // whose delta exceeds the budget no longer floods the whole ring: it is
    // capped to a ground-state tail and reset:true, because the client's xterm
    // cannot keep more than its scrollback anyway.
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let id: String = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({ "name": "warm", "command": ["/bin/cat"] }))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    const TAIL: usize = 512;
    fill_scrollback(&base, &id, 4 * TAIL).await;

    // resume_from = 0 resolves to the whole retained ring as a delta — far more
    // than the 512-byte budget — so F1 caps it to a bounded reset.
    let mut warm = ws_attach_with_auth(
        &base,
        &id,
        json!({
            "type": "auth",
            "token": TEST_TOKEN,
            "resume_from": 0,
            "snapshot_tail_bytes": TAIL,
        }),
    )
    .await;
    let (reset, len, head) = read_snapshot(&mut warm).await;
    assert!(
        reset,
        "an over-budget delta is a bounded reset, not an append"
    );
    assert!(
        len > 0 && len <= TAIL,
        "the over-budget delta must be capped to the tail budget ({TAIL}); got {len}"
    );
    assert_ground_state_aligned(&head);
    warm.close(None).await.ok();
    kill_session(&client, &base, &id).await;
}

#[tokio::test]
async fn large_stale_warm_reattach_is_a_bounded_reset() {
    // H1(a) / F1. Ring 64 KiB. A load session pushes far past capacity, so a
    // cursor recorded early has aged out of the ring by the time we reattach
    // with it. Before F1 `snapshot_for_attach` answered that stale warm
    // reattach with the FULL untrimmed ring (the flood, 4x worse than a cold
    // attach). F1 (WI-125) bounds it: a ground-state-aligned tail of at most
    // the budget, still reset:true.
    const CAP: usize = 64 * 1024;
    const TAIL_BUDGET: usize = 8 * 1024; // the F1 budget, well under CAP
    let (base, _h) = boot_with(CAP).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let id = create_command_session(&client, &base, "flood", load_session_command(20_000)).await;

    // Record an early cursor, then wait until the ring has advanced several
    // capacities past it so it is unambiguously aged out.
    let early = poll_scrollback_at_least(&client, &base, &id, CAP as u64 / 2).await;
    let _ = poll_scrollback_at_least(&client, &base, &id, early + 4 * CAP as u64).await;

    let mut warm = ws_attach_with_auth(
        &base,
        &id,
        json!({
            "type": "auth",
            "token": TEST_TOKEN,
            "resume_from": early,
            "snapshot_tail_bytes": TAIL_BUDGET,
        }),
    )
    .await;
    let (reset, len, head) = read_snapshot(&mut warm).await;
    assert!(reset, "a cursor aged out of the ring forces a full reset");
    assert!(
        len > 0 && len <= TAIL_BUDGET,
        "F1 bounds the aged-out reattach to the tail budget ({TAIL_BUDGET}); got {len}"
    );
    assert_ground_state_aligned(&head);
    warm.close(None).await.ok();
    kill_session(&client, &base, &id).await;
}

#[tokio::test]
async fn four_mib_ring_stale_reattach_is_ground_state_aligned_and_bounded() {
    // H1(b) / F1. The dimensions prod runs: a 4 MiB ring with ~6.5 MiB pushed.
    // The stale warm reattach is bounded to the tail budget and reset:true,
    // aligned to the terminal's ground state — not the whole 4 MiB ring.
    const CAP: usize = 4 * 1024 * 1024;
    const TAIL_BUDGET: usize = 1024 * 1024;
    let (base, _h) = boot_with(CAP).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let id =
        create_command_session(&client, &base, "flood-4m", load_session_command(100_000)).await;

    // Push well past the ring, then reattach from position 1 (long aged out).
    let _ = poll_scrollback_at_least(&client, &base, &id, CAP as u64 + CAP as u64 / 2).await;
    let mut warm = ws_attach_with_auth(
        &base,
        &id,
        json!({
            "type": "auth",
            "token": TEST_TOKEN,
            "resume_from": 1,
            "snapshot_tail_bytes": TAIL_BUDGET,
        }),
    )
    .await;
    let (reset, len, head) = read_snapshot(&mut warm).await;
    assert!(reset, "position 1 has long since aged out of a 4 MiB ring");
    assert!(
        len > 0 && len <= TAIL_BUDGET,
        "F1 bounds the aged-out 4 MiB reattach to the tail budget ({TAIL_BUDGET}); got {len}"
    );
    assert_ground_state_aligned(&head);
    warm.close(None).await.ok();
    kill_session(&client, &base, &id).await;
}

#[tokio::test]
async fn lagging_subscriber_recovers_in_band_bounded_and_without_duplicates() {
    // H1(c) — lag recovery. A client stops reading while a load session floods
    // far past the 1024-slot broadcast channel, so the server must recover the
    // socket in-band (`send_resync`) rather than drop it. The ring is sized
    // above the ~8 MiB broadcast overflow so the resync stays a reset:false
    // delta. Assert: a resync segment appears, every segment's payload is
    // bounded by the ring, and the SEQ spine across the whole delivered stream
    // is strictly increasing — no duplicate and no reordering across the
    // resync boundary.
    const CAP: usize = 12 * 1024 * 1024;
    let (base, _h) = boot_with(CAP).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    // ~11.7 MiB of output, comfortably past the broadcast channel capacity.
    let id = create_command_session(&client, &base, "lag", load_session_command(180_000)).await;

    let mut ws = ws_attach(&base, &id).await;
    // Do not read at all: the initial snapshot and the live stream both queue
    // in the socket buffer, the outbound task blocks on a full socket, and the
    // broadcast channel overflows behind it. Wait until the producer has
    // written far past the 1024-slot channel, then until it stops — a fixed
    // sleep raced the producer on a slow runner, where the channel never
    // overflowed and the test saw no resync segment.
    let _ = poll_scrollback_at_least(&client, &base, &id, 8 * 1024 * 1024).await;
    let end = wait_until_scrollback_stable(&client, &base, &id).await;
    // Drain until the client's cursor reaches the end of the output, however
    // long the resync takes to arrive — an idle window raced a slow runner.
    let segs = read_segments_until_pos(&mut ws, end).await;

    assert!(
        segs.len() >= 2,
        "expected an in-band resync segment after the flood; saw {} segment(s)",
        segs.len()
    );
    assert!(
        segs[0].reset,
        "the initial cold attach is always a full reset snapshot"
    );
    for s in &segs {
        assert!(
            s.payload.len() <= CAP + CAP / 16,
            "a resync/snapshot payload must be bounded by the ring; got {}",
            s.payload.len()
        );
    }
    // The delivered spine. Live bytes and a `reset:false` delta are the raw
    // byte stream, so every SEQ in them counts, in order. A `reset:true`
    // payload is the grid's frame, a paint of the screen: the producer homes
    // the cursor every 64 lines, so rows below the cursor legitimately hold
    // older lines and the frame's own order is not a stream. What it must
    // never do is carry output past its `scrollback_pos` — that is the
    // duplicate — so it contributes its newest SEQ, which has to precede the
    // first one streamed after it. A resync snapshots from the client's last
    // sent position, so it never re-sends bytes already delivered: the spine
    // is strictly increasing.
    let mut all = Vec::new();
    for s in &segs {
        let payload = seq_numbers(&s.payload);
        if s.reset {
            all.extend(payload.into_iter().max());
        } else {
            all.extend(payload);
        }
        all.extend(seq_numbers(&s.live));
    }
    assert!(
        all.len() > 100,
        "expected a long SEQ spine across the delivered stream, saw {}",
        all.len()
    );
    for w in all.windows(2) {
        assert!(
            w[1] > w[0],
            "SEQ spine went backward or duplicated across a resync: {} then {}",
            w[0],
            w[1]
        );
    }

    ws.close(None).await.ok();
    kill_session(&client, &base, &id).await;
}

/// WI-1519: a worker that finished its turn and rests `idle` at its prompt is
/// a `ready` change, not silence. Claude Code's boxed prompt rarely matches
/// the waiting patterns, so it used not to wake `wait-any` while
/// `/wait?until=ready` did.
#[tokio::test]
async fn an_idle_prompt_is_one_ready_change_and_wait_any_agrees_with_wait() {
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    // Claude Code's input box: a `❯` at the start of its line, inside a border,
    // with no `?` waiting pattern anywhere, drawn after a burst of output and
    // then left alone. The border characters the screen reader strips are `│`,
    // `┃`, `║` and `|`; a `┌┐` box leaves the `❯` where `shows_prompt` reads it.
    let boxed = "printf 'thinking\\n'; sleep 0.4; printf '\\033[2J\\033[H\\n  output\\n┌────┐\\n❯\\n└────┘\\n'; sleep 30";
    let claude = create_session_with(
        &client,
        &base,
        json!({
            "name": "idle-boxed-prompt",
            "command": ["/bin/sh", "-c", boxed],
        }),
    )
    .await;

    // The `ready` rows for one session, polled until one appears.
    let ready_rows = |id: String| {
        let client = client.clone();
        let base = base.clone();
        async move {
            for _ in 0..60 {
                let got: Value = client
                    .get(format!("{base}/api/sessions/changes?since=0&ids={id}"))
                    .send()
                    .await
                    .unwrap()
                    .json()
                    .await
                    .unwrap();
                let rows: Vec<Value> = got["changes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|c| c["kind"] == "ready")
                    .cloned()
                    .collect();
                if !rows.is_empty() {
                    return rows;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            panic!("session {id} never published a ready change");
        }
    };

    let rows = ready_rows(claude.clone()).await;
    assert_eq!(rows.len(), 1, "one ready per turn: {rows:?}");

    let seq = rows[0]["seq"].as_u64().unwrap();
    let any: Value = client
        .get(format!(
            "{base}/api/sessions/wait-any?ids={claude}&until=ready&cursor=0&timeout_s=5"
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(any["outcome"], "changed", "{any}");
    assert_eq!(any["change"]["seq"].as_u64().unwrap(), seq);

    let one: Value = client
        .get(format!(
            "{base}/api/sessions/{claude}/wait?until=ready&timeout_s=5"
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(one["outcome"], "ready", "{one}");
    assert_eq!(one["matched"], true, "{one}");

    // Still at the prompt a moment later: the quiet does not become a second
    // `ready`. (The brief's ten minutes is the same claim — one per turn,
    // reset only when the session goes running again.)
    tokio::time::sleep(Duration::from_secs(3)).await;
    let rows = ready_rows(claude.clone()).await;
    assert_eq!(rows.len(), 1, "no ready storm: {rows:?}");

    client
        .delete(format!("{base}/api/sessions/{claude}"))
        .send()
        .await
        .unwrap();
}

/// A shell nobody reported a conversation from is still forgotten at boot —
/// unless it is the overseer, which stays listed, not resumable, and is not
/// woken into an empty shell (WI-962).
#[tokio::test]
async fn an_oversight_shell_stays_listed_after_a_sigkill_and_a_worker_shell_does_not() {
    let (_tmp, cfg, _stub) = conversation_sandbox();
    let state_dir = cfg.state_dir.clone();
    let (base, guard) = boot_with_config(cfg.clone()).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let mut records = Vec::new();
    for (name, role) in [("the overseer", "oversight"), ("a shell", "worker")] {
        let created: Value = client
            .post(format!("{base}/api/sessions"))
            .json(&json!({ "name": name, "role": role }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let id = created["id"].as_str().unwrap().to_string();
        let path = state_dir.join("sessions").join(format!("{id}.json"));
        lose_session(&client, &base, &id, &path).await;
        records.push((id, path));
    }
    drop(guard);

    let (base, _h) = boot_with_config(cfg).await;
    // Give a boot wake time to (wrongly) happen.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let overseer = session_row(&client, &base, &records[0].0).await;
    assert_eq!(overseer["activity"], "hibernated", "{overseer}");
    assert_eq!(overseer["role"], "oversight");
    assert_eq!(overseer["hibernation"]["resumable"], false);
    let listed: Value = client
        .get(format!("{base}/api/sessions"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let listed: Vec<Value> = serde_json::from_value(listed["sessions"].clone()).unwrap();
    assert!(
        !listed.iter().any(|s| s["id"] == records[1].0.as_str()),
        "a lost worker shell is forgotten: {listed:?}"
    );
    assert!(!records[1].1.exists());
}

/// The hook itself (`engine/deploy/claude-session-hook.sh`), as Claude Code
/// runs it inside a session: it links the conversation through the
/// session's own broker token, unlinks it on `end` only while it is still
/// the session's, and ignores a `claude` whose stdin is not a terminal.
#[tokio::test]
async fn the_claude_session_hook_links_and_unlinks_the_conversation_it_runs_in() {
    let hook = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../deploy/claude-session-hook.sh")
        .canonicalize()
        .unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let cfg = broker_config(&tmp, "LATER proj later ondemand\n");
    // A stand-in `claude` that runs the hook the way Claude Code does, with
    // the hook's JSON on stdin.
    let cli = tmp.path().join("claude");
    std::fs::write(
        &cli,
        format!(
            "#!/bin/bash\nprintf '{{\"session_id\":\"%s\",\"hook_event_name\":\"x\"}}' \"$2\" | '{}' \"$1\"\n",
            hook.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o755)).unwrap();
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let first = "11111111-2222-4333-8444-555555555555";
    let second = "66666666-7777-4888-9999-aaaaaaaaaaaa";
    let other = "bbbbbbbb-cccc-4ddd-8eee-ffffffffffff";
    let cli = cli.display();
    let script = format!(
        "'{cli}' start {first} </dev/null; echo step-1; read; \
         '{cli}' start {first}; echo step-2; read; \
         '{cli}' end {other}; echo step-3; read; \
         '{cli}' start {second}; '{cli}' end {first}; echo step-4; read; \
         '{cli}' end {second}; echo step-5; sleep 30"
    );
    let id = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "hand-typed claude",
            "command": ["/bin/bash", "-c", script],
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
    let conversation = |row: &Value| row["conversation"]["id"].as_str().map(str::to_string);
    let step = |n: u32| {
        let client = client.clone();
        let base = base.clone();
        let id = id.clone();
        async move {
            live_output_containing(&client, &base, &id, &[&format!("step-{n}")]).await;
            session_row(&client, &base, &id).await
        }
    };
    let next = |client: &reqwest::Client| {
        client
            .post(format!("{base}/api/sessions/{id}/input"))
            .json(&json!({ "text": "\r" }))
            .send()
    };

    // A `claude` with no terminal on stdin is not the session's own.
    assert_eq!(conversation(&step(1).await), None);
    next(&client).await.unwrap();
    assert_eq!(conversation(&step(2).await).as_deref(), Some(first));
    next(&client).await.unwrap();
    // The end of a conversation that is not the session's changes nothing.
    assert_eq!(conversation(&step(3).await).as_deref(), Some(first));
    next(&client).await.unwrap();
    // `/clear`: the new conversation is reported, then the old one ends.
    assert_eq!(conversation(&step(4).await).as_deref(), Some(second));
    next(&client).await.unwrap();
    assert_eq!(conversation(&step(5).await), None);

    // Nobody else can report: no token, or one the engine never issued.
    let anonymous = reqwest::Client::new();
    for bearer in [None, Some("not-a-broker-token")] {
        let mut request = anonymous
            .post(format!("{base}/api/agent-auth/conversation"))
            .json(&json!({ "agent": "claude", "id": first }));
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
async fn a_named_claude_account_runs_the_session_in_its_own_config_dir() {
    let (tmp, mut cfg, _stub) = hibernation_sandbox();
    let acct2 = tmp.path().join("acct2");
    let default_home = tmp.path().join("default-home");
    let workspace = cfg.workspace_root.clone();
    let cfg_root = workspace.to_string_lossy().into_owned();
    cfg.agent_onboarding = vogt_engine_server::claude_config::Onboarding {
        enabled: true,
        default_dir: Some(default_home.clone()),
        settings_file: None,
        opencode_config: None,
        account_concurrency_cap: 8,
        default_account: None,
        accounts_root: None,
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
    let want = format!("cfg=[{}]", acct2.display());

    let created: Value = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "second",
            "command": [stub.to_string_lossy()],
            "env": [["CLAUDE_CONFIG_DIR", "/somewhere/the/caller/chose"]],
            "account": "acct2",
            "cwd": workspace.to_string_lossy(),
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(created["account"], "acct2", "{created}");
    let id = created["id"].as_str().unwrap().to_string();
    // The engine's directory for the name wins over a caller-supplied one.
    live_output_containing(&client, &base, &id, &[&want]).await;

    // Folder trust and the external-import approval are answered in the
    // account's own `.claude.json`, not the default one (WI-2000).
    let config: Value =
        serde_json::from_slice(&std::fs::read(acct2.join(".claude.json")).unwrap()).unwrap();
    let project = &config["projects"][workspace.to_string_lossy().as_ref()];
    assert_eq!(project["hasTrustDialogAccepted"], true, "{config}");
    assert_eq!(
        project["hasClaudeMdExternalIncludesApproved"], true,
        "{config}"
    );
    // So is the workspace root, whose `CLAUDE.md` imports from outside it.
    let root = &config["projects"][cfg_root.as_str()];
    assert_eq!(root["hasTrustDialogAccepted"], true, "{config}");
    assert_eq!(
        root["hasClaudeMdExternalIncludesApproved"], true,
        "{config}"
    );
    assert!(!default_home.join(".claude.json").exists());

    // A wake stays on the same login, and the summary still names it.
    client
        .post(format!("{base}/api/sessions/{id}/hibernate"))
        .send()
        .await
        .unwrap();
    let hibernated: Value = client
        .get(format!("{base}/api/sessions/{id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(hibernated["summary"]["account"], "acct2", "{hibernated}");
    client
        .post(format!("{base}/api/sessions/{id}/wake"))
        .send()
        .await
        .unwrap();
    live_output_containing(&client, &base, &id, &[&want]).await;

    // An account the operator never listed, and a command that is not Claude
    // Code, are refused rather than started on the default login.
    for body in [
        json!({ "name": "x", "command": [stub.to_string_lossy()], "account": "nope" }),
        json!({ "name": "y", "command": ["/bin/sh", "-c", "true"], "account": "acct2" }),
    ] {
        let refused = client
            .post(format!("{base}/api/sessions"))
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(refused.status(), StatusCode::BAD_REQUEST, "{body}");
    }
}

/// WI-2232: a session reported blocked, whose block is seen before it is back
/// at its prompt (as a woken agent's is), answers a `ready` wait only once it
/// is there. The caller then types into it, so the wait and the input agree.
#[tokio::test]
async fn a_blocked_session_is_ready_for_input_once_it_is_back_at_its_prompt() {
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let id = create_session_with(
        &client,
        &base,
        json!({
            "name": "woken-blocked",
            "command": ["/bin/sh", "-c", "sleep 1; printf '> '; sleep 30"],
        }),
    )
    .await;
    // The block is reported before the prompt has appeared.
    let blocked = client
        .post(format!("{base}/api/sessions/{id}/blocked"))
        .json(&json!({ "blocked": true, "reason": "needs a person" }))
        .send()
        .await
        .unwrap();
    assert_eq!(blocked.status(), StatusCode::OK);

    let waited: Value = client
        .get(format!(
            "{base}/api/sessions/{id}/wait?until=ready&timeout_s=20"
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(waited["outcome"], "blocked", "{waited:?}");
    assert_eq!(waited["screen"]["ready"], true, "{waited:?}");

    let typed = client
        .post(format!("{base}/api/sessions/{id}/input"))
        .json(&json!({ "text": "continue", "submit": true, "person": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(typed.status(), StatusCode::OK, "{waited:?}");

    client
        .delete(format!("{base}/api/sessions/{id}"))
        .send()
        .await
        .unwrap();
}

/// WI-949: a driven opencode session on autopilot works through a backlog
/// unattended. The stand-in draws opencode's own idle composer (the `┃` bar
/// and `╹▀` footer, no prompt glyph), takes one backlog item per line of
/// input, and prints `AUTOPILOT: DONE` after the last. Nothing in this test
/// types into it: every item after the start is the engine's nudge.
#[tokio::test]
#[ignore = "quarantined: load-flaky, a fourth nudge after a silent gap (WI-4449 part c; operator kept the re-nudge rule). See docs/ci.md, Quarantined tests."]
async fn an_autopilot_opencode_session_advances_through_its_backlog_unattended() {
    use std::os::unix::fs::PermissionsExt;
    const ITEMS: usize = 3;

    let tmp = tempfile::tempdir().unwrap();
    let log = tmp.path().join("done.log");
    let bin = tmp.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let opencode = bin.join("opencode");
    std::fs::write(
        &opencode,
        format!(
            r#"#!/usr/bin/env bash
box() {{ printf '  ┃\r\n  ┃  Ask anything…\r\n  ┃\r\n  ╹▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀\r\n   tab agents  ctrl+p commands\r\n'; }}
n=0
box
while IFS= read -r _; do
  n=$((n+1))
  echo "item $n" >> '{log}'
  printf '     ▣  Build · stand-in · item %s done\r\n' "$n"
  if [ "$n" -ge {ITEMS} ]; then printf '     AUTOPILOT: DONE\r\n'; fi
  box
done
"#,
            log = log.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&opencode, std::fs::Permissions::from_mode(0o755)).unwrap();

    let mut cfg = test_config();
    cfg.autopilot = vogt_engine_server::autopilot::Policy {
        nudge_after: Duration::from_millis(600),
        max_nudges: 10,
    };
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let created: Value = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "backlog",
            "command": [opencode.display().to_string()],
            "autopilot": true,
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap().to_string();
    assert_eq!(created["autopilot"], true);

    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let summary = loop {
        let detail: Value = client
            .get(format!("{base}/api/sessions/{id}"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let summary = detail["summary"].clone();
        if summary.get("autopilot").is_none() {
            break summary;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "autopilot never finished: {summary}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    let done = std::fs::read_to_string(&log).unwrap_or_default();
    assert_eq!(
        done.lines().count(),
        ITEMS,
        "one item per nudge, and none after DONE: {done:?}"
    );
    assert_eq!(summary["autopilot_nudges"], ITEMS as u64);

    // Done is final: no further nudge however long it sits there.
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    let done = std::fs::read_to_string(&log).unwrap_or_default();
    assert_eq!(done.lines().count(), ITEMS);
}

/// WI-1518: an autopilot session whose agent prints `AUTOPILOT: DONE` at its
/// prompt yields exactly one `done` change on the overseer feed, and a
/// wait-any on the default `until` wakes for it. Autopilot is off once the
/// marker is seen, so the change does not repeat however long it sits there.
#[tokio::test]
async fn an_autopilot_session_that_declares_done_yields_one_done_change() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::tempdir().unwrap();
    let bin = tmp.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let opencode = bin.join("opencode");
    std::fs::write(
        &opencode,
        r#"#!/usr/bin/env bash
box() { printf '  ┃\r\n  ┃  Ask anything…\r\n  ┃\r\n  ╹▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀\r\n   tab agents  ctrl+p commands\r\n'; }
box
printf '     AUTOPILOT: DONE\r\n'
box
while IFS= read -r _; do box; done
"#,
    )
    .unwrap();
    std::fs::set_permissions(&opencode, std::fs::Permissions::from_mode(0o755)).unwrap();

    let mut cfg = test_config();
    cfg.autopilot = vogt_engine_server::autopilot::Policy {
        nudge_after: Duration::from_millis(600),
        max_nudges: 10,
    };
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let created: Value = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "backlog",
            "command": [opencode.display().to_string()],
            "autopilot": true,
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap().to_string();

    // An overseer's wait: the default `until`, advancing its cursor past
    // anything else (the prompt's `ready` comes first) until `done` lands.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let mut cursor = 0u64;
    let done = loop {
        let wait: Value = client
            .get(format!(
                "{base}/api/sessions/wait-any?ids={id}&cursor={cursor}&timeout_s=5"
            ))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        cursor = wait["next_cursor"].as_u64().unwrap();
        if wait["change"]["kind"] == "done" {
            break wait["change"].clone();
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "no done change arrived: {wait}"
        );
    };
    assert_eq!(done["session"], id.as_str());

    // Sit past several ticks: no second `done`, and autopilot is off.
    tokio::time::sleep(Duration::from_millis(1_500)).await;
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
    let dones = feed["changes"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["kind"] == "done")
        .count();
    assert_eq!(dones, 1, "exactly one done change: {feed}");

    let detail: Value = client
        .get(format!("{base}/api/sessions/{id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let summary = &detail["summary"];
    assert!(
        summary.get("autopilot").is_none(),
        "autopilot is off: {summary}"
    );
    assert!(
        summary["done_at"].is_string(),
        "the summary keeps when it was declared: {summary}"
    );
}

/// A session blocked on a person is never nudged, autopilot or not.
#[tokio::test]
async fn an_autopilot_session_blocked_on_a_person_is_left_alone() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let log = tmp.path().join("input.log");
    // The agent shows its prompt only once the test has blocked the session,
    // so the nudge clock cannot start before the block lands (WI-2711).
    let go = tmp.path().join("go");
    let bin = tmp.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let opencode = bin.join("opencode");
    std::fs::write(
        &opencode,
        format!(
            "#!/usr/bin/env bash\nuntil [ -e '{}' ]; do sleep 0.02; done\nprintf '  ┃\\r\\n  ╹▀▀▀▀▀▀▀▀\\r\\n'\nwhile IFS= read -r line; do echo x >> '{}'; done\n",
            go.display(),
            log.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&opencode, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut cfg = test_config();
    cfg.autopilot = vogt_engine_server::autopilot::Policy {
        nudge_after: Duration::from_millis(400),
        max_nudges: 10,
    };
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let created: Value = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "blocked",
            "command": [opencode.display().to_string()],
            "autopilot": true,
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap().to_string();
    let status = client
        .post(format!("{base}/api/sessions/{id}/blocked"))
        .json(&json!({"blocked": true, "reason": "needs a person"}))
        .send()
        .await
        .unwrap()
        .status();
    assert!(status.is_success());
    std::fs::write(&go, "").unwrap();
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(
        std::fs::read_to_string(&log).unwrap_or_default().is_empty(),
        "a blocked session was nudged"
    );
}

/// WI-1313: a session that sits at its prompt through its whole nudge budget
/// is not driven forever. Once the cap is spent, autopilot turns off and the
/// session reports itself blocked, which is what the inbox reads as needing a
/// person.
#[tokio::test]
async fn an_autopilot_session_that_never_finishes_escalates_when_its_nudges_run_out() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let log = tmp.path().join("input.log");
    let bin = tmp.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let claude = bin.join("claude");
    std::fs::write(
        &claude,
        format!(
            "#!/usr/bin/env bash\nprintf '  ┃\\r\\n  ╹▀▀▀▀▀▀▀▀\\r\\n'\nwhile IFS= read -r _; do echo x >> '{}'; done\n",
            log.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut cfg = test_config();
    cfg.autopilot = vogt_engine_server::autopilot::Policy {
        nudge_after: Duration::from_millis(400),
        max_nudges: 2,
    };
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let created: Value = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "trailed-off",
            "command": [claude.display().to_string()],
            "autopilot": true,
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap().to_string();

    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let summary = loop {
        let detail: Value = client
            .get(format!("{base}/api/sessions/{id}"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let summary = detail["summary"].clone();
        if summary.get("autopilot").is_none() && summary.get("blocked").is_some() {
            break summary;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "a trailed-off session never escalated: {summary}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    assert_eq!(summary["autopilot_nudges"], 2);
    assert!(
        summary["blocked"]["reason"]
            .as_str()
            .unwrap_or_default()
            .contains("needs a person"),
        "{summary}"
    );
    let nudged = std::fs::read_to_string(&log).unwrap_or_default();
    assert_eq!(
        nudged.lines().count(),
        2,
        "exactly the cap, then stop: {nudged:?}"
    );

    // Escalation is final: sitting longer adds neither a nudge nor a second report.
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    let nudged = std::fs::read_to_string(&log).unwrap_or_default();
    assert_eq!(nudged.lines().count(), 2);
}

#[tokio::test]
async fn removing_oversight_lifts_its_pin_but_keeps_a_workers_own_pin() {
    // WI-1091: oversight pins a session awake; making it a worker again must
    // put it back under the ordinary idle policy, or a demoted overseer stays
    // pinned for good. A worker pinned on its own is left pinned.
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let create = |name: &'static str| {
        let client = client.clone();
        let base = base.clone();
        async move {
            client
                .post(format!("{base}/api/sessions"))
                .json(&json!({ "name": name, "command": ["/bin/cat"] }))
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
    let post = |path: String, body: Value| {
        let client = client.clone();
        async move {
            client
                .post(path)
                .json(&body)
                .send()
                .await
                .unwrap()
                .json::<Value>()
                .await
                .unwrap()
        }
    };
    let overseer = create("overseer").await;
    let promoted = post(
        format!("{base}/api/sessions/{overseer}/role"),
        json!({ "role": "oversight" }),
    )
    .await;
    assert_eq!(promoted["keep_awake"], true);
    let demoted = post(
        format!("{base}/api/sessions/{overseer}/role"),
        json!({ "role": "worker" }),
    )
    .await;
    assert!(demoted.get("role").is_none(), "a worker again: {demoted}");
    assert!(
        demoted.get("keep_awake").is_none(),
        "removing oversight lifts its pin: {demoted}"
    );

    let pinned = create("pinned worker").await;
    post(
        format!("{base}/api/sessions/{pinned}/keep-awake"),
        json!({ "keep_awake": true }),
    )
    .await;
    let still = post(
        format!("{base}/api/sessions/{pinned}/role"),
        json!({ "role": "worker" }),
    )
    .await;
    assert_eq!(
        still["keep_awake"], true,
        "a worker's own pin stays: {still}"
    );

    kill_session(&client, &base, &overseer).await;
    kill_session(&client, &base, &pinned).await;
}

/// WI-983: a permission prompt is answered by a person and nobody else. A
/// session token, an engine-minted token, the pod token — even one holding
/// `admin` — and vogt-core's own credential without a person behind it are
/// refused on every route that reaches the terminal: `/answer`, raw
/// `/input`, and a WebSocket keystroke. Nothing they send is typed. A person
/// answers it; so does vogt-core relaying a person.
#[tokio::test]
async fn only_a_person_answers_a_permission_prompt() {
    let (base, tmp, _guard) = wi983_front_door().await;
    let operator = client_for(TEST_TOKEN);
    let script = dialog_script(tmp.path(), "ask.py", "Do you want to proceed?");
    let (id, approval) = session_showing_dialog(&operator, &base, "asking", &script).await;
    assert_eq!(approval["kind"], "permission", "{approval}");

    for token in [WI983_SESSION, WI983_ENGINE, WI983_POD, WI983_AGENT_ADMIN] {
        let agent = client_for(token);
        let answered = agent
            .post(format!("{base}/api/sessions/{id}/answer"))
            .json(&json!({ "option": 1, "person": true }))
            .send()
            .await
            .unwrap();
        assert_eq!(answered.status(), StatusCode::FORBIDDEN, "{token}");
        let said: Value = answered.json().await.unwrap();
        let said = said["error"].as_str().unwrap();
        assert!(said.contains("person required"), "{said}");
        assert!(said.contains("session_report_blocked"), "{said}");

        // Raw input is the same answer by other means: a TUI dialog is
        // modal, so "1", Enter or Esc all land on it.
        for text in ["1", "\r", "\x1b"] {
            let typed = agent
                .post(format!("{base}/api/sessions/{id}/input"))
                .json(&json!({ "text": text, "submit": false, "person": true }))
                .send()
                .await
                .unwrap();
            assert_eq!(typed.status(), StatusCode::FORBIDDEN, "{token} {text:?}");
        }
    }

    // vogt-core's own credential relays for a principal; without saying a
    // person is behind it, it is refused like an agent.
    let core = client_for(WI983_STACK);
    for body in [
        json!({ "option": 1 }),
        json!({ "option": 1, "person": false }),
    ] {
        let answered = core
            .post(format!("{base}/api/sessions/{id}/answer"))
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(answered.status(), StatusCode::FORBIDDEN, "{body}");
    }
    let typed = core
        .post(format!("{base}/api/sessions/{id}/input"))
        .json(&json!({ "text": "1", "submit": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(typed.status(), StatusCode::FORBIDDEN);
    // The break-glass operator, told by the core that an agent is behind
    // it, is refused too.
    let relayed = operator
        .post(format!("{base}/api/sessions/{id}/input"))
        .json(&json!({ "text": "1", "submit": true, "person": false }))
        .send()
        .await
        .unwrap();
    assert_eq!(relayed.status(), StatusCode::FORBIDDEN);

    // A WebSocket attach by an agent: the keystroke is dropped and the
    // socket told why, in band.
    let mut ws = ws_attach_with_token(&base, &id, WI983_SESSION).await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        if let Message::Text(t) = &msg {
            if t.contains("snapshot-done") {
                break;
            }
        }
        assert!(tokio::time::Instant::now() < deadline);
    }
    ws.send(Message::Binary(b"1\r".to_vec().into()))
        .await
        .unwrap();
    let refused = loop {
        let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("an input-refused frame")
            .unwrap()
            .unwrap();
        if let Message::Text(t) = &msg {
            let frame: Value = serde_json::from_str(t).unwrap();
            if frame["type"] == "input-refused" {
                break frame;
            }
        }
    };
    assert!(
        refused["reason"]
            .as_str()
            .unwrap()
            .contains("person required"),
        "{refused}"
    );

    // Nothing any of them sent reached the dialog.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let screen: Value = operator
        .get(format!("{base}/api/sessions/{id}/screen"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(screen["activity"], "awaiting-approval", "{screen}");
    let shown = screen["lines"].to_string();
    assert!(!shown.contains("typed"), "{shown}");
    assert!(!shown.contains("chose"), "{shown}");

    // A person answers it.
    let answered: Value = client_for(WI983_PERSON)
        .post(format!("{base}/api/sessions/{id}/answer"))
        .json(&json!({ "option": 2, "expect_question": "Do you want to proceed?" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(answered["chosen"]["number"], 2, "{answered}");
    assert_eq!(answered["kind"], "permission");
    live_output_containing(&operator, &base, &id, &["chose 2"]).await;

    // vogt-core relaying a person answers it as well — what the core's own
    // `session.answer` sends after deciding its principal is a person.
    let (relayed, _) = session_showing_dialog(&operator, &base, "relayed", &script).await;
    let answered = core
        .post(format!("{base}/api/sessions/{relayed}/answer"))
        .json(&json!({ "option": 1, "person": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(answered.status(), StatusCode::OK);
    live_output_containing(&operator, &base, &relayed, &["chose 1"]).await;
}

/// WI-2028: vogt-core relaying an oversight session (`overseer`) answers a
/// worker's permission prompt, by `/answer` and by `/input` keys. The same
/// relay naming a plain worker, or the worker itself, is still refused.
#[tokio::test]
async fn an_oversight_session_answers_a_workers_permission_prompt() {
    let (base, tmp, _guard) = wi983_front_door().await;
    let operator = client_for(TEST_TOKEN);
    let core = client_for(WI983_STACK);
    let script = dialog_script(tmp.path(), "ask.py", "Do you want to proceed?");
    let new_shell = |name: &'static str| {
        let operator = operator.clone();
        let base = base.clone();
        async move {
            let made: Value = operator
                .post(format!("{base}/api/sessions"))
                .json(&json!({ "name": name, "command": ["/bin/sh"] }))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            made["id"].as_str().unwrap().to_string()
        }
    };
    let overseer = new_shell("overseer").await;
    let promoted = operator
        .post(format!("{base}/api/sessions/{overseer}/role"))
        .json(&json!({ "role": "oversight" }))
        .send()
        .await
        .unwrap();
    assert_eq!(promoted.status(), StatusCode::OK);
    let plain = new_shell("plain worker").await;

    let (id, approval) = session_showing_dialog(&operator, &base, "asking", &script).await;
    assert_eq!(approval["kind"], "permission", "{approval}");
    // A worker that is not oversight, and the asking session itself, are not
    // overseers: refused, nothing typed.
    for named in [plain.as_str(), id.as_str()] {
        for (route, body) in [
            (
                "answer",
                json!({ "option": 1, "person": false, "overseer": named }),
            ),
            (
                "input",
                json!({ "text": "1", "person": false, "overseer": named }),
            ),
        ] {
            let refused = core
                .post(format!("{base}/api/sessions/{id}/{route}"))
                .json(&body)
                .send()
                .await
                .unwrap();
            assert_eq!(refused.status(), StatusCode::FORBIDDEN, "{route} {named}");
            let said: Value = refused.json().await.unwrap();
            assert!(
                said["error"].as_str().unwrap().contains("person required"),
                "{said}"
            );
        }
    }
    // `overseer` from anyone but vogt-core is ignored.
    let forged = client_for(WI983_SESSION)
        .post(format!("{base}/api/sessions/{id}/answer"))
        .json(&json!({ "option": 1, "overseer": overseer }))
        .send()
        .await
        .unwrap();
    assert_eq!(forged.status(), StatusCode::FORBIDDEN);

    // The oversight session answers by option.
    let answered = core
        .post(format!("{base}/api/sessions/{id}/answer"))
        .json(&json!({ "option": 1, "person": false, "overseer": overseer }))
        .send()
        .await
        .unwrap();
    assert_eq!(answered.status(), StatusCode::OK);
    live_output_containing(&operator, &base, &id, &["chose 1"]).await;

    // By keys it is refused while the prompt is open: typing is not an answer,
    // and the refusal says to answer by `session_answer` (WI-4455).
    let (keyed, _) = session_showing_dialog(&operator, &base, "keyed", &script).await;
    let typed = core
        .post(format!("{base}/api/sessions/{keyed}/input"))
        .json(&json!({ "text": "\r", "person": false, "overseer": overseer }))
        .send()
        .await
        .unwrap();
    assert_eq!(typed.status(), StatusCode::FORBIDDEN);
    let said: Value = typed.json().await.unwrap();
    let said = said["error"].as_str().unwrap();
    assert!(said.contains("person required"), "{said}");
    assert!(said.contains("session_answer"), "{said}");
}

/// WI-983 stops at permission prompts. An overseer still drives: it types
/// into a session with no dialog showing, by REST and over a WebSocket, and
/// it answers a startup gate (folder trust), which is not a permission
/// prompt.
#[tokio::test]
async fn an_overseer_still_drives_a_session_that_is_not_asking_permission() {
    let (base, tmp, _guard) = wi983_front_door().await;
    let operator = client_for(TEST_TOKEN);
    let overseer = client_for(WI983_SESSION);

    // Ordinary input to a shell.
    let shell: Value = operator
        .post(format!("{base}/api/sessions"))
        .json(&json!({ "name": "worker", "command": ["/bin/sh"] }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let shell = shell["id"].as_str().unwrap().to_string();
    let typed = overseer
        .post(format!("{base}/api/sessions/{shell}/input"))
        .json(&json!({ "text": "echo driven-$((40+2))", "submit": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(typed.status(), StatusCode::OK);
    live_output_containing(&operator, &base, &shell, &["driven-42"]).await;
    let mut ws = ws_attach_with_token(&base, &shell, WI983_SESSION).await;
    ws.send(Message::Binary(b"echo attached-$((6*7))\r".to_vec().into()))
        .await
        .unwrap();
    live_output_containing(&operator, &base, &shell, &["attached-42"]).await;

    // A folder-trust gate is a startup gate, not a permission prompt.
    let gate = dialog_script(
        tmp.path(),
        "trust.py",
        "Quick safety check: Is this a project you created or one you trust?",
    );
    let (gated, approval) = session_showing_dialog(&operator, &base, "gated", &gate).await;
    assert_eq!(approval["kind"], "folder-trust", "{approval}");
    let answered = overseer
        .post(format!("{base}/api/sessions/{gated}/answer"))
        .json(&json!({ "option": 1 }))
        .send()
        .await
        .unwrap();
    assert_eq!(answered.status(), StatusCode::OK);
    live_output_containing(&operator, &base, &gated, &["chose 1"]).await;
}

/// The assistant's `session.input` card types as whoever approves it, so an
/// agent driving the assistant with its own token cannot approve its way
/// past a permission prompt; a person's approval is typed.
#[tokio::test]
async fn an_agent_cannot_approve_an_assistant_card_onto_a_permission_prompt() {
    let (core, engine) = stand_in_core_relaying(
        vec![
            (
                WI983_SESSION,
                "agent:session:ses_overseer",
                vec!["work.write"],
            ),
            (WI983_PERSON, "human:ada", vec!["work.write"]),
        ],
        WI983_STACK,
    )
    .await;
    let (chat, script) = stand_in_chat().await;
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = test_config();
    cfg.vogt_core_url = Some(core);
    cfg.vogt_core_token = Some(WI983_STACK.into());
    cfg.assistant_api_key = Some("sk-test".into());
    cfg.assistant_base_url = chat;
    let (base, _guard) = boot_with_config(cfg).await;
    engine.set(base.clone()).unwrap();
    let operator = client_for(TEST_TOKEN);
    let dialog = dialog_script(tmp.path(), "ask.py", "Do you want to proceed?");
    let (id, approval) = session_showing_dialog(&operator, &base, "asking", &dialog).await;
    assert_eq!(approval["kind"], "permission", "{approval}");

    // The agent's approval is refused at delivery: the model is told so and
    // nothing reaches the terminal.
    let agent = client_for(WI983_SESSION);
    let approved = approve_typing_one(&agent, &base, &script, &id).await;
    assert_eq!(approved.status(), StatusCode::OK);
    let told = script.lock().unwrap().requests.last().unwrap().to_string();
    assert!(
        told.contains("person required"),
        "the refusal is what the model saw: {told}"
    );
    tokio::time::sleep(Duration::from_millis(500)).await;
    let screen: Value = operator
        .get(format!("{base}/api/sessions/{id}/screen"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(screen["activity"], "awaiting-approval", "{screen}");
    let shown = screen.to_string();
    assert!(
        !shown.contains("typed") && !shown.contains("chose"),
        "nothing was typed: {screen}"
    );

    // A person's approval of the same card is typed.
    let person = client_for(WI983_PERSON);
    let approved = approve_typing_one(&person, &base, &script, &id).await;
    assert_eq!(approved.status(), StatusCode::OK);
    live_output_containing(&operator, &base, &id, &["chose 1"]).await;
}

/// WI-1255: starting an overseer is nominating one. A worker's token — and an
/// agent token with no session — may start a worker but not an oversight
/// session, which would get its own credential and could then be driven to
/// commit for anyone. A person, vogt-core (the stack secret, after the
/// core's own check) and break-glass may.
#[tokio::test]
async fn only_a_person_or_the_core_starts_an_oversight_session() {
    let door = wi1255_front_door().await;
    let spec = |role: &str| {
        json!({
            "name": format!("started as {role}"),
            "role": role,
            "cwd": door.repo.to_string_lossy(),
            "command": ["/bin/bash", "-c", "sleep 300"],
        })
    };
    let operator = client_for(TEST_TOKEN);
    let count = || async {
        let listed: Value = operator
            .get(format!("{}/api/sessions", door.base))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        listed["sessions"].as_array().unwrap().len()
    };
    let before = count().await;

    for token in [WI1255_WORKER, WI1255_STRANGER] {
        let res = client_for(token)
            .post(format!("{}/api/sessions", door.base))
            .json(&spec("oversight"))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN, "{token}");
        let said: Value = res.json().await.unwrap();
        assert!(
            said["error"]
                .as_str()
                .unwrap()
                .contains("only a person nominates"),
            "{said}"
        );
    }
    assert_eq!(count().await, before, "a refused start created nothing");

    let res = client_for(WI1255_WORKER)
        .post(format!("{}/api/sessions", door.base))
        .json(&spec("worker"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    for token in [WI1255_PERSON, WI1255_STACK, TEST_TOKEN] {
        let res = client_for(token)
            .post(format!("{}/api/sessions", door.base))
            .json(&spec("oversight"))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK, "{token}");
        let row: Value = res.json().await.unwrap();
        assert_eq!(row["role"], "oversight", "{token} {row}");
    }
}

/// WI-1281, again: a blocked report must not keep a session past its budget.
///
/// The report is the agent's own word that it is waiting on a person, and the
/// watcher defers to it. An agent that instead keeps taking turns while
/// re-posting the report — each one worded differently, so no repeat guard
/// sees a loop — used that deferral to run for hundreds of turns. Once the
/// hard limit is crossed the report is cleared and the session hibernates at
/// the next turn boundary, the same as one that never reported a block.
#[tokio::test]
async fn a_blocked_report_does_not_outrun_the_turn_budget() {
    const LIMIT: usize = 3;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let stub = file_fed_claude(dir);
    let (base, _h) = boot_with_config(budget_sandbox(dir)).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let created: Value = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "claude reporting itself blocked",
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
    let conversation = created["conversation"]["id"].as_str().unwrap();
    std::fs::write(
        dir.join("transcript_at"),
        dir.join(format!(".claude/projects/k/{conversation}.jsonl"))
            .display()
            .to_string(),
    )
    .unwrap();

    // What the runaway sessions did: keep taking turns while re-reporting
    // "blocked", each report worded differently. Autopilot will not nudge a
    // blocked session, and the runaway did not need it to — it prompted
    // itself. The turns are fed through a file so they leave no person-input
    // stamp, and the feed stops once the limit is passed: past that the
    // session sits at its prompt, which is where the stop lands.
    let reporter = tokio::spawn({
        let client = client.clone();
        let base = base.clone();
        let id = id.clone();
        let feed = dir.join("turns.feed");
        let log = dir.join("turns.log");
        async move {
            loop {
                let done = std::fs::read_to_string(&log)
                    .unwrap_or_default()
                    .lines()
                    .count();
                if done > LIMIT {
                    break;
                }
                client
                    .post(format!("{base}/api/sessions/{id}/blocked"))
                    .json(&json!({
                        "blocked": true,
                        "reason": format!("waiting on operator input, report {done}"),
                    }))
                    .send()
                    .await
                    .ok();
                let mut f = std::fs::OpenOptions::new()
                    .append(true)
                    .open(&feed)
                    .unwrap();
                std::io::Write::write_all(&mut f, b"go on\n").unwrap();
                tokio::time::sleep(Duration::from_millis(700)).await;
            }
        }
    });

    let row = wait_hibernated(&client, &base, &id, 60).await;
    reporter.abort();
    assert_eq!(row["hibernation"]["trigger"], "budget", "{row}");
    // The report must not survive into the record a wake restores, or the
    // session would come back still excused from its budget.
    assert!(row.get("blocked").is_none_or(Value::is_null), "{row}");
    let turns = turns_run(dir);
    assert!(
        (LIMIT..=LIMIT + 2).contains(&turns),
        "stopped after {turns} turns against a limit of {LIMIT}"
    );
}

/// WI-4068: a TUI at its composer with the brief queued is given one Enter.
/// The fake swallows nothing about the prompt itself — it starts the turn when
/// the Enter arrives — which is exactly the "the text is in the box but the
/// submit did not take" state the watchdog exists for.
#[tokio::test]
async fn a_queued_first_prompt_is_submitted_by_the_watchdog_and_starts_a_turn() {
    let tmp = tempfile::tempdir().unwrap();
    let bus = vogt_engine_server::events::EventBus::new(64);
    let session = spawn_fake_codex(
        tmp.path(),
        "#!/bin/sh\n\
         printf '\\033]0;codex: ready\\007'\n\
         printf '> '\n\
         read _line\n\
         printf '\\033]0;codex: working\\007'\n\
         printf 'TURN STARTED\\n'\n\
         exec sleep 30\n",
        &bus,
    );
    vogt_engine_server::first_turn::watch(bus, Arc::clone(&session), first_turn_policy())
        .await
        .unwrap();
    assert!(
        session.blocked().is_none(),
        "a turn started, so nothing is blocked: {:?}",
        session.blocked()
    );
    let (screen, _) = session.render_screen(0);
    assert!(
        screen
            .lines
            .iter()
            .any(|line| line.contains("TURN STARTED")),
        "{:?}",
        screen.lines
    );
    session.kill().unwrap();
}

/// WI-4068: a session that never submits its first prompt must not sit in the
/// list at zero turns. After the one Enter it is reported blocked, with a
/// reason the operator can act on.
#[tokio::test]
async fn a_first_prompt_that_never_submits_is_reported_blocked_after_one_enter() {
    let tmp = tempfile::tempdir().unwrap();
    let bus = vogt_engine_server::events::EventBus::new(64);
    let session = spawn_fake_codex(
        tmp.path(),
        "#!/bin/sh\n\
         printf '\\033]0;codex: ready\\007'\n\
         printf '> '\n\
         exec sleep 30\n",
        &bus,
    );
    vogt_engine_server::first_turn::watch(bus, Arc::clone(&session), first_turn_policy())
        .await
        .unwrap();
    let blocked = session
        .blocked()
        .expect("a stuck first prompt needs a person");
    assert!(
        blocked.reason.contains("first prompt never started a turn"),
        "{blocked:?}"
    );
    session.kill().unwrap();
}

/// WI-1314 (b): `AUTOPILOT: DONE` at the end of a turn after the report
/// clears it with no input; the reporting reply's own sign-off does not.
#[tokio::test]
async fn a_blocked_report_clears_when_a_later_turn_says_autopilot_done() {
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
            "name": "blocked-done",
            "command": turn_script(&format!(
                "while [ ! -e {go} ]; do sleep 0.05; done; \
                 printf 'disk freed, finished\\r\\nAUTOPILOT: DONE\\r\\n'",
                go = go.display()
            )),
        }),
    )
    .await;
    wait_for_activity(&client, &base, &id, RESTING).await;
    report_blocked(&client, &base, &id).await;
    assert_eq!(
        blocked_report(&client, &base, &id).await["reason"],
        "push refused"
    );
    std::fs::write(&go, b"").unwrap();
    live_output_containing(&client, &base, &id, &["AUTOPILOT: DONE"]).await;
    wait_for_unblocked(&client, &base, &id).await;
}
