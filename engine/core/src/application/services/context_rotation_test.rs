//! WI-3132: an oversight session over its context budget is asked for a
//! hand-off, then replaced by a session seeded with it.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use super::context_rotation::rotate_over_budget;
use super::preferences::preference_set_op;
use super::sessions::start_session;
use super::sessions_test::{insert_project, start_params};
use crate::application::context::Built;
use crate::application::dispatch::CONTEXT_BUDGET_KEY;
use crate::core::{ActorKind, Moment, Principal};
use crate::storage::interface::{AuditQuery, DeclaredStore, ObservedStore, ReadView};

const WHY: &str = "context rotation test";

/// One live oversight session, `eng-old`, reading `context` tokens while
/// idle. POSTs are recorded; a hibernate marks it hibernated.
struct Script {
    seen: Mutex<Vec<String>>,
    created: Mutex<Vec<String>>,
    hibernated: Mutex<bool>,
    context: i64,
}

impl Script {
    fn posts(&self, suffix: &str) -> usize {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|line| line.starts_with("POST") && line.contains(suffix))
            .count()
    }
}

fn opened(context: i64) -> (Built, Arc<Script>, PathBuf) {
    let script = Arc::new(Script {
        seen: Mutex::new(Vec::new()),
        created: Mutex::new(Vec::new()),
        hibernated: Mutex::new(false),
        context,
    });
    let seen = Arc::clone(&script);
    let transport: crate::adapters::engine::Transport =
        Arc::new(move |url, _headers, body, method| {
            let text = String::from_utf8_lossy(body).into_owned();
            seen.seen
                .lock()
                .unwrap()
                .push(format!("{method} {url} {text}"));
            let path = url.split('?').next().unwrap_or(url);
            if method == "GET" && path.ends_with("/api/sessions") {
                let old_down = *seen.hibernated.lock().unwrap();
                let created = seen.created.lock().unwrap().clone();
                let rows: Vec<Value> = created
                    .iter()
                    .map(|id| {
                        let old = id == "eng-old";
                        json!({
                            "id": id,
                            "name": id,
                            "role": "oversight",
                            "activity": if old && old_down { "hibernated" } else { "idle" },
                            "alive": !(old && old_down),
                            "cwd": "/work/tree",
                            "economics": {"context_tokens": if old { seen.context } else { 10 }},
                        })
                    })
                    .collect();
                return (200, serde_json::to_vec(&json!({"sessions": rows})).unwrap());
            }
            if method == "POST" && path.ends_with("/api/sessions") {
                let mut made = seen.created.lock().unwrap();
                let id = if made.is_empty() {
                    "eng-old"
                } else {
                    "eng-new"
                };
                made.push(id.to_string());
                return (200, format!(r#"{{"id":"{id}"}}"#).into_bytes());
            }
            if method == "POST" && path.ends_with("/hibernate") {
                *seen.hibernated.lock().unwrap() = true;
                return (200, br#"{"id":"eng-old","activity":"hibernated"}"#.to_vec());
            }
            (200, b"{}".to_vec())
        });
    let dir = std::env::temp_dir().join(format!(
        "vogt-rot-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let config = crate::config::VogtConfig {
        data_dir: dir.clone(),
        ..crate::config::VogtConfig::default()
    };
    let human = Principal::new("local:test-user", ActorKind::Human, "Test").unwrap();
    let built = crate::application::context::build_context(
        config,
        Some(human.clone()),
        Some(crate::core::StepClock::new(Moment::from_unix(
            1_700_000_000,
            0,
        ))),
        Some(crate::core::SequentialIds::new(None).unwrap()),
        None,
        Some(crate::adapters::engine::EngineClient::new(
            "http://engine",
            Some("tok".to_string()),
            Some(transport),
        )),
        None,
        None,
    )
    .unwrap();
    let Built::StepSequential(ctx) = &built else {
        unreachable!("a step clock and sequential ids");
    };
    ctx.declared.migrate().unwrap();
    ctx.declared.bootstrap(&human).unwrap();
    ctx.observed.migrate().unwrap();
    insert_project(&built, "vogt");
    let mut params = start_params(Some("vogt"), Some("oversee"), WHY);
    params.role = "oversight".to_string();
    start_session(ctx, &params).unwrap();
    (built, script, dir)
}

fn budget(built: &Built, tokens: i64) {
    preference_set_op(
        built,
        json!({"key": CONTEXT_BUDGET_KEY, "value": {"oversight": {"tokens": tokens}}, "reason": WHY}),
    )
    .unwrap();
}

fn ctx(
    built: &Built,
) -> &crate::application::context::AppContext<crate::core::StepClock, crate::core::SequentialIds> {
    let Built::StepSequential(ctx) = built else {
        unreachable!();
    };
    ctx
}

#[test]
fn over_budget_oversight_is_asked_then_rotated() {
    let (built, script, dir) = opened(400_000);
    budget(&built, 250_000);

    let first = rotate_over_budget(ctx(&built)).unwrap();
    assert_eq!(first.asked, vec!["eng-old".to_string()]);
    assert!(first.rotated.is_empty());
    assert_eq!(script.posts("/input"), 1);
    // Asked once; the marker holds off a second ask until it ages.
    let again = rotate_over_budget(ctx(&built)).unwrap();
    assert!(again.asked.is_empty() && again.rotated.is_empty());
    assert_eq!(script.posts("/input"), 1);

    let handoff = dir.join("context-rotation").join("eng-old.md");
    std::fs::write(&handoff, "fleet: eng-1\nwait_any cursor: 42\n").unwrap();
    let rotated = rotate_over_budget(ctx(&built)).unwrap();
    assert_eq!(
        rotated.rotated,
        vec![("eng-old".to_string(), "eng-new".to_string())]
    );
    assert_eq!(script.posts("/hibernate"), 1);
    // The replacement is an oversight session seeded with the file.
    let seen = script.seen.lock().unwrap().clone();
    let create = seen
        .iter()
        .rfind(|line| line.starts_with("POST") && line.contains("/api/sessions "))
        .unwrap()
        .clone();
    assert!(create.contains("\"role\":\"oversight\""), "{create}");
    assert!(create.contains("eng-old.md"), "{create}");
    drop(seen);

    // The old session is hibernated and the new one is small: nothing more.
    let after = rotate_over_budget(ctx(&built)).unwrap();
    assert!(after.asked.is_empty() && after.rotated.is_empty());
    assert_eq!(script.posts("/hibernate"), 1);

    // The rotation is an audited event.
    let audit = ctx(&built)
        .declared
        .read()
        .unwrap()
        .list_audit(&AuditQuery {
            limit: 50,
            offset: 0,
            actor_id: None,
            operation: Some("session.context_rotate".to_string()),
            entity_id: None,
            project_id: None,
            since: None,
            until: None,
        })
        .unwrap();
    assert_eq!(audit.len(), 1);
    assert_eq!(audit[0].entity_id, "eng-old");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn under_budget_or_unconfigured_roles_are_left_alone() {
    let (built, script, dir) = opened(100_000);
    // No budget stored: nothing happens however large the context.
    assert_eq!(rotate_over_budget(ctx(&built)).unwrap(), Default::default());
    budget(&built, 250_000);
    assert_eq!(rotate_over_budget(ctx(&built)).unwrap(), Default::default());
    assert_eq!(script.posts("/input"), 0);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn the_budget_setting_is_validated() {
    use crate::application::dispatch::validate;
    assert!(validate(
        CONTEXT_BUDGET_KEY,
        &json!({"oversight": {"tokens": 250000}})
    )
    .is_ok());
    assert!(validate(CONTEXT_BUDGET_KEY, &json!({"worker": {"tokens": 1}})).is_err());
    assert!(validate(CONTEXT_BUDGET_KEY, &json!({"oversight": {"tokens": 0}})).is_err());
    assert!(validate(
        CONTEXT_BUDGET_KEY,
        &json!({"oversight": {"tokens": 1, "x": 1}})
    )
    .is_err());
    assert!(validate(CONTEXT_BUDGET_KEY, &json!({}))
        .unwrap()
        .as_object()
        .unwrap()
        .is_empty());
}
