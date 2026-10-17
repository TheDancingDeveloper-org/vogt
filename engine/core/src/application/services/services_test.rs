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
use crate::storage::interface::{DeclaredStore, ObservedStore, ReadView, WriteTxn};
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
            assert_eq!(
                message,
                format!(
                    "Inbox entry {} is already archived",
                    crate::core::py_repr(&key)
                )
            );
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

/// Status reports only the acting actor's accounts, never the token, and a
/// principal who has never written has none.
#[test]
fn forge_account_status_lists_what_is_linked() {
    use crate::application::services::forge_accounts;
    let built = opened();
    let Built::StepSequential(ctx) = &built else {
        panic!("expected a step clock");
    };
    let empty = forge_accounts::account_status_op(&built, json!({})).unwrap();
    assert_eq!(empty["accounts"].as_array().unwrap().len(), 0);

    let actor = ctx
        .declared
        .read()
        .unwrap()
        .actor_by_identity(&principal().identity_ref)
        .unwrap()
        .unwrap();
    let mut txn = ctx.declared.write().unwrap();
    txn.upsert_forge_account(
        &actor.id,
        "github.com",
        "octocat",
        "repo",
        "ciphertext",
        moment(),
    )
    .unwrap();
    txn.commit().unwrap();

    let listed = forge_accounts::account_status_op(&built, json!({})).unwrap();
    let accounts = listed["accounts"].as_array().unwrap();
    assert_eq!(accounts.len(), 1);
    assert_eq!(accounts[0]["login"], "octocat");
    assert_eq!(accounts[0]["linked"], true);
    assert!(accounts[0].get("encrypted_token").is_none());

    let one = forge_accounts::account_status_op(&built, json!({"host": "github.com"})).unwrap();
    assert_eq!(one["accounts"].as_array().unwrap().len(), 1);
    let none = forge_accounts::account_status_op(&built, json!({"host": "other.example"})).unwrap();
    assert_eq!(none["accounts"].as_array().unwrap().len(), 0);
}

/// Unlink deletes the stored account and records that one was linked. An
/// unknown host is refused, and unlinking twice reports that nothing was.
#[test]
fn forge_account_unlink_removes_the_stored_account() {
    use crate::application::services::forge_accounts;
    let built = opened();
    let Built::StepSequential(ctx) = &built else {
        panic!("expected a step clock");
    };
    let actor = ctx
        .declared
        .read()
        .unwrap()
        .actor_by_identity(&principal().identity_ref)
        .unwrap()
        .unwrap();
    let mut txn = ctx.declared.write().unwrap();
    txn.upsert_forge_account(
        &actor.id,
        "github.com",
        "octocat",
        "repo",
        "ciphertext",
        moment(),
    )
    .unwrap();
    txn.commit().unwrap();

    let refused = forge_accounts::account_unlink_op(
        &built,
        json!({"host": "no.such.host", "reason": "tidying"}),
    );
    match refused {
        Err(VogtError::InvalidRequest(message)) => assert_eq!(
            message,
            "'no.such.host' has no provider to validate a token against; \
             configure the host under forge_token_files first"
        ),
        other => panic!("unexpected {other:?}"),
    }
    let blank = forge_accounts::account_unlink_op(&built, json!({"host": "", "reason": "tidying"}));
    match blank {
        Err(VogtError::InvalidRequest(message)) => assert_eq!(
            message,
            "'' has no provider to validate a token against; \
             configure the host under forge_token_files first"
        ),
        other => panic!("unexpected {other:?}"),
    }
    let missing = forge_accounts::account_unlink_op(&built, json!({"reason": "tidying"}));
    match missing {
        Err(VogtError::InvalidRequest(message)) => assert_eq!(message, "host is required"),
        other => panic!("unexpected {other:?}"),
    }

    let unlinked = forge_accounts::account_unlink_op(
        &built,
        json!({"host": "github.com", "reason": "the token leaked"}),
    )
    .unwrap();
    assert_eq!(unlinked["linked"], false);
    assert!(unlinked["login"].is_null());
    let after = forge_accounts::account_status_op(&built, json!({})).unwrap();
    assert_eq!(after["accounts"].as_array().unwrap().len(), 0);

    let again = forge_accounts::account_unlink_op(
        &built,
        json!({"host": "github.com", "reason": "nothing left"}),
    )
    .unwrap();
    assert_eq!(again["linked"], false);
}

