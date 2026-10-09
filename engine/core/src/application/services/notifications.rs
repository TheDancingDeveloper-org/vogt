//! The forge notification list. Ports `application/services/notifications.py`.
//!
//! A notification is a forge observation, not a declared entity, so this is a
//! read: the forge collector writes them and this projects the latest of each
//! thread. There is no write path.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::adapters::forge::KIND_NOTIFICATION;
use crate::application::context::{AppContext, Built};
use crate::application::services::preferences::{optional_i64, optional_string};
use crate::application::services::{context_parts, dispatch, now_of};
use crate::core::{Clock, IdFactory, Observation};
use crate::errors::VogtError;
use crate::storage::interface::{DeclaredStore, ObservedStore, ReadView};

/// How many latest rows the projection reads before filtering. A notification
/// thread is one row, so this is threads, not events.
const SCAN_LIMIT: i64 = 2000;

pub fn notifications_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    dispatch!(built, notifications, params)
}

fn notifications<C, I>(ctx: &AppContext<C, I>, params: Value) -> Result<Value, VogtError>
where
    C: Clock,
    I: IdFactory,
{
    let project = optional_string(&params, "project")?;
    let reason = optional_string(&params, "reason")?;
    let unread_only = params
        .get("unread_only")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let limit = optional_i64(&params, "limit", 100)?;
    let offset = optional_i64(&params, "offset", 0)?;

    let (declared, observed, _, clock, _, _) = context_parts(ctx);
    let view = declared.read()?;
    if !observed.has_evidence_tables()? {
        return Ok(json!({
            "notifications": [],
            "total": 0,
            "by_reason": {},
            "unread": 0,
            "scope": "the GitHub account whose token this instance is configured with; notifications are instance-scoped, not per-actor",
            "freshness": crate::application::services::freshness::freshness_of(
                observed,
                now_of(&clock),
            )?,
            "detail": "no sweep has run; notifications are not collected",
        }));
    }
    let project_id = match project.as_deref() {
        Some(slug) => Some(crate::application::resolve::project(&view, slug)?.id),
        None => None,
    };
    let slugs: BTreeMap<String, String> = view
        .list_projects(10_000, 0)?
        .into_iter()
        .map(|project| (project.id, project.slug))
        .collect();

    let mut rows = observed.latest(
        &[KIND_NOTIFICATION.to_string()],
        project_id.as_deref(),
        false,
        false,
        SCAN_LIMIT,
    )?;
    if let Some(reason) = reason.as_deref() {
        rows.retain(|row| text_of(&row.payload, "reason").as_deref() == Some(reason));
    }
    if unread_only {
        rows.retain(|row| {
            row.payload
                .get("unread")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        });
    }
    rows.sort_by_key(|row| std::cmp::Reverse(stamp_of(row)));

    let mut by_reason: BTreeMap<String, i64> = BTreeMap::new();
    for row in &rows {
        let key = text_of(&row.payload, "reason").unwrap_or_else(|| "unknown".to_string());
        *by_reason.entry(key).or_insert(0) += 1;
    }
    let total = rows.len() as i64;
    let unread = rows
        .iter()
        .filter(|row| {
            row.payload
                .get("unread")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        })
        .count() as i64;
    let start = offset.max(0) as usize;
    let window = rows
        .iter()
        .skip(start)
        .take(limit.max(0) as usize)
        .map(|row| notification_json(row, &slugs))
        .collect::<Vec<_>>();
    let detail = if total == 0 {
        Some(
            "nothing collected — either there is nothing to say, or the configured token cannot read notifications; `coverage` distinguishes the two",
        )
    } else {
        None
    };
    Ok(json!({
        "notifications": window,
        "total": total,
        "by_reason": by_reason,
        "unread": unread,
        "scope": "the GitHub account whose token this instance is configured with; notifications are instance-scoped, not per-actor",
        "freshness": crate::application::services::freshness::freshness_of(observed, now_of(&clock))?,
        "detail": detail,
    }))
}

fn notification_json(row: &Observation, slugs: &BTreeMap<String, String>) -> Value {
    let payload = &row.payload;
    json!({
        "thread": text_of(payload, "thread").unwrap_or_default(),
        "project_slug": row.project_id.as_ref().and_then(|id| slugs.get(id).cloned()),
        "repo": text_of(payload, "repo").unwrap_or_default(),
        "title": text_of(payload, "title").unwrap_or_default(),
        "reason": text_of(payload, "reason").unwrap_or_default(),
        "subject_type": text_of(payload, "subject_type").unwrap_or_default(),
        "unread": payload.get("unread").and_then(Value::as_bool).unwrap_or(false),
        "url": row.source_url,
        "updated_at": text_of(payload, "updated_at"),
        "observed_at": row.observed_at.to_json(),
    })
}

/// Newest first. A thread with no `updated_at` sorts by when Vogt saw it.
fn stamp_of(row: &Observation) -> String {
    text_of(&row.payload, "updated_at").unwrap_or_else(|| row.observed_at.to_json())
}

fn text_of(payload: &Value, key: &str) -> Option<String> {
    payload.get(key).and_then(Value::as_str).map(str::to_string)
}
