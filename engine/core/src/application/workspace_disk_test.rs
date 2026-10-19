//! Ports `tests/test_workspace_disk.py`: the workspace-volume row (WI-1304)
//! that `status` and `sweep` carry as `disk` when the session engine answers
//! `GET /api/status`, and leave absent when there is no engine or it cannot
//! say. The `engine.status` cases are not ported: that operation is not yet
//! ported to the Rust core.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::adapters::engine::{EngineClient, Transport};
use crate::application::collect::sweep_op;
use crate::application::context::{build_context, Built};
use crate::application::instance::status_op;
use crate::core::{ActorKind, Moment, Principal, SequentialIds, StepClock};
use crate::storage::interface::{DeclaredStore, ObservedStore};

fn disk() -> Value {
    json!({
        "path": "/w",
        "total_bytes": 1000,
        "free_bytes": 100,
        "used_percent": 90,
        "threshold_percent": 85,
        "needs_attention": true,
    })
}

fn unique() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

/// An engine that answers `GET /api/status` with `answer`; anything else is
/// not this test's and fails like an engine that is down.
fn engine(answer: (u16, String)) -> EngineClient {
    let transport: Transport = Arc::new(move |url, _headers, _body, method| {
        if method == "GET" && url == "http://engine/api/status" {
            (answer.0, answer.1.clone().into_bytes())
        } else {
            (503, b"{}".to_vec())
        }
    });
    EngineClient::new("http://engine", None, Some(transport))
}

fn opened(client: Option<EngineClient>) -> Built {
    let dir = std::env::temp_dir().join(format!("vogt-disk-{}-{}", std::process::id(), unique()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let config = crate::config::VogtConfig {
        data_dir: dir,
        ..crate::config::VogtConfig::default()
    };
    let principal = Principal::new("local:test-user", ActorKind::Human, "Test").unwrap();
    let built = build_context(
        config,
        Some(principal.clone()),
        Some(StepClock::new(Moment::from_unix(1_700_000_000, 0))),
        Some(SequentialIds::new(None).unwrap()),
        None,
        client,
        None,
        None,
    )
    .unwrap();
    let Built::StepSequential(ctx) = &built else {
        panic!("expected a step clock");
    };
    ctx.declared.migrate().unwrap();
    ctx.declared.bootstrap(&principal).unwrap();
    ctx.observed.migrate().unwrap();
    built
}

fn surface(built: &Built, which: &str) -> Value {
    let out = match which {
        "status" => status_op(built, json!({})),
        _ => sweep_op(built, json!({"reason": "test"})),
    };
    out.unwrap()["disk"].clone()
}

#[test]
fn status_and_sweep_carry_the_disk_row() {
    for which in ["status", "sweep"] {
        let built = opened(Some(engine((200, json!({"disk": disk()}).to_string()))));
        let row = surface(&built, which);
        assert_eq!(row["needs_attention"], json!(true), "{which}");
        assert_eq!(row["free_bytes"], json!(100), "{which}");
        assert_eq!(row["used_percent"], json!(90), "{which}");
    }
}

#[test]
fn the_row_is_absent_when_the_engine_cannot_say() {
    for which in ["status", "sweep"] {
        // no engine, engine unavailable, engine answering garbage
        let cases: [(&str, Option<EngineClient>); 3] = [
            ("no-engine", None),
            ("engine-unavailable", Some(engine((503, "{}".to_string())))),
            ("engine-error", Some(engine((200, "[]".to_string())))),
        ];
        for (id, client) in cases {
            let built = opened(client);
            assert_eq!(surface(&built, which), Value::Null, "{which} {id}");
        }
    }
}

#[test]
fn a_status_without_a_disk_object_has_no_row() {
    for answer in [
        json!({"version": "1"}),
        json!({"version": "1", "disk": "x"}),
    ] {
        let built = opened(Some(engine((200, answer.to_string()))));
        assert_eq!(surface(&built, "status"), Value::Null, "{answer}");
    }
}
