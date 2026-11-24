//! The audited write path and the resolvers.
//!
//! Ports the cases in `tests/test_writes.py` that stand on the write path
//! alone — a service layer does not exist yet — and the messages
//! `_resolve.py` produces for a name that is not there.

use std::sync::Arc;

use serde_json::json;

use super::resolve;
use super::writes::{audited_action, audited_write, validate_reason, WriteContext, WriteOutcome};
use crate::core::{
    ActorKind, Initiative, InitiativeState, Label, Moment, Origin, Principal, Priority, Project,
    SequentialIds, StepClock, TrustState, WorkItem, WorkKind,
};
use crate::errors::VogtError;
use crate::storage::interface::{AuditQuery, DeclaredStore, ReadView, WriteTxn};
use crate::storage::sqlite::declared::SqliteDeclaredStore;

fn moment() -> Moment {
    Moment::from_unix(1_700_000_000, 0)
}

fn unique() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

/// A migrated, bootstrapped store, with the principal the suite uses.
fn opened() -> SqliteDeclaredStore<StepClock, SequentialIds> {
    let dir = std::env::temp_dir().join(format!("vogt-writes-{}-{}", std::process::id(), unique()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let store = SqliteDeclaredStore::new(
        dir.join("declared.sqlite3"),
        StepClock::new(moment()),
        SequentialIds::new(None).unwrap(),
    );
    store.migrate().unwrap();
    let principal = Principal::new("local:test-user", ActorKind::Human, "Test").unwrap();
    store.bootstrap(&principal).unwrap();
    store
}

fn writing<'a>(
    store: &'a SqliteDeclaredStore<StepClock, SequentialIds>,
    identity: &'a str,
    kind: ActorKind,
    display: &'a str,
) -> WriteContext<'a, StepClock, SequentialIds, SqliteDeclaredStore<StepClock, SequentialIds>> {
    // The store already holds the one clock and the one id factory; the write
    // uses those, so a draw inside the body continues the store's count.
    let principal = Principal::new(identity, kind, display).unwrap();
    WriteContext::new(
        store,
        // The principal has to outlive the context, so it is leaked for the
        // test. Each test builds one.
        Box::leak(Box::new(principal)),
        Arc::clone(store.clock()),
        Arc::clone(store.id_factory()),
    )
}

fn project(id: &str, slug: &str) -> Project {
    Project::new(id, slug, slug, &format!("/srv/{slug}"), moment())
}

#[test]
fn a_blank_reason_is_refused() {
    for reason in ["", "   ", "\n\t "] {
        let error = validate_reason(reason).unwrap_err();
        assert!(
            matches!(error, VogtError::MissingReason(ref message) if message.contains("non-empty reason")),
            "{reason:?} -> {error}"
        );
    }
}

#[test]
fn a_reason_is_stored_stripped() {
    let store = opened();
    audited_write(
        &mut writing(&store, "local:test-user", ActorKind::Human, "Test"),
        "test.op",
        "  padded reason  ",
        |txn, _actor| {
            let made = project("prj_spaced", "spaced");
            txn.insert_project(&made)?;
            Ok(WriteOutcome::new(
                (),
                "project",
                &made.id,
                json!({"slug": "spaced"}),
                "test.happened",
                json!({}),
            ))
        },
    )
    .unwrap();

    let view = store.read().unwrap();
    let audit = view
        .list_audit(&AuditQuery {
            limit: 1,
            offset: 0,
            actor_id: None,
            operation: Some("test.op".to_string()),
            entity_id: None,
            project_id: None,
            since: None,
            until: None,
        })
        .unwrap();
    assert_eq!(audit[0].reason, "padded reason");
}

