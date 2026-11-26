use super::*;

/// Block until a session reaches a state, or the timeout passes.
pub fn wait<C, I>(
    ctx: &AppContext<C, I>,
    params: &WaitParams,
) -> Result<SessionWaitResult, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let engine = engine_of(ctx)?;
    let engine_id = resolve_target(ctx, &params.id)?.engine_session_id;
    let until = params.until.replace('_', "-");
    let found = match engine.wait_session(
        &engine_id,
        &until,
        Duration::from_secs(params.timeout_s.max(0) as u64),
    ) {
        // A hibernated session does nothing until somebody wakes it, and a wait
        // is a read: it does not wake it. Say so at once, with the screen it
        // kept, rather than holding the caller to a timeout.
        Err(refused @ VogtError::Conflict(_)) => {
            let screen = engine.session_screen(&engine_id, 0)?;
            if screen
                .as_ref()
                .is_none_or(|screen| screen.activity.as_deref() != Some("hibernated"))
            {
                return Err(refused);
            }
            let Some(screen) = screen else {
                // The activity check above only reaches here when a screen was
                // returned and it says hibernated. A missing screen is the
                // engine disagreeing with itself, not a reason to panic.
                return Err(VogtError::EngineUnavailable(
                    "the session engine reported a hibernated session with no screen".to_string(),
                ));
            };
            let shown = screen_result(&params.id, &engine_id, &screen);
            return Ok(SessionWaitResult {
                id: params.id.clone(),
                engine_session_id: engine_id.clone(),
                outcome: "hibernated".to_string(),
                matched: false,
                waited_ms: 0,
                screen: params.screen.then(|| shown.clone()),
                digest: if params.screen {
                    None
                } else {
                    wait_digest(ctx, &params.id, &engine_id)
                },
                screen_tail: if params.screen {
                    Vec::new()
                } else {
                    tail_of(&shown)
                },
            });
        }
        other => other?,
    };
    let waited = found.ok_or_else(|| {
        if engine.get_session(&engine_id).ok().flatten().is_some() {
            VogtError::EngineUnavailable(
                "the session engine does not support wait yet (it has no GET \
                 /api/sessions/{id}/wait route); upgrade the engine, or poll \
                 session.screen"
                    .to_string(),
            )
        } else {
            VogtError::NotFound(format!(
                "the engine has no live session {}",
                py_repr(&params.id)
            ))
        }
    })?;
    let shown = screen_result(&params.id, &engine_id, &waited.screen);
    Ok(SessionWaitResult {
        id: params.id.clone(),
        engine_session_id: engine_id.clone(),
        outcome: waited.outcome,
        matched: waited.matched,
        waited_ms: waited.waited_ms,
        screen: params.screen.then(|| shown.clone()),
        digest: if params.screen {
            None
        } else {
            wait_digest(ctx, &params.id, &engine_id)
        },
        screen_tail: if params.screen {
            Vec::new()
        } else {
            tail_of(&shown)
        },
    })
}

/// Clear `found`'s blocked report because someone acted on the session. The
/// report it replaces stays in the audit as an unblock naming the cause and
/// what it said. A session acting on itself clears nothing, and a report that
/// cannot be read or cleared is left for the sweep's staleness rule. The
/// session as cleared when one was.
pub(super) fn clear_block_on<C, I>(
    ctx: &AppContext<C, I>,
    engine: &EngineClient,
    found: &Target,
    cause: &str,
    reason: &str,
    current: &EngineSession,
) -> Option<EngineSession>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let engine_id = &found.engine_session_id;
    let caller = ctx.principal.identity_ref.as_str();
    if caller == format!("agent:session:{engine_id}")
        || found
            .session
            .as_ref()
            .is_some_and(|session| caller == format!("agent:session:{}", session.id))
    {
        return None;
    }
    let was = current.blocked.as_ref()?;
    let cleared = engine
        .set_blocked(engine_id, false, None, &[], "person", "other")
        .ok()??;
    let entity = found
        .session
        .as_ref()
        .map_or_else(|| engine_id.clone(), |session| session.id.clone());
    let mut writing = write_of(ctx);
    audited_action(
        &mut writing,
        "session.report_unblocked",
        &format!("cleared by {cause}: {reason}"),
        "session",
        &entity,
        &json!({
            "engine_session_id": engine_id,
            "linked": found.session.is_some(),
            "blocker": Value::Null,
            "items": [],
            "cleared_by": cause,
            "was_blocker": was.reason,
            "was_items": was.items,
            "was_since": was.since,
        }),
        "session.unblocked",
        None,
    )
    .ok()?;
    Some(cleared)
}
