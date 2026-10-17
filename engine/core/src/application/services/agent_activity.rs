//! Reading the agent activity index: what agents did, and how it went.
//!
//! Ports `src/vogt/application/services/agent_activity.py`. The collector writes
//! the index; these two operations only read it. A deployment that has not
//! opted into transcript roots, and one that has but has not swept yet, are
//! different states and both are said, never returned as an empty success.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{json, Value};

use super::lifecycle::expand_user;
use super::{dispatch, Built};

use crate::application::context::AppContext;
use crate::application::resolve;
use crate::core::session_economics::{meter, split_lines, SessionEconomics};
use crate::core::{py_repr, Clock, IdFactory, Moment};
use crate::errors::VogtError;
use crate::storage::interface::{DeclaredStore, ObservedStore, ReadView};
use crate::storage::observed_types::{ActivityEventRow, ActivityQuery, ActivitySessionRow};

/// The index filters a `session` and a `project` become.
struct ActivityScope {
    agent_session_ids: Option<Vec<String>>,
    cwd_roots: Option<Vec<String>>,
}
struct Prepared {
    query: ActivityQuery,
    limit: i64,
    offset: i64,
    indexed_at: Option<Moment>,
    detail: Option<String>,
}
const COLLECTOR_NAME: &str = "agent-activity";
const NOT_CONFIGURED: &str = "agent activity is not collected: no transcript roots are configured (set `agent_activity_roots`)";
const NOT_YET_INDEXED: &str = "agent activity is configured but has not been indexed yet: the next sweep will, or run `vogt sweep --collectors agent-activity`";

pub fn search_agent_activity_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    dispatch!(built, search_agent_activity, &params)
}

pub fn summarize_agent_activity_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    dispatch!(built, summarize_agent_activity, &params)
}

fn search_agent_activity<C, I>(ctx: &AppContext<C, I>, params: &Value) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let prepared = prepare(ctx, params, 500)?;
    if let Some(detail) = prepared.detail {
        return Ok(empty("events", prepared.indexed_at, &detail));
    }
    let rows = ctx
        .observed
        .search_activity(&prepared.query, prepared.limit, prepared.offset)?;
    let links = links_of(
        ctx,
        &rows
            .iter()
            .map(|row| row.agent_session_id.clone())
            .collect::<Vec<_>>(),
    )?;
    let projects = project_roots(ctx)?;
    let events: Vec<Value> = rows
        .iter()
        .map(|row| {
            event_of(
                row,
                links.get(&row.agent_session_id).cloned(),
                project_of(&projects, row.cwd.as_deref()),
            )
        })
        .collect();
    Ok(page(
        "events",
        events,
        prepared.offset,
        prepared.limit,
        prepared.indexed_at,
    ))
}

fn summarize_agent_activity<C, I>(
    ctx: &AppContext<C, I>,
    params: &Value,
) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let prepared = prepare(ctx, params, 200)?;
    if let Some(detail) = prepared.detail {
        return Ok(empty("sessions", prepared.indexed_at, &detail));
    }
    let metered = flag(params, "economics")?;
    let rows = ctx
        .observed
        .summarize_activity(&prepared.query, prepared.limit, prepared.offset)?;
    let links = links_of(
        ctx,
        &rows
            .iter()
            .map(|row| row.agent_session_id.clone())
            .collect::<Vec<_>>(),
    )?;
    let projects = project_roots(ctx)?;
    let sessions: Vec<Value> = rows
        .iter()
        .map(|row| {
            summary_of(
                row,
                links.get(&row.agent_session_id).cloned(),
                project_of(&projects, row.cwd.as_deref()),
                if metered {
                    economics_of(row.transcript_path.as_deref(), &row.agent)
                } else {
                    None
                },
            )
        })
        .collect();
    Ok(page(
        "sessions",
        sessions,
        prepared.offset,
        prepared.limit,
        prepared.indexed_at,
    ))
}

