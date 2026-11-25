use super::*;

/// Stop a session and revoke the token it ran with.
pub fn stop_session<C, I>(
    ctx: &AppContext<C, I>,
    params: &StopSessionParams,
) -> Result<SessionResult, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let reason = writes::validate_reason(&params.reason)?;
    let found = resolve_target(ctx, &params.id)?;
    let Some(session) = found.session else {
        let engine = engine_of(ctx)?;
        return stop_unlinked(ctx, engine, &found.engine_session_id, &reason);
    };
    // The kill goes first and its failure is not fatal: an engine that has
    // already forgotten the session, or one that is down, must not leave Vogt
    // unable to close its own record. Only an unconfigured engine yields no
    // answer; any other failure is the caller's to see.
    let engine_killed = match ctx.engine.as_ref() {
        None => None,
        Some(engine) => match engine.kill_session(
            &session.engine_session_id,
            Some(&reason),
            Some(&ctx.principal.identity_ref),
        ) {
            Ok(killed) => Some(killed),
            // An engine that is down answers nothing; any other failure is
            // the caller's to see.
            Err(VogtError::EngineUnavailable(_)) => None,
            Err(other) => return Err(other),
        },
    };
    let mut writing = write_of(ctx);
    let stopped_at = now(&writing);
    let actor_id = session.actor_id.clone();
    let session_id = session.id.clone();
    let named = params.id.clone();
    let stop_reason = reason.clone();
    audited_write(&mut writing, SESSION_STOP, &reason, move |txn, _actor| {
        let current = txn
            .session_by_id(&session_id)?
            .ok_or_else(|| VogtError::NotFound(format!("no session {}", py_repr(&named))))?;
        if current.stopped_at.is_some() {
            return Err(VogtError::Conflict(format!(
                "session {} was already stopped",
                py_repr(&named)
            )));
        }
        txn.mark_session_stopped(&session_id, stopped_at)?;
        for token in txn.tokens_for_actor(&actor_id, false)? {
            if token.revoked_at.is_none() {
                txn.revoke_token(&token.id, &stop_reason, stopped_at)?;
            }
        }
        let mut stopped = current;
        stopped.stopped_at = Some(stopped_at);
        Ok(WriteOutcome::new(
            session_id.clone(),
            "session",
            &session_id,
            audited_payload(&stopped),
            "session.stopped",
            json!({"engine_killed": engine_killed}),
        ))
    })?;
    let stopped = ctx
        .declared
        .read()?
        .session_by_id(&session.id)?
        .unwrap_or(session);
    Ok(SessionResult {
        session: summarize(ctx, &stopped, None)?,
        escalated_to: None,
    })
}

/// Wake a hibernated session. A live session is returned as it is — waking is
/// idempotent, so a caller that is not sure need not check first.
pub fn wake<C, I>(ctx: &AppContext<C, I>, params: &WakeParams) -> Result<SessionResult, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let reason = writes::validate_reason(&params.reason)?;
    let engine = engine_of(ctx)?;
    if params.force && !may_force_wake(ctx, engine)? {
        return Err(VogtError::RoleRefused(
            "only a person or an oversight session may force a wake".to_string(),
        ));
    }
    let found = resolve_target(ctx, &params.id)?;
    let result = wake_resolved(ctx, engine, &found, &reason, params.force)?;
    // Woken on purpose: whoever woke it is acting on it, so the report it slept
    // with no longer stands (WI-1834). A session that is still blocked reports
    // again.
    let current = engine.get_session(&found.engine_session_id).ok().flatten();
    if current
        .and_then(|current| clear_block_on(ctx, engine, &found, "wake", &reason, &current))
        .is_some()
    {
        return wake_resolved(ctx, engine, &found, &reason, params.force);
    }
    Ok(result)
}

/// `force` overrides the handoff guard, so only a person, or a session the
/// engine has marked `oversight`, may use it.
fn may_force_wake<C, I>(ctx: &AppContext<C, I>, engine: &EngineClient) -> Result<bool, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    if answers_as_person(ctx)? {
        return Ok(true);
    }
    Ok(relayed_overseer(ctx, engine, false)?.is_some())
}

