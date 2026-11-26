use super::*;

/// One session as the list shows it, live fields merged over the declared row.
pub(in crate::application::services) fn summarize<C, I>(
    ctx: &AppContext<C, I>,
    session: &CodingSession,
    live: Option<&EngineSession>,
) -> Result<SessionSummary, VogtError>
where
    C: Clock,
    I: IdFactory,
{
    summarize_asked(ctx, session, live, true)
}

/// `asked` is whether the engine was reached: `alive` is its answer then, and
/// null when it could not be asked, which is a different fact from "not running".
pub(super) fn summarize_asked<C, I>(
    ctx: &AppContext<C, I>,
    session: &CodingSession,
    live: Option<&EngineSession>,
    asked: bool,
) -> Result<SessionSummary, VogtError>
where
    C: Clock,
    I: IdFactory,
{
    let project = ctx.declared.read()?.project_by_id(&session.project_id)?;
    let work_item = match session.work_item_id.as_deref() {
        Some(id) => ctx.declared.read()?.work_item_by_id(id)?,
        None => None,
    };
    let actor = ctx.declared.read()?.actor_by_id(&session.actor_id)?;
    let mut summary = SessionSummary {
        id: session.id.clone(),
        engine_session_id: session.engine_session_id.clone(),
        linked: true,
        project: project.map(|project| project.slug),
        work_item: work_item.as_ref().map(|item| item.reference.clone()),
        work_item_title: work_item.as_ref().map(|item| item.title.clone()),
        work_item_state: work_item.map(|item| item.state.to_string()),
        actor: Some(
            actor
                .map(|actor| actor.identity_ref)
                .unwrap_or(session.actor_id.clone()),
        ),
        cwd: session.cwd.clone(),
        template: session.template.clone(),
        model: session.model.clone(),
        effort: session.effort.clone(),
        reason: Some(session.reason.clone()),
        purpose: session.purpose.clone(),
        started_by_session: session.started_by_session.clone(),
        started_by_kind: session.started_by_kind.clone(),
        started_at: Some(session.started_at),
        stopped_at: session.stopped_at,
        name: None,
        command: None,
        exit_code: None,
        activity_changed_at: None,
        conversation_agent: None,
        activity: None,
        alive: asked.then(|| live.is_some_and(|live| live.alive)),
        turn_started_at: None,
        last_output_at: None,
        approval: None,
        blocked: None,
        reported_status: None,
        last_reply_excerpt: None,
        hibernation: None,
        // None until the engine was asked and answered: an engine that was
        // not asked has nothing to report, not a default of false/"worker".
        keep_awake: None,
        autopilot: None,
        autopilot_nudges: None,
        role: None,
        stopped_by: None,
        stop_reason: None,
        permission_mode: None,
        posture_lost_on_resume: false,
        account: None,
        budget: None,
        running: None,
        resources: None,
        conversation_id: None,
        done_at: None,
        latest_report: None,
        economics: None,
    };
    if let Some(live) = live {
        apply_live(&mut summary, live);
    }
    Ok(summary)
}

/// Copy the engine's live fields onto a summary.
fn apply_live(summary: &mut SessionSummary, live: &EngineSession) {
    summary.activity = Some(live.activity.clone());
    summary.done_at = live.done_at.clone();
    summary.economics = live.economics.clone();
    summary.name = Some(live.name.clone());
    summary.command = live.command.clone();
    summary.exit_code = live.exit_code;
    summary.activity_changed_at = live.activity_changed_at.clone();
    summary.conversation_agent = live.conversation_agent.clone();
    summary.turn_started_at = live.turn_started_at.clone();
    summary.last_output_at = live.last_output_at.clone();
    summary.approval = live.approval.as_ref().map(approval_of);
    summary.blocked = live.blocked.as_ref().map(blocked_of);
    summary.hibernation = live.hibernation.as_ref().map(hibernation_of);
    summary.keep_awake = Some(live.keep_awake);
    summary.autopilot = Some(live.autopilot);
    summary.autopilot_nudges = Some(live.autopilot_nudges);
    summary.role = Some(live.role.clone());
    summary.account = live.account.clone();
    summary.budget = budget_of_engine(live.budget.as_ref());
    summary.conversation_id = live.conversation_id.clone();
    summary.resources = live.resources.as_ref().map(resources_of);
    summary.permission_mode = live.permission_mode.clone();
    summary.posture_lost_on_resume = live.posture_lost_on_resume;
    summary.stopped_by = live.stopped_by.clone();
    summary.stop_reason = live.stop_reason.clone();
}

