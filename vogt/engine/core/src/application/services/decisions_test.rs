//! The decision service, ported from `tests/test_decisions.py` (WI-1321).
//!
//! Each test opens one data directory as a person, then reopens it as the
//! principal it needs: an agent session, a second agent, or a later clock.
//! Nothing reaches an engine, so no stand-in is needed.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::application::context::{build_context, Built};
use crate::application::services::{decisions, history, inbox};
use crate::config::VogtConfig;
use crate::core::{ActorKind, Decision, Moment, Principal, SequentialIds, StepClock};
use crate::errors::VogtError;
use crate::storage::interface::{DeclaredStore, ObservedStore, ReadView};

const SESSION: &str = "agent:engine:eng-overseer";
const WORKER: &str = "agent:engine:eng-worker";
const POD: &str = "agent:pod:vogt";
const SESSION_REF: &str = "agent:session:ses_x";
const WHY: &str = "decision test";

fn start() -> Moment {
    Moment::from_unix(1_700_000_000, 0)
}

fn unique_dir() -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("vogt-decisions-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn person() -> Principal {
    Principal::new("local:test-user", ActorKind::Human, "Test").unwrap()
}

fn agent(identity: &str) -> Principal {
    Principal::new(identity, ActorKind::Agent, identity).unwrap()
}

fn open(dir: &Path, principal: Principal, at: Moment) -> Built {
    let config = VogtConfig {
        data_dir: dir.to_path_buf(),
        ..VogtConfig::default()
    };
    build_context(
        config,
        Some(principal),
        Some(StepClock::new(at)),
        // Persisted, so a reopened directory keeps counting from its last id.
        Some(SequentialIds::new(Some(dir.join("ids.json"))).unwrap()),
        None,
        None,
        None,
        None,
    )
    .unwrap()
}

/// A migrated, bootstrapped store in a fresh directory, opened as a person.
fn store() -> (PathBuf, Built) {
    let dir = unique_dir();
    let built = open(&dir, person(), start());
    match &built {
        Built::StepSequential(ctx) => {
            ctx.declared.migrate().unwrap();
            ctx.declared.bootstrap(&person()).unwrap();
            ctx.observed.migrate().unwrap();
        }
        _other => panic!("expected a step clock"),
    }
    (dir, built)
}

/// A context for one principal, opened on the directory at its start clock.
/// Each action opens its own: a context keeps its id counter in memory, so a
/// second context over the same directory would draw ids the first has used.
fn me(dir: &Path) -> Built {
    open(dir, person(), start())
}

fn as_agent(dir: &Path, identity: &str) -> Built {
    open(dir, agent(identity), start())
}

fn options() -> Value {
    json!([
        {"label": "Merge", "description": "land it now", "recommended": true},
        {"label": "Hold", "description": "wait for the review"},
    ])
}

fn with(mut base: Value, extra: Value) -> Value {
    if let (Some(base), Some(extra)) = (base.as_object_mut(), extra.as_object()) {
        for (key, value) in extra {
            base.insert(key.clone(), value.clone());
        }
    }
    base
}

fn ask_with(built: &Built, extra: Value) -> Result<String, VogtError> {
    let params = with(
        json!({
            "question": "Merge the branch?",
            "options": options(),
            "context": "CI is green.",
            "reason": "the branch is reviewed",
        }),
        extra,
    );
    let result = decisions::ask_decision_op(built, params)?;
    Ok(result["decision"]["id"].as_str().unwrap().to_string())
}

fn ask(built: &Built) -> String {
    ask_with(built, json!({})).unwrap()
}

fn answer_as(built: &Built, id: &str, label: &str) -> Result<Value, VogtError> {
    decisions::answer_decision_op(
        built,
        json!({"id": id, "label": label, "note": "ok", "reason": WHY}),
    )
}

fn withdraw_as(built: &Built, id: &str) -> Result<Value, VogtError> {
    decisions::withdraw_decision_op(built, json!({"id": id, "reason": WHY}))
}

fn list(built: &Built, extra: Value) -> Vec<Value> {
    decisions::list_decisions_op(built, extra).unwrap()["decisions"]
        .as_array()
        .unwrap()
        .clone()
}

