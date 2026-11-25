use super::*;

/// The tail of a session's own raw output log.
pub fn log_tail<C, I>(
    ctx: &AppContext<C, I>,
    params: &LogTailParams,
) -> Result<LogTailResult, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    // No engine is an ordinary answer here, not an error: there is no log to
    // read. A `ses_…` id Vogt has no record of is still a wrong id.
    let Some(engine) = ctx.engine.as_ref() else {
        return Ok(LogTailResult {
            engine: Some(NO_ENGINE.to_string()),
            ..LogTailResult::empty()
        });
    };
    let engine_id = resolve_target(ctx, &params.id)?.engine_session_id;
    // `tail_bytes` is bounded by the parameter schema (1..=256 KiB), exactly as
    // Python's pydantic model bounds it, and the service passes it through.
    let log = match engine.history_log(&engine_id, params.tail_bytes, params.strip_ansi) {
        // A missing log — unknown id, or history off — is an empty answer, not
        // an error: there is simply no output to show.
        Ok(None) => return Ok(LogTailResult::empty()),
        Ok(Some(log)) => log,
        Err(VogtError::EngineUnavailable(message)) => {
            return Ok(LogTailResult {
                engine: Some(message),
                ..LogTailResult::empty()
            });
        }
        Err(error) => return Err(error),
    };
    Ok(LogTailResult {
        session_id: Some(log.session_id),
        text: log.text,
        bytes: log.bytes,
        total_bytes: log.total_bytes,
        truncated: log.truncated,
        engine: None,
    })
}

/// Read a session's screen.
pub fn screen<C, I>(
    ctx: &AppContext<C, I>,
    params: &ScreenParams,
) -> Result<SessionScreenResult, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let engine = engine_of(ctx)?;
    let engine_id = resolve_target(ctx, &params.id)?.engine_session_id;
    let found = engine.session_screen(&engine_id, params.scrollback_lines)?;
    Ok(screen_result(
        &params.id,
        &engine_id,
        &require_screen(engine, found, &engine_id, &params.id)?,
    ))
}

/// The screen the caller asked about. `named` is the id they used, kept as
/// given; the engine's own id is reported beside it.
pub(super) fn screen_result(
    named: &str,
    engine_id: &str,
    screen: &EngineScreen,
) -> SessionScreenResult {
    let cursor = match (screen.cursor_row, screen.cursor_col) {
        (Some(row), Some(col)) => Some(SessionCursor { row, col }),
        _ => None,
    };
    SessionScreenResult {
        id: named.to_string(),
        engine_session_id: engine_id.to_string(),
        cols: screen.cols,
        rows: screen.rows,
        lines: screen.lines.clone(),
        cursor,
        title: screen.title.clone(),
        activity: screen.activity.clone(),
        alive: screen.alive,
        ready: screen.ready,
        scrollback: screen.scrollback.clone(),
        turn_started_at: screen.turn_started_at.clone(),
        last_output_at: screen.last_output_at.clone(),
        approval: screen.approval.as_ref().map(approval_of),
        blocked: screen.blocked.as_ref().map(blocked_of),
    }
}

/// A missing screen is either an engine without the route, or a session the
/// engine no longer holds. The second lookup tells them apart.
fn require_screen(
    engine: &EngineClient,
    screen: Option<EngineScreen>,
    engine_id: &str,
    named: &str,
) -> Result<EngineScreen, VogtError> {
    if let Some(screen) = screen {
        return Ok(screen);
    }
    if engine.get_session(engine_id)?.is_some() {
        return Err(VogtError::EngineUnavailable(
            "the session engine does not support screen yet (it has no GET              /api/sessions/{id}/screen route); upgrade the engine, or read the              output with session.log_tail"
                .to_string(),
        ));
    }
    Err(VogtError::NotFound(format!(
        "the engine has no live session {}",
        py_repr(named)
    )))
}