/// Keep a session awake, or let it hibernate again.
pub fn keep_awake<C, I>(
    ctx: &AppContext<C, I>,
    params: &KeepAwakeParams,
) -> Result<SessionResult, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let reason = writes::validate_reason(&params.reason)?;
    let engine = engine_of(ctx)?;
    let engine_id = resolve_target(ctx, &params.id)?.engine_session_id;
    let updated = engine
        .keep_awake(&engine_id, params.keep_awake)?
        .ok_or_else(|| {
            VogtError::NotFound(format!(
                "the engine has no session {}, or predates hibernation",
                py_repr(&params.id)
            ))
        })?;
    let recorded = resolve_target(ctx, &params.id)?;
    let why = recorded
        .session
        .as_ref()
        .map(|session| session.id.clone())
        .unwrap_or_else(|| engine_id.clone());
    let mut writing = write_of(ctx);
    let detail = json!({
        "engine_session_id": engine_id.clone(),
        "linked": recorded.session.is_some(),
        "keep_awake": params.keep_awake,
    });
    audited_action(
        &mut writing,
        SESSION_KEEP_AWAKE,
        &reason,
        "session",
        &why,
        &detail,
        "session.keep_awake",
        None,
    )?;
    let summary = match &recorded.session {
        Some(session) => summarize(ctx, session, Some(&updated))?,
        None => unlinked_summary(&ctx.declared.read()?, &updated)?,
    };
    Ok(SessionResult {
        session: summary,
        escalated_to: None,
    })
}

/// Change a session's role. Nominating oversight is a person's act.
pub fn set_role<C, I>(
    ctx: &AppContext<C, I>,
    params: &SetRoleParams,
) -> Result<SessionResult, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let reason = writes::validate_reason(&params.reason)?;
    // The role is a person's nomination. An agent that could nominate itself
    // would hand itself an overseer's standing, and a worker demoting its
    // overseer is no better. Refused by who is asking, like `bypass`.
    if ctx.principal.kind == ActorKind::Agent {
        return Err(VogtError::RoleRefused(format!(
            "only a person nominates an oversight session, or makes one a worker              again ({} is an agent); ask a person to set the role from the GUI,              or start the session with role=oversight",
            ctx.principal.identity_ref
        )));
    }
    let engine = engine_of(ctx)?;
    let engine_id = resolve_target(ctx, &params.id)?.engine_session_id;
    let updated = engine.set_role(&engine_id, &params.role)?.ok_or_else(|| {
        VogtError::NotFound(format!(
            "the engine has no session {}, or predates session roles",
            py_repr(&params.id)
        ))
    })?;
    let recorded = resolve_target(ctx, &params.id)?;
    let why = recorded
        .session
        .as_ref()
        .map(|session| session.id.clone())
        .unwrap_or_else(|| engine_id.clone());
    let mut writing = write_of(ctx);
    let detail = json!({
        "engine_session_id": engine_id.clone(),
        "linked": recorded.session.is_some(),
        "role": params.role,
        "keep_awake": updated.keep_awake,
    });
    audited_action(
        &mut writing,
        SESSION_SET_ROLE,
        &reason,
        "session",
        &why,
        &detail,
        "session.role_set",
        None,
    )?;
    let summary = match &recorded.session {
        Some(session) => summarize(ctx, session, Some(&updated))?,
        None => unlinked_summary(&ctx.declared.read()?, &updated)?,
    };
    Ok(SessionResult {
        session: summary,
        escalated_to: None,
    })
}

