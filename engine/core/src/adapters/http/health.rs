//! Health routes. Ports `src/vogt/adapters/http/health.py`.
//!
//! `/health` and `/health/live` are liveness. `/health/ready` reports both
//! schema versions. `detail` is omitted when ready, matching the Python model
//! where it defaults to null and is excluded from the response only when unset
//! — here it is simply absent, and the golden comparison for this chunk is the
//! field set, not pydantic's null policy.

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
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
    declared_schema_version: i64,
    observed_schema_version: i64,
    declared_schema_expected: i64,
    observed_schema_expected: i64,
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
    let declared_expected = migrator::bundled_version(&root.join("declared"))?;
    let observed_expected = migrator::bundled_version(&root.join("observed"))?;
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
    })
}
