use super::*;

#[test]
fn decode_messages_are_unchanged() {
    fn message<P: serde::de::DeserializeOwned>(params: Value) -> String {
        match crate::application::typed::decode::<P>(params) {
            Err(error) => error.message().to_string(),
            Ok(_) => "accepted".to_string(),
        }
    }
    assert_eq!(
        message::<super::StartRequest>(json!({})),
        "reason is required"
    );
    assert_eq!(
        message::<super::StartRequest>(json!({"reason": "r", "autopilot": "yes"})),
        "autopilot must be a boolean"
    );
    assert_eq!(
        message::<super::StartRequest>(json!({"reason": "r", "mcp_set": "x"})),
        "mcp_set must be full or worker, not \"x\""
    );
    assert_eq!(
        message::<super::StartRequest>(json!({"reason": "r", "override": ""})),
        "override must not be empty"
    );
    assert_eq!(
        message::<super::StopSessionParams>(json!({"reason": "r"})),
        "a session id is required (ses_… or the engine's session UUID)"
    );
    assert_eq!(
        message::<super::StopSessionParams>(json!({"id": "s"})),
        "reason is required"
    );
    assert_eq!(
        message::<super::ListRequest>(json!({"limit": "x"})),
        "limit must be an integer"
    );
    assert_eq!(
        message::<super::ListRequest>(json!({"starters_only": true})),
        "starters_only needs mode=full, fields naming only engine_session_id, started_by_session and work_item, order=started, and no states, include_hidden or started_by_session"
    );
    assert_eq!(
        message::<super::TemplatesParams>(json!(null)),
        "parameters must be an object"
    );
    assert_eq!(message::<super::TemplatesParams>(json!({})), "accepted");
    assert_eq!(
        message::<super::KeepAwakeParams>(json!({"id": "s", "reason": "r"})),
        "keep_awake is required"
    );
    assert_eq!(
        message::<super::AnswerParams>(json!({"id": "s", "reason": "r", "option": "1"})),
        "option must be an integer"
    );
    assert_eq!(
        message::<super::SweepSessionsParams>(json!({"exclude_roles": [1]})),
        "exclude_roles must be a list of strings"
    );
    assert_eq!(
        message::<super::SessionTokenParams>(json!({"engine_session_id": "nope", "reason": "r"})),
        "engine_session_id must be a UUID"
    );
    assert_eq!(
        message::<super::RenameSessionParams>(json!({"id": "s", "reason": "r"})),
        "name is required"
    );
    assert_eq!(
        message::<super::RemoveSessionParams>(json!({"id": "s"})),
        "reason is required"
    );
}
