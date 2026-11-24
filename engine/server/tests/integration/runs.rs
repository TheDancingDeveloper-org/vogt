use super::*;

/// A declared gate opens at the run's prompt boundary and holds the PTY: the
/// session stays alive and the run stays running until the gate is answered.
#[tokio::test]
async fn a_declared_gate_opens_and_holds_the_run() {
    let (cfg, _tmp) = agent_task_config();
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let (task_id, _run) = create_and_run(
        &client,
        &base,
        json!({
            "name": "gated idle",
            "prompt": "Wait at a gate.",
            "schedule": { "kind": "manual" },
            "command": fake_agent_command("idle"),
            "gates": [ approve_hold_gate() ],
        }),
    )
    .await;

    let gate = wait_for_gate(&client, &base, &task_id, |g| g["state"] == "open").await;
    assert_eq!(gate["question"], "Proceed with the deploy?");

    // Held: after the gate opens, the run does not proceed on its own.
    tokio::time::sleep(Duration::from_millis(400)).await;
    let detail: Value = client
        .get(format!("{base}/api/agent-tasks/{task_id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        detail["runs"][0]["status"], "running",
        "an unanswered gate must hold the run"
    );
    assert_eq!(detail["runs"][0]["gates"][0]["state"], "open");

    // End the held run so the fake-agent exits: a session left blocked on its
    // stdin keeps a `child.wait()` blocking task alive, which the test
    // runtime's shutdown would otherwise wait on forever.
    let session_id = detail["runs"][0]["session_id"].as_str().unwrap();
    client
        .post(format!("{base}/api/sessions/{session_id}/kill"))
        .send()
        .await
        .unwrap();
}

/// Answering an open gate resolves it to the chosen option, delivers that
/// option's input to the PTY, and lets the run finish.
#[tokio::test]
async fn answering_a_gate_resolves_it_and_the_run_continues() {
    let (cfg, _tmp) = agent_task_config();
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let (task_id, _run) = create_and_run(
        &client,
        &base,
        json!({
            "name": "gated idle",
            "prompt": "Wait at a gate.",
            "schedule": { "kind": "manual" },
            "command": fake_agent_command("idle"),
            "gates": [ approve_hold_gate() ],
        }),
    )
    .await;

    let gate = wait_for_gate(&client, &base, &task_id, |g| g["state"] == "open").await;
    let gate_id = gate["id"].as_str().unwrap().to_string();

    let answered: Value = client
        .post(format!(
            "{base}/api/agent-tasks/{task_id}/gates/{gate_id}/answer"
        ))
        .json(&json!({ "option": 0, "actor": "sprooty" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(answered["state"], "answered");
    assert_eq!(answered["actor"], "sprooty");
    assert_eq!(answered["approved"], true);

    let detail = wait_for_run_finish(&client, &base, &task_id).await;
    assert_eq!(detail["runs"][0]["status"], "completed");
    let session_id = detail["runs"][0]["session_id"].as_str().unwrap();
    let text = session_scrollback(&client, &base, session_id).await;
    assert!(
        text.contains("steered with 'go'"),
        "the approve option's input should reach the PTY: {text:?}"
    );
}

/// Fail closed on death: a session killed with a gate held resolves the gate to
/// `blocked`, never approved.
#[tokio::test]
async fn a_gate_whose_session_dies_fails_closed_to_blocked() {
    let (cfg, _tmp) = agent_task_config();
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let (task_id, _run) = create_and_run(
        &client,
        &base,
        json!({
            "name": "gated idle",
            "prompt": "Wait at a gate.",
            "schedule": { "kind": "manual" },
            "command": fake_agent_command("idle"),
            "gates": [ approve_hold_gate() ],
        }),
    )
    .await;

    let gate = wait_for_gate(&client, &base, &task_id, |g| g["state"] == "open").await;
    assert_eq!(gate["state"], "open");
    let detail: Value = client
        .get(format!("{base}/api/agent-tasks/{task_id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let session_id = detail["runs"][0]["session_id"]
        .as_str()
        .unwrap()
        .to_string();

    client
        .post(format!("{base}/api/sessions/{session_id}/kill"))
        .send()
        .await
        .unwrap();

    let blocked = wait_for_gate(&client, &base, &task_id, |g| g["state"] == "blocked").await;
    assert_eq!(blocked["state"], "blocked");
    assert!(
        blocked.get("approved").is_none(),
        "a blocked gate is never approved"
    );
    assert!(blocked["reason"].as_str().unwrap().contains("session"));
}

/// Fail closed on timeout: a gate nobody answers within its deadline resolves
/// to `blocked` and the run ends errored.
#[tokio::test]
async fn a_gate_that_times_out_fails_closed_to_blocked() {
    let (cfg, _tmp) = agent_task_config();
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let mut gate = approve_hold_gate();
    gate["timeout_ms"] = json!(300);
    let (task_id, _run) = create_and_run(
        &client,
        &base,
        json!({
            "name": "gated idle",
            "prompt": "Wait at a gate.",
            "schedule": { "kind": "manual" },
            "command": fake_agent_command("idle"),
            "gates": [ gate ],
        }),
    )
    .await;

    let blocked = wait_for_gate(&client, &base, &task_id, |g| g["state"] == "blocked").await;
    assert!(
        blocked["reason"].as_str().unwrap().contains("timed out"),
        "the block reason should name the timeout: {blocked:?}"
    );

    let detail = wait_for_run_finish(&client, &base, &task_id).await;
    assert_eq!(
        detail["runs"][0]["status"], "errored",
        "a run whose gate blocked cannot complete successfully"
    );
}

/// `--auto-approve` is the one bypass: it answers the gate with its approve
/// option without a human, and the resolution is audited as `auto-approve`.
#[tokio::test]
async fn auto_approve_bypasses_the_gate_and_is_audited() {
    let (cfg, _tmp) = agent_task_config();
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let (task_id, _run) = create_and_run(
        &client,
        &base,
        json!({
            "name": "auto gated idle",
            "prompt": "Wait at a gate, but approve it yourself.",
            "schedule": { "kind": "manual" },
            "command": fake_agent_command("idle"),
            "gates": [ approve_hold_gate() ],
            "auto_approve": true,
        }),
    )
    .await;

    let answered = wait_for_gate(&client, &base, &task_id, |g| g["state"] == "answered").await;
    assert_eq!(answered["actor"], "auto-approve");
    assert_eq!(answered["auto"], true);
    assert_eq!(answered["approved"], true);

    let detail = wait_for_run_finish(&client, &base, &task_id).await;
    assert_eq!(detail["runs"][0]["status"], "completed");
}

/// A queued steer is delivered to the PTY at the next prompt boundary.
#[tokio::test]
async fn a_queued_steer_reaches_the_pty_at_the_next_boundary() {
    let (cfg, _tmp) = agent_task_config();
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let (task_id, _run) = create_and_run(
        &client,
        &base,
        json!({
            "name": "steerable idle",
            "prompt": "Idle until steered.",
            "schedule": { "kind": "manual" },
            "command": fake_agent_command("idle"),
        }),
    )
    .await;

    let steered: Value = client
        .post(format!("{base}/api/agent-tasks/{task_id}/steer"))
        .json(&json!({ "text": "focus here", "actor": "sprooty" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(steered["ok"], true);

    let detail = wait_for_run_finish(&client, &base, &task_id).await;
    assert_eq!(detail["runs"][0]["status"], "completed");
    let session_id = detail["runs"][0]["session_id"].as_str().unwrap();
    let text = session_scrollback(&client, &base, session_id).await;
    assert!(
        text.contains("steered with 'focus here'"),
        "the steer text should reach the PTY at the boundary: {text:?}"
    );
}

/// `interrupt=true` sends the CLI's cancel (Ctrl-C) first: it reaches the idle
/// fake-agent as a SIGINT, ending the run — proof the cancel was delivered
/// ahead of the text.
#[tokio::test]
async fn an_interrupting_steer_cancels_the_cli_first() {
    let (cfg, _tmp) = agent_task_config();
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let (task_id, _run) = create_and_run(
        &client,
        &base,
        json!({
            "name": "interruptible idle",
            "prompt": "Idle until interrupted.",
            "schedule": { "kind": "manual" },
            "command": fake_agent_command("idle"),
        }),
    )
    .await;

    // Give the run a moment to reach its idle boundary before interrupting.
    tokio::time::sleep(Duration::from_millis(300)).await;
    client
        .post(format!("{base}/api/agent-tasks/{task_id}/steer"))
        .json(&json!({ "text": "stop", "interrupt": true, "actor": "sprooty" }))
        .send()
        .await
        .unwrap();

    let detail = wait_for_run_finish(&client, &base, &task_id).await;
    assert_eq!(
        detail["runs"][0]["status"], "errored",
        "the cancel should interrupt the idle CLI, ending the run: {detail:?}"
    );
}

/// Steering a task with no run in flight is a 409, not a silent no-op: there is
/// nothing to deliver the text to.
#[tokio::test]
async fn steering_a_task_with_no_live_run_is_refused() {
    let (cfg, _tmp) = agent_task_config();
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let created: Value = client
        .post(format!("{base}/api/agent-tasks"))
        .json(&json!({
            "name": "never run",
            "prompt": "Nothing runs.",
            "schedule": { "kind": "manual" },
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let task_id = created["id"].as_str().unwrap();

    let res = client
        .post(format!("{base}/api/agent-tasks/{task_id}/steer"))
        .json(&json!({ "text": "hello" }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CONFLICT);
}

/// A clean exit in a git workspace concludes `succeeded`, and the conclusion
/// carries the run's duration, exit code, the final sha of the bound branch and
/// the diff stats for what the run left on it.
#[tokio::test]
async fn a_succeeded_run_records_a_conclusion_with_git_stats() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    init_git_repo(&repo);

    let mut cfg = test_config();
    cfg.default_cwd = tmp.path().to_path_buf();
    cfg.workspace_root = tmp.path().canonicalize().unwrap();
    cfg.state_dir = tmp.path().join("state");
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let (task_id, _run) = create_and_run(
        &client,
        &base,
        json!({
            "name": "succeeds",
            "prompt": "Make a checkpoint.",
            "schedule": { "kind": "manual" },
            "command": fake_agent_command("edit+commit"),
            "cwd": repo.to_string_lossy(),
        }),
    )
    .await;

    let detail = wait_for_run_finish(&client, &base, &task_id).await;
    let run = &detail["runs"][0];
    assert_eq!(run["status"], "completed");
    assert_eq!(run["outcome"], "succeeded");

    let concl = &run["conclusion"];
    assert_eq!(concl["outcome"], "succeeded");
    assert_eq!(concl["exit_code"], 0);
    assert!(
        concl["duration_ms"].as_u64().unwrap() > 0,
        "a run that ran took some time: {concl}"
    );
    assert!(
        concl["final_sha"].as_str().is_some_and(|s| s.len() >= 7),
        "the bound branch has a tip sha: {concl}"
    );
    let diff = &concl["diffstat"];
    assert!(
        diff["files"].as_u64().unwrap() >= 1,
        "edit+commit touched at least one file: {concl}"
    );
    assert!(
        diff["insertions"].as_u64().unwrap() >= 1,
        "edit+commit added at least one line: {concl}"
    );
}

/// WI-4449: a run's base is the head the checkout was on before the agent ran,
/// even when the agent commits at once. The base used to be read after the
/// agent spawned, so a commit that landed first made the base the commit itself
/// and the diff empty. Each round starts from the head the previous one left,
/// so the window is hit again and again.
#[tokio::test]
async fn a_run_that_commits_at_once_diffs_from_the_head_it_started_on() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    init_git_repo(&repo);

    let mut cfg = test_config();
    cfg.default_cwd = tmp.path().to_path_buf();
    cfg.workspace_root = tmp.path().canonicalize().unwrap();
    cfg.state_dir = tmp.path().join("state");
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    for round in 0..10 {
        let started_on = git_head(&repo);
        let (task_id, _run) = create_and_run(
            &client,
            &base,
            json!({
                "name": format!("commits-at-once-{round}"),
                "prompt": "Make a checkpoint.",
                "schedule": { "kind": "manual" },
                "command": fake_agent_command("edit+commit"),
                "cwd": repo.to_string_lossy(),
            }),
        )
        .await;
        let detail = wait_for_run_finish(&client, &base, &task_id).await;
        let concl = &detail["runs"][0]["conclusion"];
        assert_eq!(
            concl["base_sha"].as_str().map(str::to_string),
            started_on,
            "round {round}: the base is the head the run started on: {concl}"
        );
        assert!(
            concl["diffstat"]["files"].as_u64().unwrap_or(0) >= 1,
            "round {round}: the run's own commit is in its diff: {concl}"
        );
    }
}

/// A non-zero exit concludes `failed`, and the conclusion records the code.
#[tokio::test]
async fn a_nonzero_exit_concludes_failed() {
    let (cfg, _tmp) = agent_task_config();
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let (task_id, _run) = create_and_run(
        &client,
        &base,
        json!({
            "name": "fails",
            "prompt": "Exit non-zero.",
            "schedule": { "kind": "manual" },
            "command": fake_agent_command("outcome"),
            "env": [["FAKE_AGENT_EXIT_CODE", "7"]],
        }),
    )
    .await;

    let detail = wait_for_run_finish(&client, &base, &task_id).await;
    let run = &detail["runs"][0];
    assert_eq!(run["status"], "errored");
    assert_eq!(run["outcome"], "failed");
    assert_eq!(run["conclusion"]["exit_code"], 7);
}

/// A run that prints the skip sentinel and exits cleanly concludes `skipped` —
/// distinct from `succeeded`, so a reader can tell "nothing to do" from "did it".
#[tokio::test]
async fn a_skip_sentinel_concludes_skipped() {
    let (cfg, _tmp) = agent_task_config();
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let (task_id, _run) = create_and_run(
        &client,
        &base,
        json!({
            "name": "skips",
            "prompt": "Nothing to do.",
            "schedule": { "kind": "manual" },
            "command": fake_agent_command("skip"),
            "env": [["FAKE_AGENT_SKIP_REASON", "already current"]],
        }),
    )
    .await;

    let detail = wait_for_run_finish(&client, &base, &task_id).await;
    let run = &detail["runs"][0];
    assert_eq!(run["outcome"], "skipped");
    assert_eq!(run["conclusion"]["outcome"], "skipped");
}

/// A gate that fails closed (here by timing out) concludes the run `blocked` —
/// the fail-closed path surfaced as the typed verdict.
#[tokio::test]
async fn a_gate_that_times_out_concludes_blocked() {
    let (cfg, _tmp) = agent_task_config();
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let (task_id, _run) = create_and_run(
        &client,
        &base,
        json!({
            "name": "gated",
            "prompt": "Wait at a gate that no one answers.",
            "schedule": { "kind": "manual" },
            "command": fake_agent_command("idle"),
            "gates": [ {
                "question": "Proceed?",
                "options": [ { "label": "Approve", "input": "go", "approve": true } ],
                "timeout_ms": 300,
            } ],
        }),
    )
    .await;

    // The gate fails closed on its deadline, which kills the session.
    let gate = wait_for_gate(&client, &base, &task_id, |g| g["state"] == "blocked").await;
    assert_eq!(gate["state"], "blocked");

    let detail = wait_for_run_finish(&client, &base, &task_id).await;
    let run = &detail["runs"][0];
    assert_eq!(run["outcome"], "blocked");
    assert_eq!(run["conclusion"]["outcome"], "blocked");
}

/// Findings that match the `output_schema` first try conclude `succeeded` with
/// no re-prompts.
#[tokio::test]
async fn output_schema_that_matches_first_try_concludes_succeeded() {
    let (cfg, _tmp) = agent_task_config();
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let (task_id, _run) = create_and_run(
        &client,
        &base,
        json!({
            "name": "schema-pass",
            "prompt": "Report structured findings.",
            "schedule": { "kind": "manual" },
            "command": fake_agent_command("schema"),
            "output_schema": findings_schema(),
            "env": [["FAKE_AGENT_SCHEMA_PASS_ON", "1"]],
        }),
    )
    .await;

    let detail = wait_for_run_finish(&client, &base, &task_id).await;
    let run = &detail["runs"][0];
    assert_eq!(run["outcome"], "succeeded", "run: {run}");
    assert_eq!(run["retries"], 0);
    assert_eq!(run["schema_ok"], true);
}

/// Findings that never match are re-prompted up to the budget, then the run
/// concludes `partially-succeeded` with the re-prompt count recorded.
#[tokio::test]
async fn output_schema_that_never_matches_concludes_partially_succeeded_after_retries() {
    let (cfg, _tmp) = agent_task_config();
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let (task_id, _run) = create_and_run(
        &client,
        &base,
        json!({
            "name": "schema-fail",
            "prompt": "Report structured findings it cannot get right.",
            "schedule": { "kind": "manual" },
            "command": fake_agent_command("schema"),
            "output_schema": findings_schema(),
            "output_schema_max_retries": 2,
            // Never reaches a good block, so every attempt fails validation.
            "env": [["FAKE_AGENT_SCHEMA_PASS_ON", "9"]],
        }),
    )
    .await;

    let detail = wait_for_run_finish(&client, &base, &task_id).await;
    let run = &detail["runs"][0];
    assert_eq!(run["outcome"], "partially-succeeded", "run: {run}");
    assert_eq!(run["retries"], 2, "two re-prompts were spent: {run}");
    assert_eq!(run["schema_ok"], false);
    assert_eq!(run["conclusion"]["outcome"], "partially-succeeded");
}

/// A `VOGT_COST:` line the run prints is parsed into the conclusion's cost.
#[tokio::test]
async fn a_cost_line_is_parsed_into_the_conclusion() {
    let (cfg, _tmp) = agent_task_config();
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let (task_id, _run) = create_and_run(
        &client,
        &base,
        json!({
            "name": "reports-cost",
            "prompt": "Report a cost then finish.",
            "schedule": { "kind": "manual" },
            "command": fake_agent_command("cost"),
            "env": [["FAKE_AGENT_COST", "{\"total_usd\": 0.42, \"input_tokens\": 1200}"]],
        }),
    )
    .await;

    let detail = wait_for_run_finish(&client, &base, &task_id).await;
    let run = &detail["runs"][0];
    assert_eq!(run["outcome"], "succeeded");
    let cost = &run["conclusion"]["cost"];
    assert_eq!(cost["total_usd"].as_f64().unwrap(), 0.42);
    assert_eq!(cost["input_tokens"].as_u64().unwrap(), 1200);
}

/// WI-917: a startup gate shows as `awaiting-approval` with its kind and
/// options, and is answered by choice, not by counting arrow presses.
#[tokio::test]
async fn a_trust_gate_is_reported_with_its_options_and_answered_by_choice() {
    let (tmp, cfg, _stub) = hibernation_sandbox();
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    // A menu that redraws its highlight on arrows and reports the choice.
    let gate = tmp.path().join("gate.py");
    std::fs::write(
        &gate,
        r#"import sys, termios, tty, os
opts = ["Yes, I trust this folder", "No, exit"]
sel = 0
def draw():
    sys.stdout.write("\x1b[2J\x1b[H")
    sys.stdout.write("Quick safety check: Is this a project you created or one you trust?\r\n\r\n")
    for i, o in enumerate(opts):
        sys.stdout.write(("❯ " if i == sel else "  ") + f"{i+1}. {o}\r\n")
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
            sys.stdout.write("\x1b[2J\x1b[H" + f"chose {sel+1}\r\n> ")
            sys.stdout.flush()
            os.read(fd, 1)
            sys.exit(0)
        elif buf.startswith(b"\x1b") and len(buf) < 3:
            break
        else:
            buf = buf[1:]
"#,
    )
    .unwrap();
    let created: Value = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({ "name": "gated", "command": ["python3", gate.to_string_lossy()] }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap().to_string();

    // It reads as awaiting-approval, with the gate's kind and its menu.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let approval = loop {
        let screen: Value = client
            .get(format!("{base}/api/sessions/{id}/screen"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if screen["activity"] == "awaiting-approval" {
            break screen["approval"].clone();
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "never awaiting-approval: {screen}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert_eq!(approval["kind"], "folder-trust", "{approval}");
    assert_eq!(approval["options"][1]["label"], "No, exit");
    assert_eq!(approval["options"][0]["selected"], true);

    // A stale question is refused and nothing is typed.
    let stale = client
        .post(format!("{base}/api/sessions/{id}/answer"))
        .json(&json!({ "option": 2, "expect_question": "Do you want to proceed?" }))
        .send()
        .await
        .unwrap();
    assert_eq!(stale.status(), StatusCode::CONFLICT);

    // Answer by label: the engine moves down one and presses Enter.
    let answered: Value = client
        .post(format!("{base}/api/sessions/{id}/answer"))
        .json(&json!({ "label": "no, exit" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(answered["chosen"]["number"], 2, "{answered}");
    assert_eq!(answered["kind"], "folder-trust");
    assert_eq!(answered["dismissed"], true, "{answered}");
    live_output_containing(&client, &base, &id, &["chose 2"]).await;

    let none = client
        .post(format!("{base}/api/sessions/{id}/answer"))
        .json(&json!({ "option": 1 }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        none.status(),
        StatusCode::CONFLICT,
        "no dialog left to answer"
    );
}

/// WI-4455: an oversight session's input is refused while a permission prompt
/// is open, and accepted once the prompt has closed (answered by `/answer`).
/// A plain worker's input stays refused throughout.
#[tokio::test]
async fn an_oversight_session_steers_once_the_prompt_has_closed() {
    let (base, tmp, _guard) = wi983_front_door().await;
    let operator = client_for(TEST_TOKEN);
    let core = client_for(WI983_STACK);
    let script = dialog_then_steering_script(tmp.path(), "steer.py");
    let made: Value = operator
        .post(format!("{base}/api/sessions"))
        .json(&json!({ "name": "overseer", "command": ["/bin/sh"] }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let overseer = made["id"].as_str().unwrap().to_string();
    let promoted = operator
        .post(format!("{base}/api/sessions/{overseer}/role"))
        .json(&json!({ "role": "oversight" }))
        .send()
        .await
        .unwrap();
    assert_eq!(promoted.status(), StatusCode::OK);
    let plain = operator
        .post(format!("{base}/api/sessions"))
        .json(&json!({ "name": "plain worker", "command": ["/bin/sh"] }))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    let plain = plain["id"].as_str().unwrap().to_string();

    let (id, approval) = session_showing_dialog(&operator, &base, "steered", &script).await;
    assert_eq!(approval["kind"], "permission", "{approval}");

    // While the prompt is open: the oversight session's input is refused, and
    // so is a plain worker's, nothing typed.
    for named in [overseer.as_str(), plain.as_str()] {
        let refused = core
            .post(format!("{base}/api/sessions/{id}/input"))
            .json(&json!({ "text": "go\r", "person": false, "overseer": named }))
            .send()
            .await
            .unwrap();
        assert_eq!(refused.status(), StatusCode::FORBIDDEN, "{named}");
    }

    // The overseer answers the prompt; it closes.
    let answered = core
        .post(format!("{base}/api/sessions/{id}/answer"))
        .json(&json!({ "option": 1, "person": false, "overseer": overseer }))
        .send()
        .await
        .unwrap();
    assert_eq!(answered.status(), StatusCode::OK);
    live_output_containing(&operator, &base, &id, &["chose 1"]).await;

    // Once the prompt is gone, the same overseer's steering input is accepted.
    let steered = core
        .post(format!("{base}/api/sessions/{id}/input"))
        .json(&json!({ "text": "go\r", "person": false, "overseer": overseer }))
        .send()
        .await
        .unwrap();
    assert_eq!(steered.status(), StatusCode::OK);
    live_output_containing(&operator, &base, &id, &["steered"]).await;
}

#[tokio::test]
async fn an_inconclusive_github_answer_is_cached_so_fetches_do_not_wait_each_time() {
    let (api, hits) = fake_github_api(503).await;
    let (base, token, _state, _tmp) = github_broker_session(&api).await;
    for _ in 0..3 {
        let r = reqwest::Client::new()
            .post(format!("{base}/api/agent-auth/fetch/GH_TOKEN"))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200, "fail open");
    }
    assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);
}
