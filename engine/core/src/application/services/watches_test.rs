//! Watch create / list / cancel, ported from `tests/test_watches.py` (WI-2036).
//!
//! The loop that fires a watch is not part of this port, so these tests cover
//! the registry operations only: arming, listing, and cancelling.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::application::context::{build_context, Built};
use crate::application::services::watches;
use crate::config::VogtConfig;
use crate::core::{ActorKind, Moment, Principal, SequentialIds, StepClock};
use crate::errors::VogtError;
use crate::storage::interface::{DeclaredStore, ObservedStore};

const ENGINE: &str = "eng-overseer";
const WHY: &str = "watch test";

fn start() -> Moment {
    Moment::from_unix(1_700_000_000, 0)
}

fn unique_dir() -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("vogt-watches-{}-{n}", std::process::id()));
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

fn open(dir: &Path, principal: Principal) -> Built {
    let config = VogtConfig {
        data_dir: dir.to_path_buf(),
        ..VogtConfig::default()
    };
    build_context(
        config,
        Some(principal),
        Some(StepClock::new(start())),
        Some(SequentialIds::new(Some(dir.join("ids.json"))).unwrap()),
        None,
        None,
        None,
        None,
    )
    .unwrap()
}

fn store() -> (PathBuf, Built) {
    let dir = unique_dir();
    let built = open(&dir, person());
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

fn session_trigger(target: &str) -> Value {
    json!({
        "trigger": {"kind": "session", "session": target, "until": "ended"},
        "reason": WHY,
    })
}

fn create(built: &Built, params: Value) -> Result<Value, VogtError> {
    watches::create_watch_op(built, params)
}

#[test]
fn a_person_names_the_session_a_watch_wakes() {
    let (_dir, built) = store();
    let result = create(
        &built,
        json!({
            "trigger": {"kind": "session", "session": "other-engine", "until": "done"},
            "session": ENGINE,
            "note": "land it",
            "reason": WHY,
        }),
    )
    .unwrap();
    let watch = &result["watch"];
    assert!(watch["id"].as_str().unwrap().starts_with("wat_"));
    assert_eq!(watch["session"], ENGINE);
    assert_eq!(watch["kind"], "session");
    assert_eq!(watch["state"], "active");
    assert_eq!(watch["owner"], "local:test-user");
    assert!(watch["waits_for"]
        .as_str()
        .unwrap()
        .contains("other-engine"));
}

#[test]
fn an_agent_session_watches_itself_when_none_is_named() {
    let (dir, _) = store();
    let built = open(&dir, agent(&format!("agent:engine:{ENGINE}")));
    let result = create(&built, session_trigger("other-engine")).unwrap();
    assert_eq!(result["watch"]["session"], ENGINE);
}

#[test]
fn a_caller_that_is_not_a_session_must_name_one() {
    let (_dir, built) = store();
    let err = create(&built, session_trigger("other-engine")).unwrap_err();
    assert!(matches!(err, VogtError::InvalidRequest(_)));
}

#[test]
fn ttl_outside_the_window_is_refused() {
    let (_dir, built) = store();
    let err = create(
        &built,
        json!({
            "trigger": {"kind": "session", "session": "other-engine"},
            "session": ENGINE,
            "ttl_seconds": 10,
            "reason": WHY,
        }),
    )
    .unwrap_err();
    assert!(matches!(err, VogtError::InvalidRequest(_)));
}

#[test]
fn list_returns_the_watch_and_cancel_ends_it() {
    let (dir, built) = store();
    let created = create(
        &built,
        json!({
            "trigger": {"kind": "session", "session": "other-engine"},
            "session": ENGINE,
            "reason": WHY,
        }),
    )
    .unwrap();
    let id = created["watch"]["id"].as_str().unwrap().to_string();
    let listed = watches::list_watches_op(&built, json!({})).unwrap();
    assert_eq!(listed["watches"].as_array().unwrap().len(), 1);

    let other = open(&dir, agent("agent:engine:stranger"));
    let refused =
        watches::cancel_watch_op(&other, json!({"id": id, "reason": "not mine"})).unwrap_err();
    assert!(matches!(refused, VogtError::GrantRefused(_)));

    let cancelled =
        watches::cancel_watch_op(&built, json!({"id": id, "reason": "no longer needed"})).unwrap();
    assert_eq!(cancelled["watch"]["state"], "cancelled");

    let again = watches::cancel_watch_op(&built, json!({"id": id, "reason": "twice"})).unwrap_err();
    assert!(matches!(again, VogtError::Conflict(_)));
}
