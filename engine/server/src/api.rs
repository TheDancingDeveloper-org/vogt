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
use vogt_engine_contract::{OkResponse, SessionDetail, SessionList, SessionScreen, SessionSummary};

use crate::{app::AppState, error::Result, pty::SessionSpec};

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    /// `hidden` returns the sessions the default list leaves out as well as
    /// the ones it shows (WI-1289). Anything else, including absent, is the
    /// visible set. `hidden_count` is reported either way.
    pub include: Option<String>,
}

pub async fn list_sessions(
    State(state): State<Arc<AppState>>,
    Query(q): Query<ListQuery>,
) -> Json<SessionList> {
    let include_hidden = q.include.as_deref() == Some("hidden");
    Json(state.sessions.list(include_hidden))
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

#[derive(Debug, Deserialize)]
pub struct ChangesQuery {
    /// Replay events published after this sequence. `0` reads the whole ring.
    pub since: Option<u64>,
    /// Engine session ids, comma separated. Absent means every session.
    pub ids: Option<String>,
    /// At most this many changes; default 100, at most 500.
    pub limit: Option<usize>,
}

/// Changes published after a cursor, so an overseer reads a delta instead of
/// sweeping (WI-1246).
pub async fn session_changes(
    State(state): State<Arc<AppState>>,
    Query(q): Query<ChangesQuery>,
) -> Result<Json<vogt_engine_contract::SessionChanges>> {
    Ok(Json(crate::changes::changes(
        &state.bus,
        q.since.unwrap_or(0),
        q.ids.as_deref(),
        q.limit,
    )?))
}

#[derive(Debug, Deserialize)]
pub struct WaitAnyQuery {
    /// Engine session ids, comma separated. Absent means every session.
    pub ids: Option<String>,
    /// Kinds to wait for, comma separated. Defaults to the overseer set.
    pub until: Option<String>,
    /// A sequence to ignore up to, or `now` to skip everything already
    /// published. Absent replays from the start of the ring.
    pub cursor: Option<String>,
    /// Seconds to wait at most; default 120, at most 600.
    pub timeout_s: Option<u64>,
}

/// `now` means the bus head at the start of the wait. Anything else is a seq.
fn parse_wait_cursor(raw: Option<&str>, head: u64) -> crate::error::Result<u64> {
    let Some(raw) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(0);
    };
    if raw.eq_ignore_ascii_case("now") {
        return Ok(head);
    }
    raw.parse::<u64>().map_err(|_| {
        crate::error::ApiError::BadRequest(
            "cursor is a sequence number, or 'now' to skip events already published".into(),
        )
    })
}

