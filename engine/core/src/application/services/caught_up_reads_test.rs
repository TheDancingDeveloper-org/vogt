//! charter.get and watch.list, ported as reads (WI-2657, gap C).
//!
//! Empty tables answer with an empty current and an empty list, not 501.
//! Rows are seeded through SQL because the write operations stay unported.

use std::path::{Path, PathBuf};

use rusqlite::params;
use serde_json::json;

use crate::application::context::{build_context, Built};
use crate::application::services::{charters, watches};
use crate::config::VogtConfig;
use crate::core::{ActorKind, Moment, Principal, SequentialIds, StepClock};
use crate::errors::VogtError;
use crate::storage::interface::DeclaredStore;

fn person() -> Principal {
    Principal::new("local:test-user", ActorKind::Human, "Test").unwrap()
}

fn agent(identity: &str) -> Principal {
    Principal::new(identity, ActorKind::Agent, identity).unwrap()
}
use crate::storage::sqlite::connection::connect_with;

fn start() -> Moment {
    Moment::from_unix(1_700_000_000, 0)
}

fn unique_dir() -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("vogt-caught-up-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn open_as(dir: &Path, principal: Principal) -> Built {
    let config = VogtConfig {
        data_dir: dir.to_path_buf(),
        ..VogtConfig::default()
    };
    let built = build_context(
        config,
        Some(principal),
        Some(StepClock::new(start())),
        Some(SequentialIds::new(Some(dir.join("ids.json"))).unwrap()),
        None,
        None,
        None,
        None,
    )
    .unwrap();
    match &built {
        Built::StepSequential(ctx) => {
            ctx.declared.migrate().unwrap();
        }
        _other => panic!("expected a step clock"),
    }
    built
}

fn open(dir: &Path) -> Built {
    open_as(dir, person())
}

fn store() -> (PathBuf, Built) {
    let dir = unique_dir();
    let built = open(&dir);
    match &built {
        Built::StepSequential(ctx) => {
            let _ = ctx.declared.bootstrap(&person()).unwrap();
        }
        _other => panic!("expected a step clock"),
    }
    (dir, built)
}

fn db(dir: &Path) -> rusqlite::Connection {
    connect_with(&dir.join("declared.sqlite3"), false, "FULL").unwrap()
}

fn actor_id(conn: &rusqlite::Connection) -> String {
    conn.query_row("SELECT id FROM actors LIMIT 1", [], |row| row.get(0))
        .unwrap()
}

#[test]
fn charter_get_is_empty_until_one_is_set() {
    let (dir, built) = store();
    let result = charters::charter_get_op(&built, json!({"scope": "workspace"})).unwrap();
    assert_eq!(result, json!({"current": null, "history": []}));

    let conn = db(&dir);
    let author = actor_id(&conn);
    conn.execute(
        "INSERT INTO charters (scope_kind, scope_id, version, body, author_actor_id, created_at, reason)
         VALUES ('workspace', '', 1, 'be brief', ?, '2023-11-14T22:13:20+00:00', 'first')",
        params![author],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO charters (scope_kind, scope_id, version, body, author_actor_id, created_at, reason)
         VALUES ('workspace', '', 2, '', ?, '2023-11-14T22:13:21+00:00', 'cleared')",
        params![author],
    )
    .unwrap();
    drop(conn);

    let current = charters::charter_get_op(&built, json!({"scope": "workspace"})).unwrap();
    assert!(current["history"].as_array().unwrap().is_empty());
    assert_eq!(current["current"]["version"], 2);
    assert_eq!(current["current"]["body"], "");
    assert_eq!(current["current"]["author"], "local:test-user");
    assert!(current["current"]["ref"].is_null());

    let with_history =
        charters::charter_get_op(&built, json!({"scope": "workspace", "history": true})).unwrap();
    let versions: Vec<i64> = with_history["history"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["version"].as_i64().unwrap())
        .collect();
    assert_eq!(versions, vec![2, 1]);
}

#[test]
fn charter_get_rejects_a_workspace_ref_and_a_missing_project() {
    let (_dir, built) = store();
    let refused = charters::charter_get_op(&built, json!({"scope": "workspace", "ref": "vogt"}));
    assert!(
        matches!(refused, Err(VogtError::InvalidRequest(message)) if message.contains("takes no ref"))
    );
    let missing = charters::charter_get_op(&built, json!({"scope": "project", "ref": "nope"}));
    assert!(matches!(missing, Err(VogtError::NotFound(_))));
}