/// A write-back policy needs a linked project, and the stored value is the
/// policy the caller named.
#[test]
fn forge_writeback_sets_the_policy_on_a_linked_project() {
    use crate::application::services::forge_writeback;
    use crate::core::{LinkState, Project, WriteBack};
    let built = opened();
    let Built::StepSequential(ctx) = &built else {
        panic!("expected a step clock");
    };
    let mut project = Project::new("prj_beta", "beta", "beta", "/tmp", moment());
    project.link_state = LinkState::Linked;
    project.write_back = WriteBack::Disabled;
    let mut txn = ctx.declared.write().unwrap();
    txn.insert_project(&project).unwrap();
    txn.commit().unwrap();

    let set = forge_writeback::writeback_op(
        &built,
        json!({"project": "beta", "policy": "comment_only", "reason": "comments only"}),
    )
    .unwrap();
    assert_eq!(set["project"]["write_back"], "comment_only");

    // An unlinked project takes the policy too. Python stores it and the
    // consequence waits for the later link; see the dedicated test.
    let unlinked = Project::new("prj_plain", "plain", "plain", "/tmp", moment());
    let mut txn = ctx.declared.write().unwrap();
    txn.insert_project(&unlinked).unwrap();
    txn.commit().unwrap();
    let set_unlinked = forge_writeback::writeback_op(
        &built,
        json!({"project": "plain", "policy": "full", "reason": "too soon"}),
    )
    .unwrap();
    assert_eq!(set_unlinked["project"]["write_back"], "full");
    assert_eq!(set_unlinked["project"]["link_state"], "unlinked");

    let listed = forge_writeback::writeback_list_op(&built, json!({})).unwrap();
    assert!(listed["actions"].as_array().unwrap().is_empty());
}

/// A host outside the configured set is refused, not listed as empty.
#[test]
fn forge_repos_refuses_an_unconfigured_host() {
    use crate::application::services::forge_repos;
    let built = opened();
    let refused = forge_repos::repos_op(&built, json!({"host": "no.such.host"}));
    match refused {
        Err(VogtError::InvalidRequest(message)) => {
            assert!(message.contains("'no.such.host'"));
            assert!(message.contains("forge_token_files"));
        }
        other => panic!("unexpected {other:?}"),
    }
}

/// `forge.writeback` stores the policy whether or not the project is linked,
/// and the audit summary names the slug. Python's `set_write_back` has no
/// linked-project check, and the parity step exits 0.
#[test]
fn forge_writeback_sets_the_policy_on_an_unlinked_project() {
    use crate::application::services::forge_writeback;
    use crate::core::Project;
    let built = opened();
    let Built::StepSequential(ctx) = &built else {
        panic!("expected a step clock");
    };
    let project = Project::new("prj_plain", "plain", "plain", "/tmp", moment());
    let mut txn = ctx.declared.write().unwrap();
    txn.insert_project(&project).unwrap();
    txn.commit().unwrap();

    let set = forge_writeback::writeback_op(
        &built,
        json!({"project": "plain", "policy": "full", "reason": "arm it early"}),
    )
    .unwrap();
    assert_eq!(set["project"]["write_back"], "full");

    let events = ctx
        .declared
        .read()
        .unwrap()
        .list_events(0, 10, None)
        .unwrap();
    let summary = &events
        .iter()
        .find(|event| event.kind == "project.writeback_set")
        .expect("the set is audited")
        .summary;
    assert_eq!(summary["slug"], "plain");
    assert_eq!(summary["from"], "none");
    assert_eq!(summary["to"], "full");

    for limit in [0, 501] {
        let refused = forge_writeback::writeback_list_op(&built, json!({"limit": limit}));
        assert!(
            matches!(refused, Err(VogtError::InvalidRequest(_))),
            "limit {limit}: {refused:?}"
        );
    }
    assert!(forge_writeback::writeback_list_op(&built, json!({"limit": 1})).is_ok());
}