/// Hibernate a session to free its memory, keeping it listed. The session's
/// token is revoked: nothing runs to hold it, and a wake mints a new one. A
/// stopped session has nothing to hibernate.
pub fn hibernate<C, I>(
    ctx: &AppContext<C, I>,
    params: &HibernateParams,
) -> Result<SessionResult, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let reason = writes::validate_reason(&params.reason)?;
    let engine = engine_of(ctx)?;
    let found = resolve_target(ctx, &params.id)?;
    let engine_id = found.engine_session_id.clone();
    let successor = match params.superseded_by.as_deref() {
        Some(text) if !text.is_empty() => Some(resolve_target(ctx, text)?.engine_session_id),
        _ => None,
    };
    if found
        .session
        .as_ref()
        .is_some_and(|session| session.stopped_at.is_some())
    {
        return Err(VogtError::Conflict(format!(
            "session {} was stopped; there is nothing to hibernate",
            py_repr(&params.id)
        )));
    }
    let hibernated = engine
        .hibernate_session(&engine_id, Some(&reason), params.allow_shell, successor.as_deref())?
        .ok_or_else(|| {
            VogtError::NotFound(format!(
                "the engine has no session {}, or predates hibernation (no POST /api/sessions/{{id}}/hibernate)",
                py_repr(&params.id)
            ))
        })?;
    let resumable = hibernated
        .hibernation
        .as_ref()
        .map(|hibernation| hibernation.resumable);
    let outcome = json!({
        "engine_session_id": engine_id,
        "linked": found.session.is_some(),
        "allow_shell": params.allow_shell,
        "resumable": resumable,
        "cold_wake_opt_out": params.cold_wake_opt_out,
    });
    let Some(session) = found.session.clone() else {
        let mut writing = write_of(ctx);
        audited_action(
            &mut writing,
            SESSION_HIBERNATE,
            &reason,
            "session",
            &engine_id,
            &outcome,
            "session.hibernated",
            Some(&outcome),
        )?;
        return Ok(SessionResult {
            session: unlinked_summary(&ctx.declared.read()?, &hibernated)?,
            escalated_to: None,
        });
    };
    let mut writing = write_of(ctx);
    let actor_id = session.actor_id.clone();
    let session_id = session.id.clone();
    let why = reason.clone();
    let recorded = outcome.clone();
    let opt_out = params.cold_wake_opt_out;
    let revoked_at = now(&writing);
    audited_write(
        &mut writing,
        SESSION_HIBERNATE,
        &reason,
        move |txn, _actor| {
            let current = txn.session_by_id(&session_id)?.ok_or_else(|| {
                VogtError::NotFound(format!("no session {}", py_repr(&session_id)))
            })?;
            let mut revoked = 0;
            for token in txn.tokens_for_actor(&actor_id, false)? {
                if txn.revoke_token(&token.id, &format!("hibernated: {why}"), revoked_at)? {
                    revoked += 1;
                }
            }
            if let Some(opt_out) = opt_out {
                if opt_out != txn.session_cold_wake_opt_out(&current.id)? {
                    txn.set_session_cold_wake_opt_out(&current.id, opt_out)?;
                }
            }
            let mut summary = recorded.clone();
            summary["tokens_revoked"] = json!(revoked);
            Ok(WriteOutcome::new(
                (),
                "session",
                &current.id,
                audited_payload(&current),
                "session.hibernated",
                summary,
            ))
        },
    )?;
    Ok(SessionResult {
        session: summarize(ctx, &session, Some(&hibernated))?,
        escalated_to: None,
    })
}

/// Stop a session Vogt never recorded.
fn stop_unlinked<C, I>(
    ctx: &AppContext<C, I>,
    engine: &EngineClient,
    engine_id: &str,
    reason: &str,
) -> Result<SessionResult, VogtError>
where
    C: Clock,
    I: IdFactory,
{
    // Read first: the kill removes the session, and the summary is built from
    // what the engine said it was, with the stop applied on top.
    let session = engine
        .get_session(engine_id)?
        .ok_or_else(|| VogtError::NotFound(format!("no session {}", py_repr(engine_id))))?;
    let killed = engine.kill_session(engine_id, Some(reason), Some(&ctx.principal.identity_ref))?;
    // The audit records its own moment, so the summary's stopped_at is the read
    // after it, one tick later than the moment the stop was asked.
    let outcome = json!({"engine_session_id": engine_id, "linked": false});
    let summary_row = json!({"engine_killed": killed, "linked": false});
    let mut writing = write_of(ctx);
    audited_action(
        &mut writing,
        SESSION_STOP,
        reason,
        "session",
        engine_id,
        &outcome,
        "session.stopped",
        Some(&summary_row),
    )?;
    let stopped_at = now(&writing);
    let mut summary = unlinked_summary(&ctx.declared.read()?, &session)?;
    summary.alive = killed.then_some(false);
    summary.stopped_at = Some(stopped_at);
    Ok(SessionResult {
        session: summary,
        escalated_to: None,
    })
}

/// Wake `target` if it is hibernated; its summary either way.
///
/// For a session Vogt started, a new token is minted for the session's own
/// actor and handed to the woken process, and every older token of that actor
/// is revoked in the same write. The process starts before the declared write.
pub(super) fn wake_resolved<C, I>(
    ctx: &AppContext<C, I>,
    engine: &EngineClient,
    target: &Target,
    reason: &str,
    force: bool,
) -> Result<SessionResult, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    match wake_inner(ctx, engine, target, reason, force) {
        Err(VogtError::Conflict(message)) => {
            Err(VogtError::Conflict(in_session_terms(ctx, &message)))
        }
        other => other,
    }
}

