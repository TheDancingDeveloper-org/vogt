use super::*;

#[test]
fn a_person_answers_as_a_person_and_an_agent_does_not() {
    let (_dir, built, seen) = context("person", person(), None, |_| {}, vec![live("eng-1")]);
    record_session(&built, "ses_1", "eng-1");
    with_ctx!(&built, |ctx| {
        assert!(answers_as_person(ctx).unwrap());
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
    let calls = seen.lock().unwrap().clone();
    let writes: Vec<_> = calls
        .iter()
        .filter(|(_, _, body)| body.contains("person"))
        .collect();
    assert!(
        writes
            .iter()
            .all(|(_, _, body)| body.contains(r#""person":true"#)),
        "{calls:?}"
    );
    drop(built);

    let (_dir, built, seen) = context(
        "agent",
        agent("agent:worker"),
        None,
        |_| {},
        vec![live("eng-1")],
    );
    record_session(&built, "ses_1", "eng-1");
    with_ctx!(&built, |ctx| {
        assert!(!answers_as_person(ctx).unwrap());
    });
    assert!(seen.lock().unwrap().is_empty());
}

#[test]
fn the_engine_credential_is_not_a_person_even_when_human() {
    let (_dir, built, _) = context(
        "engine-actor",
        Principal::new("agent:vogt-engine", ActorKind::Human, "engine").unwrap(),
        None,
        |_| {},
        vec![],
    );
    with_ctx!(&built, |ctx| assert!(!answers_as_person(ctx).unwrap()));

    let secret_path = std::env::temp_dir().join(format!("vogt-secret-{}", std::process::id()));
    std::fs::write(&secret_path, "the-stack-secret\n").unwrap();
    let (_dir, built, _) = context(
        "stack",
        person(),
        None,
        |config| {
            config.bootstrap_core_token_file = Some(secret_path.clone());
        },
        vec![],
    );
    // The presented token is the one the stack secret hashes to.
    with_ctx!(&built, |ctx| {
        let mut write = write_of(ctx);
        audited_write(&mut write, "token.issue", "test", |txn, actor| {
            txn.insert_token(
                &Token {
                    id: "tok_stack".to_string(),
                    actor_id: actor.id.clone(),
                    actor_identity_ref: Some(actor.identity_ref.clone()),
                    name: "stack".to_string(),
                    scopes: vec!["admin".to_string()],
                    kind: TokenKind::Api,
                    created_at: Moment::from_unix(1_700_000_000, 0),
                    expires_at: None,
                    last_used_at: None,
                    revoked_at: None,
                    revoked_reason: None,
                },
                &auth::hash_token("the-stack-secret"),
            )?;
            Ok(WriteOutcome::new(
                (),
                "token",
                "tok_stack",
                json!({}),
                "token.issued",
                json!({}),
            ))
        })
        .unwrap();
    });
    // Rebuilt over the same store, presenting that token.
    let presented = with_ctx!(&built, |ctx| ctx
        .declared
        .read()
        .unwrap()
        .token_by_hash(&auth::hash_token("the-stack-secret"))
        .unwrap()
        .unwrap());
    let dir = with_ctx!(&built, |ctx| ctx.config.data_dir.clone());
    drop(built);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let rebuilt = build_context(
        crate::config::VogtConfig {
            data_dir: dir,
            bootstrap_core_token_file: Some(secret_path),
            ..crate::config::VogtConfig::default()
        },
        Some(person()),
        Some(StepClock::new(Moment::from_unix(1_700_000_000, 123_000))),
        None,
        Some(presented),
        Some(engine(vec![], Arc::clone(&seen))),
        None,
        None,
    )
    .unwrap();
    with_ctx!(&rebuilt, |ctx| assert!(!answers_as_person(ctx).unwrap()));
    let _ = std::fs::remove_file(
        std::env::temp_dir().join(format!("vogt-secret-{}", std::process::id())),
    );
}

/// A session showing a permission prompt, as the engine reads it: the
/// engine's own escalation key rides on the dialog.
const PROMPTED: &str = r#"{"id":"eng-1","alive":true,"activity":"awaiting-approval","activity_changed_at":"2026-10-31T06:00:00Z","role":"worker","approval":{"question":"Do you want to proceed?","command_excerpt":"rm -rf /tmp/x","detected_at":"2026-10-31T06:00:00Z","kind":"permission","key":"eng-1:permission_prompt:abc123abc123abc1","options":[{"number":1,"label":"Yes","selected":true},{"number":2,"label":"No","selected":false}]}}"#;
const PROMPT_KEY: &str = "eng-1:permission_prompt:abc123abc123abc1";

/// An engine that refuses an agent's answer to a permission prompt (WI-983)
/// and serves `PROMPTED` on every read.
fn refusing_context(
    name: &str,
    principal: Principal,
    seen: Arc<Mutex<Vec<(String, String, String)>>>,
) -> (std::path::PathBuf, crate::application::context::Built) {
    refusing_context_reading(name, principal, seen, true)
}

/// `refusing_context`, and when `readable` is false the session read fails
/// too: the refusal must still come back as an escalation, without keys.
fn refusing_context_reading(
    name: &str,
    principal: Principal,
    seen: Arc<Mutex<Vec<(String, String, String)>>>,
    readable: bool,
) -> (std::path::PathBuf, crate::application::context::Built) {
    let dir = std::env::temp_dir().join(format!("vogt-ses-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut clock = None;
    let mut ids = None;
    crate::application::instance::init(&dir, &mut clock, &mut ids).unwrap();
    let engine = EngineClient::new(
        "http://engine",
        None,
        Some(std::sync::Arc::new(move |path, _, body, method| {
            seen.lock().unwrap().push((
                method.to_string(),
                path.to_string(),
                String::from_utf8_lossy(body).to_string(),
            ));
            if method == "POST" {
                (
                    403,
                    b"forbidden: person required: a permission prompt is answered by a person"
                        .to_vec(),
                )
            } else if readable {
                (200, PROMPTED.as_bytes().to_vec())
            } else {
                (500, b"engine read failed".to_vec())
            }
        })),
    );
    let config = crate::config::VogtConfig {
        data_dir: dir.clone(),
        ..crate::config::VogtConfig::default()
    };
    let built = build_context(
        config,
        Some(principal),
        Some(StepClock::new(Moment::from_unix(1_700_000_000, 123_000))),
        None,
        None,
        Some(engine),
        None,
        None,
    )
    .unwrap();
    (dir, built)
}

fn answer_as(
    built: &crate::application::context::Built,
    option: Option<i64>,
) -> Result<Value, VogtError> {
    crate::with_ctx!(&built, |ctx| super::answer(
        ctx,
        &super::AnswerParams {
            id: "ses_1".to_string(),
            option,
            label: None,
            expect_question: None,
            reason: "picking".to_string(),
        },
    ))
}

#[test]
fn a_refused_answer_to_a_prompt_comes_back_escalated_with_its_pick_recorded() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (dir, built) = refusing_context("escalate", agent("agent:worker"), Arc::clone(&seen));
    record_session(&built, "ses_1", "eng-1");
    let err = answer_as(&built, Some(2)).unwrap_err();
    let VogtError::Escalated { message, details } = &err else {
        panic!("expected an escalation, got {err:?}");
    };
    assert_eq!(details["outcome"], "escalated", "{details}");
    assert_eq!(details["needs"], "person");
    assert_eq!(details["boundary"], "permission_prompt");
    assert_eq!(details["session_id"], "eng-1");
    assert_eq!(details["escalation_key"], PROMPT_KEY);
    assert_eq!(
        details["inbox_entry_key"],
        "agent:session:eng-1:awaiting-approval:2026-10-31T06:00:00Z"
    );
    assert_eq!(details["recommendation"], "recorded");
    assert!(
        message.contains(
            "The prompt is escalated to a person in the Inbox (agent:session:eng-1:awaiting-approval:2026-10-31T06:00:00Z); wait for it, do not retry."
        ) && message.ends_with("Your pick is shown to them as a recommendation."),
        "{message}"
    );
    assert_eq!(err.code(), "person_required");

    let events = crate::with_ctx!(&built, |ctx| {
        let view = ctx.declared.read().unwrap();
        view.list_events(0, 20, Some(PROMPT_KEY)).unwrap()
    });
    let recorded = events
        .iter()
        .find(|event| event.kind == "session.answer_recommended")
        .expect("the pick is recorded under the dialog's key");
    assert_eq!(recorded.summary["option"], json!(2));
    assert_eq!(recorded.summary["label"], json!("No"));
    assert_eq!(recorded.summary["by"], json!("agent:worker"));

    // The Inbox and the oversight board read the same pick back.
    let approval = crate::adapters::engine::EngineApproval {
        question: "Do you want to proceed?".to_string(),
        command_excerpt: "rm -rf /tmp/x".to_string(),
        detected_at: "2026-10-31T06:00:00Z".to_string(),
        deadline_seconds: None,
        deadline_at: None,
        kind: "permission".to_string(),
        options: vec![(1, "Yes".to_string(), true), (2, "No".to_string(), false)],
        key: PROMPT_KEY.to_string(),
    };
    let shown = crate::with_ctx!(&built, |ctx| {
        let view = ctx.declared.read().unwrap();
        crate::application::session_blocked::dialog_recommendation(&view, "eng-1", &approval)
            .unwrap()
    });
    let shown = shown.expect("the recorded pick is shown on its own dialog");
    assert_eq!(shown["option"], json!(2));
    assert_eq!(shown["label"], json!("No"));
    assert_eq!(shown["escalation_key"], json!(PROMPT_KEY));

    // Another showing of the same question (a different command) does not
    // carry the pick over, and neither does a relabelled option.
    let other_command = crate::adapters::engine::EngineApproval {
        command_excerpt: "rm -rf /".to_string(),
        ..approval.clone()
    };
    let relabelled = crate::adapters::engine::EngineApproval {
        options: vec![(1, "Yes".to_string(), true), (2, "Nope".to_string(), false)],
        ..approval.clone()
    };
    crate::with_ctx!(&built, |ctx| {
        let view = ctx.declared.read().unwrap();
        for other in [other_command, relabelled] {
            assert!(crate::application::session_blocked::dialog_recommendation(
                &view, "eng-1", &other
            )
            .unwrap()
            .is_none());
        }
    });
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn a_pick_that_matches_no_option_escalates_without_a_recommendation() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (dir, built) = refusing_context("nopick", agent("agent:worker"), Arc::clone(&seen));
    record_session(&built, "ses_1", "eng-1");
    let err = answer_as(&built, Some(9)).unwrap_err();
    let VogtError::Escalated { message, details } = &err else {
        panic!("expected an escalation, got {err:?}");
    };
    assert_eq!(details["outcome"], "escalated");
    assert!(details.get("recommendation").is_none(), "{details}");
    assert!(!message.contains("recommendation"), "{message}");
    let events = crate::with_ctx!(&built, |ctx| {
        let view = ctx.declared.read().unwrap();
        view.list_events(0, 20, Some(PROMPT_KEY)).unwrap()
    });
    assert!(
        events
            .iter()
            .all(|event| event.kind != "session.answer_recommended"),
        "{events:?}"
    );
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn the_oversight_board_shows_the_recorded_pick_on_an_approval_row() {
    let payload = format!(r#"[{{"summary":{PROMPTED},"screen_tail":[],"ready":false}}]"#);
    let (dir, built, _seen) = context(
        "sweep-pick",
        person(),
        None,
        |_| {},
        vec![payload.into_bytes()],
    );
    let approval = crate::adapters::engine::EngineApproval {
        question: "Do you want to proceed?".to_string(),
        command_excerpt: "rm -rf /tmp/x".to_string(),
        detected_at: "2026-10-31T06:00:00Z".to_string(),
        deadline_seconds: None,
        deadline_at: None,
        kind: "permission".to_string(),
        options: vec![(1, "Yes".to_string(), true), (2, "No".to_string(), false)],
        key: PROMPT_KEY.to_string(),
    };
    crate::with_ctx!(&built, |ctx| {
        assert!(crate::application::session_blocked::record_recommendation(
            ctx,
            PROMPT_KEY,
            &approval,
            (Some(2), None),
            "pick",
        )
        .unwrap());
    });
    let result = super::sweep_sessions_op(&built, json!({})).unwrap();
    let row = &result["rows"][0];
    assert_eq!(row["attention"], "approval", "{result}");
    assert_eq!(row["recommendation"]["option"], json!(2), "{result}");
    assert_eq!(row["recommendation"]["label"], json!("No"));
    assert_eq!(row["recommendation"]["escalation_key"], json!(PROMPT_KEY));
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn an_unreadable_session_still_escalates_the_refusal_without_its_keys() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (dir, built) = refusing_context_reading(
        "unreadable",
        agent("agent:worker"),
        Arc::clone(&seen),
        false,
    );
    record_session(&built, "ses_1", "eng-1");
    let err = answer_as(&built, Some(2)).unwrap_err();
    let VogtError::Escalated { message, details } = &err else {
        panic!("expected an escalation, got {err:?}");
    };
    assert_eq!(details["outcome"], "escalated", "{details}");
    assert!(details.get("escalation_key").is_none(), "{details}");
    assert!(
        message.contains("escalated to a person in the Inbox"),
        "{message}"
    );
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn the_newest_pick_on_a_dialog_is_the_one_shown_and_a_long_label_still_matches() {
    use crate::application::session_blocked::{dialog_recommendation, record_recommendation};
    let (dir, built, _seen) = context("newest-pick", person(), None, |_| {}, vec![]);
    let long = "y".repeat(250);
    let approval = crate::adapters::engine::EngineApproval {
        question: "Do you want to proceed?".to_string(),
        command_excerpt: "rm -rf /tmp/x".to_string(),
        detected_at: "2026-10-31T06:00:00Z".to_string(),
        deadline_seconds: None,
        deadline_at: None,
        kind: "permission".to_string(),
        options: vec![
            (1, "Yes".to_string(), true),
            (2, "No".to_string(), false),
            (3, long.clone(), false),
        ],
        key: PROMPT_KEY.to_string(),
    };
    crate::with_ctx!(&built, |ctx| {
        record_recommendation(ctx, PROMPT_KEY, &approval, (Some(1), None), "first").unwrap();
        record_recommendation(ctx, PROMPT_KEY, &approval, (Some(2), None), "second").unwrap();
        record_recommendation(ctx, PROMPT_KEY, &approval, (Some(3), None), "third").unwrap();
    });
    // Three picks on one dialog: the newest one is read, not the oldest.
    // The third option's label is longer than the 200 characters recorded.
    let shown = crate::with_ctx!(&built, |ctx| {
        let view = ctx.declared.read().unwrap();
        dialog_recommendation(&view, "eng-1", &approval).unwrap()
    });
    let shown = shown.expect("the newest pick is shown");
    assert_eq!(shown["option"], json!(3), "{shown}");
    assert_eq!(shown["label"], json!("y".repeat(200)));
    std::fs::remove_dir_all(dir).ok();
}
