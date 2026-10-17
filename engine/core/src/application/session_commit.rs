//! Committing a worker session's staged work. Ports `commit_for_session` from
//! `src/vogt/application/services/sessions.py` (C14, WI-1074 S8b).
//!
//! The engine owns the checklist and the commit: the worker must be quiet, and
//! the staged diff must carry no conflict marker, secret, intent-to-add or
//! forbidden text. The core decides who may ask, and reads the target
//! project's `forbidden_text` itself; the caller never names the patterns.

use serde_json::{json, Map, Value};

use crate::adapters::engine::EngineClient;
use crate::application::context::{write_of, AppContext, Built};
use crate::application::services::sessions::{
    answers_as_person, engine_of, resolve_target, Target,
};
use crate::application::writes::{self, audited_action};
use crate::core::{py_repr, Clock, IdFactory};
use crate::errors::VogtError;
use crate::storage::interface::{DeclaredStore, ReadView};

const SESSION_COMMIT_FOR: &str = "session.commit_for";
const SESSION_COMMITTED: &str = "session.committed";
const OWN_SESSION_PREFIX: &str = "agent:session:";
const ENGINE_SESSION_PREFIX: &str = "agent:engine:";

#[derive(Debug, Clone)]
pub struct CommitForSessionParams {
    pub id: String,
    pub message: String,
    pub paths: Vec<String>,
    pub full_diff: bool,
    pub worktree: Option<String>,
    pub reason: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct CommitForSessionResult {
    pub commit: String,
    pub digest: String,
    pub diff_stat: String,
    pub diff: Option<String>,
    pub review: Vec<String>,
}

pub fn commit_for_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    let parsed = CommitForSessionParams {
        id: required(&params, "id")?,
        message: required(&params, "message")?,
        paths: match params.get("paths") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(items)) => items
                .iter()
                .map(|item| {
                    item.as_str().map(str::to_string).ok_or_else(|| {
                        VogtError::InvalidRequest("paths must be a list of strings".to_string())
                    })
                })
                .collect::<Result<_, _>>()?,
            Some(_) => {
                return Err(VogtError::InvalidRequest(
                    "paths must be a list of strings".to_string(),
                ))
            }
        },
        full_diff: match params.get("full_diff") {
            None | Some(Value::Null) => false,
            Some(Value::Bool(flag)) => *flag,
            Some(_) => {
                return Err(VogtError::InvalidRequest(
                    "full_diff must be a boolean".to_string(),
                ))
            }
        },
        worktree: match params.get("worktree") {
            None | Some(Value::Null) => None,
            Some(Value::String(text)) => Some(text.clone()),
            Some(_) => {
                return Err(VogtError::InvalidRequest(
                    "worktree must be a string".to_string(),
                ))
            }
        },
        reason: required(&params, "reason")?,
    };
    crate::with_ctx!(built, |ctx| commit_for_session(ctx, &parsed)).and_then(|result| {
        serde_json::to_value(result).map_err(|error| {
            VogtError::InvalidRequest(format!("could not render the result: {error}"))
        })
    })
}

fn required(params: &Value, field: &str) -> Result<String, VogtError> {
    match params.get(field) {
        Some(Value::String(text)) => Ok(text.clone()),
        None | Some(Value::Null) => Err(VogtError::InvalidRequest(format!("{field} is required"))),
        Some(_) => Err(VogtError::InvalidRequest(format!(
            "{field} must be a string"
        ))),
    }
}

