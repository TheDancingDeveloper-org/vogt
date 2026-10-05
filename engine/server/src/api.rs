use std::{convert::Infallible, path::Path as FsPath, sync::Arc, time::Duration};

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{sse::Event, Sse},
    Json,
};
use base64::Engine as _;
use futures_util::Stream;
use serde::{Deserialize, Serialize};
use tokio_stream::{wrappers::BroadcastStream, StreamExt};
use uuid::Uuid;
use vogt_engine_contract::{OkResponse, SessionDetail, SessionScreen, SessionSummary};

use crate::{app::AppState, error::Result, pty::SessionSpec};

pub async fn list_sessions(State(state): State<Arc<AppState>>) -> Json<Vec<SessionSummary>> {
    Json(state.sessions.list())
}

#[derive(Debug, Deserialize)]
pub struct WaitQuery {
    /// `ready` (default), `exited` or `any-change` (`any_change` accepted).
    pub until: Option<String>,
    /// Seconds to wait at most; default 120, at most 600.
    pub timeout_s: Option<u64>,
}

/// Block until the session is ready for input (or needs a person, or exits),
/// exits, or changes at all — or the timeout passes — and answer with why
/// and the screen at that moment. Gated like the screen: `sessions`.
pub async fn wait_session(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    Query(q): Query<WaitQuery>,
) -> Result<Json<vogt_engine_contract::SessionWait>> {
    use vogt_engine_contract::WaitUntil;
    let until = match q.until.as_deref().map(str::trim) {
        None | Some("") | Some("ready") => WaitUntil::Ready,
        Some("exited") => WaitUntil::Exited,
        Some("any-change") | Some("any_change") => WaitUntil::AnyChange,
        Some(other) => {
            return Err(crate::error::ApiError::BadRequest(format!(
                "until {other:?} is not one of ready, exited, any-change"
            )))
        }
    };
    let timeout = Duration::from_secs(q.timeout_s.unwrap_or(120));
    let session = state.sessions.get(id)?;
    Ok(Json(
        crate::wait::wait(&state.bus, session, until, timeout).await?,
    ))
}

/// Mark the session's agent blocked on a person (with what it needs), or
/// clear that. The core's `session.report_blocked` / `report_unblocked` call
/// this; the report rides on the session summary, the screen and a
/// `session-blocked` event, and blocking sends a push.
pub async fn set_session_blocked(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    Json(req): Json<vogt_engine_contract::SetBlocked>,
) -> Result<Json<SessionSummary>> {
    let session = state.sessions.get(id)?;
    let report = if req.blocked {
        let reason = req.reason.as_deref().map(str::trim).unwrap_or_default();
        if reason.is_empty() {
            return Err(crate::error::ApiError::BadRequest(
                "a blocked report needs a reason".into(),
            ));
        }
        if reason.len() > 2000 || req.items.len() > 20 || req.items.iter().any(|i| i.len() > 500) {
            return Err(crate::error::ApiError::BadRequest(
                "a blocked report is at most 2000 bytes of reason and 20 items of 500 bytes".into(),
            ));
        }
        Some(vogt_engine_contract::BlockedReport {
            reason: reason.to_string(),
            items: req
                .items
                .iter()
                .map(|i| i.trim().to_string())
                .filter(|i| !i.is_empty())
                .collect(),
            since: time::OffsetDateTime::now_utc()
                .format(&time::format_description::well_known::Rfc3339)
                .unwrap_or_default(),
        })
    } else {
        None
    };
    session.set_blocked(report, &state.bus)?;
    Ok(Json(session.summary()))
}

/// Stop the session's process tree and keep it listed as hibernated
/// (WI-912). `409` with the reason when it cannot be: no conversation to
/// resume (unless `allow_shell`), an agent-task run, or already exited.
pub async fn hibernate_session(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    body: Option<Json<vogt_engine_contract::HibernateRequest>>,
) -> Result<Json<SessionSummary>> {
    let req = body.map(|Json(r)| r).unwrap_or_default();
    Ok(Json(
        state
            .sessions
            .hibernate(
                id,
                req.reason,
                vogt_engine_contract::HibernateTrigger::Manual,
                req.allow_shell,
            )
            .await?,
    ))
}

/// Start a hibernated session again under the same id, resuming its agent
/// conversation; a live one is returned as it is.
pub async fn wake_session(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    body: Option<Json<vogt_engine_contract::WakeRequest>>,
) -> Result<Json<SessionSummary>> {
    let req = body.map(|Json(r)| r).unwrap_or_default();
    if let Some(env) = req.env.as_ref() {
        if env.len() > 64 || env.iter().any(|(k, v)| k.len() > 256 || v.len() > 8192) {
            return Err(crate::error::ApiError::BadRequest(
                "wake env is at most 64 variables, names of 256 bytes and values of 8 KiB".into(),
            ));
        }
    }
    let session = state.sessions.wake(id, req).await?;
    Ok(Json(session.summary()))
}