/// Say a refused wake in ses_… terms when the successor is a linked session.
/// An unlinked successor stays a uuid, and so does any other refusal.
fn in_session_terms<C, I>(ctx: &AppContext<C, I>, message: &str) -> String
where
    C: Clock,
    I: IdFactory,
{
    static SUCCESSOR: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let pattern = SUCCESSOR.get_or_init(|| {
        regex::Regex::new(r"session [0-9a-fA-F-]{36} was superseded by (?P<uuid>[0-9a-fA-F-]{36})")
            .expect("the successor pattern compiles")
    });
    let Some(uuid) = pattern
        .captures(message)
        .and_then(|found| found.name("uuid"))
        .map(|found| found.as_str().to_string())
    else {
        return message.to_string();
    };
    let named = ctx
        .declared
        .read()
        .ok()
        .and_then(|view| view.session_by_engine_id(&uuid).ok().flatten())
        .map(|row| row.id);
    match named {
        Some(named) if named != uuid => message.replacen(&uuid, &named, 1),
        _ => message.to_string(),
    }
}

/// Whether this wake compacts (WI-2037), and the summary it starts from. The
/// clock is read once here, as Python's `_cold_wake` reads it, whether or not
/// the feature is on. A summary that cannot be read falls back to a plain
/// resume: compaction is a saving, never a gate.
fn cold_wake_of<C, I>(
    ctx: &AppContext<C, I>,
    current: &EngineSession,
    session: Option<&CodingSession>,
) -> Result<(cold_wake::Decision, Option<String>), VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let config = &ctx.config;
    let tokens = current
        .economics
        .as_ref()
        .and_then(|economics| economics.get("context_tokens"))
        .and_then(Value::as_i64)
        .filter(|tokens| *tokens > 0);
    let opted_out = match session {
        Some(session) => ctx
            .declared
            .read()?
            .session_cold_wake_opt_out(&session.id)?,
        None => false,
    };
    let hibernated_at = current.hibernation.as_ref().and_then(|hibernation| {
        crate::application::session_changes::engine_moment(&hibernation.at)
    });
    let moment = now(&write_of(ctx));
    let decision = cold_wake::decide(&cold_wake::Inputs {
        enabled: config.cold_wake_compaction,
        opted_out,
        agent: current.conversation_agent.as_deref(),
        model: session.and_then(|session| session.model.as_deref()),
        hibernated_at,
        now: moment,
        context_tokens: tokens,
        ttl_seconds: &config.cold_wake_ttl_seconds,
        threshold_tokens: config.cold_wake_threshold_tokens,
    });
    if !decision.compact() {
        return Ok((decision, None));
    }
    let reply = crate::application::session_reads::last_reply_in(ctx, &current.id, 1)
        .ok()
        .and_then(|body| {
            body.get("messages")
                .and_then(Value::as_array)
                .and_then(|messages| messages.last().cloned())
        })
        .and_then(|message| {
            message
                .get("text")
                .and_then(Value::as_str)
                .map(str::to_string)
        });
    let (work_item, handoff) = match session.and_then(|session| session.work_item_id.as_deref()) {
        Some(item_id) => {
            let view = ctx.declared.read()?;
            let item = view.work_item_by_id(item_id)?;
            let comments = view.comments_for(item_id, 20, true)?;
            (
                item,
                comments
                    .into_iter()
                    .find(|comment| comment.kind == "handoff"),
            )
        }
        None => (None, None),
    };
    let summary = cold_wake::render_summary(&cold_wake::SummaryInputs {
        session_id: session.map_or(current.id.as_str(), |session| session.id.as_str()),
        work_item: work_item.as_ref().map(|item| item.reference.as_str()),
        work_item_title: work_item.as_ref().map(|item| item.title.as_str()),
        reason: session.map(|session| session.reason.as_str()),
        last_reply: reply.as_deref(),
        handoff: handoff.as_ref(),
        decision: &decision,
    });
    Ok((decision, Some(summary)))
}

/// A storage read error while deciding on compaction is logged and the wake
/// resumes the transcript: compaction is a saving, never a gate.
pub(super) fn plain_resume_on_read_error(
    result: Result<(cold_wake::Decision, Option<String>), VogtError>,
    engine_session: &str,
) -> (cold_wake::Decision, Option<String>) {
    result.unwrap_or_else(|error| {
        tracing::warn!(
            session = %engine_session, error = %error,
            "could not read the cold-wake inputs; resuming the transcript"
        );
        (cold_wake::Decision::read_failed(), None)
    })
}