/// Commit a worker session's staged work (C14), in Python's order: the
/// reason, the message, the engine, the target, who asks, then the commit.
pub fn commit_for_session<C, I>(
    ctx: &AppContext<C, I>,
    params: &CommitForSessionParams,
) -> Result<CommitForSessionResult, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let reason = writes::validate_reason(&params.reason)?;
    let message = params.message.trim();
    if message.is_empty() {
        return Err(VogtError::InvalidRequest(
            "message must not be empty: say what the commit does".to_string(),
        ));
    }
    let engine = engine_of(ctx)?;
    let target = resolve_target(ctx, &params.id)?;
    require_commit_authority(ctx, engine, &target.engine_session_id)?;
    let forbidden_text = forbidden_text_for(ctx, &target)?;
    let mut body = Map::new();
    body.insert("op".to_string(), json!("commit"));
    body.insert("session".to_string(), json!(target.engine_session_id));
    body.insert("message".to_string(), json!(message));
    body.insert("paths".to_string(), json!(params.paths));
    body.insert("full_diff".to_string(), json!(params.full_diff));
    body.insert("forbidden_text".to_string(), json!(forbidden_text));
    // Python sends the worktree only when it is non-empty.
    if let Some(worktree) = params.worktree.as_deref().filter(|w| !w.is_empty()) {
        body.insert("worktree".to_string(), json!(worktree));
    }
    let committed = engine.git_commit(&target.engine_session_id, &Value::Object(body))?;
    let field = |name: &str| -> String {
        match committed.get(name) {
            None | Some(Value::Null) => String::new(),
            Some(Value::String(text)) => text.clone(),
            Some(other) => other.to_string(),
        }
    };
    let sha = field("commit");
    if sha.is_empty() {
        return Err(VogtError::InvalidRequest(format!(
            "the engine committed nothing for session {}",
            py_repr(&params.id)
        )));
    }
    let digest = field("digest");
    let entity_id = target
        .session
        .as_ref()
        .map_or_else(|| target.engine_session_id.clone(), |s| s.id.clone());
    let mut writing = write_of(ctx);
    audited_action(
        &mut writing,
        SESSION_COMMIT_FOR,
        &reason,
        "session",
        &entity_id,
        &json!({
            "engine_session_id": target.engine_session_id,
            "commit": sha,
            "digest": digest,
            "paths": params.paths.len(),
        }),
        SESSION_COMMITTED,
        None,
    )?;
    let review = match committed.get("review") {
        Some(Value::Array(flags)) => flags
            .iter()
            .map(|flag| match flag {
                Value::String(text) => text.clone(),
                other => other.to_string(),
            })
            .collect(),
        _ => Vec::new(),
    };
    Ok(CommitForSessionResult {
        commit: sha,
        digest,
        diff_stat: field("diff_stat"),
        diff: committed
            .get("diff")
            .and_then(Value::as_str)
            .map(str::to_string),
        review,
    })
}

/// The target session's project policy patterns (C13), read here, never taken
/// from the caller. A session Vogt never linked has no project, so nothing is
/// forbidden for it beyond what the engine checks anyway.
fn forbidden_text_for<C, I>(
    ctx: &AppContext<C, I>,
    target: &Target,
) -> Result<Vec<String>, VogtError>
where
    C: Clock,
    I: IdFactory,
{
    let Some(session) = &target.session else {
        return Ok(Vec::new());
    };
    let project = ctx.declared.read()?.project_by_id(&session.project_id)?;
    Ok(project.map_or_else(Vec::new, |p| p.agent_policy.forbidden_text))
}

/// The engine session id this caller is, when the caller is a session: a
/// session Vogt started through its row, one the engine started by its id.
pub(crate) fn caller_engine_session_id<C, I>(
    ctx: &AppContext<C, I>,
) -> Result<Option<String>, VogtError>
where
    C: Clock,
    I: IdFactory,
{
    let reference = ctx.principal.identity_ref.as_str();
    if let Some(own) = reference.strip_prefix(OWN_SESSION_PREFIX) {
        let session = ctx.declared.read()?.session_by_id(own)?;
        return Ok(session.map(|s| s.engine_session_id));
    }
    Ok(reference
        .strip_prefix(ENGINE_SESSION_PREFIX)
        .map(str::to_string))
}

