use super::*;

fn hibernated(id: &str) -> Vec<u8> {
    format!(
        r#"{{"id":"{id}","alive":false,"activity":"hibernated","role":"worker","conversation":{{"id":"conv-1","agent":"claude"}}}}"#
    )
    .into_bytes()
}

#[test]
fn a_refused_wake_says_why_and_what_to_call() {
    assert!(not_ready_next_step("blocked").contains("session_screen"));
    assert!(not_ready_next_step("awaiting-approval").contains("session_answer"));
    assert!(not_ready_next_step("timeout").contains("session_screen"));
}

#[test]
fn hibernate_revokes_the_sessions_tokens() {
    let (_dir, built, seen) = context("hibernate", person(), None, |_| {}, vec![live("eng-1")]);
    let old = record_session(&built, "ses_1", "eng-1");
    with_ctx!(&built, |ctx| {
        hibernate(
            ctx,
            &HibernateParams {
                id: "ses_1".to_string(),
                reason: "freeing memory".to_string(),
                allow_shell: false,
                cold_wake_opt_out: None,
                superseded_by: None,
            },
        )
        .unwrap();
    });
    let tokens = tokens_of(&built, "local:ada");
    let revoked = tokens.iter().find(|token| token.id == old).unwrap();
    assert_eq!(
        revoked.revoked_reason.as_deref(),
        Some("hibernated: freeing memory")
    );
    assert!(revoked.revoked_at.is_some());
    assert!(seen
        .lock()
        .unwrap()
        .iter()
        .any(|(_, path, _)| path.ends_with("/hibernate")));
}

fn hibernate_with_successor(successor: &str) -> (Result<(), VogtError>, Vec<String>) {
    let (_dir, built, seen) = context(
        "hibernate_successor",
        person(),
        None,
        |_| {},
        vec![live("eng-1")],
    );
    record_session(&built, "ses_1", "eng-1");
    with_ctx!(&built, |ctx| {
        let mut write = write_of(ctx);
        audited_write(&mut write, "session.start", "test", |txn, actor| {
            txn.insert_session(&CodingSession {
                id: "ses_2".to_string(),
                engine_session_id: "eng-2".to_string(),
                project_id: "prj_1".to_string(),
                work_item_id: None,
                actor_id: actor.id.clone(),
                cwd: "/work".to_string(),
                template: None,
                model: None,
                effort: None,
                reason: "test".to_string(),
                purpose: None,
                started_by_session: None,
                started_by_kind: None,
                started_at: Moment::from_unix(1_700_000_000, 0),
                stopped_at: None,
            })?;
            Ok(WriteOutcome::new(
                (),
                "session",
                "ses_2",
                json!({}),
                "session.started",
                json!({}),
            ))
        })
        .unwrap();
    });
    let outcome = with_ctx!(&built, |ctx| hibernate(
        ctx,
        &HibernateParams {
            id: "ses_1".to_string(),
            reason: "handing on".to_string(),
            allow_shell: false,
            cold_wake_opt_out: None,
            superseded_by: Some(successor.to_string()),
        },
    )
    .map(|_| ()));
    let bodies = seen
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, path, _)| path.ends_with("/hibernate"))
        .map(|(_, _, body)| body.clone())
        .collect();
    (outcome, bodies)
}

