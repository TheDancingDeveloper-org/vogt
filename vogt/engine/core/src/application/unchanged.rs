//! The `since` / `cursor` pair on `session.last_reply` and `session.digest`.
//!
//! The cursor is a short digest of the answer's body. A caller that passes it
//! back as `since` and finds the body unchanged gets `{unchanged: true,
//! cursor}` instead of the body again, so a re-read costs a few tokens.

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

/// The cursor for `body`: sixteen hex digits of its SHA-256.
fn cursor_of(body: &Value) -> String {
    let hash = Sha256::digest(body.to_string().as_bytes());
    hash[..8].iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Stamp `body` with its cursor, or cut it to the unchanged answer when
/// `since` names that cursor. `keep` are the identity fields the short answer
/// still carries.
pub(crate) fn stamp(mut body: Value, since: Option<&str>, keep: &[&str]) -> Value {
    let cursor = cursor_of(&body);
    if since.is_some_and(|since| since.trim() == cursor) {
        let mut short = Map::new();
        for name in keep {
            if let Some(value) = body.get(*name) {
                short.insert((*name).to_string(), value.clone());
            }
        }
        short.insert("unchanged".to_string(), json!(true));
        short.insert("cursor".to_string(), json!(cursor));
        return Value::Object(short);
    }
    if let Value::Object(object) = &mut body {
        object.insert("unchanged".to_string(), json!(false));
        object.insert("cursor".to_string(), json!(cursor));
    }
    body
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::stamp;

    #[test]
    fn a_matching_since_cuts_the_body() {
        let body = json!({"id": "ses_1", "activity": "idle", "turns": 3});
        let first = stamp(body.clone(), None, &["id"]);
        assert_eq!(first["unchanged"], false);
        let cursor = first["cursor"].as_str().unwrap().to_string();
        let again = stamp(body, Some(&cursor), &["id"]);
        assert_eq!(
            again,
            json!({"id": "ses_1", "unchanged": true, "cursor": cursor})
        );
    }

    #[test]
    fn a_changed_body_or_stale_since_returns_everything() {
        let before = stamp(json!({"id": "a", "turns": 3}), None, &["id"]);
        let stale = before["cursor"].as_str().unwrap();
        let after = stamp(json!({"id": "a", "turns": 4}), Some(stale), &["id"]);
        assert_eq!(after["unchanged"], false);
        assert_eq!(after["turns"], 4);
        assert_ne!(after["cursor"], before["cursor"]);
    }
}
