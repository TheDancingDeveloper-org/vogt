//! Read-only session views that do not live in `services::sessions`.
//!
//! `session.last_reply` ports `sessions.py`'s `last_reply`: an opencode
//! conversation is answered from the engine's reply log, and every other
//! agent from the transcript files under `session_transcript_roots`.
//! `session.history_list` and `session.search_output` are thin reads of the
//! engine's history: no engine, or one that cannot be reached, sets `engine`
//! and returns an empty view rather than an error that reads as "no history".

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use crate::adapters::engine::EngineSession;
use crate::adapters::transcripts::{self, Reply, Transcript};
use crate::application::context::{AppContext, Built};
use crate::application::services::resolve_session;
use crate::core::{from_iso, py_repr, Clock, CodingSession, IdFactory, Moment};
use crate::errors::VogtError;
use crate::storage::interface::{DeclaredStore, ReadView};

const OPENCODE: &str = "opencode";
const NO_ENGINE: &str = "no session engine is configured (VOGT_ENGINE_URL is unset)";
const NO_TRANSCRIPT: &str = "no agent transcript found for this session \
     (a plain shell, an agent started by hand in another directory, or \
     transcripts this process cannot read)";

pub(crate) fn last_reply(ctx: &Built, params: Value) -> Result<Value, VogtError> {
    let id = text(params.get("id")).ok_or_else(|| {
        VogtError::InvalidRequest(
            "a session id is required (ses_… or the engine's session UUID)".into(),
        )
    })?;
    let n = params.get("n").and_then(Value::as_u64).unwrap_or(1) as usize;
    crate::with_ctx!(ctx, |ctx| last_reply_in(ctx, id, n))
}

fn last_reply_in<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    id: &str,
    n: usize,
) -> Result<Value, VogtError> {
    let engine_id = resolve_session(ctx, id)?;
    let declared = ctx.declared.read()?.session_by_engine_id(&engine_id)?;
    let live = match ctx.engine.as_ref() {
        Some(engine) => match engine.get_session(&engine_id) {
            Ok(found) => found,
            Err(VogtError::EngineUnavailable(_)) => None,
            Err(error) => return Err(error),
        },
        None => None,
    };
    if live.is_none() && declared.is_none() {
        return Err(VogtError::NotFound(format!("no session {}", py_repr(id))));
    }
    let agent = live
        .as_ref()
        .and_then(|session| session.conversation_agent.clone())
        .filter(|agent| !agent.is_empty());
    if agent.as_deref() == Some(OPENCODE) {
        if let Some(answered) = opencode_replies(ctx, id, &engine_id, n)? {
            return Ok(answered);
        }
    }
    file_replies(ctx, id, &engine_id, live.as_ref(), declared.as_ref(), n)
}

/// Opencode keeps no transcript file. `None` means the engine has no reply
/// log for it, or could not be asked, so the file path answers instead.
fn opencode_replies<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    given: &str,
    engine_id: &str,
    n: usize,
) -> Result<Option<Value>, VogtError> {
    let replies = match ctx.engine.as_ref() {
        Some(engine) => match engine.session_replies(engine_id, n as i64) {
            Ok(found) => found,
            Err(VogtError::EngineUnavailable(_)) => None,
            Err(error) => return Err(error),
        },
        None => None,
    };
    let Some((conversation_id, replies)) = replies else {
        return Ok(None);
    };
    let messages: Vec<Reply> = replies
        .into_iter()
        .rev()
        .take(n)
        .map(|(text, at)| transcripts::reply(&text, at))
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    Ok(Some(answer(
        given,
        engine_id,
        Some(OPENCODE),
        conversation_id.as_deref(),
        Some("engine-id"),
        None,
        &messages,
        messages
            .is_empty()
            .then_some("opencode has no assistant reply yet"),
    )))
}