/// Block until one of the named sessions changes in a way `until` names, or
/// the timeout passes (WI-1246).
pub async fn session_wait_any(
    State(state): State<Arc<AppState>>,
    Query(q): Query<WaitAnyQuery>,
) -> Result<Json<vogt_engine_contract::SessionWaitAny>> {
    let until = crate::changes::parse_until(q.until.as_deref())?;
    let timeout = Duration::from_secs(q.timeout_s.unwrap_or(120));
    Ok(Json(
        crate::changes::wait_any(
            &state.bus,
            q.ids.as_deref(),
            &until,
            parse_wait_cursor(q.cursor.as_deref(), state.bus.head())?,
            timeout,
        )
        .await?,
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
            needs: req.needs,
            boundary: req.boundary,
            key: String::new(),
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
                req.superseded_by,
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
    if req
        .fresh_prompt
        .as_ref()
        .is_some_and(|p| p.len() > crate::sessions::FRESH_PROMPT_MAX)
    {
        return Err(crate::error::ApiError::BadRequest(
            "a wake's fresh_prompt is at most 64 KiB".into(),
        ));
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

/// Whether this caller may give a session a role other than `worker`, or
/// change its role at all (WI-957, WI-1255).
///
/// The role is a person's nomination: vogt-core's `session.set_role` and
/// `session.start` refuse every agent principal, and an oversight role is
/// what lets a session commit for others (C14) and ask for grants on their
/// behalf. So here too only a person, the break-glass operator, or vogt-core
/// (which has made that check for its own caller) may assign it — otherwise
/// any session token, holding `sessions`, could promote itself, or start an
/// overseer and drive its terminal, and walk past both checks. Decided by the
/// gate's flags, never by `name`. A refusal is a 403 with an audit line.
fn require_role_authority(
    identity: Option<&crate::auth::AuthorizedIdentity>,
    session: &str,
    role: vogt_engine_contract::SessionRole,
) -> crate::error::Result<()> {
    match identity {
        Some(identity) if identity.person || identity.break_glass || identity.stack_secret => {
            Ok(())
        }
        Some(identity) => {
            tracing::warn!(
                target: "vogt::audit",
                event = "session.role",
                outcome = "refused",
                caller = %identity.name,
                session = session,
                role = role.as_str(),
                "session role refused"
            );
            Err(crate::error::ApiError::Forbidden(format!(
                "only a person nominates an oversight session, or changes a session's \
                 role ({} is an agent); ask a person to do it from the GUI",
                identity.name
            )))
        }
        None => Err(crate::error::ApiError::Unauthorized),
    }
}

/// Nominate a session as oversight, or make it a worker again (WI-957).
/// Becoming oversight pins it awake too. Only a person, break-glass or
/// vogt-core (`require_role_authority`).
pub async fn set_session_role(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    identity: Option<axum::Extension<crate::auth::AuthorizedIdentity>>,
    Json(req): Json<vogt_engine_contract::SessionRoleRequest>,
) -> Result<Json<SessionSummary>> {
    require_role_authority(identity.as_deref(), &id.to_string(), req.role)?;
    Ok(Json(state.sessions.set_role(id, req.role)?))
}

/// Label a session with the work item it serves, or clear the label (WI-998).
/// Gated like every session write; vogt-core's `session.bind_work` is the
/// audited way to reach it.
pub async fn set_session_work_item(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    Json(req): Json<vogt_engine_contract::SessionWorkItemRequest>,
) -> Result<Json<SessionSummary>> {
    Ok(Json(
        state.sessions.set_work_item(id, req.work_item.as_deref())?,
    ))
}

/// `POST /api/sessions/{id}/conversation` — link (or, with `ended`, unlink)
/// the agent conversation running in a session (WI-962). Gated like every
/// session write: `sessions`. The agent's own hook normally reports through
/// [`report_own_conversation`] instead, which needs no engine credential.
pub async fn report_session_conversation(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    Json(req): Json<vogt_engine_contract::ConversationReport>,
) -> Result<Json<SessionSummary>> {
    Ok(Json(state.sessions.report_conversation(id, req)?))
}

/// The broker route for a conversation report: `/api/agent-auth/conversation`.
pub const OWN_CONVERSATION_ROUTE: &str = "/api/agent-auth/conversation";

/// `POST /api/agent-auth/conversation` — the same report as
/// [`report_session_conversation`], from inside the session, authenticated
/// by the session's own broker token (WI-962). The token names the session,
/// so a session can only ever link a conversation to itself. Outside the
/// bearer gate for the reason the broker is: a session holds no engine
/// bearer.
pub async fn report_own_conversation(
    State(state): State<Arc<AppState>>,
    request: axum::extract::Request,
) -> axum::response::Response {
    use axum::{http::header, response::IntoResponse};
    let path = request.uri().path().to_string();
    let request_id = request
        .extensions()
        .get::<crate::observability::RequestId>()
        .map(|id| id.0.clone())
        .unwrap_or_default();
    let session = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .and_then(|token| state.sessions.secret_broker().authenticate(token));
    let Some(id) = session else {
        crate::auth::record_auth_failure(
            &axum::http::Method::POST,
            &path,
            &request_id,
            "missing or unknown broker token",
        )
        .await;
        return crate::error::ApiError::Unauthorized.into_response();
    };
    let body = match axum::body::to_bytes(request.into_body(), 4096).await {
        Ok(body) => body,
        Err(_) => {
            return crate::error::ApiError::BadRequest("conversation report too large".into())
                .into_response()
        }
    };
    let report: vogt_engine_contract::ConversationReport = match serde_json::from_slice(&body) {
        Ok(report) => report,
        Err(e) => {
            return crate::error::ApiError::BadRequest(format!("conversation report: {e}"))
                .into_response()
        }
    };
    match state.sessions.report_conversation(id, report) {
        Ok(summary) => Json(summary).into_response(),
        Err(e) => e.into_response(),
    }
}

/// The broker route for a policy check: `/api/agent-auth/policy-check`.
pub const POLICY_CHECK_ROUTE: &str = "/api/agent-auth/policy-check";

/// What the hook posts: the PreToolUse payload, of which only the tool name,
/// the command and the working directory are read. Unknown fields are
/// ignored so a CLI that adds one does not start failing closed.
#[derive(serde::Deserialize)]
struct PolicyCheckRequest {
    #[serde(default)]
    tool_name: String,
    #[serde(default)]
    tool_input: PolicyCheckInput,
    #[serde(default)]
    cwd: String,
}

#[derive(serde::Deserialize, Default)]
struct PolicyCheckInput {
    #[serde(default)]
    command: String,
}

/// `POST /api/agent-auth/policy-check` — whether the shell command a session's
/// agent is about to run breaks the project's policy (WI-1243). Authenticated
/// like [`report_own_conversation`]: the session's own broker token, which
/// names the session, so a session is only ever checked against its own
/// policy. A session with no policy, or a tool that is not a shell, is
/// allowed; the hook only asks about `Bash`.
pub async fn policy_check(
    State(state): State<Arc<AppState>>,
    request: axum::extract::Request,
) -> axum::response::Response {
    use axum::{http::header, response::IntoResponse};
    let path = request.uri().path().to_string();
    let request_id = request
        .extensions()
        .get::<crate::observability::RequestId>()
        .map(|id| id.0.clone())
        .unwrap_or_default();
    let session = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .and_then(|token| state.sessions.secret_broker().authenticate(token));
    let Some(id) = session else {
        crate::auth::record_auth_failure(
            &axum::http::Method::POST,
            &path,
            &request_id,
            "missing or unknown broker token",
        )
        .await;
        return crate::error::ApiError::Unauthorized.into_response();
    };
    let body = match axum::body::to_bytes(request.into_body(), 64 * 1024).await {
        Ok(body) => body,
        Err(_) => {
            return crate::error::ApiError::BadRequest("policy check too large".into())
                .into_response()
        }
    };
    let req: PolicyCheckRequest = match serde_json::from_slice(&body) {
        Ok(req) => req,
        Err(e) => {
            return crate::error::ApiError::BadRequest(format!("policy check: {e}")).into_response()
        }
    };
    let allow = serde_json::json!({"decision": "allow"});
    if req.tool_name != "Bash" {
        return Json(allow).into_response();
    }
    let Ok(session) = state.sessions.get(id) else {
        return Json(allow).into_response();
    };
    let Some(policy) = session.policy() else {
        return Json(allow).into_response();
    };
    let cwd = if req.cwd.is_empty() {
        std::path::PathBuf::from(&session.cwd)
    } else {
        std::path::PathBuf::from(&req.cwd)
    };
    let command = req.tool_input.command;
    // The check reads a commit-message file and runs `git rev-parse`, both
    // blocking, so it runs off the runtime and under a bound: a check that
    // does not return is a refusal, not a pass.
    let verdict = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        tokio::task::spawn_blocking(move || crate::policy::check(&policy, &command, &cwd)),
    )
    .await;
    match verdict {
        Ok(Ok(Some(reason))) => {
            tracing::warn!(session = %id, event = "policy.deny", "project policy refused a command");
            Json(serde_json::json!({"decision": "deny", "reason": reason})).into_response()
        }
        Ok(Ok(None)) => Json(allow).into_response(),
        Ok(Err(_)) | Err(_) => {
            // A check that did not finish says nothing, so it refuses, the same
            // way the hook refuses a check it cannot read.
            tracing::warn!(session = %id, event = "policy.timeout", "project policy check did not finish");
            Json(serde_json::json!({
                "decision": "deny",
                "reason": "policy check timed out",
            }))
            .into_response()
        }
    }
}

/// Only vogt-core's own identity may change a session's grants (WI-973): the
/// core sends one only after a person approved it, so every other credential
/// — a person's token, the break-glass token, a session's token — is refused
/// even though it holds `sessions`.
fn require_core_identity(
    identity: Option<&crate::auth::AuthorizedIdentity>,
) -> crate::error::Result<()> {
    match identity {
        // The flag, not the name: `name` is also a core actor's identity_ref,
        // which an admin can set to any string.
        Some(identity) if identity.stack_secret => Ok(()),
        Some(identity) => Err(crate::error::ApiError::Forbidden(format!(
            "only vogt-core applies grants, after a person approves one (session_grant_decide); \
             {} cannot",
            identity.name
        ))),
        None => Err(crate::error::ApiError::Unauthorized),
    }
}

fn grant_view(held: &crate::secret_broker::HeldGrant) -> vogt_engine_contract::SessionGrant {
    held.grant.clone()
}

/// `POST /api/sessions/{id}/grants` — hold a person-approved credential grant
/// for a live session (WI-973). vogt-core only.
pub async fn apply_session_grant(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    identity: Option<axum::Extension<crate::auth::AuthorizedIdentity>>,
    Json(grant): Json<vogt_engine_contract::SessionGrant>,
) -> Result<Json<vogt_engine_contract::SessionGrant>> {
    use crate::{error::ApiError, secret_broker::GrantRefusal};
    require_core_identity(identity.as_deref())?;
    let session = state.sessions.get(id)?;
    if !session.is_alive() {
        return Err(ApiError::Conflict(
            "the session has exited; a grant is for a running session".into(),
        ));
    }
    let held = state
        .sessions
        .secret_broker()
        .apply_grant(id, grant, time::OffsetDateTime::now_utc())
        .map_err(|refusal| match refusal {
            GrantRefusal::Invalid(_) => ApiError::BadRequest(refusal.to_string()),
            GrantRefusal::ProjectNotGrantable(_) => ApiError::Forbidden(refusal.to_string()),
            GrantRefusal::NoBroker
            | GrantRefusal::ShadowsManifest(_)
            | GrantRefusal::VarInUse(_) => ApiError::Conflict(refusal.to_string()),
        })?;
    tracing::info!(
        target: "vogt::audit",
        event = "session.grant",
        action = "applied",
        session_id = %id,
        grant_id = %held.grant.grant_id,
        var = %held.grant.var,
        project_id = %held.grant.project_id,
        secret_name = %held.grant.secret_name,
        uses = ?held.grant.uses,
        expires_at = %held.grant.expires_at,
        "approved grant applied to the session"
    );
    Ok(Json(grant_view(&held)))
}

/// `DELETE /api/sessions/{id}/grants/{grant_id}` — revoke one grant at once
/// (WI-973). vogt-core only. Revoking a grant the engine no longer holds is
/// not an error: absence is the revoked state.
pub async fn revoke_session_grant(
    State(state): State<Arc<AppState>>,
    Path((id, grant_id)): Path<(Uuid, String)>,
    identity: Option<axum::Extension<crate::auth::AuthorizedIdentity>>,
) -> Result<Json<serde_json::Value>> {
    require_core_identity(identity.as_deref())?;
    let held = state.sessions.secret_broker().remove_grant(id, &grant_id);
    tracing::info!(
        target: "vogt::audit",
        event = "session.grant",
        action = "revoked",
        session_id = %id,
        grant_id = %grant_id,
        held,
        "grant revoked"
    );
    Ok(Json(serde_json::json!({ "revoked": held })))
}

/// `GET /api/sessions/{id}/grants` — the grants a session holds now, never a
/// value. An operator's view: the stack secret, the break-glass token or an
/// `admin`-scoped token. Every `work.write` token holds `sessions`, and
/// which secrets another session was granted is not every session's to
/// read; a session sees its own with `GET /api/agent-auth/grants`.
pub async fn list_session_grants(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    identity: Option<axum::Extension<crate::auth::AuthorizedIdentity>>,
) -> Result<Json<Vec<vogt_engine_contract::SessionGrant>>> {
    use crate::{auth::TokenCapability, error::ApiError};
    match identity.as_deref() {
        Some(identity) if identity.stack_secret || identity.allows(TokenCapability::GuiControl) => {
        }
        Some(identity) => {
            return Err(ApiError::Forbidden(format!(
                "listing a session's grants is an operator's read (the admin scope, the \
                 break-glass token or vogt-core); {} is none of those — a session lists \
                 its own with `vogt-agent-auth grants`",
                identity.name
            )));
        }
        None => return Err(ApiError::Unauthorized),
    }
    state.sessions.get(id)?;
    let now = time::OffsetDateTime::now_utc();
    Ok(Json(
        state
            .sessions
            .secret_broker()
            .grants_for(id, now)
            .iter()
            .map(grant_view)
            .collect(),
    ))
}

/// Let an oversight session answer `dialog` on `session`, or refuse it as
/// `person_gate` would (WI-1831, WI-2028, `oversight_answer`). The overseer
/// must be a live session the engine has marked oversight, other than the
/// target, and the dialog a `permission` prompt. Audited either way, as the
/// oversight session and with the command excerpt.
fn oversight_may_answer(
    state: &AppState,
    session: &crate::pty::Session,
    dialog: &crate::approval::Detected,
    overseer: &str,
    via: &str,
    label: &str,
) -> Result<()> {
    let who = format!("oversight session {overseer}");
    let refuse = |why: String| {
        tracing::warn!(
            target: "vogt::audit",
            event = "session.permission_answer",
            outcome = "refused",
            via = %via,
            session_id = %session.id,
            overseer = %overseer,
            why = %why,
            "refused an oversight answer to a permission prompt"
        );
        crate::person_gate::refusal(&format!("{who} ({why})"), dialog)
    };
    let caller = Uuid::parse_str(overseer)
        .ok()
        .and_then(|id| state.sessions.get(id).ok());
    let Some(caller) = caller else {
        return Err(refuse("no such session".into()));
    };
    if caller.id == session.id {
        return Err(refuse("a session does not answer its own prompt".into()));
    }
    if caller.role() != vogt_engine_contract::SessionRole::Oversight {
        return Err(refuse("it is not an oversight session".into()));
    }
    let command = crate::oversight_answer::may_answer(dialog).map_err(refuse)?;
    tracing::info!(
        target: "vogt::audit",
        event = "session.permission_answer",
        outcome = "answered",
        via = %via,
        session_id = %session.id,
        overseer = %overseer,
        question = %dialog.question,
        command = %command,
        label = %label,
        "an oversight session answered a permission prompt"
    );
    Ok(())
}

/// Choose one option of the dialog on screen (WI-917): a permission
/// dialog or a startup gate. The engine reads the menu as it is now, moves
/// the highlight to the option with arrow keys, presses Enter, and looks
/// again to report whether the dialog went away. `409` when no dialog is
/// showing, when it no longer asks `expect_question`, or when the option is
/// not on its menu.
///
/// A permission dialog (not a startup gate) is answered only by a person
/// (WI-983, `person_gate`): `403 person required` for anyone else, before
/// anything is typed.
pub async fn answer_session(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    identity: Option<axum::Extension<crate::auth::AuthorizedIdentity>>,
    Json(req): Json<vogt_engine_contract::AnswerRequest>,
) -> Result<Json<vogt_engine_contract::AnswerResult>> {
    use crate::error::ApiError;
    let session = state.sessions.get(id)?;
    let reading = Arc::clone(&session);
    let dialog = tokio::task::spawn_blocking(move || reading.current_dialog())
        .await
        .map_err(|e| ApiError::Internal(format!("read the screen: {e}")))?
        .ok_or_else(|| ApiError::Conflict("no dialog is showing on this session".into()))?;
    let identity = identity.as_deref();
    let person = crate::person_gate::is_person(identity, req.person);
    // An overseer named by vogt-core, and only by vogt-core: checked against
    // the chosen option below, once the menu is read (WI-1831).
    let overseer = match (identity, req.overseer.as_deref()) {
        (Some(i), Some(overseer)) if i.stack_secret && !person => Some(overseer),
        _ => None,
    };
    if crate::person_gate::needs_person(dialog.kind) && !person && overseer.is_none() {
        let who = identity.map_or("unidentified", |i| i.name.as_str());
        return Err(crate::person_gate::refuse(
            &session,
            who,
            &dialog,
            crate::person_gate::Via::Answer,
        ));
    }
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
    if let Some(overseer) = overseer.filter(|_| crate::person_gate::needs_person(dialog.kind)) {
        oversight_may_answer(&state, &session, &dialog, overseer, "answer", &chosen.label)?;
    }
    let keys = crate::approval::keys_to_choose(&dialog.options, chosen.number, dialog.horizontal)
        .ok_or_else(|| ApiError::Conflict("the option is not on the menu".into()))?;
    // Arrows one write each, then Enter: a TUI reading a burst can merge an
    // escape sequence with what follows it.
    for key in keys.split_inclusive(['A', 'B', 'C', 'D', '\r']) {
        session
            .write_input(key.as_bytes())
            .map_err(|e| ApiError::Pty(format!("write input: {e}")))?;
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    crate::person_gate::audit_answer(
        &session,
        identity,
        req.person,
        dialog.kind,
        &dialog.question,
        chosen.number,
        &chosen.label,
    );
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
    identity: Option<axum::Extension<crate::auth::AuthorizedIdentity>>,
    Json(spec): Json<SessionSpec>,
) -> Result<Json<SessionSummary>> {
    // Starting an overseer is nominating one (WI-1255): a worker that could
    // start one would drive its terminal and borrow its standing.
    if !spec.role.is_worker() {
        require_role_authority(identity.as_deref(), "(new)", spec.role)?;
    }
    let s = state.sessions.create_launched(spec).await?;
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
pub struct RepliesQuery {
    /// How many of the latest replies (default 1, at most 20).
    pub n: Option<usize>,
}

/// `GET /api/sessions/{id}/replies` — an opencode session's last assistant
/// replies, read from opencode's own store (WI-931). Claude Code and Codex
/// write transcripts the core reads itself; opencode keeps one database that
/// also holds its credentials, so the engine reads the session's assistant
/// text out of it and nothing else. `replies` is empty for a session that is
/// not opencode, or whose conversation is not known yet. Unredacted: the
/// core redacts before showing it, as it does a transcript.
pub async fn get_session_replies(
    Path(id): Path<Uuid>,
    Query(q): Query<RepliesQuery>,
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>> {
    let conversation = match state.sessions.hibernated_screen(id) {
        Some((_, _, _, summary)) => summary.conversation,
        None => state.sessions.get(id)?.conversation(),
    };
    let n = q.n.unwrap_or(1).clamp(1, crate::opencode::MAX_REPLIES);
    let Some(conversation) = conversation.filter(|c| c.agent == "opencode") else {
        return Ok(Json(
            serde_json::json!({ "agent": null, "conversation_id": null, "replies": [] }),
        ));
    };
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_default();
    let replies =
        crate::opencode::last_replies(&crate::opencode::db_path(&home), &conversation.id, n)
            .await
            .unwrap_or_default();
    Ok(Json(serde_json::json!({
        "agent": "opencode",
        "conversation_id": conversation.id,
        "replies": replies,
    })))
}

#[derive(Debug, Deserialize)]
pub struct SweepQuery {
    /// How many non-blank screen lines each row carries (default 8, at
    /// most 40; 0 for none).
    pub screen_lines: Option<usize>,
    /// Include sessions whose process has exited (default false).
    #[serde(default)]
    pub include_exited: bool,
    /// Include sessions the default list hides (WI-1289). Off by default, so
    /// a sweep is the same set the rail shows.
    #[serde(default)]
    pub include_hidden: bool,
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
        .list(q.include_hidden)
        .sessions
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

/// The workspace volume's usage, and whether it is past the threshold
/// (WI-1304). A volume that cannot be read is reported as needing attention
/// rather than as empty and fine.
fn disk_status(path: &std::path::Path, threshold_percent: u8) -> DiskStatus {
    disk_status_from(
        path,
        crate::worktree_clean::volume_usage(path),
        threshold_percent,
    )
}

fn disk_status_from(
    path: &std::path::Path,
    usage: Option<crate::worktree_clean::VolumeUsage>,
    threshold_percent: u8,
) -> DiskStatus {
    match usage {
        Some(usage) => DiskStatus {
            path: path.display().to_string(),
            total_bytes: usage.total_bytes,
            free_bytes: usage.free_bytes,
            used_percent: usage.used_percent,
            threshold_percent,
            needs_attention: usage.used_percent >= threshold_percent,
        },
        None => DiskStatus {
            path: path.display().to_string(),
            total_bytes: 0,
            free_bytes: 0,
            used_percent: 0,
            threshold_percent,
            needs_attention: true,
        },
    }
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
    /// Whether the principal behind this input is a person, as vogt-core
    /// relays it (WI-983); read only from the stack secret and the
    /// break-glass token, as for `AnswerRequest::person`.
    #[serde(default)]
    pub person: Option<bool>,
    /// The engine id of the oversight session vogt-core is relaying
    /// (WI-2028), read only from vogt-core's own credential, as for
    /// `AnswerRequest::overseer`.
    #[serde(default)]
    pub overseer: Option<String>,
}

/// Type into a session. Refused with `403 person required`, nothing typed,
/// when a permission prompt is showing and the caller is not a person: on a
/// modal dialog every keystroke is an answer to it (WI-983). An oversight
/// session vogt-core relays may answer a `permission` prompt (WI-2028).
pub async fn session_input(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    identity: Option<axum::Extension<crate::auth::AuthorizedIdentity>>,
    Json(req): Json<SessionInputReq>,
) -> Result<Json<OkResponse>> {
    if req.text.len() > MAX_HTTP_INPUT_BYTES {
        return Err(crate::error::ApiError::BadRequest(format!(
            "input exceeds {MAX_HTTP_INPUT_BYTES} bytes"
        )));
    }
    let session = state.sessions.get(id)?;
    let identity = identity.map(|axum::Extension(identity)| identity);
    let person = crate::person_gate::is_person(identity.as_ref(), req.person);
    let overseer = match (identity.as_ref(), req.overseer.clone()) {
        (Some(i), Some(overseer)) if i.stack_secret && !person => Some(overseer),
        _ => None,
    };
    if !person {
        let reading = Arc::clone(&session);
        let person = req.person;
        let gate_state = Arc::clone(&state);
        tokio::task::spawn_blocking(move || match overseer {
            Some(overseer) => match crate::person_gate::permission_dialog(&reading) {
                Some(dialog) => oversight_may_answer(
                    &gate_state,
                    &reading,
                    &dialog,
                    &overseer,
                    "input",
                    "(keys)",
                ),
                None => Ok(()),
            },
            None => crate::person_gate::guard(
                &reading,
                identity.as_ref(),
                person,
                crate::person_gate::Via::Input,
            ),
        })
        .await
        .map_err(|e| crate::error::ApiError::Internal(format!("read the screen: {e}")))??;
    }
    let bytes = crate::screen::input_bytes(&req.text, session.bracketed_paste(), req.submit);
    session
        .write_input(&bytes)
        .map_err(|e| crate::error::ApiError::Pty(format!("write input: {e}")))?;
    // Only a person moves the retention clock. An agent's `/input` — the
    // common case, and what `session.input` relays with `person: false` —
    // writes the PTY and leaves `last_person_input_at` where it was.
    if person {
        session.note_person_input();
    }
    Ok(Json(OkResponse::new(true)))
}

pub async fn events_stream(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
) -> Sse<impl Stream<Item = std::result::Result<Event, Infallible>>> {
    let last_event_id = headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok());
    // Subscribe before reading the ring, so an event published between the
    // two is on the live stream and deduped by seq rather than lost.
    let (replay, truncated, rx) = crate::changes::replay_then_live(&state.bus, last_event_id);
    let bus = state.bus.clone();
    // One threshold dedupes the overlap between the replay and the live
    // channel, which only delivers in seq order, without a set that grows
    // for the life of the connection. It is the highest seq already handed to
    // this stream: the last replayed seq. Not the client's cursor: after an
    // engine restart the cursor is from the previous run's sequence and can
    // be far above anything this run publishes, and every new event would be
    // dropped. An event at or below a current cursor cannot arrive live
    // anyway — it was published before the subscription.
    let replay_max = replay.last().map(|ev| ev.seq).unwrap_or(0);
    let mut prelude = Vec::new();
    if truncated {
        bus.note_lag("sse-events", 0);
        prelude.push(Ok(Event::default().data(
            serde_json::json!({ "type": "lagged", "skipped": 0 }).to_string(),
        )));
    }
    for sequenced in replay {
        prelude.push(Ok(Event::default()
            .id(sequenced.seq.to_string())
            .data(serde_json::to_string(&sequenced.event).unwrap_or_default())));
    }
    let live = BroadcastStream::new(rx).filter_map(move |res| match res {
        Ok(sequenced) => {
            if sequenced.seq <= replay_max {
                return None;
            }
            match serde_json::to_string(&sequenced.event) {
                Ok(json) => Some(Ok(Event::default()
                    .id(sequenced.seq.to_string())
                    .data(json))),
                Err(_) => None,
            }
        }
        Err(tokio_stream::wrappers::errors::BroadcastStreamRecvError::Lagged(skipped)) => {
            bus.note_lag("sse-events", skipped);
            Some(Ok(Event::default().data(
                serde_json::json!({ "type": "lagged", "skipped": skipped }).to_string(),
            )))
        }
    });
    let stream = futures_util::stream::iter(prelude).chain(live);
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
    /// Quick chats (WI-1097): agent processes running now and their total
    /// resident memory. Absent when chats are off.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chats: Option<ChatStatus>,
    /// The workspace volume (WI-1304). `needs_attention` once used space
    /// passes `ENGINE_DISK_WARN_PERCENT` (85 by default).
    pub disk: DiskStatus,
}

#[derive(Debug, Serialize)]
pub struct ChatStatus {
    pub running: usize,
    pub rss_bytes: u64,
}

#[derive(Debug, Serialize)]
pub struct DiskStatus {
    pub path: String,
    pub total_bytes: u64,
    pub free_bytes: u64,
    pub used_percent: u8,
    pub threshold_percent: u8,
    pub needs_attention: bool,
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
    /// Live sessions that started without their brokered credentials because
    /// the secrets manager was unreachable (WI-1812).
    pub degraded_session_count: usize,
    /// The most recent of them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub degraded: Option<crate::launch::DegradedLaunch>,
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
    let (degraded_sessions, last_degraded) =
        crate::launch::degraded_summary(&state.sessions.live_sessions());

    Ok(Json(OperationalStatus {
        version: crate::product::VERSION,
        product_version: crate::product::VERSION,
        source_ref: crate::product::SOURCE_REF,
        source_sha: crate::product::SOURCE_SHA,
        release_url: crate::product::release_url(),
        session_count: state.sessions.list(false).sessions.len(),
        event_lag: state.bus.lags(),
        chats: match state.chats.clone() {
            Some(chats) => {
                let (running, rss_bytes) = tokio::task::spawn_blocking(move || chats.usage())
                    .await
                    .unwrap_or((0, 0));
                Some(ChatStatus { running, rss_bytes })
            }
            None => None,
        },
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
            degraded_session_count: degraded_sessions,
            degraded: last_degraded,
        },
        storage: ServerStorageStatus {
            state_dir: state.config.state_dir.display().to_string(),
            workspace_root: state.config.workspace_root.display().to_string(),
        },
        disk: disk_status(&state.config.workspace_root, state.config.disk_warn_percent),
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

#[cfg(test)]
mod disk_status_tests {
    use super::*;
    use crate::worktree_clean::VolumeUsage;

    fn at(percent: u8) -> Option<VolumeUsage> {
        Some(VolumeUsage {
            total_bytes: 100,
            free_bytes: 100 - u64::from(percent),
            used_percent: percent,
        })
    }

    #[test]
    fn needs_attention_from_the_threshold_up() {
        let path = std::path::Path::new("/w");
        assert!(!disk_status_from(path, at(84), 85).needs_attention);
        assert!(disk_status_from(path, at(85), 85).needs_attention);
        assert!(disk_status_from(path, at(100), 85).needs_attention);
        assert!(!disk_status_from(path, at(95), 100).needs_attention);
        let row = disk_status_from(path, at(50), 85);
        assert_eq!(row.threshold_percent, 85);
        assert_eq!(row.used_percent, 50);
        assert_eq!(row.path, "/w");
    }

    /// A volume that cannot be read is not reported as an empty, fine one.
    #[test]
    fn an_unreadable_volume_needs_attention() {
        let missing = std::path::Path::new("/nonexistent/vogt-wi-1304/volume");
        let row = disk_status(missing, 85);
        assert!(row.needs_attention);
        assert_eq!(row.total_bytes, 0);
    }
}
