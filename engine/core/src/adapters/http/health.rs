//! Health routes. Ports `src/vogt/adapters/http/health.py`.
//!
//! `/health/live` is liveness. `/health/ready` reports both schema versions and
//! the instance id. `/version` and `/connection-info` are what the engine proxies
//! and the Setup Wizard points at. Python has no bare `/health`, so neither do we.

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
    /// Whether bearer authentication is on. Reported by `/connection-info`.
    pub auth_enabled: bool,
    /// Whether writes are accepted. Reported by `/connection-info`.
    pub writes_enabled: bool,
}

pub fn router(state: HealthState) -> Router {
    Router::new()
        .route("/health/live", get(live))
        .route("/health/ready", get(ready))
        .route("/version", get(version))
        .route("/connection-info", get(connection_info))
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
struct VersionBody {
    version: String,
    name: &'static str,
}

async fn version(State(state): State<Arc<HealthState>>) -> Json<VersionBody> {
    Json(VersionBody {
        version: state.version.clone(),
        name: "vogt",
    })
}

#[derive(Serialize)]
struct ConnectionInfo {
    name: &'static str,
    version: String,
    url: Option<String>,
    api_path: String,
    mcp_path: String,
    health_path: &'static str,
    supported_mcp_protocol_versions: &'static [&'static str],
    authentication: &'static str,
    writes_enabled: bool,
}

const SUPPORTED_MCP_PROTOCOL_VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

const API_PATH: &str = "/api";
const MCP_PATH: &str = "/mcp";

fn clean_url(value: Option<&str>) -> Option<String> {
    let trimmed = value?.trim().trim_end_matches('/');
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn clean_path(value: Option<&str>) -> Option<String> {
    let trimmed = value?.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return None;
    }
    Some(if trimmed.starts_with('/') {
        trimmed.to_string()
    } else {
        format!("/{trimmed}")
    })
}

