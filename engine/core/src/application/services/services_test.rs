//! The S4 services: preferences, notifications, initiatives and the Inbox.
//!
//! Ports the cases that stand on the service alone: a preference's optimistic
//! version, an initiative round trip, the notification ordering, and the Inbox
//! triage transitions with their verbatim refusals and the cursor paging.

use serde_json::json;

use crate::application::context::{build_context, Built};
use crate::application::services::{inbox, initiatives, notifications, preferences};
use crate::core::{ActorKind, Moment, Principal, SequentialIds, StepClock};
use crate::errors::VogtError;
use crate::storage::interface::{DeclaredStore, ObservedStore, WriteTxn};
use crate::storage::observed_types::PendingObservation;

fn moment() -> Moment {
    Moment::from_unix(1_700_000_000, 0)
}

fn unique() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

fn principal() -> Principal {
    Principal::new("local:test-user", ActorKind::Human, "Test").unwrap()
}

/// A migrated, bootstrapped pair of stores under a temporary data directory,
/// held by a context with a step clock.
fn opened() -> Built {
    let dir = std::env::temp_dir().join(format!("vogt-s4-{}-{}", std::process::id(), unique()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let config = crate::config::VogtConfig {
        data_dir: dir,
        ..crate::config::VogtConfig::default()
    };
    let built = build_context(
        config,
        Some(principal()),
        Some(StepClock::new(moment())),
        Some(SequentialIds::new(None).unwrap()),
        None,
        None,
        None,
        None,
    )
    .unwrap();
    match &built {
        Built::StepSequential(ctx) => {
            ctx.declared.migrate().unwrap();
            ctx.declared.bootstrap(&principal()).unwrap();
            ctx.observed.migrate().unwrap();
        }
        _other => panic!("expected a step clock"),
    }
    built
}

fn record_notification(built: &Built, subject: &str, title: &str, updated_at: &str) {
    let Built::StepSequential(ctx) = built else {
        unreachable!("opened() builds a step clock");
    };
    let sweep = ctx
        .observed
        .begin_sweep("forge-notifications", &[], moment())
        .unwrap();
    ctx.observed
        .append(
            &sweep.id,
            &[PendingObservation {
                kind: "forge.notification".to_string(),
                subject_key: subject.to_string(),
                payload: json!({
                    "title": title,
                    "updated_at": updated_at,
                    "reason": title,
                    "unread": true,
                }),
                content_digest: format!("sha256:{subject}"),
                project_id: None,
                source_url: None,
                promoted: false,
            }],
            moment(),
        )
        .unwrap();
    ctx.observed
        .finish_sweep(
            &sweep.id,
            crate::core::SweepOutcome::Ok,
            &[],
            moment(),
            None,
        )
        .unwrap();
    ctx.observed.rebuild_latest().unwrap();
}

fn open_drift(built: &Built, summary: &str) -> String {
    let Built::StepSequential(ctx) = built else {
        unreachable!("opened() builds a step clock");
    };
    let mut txn = ctx.declared.write().unwrap();
    let id = format!("drift-{summary}");
    txn.insert_drift(&crate::core::DriftProposal {
        id: id.clone(),
        kind: "work_item.unlinked".to_string(),
        subject_kind: "project".to_string(),
        subject_id: "prj".to_string(),
        project_id: None,
        project_slug: None,
        summary: summary.to_string(),
        evidence_observation_id: None,
        evidence_snapshot: json!({}),
        proposed_change: json!({}),
        status: crate::core::DriftStatus::Open,
        opened_at: moment(),
        superseded_at: None,
        superseded_detail: None,
        resolved_by_actor_id: None,
        resolved_by_identity_ref: None,
        resolved_at: None,
        resolution_reason: None,
    })
    .unwrap();
    txn.commit().unwrap();
    id
}

#[test]
fn a_preference_write_bumps_its_version_and_a_stale_one_is_refused() {
    let built = opened();
    let stored = preferences::preference_set_op(
        &built,
        json!({"key": "inbox.filter", "value": {"sources": ["ci"]}, "reason": "my default view"}),
    )
    .unwrap();
    assert_eq!(stored["preference"]["version"], 1);

    let conflict = preferences::preference_set_op(
        &built,
        json!({
            "key": "inbox.filter",
            "value": {"sources": ["drift"]},
            "expected_version": 0,
            "reason": "stale",
        }),
    )
    .unwrap_err();
    match conflict {
        VogtError::PreferenceVersionConflict(message) => {
            assert!(message.contains("re-read it"), "{message}");
        }
        other => panic!("unexpected {other:?}"),
    }

    let again = preferences::preference_set_op(
        &built,
        json!({
            "key": "inbox.filter",
            "value": {"sources": ["drift"]},
            "expected_version": 1,
            "reason": "moved on",
        }),
    )
    .unwrap();
    assert_eq!(again["preference"]["version"], 2);
}

#[test]
fn a_saved_filter_with_bad_fields_reads_like_pydantic() {
    let built = opened();
    let bad = preferences::preference_set_op(
        &built,
        json!({
            "key": "inbox.filter",
            "value": {"sources": ["bogus"], "actor": "nobody", "project": "x"},
            "reason": "bad",
        }),
    )
    .unwrap_err();
    match bad {
        VogtError::InvalidPreference(message) => assert_eq!(
            message,
            "invalid inbox.filter value — sources.0: Input should be 'github', \
             'drift', 'ci' or 'agent'; actor: Input should be 'any', 'external', \
             'org' or 'bot'; project: Extra inputs are not permitted"
        ),
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn a_preference_key_outside_the_shape_is_refused() {
    let built = opened();
    let bad_key = preferences::preference_set_op(
        &built,
        json!({"key": "Not A Key", "value": {}, "reason": "bad"}),
    )
    .unwrap_err();
    assert!(
        matches!(bad_key, VogtError::InvalidRequest(_)),
        "{bad_key:?}"
    );
}

#[test]
fn notifications_come_back_newest_first() {
    let built = opened();
    record_notification(&built, "note-old", "older", "2026-01-01T00:00:00+00:00");
    record_notification(&built, "note-new", "newer", "2026-02-01T00:00:00+00:00");
    let listed = notifications::notifications_op(&built, json!({})).unwrap();
    let titles: Vec<&str> = listed["notifications"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["title"].as_str().unwrap())
        .collect();
    assert_eq!(titles, vec!["newer", "older"]);
    assert_eq!(listed["unread"], 2);
    assert_eq!(listed["by_reason"]["newer"], 1);
}

#[test]
fn an_initiative_round_trips() {
    let built = opened();
    let created = initiatives::initiative_create_op(
        &built,
        json!({"title": "Platform", "reason": "starting the theme"}),
    )
    .unwrap();
    assert_eq!(created["initiative"]["slug"], "platform");
    assert_eq!(created["initiative"]["title"], "Platform");

    let listed = initiatives::initiative_list_op(&built, json!({})).unwrap();
    assert_eq!(listed["initiatives"].as_array().unwrap().len(), 1);

    let updated = initiatives::initiative_update_op(
        &built,
        json!({"slug": "platform", "title": "Platform work", "reason": "renamed"}),
    )
    .unwrap();
    assert_eq!(updated["initiative"]["title"], "Platform work");
}

#[test]
fn publishing_an_initiative_refuses_honestly() {
    let built = opened();
    initiatives::initiative_create_op(
        &built,
        json!({"title": "Platform", "reason": "starting the theme"}),
    )
    .unwrap();
    let published = initiatives::initiative_publish_op(
        &built,
        json!({"slug": "platform", "reason": "ship it"}),
    )
    .unwrap();
    assert_eq!(published["slug"], "platform");
    assert_eq!(published["tracking_issues"].as_array().unwrap().len(), 0);
}

#[test]
fn inbox_triage_transitions_and_their_refusals() {
    let built = opened();
    let drift_id = open_drift(&built, "a branch drifted");

    let listed = inbox::inbox_list_op(&built, json!({})).unwrap();
    let entries = listed["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1, "{listed}");
    let key = entries[0]["entry_key"].as_str().unwrap().to_string();
    assert!(key.starts_with(&format!("drift:{drift_id}:")), "{key}");
    assert_eq!(listed["counts"]["active"], 1);
    assert_eq!(listed["engine_status"], "not_configured");

    let archived =
        inbox::inbox_archive_op(&built, json!({"entry_key": key, "reason": "seen it"})).unwrap();
    assert_eq!(archived["entry"]["triage_state"], "archived");

    let again =
        inbox::inbox_archive_op(&built, json!({"entry_key": key, "reason": "again"})).unwrap_err();
    match again {
        VogtError::InvalidTriageState(message) => {
            assert_eq!(message, format!("Inbox entry {key:?} is already archived"));
        }
        other => panic!("unexpected {other:?}"),
    }

    let snooze = inbox::inbox_snooze_op(
        &built,
        json!({"entry_key": key, "until": "2026-06-01T00:00:00+00:00", "reason": "later"}),
    )
    .unwrap_err();
    match snooze {
        VogtError::InvalidTriageState(message) => {
            assert_eq!(
                message,
                "an archived Inbox entry must be restored before snoozing"
            );
        }
        other => panic!("unexpected {other:?}"),
    }

    let past = inbox::inbox_snooze_op(
        &built,
        json!({"entry_key": key, "until": "2020-01-01T00:00:00+00:00", "reason": "too late"}),
    )
    .unwrap_err();
    match past {
        VogtError::InvalidSnooze(message) => {
            assert_eq!(message, "snooze deadline must be in the future");
        }
        other => panic!("unexpected {other:?}"),
    }

    let missing =
        inbox::inbox_archive_op(&built, json!({"entry_key": "nope", "reason": "missing"}))
            .unwrap_err();
    match missing {
        VogtError::InboxEntryNotFound(message) => {
            assert_eq!(message, "no current Inbox entry 'nope'");
        }
        other => panic!("unexpected {other:?}"),
    }

    inbox::inbox_restore_op(&built, json!({"entry_key": key, "reason": "back"})).unwrap();
    let snoozed = inbox::inbox_snooze_op(
        &built,
        json!({"entry_key": key, "until": "2026-06-01T00:00:00+00:00", "reason": "later"}),
    )
    .unwrap();
    assert_eq!(snoozed["entry"]["triage_state"], "snoozed");

    let active = inbox::inbox_list_op(&built, json!({})).unwrap();
    assert_eq!(active["entries"].as_array().unwrap().len(), 0);
    let all = inbox::inbox_list_op(
        &built,
        json!({"triage_states": ["active", "snoozed", "archived"]}),
    )
    .unwrap();
    assert_eq!(all["entries"].as_array().unwrap().len(), 1);
}

#[test]
fn inbox_paging_returns_a_cursor_that_continues() {
    let built = opened();
    record_notification(&built, "n1", "first", "2026-01-01T00:00:00+00:00");
    record_notification(&built, "n2", "second", "2026-01-02T00:00:00+00:00");
    record_notification(&built, "n3", "third", "2026-01-03T00:00:00+00:00");

    let page = inbox::inbox_list_op(&built, json!({"limit": 2})).unwrap();
    assert_eq!(page["entries"].as_array().unwrap().len(), 2);
    let cursor = page["next_cursor"].as_str().unwrap();

    let rest = inbox::inbox_list_op(&built, json!({"limit": 2, "cursor": cursor})).unwrap();
    assert_eq!(rest["entries"].as_array().unwrap().len(), 1);
    assert!(rest["next_cursor"].is_null());

    let bad = inbox::inbox_list_op(&built, json!({"cursor": "!!!not-a-cursor"})).unwrap_err();
    assert!(matches!(bad, VogtError::InvalidCursor(_)), "{bad:?}");
}

/// The inbox rollup reads triage once, through `inbox_triage_by_keys`, so a
/// listing must not grow a query per entry. Measured as the marginal cost of a
/// second listing in the same database, so the store setup is not in the
/// number: ten times the entries must not cost more than about ten times as
/// long. A per-entry query would blow that by an order of magnitude. The bound
/// is relative and both sides run back to back, so load on the machine moves
/// them together.
#[test]
fn inbox_list_stays_linear_at_the_load_profile() {
    let built = opened();
    seed_drift(&built, 1000);
    let _ = inbox::inbox_list_op(&built, json!({"limit": 50})).unwrap();

    let small = time_inbox_list(&built, 100);
    let large = time_inbox_list(&built, 1000);
    assert!(
        large
            < small
                .saturating_mul(15)
                .max(std::time::Duration::from_millis(500)),
        "inbox.list did not stay linear: {small:?} for 100 entries, {large:?} for 1000"
    );
}

fn seed_drift(built: &Built, count: usize) {
    let Built::StepSequential(ctx) = built else {
        unreachable!("opened() builds a step clock");
    };
    let mut txn = ctx.declared.write().unwrap();
    for index in 0..count {
        txn.insert_drift(&crate::core::DriftProposal {
            id: format!("drift-{index:05}"),
            kind: "work_item.unlinked".to_string(),
            subject_kind: "project".to_string(),
            subject_id: format!("prj-{index:05}"),
            project_id: None,
            project_slug: None,
            summary: format!("drift {index}"),
            evidence_observation_id: None,
            evidence_snapshot: json!({}),
            proposed_change: json!({}),
            status: crate::core::DriftStatus::Open,
            opened_at: moment(),
            superseded_at: None,
            superseded_detail: None,
            resolved_by_actor_id: None,
            resolved_by_identity_ref: None,
            resolved_at: None,
            resolution_reason: None,
        })
        .unwrap();
    }
    txn.commit().unwrap();
}

fn time_inbox_list(built: &Built, count: usize) -> std::time::Duration {
    let started = std::time::Instant::now();
    let listed = inbox::inbox_list_op(built, json!({"limit": count})).unwrap();
    let elapsed = started.elapsed();
    assert_eq!(listed["entries"].as_array().unwrap().len(), count);
    elapsed
}
