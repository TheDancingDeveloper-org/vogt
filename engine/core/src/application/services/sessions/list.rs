use super::*;

/// The columns a `starters_only` read may name, all held on the declared
/// session row: the GUI rail nests a session under the one that started it
/// (WI-4531), and a terminal reads the work item its session was opened for
/// (WI-4543).
const STARTER_FIELDS: [&str; 3] = ["engine_session_id", "started_by_session", "work_item"];

/// The starters path is asked for with `starters_only: true` (WI-4543), never
/// inferred from `fields`, so a caller that names starter columns alone still
/// gets the ordinary full rows. The flag needs `mode=full`, `fields` naming
/// only declared-row columns, and no filter that needs the engine's state.
pub(super) fn starters_only(
    parsed: &ListSessionsParams,
    flag: bool,
    started_by_session_set: bool,
) -> Result<bool, VogtError> {
    if !flag {
        return Ok(false);
    }
    let fields = parsed.fields.as_deref().unwrap_or_default();
    let usable = parsed.mode == "full"
        && !fields.is_empty()
        && fields
            .iter()
            .all(|field| STARTER_FIELDS.contains(&field.as_str()))
        && parsed.order == "started"
        && parsed.states.is_none()
        && !parsed.include_hidden
        && !started_by_session_set;
    if usable {
        Ok(true)
    } else {
        Err(VogtError::InvalidRequest(
            "starters_only needs mode=full, fields naming only engine_session_id, started_by_session and work_item, order=started, and no states, include_hidden or started_by_session"
                .to_string(),
        ))
    }
}

/// Each Vogt session's `id` plus the named starter columns, read from the
/// declared table only: no engine call and no transcript read (WI-4531).
/// Unlinked engine sessions have no starter and are not listed. The page is
/// cut in the query; `total` is one past the page when more rows follow.
pub(super) fn list_starters<C, I>(
    ctx: &AppContext<C, I>,
    params: &ListSessionsParams,
) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let view = ctx.declared.read()?;
    let project_id = match &params.project {
        Some(slug) => Some(resolve::project(&view, slug)?.id),
        None => None,
    };
    let work_item_id = match &params.work_item {
        Some(reference) => Some(resolve::work_item(&view, reference)?.id),
        None => None,
    };
    let limit = params.limit.max(0);
    let offset = params.offset.max(0);
    let mut rows = view.list_sessions(
        project_id.as_deref(),
        work_item_id.as_deref(),
        params.include_stopped,
        limit.saturating_add(1),
        offset,
    )?;
    let truncated = rows.len() as i64 > limit;
    rows.truncate(limit as usize);
    let fields = params.fields.as_deref().unwrap_or_default();
    // The item's ref is read once per item, and only when it was asked for.
    let mut refs: std::collections::BTreeMap<String, Option<String>> = Default::default();
    if fields.iter().any(|wanted| wanted == "work_item") {
        let mut ids: Vec<String> = rows
            .iter()
            .filter_map(|row| row.work_item_id.clone())
            .collect();
        ids.sort();
        ids.dedup();
        for (id, reference, _, _) in view.work_briefs_by_ids(&ids)? {
            refs.insert(id, Some(reference));
        }
        for id in ids {
            refs.entry(id).or_insert(None);
        }
    }
    drop(view);
    let sessions: Vec<Value> = rows
        .iter()
        .map(|session| {
            let mut row = serde_json::Map::new();
            row.insert("id".to_string(), Value::String(session.id.clone()));
            for field in STARTER_FIELDS {
                if !fields.iter().any(|wanted| wanted == field) {
                    continue;
                }
                let value = match field {
                    "engine_session_id" => Value::String(session.engine_session_id.clone()),
                    "work_item" => session
                        .work_item_id
                        .as_deref()
                        .and_then(|id| refs.get(id).cloned().flatten())
                        .map_or(Value::Null, Value::String),
                    _ => session
                        .started_by_session
                        .clone()
                        .map_or(Value::Null, Value::String),
                };
                row.insert(field.to_string(), value);
            }
            Value::Object(row)
        })
        .collect();
    let total = offset + sessions.len() as i64 + i64::from(truncated);
    Ok(serde_json::json!({
        "sessions": sessions,
        "digests": [],
        "engine": null,
        "total": total,
        "truncated": truncated,
        "hidden_count": 0,
    }))
}

/// Slim `mode=full` rows to `id` plus the named `fields` (WI-3130). A full page
/// of the whole roster is over a megabyte; a caller that reads a few columns
/// names them. No `fields` (the default) leaves every row whole, which is what
/// the Python core returns, and an unknown name is left out, as a digest's
/// `extra` leaves it.
pub(in crate::application::services) fn slim_full_rows(
    listed: &mut Value,
    fields: Option<&[String]>,
) {
    let Some(rows) = listed.get_mut("sessions").and_then(Value::as_array_mut) else {
        return;
    };
    let Some(fields) = fields.filter(|fields| !fields.is_empty()) else {
        // The launch line is long and repeated on every row; it is left out
        // unless `fields` asks for it (WI-4725).
        for row in rows {
            if let Some(row) = row.as_object_mut() {
                row.remove("command");
            }
        }
        return;
    };
    for row in rows {
        let Some(whole) = row.as_object() else {
            continue;
        };
        let slim: serde_json::Map<String, Value> = whole
            .iter()
            .filter(|(name, _)| name.as_str() == "id" || fields.iter().any(|f| f == *name))
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
        *row = Value::Object(slim);
    }
}