async fn connection_info(
    State(state): State<Arc<HealthState>>,
    headers: axum::http::HeaderMap,
) -> Json<ConnectionInfo> {
    let config = crate::config::load_config(&serde_json::Map::new()).ok();
    let fronted = config.as_ref().is_some_and(|config| config.fronted);
    let header = |name: &str| headers.get(name).and_then(|value| value.to_str().ok());
    // Unfronted, the headers are ignored entirely: anyone who can reach the
    // core must not be able to tell it that it lives somewhere else.
    let url = fronted
        .then(|| clean_url(header("x-vogt-public-url")))
        .flatten()
        .or_else(|| clean_url(config.as_ref().and_then(|c| c.public_url.as_deref())));
    let api_path = fronted
        .then(|| clean_path(header("x-vogt-api-path")))
        .flatten()
        .unwrap_or_else(|| API_PATH.to_string());
    let mcp_path = fronted
        .then(|| clean_path(header("x-vogt-mcp-path")))
        .flatten()
        .unwrap_or_else(|| MCP_PATH.to_string());
    Json(ConnectionInfo {
        name: "vogt",
        version: state.version.clone(),
        url,
        api_path,
        mcp_path,
        health_path: "/health/ready",
        supported_mcp_protocol_versions: &SUPPORTED_MCP_PROTOCOL_VERSIONS,
        authentication: if state.auth_enabled {
            "bearer token"
        } else {
            "none (loopback)"
        },
        writes_enabled: state.writes_enabled,
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

/// The id bootstrap wrote into `meta`. Null only on a database that was never
/// bootstrapped.
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
    use std::sync::Mutex;

    static ENV: Mutex<()> = Mutex::new(());

    #[test]
    fn a_ready_probe_carries_detail_null_and_the_instance_id() {
        let _guard = ENV.lock().unwrap();
        let dir = std::env::temp_dir().join(format!("vogt-health-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        unsafe { std::env::set_var("VOGT_TEST_IDS", "sequential") };
        let mut clock = None;
        let mut ids = crate::core::ids_from_env(Some("sequential"), None).unwrap();
        crate::application::instance::init(&dir, &mut clock, &mut ids).unwrap();

        let body = readiness(&dir).unwrap();
        let json = serde_json::to_value(&body).unwrap();
        assert_eq!(json["status"], "ready");
        assert!(json["detail"].is_null());
        assert_eq!(json["instance_id"], "ins_0001");
        assert!(json["declared_schema_version"].as_i64().unwrap() > 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn version_and_connection_info_match_python() {
        let _guard = ENV.lock().unwrap();
        let state = HealthState {
            data_dir: PathBuf::from("/unused"),
            version: "0.7.7".to_string(),
            auth_enabled: false,
            writes_enabled: true,
        };
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.spawn(async move {
            let listener = tokio::net::TcpListener::from_std(listener).unwrap();
            axum::serve(listener, router(state)).await.unwrap();
        });
        std::thread::sleep(std::time::Duration::from_millis(100));
        let get = |path: &str, extra: &str| -> String {
            let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
            use std::io::{Read, Write};
            write!(
                stream,
                "GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n{extra}\r\n"
            )
            .unwrap();
            let mut buf = String::new();
            stream.read_to_string(&mut buf).unwrap();
            buf
        };
        let body = |raw: &str| raw.split("\r\n\r\n").nth(1).unwrap().to_string();

        let version: serde_json::Value = serde_json::from_str(&body(&get("/version", ""))).unwrap();
        assert_eq!(
            version,
            serde_json::json!({"version": "0.7.7", "name": "vogt"})
        );

        let info: serde_json::Value =
            serde_json::from_str(&body(&get("/connection-info", ""))).unwrap();
        assert_eq!(
            info,
            serde_json::json!({
                "name": "vogt",
                "version": "0.7.7",
                "url": null,
                "api_path": "/api",
                "mcp_path": "/mcp",
                "health_path": "/health/ready",
                "supported_mcp_protocol_versions": ["2025-06-18", "2025-03-26", "2024-11-05"],
                "authentication": "none (loopback)",
                "writes_enabled": true
            })
        );
        let ignored: serde_json::Value = serde_json::from_str(&body(&get(
            "/connection-info",
            "x-vogt-public-url: https://attacker.example\r\n",
        )))
        .unwrap();
        assert!(
            ignored["url"].is_null(),
            "unfronted ignores the door headers"
        );
        assert!(get("/health", "").starts_with("HTTP/1.1 404"));
    }

    #[test]
    fn a_fronted_instance_honours_the_door_headers() {
        let _guard = ENV.lock().unwrap();
        let dir = std::env::temp_dir().join(format!("vogt-fronted-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("vogt.toml");
        std::fs::write(
            &file,
            "fronted = true\npublic_url = \"https://config.example\"\n",
        )
        .unwrap();
        // SAFETY: the test runs single-threaded and restores the variable.
        unsafe { std::env::set_var("VOGT_CONFIG_FILE", &file) };

        let state = HealthState {
            data_dir: PathBuf::from("/unused"),
            version: "0.7.7".to_string(),
            auth_enabled: true,
            writes_enabled: true,
        };
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.spawn(async move {
            let listener = tokio::net::TcpListener::from_std(listener).unwrap();
            axum::serve(listener, router(state)).await.unwrap();
        });
        std::thread::sleep(std::time::Duration::from_millis(100));
        let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        use std::io::{Read, Write};
        write!(
            stream,
            "GET /connection-info HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\
             x-vogt-public-url: https://door.example/\r\n\
             x-vogt-api-path: api/vogt/\r\n\
             x-vogt-mcp-path: /mcp/\r\n\r\n"
        )
        .unwrap();
        let mut buf = String::new();
        stream.read_to_string(&mut buf).unwrap();
        let body = buf.split("\r\n\r\n").nth(1).unwrap();
        let info: serde_json::Value = serde_json::from_str(body).unwrap();
        // The door wins field by field, and both cleaners strip the slash.
        assert_eq!(info["url"], "https://door.example");
        assert_eq!(info["api_path"], "/api/vogt");
        assert_eq!(info["mcp_path"], "/mcp");
        assert_eq!(info["authentication"], "bearer token");

        unsafe { std::env::remove_var("VOGT_CONFIG_FILE") };
        let _ = std::fs::remove_dir_all(&dir);
    }
}
