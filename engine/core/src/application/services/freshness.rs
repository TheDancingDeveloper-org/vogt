//! How old the evidence behind an aggregating answer is. Ports `freshness_of`
//! in `src/vogt/application/services/views.py`.
//!
//! An answer is exactly as fresh as the least fresh collector it depends on,
//! so the stamp is the oldest relevant sweep. A collector that has never
//! finished makes the whole answer `partial` rather than quietly not counting.
//! `project.brief`, `place.metrics`, `deployed.versions` and the notification
//! list all read this one shape.

use serde_json::{json, Map, Value};

use crate::core::Moment;
use crate::errors::VogtError;
use crate::storage::interface::ObservedStore;

pub fn freshness_of(observed: &impl ObservedStore, now: Moment) -> Result<Value, VogtError> {
    if !observed.has_evidence_tables()? {
        return Ok(never_swept(
            "no sweep has run; observed subjects are not collected",
        ));
    }
    let newest = observed.coverage()?;
    if newest.is_empty() {
        return Ok(never_swept("no collector has completed a sweep yet"));
    }
    // Sorted by name, as Python's `sorted(newest.items())` does, so two runs
    // agree on the collector order.
    let mut names: Vec<&String> = newest.keys().collect();
    names.sort();
    let mut collectors = Map::new();
    let mut oldest: Option<Moment> = None;
    for name in names {
        let sweep = &newest[name];
        let finished = sweep.finished_at.unwrap_or(sweep.started_at);
        let age = now.seconds_since(finished) as i64;
        collectors.insert(
            name.clone(),
            json!(format!("{age}s ago ({})", sweep.outcome)),
        );
        if oldest.is_none_or(|so_far| finished < so_far) {
            oldest = Some(finished);
        }
    }
    let partial = newest
        .values()
        .any(|sweep| sweep.outcome.to_string() != "ok");
    Ok(json!({
        "status": if partial { "partial" } else { "fresh" },
        "oldest_relevant_sweep": oldest.map(|moment| moment.to_json()),
        "age_seconds": oldest.map(|moment| now.seconds_since(moment) as i64),
        "collectors": collectors,
        "detail": if partial {
            Value::String(
                "at least one collector reported a partial or failed sweep".to_string(),
            )
        } else {
            Value::Null
        },
    }))
}

fn never_swept(detail: &str) -> Value {
    json!({
        "status": "never_swept",
        "oldest_relevant_sweep": Value::Null,
        "age_seconds": Value::Null,
        "collectors": {},
        "detail": detail,
    })
}