/// Pin a session awake (never hibernated by policy, woken at boot), or
/// unpin it.
pub async fn keep_session_awake(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    Json(req): Json<vogt_engine_contract::KeepAwakeRequest>,
) -> Result<Json<SessionSummary>> {
    Ok(Json(state.sessions.set_keep_awake(id, req.keep_awake)?))
}

/// Choose one option of the dialog on screen (WI-917): a permission
/// dialog or a startup gate. The engine reads the menu as it is now, moves
/// the highlight to the option with arrow keys, presses Enter, and looks
/// again to report whether the dialog went away. `409` when no dialog is
/// showing, when it no longer asks `expect_question`, or when the option is
/// not on its menu.
pub async fn answer_session(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    Json(req): Json<vogt_engine_contract::AnswerRequest>,
) -> Result<Json<vogt_engine_contract::AnswerResult>> {
    use crate::error::ApiError;
    let session = state.sessions.get(id)?;
    let reading = Arc::clone(&session);
    let dialog = tokio::task::spawn_blocking(move || reading.current_dialog())
        .await
        .map_err(|e| ApiError::Internal(format!("read the screen: {e}")))?
        .ok_or_else(|| ApiError::Conflict("no dialog is showing on this session".into()))?;
    if let Some(expected) = req.expect_question.as_deref() {
        if dialog.question.trim() != expected.trim() {
            return Err(ApiError::Conflict(format!(
                "the dialog on screen now asks {:?}, not {expected:?}; nothing was sent",
                dialog.question
            )));
        }
    }
    let menu: Vec<String> = dialog
        .options
        .iter()
        .map(|o| format!("{}. {}", o.number, o.label))
        .collect();
    let chosen = match (req.option, req.label.as_deref()) {
        (Some(n), _) => dialog.options.iter().find(|o| o.number == n),
        (None, Some(label)) => {
            let needle = label.trim().to_lowercase();
            let hits: Vec<_> = dialog
                .options
                .iter()
                .filter(|o| o.label.to_lowercase().contains(&needle))
                .collect();
            if hits.len() > 1 {
                return Err(ApiError::Conflict(format!(
                    "{label:?} matches more than one option: {}",
                    menu.join(" | ")
                )));
            }
            hits.first().copied()
        }
        (None, None) => {
            return Err(ApiError::BadRequest(
                "give option (a number) or label".into(),
            ));
        }
    }
    .cloned()
    .ok_or_else(|| {
        ApiError::Conflict(format!(
            "no such option on the dialog; it offers: {}",
            menu.join(" | ")
        ))
    })?;
    let keys = crate::approval::keys_to_choose(&dialog.options, chosen.number)
        .ok_or_else(|| ApiError::Conflict("the option is not on the menu".into()))?;
    // Arrows one write each, then Enter: a TUI reading a burst can merge an
    // escape sequence with what follows it.
    for key in keys.split_inclusive(['A', 'B', '\r']) {
        session
            .write_input(key.as_bytes())
            .map_err(|e| ApiError::Pty(format!("write input: {e}")))?;
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    let mut dismissed = false;
    for _ in 0..20 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let reading = Arc::clone(&session);
        let still = tokio::task::spawn_blocking(move || reading.current_dialog())
            .await
            .ok()
            .flatten();
        if still.is_none_or(|d| d.question != dialog.question) {
            dismissed = true;
            break;
        }
    }
    Ok(Json(vogt_engine_contract::AnswerResult {
        question: dialog.question,
        kind: dialog.kind.to_string(),
        chosen: vogt_engine_contract::ApprovalOption {
            number: chosen.number,
            label: chosen.label,
            selected: true,
        },
        dismissed,
    }))
}

pub async fn create_session(
    State(state): State<Arc<AppState>>,
    Json(spec): Json<SessionSpec>,
) -> Result<Json<SessionSummary>> {
    let s = state.sessions.create(spec)?;
    Ok(Json(s.summary()))
}

#[derive(Debug, Deserialize)]
pub struct SessionDetailQuery {
    /// Return only the last N bytes of scrollback instead of the whole ring
    /// The PWA's waiting-session cards render a 12-line tail, so they
    /// ask for ~16 KB rather than fetching and base64-decoding up to 4 MiB.
    /// Aligned forward to a ground-state boundary, so replay is still safe.
    pub tail_bytes: Option<usize>,
}

