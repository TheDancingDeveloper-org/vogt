use super::*;

#[test]
fn named_keys_are_their_own_writes_and_text_submits_after_it() {
    // vogt-core, not the engine, turns named keys into bytes and orders
    // the writes: Enter and Tab are one write each, and text is a write
    // with a separate Enter after it. The engine's `/input` sees exactly
    // these; its own paste rules must keep each one a key (WI-4068).
    let (_dir, built, seen) = context("keys", person(), None, |_| {}, vec![]);
    record_session(&built, "ses_1", "eng-1");
    with_ctx!(&built, |ctx| {
        for key in ["enter", "tab"] {
            input(
                ctx,
                &InputParams {
                    id: "ses_1".to_string(),
                    text: None,
                    keys: Some(vec![key.to_string()]),
                    submit: false,
                    reason: "pressing a key".to_string(),
                    confirm: false,
                    wake_timeout_s: 120,
                },
            )
            .unwrap();
        }
        input(
            ctx,
            &InputParams {
                id: "ses_1".to_string(),
                text: Some("hello".to_string()),
                keys: None,
                submit: true,
                reason: "typing".to_string(),
                confirm: false,
                wake_timeout_s: 120,
            },
        )
        .unwrap();
    });
    let calls = seen.lock().unwrap();
    let writes: Vec<(String, bool)> = calls
        .iter()
        .filter(|(method, path, _)| method == "POST" && path.ends_with("/input"))
        .map(|(_, _, body)| {
            let body: serde_json::Value = serde_json::from_str(body).unwrap();
            (
                body["text"].as_str().unwrap().to_string(),
                body["submit"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        writes,
        vec![
            ("\r".to_string(), false),
            ("\t".to_string(), false),
            ("hello".to_string(), false),
            (String::new(), true),
        ],
        "{calls:?}"
    );
}

#[test]
fn an_input_write_leaves_one_audit_row_and_never_the_typed_text() {
    // WI-834: typing into a PTY is code execution, so each `session.input`
    // is one audit row (actor, session, reason, byte count, key names)
    // and neither the audit nor the event rows may hold the text, which
    // may be a password.
    const SECRET: &str = "hunter2-correct-horse";
    let (_dir, built, _seen) = context("audit", agent("agent:worker"), None, |_| {}, vec![]);
    record_session(&built, "ses_1", "eng-1");
    with_ctx!(&built, |ctx| {
        input(
            ctx,
            &InputParams {
                id: "ses_1".to_string(),
                text: Some(SECRET.to_string()),
                keys: Some(vec!["tab".to_string()]),
                submit: true,
                reason: "steering".to_string(),
                confirm: false,
                wake_timeout_s: 120,
            },
        )
        .unwrap();
        let view = ctx.declared.read().unwrap();
        let rows = view
            .list_audit(&crate::storage::interface::AuditQuery {
                limit: 20,
                offset: 0,
                actor_id: None,
                operation: Some("session.input".to_string()),
                entity_id: None,
                project_id: None,
                since: None,
                until: None,
            })
            .unwrap();
        assert_eq!(rows.len(), 1, "exactly one audit row per write: {rows:?}");
        let row = &rows[0];
        assert_eq!(row.entity_kind, "session");
        assert_eq!(row.entity_id, "ses_1");
        assert_eq!(row.reason, "steering");
        assert_eq!(row.actor_identity_ref, "agent:worker");
        assert!(
            !format!("{row:?}").contains(SECRET),
            "audit row holds the text: {row:?}"
        );
        let events = view.list_events(0, 100, None).unwrap();
        let input_events: Vec<_> = events
            .iter()
            .filter(|event| event.kind == "session.input")
            .collect();
        assert_eq!(input_events.len(), 1, "{events:?}");
        let summary = &input_events[0].summary;
        assert_eq!(summary["bytes"], SECRET.len());
        assert_eq!(summary["keys"], json!(["tab"]));
        assert_eq!(summary["submit"], true);
        assert!(
            !events.iter().any(|e| format!("{e:?}").contains(SECRET)),
            "an event holds the text: {events:?}"
        );
    });
}

fn wait_at(outcome: &str, activity: Option<&str>, ready: Option<bool>) -> EngineWait {
    EngineWait {
        outcome: outcome.to_string(),
        matched: false,
        waited_ms: 0,
        screen: EngineScreen {
            id: "eng-1".to_string(),
            cols: 80,
            rows: 24,
            lines: Vec::new(),
            cursor_row: None,
            cursor_col: None,
            title: None,
            activity: activity.map(str::to_string),
            alive: Some(true),
            ready,
            scrollback: Vec::new(),
            turn_started_at: None,
            last_output_at: None,
            approval: None,
            blocked: None,
        },
    }
}

#[test]
fn a_blocked_session_idle_at_its_prompt_is_ready_for_its_overseer() {
    // WI-2232: a reported block is a waiting state. Idle at the prompt and
    // answered by an overseer or a person, the input goes through.
    let idle = wait_at("blocked", Some("idle"), Some(true));
    assert!(at_prompt_for_input(&idle, true));
    // Nobody relaying it (a plain agent caller) keeps the refusal.
    assert!(!at_prompt_for_input(&idle, false));
    // A permission dialog is never ready here, whoever asks.
    let dialog = wait_at("blocked", Some("awaiting-approval"), Some(true));
    assert!(!at_prompt_for_input(&dialog, true));
    // Still running, or not at the prompt, stays not ready.
    let busy = wait_at("blocked", Some("running"), Some(false));
    assert!(!at_prompt_for_input(&busy, true));
    assert!(at_prompt_for_input(&wait_at("ready", None, None), false));
}

#[test]
fn codex_input_left_in_the_composer_is_resubmitted() {
    fn screen(activity: &str, text: &str) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "id": "eng-1",
            "activity": activity,
            "lines": [format!("› {text}"), "", "  gpt high · ~/work", "  ? for shortcuts"]
        }))
        .unwrap()
    }

    let held = screen("idle", "carry on");
    let mut replies = vec![held.clone(), held.clone()];
    replies.extend(std::iter::repeat_n(held.clone(), 10));
    replies.push(br#"{"outcome":"ready"}"#.to_vec());
    replies.push(held);
    replies.push(screen("running", ""));
    replies.push(screen("running", ""));
    replies.reverse();

    let seen = Arc::new(Mutex::new(Vec::new()));
    let client = engine(replies, Arc::clone(&seen));
    let verdict =
        type_then_submit(&client, "eng-1", "ses_1", "carry on", Some("idle"), true).unwrap();
    let calls = seen.lock().unwrap();
    let submits = calls
        .iter()
        .filter(|(method, path, body)| {
            method == "POST" && path.ends_with("/input") && body.contains(r#""submit":true"#)
        })
        .count();
    assert_eq!(
        submits, 2,
        "the held composer gets one more Enter: {calls:?}"
    );
    assert_eq!(verdict.delivery.as_str(), "delivered");
}

/// WI-4459 (WI-1763): the composer holds other text on every read, so the
/// typed text never shows. No Enter may follow it, and the verdict must
/// say it was not delivered.
#[test]
fn a_text_that_never_shows_in_the_composer_is_not_submitted() {
    let other = serde_json::to_vec(&json!({
        "id": "eng-1",
        "activity": "idle",
        "lines": ["› something else", "", "  gpt high · ~/work", "  ? for shortcuts"]
    }))
    .unwrap();

    let seen = Arc::new(Mutex::new(Vec::new()));
    let client = engine(vec![other; 20], Arc::clone(&seen));
    let verdict =
        type_then_submit(&client, "eng-1", "ses_1", "carry on", Some("idle"), true).unwrap();
    let calls = seen.lock().unwrap();
    let submits = calls
        .iter()
        .filter(|(method, path, body)| {
            method == "POST" && path.ends_with("/input") && body.contains(r#""submit":true"#)
        })
        .count();
    assert_eq!(
        submits, 0,
        "an Enter went out with the text never in the box: {calls:?}"
    );
    assert_eq!(verdict.delivery.as_str(), "unconfirmed");
}

#[test]
fn only_an_oversight_session_is_relayed_as_overseer() {
    let oversight = br#"{"id":"eng-boss","alive":true,"activity":"idle","role":"oversight"}"#;
    let (_dir, built, _) = context(
        "overseer",
        agent("agent:engine:eng-boss"),
        None,
        |_| {},
        vec![oversight.to_vec()],
    );
    with_ctx!(&built, |ctx| {
        let engine = ctx.engine.as_ref().unwrap();
        assert_eq!(
            relayed_overseer(ctx, engine, false).unwrap().as_deref(),
            Some("eng-boss")
        );
    });
    drop(built);
    // A plain worker is not relayed, so the engine still refuses it.
    let (_dir, built, _) = context(
        "not-overseer",
        agent("agent:engine:eng-1"),
        None,
        |_| {},
        vec![live("eng-1")],
    );
    with_ctx!(&built, |ctx| {
        let engine = ctx.engine.as_ref().unwrap();
        assert_eq!(relayed_overseer(ctx, engine, false).unwrap(), None);
        // A person needs no relay and asks the engine nothing.
        assert_eq!(relayed_overseer(ctx, engine, true).unwrap(), None);
    });
}
