use super::*;

/// Refuse a start whose explicit `provider` is not registered or does not serve
/// `model` (WI-4317). Absent `provider` is always fine. The engine picks the
/// provider by model, so this only guarantees the named one is a valid server.
pub(super) fn check_named_provider<C, I>(
    ctx: &AppContext<C, I>,
    provider: Option<&str>,
    model: Option<&str>,
) -> Result<(), VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let Some(provider) = provider.filter(|name| !name.is_empty()) else {
        return Ok(());
    };
    let Some(model) = model.filter(|model| !model.is_empty()) else {
        return Err(VogtError::InvalidRequest(
            "provider needs a model: name the model the provider must serve".to_string(),
        ));
    };
    let listed = engine_of(ctx)?.llm_providers()?;
    let entry = listed
        .get("providers")
        .and_then(Value::as_array)
        .and_then(|providers| {
            providers
                .iter()
                .find(|one| one.get("name").and_then(Value::as_str) == Some(provider))
        });
    let Some(entry) = entry else {
        return Err(VogtError::InvalidRequest(format!(
            "no LLM provider named {provider:?} is registered on this instance; see llm_provider.list or register it with llm_provider.set"
        )));
    };
    let serves = entry
        .get("models")
        .and_then(Value::as_array)
        .is_some_and(|models| models.iter().any(|one| one.as_str() == Some(model)));
    if !serves {
        return Err(VogtError::InvalidRequest(format!(
            "LLM provider {provider:?} does not serve model {model:?}; pick a model it lists (llm_provider.list)"
        )));
    }
    Ok(())
}

/// A warning when `model` is not one a registered LLM provider serves (WI-4317).
/// The start still proceeds: the engine decides whether it can run the model.
/// Claude models and an unreachable registry give no warning.
pub(super) fn unserved_model_warning<C, I>(
    ctx: &AppContext<C, I>,
    model: Option<&str>,
) -> Option<String>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let model = model.filter(|model| !model.is_empty() && !is_claude_model(model))?;
    let listed = ctx.engine.as_ref()?.llm_providers().ok()?;
    let served = listed
        .get("providers")
        .and_then(Value::as_array)
        .is_some_and(|providers| {
            providers.iter().any(|provider| {
                provider
                    .get("models")
                    .and_then(Value::as_array)
                    .is_some_and(|models| models.iter().any(|one| one.as_str() == Some(model)))
            })
        });
    (!served).then(|| {
        format!(
            "model {model:?} is not served by any registered LLM provider on this instance; the session may fail to run it. Register one with llm_provider.set, or pick a registered model (llm_provider.list)"
        )
    })
}

/// The start fields a dispatch profile fills, in Python's override order.
pub(super) const PROFILE_FIELDS: [&str; 8] = [
    "template",
    "model",
    "effort",
    "budget",
    "worktree",
    "repo_map",
    "autopilot",
    "mcp_set",
];

/// The `mcp_set` a start names, when it names one: `worker` or `full` (WI-2833).
pub(super) fn optional_mcp_set(params: &Value) -> Result<Option<String>, VogtError> {
    let value = optional_string(params, "mcp_set")?;
    match value.as_deref() {
        None | Some("full") | Some("worker") => Ok(value),
        Some(other) => Err(VogtError::InvalidRequest(format!(
            "mcp_set must be full or worker, not {other:?}"
        ))),
    }
}

/// Python's `_budget`: the limits a start carries. Asked explicitly, that
/// object is the answer, even an empty one. Asked nothing, a worker running an
/// agent template on a task gets the default, plus the project's whole-session
/// wall clock when its policy sets one. Any other start gets none.
/// Python `_CLAUDE_MODEL`: aliases and `claude-…` ids.
fn is_claude_model(model: &str) -> bool {
    matches!(model, "opus" | "sonnet" | "haiku" | "fable") || model.starts_with("claude-")
}

/// Python `_CLAUDE_ALIAS`: the bare names only Claude Code resolves.
fn is_claude_alias(model: &str) -> bool {
    matches!(model, "opus" | "sonnet" | "haiku" | "fable")
}

/// The agent a template name means (Python `_resolved_template_tag`): a name
/// or tag of `claude`, `codex` or `opencode`, compared case-insensitively;
/// anything else is returned lower-cased.
fn resolved_template_tag(template: &str) -> String {
    let folded = template.trim().to_lowercase();
    for agent in ["claude", "codex", "opencode"] {
        if folded == agent || folded.ends_with(&format!("({agent})")) {
            return agent.to_string();
        }
    }
    // The built-in template's own name, "Claude Code (protected)".
    if folded.starts_with("claude code") {
        return "claude".to_string();
    }
    folded
}

/// The first `forbidden_text` pattern `model` matches. A pattern that does not
/// compile is skipped, as in Python: the write path already refuses one.
fn model_matches_forbidden_text<'a>(model: &str, patterns: &'a [String]) -> Option<&'a String> {
    patterns
        .iter()
        .find(|pattern| regex::Regex::new(pattern).is_ok_and(|compiled| compiled.is_match(model)))
}

