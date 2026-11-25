use super::*;

/// Answer the dialog a session shows — a permission request or a startup gate —
/// by option, not by keystrokes. Exactly one of `option` or `label`. Whether the
/// caller counts as a person is the engine's decision: the flag is sent, and a
/// permission prompt an agent answers is refused there.
pub fn answer<C, I>(ctx: &AppContext<C, I>, params: &AnswerParams) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let reason = writes::validate_reason(&params.reason)?;
    let label = params
        .label
        .as_deref()
        .map(str::trim)
        .filter(|label| !label.is_empty());
    if params.option.is_some() == label.is_some() {
        return Err(VogtError::InvalidRequest(
            "give exactly one of option (a number) or label".to_string(),
        ));
    }
    let engine = engine_of(ctx)?;
    let found = resolve_target(ctx, &params.id)?;
    let engine_id = found.engine_session_id.clone();
    let person = answers_as_person(ctx)?;
    // A person answers anything; an oversight session is relayed by its
    // engine id and may answer a permission prompt (WI-2028).
    let overseer = relayed_overseer(ctx, engine, person)?;
    let answered = match engine.answer_session(
        &engine_id,
        params.option,
        label,
        params.expect_question.as_deref(),
        person,
        overseer.as_deref(),
    ) {
        // The engine refuses an agent's answer to a permission prompt: the
        // prompt is a person's, so the refusal comes back as an escalation.
        Err(VogtError::PersonRequired(refused)) => {
            return Err(escalated(
                ctx,
                engine,
                &engine_id,
                &refused,
                (params.option, label),
                &reason,
            )?)
        }
        answered => answered?,
    }
    .ok_or_else(|| {
            VogtError::NotFound(format!(
                "the engine has no session {}, or predates answering (no POST /api/sessions/{{id}}/answer)",
                py_repr(&params.id)
            ))
        })?;
    let chosen = answered.get("chosen").cloned().unwrap_or(Value::Null);
    let number = chosen.get("number").and_then(Value::as_i64).unwrap_or(0);
    let chosen_label = chosen
        .get("label")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let question = answered
        .get("question")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let kind = answered
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("permission")
        .to_string();
    let dismissed = answered.get("dismissed") == Some(&Value::Bool(true));
    let entity = found
        .session
        .as_ref()
        .map(|session| session.id.clone())
        .unwrap_or(engine_id.clone());
    let detail = json!({
        "engine_session_id": engine_id,
        "linked": found.session.is_some(),
        "question": question.chars().take(300).collect::<String>(),
        "kind": kind,
        "option": number,
        "label": chosen_label.chars().take(200).collect::<String>(),
        "dismissed": dismissed,
        "person": person,
        "oversight": overseer.is_some(),
    });
    let mut writing = write_of(ctx);
    audited_action(
        &mut writing,
        SESSION_ANSWER,
        &reason,
        "session",
        &entity,
        &detail,
        "session.answered",
        Some(&detail),
    )?;
    Ok(json!({
        "id": params.id,
        "engine_session_id": engine_id,
        "question": question,
        "kind": kind,
        "chosen": {"number": number, "label": chosen_label, "selected": true},
        "dismissed": dismissed,
    }))
}

/// The engine's refusal of an agent's answer to a permission prompt (WI-983),
/// handed back as an escalation (WI-996): the prompt already stands in the
/// Inbox for a person, so the refusal names that entry and the dialog's
/// content key, with `outcome: escalated`. The caller waits on it or reports
/// blocked; it does not retry. The agent's pick is recorded under the dialog's
/// key as a recommendation (E0b): shown beside the person's buttons, never
/// chosen for them.
///
/// Built from the engine's read of the session. When that read fails the
/// refusal still carries the outcome; only the keys are missing.
fn escalated<C, I>(
    ctx: &AppContext<C, I>,
    engine: &EngineClient,
    engine_id: &str,
    refused: &str,
    pick: (Option<i64>, Option<&str>),
    reason: &str,
) -> Result<VogtError, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let mut details = json!({
        "outcome": "escalated",
        "needs": "person",
        "boundary": "permission_prompt",
    });
    // A failed read still leaves the refusal an escalation: only the keys are
    // missing (the refusal's own outcome is what the caller acts on).
    let seen = engine.get_session(engine_id).ok().flatten();
    let mut inbox_key = None;
    let mut recommended = false;
    if let Some(seen) = seen.as_ref() {
        details["session_id"] = json!(seen.id);
        let key = match seen.approval.as_ref() {
            None => session_blocked::approval_key(&seen.id, ""),
            Some(approval) => {
                session_blocked::dialog_key(&seen.id, &approval.key, &approval.question)
            }
        };
        details["escalation_key"] = json!(key);
        if seen.activity == "awaiting-approval" {
            let inbox = format!(
                "agent:session:{}:awaiting-approval:{}",
                seen.id,
                seen.activity_changed_at.as_deref().unwrap_or("unknown")
            );
            details["inbox_entry_key"] = json!(inbox);
            inbox_key = Some(inbox);
        }
        if let Some(approval) = seen.approval.as_ref() {
            recommended =
                session_blocked::record_recommendation(ctx, &key, approval, pick, reason)?;
            if recommended {
                details["recommendation"] = json!("recorded");
            }
        }
    }
    let mut message = format!("{refused} The prompt is escalated to a person in the Inbox");
    if let Some(inbox) = &inbox_key {
        message.push_str(&format!(" ({inbox})"));
    }
    message.push_str("; wait for it, do not retry.");
    if recommended {
        message.push_str(" Your pick is shown to them as a recommendation.");
    }
    Ok(VogtError::Escalated { message, details })
}