fn transcript_of<C, I>(
    ctx: &AppContext<C, I>,
    session: &CodingSession,
    live: Option<&EngineSession>,
) -> Option<transcripts::Transcript>
where
    C: Clock,
    I: IdFactory,
{
    let roots: HashMap<&str, &Path> = ctx
        .config
        .session_transcript_roots
        .iter()
        .map(|(agent, path)| (agent.as_str(), path.as_path()))
        .collect();
    let started = UNIX_EPOCH + Duration::from_secs(session.started_at.unix_seconds().max(0) as u64);
    transcripts::find_in(
        &roots,
        &ctx.config.claude_account_dirs,
        &session.engine_session_id,
        live.and_then(|session| session.command.as_deref()),
        session.template.as_deref(),
        live.map(|session| session.cwd.as_str()),
        Some(started),
        false,
    )
}

/// The transcript a listed session's conversation is in. Only the id-based
/// lookups: a list must stay cheap and must not guess which of two agents in
/// one directory a session is, so no start time and no cwd guess.
pub(super) fn transcript_for<C, I>(
    ctx: &AppContext<C, I>,
    live: &EngineSession,
    template: Option<&str>,
) -> Option<transcripts::Transcript>
where
    C: Clock,
    I: IdFactory,
{
    if ctx.config.session_transcript_roots.is_empty() {
        return None;
    }
    let roots: HashMap<&str, &Path> = ctx
        .config
        .session_transcript_roots
        .iter()
        .map(|(agent, path)| (agent.as_str(), path.as_path()))
        .collect();
    transcripts::find_in(
        &roots,
        &ctx.config.claude_account_dirs,
        &live.id,
        live.command.as_deref(),
        template,
        Some(live.cwd.as_str()),
        None,
        false,
    )
}

/// What the session is actually running: the transcript's latest turn, else
/// the command's flags, else what was asked. Null when none of the three
/// says anything.
pub(super) fn running_of(
    summary: &SessionSummary,
    live: &EngineSession,
    found: Option<&transcripts::Transcript>,
) -> Option<SessionRuntime> {
    let (model, effort) = found.map(transcripts::runtime).unwrap_or((None, None));
    let resolved = decisions::resolve_runtime(
        live.command.as_deref(),
        live.conversation_agent.as_deref(),
        model.as_deref(),
        effort.as_deref(),
        summary.model.as_deref(),
        summary.effort.as_deref(),
    );
    if resolved.agent.is_none() && resolved.model.is_none() && resolved.effort.is_none() {
        return None;
    }
    Some(SessionRuntime {
        agent: resolved.agent,
        model: resolved.model,
        model_basis: resolved.model_basis.map(str::to_string),
        effort: resolved.effort,
        effort_basis: resolved.effort_basis.map(str::to_string),
    })
}

/// An opencode session's last reply, from the engine: opencode keeps no
/// transcript file. An engine that cannot be asked, or one that has no such
/// session, is a null.
pub(super) fn opencode_excerpt<C, I>(
    ctx: &AppContext<C, I>,
    engine_session_id: &str,
) -> Option<String>
where
    C: Clock,
    I: IdFactory,
{
    let engine = ctx.engine.as_ref()?;
    let found = engine.session_replies(engine_session_id, 1).ok()??;
    found.1.last().map(|(text, _at)| transcripts::excerpt(text))
}

fn runtime_of(
    session: &CodingSession,
    live: Option<&EngineSession>,
    transcript: Option<&transcripts::Transcript>,
) -> decisions::ResolvedRuntime {
    let (model, effort) = transcript.map(transcripts::runtime).unwrap_or((None, None));
    decisions::resolve_runtime(
        live.and_then(|session| session.command.as_deref()),
        live.and_then(|session| session.conversation_agent.as_deref()),
        model.as_deref(),
        effort.as_deref(),
        session.model.as_deref(),
        session.effort.as_deref(),
    )
}