fn file_replies<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    given: &str,
    engine_id: &str,
    live: Option<&EngineSession>,
    declared: Option<&CodingSession>,
    n: usize,
) -> Result<Value, VogtError> {
    let roots = &ctx.config.session_transcript_roots;
    if roots.is_empty() {
        return Ok(answer(
            given,
            engine_id,
            None,
            None,
            None,
            None,
            &[],
            Some("session_transcript_roots is empty: reading is off"),
        ));
    }
    let started = declared
        .map(|row| row.started_at)
        .or_else(|| parse_created(live.and_then(|session| session.created_at.as_deref())));
    let found = transcripts::find(
        &roots
            .iter()
            .map(|(agent, root)| (agent.as_str(), root.as_path()))
            .collect(),
        engine_id,
        live.and_then(|session| session.command.as_deref()),
        declared.and_then(|row| row.template.as_deref()),
        live.map(|session| session.cwd.as_str())
            .or_else(|| declared.map(|row| row.cwd.as_str())),
        started.map(system_time),
        true,
    );
    let Some(transcript) = found else {
        return Ok(answer(
            given,
            engine_id,
            None,
            None,
            None,
            None,
            &[],
            Some(NO_TRANSCRIPT),
        ));
    };
    let messages = transcripts::last_replies(&transcript, n);
    let detail = messages
        .is_empty()
        .then_some("the transcript has no assistant reply yet");
    Ok(answer(
        given,
        engine_id,
        Some(transcript.agent.as_str()),
        Some(transcript.conversation_id.as_str()),
        Some(transcript.basis.as_str()),
        Some(&transcript),
        &messages,
        detail,
    ))
}

#[allow(clippy::too_many_arguments)]
fn answer(
    given: &str,
    engine_id: &str,
    agent: Option<&str>,
    conversation_id: Option<&str>,
    basis: Option<&str>,
    transcript: Option<&Transcript>,
    messages: &[Reply],
    detail: Option<&str>,
) -> Value {
    json!({
        "id": given,
        "engine_session_id": engine_id,
        "agent": agent,
        "conversation_id": conversation_id,
        "basis": basis,
        "transcript": transcript.map(|found| found.path.display().to_string()),
        "messages": messages
            .iter()
            .map(|reply| json!({"text": reply.text, "at": reply.at}))
            .collect::<Vec<_>>(),
        "detail": detail,
    })
}

/// An engine `created_at` that is absent or not an instant is no start time,
/// so the cwd guess is not bounded by it.
fn parse_created(text: Option<&str>) -> Option<Moment> {
    let text = text.filter(|text| !text.is_empty())?;
    from_iso(text).ok()
}

fn system_time(moment: Moment) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(moment.unix_seconds().max(0) as u64)
}

fn text(value: Option<&Value>) -> Option<&str> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

/// The engine's archived-session listing, newest first, paginated.
pub(crate) fn history_list(ctx: &Built, params: Value) -> Result<Value, VogtError> {
    let limit = params.get("limit").and_then(Value::as_i64).unwrap_or(50);
    let offset = params.get("offset").and_then(Value::as_i64).unwrap_or(0);
    crate::with_ctx!(ctx, |ctx| history_list_in(ctx, limit, offset))
}

fn history_list_in<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    limit: i64,
    offset: i64,
) -> Result<Value, VogtError> {
    let (rows, engine) = match ctx.engine.as_ref() {
        None => (Vec::new(), Some(NO_ENGINE.to_string())),
        Some(client) => match client.history_sessions(limit, offset) {
            Ok(rows) => (rows, None),
            Err(VogtError::EngineUnavailable(text)) => (Vec::new(), Some(text)),
            Err(error) => return Err(error),
        },
    };
    Ok(json!({
        "sessions": rows.iter().map(|row| json!({
            "id": row.id,
            "name": row.name,
            "created_at": row.created_at,
            "ended_at": row.ended_at,
            "exit_code": row.exit_code,
            "cwd": row.cwd,
            "command": row.command,
            "scrollback_bytes": row.scrollback_bytes,
            "template": row.template,
            "role": row.role,
            "conversation_agent": row.conversation_agent,
            "conversation_id": row.conversation_id,
            "work_item": row.work_item,
            "resume_template": row.resume_template,
        })).collect::<Vec<_>>(),
        "engine": engine,
    }))
}

/// Full-text search over session output, live sessions included.
pub(crate) fn search_output(ctx: &Built, params: Value) -> Result<Value, VogtError> {
    let query =
        text(params.get("q")).ok_or_else(|| VogtError::InvalidRequest("q is required".into()))?;
    let limit = params.get("limit").and_then(Value::as_i64).unwrap_or(20);
    let include_live = params
        .get("include_live")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    crate::with_ctx!(ctx, |ctx| {
        search_output_in(ctx, query, limit, include_live)
    })
}