/// Is `text` a hyphenated UUID (8-4-4-4-12 hex digits)?
fn is_uuid(text: &str) -> bool {
    let groups: Vec<&str> = text.split('-').collect();
    groups.len() == 5
        && groups
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(group, len)| group.len() == len && group.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Let `resume` name a Vogt session (WI-4738). A `ses_…` id or an engine UUID
/// that Vogt or the engine knows is replaced by that session's recorded agent
/// conversation id, and its `resume_template` fills a template the caller left
/// open. Anything that resolves to nothing is a raw CLI conversation id and
/// passes through unchanged. Resolved here, not in the engine: the `ses_` id
/// is Vogt's, and the engine only ever sees conversation ids.
fn resolve_resume<C, I>(
    ctx: &AppContext<C, I>,
    engine: &EngineClient,
    mut params: StartSessionParams,
) -> Result<StartSessionParams, VogtError>
where
    C: Clock,
    I: IdFactory,
{
    let Some(wanted) = params.resume.as_deref().map(str::trim) else {
        return Ok(params);
    };
    let (engine_id, known) = if wanted.starts_with("ses_") {
        match resolve_target(ctx, wanted) {
            Ok(target) => (target.engine_session_id, true),
            Err(VogtError::NotFound(_)) => return Ok(params),
            Err(error) => return Err(error),
        }
    } else if is_uuid(wanted) {
        let known = ctx.declared.read()?.session_by_engine_id(wanted)?.is_some();
        (wanted.to_string(), known)
    } else {
        return Ok(params);
    };
    let (conversation, template) = match engine.archived_session(&engine_id)? {
        Some(row) => (row.conversation_id, row.resume_template),
        // Not archived yet: a live session records its conversation too.
        None => match engine.get_session(&engine_id)? {
            // The live row's own template is the one that resumes it.
            Some(live) => (live.conversation_id, live.template),
            // Unknown to the engine. A Vogt session still has no
            // conversation to resume; any other UUID is a raw CLI id.
            None if known => (None, None),
            None => return Ok(params),
        },
    };
    let conversation = conversation.filter(|id| !id.is_empty()).ok_or_else(|| {
        VogtError::InvalidRequest(format!(
            "session {wanted} has no recorded agent conversation to resume (it never ran an \
             agent, or the engine did not capture its id); start a new session instead"
        ))
    })?;
    if let (Some(asked), Some(needed)) = (params.template.as_deref(), template.as_deref()) {
        if resolved_template_tag(asked) != resolved_template_tag(needed) {
            return Err(VogtError::InvalidRequest(format!(
                "session {wanted} resumes through template {}, not {}; drop template or name {}",
                py_repr(needed),
                py_repr(asked),
                py_repr(needed)
            )));
        }
    }
    if params.template.is_none() {
        params.template = template;
    }
    params.resume = Some(conversation);
    Ok(params)
}

/// Refuse a start whose model the template or the subject's project forbids
/// (WI-1590, WI-1852; Python `_refuse_forbidden_model`). Judged on the params
/// after the profile has filled them, before anything reaches the engine.
fn refuse_forbidden_model<C, I>(
    ctx: &AppContext<C, I>,
    params: &StartSessionParams,
) -> Result<(), VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let template = params.template.as_deref().map(resolved_template_tag);
    let Some(model) = params.model.as_deref() else {
        return Ok(());
    };
    if template.as_deref() == Some("claude") && !is_claude_model(model) {
        return Err(VogtError::PolicyRefused(format!(
            "template 'claude' runs Claude Code, which does not take model {}; name a claude \
             model (claude-*, opus, sonnet, haiku, fable) or a different template",
            py_repr(model)
        )));
    }
    if let (Some(agent @ ("codex" | "opencode")), true) =
        (template.as_deref(), is_claude_alias(model))
    {
        return Err(VogtError::PolicyRefused(format!(
            "template {} runs {agent}, which does not resolve the Claude Code alias {}; use \
             template 'claude', or name a model that agent takes",
            py_repr(params.template.as_deref().unwrap_or("")),
            py_repr(model)
        )));
    }
    let view = ctx.declared.read()?;
    let project = if let Some(reference) = &params.work_item {
        match resolve::work_item(&view, reference)?.project_id {
            Some(id) => view.project_by_id(&id)?,
            None => None,
        }
    } else if let Some(slug) = &params.project {
        Some(resolve::project(&view, slug)?)
    } else {
        None
    };
    let Some(project) = project else {
        return Ok(());
    };
    let policy = &project.agent_policy;
    let where_ = format!("project {}", py_repr(&project.slug));
    let never = policy
        .review
        .as_ref()
        .and_then(|review| review.get("never_models"))
        .and_then(Value::as_array)
        .is_some_and(|listed| listed.iter().any(|one| one.as_str() == Some(model)));
    if never {
        return Err(VogtError::PolicyRefused(format!(
            "model {} is on {where_}'s review policy never_models, so a session may not start on it",
            py_repr(model)
        )));
    }
    if let Some(matched) = model_matches_forbidden_text(model, &policy.forbidden_text) {
        return Err(VogtError::PolicyRefused(format!(
            "model {} matches {where_}'s forbidden_text pattern {}, so a session may not start on it",
            py_repr(model),
            py_repr(matched)
        )));
    }
    Ok(())
}

/// Python's `_worktree_branch`: `<work item or project>-<session>`, with the
/// head flattened to one path component.
fn worktree_branch(subject: &Subject, session_id: &str) -> String {
    let head = subject
        .work_item_ref
        .as_deref()
        .unwrap_or(subject.project_slug.as_str());
    let mut flat = String::new();
    let mut in_gap = false;
    for character in head.chars() {
        if character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-') {
            flat.push(character);
            in_gap = false;
        } else if !in_gap {
            flat.push('-');
            in_gap = true;
        }
    }
    let flat = flat.trim_matches('-');
    let flat = if flat.is_empty() { "session" } else { flat };
    let tail = session_id.strip_prefix("ses_").unwrap_or(session_id);
    format!("{flat}-{tail}")
}

/// Python's `_worktree_base`: the project's integration branch, else `main`.
fn worktree_base(policy: &crate::core::AgentPolicy) -> String {
    let stated = policy.base_branch.trim();
    if stated.is_empty() {
        "main".to_string()
    } else {
        stated.to_string()
    }
}

// -- the operations -----------------------------------------------------------

/// Open a terminal for a work item or a project.
pub fn start_session<C, I>(
    ctx: &AppContext<C, I>,
    params: &StartSessionParams,
) -> Result<SessionResult, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    start_session_tracked(ctx, params, &mut false)
}

