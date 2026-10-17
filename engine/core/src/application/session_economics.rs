//! `session.economics`: what the session's agent transcript cost, read whole.
//! Ports `economics` in `sessions.py`.
//!
//! The same finding as `session.last_reply`, but the file is metered rather
//! than tailed. When the engine row already carries an economics reading, that
//! answers: re-reading the file would repeat the tailer's work and could
//! disagree with the number the session list shows. Cost stays null (no price
//! table) and the worktree is not measured, so `commits` and `files_changed`
//! stay null rather than claiming a clean tree.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Map, Value};

use crate::adapters::engine::EngineSession;
use crate::adapters::transcripts::{self, Transcript};
use crate::application::context::{AppContext, Built};
use crate::application::services::sessions::resolve_target;
use crate::core::session_economics::{meter, split_lines, SessionEconomics, TurnTiming};
use crate::core::{from_iso, py_repr, Clock, IdFactory};
use crate::errors::VogtError;

const NO_TRANSCRIPT: &str = "no agent transcript found for this session \
     (a plain shell, an agent started by hand in another directory, or \
     transcripts this process cannot read)";

pub(crate) fn economics_op(ctx: &Built, params: Value) -> Result<Value, VogtError> {
    let id = params
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    crate::with_ctx!(ctx, |ctx| economics_in(ctx, &id))
}

/// Where the numbers came from, beside the numbers.
#[derive(Default)]
struct Found<'a> {
    agent: Option<&'a str>,
    conversation_id: Option<&'a str>,
    basis: Option<&'a str>,
    transcript: Option<&'a Transcript>,
    detail: Option<&'a str>,
}

fn economics_in<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    id: &str,
) -> Result<Value, VogtError> {
    let target = resolve_target(ctx, id)?;
    let engine_id = target.engine_session_id.as_str();
    let live = match ctx.engine.as_ref() {
        Some(engine) => match engine.get_session(engine_id) {
            Ok(found) => found,
            Err(VogtError::EngineUnavailable(_)) => None,
            Err(error) => return Err(error),
        },
        None => None,
    };
    let declared = target.session.as_ref();
    if live.is_none() && declared.is_none() {
        return Err(VogtError::NotFound(format!("no session {}", py_repr(id))));
    }
    let live = live.as_ref();
    let answer = |reading: Option<&SessionEconomics>, found: Found| {
        result(id, engine_id, live, reading, found)
    };
    if let Some(reported) = live
        .and_then(|session| session.economics.as_ref())
        .filter(|reported| !reported.is_empty())
    {
        let session = live.expect("economics came from the live row");
        return Ok(answer(
            Some(&reported_reading(reported)),
            Found {
                agent: session.conversation_agent.as_deref(),
                conversation_id: session.conversation_id.as_deref(),
                basis: Some("engine"),
                ..Found::default()
            },
        ));
    }
    if live.and_then(|session| session.conversation_agent.as_deref()) == Some("opencode") {
        return Ok(answer(
            None,
            Found {
                agent: Some("opencode"),
                basis: Some("engine-id"),
                detail: Some("opencode transcripts are not metered"),
                ..Found::default()
            },
        ));
    }
    let roots = &ctx.config.session_transcript_roots;
    if roots.is_empty() {
        return Ok(answer(
            None,
            Found {
                detail: Some("session_transcript_roots is empty: reading is off"),
                ..Found::default()
            },
        ));
    }
    let started = declared.map(|row| row.started_at).or_else(|| {
        live.and_then(|session| session.created_at.as_deref())
            .filter(|text| !text.is_empty())
            .and_then(|text| from_iso(&text.replace('Z', "+00:00")).ok())
    });
    let roots: HashMap<&str, &Path> = roots
        .iter()
        .map(|(agent, root)| (agent.as_str(), root.as_path()))
        .collect();
    let found = transcripts::find(
        &roots,
        engine_id,
        live.and_then(|session| session.command.as_deref()),
        declared.and_then(|row| row.template.as_deref()),
        live.map(|session| session.cwd.as_str())
            .filter(|cwd| !cwd.is_empty())
            .or_else(|| declared.map(|row| row.cwd.as_str())),
        started.map(|moment| UNIX_EPOCH + Duration::from_secs(moment.unix_seconds().max(0) as u64))
            as Option<SystemTime>,
        true,
    );
    let Some(transcript) = found else {
        return Ok(answer(
            None,
            Found {
                detail: Some(NO_TRANSCRIPT),
                ..Found::default()
            },
        ));
    };
    let located = |detail| Found {
        agent: Some(transcript.agent.as_str()),
        conversation_id: Some(transcript.conversation_id.as_str()),
        basis: Some(transcript.basis.as_str()),
        transcript: Some(&transcript),
        detail,
    };
    let Ok(bytes) = std::fs::read(&transcript.path) else {
        return Ok(answer(
            None,
            located(Some("the transcript could not be read")),
        ));
    };
    let text = String::from_utf8_lossy(&bytes);
    let reading = meter(split_lines(&text), &transcript.agent);
    Ok(answer(Some(&reading), located(None)))
}

