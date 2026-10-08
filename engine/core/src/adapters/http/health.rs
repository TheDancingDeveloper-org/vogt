//! Health routes. Ports `src/vogt/adapters/http/health.py`.
//!
//! `/health` and `/health/live` are liveness. `/health/ready` reports both
//! schema versions and the instance id. `detail` is always present: pydantic
//! emits it as `null` when unset, and dropping the key is a wire difference.

use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;

use sha2::{Digest, Sha256};

use crate::storage::sqlite::connection::connect;
use crate::storage::sqlite::migrator::{self, MigrateError};
use crate::storage::sqlite::{declared_path, observed_path};

#[derive(Clone)]
pub struct HealthState {
    pub data_dir: PathBuf,
    pub version: String,
}

pub fn router(state: HealthState) -> Router {
    Router::new()
        .route("/health", get(live))
        .route("/health/live", get(live))
        .route("/health/ready", get(ready))
        .with_state(Arc::new(state))
}

#[derive(Serialize)]
struct Liveness {
    status: &'static str,
    version: String,
}

async fn live(State(state): State<Arc<HealthState>>) -> Json<Liveness> {
    Json(Liveness {
        status: "ok",
        version: state.version.clone(),
    })
}

#[derive(Serialize)]
struct Readiness {
    status: &'static str,
    detail: Option<String>,
    declared_schema_version: i64,
    observed_schema_version: i64,
    declared_schema_expected: i64,
    observed_schema_expected: i64,
    instance_id: Option<String>,
}

async fn ready(State(state): State<Arc<HealthState>>) -> (StatusCode, Json<Readiness>) {
    match readiness(&state.data_dir) {
        Ok(body) if body.status == "ready" => (StatusCode::OK, Json(body)),
        Ok(body) => (StatusCode::SERVICE_UNAVAILABLE, Json(body)),
        Err(err) => {
            let reference = error_reference(&err.to_string());
            eprintln!("vogt-core health: {err} (ref {reference})");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(Readiness {
                    status: "not_ready",
                    detail: Some(format!("internal error (ref {reference})")),
                    declared_schema_version: 0,
                    observed_schema_version: 0,
                    declared_schema_expected: 0,
                    observed_schema_expected: 0,
                    instance_id: None,
                }),
            )
        }
    }
}

fn error_reference(detail: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(detail.as_bytes());
    format!("{:x}", hasher.finalize())[..12].to_string()
}

fn readiness(data_dir: &std::path::Path) -> Result<Readiness, MigrateError> {
    let root = migrator::migrations_root();
    let declared = connect(&declared_path(data_dir))?;
    let observed = connect(&observed_path(data_dir))?;
    let declared_version = migrator::applied_version(&declared)?;
    let observed_version = migrator::applied_version(&observed)?;
    let declared_expected = migrator::bundled_version(
        "declared",
        root.as_deref().map(|path| path.join("declared")).as_deref(),
    )?;
    let observed_expected = migrator::bundled_version(
        "observed",
        root.as_deref().map(|path| path.join("observed")).as_deref(),
    )?;
    let mut behind = Vec::new();
    if declared_version < declared_expected {
        behind.push(format!(
            "declared schema is at {declared_version}, this build expects {declared_expected}"
        ));
    }
    if observed_version < observed_expected {
        behind.push(format!(
            "observed schema is at {observed_version}, this build expects {observed_expected}"
        ));
    }
    Ok(Readiness {
        status: if behind.is_empty() {
            "ready"
        } else {
            "not_ready"
        },
        detail: if behind.is_empty() {
            None
        } else {
            Some(format!("{} — run `vogt migrate`", behind.join("; ")))
        },
        declared_schema_version: declared_version,
        observed_schema_version: observed_version,
        declared_schema_expected: declared_expected,
        observed_schema_expected: observed_expected,
        instance_id: instance_id(data_dir),
    })
}

/// The id bootstrap wrote into `meta`. Absent until that chunk lands, which is
/// reported as null rather than invented.
fn instance_id(data_dir: &std::path::Path) -> Option<String> {
    let conn = connect(&declared_path(data_dir)).ok()?;
    conn.query_row(
        "SELECT value FROM meta WHERE key = 'instance_id'",
        [],
        |row| row.get(0),
    )
    .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ready_probe_carries_detail_null_and_the_instance_id() {
        let dir = std::env::temp_dir().join(format!("vogt-health-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let now = "2026-10-08T12:00:00+00:00";
        crate::application::instance::init(&dir, now).unwrap();
        let conn = connect(&declared_path(&dir)).unwrap();
        conn.execute(
            "INSERT INTO meta (key, value) VALUES ('instance_id', 'ins_01TEST')",
            [],
        )
        .unwrap();
        drop(conn);

        let body = readiness(&dir).unwrap();
        let json = serde_json::to_value(&body).unwrap();
        assert_eq!(json["status"], "ready");
        assert!(json["detail"].is_null());
        assert_eq!(json["instance_id"], "ins_01TEST");
        assert!(json["declared_schema_version"].as_i64().unwrap() > 0);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