fn search_output_in<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    query: &str,
    limit: i64,
    include_live: bool,
) -> Result<Value, VogtError> {
    let (hits, engine) = match ctx.engine.as_ref() {
        None => (Vec::new(), Some(NO_ENGINE.to_string())),
        Some(client) => match client.search_history(query, limit, include_live) {
            Ok(hits) => (hits, None),
            Err(VogtError::EngineUnavailable(text)) => (Vec::new(), Some(text)),
            Err(error) => return Err(error),
        },
    };
    Ok(json!({
        "matches": hits.iter().map(|hit| json!({
            "session_id": hit.session_id,
            "session_name": hit.session_name,
            "created_at": hit.created_at,
            "match_snippet": hit.match_snippet,
            "rank": hit.rank,
            "live": hit.live,
        })).collect::<Vec<_>>(),
        "engine": engine,
    }))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::{json, Value};

    use crate::adapters::engine::EngineClient;
    use crate::application::context::build_context;
    use crate::application::writes::audited_write;
    use crate::core::{ActorKind, CodingSession, Moment, Principal, Project, StepClock};
    use crate::errors::VogtError;
    use crate::registry::default_registry;
    use crate::storage::interface::WriteTxn;

    fn built(
        name: &str,
        roots: Vec<(String, std::path::PathBuf)>,
        engine: Option<EngineClient>,
    ) -> crate::application::context::Built {
        let dir = std::env::temp_dir().join(format!("vogt-reply-{name}-{}", std::process::id()));
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

    fn engine(
        handler: impl Fn(&str, &str, &[u8]) -> (u16, Vec<u8>) + Send + Sync + 'static,
    ) -> EngineClient {
        EngineClient::new(
            "http://engine.test",
            None,
            Some(std::sync::Arc::new(move |path, _, body, method| {
                handler(path, method, body)
            })),
        )
    }

    fn seed(built: &crate::application::context::Built) {
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
                    purpose: None,
                    started_by_session: None,
                    started_by_kind: None,
                    started_at: Moment::from_unix(1_700_000_000, 0),
                    stopped_at: None,
                })?;
                Ok(crate::application::writes::WriteOutcome {
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

    fn ask(built: &crate::application::context::Built, params: Value) -> Result<Value, VogtError> {
        default_registry()
            .get("session.last_reply")
            .expect("session.last_reply is registered")
            .run(Some(built), params)
    }

    fn ask_history(
        built: &crate::application::context::Built,
        params: Value,
    ) -> Result<Value, VogtError> {
        default_registry()
            .get("session.history_list")
            .expect("session.history_list is registered")
            .run(Some(built), params)
    }

    fn ask_search(
        built: &crate::application::context::Built,
        params: Value,
    ) -> Result<Value, VogtError> {
        default_registry()
            .get("session.search_output")
            .expect("session.search_output is registered")
            .run(Some(built), params)
    }

    fn history_engine_body(path: &str) -> (u16, Vec<u8>) {
        if path.contains("/api/history/sessions") {
            return (
                200,
                serde_json::to_vec(&json!([
                    {
                        "id": "hist-1", "name": "old-session",
                        "created_at": "2026-01-01T00:00:00Z",
                        "ended_at": "2026-01-01T00:05:00Z", "exit_code": 0,
                        "cwd": "/work", "command": "make test", "scrollback_bytes": 128
                    },
                    {
                        "id": "hist-2", "name": "Oversight",
                        "created_at": "2026-01-01T00:00:00Z",
                        "command": "/usr/local/bin/vogt-agent-auth shell",
                        "role": "oversight", "conversation_agent": "claude",
                        "conversation_id": "6c1f0d2e-5b7a-4e1c-9f3d-2a8b7c6d5e4f",
                        "resume_template": "claude", "work_item": "WI-7"
                    }
                ]))
                .unwrap(),
            );
        }
        if path.contains("/api/history/search") {
            return (
                200,
                serde_json::to_vec(&json!([
                    {
                        "session_id": "hist-1", "session_name": "old-session",
                        "created_at": "2026-01-01T00:00:00Z",
                        "match_snippet": "found the needle here",
                        "rank": -2.0, "live": false
                    },
                    {
                        "session_id": "live-9", "session_name": "running",
                        "created_at": "2026-01-02T00:00:00Z",
                        "match_snippet": "needle in the live output",
                        "rank": -1.0, "live": true
                    }
                ]))
                .unwrap(),
            );
        }
        (404, Vec::new())
    }

    #[test]
    fn history_list_maps_rows_and_forwards_pagination() {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let record = seen.clone();
        let client = engine(move |path, _, _| {
            *record.lock().unwrap() = path.to_string();
            history_engine_body(path)
        });
        let built = built("history", Vec::new(), Some(client));
        let result = ask_history(&built, json!({"limit": 10, "offset": 5})).unwrap();
        assert!(result["engine"].is_null());
        assert_eq!(result["sessions"][0]["id"], "hist-1");
        assert_eq!(result["sessions"][0]["name"], "old-session");
        assert_eq!(result["sessions"][0]["exit_code"], 0);
        assert_eq!(result["sessions"][0]["scrollback_bytes"], 128);
        assert!(result["sessions"][0]["conversation_id"].is_null());
        assert!(result["sessions"][0]["role"].is_null());
        assert!(result["sessions"][0]["work_item"].is_null());
        assert_eq!(result["sessions"][1]["role"], "oversight");
        assert_eq!(result["sessions"][1]["conversation_agent"], "claude");
        assert_eq!(
            result["sessions"][1]["conversation_id"],
            "6c1f0d2e-5b7a-4e1c-9f3d-2a8b7c6d5e4f"
        );
        assert_eq!(result["sessions"][1]["resume_template"], "claude");
        assert!(result["sessions"][1]["template"].is_null());
        assert_eq!(result["sessions"][1]["work_item"], "WI-7");
        let path = seen.lock().unwrap().clone();
        assert!(path.contains("limit=10"), "{path}");
        assert!(path.contains("offset=5"), "{path}");
    }

    #[test]
    fn search_output_maps_hits_and_flags_live() {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let record = seen.clone();
        let client = engine(move |path, _, _| {
            *record.lock().unwrap() = path.to_string();
            history_engine_body(path)
        });
        let built = built("search", Vec::new(), Some(client));
        let result = ask_search(&built, json!({"q": "needle", "limit": 25})).unwrap();
        assert!(result["engine"].is_null());
        assert_eq!(result["matches"][0]["live"], false);
        assert_eq!(result["matches"][1]["live"], true);
        assert_eq!(
            result["matches"][0]["match_snippet"],
            "found the needle here"
        );
        assert_eq!(result["matches"][1]["session_id"], "live-9");
        let path = seen.lock().unwrap().clone();
        assert!(path.contains("q=needle"), "{path}");
        assert!(path.contains("include_live=true"), "{path}");
        assert!(path.contains("limit=25"), "{path}");
    }

    #[test]
    fn search_output_can_ask_for_archive_only() {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let record = seen.clone();
        let client = engine(move |path, _, _| {
            *record.lock().unwrap() = path.to_string();
            history_engine_body(path)
        });
        let built = built("archive", Vec::new(), Some(client));
        ask_search(&built, json!({"q": "needle", "include_live": false})).unwrap();
        let path = seen.lock().unwrap().clone();
        assert!(path.contains("include_live=false"), "{path}");
    }

    #[test]
    fn history_reads_report_no_engine_configured() {
        let built = built("noengine", Vec::new(), None);
        let listed = ask_history(&built, json!({})).unwrap();
        assert_eq!(listed["sessions"], json!([]));
        assert!(listed["engine"]
            .as_str()
            .unwrap()
            .contains("VOGT_ENGINE_URL"));
        let searched = ask_search(&built, json!({"q": "x"})).unwrap();
        assert_eq!(searched["matches"], json!([]));
        assert!(searched["engine"]
            .as_str()
            .unwrap()
            .contains("VOGT_ENGINE_URL"));
    }

    #[test]
    fn history_reads_report_a_dead_engine_not_an_error() {
        let client = EngineClient::new("http://127.0.0.1:9", None, None);
        let built = built("dead", Vec::new(), Some(client));
        let listed = ask_history(&built, json!({})).unwrap();
        assert_eq!(listed["sessions"], json!([]));
        assert!(listed["engine"].as_str().unwrap().contains("not answering"));
        let searched = ask_search(&built, json!({"q": "x"})).unwrap();
        assert_eq!(searched["matches"], json!([]));
        assert!(searched["engine"]
            .as_str()
            .unwrap()
            .contains("not answering"));
    }

    #[test]
    fn history_ranges_are_refused() {
        let built = built("ranges", Vec::new(), None);
        assert!(matches!(
            ask_history(&built, json!({"limit": 0})).unwrap_err(),
            VogtError::InvalidRequest(_)
        ));
        assert!(matches!(
            ask_search(&built, json!({})).unwrap_err(),
            VogtError::InvalidRequest(_)
        ));
    }

    /// A token shape long enough for the agent-activity redactor (`sk-` needs
    /// sixteen characters after it), assembled so the source holds no secret.
    fn secret() -> String {
        format!("{}{}", "sk-", "abcdefghijklmnop")
    }

    #[test]
    fn an_unknown_session_is_not_found() {
        let built = built("missing", Vec::new(), None);
        let error = ask(&built, json!({"id": "ses_missing"})).unwrap_err();
        assert_eq!(error.to_string(), "no session 'ses_missing'");
    }

    #[test]
    fn a_blank_id_is_refused() {
        let built = built("blank", Vec::new(), None);
        let error = ask(&built, json!({"id": "   "})).unwrap_err();
        assert_eq!(
            error.to_string(),
            "a session id is required (ses_… or the engine's session UUID)"
        );
    }

    #[test]
    fn n_outside_the_range_is_refused() {
        let built = built("range", Vec::new(), None);
        let error = ask(&built, json!({"id": "ses_0001", "n": 0})).unwrap_err();
        assert!(matches!(error, VogtError::InvalidRequest(_)));
    }

    #[test]
    fn opencode_replies_come_from_the_engine() {
        let engine = engine(|path, method, _| {
            if method == "GET" && path.contains("/replies") {
                return (
                    200,
                    serde_json::to_vec(&json!({
                        "conversation_id": "oc-1",
                        "replies": [
                            {"text": format!("bearer {}", secret()), "at": "2024-01-02T03:04:05Z"},
                            {"text": "later", "at": null}
                        ]
                    }))
                    .unwrap(),
                );
            }
            (
                200,
                serde_json::to_vec(&json!({
                    "id": "eng-1",
                    "alive": true,
                    "conversation": {"agent": "opencode", "id": "oc-1"}
                }))
                .unwrap(),
            )
        });
        let built = built("opencode", Vec::new(), Some(engine));
        seed(&built);
        let result = ask(&built, json!({"id": "eng-1", "n": 2})).unwrap();
        assert_eq!(result["basis"], "engine-id");
        assert_eq!(result["conversation_id"], "oc-1");
        assert_eq!(result["agent"], "opencode");
        assert!(result["detail"].is_null());
        let text = result["messages"][0]["text"].as_str().unwrap();
        assert!(text.contains("[REDACTED:token]"), "{text}");
        assert!(!text.contains(&secret()), "{text}");
        assert_eq!(result["messages"][1]["text"], "later");
        assert_eq!(result["id"], "eng-1");
    }

    #[test]
    fn a_transcript_is_read_by_the_engine_session_id() {
        let dir = std::env::temp_dir().join(format!("vogt-reply-files-{}", std::process::id()));
        let root = dir.join("claude").join("proj");
        fs::create_dir_all(&root).unwrap();
        let path = root.join("eng-1.jsonl");
        fs::write(
            &path,
            "{\"type\":\"assistant\",\"timestamp\":\"2024-01-02T03:04:05Z\",\"message\":{\"id\":\"m1\",\"content\":[{\"type\":\"text\",\"text\":\"hello from the file\"}]}}\n",
        )
        .unwrap();
        let engine = engine(|_path, _method, _| {
            (
                200,
                serde_json::to_vec(&json!({
                    "id": "eng-1",
                    "alive": true,
                    "conversation_agent": "claude",
                    "cwd": "/work",
                    "command": "claude"
                }))
                .unwrap(),
            )
        });
        let built = built(
            "file",
            vec![("claude".to_string(), dir.join("claude"))],
            Some(engine),
        );
        seed(&built);
        let result = ask(&built, json!({"id": "ses_0001"})).unwrap();
        assert_eq!(result["basis"], "engine-id");
        assert_eq!(result["agent"], "claude");
        assert_eq!(result["conversation_id"], "eng-1");
        assert_eq!(result["messages"][0]["text"], "hello from the file");
        assert_eq!(result["messages"][0]["at"], "2024-01-02T03:04:05Z");
        assert_eq!(result["transcript"], path.display().to_string());
        assert!(result["detail"].is_null());
        assert_eq!(result["messages"].as_array().unwrap().len(), 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_roots_are_not_searched() {
        let built = built("empty", Vec::new(), None);
        seed(&built);
        let result = ask(&built, json!({"id": "ses_0001"})).unwrap();
        assert!(result["basis"].is_null());
        assert_eq!(
            result["detail"],
            "session_transcript_roots is empty: reading is off"
        );
        assert_eq!(result["messages"], json!([]));
    }

    #[test]
    fn no_transcript_says_so() {
        let dir = std::env::temp_dir().join(format!("vogt-reply-none-{}", std::process::id()));
        fs::create_dir_all(dir.join("claude")).unwrap();
        let built = built(
            "none",
            vec![("claude".to_string(), dir.join("claude"))],
            None,
        );
        seed(&built);
        let result = ask(&built, json!({"id": "ses_0001"})).unwrap();
        assert!(result["basis"].is_null());
        assert!(result["detail"]
            .as_str()
            .unwrap()
            .starts_with("no agent transcript found"));
        let _ = fs::remove_dir_all(&dir);
    }

    /// One Claude Code assistant line, with the model the turn ran on when given.
    fn claude_line(message_id: &str, text: &str, model: Option<&str>) -> String {
        let mut message = json!({
            "id": message_id,
            "content": [{"type": "text", "text": text}],
        });
        if let Some(model) = model {
            message["model"] = json!(model);
        }
        json!({
            "type": "assistant",
            "timestamp": "2024-01-02T03:04:05Z",
            "message": message,
        })
        .to_string()
    }

    /// The engine as the list needs it: one live Claude session in `/work`,
    /// answering both the list and the single-session reads.
    fn live_claude_engine() -> EngineClient {
        let summary = json!({
            "id": "eng-1",
            "name": "agent",
            "activity": "idle",
            "alive": true,
            "cwd": "/work",
            "command": "claude",
            "conversation_agent": "claude",
        });
        engine(move |path, _, _| {
            let body = if path.ends_with("/api/sessions") {
                json!([summary])
            } else {
                summary.clone()
            };
            (200, serde_json::to_vec(&body).unwrap())
        })
    }

    /// `eng-1.jsonl` under a Claude root, the way a session's transcript is kept.
    fn write_transcript(dir: &std::path::Path, lines: &[String]) {
        let root = dir.join("claude").join("proj");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("eng-1.jsonl"), lines.join("\n") + "\n").unwrap();
    }

    fn list(built: &crate::application::context::Built) -> Value {
        default_registry()
            .get("session.list")
            .expect("session.list is registered")
            .run(Some(built), json!({"mode": "full"}))
            .unwrap()
    }

    #[test]
    fn the_list_carries_the_latest_reply_excerpt() {
        let dir =
            std::env::temp_dir().join(format!("vogt-reply-excerpt-files-{}", std::process::id()));
        write_transcript(
            &dir,
            &[
                claude_line("a", "older", None),
                claude_line("b", "The operator list: token, HAR.", None),
            ],
        );
        let built = built(
            "excerpt",
            vec![("claude".to_string(), dir.join("claude"))],
            Some(live_claude_engine()),
        );
        seed(&built);
        let rows = list(&built);
        let row = rows["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["id"] == "ses_0001")
            .expect("the seeded session is listed");
        assert_eq!(row["last_reply_excerpt"], "The operator list: token, HAR.");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_list_says_which_model_the_conversation_ran() {
        let dir =
            std::env::temp_dir().join(format!("vogt-reply-model-files-{}", std::process::id()));
        write_transcript(
            &dir,
            &[
                claude_line("a", "first", None),
                claude_line("b", "second", Some("claude-opus-5-5")),
                claude_line("c", "", Some("<synthetic>")),
            ],
        );
        let built = built(
            "model",
            vec![("claude".to_string(), dir.join("claude"))],
            Some(live_claude_engine()),
        );
        seed(&built);
        let rows = list(&built);
        let row = rows["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["id"] == "ses_0001")
            .expect("the seeded session is listed");
        assert!(row["model"].is_null(), "nothing was asked for");
        assert_eq!(row["running"]["agent"], "claude");
        assert_eq!(row["running"]["model"], "claude-opus-5-5");
        assert_eq!(row["running"]["model_basis"], "transcript");
        assert_eq!(row["template"], "claude");
        let _ = fs::remove_dir_all(&dir);
    }
}