/// `start_session`, reporting through `engine_started` whether the engine
/// accepted the session. An `Err` with `engine_started` set is an unknown
/// outcome: the session may be running (a failed record is followed by a
/// best-effort kill), so a caller that must not start twice cannot retry.
///
/// The phases, in the order their side effects happen: refuse what no caller
/// may ask for (`refuse_unauthorised_start`), settle the profile, model policy
/// and dispatch verdict (`admit_start`), draw ids and mint the session's token
/// (`prepare_start`), ask the engine (`spawn_session`), then record the
/// session, its actor and token, and the audit (`record_started`).
pub(in crate::application::services) fn start_session_tracked<C, I>(
    ctx: &AppContext<C, I>,
    params: &StartSessionParams,
    engine_started: &mut bool,
) -> Result<SessionResult, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let reason = refuse_unauthorised_start(ctx, params)?;
    let engine = engine_of(ctx)?;
    let admitted = admit_start(ctx, engine, params)?;
    let params = &admitted.params;
    backfill_default_branch(ctx, params)?;
    let mut writing = write_of(ctx);
    let prepared = prepare_start(ctx, &writing, &admitted)?;
    let (started, engine_spawn_ms) = spawn_session(engine, &prepared, params)?;
    *engine_started = true;
    let stored = record_started(
        ctx,
        &mut writing,
        &admitted,
        &prepared,
        &started,
        &reason,
        engine_spawn_ms,
    );
    if let Err(error) = stored {
        // The terminal is already running and Vogt holds no record of it.
        // Stop it so a failed write never leaves an orphan process.
        let _ = engine.kill_session(
            &started.id,
            Some(&reason),
            Some(&ctx.principal.identity_ref),
        );
        return Err(error);
    }
    Ok(SessionResult {
        session: summarize(ctx, &stored?, Some(&started))?,
        escalated_to: admitted.escalated,
    })
}

/// A start's cleaned reason, after refusing a `bypass` posture an agent asked
/// for: only a person can grant it.
fn refuse_unauthorised_start<C, I>(
    ctx: &AppContext<C, I>,
    params: &StartSessionParams,
) -> Result<String, VogtError>
where
    C: Clock,
    I: IdFactory,
{
    let reason = writes::validate_reason(&params.reason)?;
    if params.permission_mode == "bypass" && ctx.principal.kind == ActorKind::Agent {
        return Err(VogtError::BypassRefused(format!(
            "permission_mode=bypass can only be granted by a person, not by an agent ({}); \
             start the session from the GUI or the CLI, or use the default posture and report \
             the denied action as blocked",
            ctx.principal.identity_ref
        )));
    }
    Ok(reason)
}

/// What the guards, the profile and the dispatch verdict settled before
/// anything is minted or sent.
struct Admitted {
    /// The start's params after the profile and a resume filled them.
    params: StartSessionParams,
    read_only: bool,
    /// The profile fields the caller set itself.
    overrides: Vec<&'static str>,
    /// A worktree the profile filled, not the caller, is a default: the engine
    /// drops it when the session resolves to a pod, which checks out the
    /// source itself (WI-4707). A caller's own `worktree=true` stays a hard ask.
    worktree_from_profile: bool,
    started_by_session: Option<String>,
    started_by_kind: Option<String>,
    override_reason: Option<String>,
    decision: dispatch_profiles::Decision,
    dispatch_notes: Vec<String>,
    escalated: Option<String>,
}

/// Resolve the caller, the profile and the model policy, and judge the start
/// against the dispatch rules. Refuses before the engine is asked.
fn admit_start<C, I>(
    ctx: &AppContext<C, I>,
    engine: &EngineClient,
    params: &StartSessionParams,
) -> Result<Admitted, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let (started_by_session, started_by_kind) = started_by(ctx, engine)?;
    let override_reason = override_reason(params, started_by_kind.as_deref())?;
    let settings = dispatch_profiles::resolve(&dispatch_preferences(ctx)?);
    let blockers_count = failed_reviews(ctx, params)?;
    // Escalation before the profile fills the start (WI-1329): an item that
    // keeps failing review gets the careful profile's model and budget.
    let escalated_to =
        dispatch_profiles::escalation(effective_purpose(params), blockers_count, &settings);
    let enforcing = ctx.principal.kind == ActorKind::Agent && settings.enforce;
    let mut escalated_params = params.clone();
    if let (Some(purpose), true) = (escalated_to, enforcing) {
        escalated_params.purpose = Some(purpose.to_string());
    }
    // Resolved after the guards above: a profile carries neither a role nor a
    // permission posture, so naming a purpose cannot raise a start past what
    // the caller could ask for directly (WI-1323).
    let (resolved, read_only, overrides) = resolve_purpose(ctx, &escalated_params)?;
    // After the purpose, before the model policy: a resume that names a
    // session brings its template, and the policy judges that template.
    let resolved = resolve_resume(ctx, engine, resolved)?;
    refuse_forbidden_model(ctx, &resolved)?;
    refuse_task_without_agent(&resolved)?;
    let params = &resolved;
    let worktree_from_profile = params.purpose.is_some() && !overrides.contains(&"worktree");
    refuse_privilege_above_caller(ctx, params, started_by_kind.as_deref())?;
    let decision = dispatch_check(
        ctx,
        params,
        &settings,
        started_by_session.as_deref(),
        started_by_kind.as_deref(),
    )?;
    if enforcing && override_reason.is_none() && !decision.violations.is_empty() {
        // Refused before the engine is asked: no terminal is started.
        let message = decision
            .violations
            .iter()
            .map(|violation| violation.message.as_str())
            .collect::<Vec<_>>()
            .join("; ");
        return Err(VogtError::InvalidRequest(format!(
            "dispatch refused this start — {message}"
        )));
    }
    let mut dispatch_notes: Vec<String> = decision
        .violations
        .iter()
        .map(|violation| violation.message.clone())
        .collect();
    if let (Some(purpose), false) = (escalated_to, enforcing) {
        dispatch_notes.push(format!(
            "escalate_after_failed_reviews={}: {blockers_count} failed reviews would run {purpose}",
            settings.escalate_after_failed_reviews
        ));
    }
    let escalated = escalated_to.filter(|_| enforcing).map(str::to_string);
    Ok(Admitted {
        params: resolved,
        read_only,
        overrides,
        worktree_from_profile,
        started_by_session,
        started_by_kind,
        override_reason,
        decision,
        dispatch_notes,
        escalated,
    })
}