/// Who may commit for a session (C14). A person always may. A session may
/// commit only for another session, and only when its own engine role is
/// `oversight`: a worker committing its own tree, or another worker's, is the
/// thing this refuses.
fn require_commit_authority<C, I>(
    ctx: &AppContext<C, I>,
    engine: &EngineClient,
    target_engine_id: &str,
) -> Result<(), VogtError>
where
    C: Clock,
    I: IdFactory,
{
    if answers_as_person(ctx)? {
        return Ok(());
    }
    let Some(caller_id) = caller_engine_session_id(ctx)? else {
        return Err(VogtError::RoleRefused(format!(
            "only a person, or an oversight session, commits for a session ({} is neither)",
            ctx.principal.identity_ref
        )));
    };
    if caller_id == target_engine_id {
        return Err(VogtError::RoleRefused(format!(
            "a session does not commit its own work; an oversight session, or a person, \
             commits for it ({caller_id})"
        )));
    }
    let caller = engine.get_session(&caller_id)?;
    match caller {
        Some(found) if found.role == "oversight" => Ok(()),
        other => {
            let role = other.map_or_else(|| "unknown".to_string(), |s| s.role);
            Err(VogtError::RoleRefused(format!(
                "only an oversight session commits for another session ({caller_id} is {role})"
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use serde_json::{json, Value};

    use crate::adapters::engine::EngineClient;
    use crate::application::context::{build_context, write_of, Built};
    use crate::application::writes::{audited_write, WriteOutcome};
    use crate::core::{
        ActorKind, AgentPolicy, CodingSession, Moment, Principal, Project, StepClock,
    };
    use crate::errors::VogtError;
    use crate::storage::interface::{AuditQuery, DeclaredStore, ReadView, WriteTxn};
    use crate::with_ctx;

    use super::commit_for_op;

    type Seen = Arc<Mutex<Vec<(String, String, Value)>>>;

    const WORKER: &str = "eng-worker";
    const OVERSEER: &str = "eng-overseer";

    /// An engine that records every request. `GET /api/sessions/<id>` answers
    /// with the role `roles` gives that id (404 for one it does not hold);
    /// `POST /api/git/op` answers `git` as (status, body).
    fn engine(roles: Vec<(&str, &str)>, git: (u16, Value), seen: Seen) -> EngineClient {
        let roles: Vec<(String, String)> = roles
            .into_iter()
            .map(|(id, role)| (id.to_string(), role.to_string()))
            .collect();
        EngineClient::new(
            "http://engine",
            None,
            Some(Arc::new(move |url, _, body, method| {
                let path = url.trim_start_matches("http://engine").to_string();
                let sent = serde_json::from_slice(body).unwrap_or(Value::Null);
                seen.lock()
                    .unwrap()
                    .push((method.to_string(), path.clone(), sent));
                if path == "/api/git/op" {
                    return (git.0, serde_json::to_vec(&git.1).unwrap());
                }
                let id = path.trim_start_matches("/api/sessions/");
                match roles.iter().find(|(known, _)| known == id) {
                    Some((_, role)) => (
                        200,
                        json!({"id": id, "alive": true, "activity": "idle", "role": role})
                            .to_string()
                            .into_bytes(),
                    ),
                    None => (404, b"{}".to_vec()),
                }
            })),
        )
    }

    fn committed() -> (u16, Value) {
        (
            200,
            json!({
                "commit": "abc123def456",
                "digest": "1 file changed, 2 insertions(+)",
                "diff_stat": " src/a.rs | 2 ++",
                "review": ["1 pin"],
            }),
        )
    }

    fn context(
        name: &str,
        principal: Principal,
        roles: Vec<(&str, &str)>,
        git: (u16, Value),
        forbidden: Vec<String>,
    ) -> (std::path::PathBuf, Built, Seen) {
        // Config tests set VOGT_* variables process-wide; hold the shared lock
        // while this context reads them, or a parallel test's bad value fails it.
        let _env = crate::config::env_lock();
        let dir = std::env::temp_dir().join(format!("vogt-commit-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut clock = None;
        let mut ids = None;
        crate::application::instance::init(&dir, &mut clock, &mut ids).unwrap();
        let seen: Seen = Arc::new(Mutex::new(Vec::new()));
        let config = crate::config::VogtConfig {
            data_dir: dir.clone(),
            ..crate::config::VogtConfig::default()
        };
        let built = build_context(
            config,
            Some(principal),
            Some(StepClock::new(Moment::from_unix(1_700_000_000, 0))),
            None,
            None,
            Some(engine(roles, git, Arc::clone(&seen))),
            None,
            None,
        )
        .unwrap();
        record(&built, "ses_worker", WORKER, forbidden);
        (dir, built, seen)
    }

    /// Record the worker's session under a project carrying `forbidden`.
    fn record(built: &Built, id: &str, engine_id: &str, forbidden: Vec<String>) {
        with_ctx!(built, |ctx| {
            let mut write = write_of(ctx);
            let id = id.to_string();
            let engine_id = engine_id.to_string();
            audited_write(&mut write, "session.start", "test", move |txn, actor| {
                let mut project = Project::new(
                    "prj_1",
                    "proj",
                    "Proj",
                    "/work",
                    Moment::from_unix(1_700_000_000, 0),
                );
                project.agent_policy = AgentPolicy {
                    forbidden_text: forbidden,
                    ..AgentPolicy::default()
                };
                txn.insert_project(&project)?;
                txn.insert_session(&CodingSession {
                    id: id.clone(),
                    engine_session_id: engine_id,
                    project_id: "prj_1".to_string(),
                    work_item_id: None,
                    actor_id: actor.id.clone(),
                    cwd: "/work".to_string(),
                    template: None,
                    model: None,
                    effort: None,
                    reason: "test".to_string(),
                    purpose: None,
                    started_by_session: None,
                    started_by_kind: None,
                    started_at: Moment::from_unix(1_700_000_000, 0),
                    stopped_at: None,
                })?;
                Ok(WriteOutcome::new(
                    (),
                    "session",
                    &id,
                    json!({}),
                    "session.started",
                    json!({}),
                ))
            })
            .unwrap();
        });
    }

    fn person() -> Principal {
        Principal::new("local:someone", ActorKind::Human, "Someone").unwrap()
    }

    fn agent(identity: &str) -> Principal {
        Principal::new(identity, ActorKind::Agent, "agent").unwrap()
    }

    fn ask(message: &str) -> Value {
        json!({"id": "ses_worker", "message": message, "reason": "testing the commit path"})
    }

    fn git_ops(seen: &Seen) -> Vec<Value> {
        seen.lock()
            .unwrap()
            .iter()
            .filter(|(_, path, _)| path == "/api/git/op")
            .map(|(_, _, body)| body.clone())
            .collect()
    }

    fn audits(built: &Built) -> usize {
        with_ctx!(built, |ctx| ctx
            .declared
            .read()
            .unwrap()
            .list_audit(&AuditQuery {
                limit: 100,
                offset: 0,
                actor_id: None,
                operation: Some("session.commit_for".to_string()),
                entity_id: None,
                project_id: None,
                since: None,
                until: None,
            })
            .unwrap()
            .len())
    }

    #[test]
    fn a_commit_returns_the_digest_and_is_audited() {
        let (_dir, built, seen) = context("digest", person(), vec![], committed(), vec![]);
        let mut params = ask("record the session commit");
        params["paths"] = json!(["src/a.rs"]);
        let result = commit_for_op(&built, params).unwrap();
        assert_eq!(result["commit"], "abc123def456");
        assert_eq!(result["digest"], "1 file changed, 2 insertions(+)");
        assert_eq!(result["diff"], Value::Null);
        assert_eq!(result["review"], json!(["1 pin"]));
        assert_eq!(
            git_ops(&seen).last().unwrap(),
            &json!({
                "op": "commit",
                "session": WORKER,
                "message": "record the session commit",
                "paths": ["src/a.rs"],
                "full_diff": false,
                "forbidden_text": [],
            })
        );
        assert_eq!(audits(&built), 1);
        let event = with_ctx!(&built, |ctx| ctx
            .declared
            .read()
            .unwrap()
            .list_events(0, 1000, None)
            .unwrap()
            .into_iter()
            .rfind(|event| event.kind == "session.committed")
            .unwrap());
        assert_eq!(event.entity_id, "ses_worker");
        assert_eq!(event.summary["commit"], "abc123def456");
        assert!(event.summary.get("diff").is_none());
    }

    #[test]
    fn a_blank_message_never_reaches_the_engine() {
        let (_dir, built, seen) = context("blank", person(), vec![], committed(), vec![]);
        let error = commit_for_op(&built, ask("   ")).unwrap_err();
        assert!(
            matches!(&error, VogtError::InvalidRequest(said) if said.contains("must not be empty")),
            "{error:?}"
        );
        assert!(git_ops(&seen).is_empty());
    }

    #[test]
    fn the_engines_refusal_reaches_the_caller_unaudited() {
        for (status, said) in [
            (400, "staged diff has a conflict marker"),
            (400, "staged diff has an intent-to-add entry"),
            (400, "message matches forbidden text"),
            (
                409,
                "session eng-1 is Running: a commit waits until it is quiet",
            ),
        ] {
            let (_dir, built, _) = context(
                &format!("refused-{status}-{}", said.len()),
                person(),
                vec![],
                (status, json!({"error": said})),
                vec![],
            );
            let error = commit_for_op(&built, ask("should not land")).unwrap_err();
            match (status, &error) {
                (400, VogtError::InvalidRequest(text)) | (409, VogtError::Conflict(text)) => {
                    assert!(text.contains(said), "{text}");
                }
                _ => panic!("{status}: {error:?}"),
            }
            assert_eq!(audits(&built), 0);
        }
    }

    #[test]
    fn a_named_worktree_is_forwarded_and_patterns_are_not_taken_from_the_caller() {
        let (_dir, built, seen) = context("worktree", person(), vec![], committed(), vec![]);
        let mut params = ask("commit in the linked worktree");
        params["worktree"] = json!("/srv/estate/vogt-wt");
        params["forbidden_text"] = json!(["ignored"]);
        commit_for_op(&built, params).unwrap();
        let sent = git_ops(&seen);
        assert_eq!(sent[0]["worktree"], "/srv/estate/vogt-wt");
        assert_eq!(sent[0]["forbidden_text"], json!([]));
    }

    #[test]
    fn the_projects_forbidden_text_is_what_the_engine_checks() {
        let (_dir, built, seen) = context(
            "forbidden",
            person(),
            vec![],
            committed(),
            vec!["(?i)grok".to_string()],
        );
        commit_for_op(&built, ask("commit for the worker")).unwrap();
        assert_eq!(git_ops(&seen)[0]["forbidden_text"], json!(["(?i)grok"]));
    }

    #[test]
    fn a_session_cannot_commit_its_own_work() {
        for who in ["agent:session:ses_worker", "agent:engine:eng-worker"] {
            let (_dir, built, seen) = context(
                &format!("own-{}", who.len()),
                agent(who),
                vec![(WORKER, "oversight")],
                committed(),
                vec![],
            );
            let error = commit_for_op(&built, ask("my own commit")).unwrap_err();
            assert!(
                matches!(&error, VogtError::RoleRefused(said) if said.contains("does not commit its own work")),
                "{error:?}"
            );
            assert!(git_ops(&seen).is_empty());
        }
    }

    #[test]
    fn a_worker_cannot_commit_for_another_worker() {
        let (_dir, built, seen) = context(
            "worker",
            agent(&format!("agent:engine:{OVERSEER}")),
            vec![(OVERSEER, "worker")],
            committed(),
            vec![],
        );
        let error = commit_for_op(&built, ask("another worker's commit")).unwrap_err();
        assert!(
            matches!(&error, VogtError::RoleRefused(said) if said.ends_with("(eng-overseer is worker)")),
            "{error:?}"
        );
        assert!(git_ops(&seen).is_empty());
    }

    #[test]
    fn an_unknown_caller_session_or_a_non_session_agent_is_refused() {
        let (_dir, built, seen) = context(
            "unknown",
            agent(&format!("agent:engine:{OVERSEER}")),
            vec![],
            committed(),
            vec![],
        );
        let error = commit_for_op(&built, ask("who am i")).unwrap_err();
        assert!(
            matches!(&error, VogtError::RoleRefused(said) if said.ends_with("is unknown)")),
            "{error:?}"
        );
        assert!(git_ops(&seen).is_empty());
        for who in ["agent:pod", "agent:session:ses_missing"] {
            let (_dir, built, seen) = context(
                &format!("neither-{}", who.len()),
                agent(who),
                vec![],
                committed(),
                vec![],
            );
            let error = commit_for_op(&built, ask("not a session")).unwrap_err();
            assert!(
                matches!(&error, VogtError::RoleRefused(said) if said.contains("is neither")),
                "{error:?}"
            );
            assert!(git_ops(&seen).is_empty());
        }
    }

    #[test]
    fn the_engine_credential_is_not_a_person() {
        let (_dir, built, seen) = context(
            "engine-cred",
            Principal::new("agent:vogt-engine", ActorKind::Human, "engine").unwrap(),
            vec![],
            committed(),
            vec![],
        );
        let error = commit_for_op(&built, ask("the engine commits")).unwrap_err();
        assert!(matches!(error, VogtError::RoleRefused(_)), "{error:?}");
        assert!(git_ops(&seen).is_empty());
    }

    #[test]
    fn oversight_may_commit_for_a_worker() {
        let (_dir, built, _) = context(
            "oversight",
            agent(&format!("agent:engine:{OVERSEER}")),
            vec![(OVERSEER, "oversight")],
            committed(),
            vec![],
        );
        let result = commit_for_op(&built, ask("oversight commits it")).unwrap();
        assert_eq!(result["commit"], "abc123def456");
    }

    #[test]
    fn a_person_may_commit_without_a_role_lookup() {
        let (_dir, built, seen) = context("person", person(), vec![], committed(), vec![]);
        let result = commit_for_op(&built, ask("a person commits it")).unwrap();
        assert_eq!(result["commit"], "abc123def456");
        assert!(seen
            .lock()
            .unwrap()
            .iter()
            .all(|(method, path, _)| !(method == "GET" && path.starts_with("/api/sessions/"))));
    }

    #[test]
    fn an_engine_that_committed_nothing_is_refused_unaudited() {
        let (_dir, built, _) = context("nothing", person(), vec![], (200, json!({})), vec![]);
        let error = commit_for_op(&built, ask("empty")).unwrap_err();
        assert!(
            matches!(&error, VogtError::InvalidRequest(said) if said == "the engine committed nothing for session 'ses_worker'"),
            "{error:?}"
        );
        assert_eq!(audits(&built), 0);
    }
}