#[test]
fn a_failing_body_rolls_the_transaction_back() {
    let store = opened();
    let before = store.read().unwrap().counts().unwrap();
    let error = audited_write(
        &mut writing(&store, "local:test-user", ActorKind::Human, "Test"),
        "test.op",
        "a reason",
        |txn, _actor| {
            txn.insert_project(&project("prj_doomed", "doomed"))?;
            Err::<WriteOutcome<()>, _>(VogtError::Conflict("body failed".to_string()))
        },
    )
    .unwrap_err();
    assert!(matches!(error, VogtError::Conflict(_)));

    let after = store.read().unwrap().counts().unwrap();
    assert_eq!(after.projects, before.projects);
    assert_eq!(after.events, before.events);
    assert_eq!(after.audit, before.audit, "only the bootstrap row");
}

#[test]
fn an_unseen_principal_is_auto_registered_and_explained() {
    let store = opened();
    audited_write(
        &mut writing(&store, "agent:claude-code", ActorKind::Agent, "Claude Code"),
        "test.op",
        "the write that introduces the actor",
        |txn, actor| {
            let made = project("prj_agent", "agent-project");
            txn.insert_project(&made)?;
            Ok(WriteOutcome::new(
                actor.id.clone(),
                "project",
                &made.id,
                json!({}),
                "test.happened",
                json!({}),
            ))
        },
    )
    .unwrap();

    let view = store.read().unwrap();
    let actor = view
        .actor_by_identity("agent:claude-code")
        .unwrap()
        .unwrap();
    assert_eq!(actor.kind, ActorKind::Agent);
    assert_eq!(actor.display_name, "Claude Code");

    let registrations = view
        .list_audit(&AuditQuery {
            limit: 10,
            offset: 0,
            actor_id: None,
            operation: Some("actor.auto_register".to_string()),
            entity_id: None,
            project_id: None,
            since: None,
            until: None,
        })
        .unwrap();
    assert_eq!(registrations.len(), 1);
    assert!(registrations[0].reason.contains("first authenticated use"));
    assert_eq!(registrations[0].entity_id, actor.id);

    let events = view.list_events(0, 10, None).unwrap();
    let kinds: Vec<_> = events.iter().map(|event| event.kind.as_str()).collect();
    assert_eq!(kinds, ["actor.auto_register", "test.happened"]);
    assert_eq!(
        events[0].audit_id.as_deref(),
        Some(registrations[0].id.as_str())
    );
}

#[test]
fn a_known_principal_is_not_registered_again() {
    let store = opened();
    for _ in 0..2 {
        audited_write(
            &mut writing(&store, "local:test-user", ActorKind::Human, "Test"),
            "test.op",
            "again",
            |txn, actor| {
                let _ = txn;
                Ok(WriteOutcome::new(
                    actor.id.clone(),
                    "instance",
                    "the-instance",
                    json!({}),
                    "test.happened",
                    json!({}),
                ))
            },
        )
        .unwrap();
    }
    let view = store.read().unwrap();
    let registrations = view
        .list_audit(&AuditQuery {
            limit: 10,
            offset: 0,
            actor_id: None,
            operation: Some("actor.auto_register".to_string()),
            entity_id: None,
            project_id: None,
            since: None,
            until: None,
        })
        .unwrap();
    assert!(
        registrations.is_empty(),
        "bootstrap already created the actor"
    );
}

#[test]
fn an_action_records_what_happened_outside_the_store() {
    let store = opened();
    let outcome = json!({"cloned": 3});
    audited_action(
        &mut writing(&store, "local:test-user", ActorKind::Human, "Test"),
        "instance.clone",
        "restoring a backup",
        "instance",
        "inst_1",
        &outcome,
        "instance.cloned",
        None,
    )
    .unwrap();

    let view = store.read().unwrap();
    let audit = view
        .list_audit(&AuditQuery {
            limit: 1,
            offset: 0,
            actor_id: None,
            operation: Some("instance.clone".to_string()),
            entity_id: None,
            project_id: None,
            since: None,
            until: None,
        })
        .unwrap();
    assert_eq!(audit[0].entity_kind, "instance");
    assert_eq!(audit[0].reason, "restoring a backup");
    let events = view.list_events(0, 10, None).unwrap();
    assert_eq!(events.last().unwrap().summary, outcome);
    assert_eq!(events.last().unwrap().kind, "instance.cloned");
}