/// The stored row, read straight from the store.
fn stored(built: &Built, id: &str) -> Decision {
    let Built::StepSequential(ctx) = built else {
        unreachable!("store() builds a step clock");
    };
    ctx.declared.read().unwrap().decision(id).unwrap().unwrap()
}

fn inbox_entries(built: &Built) -> Vec<Value> {
    inbox::inbox_list_op(built, json!({})).unwrap()["entries"]
        .as_array()
        .unwrap()
        .clone()
}

fn pending_entries(built: &Built) -> Vec<Value> {
    inbox_entries(built)
        .into_iter()
        .filter(|entry| entry["kind"] == "decision.pending")
        .collect()
}

#[test]
fn an_agent_asks_and_its_session_comes_from_the_token() {
    let (dir, _person) = store();
    let asker = as_agent(&dir, SESSION);
    let id = ask_with(
        &asker,
        json!({"question": "Merge the branch?", "options": options(), "reason": "reviewed"}),
    )
    .unwrap();
    assert!(id.starts_with("dec"), "{id}");
    let row = stored(&asker, &id);
    assert_eq!(row.asker_session.as_deref(), Some("eng-overseer"));
    assert_eq!(
        row.expires_at.unix_seconds() - row.created_at.unix_seconds(),
        86_400
    );
    assert!(row.answer.is_none());
    let view = decisions::list_decisions_op(&asker, json!({})).unwrap();
    assert_eq!(view["decisions"][0]["state"], "pending");
    assert_eq!(view["decisions"][0]["asker"], SESSION);
}

#[test]
fn no_agent_answers_only_a_person() {
    let (dir, _person) = store();
    let id = ask(&as_agent(&dir, SESSION));
    for identity in [SESSION, SESSION_REF, POD] {
        let refused = answer_as(&as_agent(&dir, identity), &id, "Merge");
        assert!(
            matches!(&refused, Err(VogtError::PersonRequired(message)) if message.starts_with("only a person answers")),
            "{identity}: {refused:?}"
        );
    }
    assert_eq!(list(&me(&dir), json!({"state": "pending"})).len(), 1);
    answer_as(&me(&dir), &id, "Merge").unwrap();
    let answered = list(&me(&dir), json!({"state": "answered"}));
    assert_eq!(answered.len(), 1);
    assert_eq!(answered[0]["answer"]["label"], "Merge");
    assert_eq!(answered[0]["answer"]["note"], "ok");
    assert_eq!(answered[0]["answered_by"], "local:test-user");
}

#[test]
fn an_answer_must_be_an_offered_option() {
    let (dir, _person) = store();
    let id = ask(&me(&dir));
    let refused = answer_as(&me(&dir), &id, "Deploy");
    assert!(
        matches!(&refused, Err(VogtError::InvalidRequest(message)) if message.contains("not an option")),
        "{refused:?}"
    );
    answer_as(&me(&dir), &id, "Hold").unwrap();
    let again = answer_as(&me(&dir), &id, "Merge");
    assert!(
        matches!(&again, Err(VogtError::Conflict(message)) if message.contains("answered, not pending")),
        "{again:?}"
    );
}

#[test]
fn options_are_labelled_distinct_and_exactly_one_recommended() {
    let (dir, _person) = store();
    let refusal = |extra: Value| match ask_with(&me(&dir), extra) {
        Err(VogtError::InvalidRequest(message)) => message,
        other => panic!("expected an invalid request, got {other:?}"),
    };
    assert!(refusal(json!({"options": [{"label": "a"}, {"label": "b"}]})).contains("exactly one"));
    assert!(refusal(json!({"options": [
        {"label": "a", "recommended": true},
        {"label": "b", "recommended": true},
    ]}))
    .contains("exactly one"));
    assert!(refusal(json!({"options": [
        {"label": "Merge", "recommended": true},
        {"label": "Merge"},
    ]}))
    .contains("differ"));
    assert!(refusal(json!({"options": [
        {"label": "  ", "recommended": true},
        {"label": "Hold"},
    ]}))
    .contains("every option has a label"));
    assert!(refusal(json!({"question": "   "})).contains("asks a question"));
}