/// Linking loads the cipher before it ever talks to the forge, and both a
/// missing provider and a forge that names no identity are one
/// `InvalidRequest`. The token never appears in the result or the audit.
#[test]
fn forge_account_link_checks_the_host_then_the_cipher_and_never_echoes_the_token() {
    use crate::application::services::forge_accounts;

    let unconfigured = match opened() {
        Built::StepSequential(ctx) => {
            Built::StepSequential(ctx.with_forge_transport(|_, _, _, _| {
                panic!("the forge was called before the cipher check");
            }))
        }
        other => other,
    };
    // A supported host reaches the cipher check: the host is checked first, as
    // Python's link does, so an unsupported host is refused before the key.
    let refused = forge_accounts::account_link_op(
        &unconfigured,
        json!({"host": "github.com", "token": "ghp_secret_value", "reason": "link me"}),
    );
    match refused {
        Err(VogtError::ForgeAccountsNotConfigured(_)) => {}
        other => panic!("expected the cipher refusal first, got {other:?}"),
    }
    let keyed = opened_with_account_key();
    let refused = forge_accounts::account_link_op(
        &keyed,
        json!({"host": "no.such.host", "token": "ghp_secret_value", "reason": "link me"}),
    );
    match refused {
        Err(VogtError::InvalidRequest(message)) => {
            assert!(message.contains("'no.such.host'"), "{message}")
        }
        other => panic!("unexpected {other:?}"),
    }

    let keyed = match opened_with_account_key() {
        Built::StepSequential(ctx) => {
            Built::StepSequential(ctx.with_forge_transport(|url, _, _, _| {
                if url.ends_with("/user") {
                    (200, b"{}".to_vec(), std::collections::BTreeMap::new())
                } else {
                    (404, b"{}".to_vec(), std::collections::BTreeMap::new())
                }
            }))
        }
        other => other,
    };
    let refused = forge_accounts::account_link_op(
        &keyed,
        json!({"host": "github.com", "token": "ghp_secret_value", "reason": "link me"}),
    );
    match refused {
        Err(VogtError::InvalidRequest(message)) => {
            assert_eq!(
                message,
                "the forge did not accept this token; it did not return an identity"
            )
        }
        other => panic!("unexpected {other:?}"),
    }

    let keyed = match opened_with_account_key() {
        Built::StepSequential(ctx) => {
            Built::StepSequential(ctx.with_forge_transport(|url, _, _, _| {
                if url.ends_with("/user") {
                    let mut headers = std::collections::BTreeMap::new();
                    headers.insert("x-oauth-scopes".to_owned(), "repo".to_owned());
                    (200, br#"{"login":"ada"}"#.to_vec(), headers)
                } else {
                    (200, b"{}".to_vec(), std::collections::BTreeMap::new())
                }
            }))
        }
        other => other,
    };
    let linked = forge_accounts::account_link_op(
        &keyed,
        json!({"token": "ghp_secret_value", "reason": "link me"}),
    )
    .unwrap();
    assert_eq!(linked["login"], "ada");
    assert_eq!(linked["host"], "github.com");
    assert_eq!(linked["linked"], true);
    let rendered = linked.to_string();
    assert!(!rendered.contains("ghp_secret_value"), "{rendered}");

    let Built::StepSequential(ctx) = &keyed else {
        unreachable!("opened builds a step clock");
    };
    let events = ctx
        .declared
        .read()
        .unwrap()
        .list_events(0, 10, None)
        .unwrap();
    let audited = events
        .iter()
        .find(|event| event.kind == "forge.account_linked")
        .expect("the link is audited");
    let audited_text = serde_json::to_string(&audited.summary).unwrap();
    assert!(!audited_text.contains("ghp_secret_value"), "{audited_text}");
    let status = forge_accounts::account_status_op(&keyed, json!({})).unwrap();
    assert!(!status.to_string().contains("ghp_secret_value"));
    assert_eq!(status["accounts"][0]["login"], "ada");
}

/// A linked account whose ciphertext cannot be read raises. It does not fall
/// back to the instance token while still reporting the linked login.
#[test]
fn forge_repos_raises_when_the_linked_token_cannot_be_decrypted() {
    use crate::application::services::forge_repos;
    let built = opened_with_account_key();
    let Built::StepSequential(ctx) = &built else {
        unreachable!("opened builds a step clock");
    };
    let actor = ctx
        .declared
        .read()
        .unwrap()
        .actor_by_identity("local:test-user")
        .unwrap()
        .expect("bootstrap created the principal");
    let mut txn = ctx.declared.write().unwrap();
    txn.upsert_forge_account(
        &actor.id,
        "github.com",
        "ada",
        "",
        "not-a-fernet-token",
        moment(),
    )
    .unwrap();
    txn.commit().unwrap();

    let refused = forge_repos::repos_op(&built, json!({}));
    assert!(
        matches!(refused, Err(VogtError::ForgeAccountsNotConfigured(_))),
        "expected the decrypt refusal, got {refused:?}"
    );
}

/// A data directory plus a real Fernet key file, so linking is configured.
fn opened_with_account_key() -> Built {
    let dir = std::env::temp_dir().join(format!("vogt-s7-{}-{}", std::process::id(), unique()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let key_file = dir.join("account.key");
    std::fs::write(&key_file, "MDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDA=").unwrap();
    let config = crate::config::VogtConfig {
        data_dir: dir,
        forge_account_key_file: Some(key_file),
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

/// Onboarding reads a project's history through the stand-in and writes
/// nothing upstream: every request is a GET, and the result says what it read.
/// An unsupported host is refused before any adapter is built.
#[test]
fn forge_onboard_reads_history_and_refuses_an_unsupported_host() {
    use crate::application::services::forge_onboard;
    use crate::config::VogtConfig;
    use crate::core::Project;

    let dir =
        std::env::temp_dir().join(format!("vogt-onboard-{}-{}", std::process::id(), unique()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let token = dir.join("forge-token");
    std::fs::write(&token, "secret-token\n").unwrap();
    let mut clock = Some(crate::core::StepClock::new(moment()));
    let mut ids = Some(crate::core::SequentialIds::new(None).unwrap());
    crate::application::instance::init(&dir, &mut clock, &mut ids).unwrap();
    let built = crate::application::context::build_context(
        VogtConfig {
            data_dir: dir.clone(),
            github_token_file: Some(token),
            ..VogtConfig::default()
        },
        Some(principal()),
        clock,
        ids,
        None,
        None,
        None,
        None,
    )
    .unwrap();

    let standin = crate::application::forge_standin::StandInForge::one_of_each();
    let transport = standin.transport();
    let Built::StepSequential(ctx) = built else {
        panic!("expected a step clock");
    };
    let built =
        Built::StepSequential(ctx.with_forge_transport(move |url, headers, body, method| {
            transport(url, headers, body, method)
        }));

    let mut github = Project::new("prj_widget", "widget", "widget", "/tmp", moment());
    github.repo_url = Some("https://github.com/acme/widget".to_owned());
    let mut stray = Project::new("prj_stray", "stray", "stray", "/tmp", moment());
    stray.repo_url = Some("https://no.such.host/acme/widget".to_owned());
    {
        let Built::StepSequential(ctx) = &built else {
            panic!("expected a step clock");
        };
        let mut txn = ctx.declared.write().unwrap();
        txn.insert_project(&github).unwrap();
        txn.insert_project(&stray).unwrap();
        txn.commit().unwrap();
    }

    let refused = forge_onboard::onboard_op(
        &built,
        json!({"project": "stray", "reason": "not a forge we read"}),
    )
    .unwrap();
    assert_eq!(refused["supported"], false);
    assert_eq!(refused["mutations"], 0);
    assert!(refused["detail"]
        .as_str()
        .is_some_and(|text| !text.is_empty()));
    assert!(
        standin.requests().is_empty(),
        "an unsupported host is never read"
    );

    let done = forge_onboard::onboard_op(
        &built,
        json!({"project": "widget", "reason": "backfill the history"}),
    )
    .unwrap();
    assert_eq!(done["supported"], true);
    assert_eq!(done["mutations"], 0);
    assert_eq!(done["issues"], 1, "{done}");
    assert_eq!(done["labels"], 1, "{done}");
    assert_eq!(done["releases"], 1, "{done}");
    let requests = standin.requests();
    assert!(!requests.is_empty());
    assert!(
        requests.iter().all(|request| request.starts_with("GET ")),
        "onboarding only reads: {requests:?}"
    );

    // Every return path is attributed, the refused one and the run alike.
    let audited = crate::application::services::history::audit_list_op(
        &built,
        json!({"operation": "forge.onboard"}),
    )
    .unwrap();
    let rows = audited["records"]
        .as_array()
        .or_else(|| audited["audit"].as_array())
        .or_else(|| audited.as_array())
        .cloned()
        .unwrap_or_default();
    assert_eq!(rows.len(), 2, "{audited}");

    let zero = forge_onboard::onboard_op(
        &built,
        json!({"project": "widget", "reason": "no pages", "max_pages": 0}),
    );
    assert!(
        matches!(zero, Err(VogtError::InvalidRequest(_))),
        "{zero:?}"
    );
}
