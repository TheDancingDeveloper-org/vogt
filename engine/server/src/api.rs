use std::{convert::Infallible, path::Path as FsPath, sync::Arc, time::Duration};

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{sse::Event, Sse},
    Json,
};
use base64::Engine as _;
use futures_util::Stream;
use serde::{Deserialize, Serialize};
use tokio_stream::{wrappers::BroadcastStream, StreamExt};
use uuid::Uuid;
use vogt_engine_contract::{OkResponse, SessionDetail, SessionSummary};

use crate::{app::AppState, error::Result, pty::SessionSpec};

pub async fn list_sessions(State(state): State<Arc<AppState>>) -> Json<Vec<SessionSummary>> {
    Json(state.sessions.list())
}

pub async fn create_session(
    State(state): State<Arc<AppState>>,
    Json(spec): Json<SessionSpec>,
) -> Result<Json<SessionSummary>> {
    let s = state.sessions.create(spec)?;
    Ok(Json(s.summary()))
}

pub async fn get_session(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<SessionDetail>> {
    let s = state.sessions.get(id)?;
    let (snap, pos) = s.snapshot();
    Ok(Json(SessionDetail {
        summary: s.summary(),
        scrollback_pos: pos,
        scrollback_base64: base64::engine::general_purpose::STANDARD.encode(&snap),
    }))
}

#[derive(Debug, Deserialize)]
pub struct RenameReq {
    pub name: String,
}

pub async fn rename_session(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    Json(req): Json<RenameReq>,
) -> Result<Json<OkResponse>> {
    state.sessions.rename(id, req.name)?;
    Ok(Json(OkResponse::new(true)))
}

pub async fn delete_session(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<OkResponse>> {
    state.sessions.remove(id)?;
    Ok(Json(OkResponse::new(true)))
}

pub async fn kill_session(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<OkResponse>> {
    state.sessions.kill(id)?;
    Ok(Json(OkResponse::new(true)))
}

/// Mirrors the WebSocket input cap (`ws::MAX_INPUT_BYTES`).
const MAX_HTTP_INPUT_BYTES: usize = 64 * 1024;

#[derive(Debug, Deserialize)]
pub struct SessionInputReq {
    /// Text written verbatim to the PTY. Control sequences are allowed —
    /// this is the same raw path as WebSocket binary frames.
    pub text: String,
    /// Append a carriage return after `text` (i.e. "press Enter").
    #[serde(default)]
    pub submit: bool,
}

pub async fn session_input(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    Json(req): Json<SessionInputReq>,
) -> Result<Json<OkResponse>> {
    if req.text.len() > MAX_HTTP_INPUT_BYTES {
        return Err(crate::error::ApiError::BadRequest(format!(
            "input exceeds {MAX_HTTP_INPUT_BYTES} bytes"
        )));
    }
    let session = state.sessions.get(id)?;
    let mut bytes = req.text.into_bytes();
    if req.submit {
        bytes.push(b'\r');
    }
    session
        .write_input(&bytes)
        .map_err(|e| crate::error::ApiError::Pty(format!("write input: {e}")))?;
    Ok(Json(OkResponse::new(true)))
}

pub async fn events_stream(
    State(state): State<Arc<AppState>>,
) -> Sse<impl Stream<Item = std::result::Result<Event, Infallible>>> {
    let rx = state.bus.subscribe();
    let stream = BroadcastStream::new(rx).filter_map(|res| match res {
        Ok(ev) => match serde_json::to_string(&ev) {
            Ok(json) => Some(Ok(Event::default().data(json))),
            Err(_) => None,
        },
        Err(_) => None, // lagging receiver — skip
    });
    Sse::new(stream).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("ka"),
    )
}

pub async fn healthz() -> Json<OkResponse> {
    Json(OkResponse::new(true))
}

#[derive(Debug, Serialize)]
pub struct ReadinessResponse {
    pub ok: bool,
    pub checks: Vec<ReadinessCheck>,
}

#[derive(Debug, Serialize)]
pub struct ReadinessCheck {
    pub name: &'static str,
    pub ok: bool,
    pub detail: String,
    /// Whether this check failing means *this container* is not ready.
    ///
    /// Every check the engine owns is fatal, because the engine is what a
    /// restart would fix. The vogt-core probe is not: the core is a separate
    /// process with its own lifecycle, restarting the engine would not
    /// revive it, and doing so would kill every live PTY — which is exactly
    /// what FR-E9 says an absent core must not cost. So its outage is
    /// reported here in full and left out of the verdict; the surfaces that
    /// need the core say so themselves (FR-U21).
    pub fatal: bool,
}

pub async fn readyz(State(state): State<Arc<AppState>>) -> (StatusCode, Json<ReadinessResponse>) {
    let mut checks = Vec::with_capacity(6);
    checks.push(check_workspace_root(&state.config.workspace_root).await);
    checks.push(check_state_dir(&state.config.state_dir).await);
    checks.push(check_tailscale().await);
    checks.push(check_gui(state.config.gui_stream_url.is_some()).await);
    checks.push(check_vogt_core(&state).await);
    checks.push(
        check_workspace_agreement(
            &state.config.workspace_root,
            state.config.vogt_import_root.as_deref(),
        )
        .await,
    );
    checks.push(
        check_backup_agreement(
            &state.config.state_dir,
            state.config.vogt_engine_state_dir.as_deref(),
        )
        .await,
    );

    let ok = checks.iter().all(|check| check.ok || !check.fatal);
    let status = if ok {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (status, Json(ReadinessResponse { ok, checks }))
}

#[derive(Debug, Serialize)]
pub struct OperationalStatus {
    pub version: &'static str,
    pub session_count: usize,
    pub push_subscription_count: usize,
    pub gui_process_count: usize,
    pub gui_stream_configured: bool,
    pub fcm_enabled: bool,
    pub history: HistoryStatus,
    pub agent_tasks: AgentTaskStorageStatus,
    pub auth_broker: AuthBrokerStatus,
    pub storage: ServerStorageStatus,
}

#[derive(Debug, Serialize)]
pub struct HistoryStatus {
    pub enabled: bool,
    pub archived_session_count: Option<u64>,
    pub log_file_count: Option<u64>,
    pub log_bytes: Option<u64>,
    pub db_bytes: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct AgentTaskStorageStatus {
    pub task_count: usize,
    pub prompt_task_dir_count: u64,
    pub prompt_file_count: u64,
    pub context_file_count: u64,
    pub session_prompt_file_count: u64,
    pub prompt_bytes: u64,
    pub orphan_task_dir_count: u64,
}

#[derive(Debug, Serialize)]
pub struct AuthBrokerStatus {
    pub auto_agent_auth: bool,
    pub helper: String,
}

#[derive(Debug, Serialize)]
pub struct ServerStorageStatus {
    pub state_dir: String,
    pub workspace_root: String,
}

pub async fn operational_status(
    State(state): State<Arc<AppState>>,
) -> Result<Json<OperationalStatus>> {
    let history_stats = match state.history.as_ref() {
        Some(history) => Some(history.storage_stats().await?),
        None => None,
    };
    let task_artifacts = state.agent_tasks.prompt_artifact_stats()?;

    Ok(Json(OperationalStatus {
        version: env!("CARGO_PKG_VERSION"),
        session_count: state.sessions.list().len(),
        push_subscription_count: state.push.list().len(),
        gui_process_count: state.gui.count_alive(),
        gui_stream_configured: state.config.gui_stream_url.is_some(),
        fcm_enabled: state.config.fcm_service_account_json.is_some(),
        history: HistoryStatus {
            enabled: history_stats.is_some(),
            archived_session_count: history_stats
                .as_ref()
                .map(|stats| stats.archived_session_count),
            log_file_count: history_stats.as_ref().map(|stats| stats.log_file_count),
            log_bytes: history_stats.as_ref().map(|stats| stats.log_bytes),
            db_bytes: history_stats.as_ref().map(|stats| stats.db_bytes),
        },
        agent_tasks: AgentTaskStorageStatus {
            task_count: state.agent_tasks.list().len(),
            prompt_task_dir_count: task_artifacts.task_dir_count,
            prompt_file_count: task_artifacts.prompt_file_count,
            context_file_count: task_artifacts.context_file_count,
            session_prompt_file_count: task_artifacts.session_prompt_file_count,
            prompt_bytes: task_artifacts.total_bytes,
            orphan_task_dir_count: task_artifacts.orphan_task_dir_count,
        },
        auth_broker: AuthBrokerStatus {
            auto_agent_auth: state.config.auto_agent_auth,
            helper: state
                .config
                .agent_auth_helper
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| state.config.agent_auth_helper.display().to_string()),
        },
        storage: ServerStorageStatus {
            state_dir: state.config.state_dir.display().to_string(),
            workspace_root: state.config.workspace_root.display().to_string(),
        },
    }))
}

async fn check_workspace_root(path: &FsPath) -> ReadinessCheck {
    match tokio::fs::metadata(path).await {
        Ok(meta) if meta.is_dir() => match tokio::fs::read_dir(path).await {
            Ok(_) => ReadinessCheck {
                fatal: true,
                name: "workspace_root",
                ok: true,
                detail: format!("readable directory at {}", path.display()),
            },
            Err(err) => ReadinessCheck {
                fatal: true,
                name: "workspace_root",
                ok: false,
                detail: format!("cannot read {}: {err}", path.display()),
            },
        },
        Ok(_) => ReadinessCheck {
            fatal: true,
            name: "workspace_root",
            ok: false,
            detail: format!("{} is not a directory", path.display()),
        },
        Err(err) => ReadinessCheck {
            fatal: true,
            name: "workspace_root",
            ok: false,
            detail: format!("cannot stat {}: {err}", path.display()),
        },
    }
}

async fn check_state_dir(path: &FsPath) -> ReadinessCheck {
    match tokio::fs::metadata(path).await {
        Ok(meta) if meta.is_dir() => {
            let probe = path.join(".readyz-writecheck");
            match tokio::fs::write(&probe, b"ok").await {
                Ok(()) => {
                    let _ = tokio::fs::remove_file(&probe).await;
                    ReadinessCheck {
                        fatal: true,
                        name: "state_dir",
                        ok: true,
                        detail: format!("writable directory at {}", path.display()),
                    }
                }
                Err(err) => ReadinessCheck {
                    fatal: true,
                    name: "state_dir",
                    ok: false,
                    detail: format!("cannot write {}: {err}", probe.display()),
                },
            }
        }
        Ok(_) => ReadinessCheck {
            fatal: true,
            name: "state_dir",
            ok: false,
            detail: format!("{} is not a directory", path.display()),
        },
        Err(err) => ReadinessCheck {
            fatal: true,
            name: "state_dir",
            ok: false,
            detail: format!("cannot stat {}: {err}", path.display()),
        },
    }
}

async fn check_tailscale() -> ReadinessCheck {
    if std::env::var("TAILSCALE_AUTH_KEY")
        .ok()
        .map(|value| value.trim().is_empty())
        .unwrap_or(true)
    {
        return ReadinessCheck {
            fatal: true,
            name: "tailscale",
            ok: true,
            detail: "not configured".into(),
        };
    }

    if !FsPath::new("/var/run/tailscale/tailscaled.sock").exists() {
        return ReadinessCheck {
            fatal: true,
            name: "tailscale",
            ok: false,
            detail: "tailscaled socket missing".into(),
        };
    }

    let output = match tokio::time::timeout(
        Duration::from_secs(2),
        tokio::process::Command::new("tailscale")
            .args(["status", "--json"])
            .output(),
    )
    .await
    {
        Ok(Ok(output)) => output,
        Ok(Err(err)) => {
            return ReadinessCheck {
                fatal: true,
                name: "tailscale",
                ok: false,
                detail: format!("tailscale status failed: {err}"),
            };
        }
        Err(_) => {
            return ReadinessCheck {
                fatal: true,
                name: "tailscale",
                ok: false,
                detail: "tailscale status timed out".into(),
            };
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return ReadinessCheck {
            fatal: true,
            name: "tailscale",
            ok: false,
            detail: format!(
                "tailscale status exited {}: {}",
                output.status,
                stderr.trim()
            ),
        };
    }

    let status: serde_json::Value = match serde_json::from_slice(&output.stdout) {
        Ok(status) => status,
        Err(err) => {
            return ReadinessCheck {
                fatal: true,
                name: "tailscale",
                ok: false,
                detail: format!("invalid tailscale status JSON: {err}"),
            };
        }
    };

    let backend_state = status
        .get("BackendState")
        .and_then(|value| value.as_str())
        .unwrap_or("unknown");
    let online = status
        .get("Self")
        .and_then(|value| value.get("Online"))
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    if backend_state == "Running" && online {
        ReadinessCheck {
            fatal: true,
            name: "tailscale",
            ok: true,
            detail: "running and online".into(),
        }
    } else {
        ReadinessCheck {
            fatal: true,
            name: "tailscale",
            ok: false,
            detail: format!("backend_state={backend_state}, online={online}"),
        }
    }
}

async fn check_gui(gui_stream_configured: bool) -> ReadinessCheck {
    let sway_enabled = std::env::var("START_SWAY")
        .ok()
        .map(|value| matches!(value.trim(), "1" | "true" | "TRUE" | "yes" | "on"))
        .unwrap_or(false);
    if !sway_enabled {
        return ReadinessCheck {
            fatal: true,
            name: "gui",
            ok: true,
            detail: if gui_stream_configured {
                "stream configured without local sway".into()
            } else {
                "disabled".into()
            },
        };
    }

    let output = match tokio::time::timeout(
        Duration::from_secs(2),
        tokio::process::Command::new("swaymsg")
            .args(["-t", "get_version"])
            .output(),
    )
    .await
    {
        Ok(Ok(output)) => output,
        Ok(Err(err)) => {
            return ReadinessCheck {
                fatal: true,
                name: "gui",
                ok: false,
                detail: format!("swaymsg failed: {err}"),
            };
        }
        Err(_) => {
            return ReadinessCheck {
                fatal: true,
                name: "gui",
                ok: false,
                detail: "swaymsg timed out".into(),
            };
        }
    };

    if output.status.success() {
        ReadinessCheck {
            fatal: true,
            name: "gui",
            ok: true,
            detail: "sway responsive".into(),
        }
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        ReadinessCheck {
            fatal: true,
            name: "gui",
            ok: false,
            detail: format!("sway unavailable: {}", stderr.trim()),
        }
    }
}

/// vogt-core's own readiness, asked of vogt-core (NFR-D11).
///
/// Reported and never fatal: see `ReadinessCheck::fatal` for why an outage in
/// the other half of the product must not take this container down with it.
async fn check_vogt_core(state: &Arc<AppState>) -> ReadinessCheck {
    let Some(core) = state.vogt_core.as_ref() else {
        return ReadinessCheck {
            name: "vogt_core",
            ok: true,
            detail: "not configured".into(),
            fatal: false,
        };
    };
    let (ok, detail) = core.probe().await;
    ReadinessCheck {
        name: "vogt_core",
        ok,
        detail,
        fatal: false,
    }
}

/// Do the two halves agree about where the estate is? (FR-E3, §6.3)
///
/// Vogt's import root and this server's `workspace_root` must be the same
/// tree: a session opened "for" a project opens in the path the project
/// registry recorded, and a project imported outside this root is a project
/// no session can be opened in and no collector here can see.
///
/// The entrypoint already says this at boot, which is the moment nobody is
/// reading it three weeks later. Reported here too, and deliberately not
/// fatal: a disagreement makes some projects invisible, which is a bad
/// answer rather than a dead server, and failing readiness over it would
/// take the terminals down with it (FR-E9).
async fn check_workspace_agreement(root: &FsPath, import_root: Option<&FsPath>) -> ReadinessCheck {
    let Some(import_root) = import_root else {
        // Absent is the ordinary case: the core is configured elsewhere, or
        // it is using its own default under the data directory. Nothing to
        // compare, and nothing to claim.
        return ReadinessCheck {
            name: "workspace_agreement",
            ok: true,
            detail: "VOGT_IMPORT_ROOT is not set here; nothing to compare".into(),
            fatal: false,
        };
    };
    // Canonically, because `/home/x/Working` and a symlink to it are the same
    // tree and a textual comparison would call them different — and by *path
    // component*, because a textual prefix says `/srv/work` contains
    // `/srv/workspace`, which is two unrelated trees agreeing by accident.
    let canonical = |path: &FsPath| path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let ours = canonical(root);
    let theirs = canonical(import_root);
    let inside = theirs.starts_with(&ours);
    let ours = ours.to_string_lossy().into_owned();
    let theirs = theirs.to_string_lossy().into_owned();
    if inside {
        ReadinessCheck {
            name: "workspace_agreement",
            ok: true,
            detail: format!("vogt imports into {theirs}, inside {ours}"),
            fatal: false,
        }
    } else {
        ReadinessCheck {
            name: "workspace_agreement",
            ok: false,
            detail: format!(
                "vogt imports into {theirs}, which is outside this server's \
                 workspace root {ours}: imported projects will be invisible to \
                 sessions and to the collectors that run here (FR-E3)"
            ),
            fatal: false,
        }
    }
}

/// Does the directory vogt-core would back up as "the engine's state" contain
/// the engine's state? (NFR-I6, §6.3 finding 14.)
///
/// The failure is quiet and it is the worst kind this pair can produce. A
/// backup that misses a directory does not fail — `vogt backup` treats an
/// absent engine state as non-fatal by design, so it writes a manifest, says
/// something true about what it copied, and produces an archive that restores
/// a running product minus its session history, push subscriptions, VAPID
/// keypair and agent-task prompts. Nobody finds out until a restore, which is
/// the one moment nobody wants to be reading a manifest closely.
///
/// Two configurations name this path — the engine's `state_dir` and the
/// core's `engine_state_dir` — because the two processes are configured
/// separately even when they share a container. This is the check that says
/// they still mean the same directory.
async fn check_backup_agreement(
    state_dir: &FsPath,
    engine_state_dir: Option<&FsPath>,
) -> ReadinessCheck {
    let Some(engine_state_dir) = engine_state_dir else {
        // Not set is the ordinary case away from the merged stack: an engine
        // with no core beside it has nothing to agree with, and the core's own
        // backup says `engine_state: "not configured"` rather than implying
        // coverage. Claiming a disagreement here would make every single-half
        // deployment look misconfigured.
        return ReadinessCheck {
            name: "backup_agreement",
            ok: true,
            detail: "VOGT_ENGINE_STATE_DIR is not set here; vogt backup, if one \
                     runs, will say it covered no engine state"
                .into(),
            fatal: false,
        };
    };
    let canonical = |path: &FsPath| path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let ours = canonical(state_dir);
    let theirs = canonical(engine_state_dir);
    if ours == theirs {
        ReadinessCheck {
            name: "backup_agreement",
            ok: true,
            detail: format!("vogt backup covers {}", theirs.to_string_lossy()),
            fatal: false,
        }
    } else {
        ReadinessCheck {
            name: "backup_agreement",
            ok: false,
            detail: format!(
                "vogt would back up {}, which is not this server's state_dir \
                 {}: backups will succeed and contain no session history, push \
                 subscriptions or VAPID keypair (NFR-I6)",
                theirs.to_string_lossy(),
                ours.to_string_lossy()
            ),
            // Non-fatal for FR-E9's reason: a wrong backup path is a bad
            // answer to a question nobody is asking yet, and killing the pod
            // over it would take the terminals with it.
            fatal: false,
        }
    }
}