#[test]
fn a_pending_decision_expires_on_read_and_cannot_be_answered() {
    let (dir, _person) = store();
    let id = ask_with(&as_agent(&dir, SESSION), json!({"ttl_seconds": 60})).unwrap();
    let later = open(&dir, person(), Moment::from_unix(1_700_000_120, 0));
    let expired = list(&later, json!({"state": "expired"}));
    assert_eq!(expired.len(), 1);
    assert_eq!(expired[0]["id"], id.as_str());
    assert!(expired[0]["answer"].is_null());
    assert!(list(&later, json!({"state": "pending"})).is_empty());
    let refused = answer_as(&later, &id, "Merge");
    assert!(
        matches!(&refused, Err(VogtError::Conflict(message)) if message.contains("expired")),
        "{refused:?}"
    );
    assert!(pending_entries(&later).is_empty());
}

#[test]
fn the_asker_or_a_person_withdraws() {
    let (dir, _person) = store();
    let first = ask(&as_agent(&dir, SESSION));
    let second = ask(&as_agent(&dir, SESSION));
    let refused = withdraw_as(&as_agent(&dir, WORKER), &first);
    assert!(
        matches!(&refused, Err(VogtError::GrantRefused(message)) if message.contains("only a person, or the session")),
        "{refused:?}"
    );
    let withdrawn = withdraw_as(&as_agent(&dir, SESSION), &first).unwrap();
    assert_eq!(withdrawn["decision"]["state"], "withdrawn");
    assert!(matches!(
        withdraw_as(&as_agent(&dir, SESSION), &first),
        Err(VogtError::Conflict(_))
    ));
    let answer_after = answer_as(&me(&dir), &first, "Merge");
    assert!(
        matches!(&answer_after, Err(VogtError::Conflict(message)) if message.contains("withdrawn")),
        "{answer_after:?}"
    );
    let by_person = withdraw_as(&me(&dir), &second).unwrap();
    assert_eq!(by_person["decision"]["state"], "withdrawn");
}

#[test]
fn list_filters_by_asker() {
    let (dir, _person) = store();
    let on_asker = ask(&as_agent(&dir, SESSION));
    ask(&me(&dir));
    let by_asker = list(&me(&dir), json!({"asker": SESSION}));
    assert_eq!(by_asker.len(), 1);
    assert_eq!(by_asker[0]["id"], on_asker.as_str());
    assert!(list(&me(&dir), json!({"asker": "nobody"})).is_empty());
}

#[test]
fn a_pending_decision_is_an_inbox_entry_until_answered() {
    let (dir, _person) = store();
    let id = ask(&as_agent(&dir, SESSION));
    let entries = pending_entries(&me(&dir));
    assert_eq!(entries.len(), 1, "{entries:?}");
    let entry = &entries[0];
    assert_eq!(entry["action"]["kind"], "decision");
    assert_eq!(entry["action"]["decision_id"], id.as_str());
    assert_eq!(entry["session_id"], "eng-overseer");
    assert_eq!(entry["source_subject_key"], id.as_str());
    assert!(entry["title"]
        .as_str()
        .unwrap()
        .contains("Merge the branch?"));
    let summary = entry["summary"].as_str().unwrap();
    assert!(
        summary.contains("Merge (recommended)") && summary.contains("Hold"),
        "{summary}"
    );
    assert_eq!(entry["actor_login"], SESSION);
    assert_eq!(entry["actor_kind"], "bot");
    let snapshot = &entry["evidence_snapshot"];
    assert_eq!(snapshot["recommended"], "Merge");
    assert_eq!(snapshot["context"], "CI is green.");
    assert_eq!(snapshot["options"][0]["label"], "Merge");
    assert_eq!(snapshot["options"][1]["label"], "Hold");
    answer_as(&me(&dir), &id, "Merge").unwrap();
    assert!(pending_entries(&me(&dir)).is_empty());
}

#[test]
fn every_step_is_audited() {
    let (dir, _person) = store();
    let decided = ask(&me(&dir));
    answer_as(&me(&dir), &decided, "Merge").unwrap();
    let other = ask(&me(&dir));
    withdraw_as(&me(&dir), &other).unwrap();
    let audited = history::audit_list_op(&me(&dir), json!({"limit": 50})).unwrap();
    let mut operations: Vec<String> = audited["records"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["entity_kind"] == "decision")
        .map(|row| row["operation"].as_str().unwrap().to_string())
        .collect();
    operations.sort();
    assert_eq!(
        operations,
        [
            "decision.answer",
            "decision.ask",
            "decision.ask",
            "decision.withdraw"
        ]
    );
}