/// The engine's reading as numbers. A counter that is not an integer (a bool,
/// a float, a string) is zero, as `_int_of` makes it.
fn reported_reading(reported: &Map<String, Value>) -> SessionEconomics {
    let int = |value: Option<&Value>| value.and_then(Value::as_i64).unwrap_or(0);
    let wall = reported.get("wall_clock").and_then(Value::as_object);
    SessionEconomics {
        turns: int(reported.get("turns")),
        context_tokens: int(reported.get("context_tokens")),
        peak_context_tokens: int(reported.get("peak_context_tokens")),
        compactions: int(reported.get("compactions")),
        tool_result_bytes: reported
            .get("tool_result_bytes")
            .and_then(Value::as_object)
            .map(|tools| {
                tools
                    .iter()
                    .map(|(name, value)| (name.clone(), int(Some(value))))
                    .collect::<BTreeMap<_, _>>()
            })
            .unwrap_or_default(),
        reads_repeated: int(reported.get("reads_repeated")),
        wall_clock: TurnTiming {
            count: int(wall.and_then(|wall| wall.get("count"))),
            max_ms: int(wall.and_then(|wall| wall.get("max_ms"))),
            mean_ms: int(wall.and_then(|wall| wall.get("mean_ms"))),
        },
        output_tokens: int(reported.get("output_tokens")),
        usage_measured: reported.get("usage_measured") == Some(&Value::Bool(true)),
    }
}