pub(super) fn approval_of(approval: &EngineApproval) -> SessionApproval {
    SessionApproval {
        question: approval.question.clone(),
        command_excerpt: approval.command_excerpt.clone(),
        kind: approval.kind.clone(),
        options: approval
            .options
            .iter()
            .map(|(number, label, selected)| SessionApprovalOption {
                number: *number,
                label: label.clone(),
                selected: *selected,
            })
            .collect(),
        deadline_seconds: approval.deadline_seconds,
        deadline_at: approval.deadline_at.clone(),
        detected_at: (!approval.detected_at.is_empty()).then(|| approval.detected_at.clone()),
    }
}

pub(super) fn blocked_of(blocked: &EngineBlocked) -> SessionBlocked {
    SessionBlocked {
        blocker: blocked.reason.clone(),
        items: blocked.items.clone(),
        since: blocked.since.clone(),
        needs: known_or(&blocked.needs, &NEEDS, "person"),
        boundary: known_or(&blocked.boundary, &BOUNDARIES, "other"),
        key: blocked.key.clone(),
    }
}

fn resources_of(resources: &EngineResources) -> SessionResources {
    SessionResources {
        rss_bytes: resources.rss_bytes,
        cpu_pct: resources.cpu_pct,
        processes: resources.processes,
        sampled_at: resources.sampled_at.clone(),
        over_threshold: resources.over_threshold,
        memory_current: resources.memory_current,
        memory_max: resources.memory_max,
        memory_peak: resources.memory_peak,
        oom_kill: resources.oom_kill,
        observed_at: resources.observed_at.clone(),
        memory_warning: resources.memory_warning,
    }
}

fn hibernation_of(hibernation: &EngineHibernation) -> SessionHibernation {
    SessionHibernation {
        at: hibernation.at.clone(),
        trigger: hibernation.trigger.clone(),
        resumable: hibernation.resumable,
        reason: hibernation.reason.clone(),
    }
}

/// A session the engine has and Vogt never recorded, carrying the engine's id
/// in both id fields and null for every declared field. Its work item, when
/// the engine named one Vogt knows, contributes its title and state.
pub(super) fn unlinked_summary(
    view: &dyn crate::storage::interface::ReadView,
    session: &EngineSession,
) -> Result<SessionSummary, VogtError> {
    let work_item = match session.work_item.as_deref() {
        Some(reference) => view.work_item_by_ref(reference)?,
        None => None,
    };
    let mut summary = SessionSummary {
        id: session.id.clone(),
        engine_session_id: session.id.clone(),
        linked: false,
        project: None,
        work_item: session.work_item.clone(),
        work_item_title: work_item.as_ref().map(|item| item.title.clone()),
        work_item_state: work_item.map(|item| item.state.to_string()),
        actor: None,
        cwd: session.cwd.clone(),
        template: None,
        model: None,
        effort: None,
        reason: None,
        purpose: None,
        started_by_session: None,
        started_by_kind: None,
        started_at: session
            .created_at
            .as_deref()
            .and_then(|text| crate::core::from_iso(text).ok()),
        stopped_at: None,
        name: None,
        command: None,
        exit_code: None,
        activity_changed_at: None,
        conversation_agent: None,
        activity: None,
        alive: Some(session.alive),
        turn_started_at: None,
        last_output_at: None,
        approval: None,
        blocked: None,
        reported_status: None,
        last_reply_excerpt: None,
        hibernation: None,
        keep_awake: None,
        autopilot: None,
        autopilot_nudges: None,
        role: None,
        stopped_by: None,
        stop_reason: None,
        permission_mode: None,
        posture_lost_on_resume: false,
        account: None,
        budget: None,
        running: None,
        resources: None,
        conversation_id: None,
        done_at: None,
        latest_report: None,
        economics: None,
    };
    apply_live(&mut summary, session);
    Ok(summary)
}
