//! Ports `tests/test_workspace_disk.py`: the workspace-volume row (WI-1304)
//! that `status` and `sweep` carry as `disk` when the session engine answers
//! `GET /api/status`, and leave absent when a configured engine cannot say.
//! With no engine at all the row stays absent: Python returns null, and the
//! core does not invent a local stat. `engine.status` carries the engine's
//! row beside the rest of its report. A row pydantic's `EngineDiskStatus`
//! would refuse (`{"used_percent": "x"}`) is no row on every surface.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::adapters::engine::{EngineClient, Transport};
use crate::application::collect::sweep_op;
use crate::application::context::{build_context, Built};
use crate::application::instance::status_op;
use crate::application::services::engine_status::engine_status_op;
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
        // A configured engine that is down or answers garbage is "not
        // collected". No engine is the same: Python returns null rather than
        // a locally invented stat.
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

fn engine_status(built: &Built) -> Value {
    engine_status_op(built, json!({})).unwrap()
}

#[test]
fn engine_status_carries_the_disk_row() {
    let built = opened(Some(engine((
        200,
        json!({"version": "1", "disk": disk()}).to_string(),
    ))));
    let out = engine_status(&built);
    assert_eq!(out["engine"], Value::Null);
    assert_eq!(out["version"], json!("1"));
    let row = &out["disk"];
    assert_eq!(row["used_percent"], json!(90));
    assert_eq!(row["threshold_percent"], json!(85));
    assert_eq!(row["needs_attention"], json!(true));
}

#[test]
fn engine_status_without_a_disk_row_says_none() {
    let built = opened(Some(engine((200, json!({"version": "1"}).to_string()))));
    assert_eq!(engine_status(&built)["disk"], Value::Null);
    let malformed = opened(Some(engine((
        200,
        json!({"version": "1", "disk": {"used_percent": "x"}}).to_string(),
    ))));
    let out = engine_status(&malformed);
    assert_eq!(out["disk"], Value::Null);
    assert_eq!(out["version"], json!("1"));
}

#[test]
fn a_malformed_disk_row_is_no_row_on_status_and_sweep() {
    for which in ["status", "sweep"] {
        for row in [
            json!({"used_percent": "x"}),
            json!({"needs_attention": null}),
            json!({"path": 1}),
            json!({"used_percent": 90, "unknown": 1}),
        ] {
            let built = opened(Some(engine((200, json!({"disk": row}).to_string()))));
            assert_eq!(surface(&built, which), Value::Null, "{which} {row}");
        }
    }
}

#[test]
fn a_disk_row_is_read_as_pydantic_reads_it() {
    // Lax coercion and defaults: "90" is 90, a missing field takes its default,
    // and the row always carries every field.
    let built = opened(Some(engine((
        200,
        json!({"disk": {"used_percent": "90", "free_bytes": 100.0}}).to_string(),
    ))));
    let want = json!({
        "path": null, "total_bytes": null, "free_bytes": 100,
        "used_percent": 90, "threshold_percent": null, "needs_attention": false,
    });
    assert_eq!(surface(&built, "status"), want);
    assert_eq!(engine_status(&built)["disk"], want);
}

#[test]
fn engine_status_says_why_when_the_engine_cannot_be_asked() {
    let none = engine_status(&opened(None));
    assert_eq!(
        none["engine"],
        json!("no session engine is configured (VOGT_ENGINE_URL is unset)")
    );
    for (id, answer) in [
        ("engine-unavailable", (503, "{}".to_string())),
        ("engine-garbage", (200, "[]".to_string())),
    ] {
        let out = engine_status(&opened(Some(engine(answer))));
        assert!(
            out["engine"].as_str().is_some_and(|s| !s.is_empty()),
            "{id} {out}"
        );
        assert_eq!(out["version"], Value::Null, "{id}");
        assert_eq!(out["disk"], Value::Null, "{id}");
        assert_eq!(out["event_lag"], json!({}), "{id}");
    }
}

#[test]
fn engine_status_reads_the_report_as_python_does() {
    let built = opened(Some(engine((
        200,
        json!({
            "product_version": "",
            "version": "0.7.20",
            "source_sha": "abc",
            "release_url": 5,
            "session_count": 3,
            "push_subscription_count": true,
            "gui_process_count": 2.0,
            "fcm_enabled": false,
            "gui_stream_configured": "yes",
            "auth_broker": {"auto_agent_auth": true},
            "storage": {"state_dir": "/s", "workspace_root": ""},
            "history": {"enabled": "true", "log_bytes": "12"},
            "agent_tasks": {"task_count": 4},
            "event_lag": {"sse": {"episodes": 1, "events_skipped": 9}, "skip": 3},
        })
        .to_string(),
    ))));
    assert_eq!(
        engine_status(&built),
        json!({
            "engine": null,
            "version": "0.7.20",
            "source_ref": null,
            "source_sha": "abc",
            "release_url": null,
            "session_count": 3,
            "push_subscription_count": null,
            "gui_process_count": null,
            "gui_stream_configured": null,
            "fcm_enabled": false,
            "auto_agent_auth": true,
            "state_dir": "/s",
            "workspace_root": null,
            "history": {
                "enabled": true, "archived_session_count": null,
                "log_file_count": null, "log_bytes": 12, "db_bytes": null,
            },
            "agent_tasks": {
                "task_count": 4, "prompt_task_dir_count": 0, "prompt_file_count": 0,
                "context_file_count": 0, "session_prompt_file_count": 0,
                "prompt_bytes": 0, "orphan_task_dir_count": 0,
            },
            "event_lag": {"sse": {"episodes": 1, "events_skipped": 9}},
            "disk": null,
        })
    );
}