/// The query both operations share, or the honest reason there is nothing to
/// read. `Ok(detail)` is not an error: the index being off or unswept is a
/// state the caller reads.
fn prepare<C, I>(
    ctx: &AppContext<C, I>,
    params: &Value,
    limit_max: i64,
) -> Result<Prepared, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let configured = !ctx.config.agent_activity_roots.is_empty();
    let sweep = if ctx.observed.has_evidence_tables()? {
        ctx.observed
            .coverage()?
            .get(COLLECTOR_NAME)
            .map(|row| row.finished_at)
    } else {
        None
    };
    // No finished sweep means there is nothing to read. A sweep that did
    // finish stays answerable even after roots are switched off.
    let Some(indexed_at) = sweep else {
        let detail = if configured {
            NOT_YET_INDEXED
        } else {
            NOT_CONFIGURED
        };
        return Ok(Prepared {
            query: ActivityQuery::default(),
            limit: 0,
            offset: 0,
            indexed_at: None,
            detail: Some(detail.to_string()),
        });
    };
    let scope = scope_of(ctx, params)?;
    let query = ActivityQuery {
        q: trimmed(params, "q"),
        service: trimmed(params, "service").map(|text| text.to_ascii_lowercase()),
        tool: trimmed(params, "tool"),
        errors_only: flag(params, "errors_only")?,
        since: moment_of(params, "since")?,
        until: moment_of(params, "until")?,
        agent_session_ids: scope.agent_session_ids,
        cwd_roots: scope.cwd_roots,
    };
    Ok(Prepared {
        query,
        limit: bounded(params, "limit", 50, limit_max)?,
        offset: bounded(params, "offset", 0, i64::MAX)?,
        indexed_at,
        detail: None,
    })
}

/// Resolve `project` and `session` into the index's own filters.
///
/// A Vogt session (`ses_…`) narrows to the engine id it was started under,
/// which is also the conversation id of a Claude session Vogt started. Any
/// other session id is an agent conversation id and is matched directly. A
/// project narrows to calls made inside its checkout.
fn scope_of<C, I>(ctx: &AppContext<C, I>, params: &Value) -> Result<ActivityScope, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let view = ctx.declared.read()?;
    let agent_session_ids = match trimmed(params, "session") {
        None => None,
        Some(session) if !session.starts_with("ses_") => Some(vec![session]),
        Some(session) => {
            let found = ctx.declared.read()?.session_by_id(&session)?;
            let Some(found) = found else {
                return Err(VogtError::NotFound(format!(
                    "no session {}",
                    py_repr(&session)
                )));
            };
            Some(vec![found.engine_session_id])
        }
    };
    let cwd_roots = match trimmed(params, "project") {
        None => None,
        Some(slug) => {
            let project = resolve::project(&view, &slug)?;
            // Path's string form: no trailing slash, except the filesystem root.
            let root = expand_user(Path::new(&project.root_path))
                .display()
                .to_string();
            let trimmed_root = root.trim_end_matches('/');
            Some(vec![if trimmed_root.is_empty() {
                "/".to_string()
            } else {
                trimmed_root.to_string()
            }])
        }
    };
    Ok(ActivityScope {
        agent_session_ids,
        cwd_roots,
    })
}

/// Vogt's own session id for an agent conversation, when one was linked.
fn links_of<C, I>(
    ctx: &AppContext<C, I>,
    agent_session_ids: &[String],
) -> Result<BTreeMap<String, String>, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let view = ctx.declared.read()?;
    let mut links = BTreeMap::new();
    for id in agent_session_ids {
        if links.contains_key(id) {
            continue;
        }
        if let Some(session) = view.session_by_engine_id(id)? {
            links.insert(id.clone(), session.id);
        }
    }
    Ok(links)
}

/// Registered project roots, longest first, so a nested project wins.
fn project_roots<C, I>(ctx: &AppContext<C, I>) -> Result<Vec<(String, String)>, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let mut roots: Vec<(String, String)> = ctx
        .declared
        .read()?
        .list_projects(10_000, 0)?
        .into_iter()
        .filter(|project| !project.root_path.is_empty())
        .map(|project| {
            (
                expand_user(Path::new(&project.root_path))
                    .display()
                    .to_string()
                    .trim_end_matches('/')
                    .to_string(),
                project.slug,
            )
        })
        .collect();
    roots.sort_by_key(|root| std::cmp::Reverse(root.0.len()));
    Ok(roots)
}