fn wake_inner<C, I>(
    ctx: &AppContext<C, I>,
    engine: &EngineClient,
    target: &Target,
    reason: &str,
    force: bool,
) -> Result<SessionResult, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let engine_id = &target.engine_session_id;
    let current = engine.get_session(engine_id)?.ok_or_else(|| {
        VogtError::NotFound(format!("the engine has no session {}", py_repr(engine_id)))
    })?;
    let session = target.session.clone();
    if !current.hibernated() {
        if !current.alive {
            return Err(VogtError::Conflict(format!(
                "session {} has exited, not hibernated; start a new one (session_start, with resume to continue its conversation)",
                py_repr(engine_id)
            )));
        }
        let summary = match &session {
            Some(session) => summarize(ctx, session, Some(&current))?,
            None => unlinked_summary(&ctx.declared.read()?, &current)?,
        };
        return Ok(SessionResult {
            session: summary,
            escalated_to: None,
        });
    }
    let (decision, fresh_prompt) =
        plain_resume_on_read_error(cold_wake_of(ctx, &current, session.as_ref()), &current.id);
    let Some(session) = session else {
        let woken = engine
            .wake_session(engine_id, None, force, fresh_prompt.as_deref())?
            .ok_or_else(|| {
                VogtError::NotFound(format!("the engine has no session {}", py_repr(engine_id)))
            })?;
        let detail = json!({
            "engine_session_id": engine_id,
            "linked": false,
            "forced": force,
            "cold_wake": decision.audit(),
        });
        let mut writing = write_of(ctx);
        audited_action(
            &mut writing,
            SESSION_WAKE,
            reason,
            "session",
            engine_id,
            &detail,
            "session.woken",
            Some(&detail),
        )?;
        return Ok(SessionResult {
            session: unlinked_summary(&ctx.declared.read()?, &woken)?,
            escalated_to: None,
        });
    };
    if session.stopped_at.is_some() {
        return Err(VogtError::Conflict(format!(
            "session {} was stopped; start a new one instead",
            py_repr(&session.id)
        )));
    }
    let scopes = session_scopes(ctx)?;
    let (secret, token_hash) = minted_token()?;
    let env = session_env(ctx, &session.id, &secret, None);
    let woken = engine
        .wake_session(engine_id, Some(&env), force, fresh_prompt.as_deref())?
        .ok_or_else(|| {
            VogtError::NotFound(format!("the engine has no session {}", py_repr(engine_id)))
        })?;
    let conversation_id = current.conversation_id.clone();
    let cold_audit = decision.audit();
    let engine_session_id = engine_id.clone();
    let session_id = session.id.clone();
    let why = reason.to_string();
    let mut writing = write_of(ctx);
    let token_id = fresh(&writing, "tok");
    let minted_at = now(&writing);
    audited_write(&mut writing, SESSION_WAKE, reason, move |txn, _actor| {
        let row = txn.session_by_id(&session_id)?;
        let holder = row
            .as_ref()
            .and_then(|row| txn.actor_by_id(&row.actor_id).ok().flatten());
        let (Some(row), Some(holder)) = (row, holder) else {
            return Err(VogtError::NotFound(format!(
                "no session {}",
                py_repr(&session_id)
            )));
        };
        let mut revoked = 0;
        for token in txn.tokens_for_actor(&row.actor_id, false)? {
            if txn.revoke_token(&token.id, &format!("superseded on wake: {why}"), minted_at)? {
                revoked += 1;
            }
        }
        txn.insert_token(
            &Token {
                id: token_id,
                actor_id: holder.id.clone(),
                actor_identity_ref: Some(holder.identity_ref.clone()),
                name: format!("session {}", row.id),
                scopes,
                kind: TokenKind::Api,
                created_at: minted_at,
                expires_at: None,
                last_used_at: None,
                revoked_at: None,
                revoked_reason: None,
            },
            &token_hash,
        )?;
        Ok(WriteOutcome::new(
            (),
            "session",
            &row.id,
            audited_payload(&row),
            "session.woken",
            json!({
                "engine_session_id": engine_session_id,
                "conversation_id": conversation_id,
                "tokens_revoked": revoked,
                "forced": force,
                "cold_wake": cold_audit,
            }),
        ))
    })?;
    Ok(SessionResult {
        session: summarize(ctx, &session, Some(&woken))?,
        escalated_to: None,
    })
}
