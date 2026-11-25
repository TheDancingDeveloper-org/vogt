use super::*;

fn renamed(id: &str, name: &str) -> Vec<u8> {
    format!(r#"{{"id":"{id}","alive":true,"activity":"idle","role":"worker","name":"{name}"}}"#)
        .into_bytes()
}

#[test]
fn renaming_a_session_asks_the_engine_and_returns_the_new_name() {
    let (_dir, built, seen) = context(
        "rename",
        person(),
        None,
        |_| {},
        vec![renamed("eng-1", "after"), renamed("eng-1", "after")],
    );
    record_session(&built, "ses_1", "eng-1");
    let result = rename_session_op(
        &built,
        json!({"id": "ses_1", "name": "after", "reason": "clearer"}),
    )
    .unwrap();
    let calls = seen.lock().unwrap();
    assert!(calls.iter().any(|(method, path, body)| {
        method == "PATCH" && path.ends_with("/eng-1") && body.contains("\"name\":\"after\"")
    }));
    assert_eq!(result["session"]["id"], "ses_1");
}

#[test]
fn renaming_with_an_empty_name_is_refused_before_the_engine() {
    let (_dir, built, seen) = context("rename-empty", person(), None, |_| {}, vec![]);
    record_session(&built, "ses_1", "eng-1");
    let error = rename_session_op(
        &built,
        json!({"id": "ses_1", "name": "   ", "reason": "no"}),
    )
    .unwrap_err();
    assert!(matches!(error, VogtError::InvalidRequest(_)), "{error:?}");
    assert!(seen.lock().unwrap().is_empty());
}

fn live_session(id: &str) -> Vec<u8> {
    format!(r#"{{"id":"{id}","alive":true,"activity":"idle","role":"worker"}}"#).into_bytes()
}

#[test]
fn removing_a_live_session_kills_it_with_reason_before_forgetting_it() {
    let (_dir, built, seen) = context(
        "remove",
        person(),
        None,
        |_| {},
        vec![live_session("eng-1"), live_session("eng-1")],
    );
    let old = record_session(&built, "ses_1", "eng-1");
    let result = remove_session_op(&built, json!({"id": "ses_1", "reason": "done"})).unwrap();
    assert_eq!(result["session"]["id"], "ses_1");
    assert!(result["session"]["stopped_at"].is_string());
    let calls = seen.lock().unwrap().clone();
    let kill = calls
        .iter()
        .position(|(method, path, body)| {
            method == "POST"
                && path.ends_with("/eng-1/kill")
                && body.contains("\"reason\":\"done\"")
                && body.contains("\"by\":\"local:ada\"")
        })
        .expect("kill with reason");
    let delete = calls
        .iter()
        .position(|(method, path, _)| method == "DELETE" && path.ends_with("/eng-1"))
        .expect("delete");
    assert!(kill < delete, "killed before forgotten: {calls:?}");
    let tokens = tokens_of(&built, "local:ada");
    assert!(tokens
        .iter()
        .find(|token| token.id == old)
        .unwrap()
        .revoked_at
        .is_some());
}

#[test]
fn removing_an_unknown_unlinked_session_is_not_found() {
    let (_dir, built, _) = context(
        "remove-missing",
        person(),
        None,
        |_| {},
        vec![b"null".to_vec()],
    );
    let error = remove_session_op(&built, json!({"id": "eng-9", "reason": "gone"})).unwrap_err();
    assert!(matches!(error, VogtError::NotFound(_)), "{error:?}");
}

#[test]
fn removing_an_unlinked_live_session_kills_then_forgets_it() {
    let (_dir, built, seen) = context(
        "remove-unlinked",
        person(),
        None,
        |_| {},
        vec![live_session("eng-9"), live_session("eng-9")],
    );
    let result = remove_session_op(&built, json!({"id": "eng-9", "reason": "gone"})).unwrap();
    assert_eq!(result["session"]["linked"], false);
    assert_eq!(result["session"]["alive"], false);
    let calls = seen.lock().unwrap().clone();
    let kill = calls
        .iter()
        .position(|(method, path, _)| method == "POST" && path.ends_with("/eng-9/kill"))
        .unwrap();
    let delete = calls
        .iter()
        .position(|(method, path, _)| method == "DELETE" && path.ends_with("/eng-9"))
        .unwrap();
    assert!(kill < delete);
}

const ENGINE_UUID: &str = "0cc46582-ed57-4327-9a28-624a7ae2474a";

#[test]
fn only_the_engine_may_mint_a_session_token() {
    let (_dir, built, _) = context("token-refused", person(), None, |_| {}, vec![]);
    let error = session_token_op(
        &built,
        json!({"engine_session_id": ENGINE_UUID, "reason": "rotated"}),
    )
    .unwrap_err();
    assert!(
        matches!(error, VogtError::EngineOnly(_)),
        "a person is not the engine: {error:?}"
    );
    let (_dir, built, _) = context(
        "token-agent",
        agent(&format!("agent:engine:{ENGINE_UUID}")),
        None,
        |_| {},
        vec![],
    );
    let error = session_token_op(
        &built,
        json!({"engine_session_id": ENGINE_UUID, "reason": "rotated"}),
    )
    .unwrap_err();
    assert!(
        matches!(error, VogtError::EngineOnly(_)),
        "the session's own actor is not the engine: {error:?}"
    );
}

#[test]
fn the_engine_mints_an_agent_token_and_a_second_mint_supersedes_it() {
    let (_dir, built, _) = context(
        "token-engine",
        agent("agent:vogt-engine"),
        None,
        |_| {},
        vec![],
    );
    let first = session_token_op(
        &built,
        json!({"engine_session_id": ENGINE_UUID, "reason": "launch"}),
    )
    .unwrap();
    assert_eq!(first["actor"], format!("agent:engine:{ENGINE_UUID}"));
    assert_eq!(first["revoked"], 0);
    assert!(first["token"].as_str().unwrap().starts_with("vogt_"));
    let second = session_token_op(
        &built,
        json!({"engine_session_id": ENGINE_UUID, "reason": "wake"}),
    )
    .unwrap();
    assert_eq!(second["revoked"], 1);
    assert_ne!(second["token"], first["token"]);
    let gone = session_token_op(
        &built,
        json!({"engine_session_id": ENGINE_UUID, "revoke": true, "reason": "stop"}),
    )
    .unwrap();
    assert!(
        gone["token"].is_null(),
        "a revoke returns token: null, {gone}"
    );
    assert_eq!(gone["revoked"], 1);
    with_ctx!(&built, |ctx| {
        let view = ctx.declared.read().unwrap();
        let holder = view
            .actor_by_identity(&format!("agent:engine:{ENGINE_UUID}"))
            .unwrap()
            .unwrap();
        assert_eq!(
            holder.display_name,
            format!("Engine session {}", &ENGINE_UUID[..8])
        );
        let records = view
            .list_audit(&crate::storage::interface::AuditQuery {
                limit: 20,
                offset: 0,
                actor_id: None,
                operation: Some("session.token".to_string()),
                entity_id: Some(holder.id.clone()),
                project_id: None,
                since: None,
                until: None,
            })
            .unwrap();
        assert!(
            records.iter().any(|row| row.entity_kind == "actor"),
            "the audit row is the actor, not the session: {records:?}"
        );
        let events = view.list_events(0, 50, Some(&holder.id)).unwrap();
        let revoked = events
            .iter()
            .find(|event| event.kind == "session.token_revoked")
            .expect("a revoke publishes session.token_revoked");
        assert_eq!(revoked.summary["minted"], false);
        assert_eq!(revoked.summary["revoked"], 1);
        let issued = events
            .iter()
            .find(|event| event.kind == "session.token_issued")
            .expect("a mint publishes session.token_issued");
        assert_eq!(issued.summary["minted"], true);
        assert_eq!(issued.summary["engine_session_id"], ENGINE_UUID);
    });
    let tokens = tokens_of(&built, &format!("agent:engine:{ENGINE_UUID}"));
    assert!(tokens.iter().all(|token| token.revoked_at.is_some()));
}

#[test]
fn a_session_id_that_is_not_a_uuid_is_refused() {
    let (_dir, built, _) = context(
        "token-shape",
        agent("agent:vogt-engine"),
        None,
        |_| {},
        vec![],
    );
    let error = session_token_op(
        &built,
        json!({"engine_session_id": "../../etc", "reason": "no"}),
    )
    .unwrap_err();
    assert!(matches!(error, VogtError::InvalidRequest(_)), "{error:?}");
}