pub async fn get_session(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    Query(q): Query<SessionDetailQuery>,
) -> Result<Json<SessionDetail>> {
    if let Some((bytes, _, _, summary)) = state.sessions.hibernated_screen(id) {
        // What a hibernated session kept of its output stands in for the
        // scrollback; there is no live position past it.
        let bytes = match q.tail_bytes {
            Some(limit) if bytes.len() > limit => bytes.slice(bytes.len() - limit..),
            _ => bytes,
        };
        return Ok(Json(SessionDetail {
            summary,
            scrollback_pos: bytes.len() as u64,
            scrollback_base64: base64::engine::general_purpose::STANDARD.encode(&bytes),
        }));
    }
    let s = state.sessions.get(id)?;
    let (snap, pos) = match q.tail_bytes {
        Some(limit) => s.snapshot_tail(limit),
        None => s.snapshot(),
    };
    Ok(Json(SessionDetail {
        summary: s.summary(),
        scrollback_pos: pos,
        scrollback_base64: base64::engine::general_purpose::STANDARD.encode(&snap),
    }))
}

/// The session's current terminal screen, rendered: visible rows as text,
/// cursor, title, and whether the program is ready for input. Gated like
/// `GET /api/sessions/{id}` (the `sessions` capability), since it is a read
/// of live output.
///
/// `?scrollback_lines=N` (at most 2000) adds the N lines that scrolled off
/// the top of the screen, oldest first, as `scrollback`.
pub async fn get_session_screen(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    Query(q): Query<ScreenQuery>,
) -> Result<Json<SessionScreen>> {
    let lines = q.scrollback_lines.unwrap_or(0);
    if lines > crate::screen::MAX_SCROLLBACK_LINES {
        return Err(crate::error::ApiError::BadRequest(format!(
            "scrollback_lines is at most {}",
            crate::screen::MAX_SCROLLBACK_LINES
        )));
    }
    // A hibernated session shows the screen it had when it stopped, and
    // reading it does not wake it.
    if let Some((bytes, rows, cols, summary)) = state.sessions.hibernated_screen(id) {
        return Ok(Json(
            crate::screen::kept_screen(id, bytes, rows, cols, summary, lines).await?,
        ));
    }
    let s = state.sessions.get(id)?;
    Ok(Json(crate::screen::session_screen(s, lines).await?))
}

#[derive(Debug, Deserialize)]
pub struct SweepQuery {
    /// How many non-blank screen lines each row carries (default 8, at
    /// most 40; 0 for none).
    pub screen_lines: Option<usize>,
    /// Include sessions whose process has exited (default false).
    #[serde(default)]
    pub include_exited: bool,
}

/// Every session at once, each with the tail of its screen: the oversight
/// table a driver otherwise builds one `/screen` call at a time (WI-915).
/// Live and hibernated sessions; exited ones only when asked. Screens are
/// rendered concurrently on the blocking pool. Gated like `/screen`.
pub async fn sweep_sessions(
    State(state): State<Arc<AppState>>,
    Query(q): Query<SweepQuery>,
) -> Result<Json<Vec<vogt_engine_contract::SessionSweepEntry>>> {
    let lines = q.screen_lines.unwrap_or(8);
    if lines > 40 {
        return Err(crate::error::ApiError::BadRequest(
            "screen_lines is at most 40".into(),
        ));
    }
    let wanted: Vec<SessionSummary> = state
        .sessions
        .list()
        .into_iter()
        .filter(|s| {
            s.alive
                || q.include_exited
                || s.activity == vogt_engine_contract::ActivityState::Hibernated
        })
        .collect();
    let rows = futures_util::future::join_all(wanted.into_iter().map(|summary| {
        let state = Arc::clone(&state);
        async move {
            let screen = if lines == 0 {
                None
            } else if let Some((bytes, rows, cols, kept)) =
                state.sessions.hibernated_screen(summary.id)
            {
                crate::screen::kept_screen(summary.id, bytes, rows, cols, kept, 0)
                    .await
                    .ok()
            } else {
                match state.sessions.get(summary.id) {
                    Ok(session) => crate::screen::session_screen(session, 0).await.ok(),
                    Err(_) => None,
                }
            };
            let (screen_tail, ready) = match screen {
                Some(screen) => {
                    let mut tail: Vec<String> = screen
                        .lines
                        .into_iter()
                        .rev()
                        .filter(|l| !l.trim().is_empty())
                        .take(lines)
                        .collect();
                    tail.reverse();
                    (tail, screen.ready)
                }
                None => (Vec::new(), false),
            };
            vogt_engine_contract::SessionSweepEntry {
                summary,
                screen_tail,
                ready,
            }
        }
    }))
    .await;
    Ok(Json(rows))
}