/// List sessions, live activity merged in. The engine being down is a fact
/// about the rows, not a failure of the list.
pub fn list_sessions<C, I>(
    ctx: &AppContext<C, I>,
    params: &ListSessionsParams,
) -> Result<SessionListResult, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let (live, detail, hidden_count) = match ctx.engine.as_ref() {
        None => (
            Vec::new(),
            Some("no session engine is configured (VOGT_ENGINE_URL is unset)".to_string()),
            0,
        ),
        Some(engine) => match engine.list_sessions_counted(params.include_hidden) {
            // Kept as a Vec: the engine's order is the order unlinked rows
            // append in, and a HashMap would shuffle it.
            Ok((rows, hidden)) => (rows, None, hidden as i64),
            Err(VogtError::EngineUnavailable(text)) => (Vec::new(), Some(text), 0),
            Err(error) => return Err(error),
        },
    };
    // Python's session list reads the clock once here, after the engine
    // answers or does not, and before the rows are assembled. The value is
    // the attention reference.
    let now = super::super::now_of(&ctx.clock);
    let full = params.mode == "full";
    // Full mode keeps every joined row, and rss order reads only the engine's
    // sample, so the page can be cut before any transcript is read (WI-4531).
    // Digest mode filters on `running`, which the transcript decides.
    let (mut joined, declared_overflow) = if full {
        joined_rows(ctx, params, &live, detail.as_deref())?
    } else {
        rows_of(ctx, params, &live, detail.as_deref())?
    };
    let stall_after = params.stall_after_minutes.saturating_mul(60);
    // The starter filter holds on every path: full or digest, with or without
    // `states`, linked or not (WI-4725).
    if let Some(starter) = params.started_by_session.as_deref() {
        let starter = resolve_target(ctx, starter)?.engine_session_id;
        joined.retain(|summary| summary.started_by_session.as_deref() == Some(starter.as_str()));
    }
    let mut kept = Vec::new();
    for summary in joined.drain(..) {
        if full
            || digest_kept(
                &summary,
                attention_name(&summary, now, stall_after),
                params.states.as_deref(),
            )
        {
            kept.push(summary);
        }
    }
    if params.order == "rss" {
        // Stable: sessions with no sample keep their order, after the
        // measured ones. Sweep shares rows_of and must not inherit this.
        kept.sort_by(|left, right| {
            let rss =
                |row: &SessionSummary| row.resources.as_ref().map(|r| r.rss_bytes).unwrap_or(0);
            match (left.resources.is_none(), right.resources.is_none()) {
                (true, false) => std::cmp::Ordering::Greater,
                (false, true) => std::cmp::Ordering::Less,
                _ => rss(right).cmp(&rss(left)),
            }
        });
    }
    let total = kept.len() as i64;
    let offset = params.offset.max(0) as usize;
    let limit = params.limit.max(0) as usize;
    let mut page: Vec<SessionSummary> = kept.into_iter().skip(offset).take(limit).collect();
    if full {
        read_transcripts(ctx, &mut page, &live);
    }
    // When the declared fetch hits its ceiling, more links match than were
    // joined, so `total` is a floor and the list stays truncated.
    let mut reported = total;
    let truncated = (offset as i64) + (page.len() as i64) < total || declared_overflow;
    if declared_overflow {
        reported += 1;
    }
    if full {
        return Ok(SessionListResult {
            sessions: page,
            digests: Vec::new(),
            engine: detail,
            total: reported,
            truncated,
            hidden_count,
        });
    }
    let digests = page
        .iter()
        .map(|summary| {
            digest_of(
                summary,
                attention_name(summary, now, stall_after),
                params.fields.as_deref(),
            )
        })
        .collect();
    Ok(SessionListResult {
        sessions: Vec::new(),
        digests,
        engine: detail,
        total: reported,
        truncated,
        hidden_count,
    })
}

/// One past the operation's own page ceiling, so a match beyond it is
/// overflow rather than a silently complete list. Python `_DECLARED_FETCH`.
const DECLARED_FETCH: i64 = 501;