fn project_of(roots: &[(String, String)], cwd: Option<&str>) -> Option<String> {
    let cwd = cwd?;
    roots.iter().find_map(|(root, slug)| {
        (cwd == root || cwd.starts_with(&format!("{root}/"))).then(|| slug.clone())
    })
}

fn event_of(
    row: &ActivityEventRow,
    vogt_session_id: Option<String>,
    project: Option<String>,
) -> Value {
    json!({
        "id": row.id,
        "at": row.at.to_iso(),
        "finished_at": row.finished_at.map(|moment| moment.to_iso()),
        "duration_ms": duration_ms(row.at, row.finished_at),
        "agent": row.agent,
        "agent_session_id": row.agent_session_id,
        "vogt_session_id": vogt_session_id,
        "project": project,
        "cwd": row.cwd,
        "tool": row.tool,
        "summary": row.summary,
        "services": row.services,
        "error": row.error,
        "excerpt": row.excerpt,
    })
}

/// Meter the conversation's transcript. `None` when it cannot be read: a
/// missing file is "not measured", not an empty conversation.
fn economics_of(path: Option<&str>, agent: &str) -> Option<SessionEconomics> {
    let path = path.filter(|path| !path.is_empty())?;
    let bytes = std::fs::read(path).ok()?;
    Some(meter(split_lines(&String::from_utf8_lossy(&bytes)), agent))
}

fn summary_of(
    row: &ActivitySessionRow,
    vogt_session_id: Option<String>,
    project: Option<String>,
    reading: Option<SessionEconomics>,
) -> Value {
    let error_rate = if row.calls == 0 {
        0.0
    } else {
        (row.errors as f64 / row.calls as f64 * 10_000.0).round() / 10_000.0
    };
    json!({
        "agent": row.agent,
        "agent_session_id": row.agent_session_id,
        "vogt_session_id": vogt_session_id,
        "project": project,
        "cwd": row.cwd,
        "first_at": row.first_at.to_iso(),
        "last_at": row.last_at.to_iso(),
        "calls": row.calls,
        "errors": row.errors,
        "error_rate": error_rate,
        "tool_wait_ms": row.wait_ms,
        "unfinished": row.calls - row.finished,
        "tools": row.tools,
        "services": row.services,
        "turns": reading.as_ref().map(|reading| reading.turns),
        "context_tokens": reading.as_ref().map(|reading| reading.context_tokens),
        "peak_context_tokens": reading.as_ref().map(|reading| reading.peak_context_tokens),
        "compactions": reading.as_ref().map(|reading| reading.compactions),
    })
}

fn page(key: &str, rows: Vec<Value>, offset: i64, limit: i64, indexed_at: Option<Moment>) -> Value {
    let total = rows.len();
    let next_offset = (total as i64 == limit).then_some(offset + total as i64);
    json!({
        key: rows,
        "total": total,
        "next_offset": next_offset,
        "indexed_at": indexed_at.map(|moment| moment.to_iso()),
        "detail": Value::Null,
    })
}

fn empty(key: &str, indexed_at: Option<Moment>, detail: &str) -> Value {
    json!({
        key: [],
        "total": 0,
        "next_offset": Value::Null,
        "indexed_at": indexed_at.map(|moment| moment.to_iso()),
        "detail": detail,
    })
}

fn duration_ms(at: Moment, finished: Option<Moment>) -> Option<i64> {
    let finished = finished?;
    let millis = (finished.unix_seconds() - at.unix_seconds()) * 1000
        + (finished.nanos() as i64 - at.nanos() as i64) / 1_000_000;
    Some(millis.max(0))
}

fn trimmed(params: &Value, field: &str) -> Option<String> {
    params
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

fn flag(params: &Value, field: &str) -> Result<bool, VogtError> {
    match params.get(field) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(value)) => Ok(*value),
        Some(_) => Err(VogtError::InvalidRequest(format!(
            "{field} must be a boolean"
        ))),
    }
}