/// Whose notes a worker brief carries (WI-1320): a person's, or oversight's.
///
/// A session author counts when the engine says its role is `oversight` now.
/// A session that no longer exists, or an engine that cannot say (unreachable,
/// refused, or rejecting the lookup), counts as a worker and its notes are
/// dropped: the brief fails closed and the start goes ahead. One engine read
/// per author; once the engine is unreachable, no later author is asked for.
pub(super) fn decision_author<'a, C, I>(
    ctx: &'a AppContext<C, I>,
    view: &'a impl ReadView,
) -> impl Fn(&crate::core::Comment) -> bool + 'a
where
    C: Clock,
    I: IdFactory,
{
    let roles: std::cell::RefCell<std::collections::HashMap<String, bool>> = Default::default();
    let engine_down = std::cell::Cell::new(false);
    let oversight = move |actor_id: &str| -> bool {
        let Ok(Some(actor)) = view.actor_by_id(actor_id) else {
            return false;
        };
        let Some(engine) = ctx.engine.as_ref() else {
            return false;
        };
        if engine_down.get() {
            return false;
        }
        let reference = actor.identity_ref;
        let engine_id = if let Some(id) = reference.strip_prefix("agent:session:") {
            match view.session_by_id(id) {
                Ok(Some(row)) => row.engine_session_id,
                _ => return false,
            }
        } else if let Some(id) = reference.strip_prefix(ENGINE_SESSION_ACTOR_PREFIX) {
            id.to_string()
        } else {
            return false;
        };
        if engine_id.is_empty() {
            return false;
        }
        match engine.get_session(&engine_id) {
            Ok(live) => live.is_some_and(|session| session.role == "oversight"),
            Err(VogtError::EngineUnavailable(error)) => {
                engine_down.set(true);
                tracing::warn!(
                    target: "vogt.sessions",
                    "brief decision notes: engine unavailable, session notes dropped actor_id={actor_id} error={error}"
                );
                false
            }
            Err(error) => {
                tracing::warn!(
                    target: "vogt.sessions",
                    "brief decision notes: engine refused the role lookup, note author counted as a worker actor_id={actor_id} error={}",
                    error.code()
                );
                false
            }
        }
    };
    move |comment: &crate::core::Comment| {
        let person = matches!(
            view.actor_by_id(&comment.actor_id),
            Ok(Some(actor)) if actor.kind == ActorKind::Human
        );
        if person {
            return true;
        }
        if let Some(known) = roles.borrow().get(&comment.actor_id) {
            return *known;
        }
        let judged = oversight(&comment.actor_id);
        roles.borrow_mut().insert(comment.actor_id.clone(), judged);
        judged
    }
}

/// Whether the caller counts as a person for the engine's person gate: a human
/// caller who is not the engine's own credential.
pub(crate) fn answers_as_person<C, I>(ctx: &AppContext<C, I>) -> Result<bool, VogtError>
where
    C: Clock,
    I: IdFactory,
{
    Ok(ctx.principal.kind != ActorKind::Agent && !is_engine_credential(ctx)?)
}

/// Whether the caller is the session engine: the actor its credential is bound
/// to by configuration, or the very token it shares with this core.
pub(super) fn is_engine_credential<C, I>(ctx: &AppContext<C, I>) -> Result<bool, VogtError>
where
    C: Clock,
    I: IdFactory,
{
    if ctx.principal.identity_ref == ctx.config.bootstrap_core_token_actor {
        return Ok(true);
    }
    let Some(configured) = &ctx.config.bootstrap_core_token_file else {
        return Ok(false);
    };
    let Some(presented) = &ctx.token else {
        return Ok(false);
    };
    let secret = match std::fs::read_to_string(configured) {
        Ok(text) => text.trim().to_string(),
        Err(_) => return Ok(false),
    };
    if secret.is_empty() {
        return Ok(false);
    }
    let row = ctx
        .declared
        .read()?
        .token_by_hash(&auth::hash_token(&secret))?;
    Ok(row.is_some_and(|row| row.revoked_at.is_none() && row.id == presented.id))
}