#[test]
fn watch_list_is_empty_and_an_active_watch_past_expiry_reads_expired() {
    let (dir, built) = store();
    let empty = watches::watch_list_op(&built, json!({})).unwrap();
    assert_eq!(empty, json!({"watches": []}));

    let conn = db(&dir);
    let owner = actor_id(&conn);
    conn.execute(
        "INSERT INTO watches (id, owner_actor_id, target_session, kind, trigger_json, note, state,
                              created_at, expires_at, attempts, reason)
         VALUES ('wat_1', ?, 'eng-1', 'pr', ?, '', 'active',
                 '2023-11-14T22:13:20+00:00', '2023-11-14T22:13:19+00:00', 0, 'wait')",
        params![owner, r#"{"number": 7, "project": "vogt"}"#],
    )
    .unwrap();
    drop(conn);

    let listed = watches::watch_list_op(&built, json!({})).unwrap();
    let watch = &listed["watches"][0];
    assert_eq!(watch["state"], "expired");
    assert_eq!(watch["waits_for"], "PR #7 in vogt");
    assert_eq!(watch["owner"], "local:test-user");
    assert!(watch["delivered"].is_null());

    let only_active = watches::watch_list_op(&built, json!({"state": "active"})).unwrap();
    assert_eq!(only_active["watches"].as_array().unwrap().len(), 0);
    let expired = watches::watch_list_op(&built, json!({"state": "expired"})).unwrap();
    assert_eq!(expired["watches"].as_array().unwrap().len(), 1);

    let missing = watches::watch_list_op(&built, json!({"session": "ses_missing"}));
    assert!(matches!(missing, Err(VogtError::NotFound(_))));
    let raw = watches::watch_list_op(&built, json!({"session": "eng-1"})).unwrap();
    assert_eq!(raw["watches"].as_array().unwrap().len(), 1);
}

#[test]
fn charter_set_by_a_person_increments_and_a_stale_version_conflicts() {
    let (dir, built) = store();
    let first = charters::charter_set_op(
        &built,
        json!({"scope": "workspace", "body": "Stop at push.", "expected_version": 0, "reason": "first"}),
    )
    .unwrap();
    assert_eq!(first["charter"]["version"], 1);
    assert_eq!(first["charter"]["body"], "Stop at push.");

    let stale = charters::charter_set_op(
        &built,
        json!({"scope": "workspace", "body": "Other.", "expected_version": 0, "reason": "stale"}),
    );
    assert!(
        matches!(stale, Err(VogtError::CharterVersionConflict(message))
            if message.contains("version 1, not 0"))
    );

    let second = charters::charter_set_op(
        &built,
        json!({"scope": "workspace", "body": "Other.", "expected_version": 1, "reason": "second"}),
    )
    .unwrap();
    assert_eq!(second["charter"]["version"], 2);

    let current = charters::charter_get_op(&built, json!({"scope": "workspace"})).unwrap();
    assert_eq!(current["current"]["body"], "Other.");
    assert_eq!(current["current"]["version"], 2);
    let _ = dir;
}

#[test]
fn only_a_person_sets_the_workspace_charter() {
    let (dir, _person_built) = store();
    let built = open_as(&dir, agent("agent:engine:eng-worker"));
    let refused = charters::charter_set_op(
        &built,
        json!({"scope": "workspace", "body": "from an agent", "reason": "agent"}),
    );
    assert!(matches!(refused, Err(VogtError::PersonRequired(message))
            if message.contains("only a person")));
    let still = charters::charter_get_op(&built, json!({"scope": "workspace"})).unwrap();
    assert!(still["current"].is_null());
}

#[test]
fn watch_create_and_cancel_round_trip_and_a_stranger_cannot_cancel() {
    let (dir, built) = store();
    let created = watches::watch_create_op(
        &built,
        json!({
            "session": "eng-target",
            "trigger": {"kind": "session", "session": "other-session", "until": "ended"},
            "reason": "wait",
        }),
    )
    .unwrap();
    let id = created["watch"]["id"].as_str().unwrap().to_string();
    assert_eq!(created["watch"]["state"], "active");
    assert_eq!(created["watch"]["kind"], "session");

    let listed = watches::watch_list_op(&built, json!({})).unwrap();
    assert_eq!(listed["watches"].as_array().unwrap().len(), 1);

    let stranger = open_as(&dir, agent("agent:engine:somebody-else"));
    let refused = watches::watch_cancel_op(&stranger, json!({"id": id, "reason": "not mine"}));
    assert!(matches!(refused, Err(VogtError::GrantRefused(_))));

    let cancelled = watches::watch_cancel_op(&built, json!({"id": id, "reason": "done"})).unwrap();
    assert_eq!(cancelled["watch"]["state"], "cancelled");

    let again = watches::watch_cancel_op(&built, json!({"id": id, "reason": "again"}));
    assert!(matches!(again, Err(VogtError::Conflict(message)) if message.contains("not active")));
}

#[test]
fn a_session_cannot_wait_for_itself() {
    let (_dir, built) = store();
    let refused = watches::watch_create_op(
        &built,
        json!({
            "session": "eng-target",
            "trigger": {"kind": "session", "session": "eng-target", "until": "ended"},
            "reason": "self",
        }),
    );
    assert!(matches!(refused, Err(VogtError::InvalidRequest(message))
            if message.contains("cannot wait for itself")));
}