/// The ids, subject, token and engine spec a start carries from admission to
/// the engine call and the record.
struct Prepared {
    session_id: String,
    subject: Subject,
    actor_ref: String,
    scopes: Vec<String>,
    token_hash: String,
    budget: Option<SessionBudget>,
    /// What the start asked for (Python `params.worktree`), so a start with no
    /// purpose and no value audits null, not false.
    audit_worktree: Option<bool>,
    audit_repo_map: Option<bool>,
    worktree: bool,
    spec: CreateSession,
}

/// Draw the session id, resolve the subject, mint the session's token and
/// build what the engine is asked to run.
fn prepare_start<C, I>(
    ctx: &AppContext<C, I>,
    writing: &writes::WriteContext<'_, C, I, impl crate::storage::interface::DeclaredStore>,
    admitted: &Admitted,
) -> Result<Prepared, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let params = &admitted.params;
    let session_id = fresh(writing, "ses");
    let subject = subject_of(ctx, params, &session_id)?;
    let actor_ref = format!("agent:session:{session_id}");
    let scopes = session_scopes(ctx)?;
    let (secret, token_hash) = minted_token()?;
    let policy = ctx
        .declared
        .read()?
        .project_by_id(&subject.project_id)?
        .map(|project| project.agent_policy)
        .unwrap_or_default();
    let budget = effective_budget(params, &policy);
    let worktree = params.worktree.unwrap_or(false);
    // A start by an oversight session makes it the supervisor, so the engine
    // can wake it when this session changes (WI-1521). A worker's or a
    // person's start names no supervisor.
    let supervisor = match (
        admitted.started_by_kind.as_deref(),
        admitted.started_by_session.as_deref(),
    ) {
        (Some("oversight"), Some(caller)) => Some(caller.to_string()),
        _ => None,
    };
    let mut spec = build_spec(
        ctx,
        params,
        &subject,
        &secret,
        admitted.read_only,
        supervisor,
    )?;
    // Every agent-made start names its caller to the engine (WI-4489), so
    // the engine can hold the child to its parent's standing on its own.
    spec.started_by = admitted.started_by_session.clone();
    spec.budget = budget.as_ref().map(budget_wire);
    if worktree {
        spec.worktree = Some(json!({
            "branch": worktree_branch(&subject, &session_id),
            "base_ref": worktree_base(&policy),
            "if_supported": admitted.worktree_from_profile,
        }));
    }
    Ok(Prepared {
        session_id,
        subject,
        actor_ref,
        scopes,
        token_hash,
        budget,
        audit_worktree: params.worktree,
        audit_repo_map: params.repo_map,
        worktree,
        spec,
    })
}

/// Ask the engine to start the session, and say how long it took to accept it:
/// on the audit row, and logged, so a slow or refused start is attributable
/// afterwards.
fn spawn_session(
    engine: &EngineClient,
    prepared: &Prepared,
    params: &StartSessionParams,
) -> Result<(EngineSession, i64), VogtError> {
    let session_id = &prepared.session_id;
    let spawn_started = std::time::Instant::now();
    let started = match engine.create_session(&prepared.spec) {
        Ok(started) => started,
        Err(error) => {
            tracing::warn!(
                target: "vogt.sessions",
                "session.start failed session_id={session_id} template={} engine_spawn_ms={} error={}",
                params.template.as_deref().unwrap_or(""),
                spawn_started.elapsed().as_millis(),
                error.code(),
            );
            return Err(error);
        }
    };
    let engine_spawn_ms = spawn_started.elapsed().as_millis() as i64;
    tracing::info!(
        target: "vogt.sessions",
        "session.start session_id={session_id} engine_session_id={} template={} engine_spawn_ms={engine_spawn_ms}",
        started.id,
        params.template.as_deref().unwrap_or(""),
    );
    Ok((started, engine_spawn_ms))
}

/// Record the started session: its actor, token and row, the override audit
/// and the declared branch, in one audited write.
fn record_started<C, I>(
    ctx: &AppContext<C, I>,
    writing: &mut writes::WriteContext<'_, C, I, impl crate::storage::interface::DeclaredStore>,
    admitted: &Admitted,
    prepared: &Prepared,
    started: &EngineSession,
    reason: &str,
    engine_spawn_ms: i64,
) -> Result<CodingSession, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let params = &admitted.params;
    let subject = &prepared.subject;
    // A resumed conversation starts in the directory its transcript records,
    // which may be below or beside the project root. Record where it actually
    // runs; every other session runs at the registry's root.
    let cwd = match (
        params.resume.is_some() || prepared.worktree,
        started.cwd.as_str(),
    ) {
        (true, resumed) if !resumed.is_empty() => resumed.to_string(),
        _ => subject.cwd.clone(),
    };
    let actor_id = fresh(writing, "act");
    let token_id = fresh(writing, "tok");
    // Read after the ranking tick: `why` consumes one, and every timestamp on
    // this path — actor, token, session, overlay — shares the next one.
    let now = now(writing);
    let record = StartRecord {
        actor_id,
        token_id,
        now,
        actor_ref: prepared.actor_ref.clone(),
        scopes: prepared.scopes.clone(),
        token_hash: prepared.token_hash.clone(),
        session_id: prepared.session_id.clone(),
        engine_session_id: started.id.clone(),
        project_id: subject.project_id.clone(),
        work_item_id: subject.work_item_id.clone(),
        template: params.template.clone(),
        model: params.model.clone(),
        effort: params.effort.clone(),
        reason: reason.to_string(),
        work_item_ref: subject.work_item_ref.clone(),
        // Python's `_review_subject` records the resolved item, not the input.
        review_of: params
            .review_of
            .as_ref()
            .and_then(|_| subject.work_item_ref.clone()),
        project_slug: subject.project_slug.clone(),
        is_scratch: subject.is_scratch,
        resume: params.resume.clone(),
        role: params.role.clone(),
        permission_mode: params.permission_mode.clone(),
        autopilot: prepared.spec.autopilot,
        overlay_template: ctx.config.branch_binding_template.clone(),
        purpose: params.purpose.clone(),
        started_by: admitted.started_by_session.clone(),
        started_by_kind: admitted.started_by_kind.clone(),
        audit_overrides: admitted.overrides.clone(),
        audit_budget: prepared.budget.as_ref().map(budget_wire),
        audit_notes: admitted.dispatch_notes.clone(),
        // The override's own record names only the rules broken (Python).
        override_notes: admitted
            .decision
            .violations
            .iter()
            .map(|violation| violation.message.clone())
            .collect(),
        audit_override: admitted.override_reason.clone(),
        audit_escalated: admitted.escalated.clone(),
        override_rules: admitted
            .decision
            .violations
            .iter()
            .map(|violation| violation.rule.name())
            .collect(),
        cwd,
        cold_wake_opt_out: params.cold_wake_opt_out,
        audit_worktree: prepared.audit_worktree,
        audit_repo_map: prepared.audit_repo_map,
        engine_spawn_ms,
    };
    audited_write(writing, SESSION_START, reason, move |txn, caller| {
        write_start(txn, caller, record)
    })
}