/// The rows `session.list` and `session.sweep` share, before any transcript
/// is read.
///
/// The second value is true when more declared links match than
/// `DECLARED_FETCH` read, so the caller's `total` is a floor.
fn joined_rows<C, I>(
    ctx: &AppContext<C, I>,
    params: &ListSessionsParams,
    live: &[EngineSession],
    detail: Option<&str>,
) -> Result<(Vec<SessionSummary>, bool), VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let view = ctx.declared.read()?;
    let project_id = match &params.project {
        Some(slug) => Some(resolve::project(&view, slug)?.id),
        None => None,
    };
    let work_item_id = match &params.work_item {
        Some(reference) => Some(resolve::work_item(&view, reference)?.id),
        None => None,
    };
    // The page is cut after the state filter, so the declared half is the
    // joined set, not this caller's page. One past the operation ceiling.
    let recorded = view.list_sessions(
        project_id.as_deref(),
        work_item_id.as_deref(),
        params.include_stopped,
        DECLARED_FETCH,
        0,
    )?;
    let declared_overflow = recorded.len() as i64 == DECLARED_FETCH;
    let recorded: Vec<_> = if declared_overflow {
        recorded
            .into_iter()
            .take((DECLARED_FETCH - 1) as usize)
            .collect()
    } else {
        recorded
    };
    drop(view);
    let mut summaries = Vec::with_capacity(recorded.len());
    for session in &recorded {
        let engine_session = live.iter().find(|row| row.id == session.engine_session_id);
        summaries.push(summarize_asked(
            ctx,
            session,
            engine_session,
            detail.is_none(),
        )?);
    }
    let unfiltered = project_id.is_none() && work_item_id.is_none();
    // Joined before the page is cut, matching Python: an offset applied here
    // would drop unlinked sessions from every page but the first.
    if unfiltered {
        let view = ctx.declared.read()?;
        for engine_session in live {
            // A hibernated session is asleep, not stopped: listed always. An
            // exited one is the engine's leftover, listed only when stopped
            // sessions were asked for. Linked rows are not filtered here —
            // the declared query already applied include_stopped.
            // An empty states list asks for every state, including an
            // unlinked session whose process has exited. Otherwise that
            // leftover is listed only when stopped sessions were asked for.
            let every_state = params.states.as_ref().is_some_and(Vec::is_empty);
            if !engine_session.alive
                && !engine_session.hibernated()
                && !params.include_stopped
                && !every_state
            {
                continue;
            }
            if view.session_by_engine_id(&engine_session.id)?.is_none() {
                summaries.push(unlinked_summary(&view, engine_session)?);
            }
        }
    }
    // The latest typed report per engine session, attached before the
    // transcript loop so a report can replace the reply excerpt (WI-2039).
    let ids: Vec<String> = summaries
        .iter()
        .map(|summary| summary.engine_session_id.clone())
        .collect();
    let reports = latest_report_lines(ctx, &ids);
    for summary in &mut summaries {
        if let Some(line) = reports.get(&summary.engine_session_id) {
            summary.latest_report = Some(line.clone());
        }
    }
    Ok((summaries, declared_overflow))
}

#[cfg(test)]
thread_local! {
    /// The engine sessions `read_transcripts` looked up, in order (WI-4531).
    pub(in crate::application::services) static TRANSCRIPT_READS: std::cell::RefCell<Vec<String>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// The joined rows with their transcript-derived fields filled in.
pub(super) fn rows_of<C, I>(
    ctx: &AppContext<C, I>,
    params: &ListSessionsParams,
    live: &[EngineSession],
    detail: Option<&str>,
) -> Result<(Vec<SessionSummary>, bool), VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let (mut summaries, declared_overflow) = joined_rows(ctx, params, live, detail)?;
    read_transcripts(ctx, &mut summaries, live);
    Ok((summaries, declared_overflow))
}

/// Fills `running`, the engine's template and `last_reply_excerpt` from each
/// live session's transcript. Callers run it on as few rows as they can: it
/// is file I/O per live session (WI-4531).
fn read_transcripts<C, I>(
    ctx: &AppContext<C, I>,
    summaries: &mut [SessionSummary],
    live: &[EngineSession],
) where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    // Outside the read transaction: this is file I/O, and an excerpt that
    // cannot be had is a null, never an error. Only a session the engine
    // still has alive or asleep gets a transcript read.
    for summary in summaries.iter_mut() {
        let Some(live_session) = live.iter().find(|row| row.id == summary.engine_session_id) else {
            continue;
        };
        if !(live_session.alive || live_session.hibernated()) {
            continue;
        }
        let template = summary
            .template
            .clone()
            .filter(|template| !template.is_empty())
            .or_else(|| live_session.template.clone());
        #[cfg(test)]
        TRANSCRIPT_READS.with(|reads| reads.borrow_mut().push(summary.engine_session_id.clone()));
        let found = transcript_for(ctx, live_session, template.as_deref());
        summary.running = running_of(summary, live_session, found.as_ref());
        if summary.template.is_none()
            && live_session
                .template
                .as_deref()
                .is_some_and(|template| !template.is_empty())
        {
            // An unlinked session's template is the engine's to report.
            summary.template = live_session.template.clone();
        }
        if let Some(found) = found.as_ref() {
            if let Some(excerpt) = transcripts::last_reply_excerpt(found) {
                summary.last_reply_excerpt = Some(excerpt);
            }
        } else if live_session.conversation_agent.as_deref() == Some("opencode") {
            if let Some(excerpt) = opencode_excerpt(ctx, &live_session.id) {
                summary.last_reply_excerpt = Some(excerpt);
            }
        }
    }
}

fn page_len(shown: usize, _unlinked: i64) -> i64 {
    shown as i64
}