#[test]
fn hibernate_sends_the_successors_engine_id_for_a_ses_id() {
    let (outcome, bodies) = hibernate_with_successor("ses_2");
    outcome.unwrap();
    assert_eq!(bodies.len(), 1, "{bodies:?}");
    assert!(
        bodies[0].contains(r#""superseded_by":"eng-2""#),
        "{bodies:?}"
    );
}

#[test]
fn hibernate_passes_a_successor_engine_uuid_through() {
    let (outcome, bodies) = hibernate_with_successor("eng-2");
    outcome.unwrap();
    assert!(
        bodies[0].contains(r#""superseded_by":"eng-2""#),
        "{bodies:?}"
    );
}

#[test]
fn hibernate_refuses_an_unknown_successor_before_the_engine_call() {
    let (outcome, bodies) = hibernate_with_successor("ses_nope");
    match outcome {
        Err(VogtError::NotFound(message)) => assert!(message.contains("ses_nope")),
        other => panic!("{other:?}"),
    }
    assert!(bodies.is_empty(), "{bodies:?}");
}

#[test]
fn wake_mints_a_token_and_revokes_the_older_ones() {
    let (_dir, built, seen) = context(
        "wake",
        person(),
        None,
        |_| {},
        vec![hibernated("eng-1"), hibernated("eng-1")],
    );
    let old = record_session(&built, "ses_1", "eng-1");
    with_ctx!(&built, |ctx| {
        wake(
            ctx,
            &WakeParams {
                id: "ses_1".to_string(),
                reason: "back to work".to_string(),
                force: false,
            },
        )
        .unwrap();
    });
    let tokens = tokens_of(&built, "local:ada");
    let revoked = tokens.iter().find(|token| token.id == old).unwrap();
    assert_eq!(
        revoked.revoked_reason.as_deref(),
        Some("superseded on wake: back to work")
    );
    let minted: Vec<_> = tokens
        .iter()
        .filter(|token| token.revoked_at.is_none())
        .collect();
    assert_eq!(minted.len(), 1, "{tokens:?}");
    assert!(minted[0].name.contains("ses_1"));
    let calls = seen.lock().unwrap().clone();
    let wake_call = calls
        .iter()
        .find(|(_, path, _)| path.ends_with("/wake"))
        .unwrap();
    assert!(wake_call.2.contains("VOGT_HTTP_TOKEN"), "{}", wake_call.2);
    assert!(wake_call.2.contains("VOGT_SESSION_ID"), "{}", wake_call.2);
}

/// A hibernated session that has sat past its cache lifetime holding a
/// large context, as the engine reports it.
fn cold_and_large(id: &str) -> Vec<u8> {
    format!(
        r#"{{"id":"{id}","alive":false,"activity":"hibernated","role":"worker","conversation":{{"id":"conv-1","agent":"claude"}},"hibernation":{{"at":"2023-11-14T00:00:00Z","trigger":"idle","resumable":true}},"economics":{{"context_tokens":180000}}}}"#
    )
    .into_bytes()
}

fn wake_body(seen: &Mutex<Vec<(String, String, String)>>) -> String {
    let calls = seen.lock().unwrap().clone();
    calls
        .iter()
        .find(|(_, path, _)| path.ends_with("/wake"))
        .map(|call| call.2.clone())
        .expect("a wake reached the engine")
}

fn wake_it(built: &crate::application::context::Built) {
    with_ctx!(built, |ctx| {
        wake(
            ctx,
            &WakeParams {
                id: "ses_1".to_string(),
                reason: "back to work".to_string(),
                force: false,
            },
        )
        .unwrap();
    });
}

#[test]
fn a_cold_wake_past_the_threshold_starts_from_a_summary() {
    let (_dir, built, seen) = context(
        "cold-wake",
        person(),
        None,
        |config| config.cold_wake_compaction = true,
        (0..6).map(|_| cold_and_large("eng-1")).collect(),
    );
    record_session(&built, "ses_1", "eng-1");
    wake_it(&built);
    let body = wake_body(&seen);
    assert!(body.contains("fresh_prompt"), "{body}");
    assert!(
        body.contains("Resumed from a summary (cold wake)"),
        "{body}"
    );
}

#[test]
fn a_cold_wake_read_error_falls_back_to_a_plain_resume() {
    let failed = Err(VogtError::NotFound("storage read failed".to_string()));
    let (decision, fresh_prompt) = super::plain_resume_on_read_error(failed, "eng-1");
    assert!(!decision.compact());
    assert_eq!(decision.audit()["outcome"], "read_failed");
    assert!(fresh_prompt.is_none());
}

#[test]
fn a_wake_resumes_the_transcript_when_compaction_is_off() {
    let (_dir, built, seen) = context(
        "cold-wake-off",
        person(),
        None,
        |_| {},
        (0..6).map(|_| cold_and_large("eng-1")).collect(),
    );
    record_session(&built, "ses_1", "eng-1");
    wake_it(&built);
    assert!(!wake_body(&seen).contains("fresh_prompt"));
}

#[test]
fn an_opted_out_session_resumes_even_when_compaction_is_on() {
    let (_dir, built, seen) = context(
        "cold-wake-opt-out",
        person(),
        None,
        |config| config.cold_wake_compaction = true,
        (0..6).map(|_| cold_and_large("eng-1")).collect(),
    );
    record_session(&built, "ses_1", "eng-1");
    with_ctx!(&built, |ctx| {
        let mut write = write_of(ctx);
        audited_write(&mut write, "session.hibernate", "test", |txn, _actor| {
            txn.set_session_cold_wake_opt_out("ses_1", true)?;
            Ok(WriteOutcome::new(
                (),
                "session",
                "ses_1",
                json!({}),
                "session.hibernated",
                json!({}),
            ))
        })
        .unwrap();
    });
    wake_it(&built);
    assert!(!wake_body(&seen).contains("fresh_prompt"));
}

#[test]
fn waking_a_stopped_session_is_refused_and_revokes_nothing() {
    let (_dir, built, _) = context("stopped", person(), None, |_| {}, vec![hibernated("eng-1")]);
    let old = record_session(&built, "ses_1", "eng-1");
    // Marked stopped directly, not through session.stop: stopping revokes
    // the token itself, and this test is about wake refusing before it
    // touches tokens.
    with_ctx!(&built, |ctx| {
        let mut write = write_of(ctx);
        audited_write(&mut write, "session.stop", "test", |txn, _actor| {
            txn.set_session_stopped("ses_1", Moment::from_unix(1_700_000_001, 0))?;
            Ok(WriteOutcome::new(
                (),
                "session",
                "ses_1",
                json!({}),
                "session.stopped",
                json!({}),
            ))
        })
        .unwrap();
        let error = wake(
            ctx,
            &WakeParams {
                id: "ses_1".to_string(),
                reason: "no".to_string(),
                force: false,
            },
        )
        .unwrap_err();
        assert!(matches!(error, VogtError::Conflict(_)), "{error:?}");
    });
    let tokens = tokens_of(&built, "local:ada");
    assert!(tokens
        .iter()
        .find(|token| token.id == old)
        .unwrap()
        .revoked_at
        .is_none());
}
