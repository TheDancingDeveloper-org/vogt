use super::*;

/// One pass over every session the engine has, ordered by who needs a person.
pub fn sweep_sessions<C, I>(
    ctx: &AppContext<C, I>,
    params: &SweepSessionsParams,
) -> Result<SessionSweepResult, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let swept_at = crate::application::services::now_of(&ctx.clock);
    let empty = |engine: Option<String>| SessionSweepResult {
        rows: Vec::new(),
        counts: serde_json::Map::from_iter([
            ("total".to_string(), json!(0)),
            ("needs_you".to_string(), json!(0)),
        ]),
        swept_at,
        engine,
    };
    let engine = match ctx.engine.as_ref() {
        None => {
            return Ok(empty(Some(
                "no session engine is configured (VOGT_ENGINE_URL is unset)".to_string(),
            )))
        }
        Some(engine) => engine,
    };
    let entries = match engine.sweep_sessions(params.screen_lines) {
        Ok(Some(rows)) => rows,
        Ok(None) => engine
            .list_sessions()?
            .into_iter()
            .filter(|row| row.alive || row.hibernated())
            .map(|session| EngineSweepEntry {
                session,
                screen_tail: Vec::new(),
                ready: false,
            })
            .collect(),
        Err(VogtError::EngineUnavailable(text)) => return Ok(empty(Some(text))),
        Err(error) => return Err(error),
    };
    let live: Vec<EngineSession> = entries.iter().map(|entry| entry.session.clone()).collect();
    let (listed, _declared_overflow) = rows_of(
        ctx,
        &ListSessionsParams {
            project: params.project.clone(),
            work_item: None,
            include_stopped: false,
            limit: 500,
            offset: 0,
            order: "started".to_string(),
            mode: "full".to_string(),
            states: None,
            stall_after_minutes: params.stall_after_minutes,
            include_hidden: false,
            fields: None,
            started_by_session: None,
        },
        &live,
        None,
    )?;
    let stall = params.stall_after_minutes.saturating_mul(60);
    let mut rows = Vec::new();
    for summary in listed {
        let Some(entry) = entries
            .iter()
            .find(|entry| entry.session.id == summary.engine_session_id)
        else {
            continue;
        };
        let verdict = decisions::classify(
            summary.activity.as_deref(),
            summary.alive,
            Some(entry.ready),
            summary
                .approval
                .as_ref()
                .map(|approval| approval.question.as_str()),
            summary
                .blocked
                .as_ref()
                .map(|blocked| blocked.blocker.as_str()),
            summary
                .approval
                .as_ref()
                .map(|approval| approval.kind.as_str()),
            summary
                .last_output_at
                .as_deref()
                .and_then(|text| crate::core::from_iso(text).ok()),
            swept_at,
            stall,
            summary.done_at.as_deref(),
        );
        // The overseer's recorded pick, shown on a dialog waiting for an
        // answer (WI-996 E0b); never chosen for the person.
        let recommendation = match (verdict.attention.as_str(), entry.session.approval.as_ref()) {
            ("approval", Some(approval)) if !approval.options.is_empty() => {
                let view = ctx.declared.read()?;
                session_blocked::dialog_recommendation(&view, &entry.session.id, approval)?
            }
            _ => None,
        };
        rows.push(SessionSweepRow {
            attention: verdict.attention.as_str().to_string(),
            attention_reason: verdict.reason.clone(),
            screen_tail: entry.screen_tail.clone(),
            ready: Some(entry.ready),
            recommendation,
            session: summary,
        });
    }
    narrow_sweep(ctx, params, &mut rows)?;
    rows.sort_by(|left, right| {
        decisions::Attention::order_of(&left.attention)
            .cmp(&decisions::Attention::order_of(&right.attention))
            .then_with(|| {
                right
                    .session
                    .last_output_at
                    .cmp(&left.session.last_output_at)
            })
    });
    let mut counts = serde_json::Map::from_iter([
        ("total".to_string(), json!(rows.len() as i64)),
        ("needs_you".to_string(), json!(0)),
    ]);
    let mut needs_you = 0;
    for row in &rows {
        let entry = counts.entry(row.attention.clone()).or_insert(json!(0));
        *entry = json!(entry.as_i64().unwrap_or(0) + 1);
        if decisions::Attention::needs_you_name(&row.attention) {
            needs_you += 1;
        }
    }
    counts.insert("needs_you".to_string(), json!(needs_you));
    Ok(SessionSweepResult {
        rows,
        counts,
        swept_at,
        engine: None,
    })
}

/// Apply `exclude_roles`, `supervised_by` and `since` (WI-1525). Counts are
/// taken from what is left, so a sweep that hides the person's own terminals
/// does not report them as sessions needing attention.
fn narrow_sweep<C, I>(
    ctx: &AppContext<C, I>,
    params: &SweepSessionsParams,
    rows: &mut Vec<SessionSweepRow>,
) -> Result<(), VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    if !params.exclude_roles.is_empty() {
        rows.retain(|row| {
            !row.session
                .role
                .as_deref()
                .is_some_and(|role| params.exclude_roles.iter().any(|wanted| wanted == role))
        });
    }
    if let Some(supervisor) = params.supervised_by.as_deref() {
        let supervisor = resolve_target(ctx, supervisor)?.engine_session_id;
        rows.retain(|row| row.session.started_by_session.as_deref() == Some(supervisor.as_str()));
    }
    if let Some(since) = params.since.as_deref() {
        let since = crate::core::from_iso(since).map_err(|_| {
            VogtError::InvalidRequest(format!(
                "since must be an ISO-8601 time, got {}",
                crate::core::py_repr(since)
            ))
        })?;
        let after = |text: Option<&str>| {
            text.and_then(|text| crate::core::from_iso(text).ok())
                .is_some_and(|moment| moment > since)
        };
        rows.retain(|row| {
            let session = &row.session;
            after(session.last_output_at.as_deref())
                || after(session.done_at.as_deref())
                || after(session.turn_started_at.as_deref())
                || after(
                    session
                        .blocked
                        .as_ref()
                        .and_then(|blocked| blocked.since.as_deref()),
                )
        });
    }
    Ok(())
}