/// Everything the start's audited write needs, drawn before it so the write
/// body holds no handle on the request.
struct StartRecord {
    actor_id: String,
    token_id: String,
    now: Moment,
    actor_ref: String,
    scopes: Vec<String>,
    token_hash: String,
    session_id: String,
    engine_session_id: String,
    project_id: String,
    work_item_id: Option<String>,
    template: Option<String>,
    model: Option<String>,
    effort: Option<String>,
    reason: String,
    work_item_ref: Option<String>,
    review_of: Option<String>,
    project_slug: String,
    is_scratch: bool,
    resume: Option<String>,
    role: String,
    permission_mode: String,
    autopilot: bool,
    overlay_template: String,
    purpose: Option<String>,
    started_by: Option<String>,
    started_by_kind: Option<String>,
    audit_overrides: Vec<&'static str>,
    audit_budget: Option<Value>,
    audit_notes: Vec<String>,
    override_notes: Vec<String>,
    audit_override: Option<String>,
    audit_escalated: Option<String>,
    override_rules: Vec<&'static str>,
    cwd: String,
    cold_wake_opt_out: bool,
    audit_worktree: Option<bool>,
    audit_repo_map: Option<bool>,
    engine_spawn_ms: i64,
}

/// The audited write of a started session: actor, token and session rows, the
/// cold-wake opt-out, the override audit and the declared branch, then the
/// `session.started` event.
fn write_start(
    txn: &mut impl WriteTxn,
    caller: &Actor,
    record: StartRecord,
) -> Result<WriteOutcome<CodingSession>, VogtError> {
    let recorded = insert_started_rows(txn, &record)?;
    let payload = audited_payload(&recorded);
    if let Some(why) = &record.audit_override {
        audit_dispatch_override(
            txn,
            caller,
            &recorded.id,
            why,
            record.work_item_ref.as_deref(),
            &record.override_rules,
            &record.override_notes,
            record.now,
        )?;
    }
    // The declared half of the branch binding: a session opened for a
    // work item records the branch it will use. The summary's branch
    // is what was recorded, which is nothing when there is no item.
    let declared_branch = match &record.work_item_ref {
        Some(work_ref) => Some(record_declared_branch(
            txn,
            work_ref,
            &record.project_id,
            &record.overlay_template,
            record.now,
        )?),
        None => None,
    };
    let summary = started_summary(&record, declared_branch);
    Ok(WriteOutcome::new(
        recorded,
        "session",
        &record.session_id,
        payload,
        "session.started",
        summary,
    ))
}

/// Insert the session's actor, token and row, and its cold-wake opt-out.
fn insert_started_rows(
    txn: &mut impl WriteTxn,
    record: &StartRecord,
) -> Result<CodingSession, VogtError> {
    let actor = Actor {
        id: record.actor_id.clone(),
        kind: ActorKind::Agent,
        display_name: format!("Session {}", record.session_id),
        identity_ref: record.actor_ref.clone(),
        disabled: false,
        created_at: record.now,
    };
    let token = Token {
        id: record.token_id.clone(),
        actor_id: actor.id.clone(),
        actor_identity_ref: Some(record.actor_ref.clone()),
        name: format!("session {}", record.session_id),
        scopes: record.scopes.clone(),
        kind: TokenKind::Api,
        created_at: record.now,
        expires_at: None,
        last_used_at: None,
        revoked_at: None,
        revoked_reason: None,
    };
    let recorded = CodingSession {
        id: record.session_id.clone(),
        engine_session_id: record.engine_session_id.clone(),
        project_id: record.project_id.clone(),
        work_item_id: record.work_item_id.clone(),
        actor_id: actor.id.clone(),
        cwd: record.cwd.clone(),
        template: record.template.clone(),
        model: record.model.clone(),
        effort: record.effort.clone(),
        reason: record.reason.clone(),
        purpose: record.purpose.clone(),
        started_by_session: record.started_by.clone(),
        started_by_kind: record.started_by_kind.clone(),
        started_at: record.now,
        stopped_at: None,
    };
    txn.insert_actor(&actor)?;
    txn.insert_token(&token, &record.token_hash)?;
    txn.insert_session(&recorded)?;
    if record.cold_wake_opt_out {
        txn.set_session_cold_wake_opt_out(&record.session_id, true)?;
    }
    Ok(recorded)
}