#[test]
fn an_action_refuses_a_blank_reason_too() {
    let store = opened();
    let error = audited_action(
        &mut writing(&store, "local:test-user", ActorKind::Human, "Test"),
        "instance.clone",
        "   ",
        "instance",
        "inst_1",
        &json!({}),
        "instance.cloned",
        None,
    )
    .unwrap_err();
    assert!(matches!(error, VogtError::MissingReason(_)));
    assert_eq!(store.read().unwrap().counts().unwrap().events, 0);
}

#[test]
fn a_missing_name_says_what_was_missing() {
    let store = opened();
    let view = store.read().unwrap();

    let project = resolve::project(&view, "nope").unwrap_err().to_string();
    assert!(project.contains("no project with slug 'nope'"), "{project}");

    let item = resolve::work_item(&view, "WI-70").unwrap_err().to_string();
    assert!(item.contains("no work item 'WI-70'"), "{item}");

    let actor = resolve::actor(&view, "agent:ghost")
        .unwrap_err()
        .to_string();
    assert!(
        actor.contains("no actor with identity 'agent:ghost'"),
        "{actor}"
    );
    assert!(actor.contains("`actor create`"), "{actor}");

    let initiative = resolve::initiative(&view, "gone").unwrap_err().to_string();
    assert!(
        initiative.contains("no initiative with slug 'gone'"),
        "{initiative}"
    );

    let label = resolve::label_exists(&view, "red").unwrap_err().to_string();
    assert!(label.contains("no label named 'red'"), "{label}");
    assert!(label.contains("`label create`"), "{label}");
}

#[test]
fn a_name_that_exists_resolves() {
    let store = opened();
    audited_write(
        &mut writing(&store, "local:test-user", ActorKind::Human, "Test"),
        "test.op",
        "seeding",
        |txn, _actor| {
            txn.insert_project(&project("prj_alpha", "alpha"))?;
            txn.insert_work_item(&WorkItem {
                id: "wrk_1".to_string(),
                reference: "WI-7".to_string(),
                kind: WorkKind::Feature,
                title: "The thing".to_string(),
                body: String::new(),
                state: "open".to_string(),
                priority: Priority::P2,
                effort: None,
                project_id: Some("prj_alpha".to_string()),
                project_slug: Some("alpha".to_string()),
                initiative_id: None,
                origin: Origin::Created,
                trust_state: TrustState::Unverified,
                assignee_actor_id: None,
                assignee_identity_ref: None,
                labels: Vec::new(),
                relations: Vec::new(),
                superseded_by: None,
                created_at: moment(),
                updated_at: moment(),
            })?;
            txn.insert_initiative(&Initiative {
                id: "ini_1".to_string(),
                slug: "the-push".to_string(),
                title: "The push".to_string(),
                body: String::new(),
                state: InitiativeState::Open,
                weight: 0,
                created_at: moment(),
                updated_at: moment(),
            })?;
            txn.insert_label(&Label {
                id: "lbl_1".to_string(),
                name: "red".to_string(),
                color: None,
                created_at: moment(),
            })?;
            Ok(WriteOutcome::new(
                (),
                "project",
                "prj_alpha",
                json!({}),
                "test.seeded",
                json!({}),
            ))
        },
    )
    .unwrap();

    let view = store.read().unwrap();
    assert_eq!(resolve::project(&view, "alpha").unwrap().id, "prj_alpha");
    assert_eq!(
        resolve::work_item(&view, "WI-7").unwrap().title,
        "The thing"
    );
    assert_eq!(
        resolve::actor(&view, "local:test-user")
            .unwrap()
            .display_name,
        "Test"
    );
    assert_eq!(
        resolve::initiative(&view, "the-push").unwrap().title,
        "The push"
    );
    resolve::label_exists(&view, "red").unwrap();
}

#[test]
fn an_apostrophe_is_quoted_the_way_python_quotes_it() {
    let store = opened();
    let view = store.read().unwrap();
    let message = resolve::project(&view, "it's").unwrap_err().to_string();
    assert!(message.contains("\"it's\""), "{message}");
}
