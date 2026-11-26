use super::*;

const DIGEST_LIVE: &[&str] = &[
    "running", "idle", "waiting", "blocked", "approval", "stalled", "done", "unknown",
];
const EXCLUDED_STATES: &[&str] = &["hibernated", "stopped", "exited"];
const DIGEST_CLIP: usize = 120;

pub(super) fn attention_name(
    summary: &SessionSummary,
    now: crate::core::Moment,
    stall_after_secs: i64,
) -> String {
    decisions::classify(
        summary.activity.as_deref(),
        summary.alive,
        Some(summary.activity.as_deref() == Some("waiting-for-input")),
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
        now,
        stall_after_secs,
        summary.done_at.as_deref(),
    )
    .attention
    .as_str()
    .to_string()
}

fn state_of(summary: &SessionSummary, attention: &str) -> Option<String> {
    if summary.activity.as_deref() == Some("stopped") || attention == "exited" {
        return Some(if attention == "exited" {
            "exited".to_string()
        } else {
            summary
                .activity
                .clone()
                .unwrap_or_else(|| attention.to_string())
        });
    }
    summary.activity.clone().or_else(|| {
        if attention.is_empty() {
            None
        } else {
            Some(attention.to_string())
        }
    })
}

/// Default listings drop hibernated, stopped and exited. An empty `states`
/// list means the caller asked for every state.
pub(super) fn digest_kept(
    summary: &SessionSummary,
    attention: String,
    states: Option<&[String]>,
) -> bool {
    if states.is_some_and(|wanted| wanted.is_empty()) {
        return true;
    }
    let allowed: Vec<&str> = match states {
        None => DIGEST_LIVE.to_vec(),
        Some(wanted) => wanted.iter().map(String::as_str).collect(),
    };
    let state = state_of(summary, &attention);
    let Some(state) = state else {
        return allowed.contains(&"unknown");
    };
    if allowed.contains(&state.as_str()) || allowed.contains(&attention.as_str()) {
        return true;
    }
    !EXCLUDED_STATES.contains(&state.as_str()) && allowed.contains(&state.as_str())
}

fn clip(text: &str) -> String {
    text.chars().take(DIGEST_CLIP).collect()
}

pub(super) fn digest_of(
    summary: &SessionSummary,
    attention: String,
    fields: Option<&[String]>,
) -> SessionDigestRow {
    let blocked = summary
        .blocked
        .as_ref()
        .map(|blocked| blocked.blocker.clone());
    let approval = summary.approval.as_ref();
    let report = summary.latest_report.clone();
    let excerpt = if report.is_some() {
        None
    } else {
        summary.last_reply_excerpt.as_ref().map(|text| clip(text))
    };
    SessionDigestRow {
        id: summary.id.clone(),
        name: summary.name.clone(),
        work_item: summary.work_item.clone(),
        activity: summary.activity.clone(),
        attention: Some(attention),
        ready: Some(summary.activity.as_deref() == Some("waiting-for-input")),
        blocked_reason: blocked.as_ref().map(|text| clip(text)),
        blocked_detail: blocked.filter(|text| text.chars().count() > DIGEST_CLIP),
        turn_started_at: summary.turn_started_at.clone(),
        done_at: summary.done_at.clone(),
        last_reply_excerpt: excerpt,
        report,
        template: summary.template.clone(),
        brief_file: None,
        question: approval.map(|approval| approval.question.clone()),
        question_kind: approval.map(|approval| approval.kind.clone()),
        options: approval.and_then(|approval| {
            if approval.options.is_empty() {
                None
            } else {
                Some(
                    approval
                        .options
                        .iter()
                        .map(|option| option.label.clone())
                        .collect(),
                )
            }
        }),
        budget: budget_of(summary.economics.as_ref()),
        supervisor: None,
        extra: digest_extra(summary, fields),
    }
}

fn budget_of(economics: Option<&serde_json::Map<String, Value>>) -> Option<String> {
    let payload = economics?;
    if payload.is_empty() {
        return None;
    }
    let int_of = |name: &str| -> i64 {
        match payload.get(name) {
            // Python `_int_of` accepts real ints and rejects bools. JSON has
            // no bool-as-number, so a number is enough; a float is not an int.
            Some(Value::Number(number)) if number.is_i64() => number.as_i64().unwrap_or(0),
            _ => 0,
        }
    };
    let reading = crate::core::session_economics::SessionEconomics {
        turns: int_of("turns"),
        context_tokens: int_of("context_tokens"),
        peak_context_tokens: int_of("peak_context_tokens"),
        compactions: int_of("compactions"),
        tool_result_bytes: match payload.get("tool_result_bytes") {
            Some(Value::Object(tools)) => tools
                .iter()
                .filter_map(|(name, value)| value.as_i64().map(|number| (name.clone(), number)))
                .collect(),
            _ => Default::default(),
        },
        reads_repeated: int_of("reads_repeated"),
        output_tokens: int_of("output_tokens"),
        usage_measured: payload.get("usage_measured") == Some(&Value::Bool(true)),
        ..Default::default()
    };
    Some(reading.budget_line(None, None))
}