fn bounded(params: &Value, field: &str, default: i64, max: i64) -> Result<i64, VogtError> {
    let value = match params.get(field) {
        None | Some(Value::Null) => default,
        Some(Value::Number(number)) => number
            .as_i64()
            .ok_or_else(|| VogtError::InvalidRequest(format!("{field} must be an integer")))?,
        Some(_) => {
            return Err(VogtError::InvalidRequest(format!(
                "{field} must be an integer"
            )))
        }
    };
    if value < 0 {
        return Err(VogtError::InvalidRequest(format!(
            "{field} must not be negative"
        )));
    }
    Ok(value.min(max))
}

/// A moment given as an ISO-8601 string, or absent.
fn moment_of(params: &Value, field: &str) -> Result<Option<Moment>, VogtError> {
    let Some(text) = trimmed(params, field) else {
        return Ok(None);
    };
    crate::core::from_iso(&text)
        .map(Some)
        .map_err(|_| VogtError::InvalidRequest(format!("{field} must be an ISO-8601 timestamp")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::context::Built;
    use crate::application::instance;
    use crate::core::{ActorKind, Principal, StepClock, SweepOutcome};
    use crate::storage::interface::ObservedStore;
    use crate::storage::observed_types::{ActivityBatch, ActivityCall, TranscriptCursor};
    use serde_json::json;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn context(name: &str, roots: Vec<(&str, &str)>) -> (std::path::PathBuf, Built) {
        let dir = std::env::temp_dir().join(format!("vogt-activity-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut clock = None;
        let mut ids = None;
        instance::init(&dir, &mut clock, &mut ids).unwrap();
        let built = crate::application::context::build_context(
            crate::config::VogtConfig {
                data_dir: dir.clone(),
                agent_activity_roots: roots
                    .into_iter()
                    .map(|(name, path)| (name.to_string(), PathBuf::from(path)))
                    .collect::<BTreeMap<_, _>>(),
                ..crate::config::VogtConfig::default()
            },
            Some(Principal::new("local:ada", ActorKind::Human, "Ada").unwrap()),
            Some(StepClock::new(Moment::from_unix(1_700_000_000, 0))),
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        (dir, built)
    }

    fn indexed(built: &Built) {
        with_ctx_index(built);
    }

    fn with_ctx_index(built: &Built) {
        crate::with_ctx!(built, |ctx| {
            let now = Moment::from_unix(1_700_000_100, 0);
            let sweep = ctx.observed.begin_sweep(COLLECTOR_NAME, &[], now).unwrap();
            ctx.observed
                .index_activity(
                    &sweep.id,
                    &ActivityBatch {
                        calls: vec![ActivityCall {
                            source_path: "/tmp/t.jsonl".into(),
                            call_id: "c1".into(),
                            agent: "claude".into(),
                            agent_session_id: "conv-1".into(),
                            cwd: Some("/work".into()),
                            tool: "bash".into(),
                            summary: "listed files".into(),
                            services: vec!["shell".into()],
                            withheld: false,
                            at: now,
                        }],
                        ..ActivityBatch::default()
                    },
                    now,
                )
                .unwrap();
            ctx.observed
                .finish_sweep(&sweep.id, SweepOutcome::Ok, &[], now, None)
                .unwrap();
        });
    }

    #[test]
    fn an_unconfigured_index_says_why_it_is_empty() {
        let (dir, built) = context("none", vec![]);
        let result = search_agent_activity_op(&built, json!({})).unwrap();
        assert_eq!(result["events"], json!([]));
        assert_eq!(result["total"], 0);
        assert!(result["detail"].as_str().unwrap().contains("not collected"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_configured_index_that_has_not_swept_says_so() {
        let (dir, built) = context("unswept", vec![("claude", "/transcripts")]);
        let result = summarize_agent_activity_op(&built, json!({})).unwrap();
        assert_eq!(result["sessions"], json!([]));
        assert!(result["detail"]
            .as_str()
            .unwrap()
            .contains("not been indexed"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_swept_index_returns_the_call_and_its_session() {
        let (dir, built) = context("swept", vec![("claude", "/transcripts")]);
        indexed(&built);
        let events = search_agent_activity_op(&built, json!({"tool": "bash"})).unwrap();
        assert_eq!(events["total"], 1);
        assert_eq!(events["events"][0]["tool"], "bash");
        assert_eq!(events["events"][0]["agent_session_id"], "conv-1");
        assert!(events["detail"].is_null());
        let summary = summarize_agent_activity_op(&built, json!({})).unwrap();
        assert_eq!(summary["sessions"][0]["calls"], 1);
        assert_eq!(summary["sessions"][0]["unfinished"], 1);
        assert!(summary["sessions"][0]["turns"].is_null());
        assert!(summary["sessions"][0]["compactions"].is_null());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn economics_meters_the_newest_indexed_transcript() {
        let (dir, built) = context("metered", vec![("claude", "/transcripts")]);
        indexed(&built);
        let transcript = dir.join("conv-1.jsonl");
        std::fs::write(
            &transcript,
            format!(
                "{}\n{}\n",
                json!({"type": "assistant", "message": {"id": "m1",
                    "usage": {"input_tokens": 30, "output_tokens": 1}}}),
                json!({"type": "user", "isCompactSummary": true, "message": {}}),
            ),
        )
        .unwrap();
        crate::with_ctx!(&built, |ctx| {
            let now = Moment::from_unix(1_700_000_200, 0);
            let sweep = ctx.observed.begin_sweep(COLLECTOR_NAME, &[], now).unwrap();
            ctx.observed
                .index_activity(
                    &sweep.id,
                    &ActivityBatch {
                        cursors: vec![TranscriptCursor {
                            path: transcript.display().to_string(),
                            agent: "claude".into(),
                            offset: 0,
                            size: 0,
                            agent_session_id: Some("conv-1".into()),
                            cwd: Some("/work".into()),
                        }],
                        ..ActivityBatch::default()
                    },
                    now,
                )
                .unwrap();
            ctx.observed
                .finish_sweep(&sweep.id, SweepOutcome::Ok, &[], now, None)
                .unwrap();
        });
        let summary = summarize_agent_activity_op(&built, json!({"economics": true})).unwrap();
        let row = &summary["sessions"][0];
        assert_eq!(row["turns"], 1);
        assert_eq!(row["context_tokens"], 30);
        assert_eq!(row["peak_context_tokens"], 30);
        assert_eq!(row["compactions"], 1);
        let plain = summarize_agent_activity_op(&built, json!({"economics": false})).unwrap();
        assert!(plain["sessions"][0]["turns"].is_null());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn project_roots_expand_the_home_directory() {
        let (dir, built) = context("home", vec![]);
        let home = std::env::var("HOME").unwrap_or_default();
        crate::with_ctx!(&built, |ctx| {
            let mut write = crate::application::context::write_of(ctx);
            crate::application::writes::audited_write(&mut write, "seed", "test", |txn, _| {
                crate::storage::interface::WriteTxn::insert_project(
                    txn,
                    &crate::core::Project::new(
                        "prj_1",
                        "proj",
                        "Proj",
                        "~/code/proj/",
                        Moment::from_unix(1_700_000_000, 0),
                    ),
                )?;
                Ok(crate::application::writes::WriteOutcome {
                    result: json!({}),
                    entity_kind: "project".to_string(),
                    entity_id: "prj_1".to_string(),
                    payload: json!({}),
                    event_kind: "project.registered".to_string(),
                    summary: json!({}),
                })
            })
            .unwrap();
            let roots = project_roots(ctx).unwrap();
            assert_eq!(
                roots,
                vec![(format!("{home}/code/proj"), "proj".to_string())]
            );
            let scope = scope_of(ctx, &json!({"project": "proj"})).unwrap();
            assert_eq!(scope.cwd_roots, Some(vec![format!("{home}/code/proj")]));
        });
        let _ = std::fs::remove_dir_all(&dir);
    }
}