/// The `session.started` event's summary: where the start's audited facts are
/// recorded.
fn started_summary(record: &StartRecord, declared_branch: Option<String>) -> Value {
    json!({
        "work_item": record.work_item_ref,
        "branch": declared_branch,
        "project": record.project_slug,
        "cwd": record.cwd,
        "scratch": record.is_scratch,
        "model": record.model,
        "effort": record.effort,
        "resume": record.resume,
        "autopilot": record.autopilot,
        "role": record.role,
        "permission_mode": record.permission_mode,
        "purpose": record.purpose,
        "overrides": record.audit_overrides,
        "budget": record.audit_budget,
        "review_of": record.review_of,
        "worktree": record.audit_worktree,
        "repo_map": record.audit_repo_map,
        "started_by_session": record.started_by,
        "started_by_kind": record.started_by_kind,
        // Dispatch (WI-1329): the rules this start broke but
        // was not refused for (a person, enforcement off, or
        // an override), the override's reason, and any
        // escalation.
        "dispatch_notes": record.audit_notes,
        "override_reason": record.audit_override,
        "escalated_to": record.audit_escalated,
        "engine_spawn_ms": record.engine_spawn_ms,
    })
}

/// The cleaned `override` text, refused unless oversight sent it (WI-1329).
///
/// A worker that could override would lift its own cap; a person is never held
/// to the rules, so it has nothing to override.
fn override_reason(
    params: &StartSessionParams,
    started_by_kind: Option<&str>,
) -> Result<Option<String>, VogtError> {
    let Some(text) = params.override_reason.as_deref() else {
        return Ok(None);
    };
    if started_by_kind != Some("oversight") {
        return Err(VogtError::InvalidRequest(format!(
            "override is accepted only from an oversight session (this caller is {}); ask \
             oversight or a person to start this session",
            started_by_kind.unwrap_or("of unknown role")
        )));
    }
    writes::validate_reason(text).map(Some)
}

/// How many `verdict=blockers` comments the start's item has (WI-1319).
fn failed_reviews<C, I>(
    ctx: &AppContext<C, I>,
    params: &StartSessionParams,
) -> Result<i64, VogtError>
where
    C: Clock,
    I: IdFactory,
{
    let Some(reference) = params
        .work_item
        .as_ref()
        .or(params.review_of.as_ref())
        .or(params.from_handoff.as_ref())
    else {
        return Ok(0);
    };
    let view = ctx.declared.read()?;
    let Ok(item) = resolve::work_item(&view, reference) else {
        return Ok(0);
    };
    Ok(view
        .comments_for(&item.id, MAX_COMMENTS_SCANNED, false)?
        .iter()
        .filter(|comment| {
            comment.kind == "verdict"
                && comment
                    .verdict
                    .as_ref()
                    .and_then(|verdict| verdict.get("verdict"))
                    .and_then(Value::as_str)
                    == Some("blockers")
        })
        .count() as i64)
}

const MAX_COMMENTS_SCANNED: i64 = 1000;

/// The dispatch rules this start breaks (WI-1329), judged for any caller.
///
/// Live workers are the caller's started sessions Vogt has not stopped; an
/// agent can start only workers, so every one counts.
fn dispatch_check<C, I>(
    ctx: &AppContext<C, I>,
    params: &StartSessionParams,
    settings: &dispatch_profiles::DispatchSettings,
    started_by_session: Option<&str>,
    started_by_kind: Option<&str>,
) -> Result<dispatch_profiles::Decision, VogtError>
where
    C: Clock,
    I: IdFactory,
{
    let live_workers = match started_by_session {
        Some(caller) => ctx
            .declared
            .read()?
            .list_live_sessions_started_by(caller, settings.max_workers_per_overseer + 1)?
            .len() as i64,
        None => 0,
    };
    Ok(dispatch_profiles::check(
        &dispatch_profiles::StartFacts {
            purpose: params.purpose.as_deref(),
            review: params.purpose.as_deref() == Some("review") || params.review_of.is_some(),
            model: params.model.as_deref(),
            caller_role: started_by_kind,
            live_workers,
        },
        settings,
    ))
}

/// The audit row and event an oversight override leaves (WI-1329).
///
/// Its own operation, so the Inbox and `audit_list` find every override
/// without reading start payloads; the override text is its reason.
#[expect(
    clippy::too_many_arguments,
    reason = "the parameters mirror the call's inputs; a params struct would churn every caller"
)]
fn audit_dispatch_override(
    txn: &mut impl WriteTxn,
    actor: &Actor,
    session_id: &str,
    reason: &str,
    work_item: Option<&str>,
    rules: &[&str],
    notes: &[String],
    at: Moment,
) -> Result<(), VogtError> {
    let payload = json!({
        "session": session_id,
        "work_item": work_item,
        "override_reason": reason,
        "rules": rules,
        "notes": notes,
    });
    let record = txn.append_audit(
        actor,
        DISPATCH_OVERRIDE,
        "session",
        session_id,
        reason,
        &crate::decisions::digest_of(&payload),
        at,
    )?;
    txn.append_event(
        DISPATCH_OVERRIDE,
        "session",
        session_id,
        Some(&actor.id),
        Some(&record.id),
        &payload,
        at,
    )?;
    Ok(())
}

/// A `task` needs an agent to read it (WI-1807). Without a template the
/// engine opens a plain shell, which never reads the brief, so the task would
/// be dropped while the session sits idle. Checked after the purpose profile
/// has filled the start, so a profile that names the template still satisfies it.
fn refuse_task_without_agent(params: &StartSessionParams) -> Result<(), VogtError> {
    let has_task = params
        .task
        .as_deref()
        .is_some_and(|task| !task.trim().is_empty());
    if has_task && params.template.is_none() {
        return Err(VogtError::InvalidRequest(
            "task given without a template: a plain shell never reads the brief, so the task \
             would be dropped; name an agent template (e.g. template='claude') or a purpose \
             that sets one"
                .to_string(),
        ));
    }
    Ok(())
}

/// The purpose a start runs as: the caller's, else `review` when it reviews an
/// item (`review_of`), else none. A named purpose is never replaced.
fn effective_purpose(params: &StartSessionParams) -> Option<&str> {
    match (params.purpose.as_deref(), params.review_of.is_some()) {
        (None, true) => Some("review"),
        (purpose, _) => purpose,
    }
}