#[derive(Debug, Deserialize)]
pub struct ScreenQuery {
    pub scrollback_lines: Option<usize>,
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

/// SIGKILL the child, recording the stop first: an optional
/// `{"reason", "by"}` body says why and who, and the exit then reads
/// `stopped` instead of `errored` (WI-913).
pub async fn kill_session(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    body: Option<Json<vogt_engine_contract::StopRequest>>,
) -> Result<Json<OkResponse>> {
    let request = body.map(|Json(r)| r).unwrap_or_default();
    state.sessions.stop(id, request)?;
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
    let bus = state.bus.clone();
    let stream = BroadcastStream::new(rx).filter_map(move |res| match res {
        Ok(ev) => match serde_json::to_string(&ev) {
            Ok(json) => Some(Ok(Event::default().data(json))),
            Err(_) => None,
        },
        // A slow client fell behind and missed events. The stream goes on,
        // but the client is told, in band, so it can re-read what it shows
        // rather than trust state built from a stream with a hole in it
        // (WI-920).
        Err(tokio_stream::wrappers::errors::BroadcastStreamRecvError::Lagged(skipped)) => {
            bus.note_lag("sse-events", skipped);
            Some(Ok(Event::default().data(
                serde_json::json!({ "type": "lagged", "skipped": skipped }).to_string(),
            )))
        }
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
pub struct AuthCheckResponse {
    pub ok: bool,
    pub version: &'static str,
    pub product_version: &'static str,
    pub storage: ServerStorageStatus,
    /// Who the gate decided the caller is, and what they may do here.
    pub identity: AuthCheckIdentity,
}

#[derive(Debug, Serialize)]
pub struct AuthCheckIdentity {
    /// `primary`, `vogt-core`, or the core actor's `identity_ref`.
    pub name: String,
    /// The core scopes behind the grant; empty for a static credential.
    pub scopes: Vec<String>,
    pub capabilities: Vec<crate::auth::TokenCapability>,
}

/// Cheap authenticated identity check. Keep this handler limited to values
/// already held in memory: `/api/status` owns the operational scans.
pub async fn auth_check(
    State(state): State<Arc<AppState>>,
    identity: Option<axum::Extension<crate::auth::AuthorizedIdentity>>,
) -> Json<AuthCheckResponse> {
    let identity = identity.map(|axum::Extension(id)| id);
    Json(AuthCheckResponse {
        ok: true,
        version: crate::product::VERSION,
        product_version: crate::product::VERSION,
        storage: ServerStorageStatus {
            state_dir: state.config.state_dir.display().to_string(),
            workspace_root: state.config.workspace_root.display().to_string(),
        },
        identity: AuthCheckIdentity {
            name: identity
                .as_ref()
                .map(|id| id.name.clone())
                .unwrap_or_else(|| "unidentified".to_string()),
            scopes: identity
                .as_ref()
                .map(|id| id.scopes.clone())
                .unwrap_or_default(),
            capabilities: identity.map(|id| id.capabilities).unwrap_or_default(),
        },
    })
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
    /// what an absent core must not cost. So its outage is
    /// reported here in full and left out of the verdict; the surfaces that
    /// need the core say so themselves.
    pub fatal: bool,
}

pub async fn readyz(State(state): State<Arc<AppState>>) -> (StatusCode, Json<ReadinessResponse>) {
    let mut checks = Vec::with_capacity(5);
    checks.push(check_workspace_root(&state.config.workspace_root).await);
    checks.push(check_state_dir(&state.config.state_dir).await);
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
    pub product_version: &'static str,
    pub source_ref: &'static str,
    pub source_sha: &'static str,
    pub release_url: Option<String>,
    pub session_count: usize,
    pub push_subscription_count: usize,
    pub gui_process_count: usize,
    pub gui_stream_configured: bool,
    pub fcm_enabled: bool,
    pub history: HistoryStatus,
    pub agent_tasks: AgentTaskStorageStatus,
    pub auth_broker: AuthBrokerStatus,
    pub storage: ServerStorageStatus,
    /// Event subscribers that have fallen behind since start, by name, with
    /// how often and how many events they missed (WI-920). Empty is healthy.
    pub event_lag: std::collections::BTreeMap<&'static str, crate::events::LagCount>,
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
        version: crate::product::VERSION,
        product_version: crate::product::VERSION,
        source_ref: crate::product::SOURCE_REF,
        source_sha: crate::product::SOURCE_SHA,
        release_url: crate::product::release_url(),
        session_count: state.sessions.list().len(),
        event_lag: state.bus.lags(),
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

/// vogt-core's own readiness, asked of vogt-core.
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

/// Do the two halves agree about where the estate is?
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
/// take the terminals down with it.
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
                 sessions and to the collectors that run here"
            ),
            fatal: false,
        }
    }
}

/// Does the directory vogt-core would back up as "the engine's state" contain
/// the engine's state?
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
                 subscriptions or VAPID keypair",
                theirs.to_string_lossy(),
                ours.to_string_lossy()
            ),
            // Non-fatal, for the reason an absent core is: a wrong backup path is a bad
            // answer to a question nobody is asking yet, and killing the pod
            // over it would take the terminals with it.
            fatal: false,
        }
    }
}
