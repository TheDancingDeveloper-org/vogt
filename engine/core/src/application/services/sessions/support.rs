use super::*;

// -- shared helpers -----------------------------------------------------------

/// The scopes a session's own token carries, from the deployment's config.
pub(super) fn session_scopes<C, I>(ctx: &AppContext<C, I>) -> Result<Vec<String>, VogtError>
where
    C: Clock,
    I: IdFactory,
{
    auth::parse_scopes(&ctx.config.agent_session_scopes)
        .map(|scopes| scopes.into_iter().map(str::to_string).collect())
        .map_err(VogtError::InvalidRequest)
}

/// A fresh token secret and the hash that is all the store keeps.
pub(super) fn minted_token() -> Result<(String, String), VogtError> {
    let mut entropy = [0u8; TOKEN_ENTROPY_BYTES];
    getrandom::getrandom(&mut entropy)
        .map_err(|_| VogtError::InvalidRequest("the system RNG is unavailable".to_string()))?;
    Ok(auth::issue(&entropy))
}

/// The engine client, or the unconfigured error.
pub(crate) fn engine_of<C, I>(ctx: &AppContext<C, I>) -> Result<&EngineClient, VogtError>
where
    C: Clock,
    I: IdFactory,
{
    ctx.engine.as_ref().ok_or_else(|| {
        VogtError::EngineUnavailable(
            "no session engine is configured, so there is nothing to open a \
             terminal on (set VOGT_ENGINE_URL)"
                .to_string(),
        )
    })
}

/// A session named by either id form, resolved to the engine's id. `session` is
/// Vogt's record when there is one; `None` means the engine id names a session
/// Vogt never linked, which the caller may still read or drive.
pub(crate) struct Target {
    pub engine_session_id: String,
    pub session: Option<CodingSession>,
}

/// Resolve a `ses_…` id or an engine UUID to the engine's session id.
///
/// A `ses_…` id must be one Vogt recorded — otherwise it is a wrong id and says
/// so. Anything else is the engine's own id, passed through as given and linked
/// to Vogt's record when one exists. Shared with the grants half (WI-1074).
pub(crate) fn resolve_target<C, I>(
    ctx: &AppContext<C, I>,
    session_id: &str,
) -> Result<Target, VogtError>
where
    C: Clock,
    I: IdFactory,
{
    let wanted = session_id.trim();
    if wanted.is_empty() {
        return Err(VogtError::InvalidRequest(
            "a session id is required (ses_… or the engine's session UUID)".to_string(),
        ));
    }
    let view = ctx.declared.read()?;
    if let Some(id) = wanted.strip_prefix("ses_") {
        let _ = id;
        let session = view
            .session_by_id(wanted)?
            .ok_or_else(|| VogtError::NotFound(format!("no session {}", py_repr(wanted))))?;
        return Ok(Target {
            engine_session_id: session.engine_session_id.clone(),
            session: Some(session),
        });
    }
    Ok(Target {
        engine_session_id: wanted.to_string(),
        session: view.session_by_engine_id(wanted)?,
    })
}

/// What an agent inside the session needs to reach Vogt.
pub(super) fn session_env<C, I>(
    ctx: &AppContext<C, I>,
    session_id: &str,
    secret: &str,
    work_item: Option<&str>,
) -> Vec<(String, String)>
where
    C: Clock,
    I: IdFactory,
{
    let mut env = vec![
        ("VOGT_HTTP_TOKEN".to_string(), secret.to_string()),
        ("VOGT_SESSION_ID".to_string(), session_id.to_string()),
    ];
    if let Some(work_item) = work_item.filter(|item| !item.is_empty()) {
        env.push(("VOGT_WORK_ITEM".to_string(), work_item.to_string()));
    }
    if let Some(url) = ctx.config.public_url.as_ref().filter(|url| !url.is_empty()) {
        env.push(("VOGT_URL".to_string(), url.clone()));
    }
    if let Some(engine) = &ctx.engine {
        env.push(("VOGT_ENGINE_URL".to_string(), engine.base_url().to_string()));
    }
    env
}

/// What the audit row records about a session. Everything except the credential.
pub(super) fn audited_payload(session: &CodingSession) -> Value {
    json!({
        "id": session.id,
        "engine_session_id": session.engine_session_id,
        "project_id": session.project_id,
        "work_item_id": session.work_item_id,
        "actor_id": session.actor_id,
        "cwd": session.cwd,
        "template": session.template,
        "model": session.model,
        "effort": session.effort,
        "purpose": session.purpose,
        "started_by_session": session.started_by_session,
        "started_by_kind": session.started_by_kind,
        "started_at": session.started_at.to_iso(),
        "stopped_at": session.stopped_at.map(|moment| moment.to_iso()),
    })
}

pub(super) fn now<C, I>(
    writing: &writes::WriteContext<'_, C, I, impl crate::storage::interface::DeclaredStore>,
) -> Moment
where
    C: Clock,
    I: IdFactory,
{
    writing.clock().lock().expect("the shared clock").now()
}

pub(super) fn fresh<C, I>(
    writing: &writes::WriteContext<'_, C, I, impl crate::storage::interface::DeclaredStore>,
    prefix: &str,
) -> String
where
    C: Clock,
    I: IdFactory,
{
    writing
        .ids()
        .lock()
        .expect("the shared id factory")
        .next(prefix)
}

/// A serde failure while rendering a result the service just built.
fn json_error(error: serde_json::Error) -> VogtError {
    VogtError::InvalidRequest(format!("could not render the result: {error}"))
}

/// The work item a branch name mentions, for a session credited to one.
pub(in crate::application::services) fn branch_work_item(
    view: &impl ReadView,
    text: &str,
) -> Option<WorkItem> {
    let references = crate::decisions::issue_references(text);
    let reference = references.first()?;
    resolve::work_item(view, reference).ok()
}

/// A person is required and the caller is an agent.
pub(in crate::application::services) fn person_required(
    kind: ActorKind,
    action: &str,
) -> Result<(), VogtError> {
    if kind == ActorKind::Agent {
        Err(VogtError::PersonRequired(format!(
            "{action} can only be done by a person, not by an agent"
        )))
    } else {
        Ok(())
    }
}

/// Silence the unused-import warning for the sweep entry type alias path.
fn _sweep_entry(entry: &EngineSweepEntry) -> &EngineSession {
    &entry.session
}

/// A started session's clock read, so a test can pin it.
#[expect(dead_code, reason = "ported ahead of the callers that will wire it in")]
fn started_at(session: &CodingSession) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(session.started_at.unix_seconds().max(0) as u64)
}