fn result(
    id: &str,
    engine_id: &str,
    live: Option<&EngineSession>,
    reading: Option<&SessionEconomics>,
    found: Found,
) -> Value {
    // Unread, the counts are the empty reading and the line stays empty: not
    // a claim that the session did nothing.
    let empty = SessionEconomics::default();
    let budget_line = reading.map(|reading| reading.budget_line(None, None));
    let reading = reading.unwrap_or(&empty);
    json!({
        "id": id,
        "engine_session_id": engine_id,
        "agent": found.agent,
        "account": live.and_then(|session| session.account.as_deref()),
        "conversation_id": found.conversation_id,
        "basis": found.basis,
        "transcript": found.transcript.map(|t| t.path.display().to_string()),
        "turns": reading.turns,
        "context_tokens": reading.context_tokens,
        "peak_context_tokens": reading.peak_context_tokens,
        "compactions": reading.compactions,
        "tool_result_bytes": reading.tool_result_bytes,
        "reads_repeated": reading.reads_repeated,
        "wall_clock_count": reading.wall_clock.count,
        "wall_clock_max_ms": reading.wall_clock.max_ms,
        "wall_clock_mean_ms": reading.wall_clock.mean_ms,
        "output_tokens": reading.output_tokens,
        "usage_measured": reading.usage_measured,
        "commits": null,
        "files_changed": null,
        "cost_usd": null,
        "budget_line": budget_line.unwrap_or_default(),
        "detail": found.detail,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use std::fs;
    use std::path::PathBuf;

    use serde_json::{json, Value};

    use crate::adapters::engine::EngineClient;
    use crate::application::context::{build_context, Built};
    use crate::application::writes::{audited_write, WriteOutcome};
    use crate::core::{ActorKind, CodingSession, Moment, Principal, Project, StepClock};
    use crate::errors::VogtError;
    use crate::registry::default_registry;
    use crate::storage::interface::WriteTxn;

    pub(crate) fn built(
        name: &str,
        roots: Vec<(String, PathBuf)>,
        engine: Option<EngineClient>,
    ) -> Built {
        let dir = std::env::temp_dir().join(format!("vogt-econ-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let mut clock = None;
        let mut ids = None;
        crate::application::instance::init(&dir, &mut clock, &mut ids).unwrap();
        let mut config = crate::config::VogtConfig {
            data_dir: dir,
            ..crate::config::VogtConfig::default()
        };
        config.session_transcript_roots = roots.into_iter().collect();
        build_context(
            config,
            Some(Principal::new("local:ada", ActorKind::Human, "Ada").unwrap()),
            Some(StepClock::new(Moment::from_unix(1_700_000_000, 0))),
            None,
            None,
            engine,
            None,
            None,
        )
        .unwrap()
    }

    /// An engine whose every request answers `row`.
    pub(crate) fn engine_row(row: Value) -> EngineClient {
        EngineClient::new(
            "http://engine.test",
            None,
            Some(std::sync::Arc::new(move |_path, _, _body, _method| {
                (200, serde_json::to_vec(&row).unwrap())
            })),
        )
    }

    pub(crate) fn seed(built: &Built) {
        crate::with_ctx!(built, |ctx| {
            let mut write = crate::application::context::write_of(ctx);
            audited_write(&mut write, "seed", "test", |txn, actor| {
                txn.insert_project(&Project::new(
                    "prj_1",
                    "proj",
                    "Proj",
                    "/work",
                    Moment::from_unix(1_700_000_000, 0),
                ))?;
                txn.insert_session(&CodingSession {
                    id: "ses_0001".to_string(),
                    engine_session_id: "eng-1".to_string(),
                    project_id: "prj_1".to_string(),
                    work_item_id: None,
                    actor_id: actor.id.clone(),
                    cwd: "/work".to_string(),
                    template: Some("claude".to_string()),
                    model: None,
                    effort: None,
                    reason: "test".to_string(),
                    started_at: Moment::from_unix(1_700_000_000, 0),
                    stopped_at: None,
                })?;
                Ok(WriteOutcome {
                    result: json!({}),
                    entity_kind: "session".to_string(),
                    entity_id: "ses_0001".to_string(),
                    payload: json!({}),
                    event_kind: "session.started".to_string(),
                    summary: json!({}),
                })
            })
        })
        .unwrap();
    }

    pub(crate) fn run(built: &Built, op: &str, params: Value) -> Result<Value, VogtError> {
        default_registry()
            .get(op)
            .expect("operation is registered")
            .run(Some(built), params)
    }

    #[test]
    fn the_transcript_is_metered_whole() {
        let dir =
            std::env::temp_dir().join(format!("vogt-econ-transcripts-{}", std::process::id()));
        let root = dir.join("claude").join("proj");
        fs::create_dir_all(&root).unwrap();
        let path = root.join("eng-1.jsonl");
        let line = |id: &str, input: i64| {
            json!({"type": "assistant", "message": {"id": id, "usage": {
                "input_tokens": input, "output_tokens": 3}}})
            .to_string()
        };
        fs::write(
            &path,
            format!("{}\n{}\nnot json\n", line("m1", 40), line("m2", 25)),
        )
        .unwrap();
        let engine = engine_row(json!({"id": "eng-1", "alive": true, "cwd": "/work",
            "command": "claude", "account": "work"}));
        let built = built(
            "file",
            vec![("claude".to_string(), dir.join("claude"))],
            Some(engine),
        );
        seed(&built);
        let result = run(&built, "session.economics", json!({"id": "ses_0001"})).unwrap();
        assert_eq!(result["engine_session_id"], "eng-1");
        assert_eq!(result["basis"], "engine-id");
        assert_eq!(result["agent"], "claude");
        assert_eq!(result["account"], "work");
        assert_eq!(result["transcript"], path.display().to_string());
        assert_eq!(result["turns"], 2);
        assert_eq!(result["context_tokens"], 25);
        assert_eq!(result["peak_context_tokens"], 40);
        assert_eq!(result["output_tokens"], 6);
        assert_eq!(result["usage_measured"], true);
        assert_eq!(result["budget_line"], "2 turns");
        assert!(result["cost_usd"].is_null());
        assert!(result["detail"].is_null());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_engine_reading_answers_without_a_file() {
        let engine = engine_row(json!({
            "id": "eng-1", "alive": true,
            "conversation": {"agent": "claude", "id": "conv-1"},
            "economics": {"turns": 7, "context_tokens": 10, "compactions": 2,
                "tool_result_bytes": {"Read": 12, "Bash": true},
                "wall_clock": {"count": 3, "max_ms": 9, "mean_ms": 4},
                "usage_measured": true, "output_tokens": 1.5}
        }));
        let built = built("engine", Vec::new(), Some(engine));
        seed(&built);
        let result = run(&built, "session.economics", json!({"id": "eng-1"})).unwrap();
        assert_eq!(result["basis"], "engine");
        assert_eq!(result["conversation_id"], "conv-1");
        assert_eq!(result["turns"], 7);
        assert_eq!(result["tool_result_bytes"], json!({"Bash": 0, "Read": 12}));
        assert_eq!(result["wall_clock_count"], 3);
        assert_eq!(result["output_tokens"], 0);
        assert_eq!(result["budget_line"], "7 turns, 2 compactions");
        assert!(result["transcript"].is_null());
    }

    #[test]
    fn unread_sessions_say_why_with_an_empty_line() {
        let off = built("off", Vec::new(), None);
        seed(&off);
        let result = run(&off, "session.economics", json!({"id": "ses_0001"})).unwrap();
        assert_eq!(
            result["detail"],
            "session_transcript_roots is empty: reading is off"
        );
        assert_eq!(result["turns"], 0);
        assert_eq!(result["budget_line"], "");
        let opencode = engine_row(json!({"id": "eng-1", "alive": true,
            "conversation": {"agent": "opencode", "id": "oc"}}));
        let other = built("opencode", Vec::new(), Some(opencode));
        seed(&other);
        let result = run(&other, "session.economics", json!({"id": "ses_0001"})).unwrap();
        assert_eq!(result["agent"], "opencode");
        assert_eq!(result["detail"], "opencode transcripts are not metered");
    }

    #[test]
    fn an_unknown_session_is_not_found() {
        let built = built("missing", Vec::new(), None);
        let error = run(&built, "session.economics", json!({"id": "eng-x"})).unwrap_err();
        assert_eq!(error.to_string(), "no session 'eng-x'");
    }
}
