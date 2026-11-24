//! The event feed and the audit log. Ports `history.py`.
//!
//! Both are reads of the declared store and nothing else: querying the record
//! of what happened must not itself become part of it.

use serde_json::{json, Value};

use crate::application::context::AppContext;
use crate::application::services::preferences::{optional_i64, optional_string};
use crate::core::{Clock, IdFactory, Moment};
use crate::errors::VogtError;
use crate::storage::interface::{AuditQuery, DeclaredStore, ReadView};

use super::{context_parts, dispatch, Built};

pub fn events_list_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    dispatch!(built, list_events, params)
}

pub fn audit_list_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    dispatch!(built, list_audit, params)
}

/// Read the cursor-based notification feed. `next_cursor` is the seq of the
/// last row returned, or the caller's own cursor when the feed is empty, so a
/// polling client never rewinds.
fn list_events<C, I>(ctx: &AppContext<C, I>, params: Value) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let after = optional_i64(&params, "after", 0)?;
    let limit = optional_i64(&params, "limit", 100)?;
    let entity_id = optional_string(&params, "entity_id")?;
    let (declared, _, _, _, _, _) = context_parts(ctx);
    let events = declared
        .read()?
        .list_events(after, limit, entity_id.as_deref())?;
    let next_cursor = events.last().map(|event| event.seq).unwrap_or(after);
    Ok(json!({
        "events": events.iter().map(event_json).collect::<Vec<_>>(),
        "next_cursor": next_cursor,
    }))
}

/// Query the audit log. `since` is inclusive and `until` exclusive, so
/// consecutive windows tile the log. `total` counts matches, not the page.
fn list_audit<C, I>(ctx: &AppContext<C, I>, params: Value) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let limit = optional_i64(&params, "limit", 50)?;
    let offset = optional_i64(&params, "offset", 0)?;
    let actor_id = optional_string(&params, "actor_id")?;
    let operation = optional_string(&params, "operation")?;
    let entity_id = optional_string(&params, "entity_id")?;
    let project = optional_string(&params, "project")?;
    let since = optional_moment(&params, "since")?;
    let until = optional_moment(&params, "until")?;

    let (declared, _, _, _, _, _) = context_parts(ctx);
    let view = declared.read()?;
    let project_id = match &project {
        Some(slug) => Some(
            view.project_by_slug(slug)?
                .ok_or_else(|| {
                    VogtError::NotFound(format!(
                        "no project with slug {}",
                        crate::core::py_repr(slug)
                    ))
                })?
                .id,
        ),
        None => None,
    };
    let query = AuditQuery {
        limit,
        offset,
        actor_id,
        operation,
        entity_id,
        project_id,
        since,
        until,
    };
    let records = view.list_audit(&query)?;
    let total = view.count_audit(&query)?;
    Ok(json!({
        "records": records.iter().map(audit_json).collect::<Vec<_>>(),
        "total": total,
    }))
}

/// A moment as pydantic's `mode="json"` spells it, with a Z.
fn at_json(moment: Moment) -> String {
    moment.to_json()
}

fn event_json(event: &crate::core::Event) -> Value {
    json!({
        "seq": event.seq,
        "kind": event.kind,
        "entity_kind": event.entity_kind,
        "entity_id": event.entity_id,
        "actor_id": event.actor_id,
        "audit_id": event.audit_id,
        "summary": event.summary,
        "at": at_json(event.at),
    })
}

fn audit_json(record: &crate::core::AuditRecord) -> Value {
    json!({
        "id": record.id,
        "txn_id": record.txn_id,
        "revision": record.revision,
        "actor_id": record.actor_id,
        "actor_identity_ref": record.actor_identity_ref,
        "operation": record.operation,
        "entity_kind": record.entity_kind,
        "entity_id": record.entity_id,
        "reason": record.reason,
        "payload_digest": record.payload_digest,
        "at": at_json(record.at),
    })
}

fn optional_moment(params: &Value, field: &str) -> Result<Option<Moment>, VogtError> {
    match optional_string(params, field)? {
        None => Ok(None),
        Some(text) if text.is_empty() => Ok(None),
        Some(text) => crate::core::from_iso(&text)
            .map(Some)
            .map_err(VogtError::InvalidRequest),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::context::build_context;
    use crate::core::{ActorKind, Principal, SequentialIds, StepClock};
    use crate::storage::interface::{DeclaredStore, ObservedStore};

    fn opened() -> Built {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "vogt-history-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let built = build_context(
            crate::config::VogtConfig {
                data_dir: dir,
                ..crate::config::VogtConfig::default()
            },
            Some(Principal::new("local:test-user", ActorKind::Human, "Test").unwrap()),
            Some(StepClock::new(Moment::from_unix(1_700_000_000, 0))),
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
                ctx.declared
                    .bootstrap(
                        &Principal::new("local:test-user", ActorKind::Human, "Test").unwrap(),
                    )
                    .unwrap();
                ctx.observed.migrate().unwrap();
            }
            _other => panic!("expected a step clock"),
        }
        built
    }

    fn ctx(built: &Built) -> &AppContext<StepClock, SequentialIds> {
        match built {
            Built::StepSequential(inner) => inner,
            _other => panic!("expected a step clock"),
        }
    }

    fn seed(built: &Built) {
        crate::application::services::initiatives::initiative_create_op(
            built,
            json!({"title": "Seeded", "reason": "seed the feed"}),
        )
        .unwrap();
    }

    #[test]
    fn an_empty_feed_hands_the_cursor_back() {
        let built = opened();
        let result = list_events(ctx(&built), json!({"after": 7})).unwrap();
        assert_eq!(result["events"], json!([]));
        assert_eq!(result["next_cursor"], json!(7));
    }

    #[test]
    fn the_feed_advances_past_what_it_returns() {
        let built = opened();
        seed(&built);
        let result = list_events(ctx(&built), json!({})).unwrap();
        let events = result["events"].as_array().unwrap();
        assert!(!events.is_empty(), "bootstrap writes events");
        assert!(events[0]["at"].as_str().unwrap().ends_with('Z'));
        let last = events.last().unwrap()["seq"].as_i64().unwrap();
        assert_eq!(result["next_cursor"], json!(last));

        let again = list_events(ctx(&built), json!({"after": last})).unwrap();
        assert_eq!(again["events"], json!([]));
        assert_eq!(again["next_cursor"], json!(last));
    }

    #[test]
    fn the_audit_total_counts_matches_not_the_page() {
        let built = opened();
        seed(&built);
        let result = list_audit(ctx(&built), json!({"limit": 1})).unwrap();
        assert_eq!(result["records"].as_array().unwrap().len(), 1);
        assert!(result["total"].as_i64().unwrap() > 1);
        assert!(result["records"][0]["at"].as_str().unwrap().ends_with('Z'));
    }

    #[test]
    fn an_unknown_project_is_not_found() {
        let built = opened();
        let error = list_audit(ctx(&built), json!({"project": "nope"})).unwrap_err();
        assert!(matches!(
            error,
            VogtError::NotFound(message) if message == "no project with slug 'nope'"
        ));
    }
}