fn digest_extra(
    summary: &SessionSummary,
    fields: Option<&[String]>,
) -> Option<serde_json::Map<String, Value>> {
    let wanted = fields?;
    if wanted.is_empty() {
        return None;
    }
    let dumped = serde_json::to_value(summary).unwrap_or(Value::Null);
    let object = dumped.as_object();
    let mut extra = serde_json::Map::new();
    for name in wanted {
        if name == "command" {
            continue;
        }
        if let Some(value) = object.and_then(|object| object.get(name)) {
            extra.insert(name.clone(), value.clone());
        }
    }
    if extra.is_empty() {
        None
    } else {
        Some(extra)
    }
}

/// Newest report per engine session, already formatted as the digest line.
/// A missing table or a locked database is no report, not a failed list.
pub(super) fn latest_report_lines<C, I>(
    ctx: &AppContext<C, I>,
    ids: &[String],
) -> std::collections::HashMap<String, String>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    use std::collections::HashMap;
    let mut found = HashMap::new();
    let wanted: Vec<&str> = ids
        .iter()
        .filter(|id| !id.is_empty())
        .map(String::as_str)
        .collect();
    if wanted.is_empty() {
        return found;
    }
    let path = ctx.declared.path().to_path_buf();
    let rows = crate::storage::sqlite::connection::latest_report_rows(&path, &wanted);
    for (kind, summary, data, engine_id) in rows {
        let Some(engine_id) = engine_id else { continue };
        found.insert(engine_id, report_line(&kind, &summary, &data));
    }
    found
}

/// Python `reports.one_line`, clipped at 200 rather than the excerpt's 120.
fn report_line(kind: &str, summary: &str, data: &str) -> String {
    let parsed: serde_json::Value = serde_json::from_str(data).unwrap_or(Value::Null);
    let mut extras = Vec::new();
    if let Some(branch) = parsed
        .get("branch")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
    {
        extras.push(format!("branch={branch}"));
    }
    if let Some(sha) = parsed
        .get("sha")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
    {
        let short: String = sha.chars().take(12).collect();
        extras.push(format!("sha={short}"));
    }
    if let Some(pr) = parsed.get("pr").filter(|value| !value.is_null()) {
        let text = match pr {
            Value::String(text) if !text.is_empty() => Some(text.clone()),
            Value::Number(number) => Some(number.to_string()),
            _ => None,
        };
        if let Some(text) = text {
            extras.push(format!("pr={text}"));
        }
    }
    let mut line = format!("{kind}: {summary}");
    if !extras.is_empty() {
        line = format!("{line} [{}]", extras.join(" "));
    }
    let count = line.chars().count();
    if count <= 200 {
        line
    } else {
        let head: String = line.chars().take(199).collect();
        format!("{head}…")
    }
}

/// The last non-blank screen lines, for a wait that did not ask for the
/// whole screen.
pub(super) fn tail_of(screen: &SessionScreenResult) -> Vec<String> {
    let kept: Vec<&String> = screen
        .lines
        .iter()
        .filter(|line| !line.trim().is_empty())
        .collect();
    kept[kept.len().saturating_sub(WAIT_TAIL_LINES)..]
        .iter()
        .map(|line| (*line).clone())
        .collect()
}

/// The digest of a session as an input or wait leaves it: no attention and no
/// ready flag, which only a sweep computes.
fn plain_digest(summary: &SessionSummary) -> SessionDigestRow {
    let mut row = digest_of(summary, String::new(), None);
    row.attention = None;
    row.ready = None;
    row
}

fn digest_of_target<C, I>(
    ctx: &AppContext<C, I>,
    found: &Target,
    live: Option<&EngineSession>,
) -> Option<SessionDigestRow>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    match (&found.session, live) {
        (Some(session), live) => summarize(ctx, session, live).ok(),
        (None, Some(live)) => ctx
            .declared
            .read()
            .ok()
            .and_then(|view| unlinked_summary(&view, live).ok()),
        (None, None) => None,
    }
    .map(|summary| plain_digest(&summary))
}

pub(super) fn wait_digest<C, I>(
    ctx: &AppContext<C, I>,
    named: &str,
    engine_id: &str,
) -> Option<SessionDigestRow>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let engine = engine_of(ctx).ok()?;
    let live = engine.get_session(engine_id).ok().flatten()?;
    let found = resolve_target(ctx, named).ok()?;
    digest_of_target(ctx, &found, Some(&live))
}

/// The digest of the session just typed into, when it can be read. A submitted
/// input also clears the session's blocked report (WI-1834): a new instruction
/// answers what it was blocked on, or supersedes it.
pub(super) fn input_digest<C, I>(
    ctx: &AppContext<C, I>,
    engine: &EngineClient,
    found: &Target,
    submitted: bool,
    reason: &str,
) -> Option<SessionDigestRow>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let mut live = engine.get_session(&found.engine_session_id).ok().flatten();
    if submitted {
        if let Some(current) = live.take() {
            live = Some(
                clear_block_on(ctx, engine, found, "input", reason, &current).unwrap_or(current),
            );
        }
    }
    digest_of_target(ctx, found, live.as_ref())
}