/// Python's `_resolve_purpose`: fill a start's unset fields from its purpose's
/// dispatch profile. Returns the filled params, whether the profile asks for a
/// read-only session, and the profile fields the caller set itself (the
/// overrides). A start with no purpose is returned unchanged. The profile is
/// the deployment's, written by a person, over the code defaults.
pub(super) fn resolve_purpose<C, I>(
    ctx: &AppContext<C, I>,
    params: &StartSessionParams,
) -> Result<(StartSessionParams, bool, Vec<&'static str>), VogtError>
where
    C: Clock,
    I: IdFactory,
{
    // A review (`review_of` set) that names no purpose runs as `review`, so it
    // gets the review profile and the purpose the verdict gate reads. A purpose
    // the caller names is never replaced.
    let params = &StartSessionParams {
        purpose: effective_purpose(params).map(str::to_string),
        ..params.clone()
    };
    let Some(purpose) = params.purpose.as_deref() else {
        return Ok((params.clone(), false, Vec::new()));
    };
    if !dispatch_profiles::PURPOSES.contains(&purpose) {
        return Err(VogtError::InvalidRequest(format!(
            "unknown purpose {}; the purposes are {}",
            py_repr(purpose),
            dispatch_profiles::PURPOSES.join(", ")
        )));
    }
    let settings = dispatch_profiles::resolve(&dispatch_preferences(ctx)?);
    let profile = settings.profile(purpose).ok_or_else(|| {
        VogtError::InvalidRequest(format!("no dispatch profile for {}", py_repr(purpose)))
    })?;
    // Sized by the model the start will run, the caller's or the profile's
    // (Python `Profile.turns_for`). A Claude model on another agent's profile
    // is switched to claude below, so the family is read after that decision.
    // A field is the caller's when it carries a value or an explicit null.
    let set = |name: &str, given: bool| given || params.explicit_null.contains(&name);
    let given = [
        params.template.is_some(),
        params.model.is_some(),
        params.effort.is_some(),
        params.budget.is_some(),
        params.worktree.is_some(),
        params.repo_map.is_some(),
        params.autopilot.is_some(),
        params.mcp_set.is_some(),
    ];
    let overrides: Vec<&'static str> = PROFILE_FIELDS
        .into_iter()
        .zip(given)
        .filter_map(|(name, given)| set(name, given).then_some(name))
        .collect();
    let caller = |name: &str| overrides.contains(&name);
    let mut filled = params.clone();
    if !caller("template") {
        filled.template = Some(profile.template.clone());
    }
    // A Claude model on the profile's other agent never runs (WI-1852).
    if caller("model")
        && !caller("template")
        && params.model.as_deref().is_some_and(is_claude_model)
        && profile.template != "claude"
    {
        filled.template = Some("claude".to_string());
    }
    if !caller("model") {
        filled.model = Some(profile.model.clone());
    }
    if !caller("effort") {
        filled.effort = profile.effort.clone();
    }
    if !caller("budget") {
        let turns = profile.turns_for(filled.model.as_deref());
        filled.budget =
            (turns.is_some() || profile.budget_wall_clock_minutes.is_some()).then(|| {
                SessionBudget {
                    turns,
                    wall_clock_minutes: profile.budget_wall_clock_minutes,
                    ..SessionBudget::default()
                }
            });
    }
    if !caller("worktree") {
        filled.worktree = Some(profile.worktree);
    }
    if !caller("repo_map") {
        filled.repo_map = Some(profile.repo_map);
    }
    if !caller("autopilot") {
        filled.autopilot = Some(profile.autopilot);
    }
    if !caller("mcp_set") {
        filled.mcp_set = profile.mcp_set.clone();
    }
    Ok((filled, profile.read_only, overrides))
}

/// The `dispatch.*` values a person last wrote, by key. A key nobody wrote is
/// absent, which the resolver reads as its default.
pub(crate) fn dispatch_preferences<C, I>(
    ctx: &AppContext<C, I>,
) -> Result<std::collections::BTreeMap<String, Value>, VogtError>
where
    C: Clock,
    I: IdFactory,
{
    let view = ctx.declared.read()?;
    let mut stored = std::collections::BTreeMap::new();
    for key in [
        dispatch_profiles::PROFILES_KEY,
        dispatch_profiles::MAX_WORKERS_KEY,
        dispatch_profiles::ESCALATE_KEY,
        dispatch_profiles::REVIEW_BY_OVERSIGHT_KEY,
        dispatch_profiles::BUDGET_RESTARTS_KEY,
        dispatch_profiles::ENFORCE_KEY,
        dispatch_profiles::CONTEXT_BUDGET_KEY,
    ] {
        if let Some(row) = view.latest_human_preference(key)? {
            if row
                .value
                .as_object()
                .is_some_and(|object| !object.is_empty())
            {
                stored.insert(key.to_string(), row.value);
            }
        }
    }
    Ok(stored)
}

/// Python's `_started_by`: who is starting this session, from the caller's
/// verified identity. A person is `person` with no session. A session is named
/// by its engine id. An oversight session is `oversight` by its engine role. A
/// worker's engine role is defaulted to `worker` for any session a person
/// starts without one, so for a worker the kind is its own origin: `worker`
/// only when an agent started it, else none (WI-4471). None too when the engine
/// cannot say. Nothing here is read from the start's parameters.
fn started_by<C, I>(
    ctx: &AppContext<C, I>,
    engine: &EngineClient,
) -> Result<(Option<String>, Option<String>), VogtError>
where
    C: Clock,
    I: IdFactory,
{
    if ctx.principal.kind != ActorKind::Agent {
        return Ok((None, Some("person".to_string())));
    }
    let Some(caller) = session_commit::caller_engine_session_id(ctx)? else {
        return Ok((None, None));
    };
    let live = match engine.get_session(&caller) {
        Ok(live) => live,
        Err(VogtError::EngineUnavailable(_)) => None,
        Err(error) => return Err(error),
    };
    let kind = match live.map(|session| session.role) {
        Some(role) if role == "oversight" => Some("oversight".to_string()),
        Some(role) if role == "worker" => worker_origin(ctx, &caller)?,
        _ => None,
    };
    Ok((Some(caller), kind))
}

/// `worker` when an agent started the caller session, else none (WI-4471).
/// Read from the caller's own Vogt row: a session an oversight session or a
/// worker started is a worker's child. A person's session, or one Vogt did not
/// start, has no agent origin to hand on.
fn worker_origin<C, I>(ctx: &AppContext<C, I>, caller: &str) -> Result<Option<String>, VogtError>
where
    C: Clock,
    I: IdFactory,
{
    let row = ctx.declared.read()?.session_by_engine_id(caller)?;
    Ok(match row {
        Some(session)
            if matches!(
                session.started_by_kind.as_deref(),
                Some("oversight") | Some("worker")
            ) =>
        {
            Some("worker".to_string())
        }
        _ => None,
    })
}

/// Refuse a start that would give the new session more standing than its
/// caller (WI-4489). Only a person, or an oversight session, may start an
/// oversight session or one on the full MCP set; any other agent caller (a
/// worker, a worker's child, or a token with no live session) is refused.
/// Checked after the purpose fills the start, so a profile cannot carry a
/// start past it either.
fn refuse_privilege_above_caller<C, I>(
    ctx: &AppContext<C, I>,
    params: &StartSessionParams,
    started_by_kind: Option<&str>,
) -> Result<(), VogtError>
where
    C: Clock,
    I: IdFactory,
{
    if ctx.principal.kind != ActorKind::Agent || started_by_kind == Some("oversight") {
        return Ok(());
    }
    let asked = if params.role == "oversight" {
        "role=oversight"
    } else if params.mcp_set.as_deref() == Some("full") {
        "mcp_set=full"
    } else {
        return Ok(());
    };
    Err(VogtError::InvalidRequest(format!(
        "{asked} can only be started by a person or an oversight session, not by {} ({}); \
         a child session never gets more standing than the session that starts it; \
         start it on the worker set, or ask a person to start it",
        ctx.principal.identity_ref,
        started_by_kind.unwrap_or("an agent with no oversight role")
    )))
}

/// What the engine was asked to run.
fn build_spec<C, I>(
    ctx: &AppContext<C, I>,
    params: &StartSessionParams,
    subject: &Subject,
    token: &str,
    read_only: bool,
    supervisor: Option<String>,
) -> Result<CreateSession, VogtError>
where
    C: Clock,
    I: IdFactory,
{
    let autopilot = params.autopilot.unwrap_or_else(|| {
        params.template.is_some()
            && params
                .task
                .as_deref()
                .is_some_and(|task| !task.trim().is_empty())
    });
    let mut spec = CreateSession::new(subject.name.clone(), subject.cwd.clone());
    spec.template = params.template.clone();
    spec.placement = params.placement.clone();
    spec.model = params.model.clone();
    spec.effort = params.effort.clone();
    spec.resume = params.resume.clone();
    spec.permission_mode = Some(params.permission_mode.clone());
    spec.role = params.role.clone();
    spec.work_item = subject.work_item_ref.clone();
    spec.session_ref = Some(subject.session_id.clone());
    spec.supervisor = supervisor;
    spec.wake_on = params.wake_on.clone();
    spec.source = subject.source.clone();
    spec.autopilot = autopilot;
    spec.mcp_set = params.mcp_set.clone();
    spec.prompt = Some(brief_with_task(
        &subject.brief.text,
        params.task.as_deref(),
        autopilot,
        read_only,
        params.review_of.is_some(),
    ));
    spec.companion = (!subject.brief.companion.is_empty()).then(|| subject.brief.companion.clone());
    spec.env = Some(session_env(
        ctx,
        &subject.session_id,
        token,
        subject.work_item_ref.as_deref(),
    ));
    Ok(spec)
}

/// The brief plus the autopilot and read-only notes and the task, matching
/// Python's `_brief_with_task`. A review's task is recorded as what the review
/// was asked to look at, not as new work. Touch points (files, symbols, a repo
/// map drawn from them) are not carried by the Rust start parameters yet.
fn brief_with_task(
    brief: &str,
    task: Option<&str>,
    autopilot: bool,
    read_only: bool,
    review: bool,
) -> String {
    let mut text = brief.to_string();
    if autopilot {
        text = format!("{}\n\n{}", text.trim_end(), brief::AUTOPILOT);
    }
    if read_only {
        text = format!("{}\n\n{}", text.trim_end(), brief::READ_ONLY);
    }
    let task = task.unwrap_or("").trim();
    if !task.is_empty() {
        let heading = if review { "## Also asked" } else { "## Task" };
        text = format!("{}\n\n{heading}\n\n{task}\n", text.trim_end());
    }
    text
}

/// Add the branch a session will use to the item's overlay, idempotently.
fn record_declared_branch(
    txn: &mut impl crate::storage::interface::WriteTxn,
    work_ref: &str,
    project_id: &str,
    template: &str,
    at: crate::core::Moment,
) -> Result<String, VogtError> {
    let branch = crate::branches::default_branch_name(work_ref, template);
    let existing = txn.work_overlay(work_ref)?;
    let mut branches = existing
        .as_ref()
        .map(|row| row.branches.clone())
        .unwrap_or_default();
    if branches.iter().any(|have| have == &branch) {
        return Ok(branch);
    }
    branches.push(branch.clone());
    let overlay = match existing {
        Some(mut row) => {
            row.branches = branches;
            row.updated_at = at;
            row
        }
        None => crate::core::WorkOverlay {
            subject_key: work_ref.to_string(),
            project_id: project_id.to_string(),
            rank: None,
            workflow_state: None,
            priority: None,
            effort: None,
            assignee_actor_id: None,
            initiative_id: None,
            branches,
            created_at: at,
            updated_at: at,
        },
    };
    txn.upsert_work_overlay(&overlay)?;
    Ok(branch)
}
