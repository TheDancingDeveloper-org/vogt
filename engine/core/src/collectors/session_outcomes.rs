//! `session-outcomes` — what a run left behind. Ports
//! `src/vogt/collectors/session_outcomes.py`.
//!
//! The application adds it only when an engine is configured, and
//! `requires_network` keeps it out of an offline sweep.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use serde_json::{json, Map, Value};

use crate::adapters::engine::{
    EngineAgentTask, EngineArchivedSession, EngineClient, EngineSession,
};
use crate::collectors::base::{finding, Collection, Collector, CollectorContext, Finding};
use crate::core::{from_iso, Moment, Project};
use crate::errors::VogtError;

pub const KIND_SESSION_OUTCOME: &str = "session.outcome";
pub const KIND_TASK_RUN: &str = "agent_task.run";

/// How many of a project's sessions are re-observed on each sweep, newest first.
/// Declared to match Python; the collector itself does not cap.
pub const SESSION_OBSERVATION_LIMIT: usize = 200;

/// Upper bound on the commits a delta will count.
pub const MAX_COMMITS_COUNTED: usize = 500;

/// What Vogt declared about one session, flattened for the collector.
#[derive(Debug, Clone)]
pub struct SessionRecord {
    pub id: String,
    pub engine_session_id: String,
    pub cwd: String,
    pub started_at: Moment,
    pub stopped_at: Option<Moment>,
    pub work_item: Option<String>,
}

/// The declared reads this collector needs, done for it.
pub trait DeclaredSessions {
    fn for_project(&self, project: &Project) -> Vec<SessionRecord>;
    fn project_of_work_item(&self, work_item_ref: &str) -> Option<String>;
}

/// Session outcomes and bound agent-task runs, as observations.
pub struct SessionOutcomeCollector<S> {
    engine: EngineClient,
    sessions: S,
    live: Option<BTreeMap<String, EngineSession>>,
    tasks: Option<Vec<EngineAgentTask>>,
}

impl<S> SessionOutcomeCollector<S> {
    pub fn new(engine: EngineClient, sessions: S) -> Self {
        Self {
            engine,
            sessions,
            live: None,
            tasks: None,
        }
    }
}

impl<S: DeclaredSessions> Collector for SessionOutcomeCollector<S> {
    fn name(&self) -> &str {
        "session-outcomes"
    }

    fn requires_network(&self) -> bool {
        true
    }

    fn collect(
        &mut self,
        _ctx: &mut CollectorContext<'_>,
        project: &Project,
        _now: &mut dyn FnMut() -> Moment,
    ) -> Collection {
        // Python yields each session before the next can raise, then yields
        // the bound runs. A failure keeps the findings already produced.
        let sessions = match self.session_outcomes(project) {
            Ok(found) => found,
            Err(stopped) => return Collection::partial(stopped.found, stopped.error),
        };
        match self.bound_task_runs(project) {
            Ok(runs) => {
                let mut found = sessions;
                found.extend(runs);
                Collection::ok(found)
            }
            Err(error) => Collection::partial(sessions, error),
        }
    }
}

impl<S: DeclaredSessions> SessionOutcomeCollector<S> {
    fn session_outcomes(&mut self, project: &Project) -> Result<Vec<Finding>, PartialFindings> {
        let records = self.sessions.for_project(project);
        let mut found = Vec::with_capacity(records.len());
        for record in records {
            match self.outcome_of(project, &record) {
                Ok(finding) => found.push(finding),
                Err(error) => return Err(PartialFindings { found, error }),
            }
        }
        Ok(found)
    }

    fn outcome_of(
        &mut self,
        project: &Project,
        record: &SessionRecord,
    ) -> Result<Finding, VogtError> {
        let live = self
            .live_sessions()?
            .get(&record.engine_session_id)
            .cloned();
        let running = live.as_ref().is_some_and(|row| row.exit_code.is_none());

        let archived = if running {
            None
        } else {
            self.engine.archived_session(&record.engine_session_id)?
        };

        let mut payload = Map::new();
        payload.insert("session".to_string(), Value::String(record.id.clone()));
        payload.insert(
            "engine_session_id".to_string(),
            Value::String(record.engine_session_id.clone()),
        );
        payload.insert("project".to_string(), Value::String(project.slug.clone()));
        payload.insert(
            "work_item".to_string(),
            match &record.work_item {
                Some(item) => Value::String(item.clone()),
                None => Value::Null,
            },
        );
        payload.insert("cwd".to_string(), Value::String(record.cwd.clone()));
        payload.insert(
            "started_at".to_string(),
            Value::String(record.started_at.to_iso()),
        );

        if running {
            payload.insert("state".to_string(), Value::String("running".to_string()));
            payload.insert("provisional".to_string(), Value::Bool(true));
            payload.insert(
                "delta".to_string(),
                delta(&record.cwd, record.started_at, None, true)?,
            );
            return Ok(outcome_finding(project, &record.id, Value::Object(payload)));
        }

        let end = ended_at(archived.as_ref()).or(record.stopped_at);
        let mut exit_code = archived.as_ref().and_then(|row| row.exit_code);
        if exit_code.is_none() {
            if let Some(row) = &live {
                exit_code = row.exit_code;
            }
        }

        if archived.is_none() && live.is_none() {
            payload.insert("state".to_string(), Value::String("unknown".to_string()));
            payload.insert("provisional".to_string(), Value::Bool(false));
            payload.insert(
                "detail".to_string(),
                Value::String(
                    "the engine no longer has this session and has no archive of it, so how it ended is not collected"
                        .to_string(),
                ),
            );
        } else {
            payload.insert("state".to_string(), Value::String("finished".to_string()));
            payload.insert("provisional".to_string(), Value::Bool(false));
            if exit_code.is_none() {
                payload.insert(
                    "detail".to_string(),
                    Value::String(
                        "the session has ended and the engine did not record an exit code for it"
                            .to_string(),
                    ),
                );
            }
        }

        if let Some(code) = exit_code {
            payload.insert("exit_code".to_string(), json!(code));
        }
        if let Some(end) = end {
            payload.insert("ended_at".to_string(), Value::String(end.to_iso()));
            for (key, value) in duration_of(record, archived.as_ref(), end) {
                payload.insert(key, value);
            }
        }
        payload.insert(
            "delta".to_string(),
            delta(&record.cwd, record.started_at, end, false)?,
        );
        Ok(outcome_finding(project, &record.id, Value::Object(payload)))
    }

    fn live_sessions(&mut self) -> Result<&BTreeMap<String, EngineSession>, VogtError> {
        if self.live.is_none() {
            let rows = self.engine.list_sessions()?;
            self.live = Some(rows.into_iter().map(|row| (row.id.clone(), row)).collect());
        }
        Ok(self.live.as_ref().expect("just filled"))
    }

    fn bound_task_runs(&mut self, project: &Project) -> Result<Vec<Finding>, VogtError> {
        let tasks = self.bound_tasks()?;
        let mut found = Vec::new();
        for task in tasks {
            if !self.binds(&task, project) {
                continue;
            }
            for run in &task.runs {
                found.push(task_run_finding(project, &task, run));
            }
        }
        Ok(found)
    }

    fn bound_tasks(&mut self) -> Result<Vec<EngineAgentTask>, VogtError> {
        if self.tasks.is_none() {
            let rows = self.engine.list_agent_tasks()?;
            self.tasks = Some(rows.into_iter().filter(EngineAgentTask::is_bound).collect());
        }
        Ok(self.tasks.clone().unwrap_or_default())
    }

    fn binds(&self, task: &EngineAgentTask, project: &Project) -> bool {
        if task
            .project
            .as_deref()
            .is_some_and(|name| name.trim().eq_ignore_ascii_case(&project.slug))
        {
            return true;
        }
        let Some(work_item) = &task.work_item else {
            return false;
        };
        self.sessions.project_of_work_item(work_item).as_deref() == Some(project.slug.as_str())
    }
}

fn outcome_finding(project: &Project, session_id: &str, payload: Value) -> Finding {
    finding(
        KIND_SESSION_OUTCOME,
        &format!("session:{session_id}"),
        payload,
        Some(project),
        None,
        false,
    )
}

fn task_run_finding(
    project: &Project,
    task: &EngineAgentTask,
    run: &crate::adapters::engine::EngineTaskRun,
) -> Finding {
    let running = run.status == "running";
    let mut payload = Map::new();
    payload.insert("task".to_string(), Value::String(task.name.clone()));
    payload.insert("task_id".to_string(), Value::String(task.id.clone()));
    payload.insert("run".to_string(), Value::String(run.id.clone()));
    payload.insert(
        "session_id".to_string(),
        Value::String(run.session_id.clone()),
    );
    payload.insert("project".to_string(), Value::String(project.slug.clone()));
    payload.insert(
        "work_item".to_string(),
        match &task.work_item {
            Some(item) => Value::String(item.clone()),
            None => Value::Null,
        },
    );
    payload.insert(
        "started_at".to_string(),
        Value::String(run.started_at.clone()),
    );
    payload.insert(
        "state".to_string(),
        Value::String(if running { "running" } else { "finished" }.to_string()),
    );
    payload.insert("status".to_string(), Value::String(run.status.clone()));
    payload.insert("provisional".to_string(), Value::Bool(running));
    payload.insert(
        "findings".to_string(),
        Value::Array(
            run.findings
                .iter()
                .map(|item| {
                    json!({
                        "at": item.at,
                        "text": item.text,
                        "source": item.source,
                    })
                })
                .collect(),
        ),
    );
    if let Some(summary) = &run.summary {
        payload.insert("summary".to_string(), Value::String(summary.clone()));
    }
    if let Some(code) = run.exit_code {
        payload.insert("exit_code".to_string(), json!(code));
    }
    if let Some(completed) = &run.completed_at {
        payload.insert("completed_at".to_string(), Value::String(completed.clone()));
    }
    if let Some(seconds) = run_duration(&run.started_at, run.completed_at.as_deref()) {
        payload.insert("duration_seconds".to_string(), json!(seconds));
    }
    finding(
        KIND_TASK_RUN,
        &format!("task-run:{}", run.id),
        Value::Object(payload),
        Some(project),
        None,
        false,
    )
}

/// Findings already yielded when a later session could not be read.
struct PartialFindings {
    found: Vec<Finding>,
    error: VogtError,
}

fn delta(
    cwd: &str,
    since: Moment,
    until: Option<Moment>,
    still_open: bool,
) -> Result<Value, VogtError> {
    // Python `Path("")` is `.`. An empty cwd is this process's directory,
    // not a missing path.
    let cwd = if cwd.is_empty() { "." } else { cwd };
    let root = expanduser(Path::new(cwd));
    if !root.is_dir() {
        return Ok(json!({
            "is_git_repository": false,
            "detail": "the path the run happened in no longer exists",
        }));
    }
    if !root.join(".git").exists() {
        return Ok(json!({
            "is_git_repository": false,
            "detail": "the path the run happened in is not a git checkout",
        }));
    }

    let mut body = Map::new();
    body.insert("is_git_repository".to_string(), Value::Bool(true));
    body.insert("attributed_to_the_session".to_string(), Value::Bool(false));
    if until.is_none() && !still_open {
        body.insert(
            "detail".to_string(),
            Value::String(
                "no commit counts: nothing recorded when this session ended, so the window has no end and any number would include later work"
                    .to_string(),
            ),
        );
        return Ok(Value::Object(body));
    }

    let since_iso = since.to_iso();
    let until_iso = until.map(|moment| moment.to_iso());
    let mut window = vec!["--since".to_string(), since_iso.clone()];
    if let Some(end) = &until_iso {
        window.push("--until".to_string());
        window.push(end.clone());
    }
    body.insert(
        "window".to_string(),
        json!({
            "from": since_iso,
            "to": until_iso,
        }),
    );
    for (key, value) in commit_stats(&root, &window)? {
        body.insert(key, value);
    }

    let window_args: Vec<&str> = window.iter().map(String::as_str).collect();
    if still_open {
        let status =
            crate::collectors::git_local::git_output(&root, &["status", "--porcelain"], true)?;
        let head = crate::collectors::git_local::git_output(&root, &["rev-parse", "HEAD"], false)?;
        body.insert(
            "head".to_string(),
            if head.is_empty() {
                Value::Null
            } else {
                Value::String(head)
            },
        );
        let uncommitted = if status.is_empty() {
            0
        } else {
            status.lines().count()
        };
        body.insert("uncommitted_files".to_string(), json!(uncommitted));
        body.insert(
            "detail".to_string(),
            Value::String(
                "the tree as it stands, and every commit reachable from HEAD since this session started, whoever made it"
                    .to_string(),
            ),
        );
        return Ok(Value::Object(body));
    }

    let mut log_args = vec!["log", "-1", "--format=%H"];
    log_args.extend(window_args.iter().copied());
    let head = crate::collectors::git_local::git_output(&root, &log_args, false)?;
    body.insert(
        "head_at_window_end".to_string(),
        if head.is_empty() {
            Value::Null
        } else {
            Value::String(head)
        },
    );
    body.insert(
        "detail".to_string(),
        Value::String(
            "every commit reachable from HEAD whose committer date falls in the window, whoever made it. What the session left uncommitted is not here: it is only observable while the session runs"
                .to_string(),
        ),
    );
    Ok(Value::Object(body))
}

fn commit_stats(root: &Path, window: &[String]) -> Result<Map<String, Value>, VogtError> {
    let cap = format!("--max-count={MAX_COMMITS_COUNTED}");
    let window_args: Vec<&str> = window.iter().map(String::as_str).collect();
    let mut rev_args = vec!["log", cap.as_str(), "--format=%H"];
    rev_args.extend(window_args.iter().copied());
    let revisions = crate::collectors::git_local::git_output(root, &rev_args, false)?;
    let commits = if revisions.is_empty() {
        0
    } else {
        revisions.lines().count()
    };

    let mut files = BTreeSet::new();
    let mut insertions = 0i64;
    let mut deletions = 0i64;
    let mut num_args = vec!["log", cap.as_str(), "--format=", "--numstat"];
    num_args.extend(window_args.iter().copied());
    let numstat = crate::collectors::git_local::git_output(root, &num_args, false)?;
    for line in numstat.lines() {
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() != 3 {
            continue;
        }
        let (added, removed, path) = (parts[0], parts[1], parts[2]);
        files.insert(path.to_string());
        insertions += if added.chars().all(|ch| ch.is_ascii_digit()) && !added.is_empty() {
            added.parse::<i64>().unwrap_or(0)
        } else {
            0
        };
        deletions += if removed.chars().all(|ch| ch.is_ascii_digit()) && !removed.is_empty() {
            removed.parse::<i64>().unwrap_or(0)
        } else {
            0
        };
    }

    let mut stats = Map::new();
    stats.insert("commits_in_window".to_string(), json!(commits));
    stats.insert("files_changed".to_string(), json!(files.len()));
    stats.insert("insertions".to_string(), json!(insertions));
    stats.insert("deletions".to_string(), json!(deletions));
    if commits >= MAX_COMMITS_COUNTED {
        stats.insert("truncated".to_string(), Value::Bool(true));
    }
    Ok(stats)
}

fn duration_of(
    record: &SessionRecord,
    archived: Option<&EngineArchivedSession>,
    end: Moment,
) -> BTreeMap<String, Value> {
    let mut out = BTreeMap::new();
    if let Some(archived) = archived {
        if let Some(began) = parse_moment(&archived.created_at) {
            out.insert(
                "duration_seconds".to_string(),
                json!(seconds_between(began, end)),
            );
            out.insert(
                "duration_source".to_string(),
                Value::String("the engine's archive of the process".to_string()),
            );
            return out;
        }
    }
    out.insert(
        "duration_seconds".to_string(),
        json!(seconds_between(record.started_at, end)),
    );
    out.insert(
        "duration_source".to_string(),
        Value::String(
            "Vogt's start to the end it recorded; the engine has no archive of this session, so the two ends of it are two clocks"
                .to_string(),
        ),
    );
    out
}

fn ended_at(archived: Option<&EngineArchivedSession>) -> Option<Moment> {
    archived.and_then(|row| row.ended_at.as_deref().and_then(parse_moment))
}

fn run_duration(started_at: &str, completed_at: Option<&str>) -> Option<i64> {
    let completed = completed_at?;
    let start = parse_moment(started_at)?;
    let end = parse_moment(completed)?;
    Some(seconds_between(start, end))
}

fn seconds_between(start: Moment, end: Moment) -> i64 {
    let seconds = end.seconds_since(start);
    if seconds < 0.0 {
        0
    } else {
        seconds as i64
    }
}

fn parse_moment(value: &str) -> Option<Moment> {
    from_iso(value).ok()
}

/// Python's `Path.expanduser`: only a leading `~` on its own expands.
///
/// Local copy. `git_local::expanduser` is private and that file is not edited.
fn expanduser(path: &Path) -> PathBuf {
    let mut components = path.components();
    match components.next() {
        Some(Component::Normal(first)) if first == "~" => std::env::var_os("HOME")
            .map(|home| PathBuf::from(home).join(components.as_path()))
            .unwrap_or_else(|| path.to_path_buf()),
        _ => path.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::engine::EngineClient;
    use std::process::Command;

    struct FixedSessions {
        records: Vec<SessionRecord>,
        work_items: BTreeMap<String, String>,
    }

    impl DeclaredSessions for FixedSessions {
        fn for_project(&self, _project: &Project) -> Vec<SessionRecord> {
            self.records.clone()
        }

        fn project_of_work_item(&self, work_item_ref: &str) -> Option<String> {
            self.work_items.get(work_item_ref).cloned()
        }
    }

    fn project(root: &Path) -> Project {
        Project::new(
            "prj_fixture",
            "fixture",
            "fixture",
            &root.display().to_string(),
            Moment::from_unix(0, 0),
        )
    }

    fn engine_with(handler: impl Fn(&str) -> (u16, Vec<u8>) + 'static) -> EngineClient {
        EngineClient::new(
            "http://engine.test",
            None,
            Some(std::sync::Arc::new(move |url, _headers, _body, _method| {
                handler(url)
            })),
        )
    }

    fn collect(engine: EngineClient, records: Vec<SessionRecord>, root: &Path) -> Vec<Finding> {
        let config = crate::config::VogtConfig::default();
        let mut context = CollectorContext { config: &config };
        let mut now = || Moment::from_unix(0, 0);
        let mut collector = SessionOutcomeCollector::new(
            engine,
            FixedSessions {
                records,
                work_items: BTreeMap::new(),
            },
        );
        collector
            .collect(&mut context, &project(root), &mut now)
            .findings
    }

    fn temp(label: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("vogt-outcomes-{}-{label}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn git(root: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }

    fn git_at(root: &Path, when: &str, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .env("GIT_AUTHOR_DATE", when)
            .env("GIT_COMMITTER_DATE", when)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} at {when} failed");
    }

    #[test]
    fn a_finished_session_records_the_archive_exit_and_duration() {
        let root = temp("finished");
        git(&root, &["init", "-b", "main"]);
        git(&root, &["config", "user.email", "outcomes@example.com"]);
        git(&root, &["config", "user.name", "outcomes"]);
        std::fs::write(root.join("note.txt"), "one\n").unwrap();
        git(&root, &["add", "note.txt"]);
        git_at(
            &root,
            "2026-08-12T05:00:30+00:00",
            &["commit", "-m", "in window"],
        );

        let started = from_iso("2026-08-12T05:00:00+00:00").unwrap();
        let record = SessionRecord {
            id: "ses_declared".to_string(),
            engine_session_id: "eng-1".to_string(),
            cwd: root.display().to_string(),
            started_at: started,
            stopped_at: None,
            work_item: None,
        };
        let engine = engine_with(|url| {
            if url.ends_with("/api/sessions") {
                return (200, b"[]".to_vec());
            }
            if url.contains("/api/history/") {
                return (
                    200,
                    br#"{"id":"eng-1","created_at":"2026-08-12T05:00:00+00:00","ended_at":"2026-08-12T06:00:00+00:00","exit_code":0}"#.to_vec(),
                );
            }
            (200, b"[]".to_vec())
        });
        let found = collect(engine, vec![record], &root);
        assert_eq!(found.len(), 1);
        let payload = &found[0].payload;
        assert_eq!(found[0].kind, KIND_SESSION_OUTCOME);
        assert_eq!(found[0].subject_key, "session:ses_declared");
        assert_eq!(payload["state"], "finished");
        assert_eq!(payload["exit_code"], 0);
        assert_eq!(payload["duration_seconds"], 3600);
        assert_eq!(
            payload["duration_source"],
            "the engine's archive of the process"
        );
        assert_eq!(payload["provisional"], false);
        assert_eq!(payload["delta"]["commits_in_window"], 1);
        assert_eq!(payload["delta"]["attributed_to_the_session"], false);
        assert!(payload.get("work_item").unwrap().is_null());
    }

    fn record_in(root: &Path, id: &str, started: &str, stopped: Option<&str>) -> SessionRecord {
        SessionRecord {
            id: id.to_string(),
            engine_session_id: "eng-1".to_string(),
            cwd: root.display().to_string(),
            started_at: from_iso(started).unwrap(),
            stopped_at: stopped.map(|text| from_iso(text).unwrap()),
            work_item: None,
        }
    }

    #[test]
    fn a_nonzero_exit_is_kept() {
        let root = temp("nonzero");
        git(&root, &["init", "-b", "main"]);
        let engine = engine_with(|url| {
            if url.contains("/api/history/") {
                return (
                    200,
                    br#"{"id":"eng-1","created_at":"2026-08-12T05:00:00+00:00","ended_at":"2026-08-12T05:10:00+00:00","exit_code":137}"#.to_vec(),
                );
            }
            (200, b"[]".to_vec())
        });
        let found = collect(
            engine,
            vec![record_in(&root, "ses_a", "2026-08-12T05:00:00+00:00", None)],
            &root,
        );
        assert_eq!(found[0].payload["exit_code"], 137);
        assert_eq!(found[0].payload["state"], "finished");
    }

    #[test]
    fn a_running_session_is_provisional_and_has_no_exit_or_duration() {
        let root = temp("running");
        git(&root, &["init", "-b", "main"]);
        git(&root, &["config", "user.email", "outcomes@example.com"]);
        git(&root, &["config", "user.name", "outcomes"]);
        std::fs::write(root.join("dirty.txt"), "x\n").unwrap();
        let engine = engine_with(|url| {
            if url.ends_with("/api/sessions") {
                return (
                    200,
                    br#"[{"id":"eng-1","cwd":"/tmp","exit_code":null}]"#.to_vec(),
                );
            }
            (200, b"[]".to_vec())
        });
        let found = collect(
            engine,
            vec![record_in(&root, "ses_r", "2026-08-12T05:00:00+00:00", None)],
            &root,
        );
        let payload = &found[0].payload;
        assert_eq!(payload["state"], "running");
        assert_eq!(payload["provisional"], true);
        assert!(payload.get("exit_code").is_none());
        assert!(payload.get("duration_seconds").is_none());
        assert!(payload.get("activity").is_none());
        assert_eq!(payload["delta"]["uncommitted_files"], 1);
        assert!(payload["delta"]["detail"]
            .as_str()
            .unwrap()
            .contains("the tree as it stands"));
    }

    #[test]
    fn a_forgotten_session_is_unknown() {
        let root = temp("forgotten");
        let engine = engine_with(|_url| (200, b"[]".to_vec()));
        let found = collect(
            engine,
            vec![record_in(
                &root,
                "ses_gone",
                "2026-08-12T05:00:00+00:00",
                Some("2026-08-12T06:00:00+00:00"),
            )],
            &root,
        );
        assert_eq!(found[0].payload["state"], "unknown");
        assert!(found[0].payload["detail"]
            .as_str()
            .unwrap()
            .contains("not collected"));
    }

    #[test]
    fn vogt_stopped_at_is_not_an_exit_code() {
        let root = temp("stopped");
        git(&root, &["init", "-b", "main"]);
        let engine = engine_with(|url| {
            if url.contains("/api/history/") {
                return (
                    200,
                    br#"{"id":"eng-1","created_at":"not-a-time","ended_at":null,"exit_code":null}"#
                        .to_vec(),
                );
            }
            (200, b"[]".to_vec())
        });
        let found = collect(
            engine,
            vec![record_in(
                &root,
                "ses_s",
                "2026-08-12T05:00:00+00:00",
                Some("2026-08-12T06:00:00+00:00"),
            )],
            &root,
        );
        let payload = &found[0].payload;
        assert_eq!(payload["state"], "finished");
        assert!(payload.get("exit_code").is_none());
        assert!(payload["duration_source"]
            .as_str()
            .unwrap()
            .contains("Vogt's start"));
        assert!(payload["delta"]["detail"]
            .as_str()
            .unwrap()
            .contains("only observable while the session runs"));
        assert!(payload["delta"].get("uncommitted_files").is_none());
    }

    #[test]
    fn an_ended_session_with_no_end_counts_no_commits() {
        let root = temp("noend");
        git(&root, &["init", "-b", "main"]);
        let engine = engine_with(|url| {
            if url.contains("/api/history/") {
                return (
                    200,
                    br#"{"id":"eng-1","created_at":"2026-08-12T05:00:00+00:00","ended_at":null,"exit_code":0}"#.to_vec(),
                );
            }
            (200, b"[]".to_vec())
        });
        let found = collect(
            engine,
            vec![record_in(&root, "ses_n", "2026-08-12T05:00:00+00:00", None)],
            &root,
        );
        assert_eq!(found[0].payload["exit_code"], 0);
        assert!(found[0].payload["delta"].get("commits_in_window").is_none());
        assert!(found[0].payload["delta"]["detail"]
            .as_str()
            .unwrap()
            .contains("no commit counts"));
    }

    #[test]
    fn a_missing_tree_says_the_path_no_longer_exists() {
        let root = temp("missing");
        let gone = root.join("nowhere");
        let engine = engine_with(|url| {
            if url.contains("/api/history/") {
                return (
                    200,
                    br#"{"id":"eng-1","created_at":"2026-08-12T05:00:00+00:00","ended_at":"2026-08-12T06:00:00+00:00","exit_code":0}"#.to_vec(),
                );
            }
            (200, b"[]".to_vec())
        });
        let found = collect(
            engine,
            vec![record_in(&gone, "ses_m", "2026-08-12T05:00:00+00:00", None)],
            &root,
        );
        assert_eq!(found[0].payload["delta"]["is_git_repository"], false);
        assert!(found[0].payload["delta"]["detail"]
            .as_str()
            .unwrap()
            .contains("no longer exists"));
    }

    #[test]
    fn a_finished_outcome_does_not_move_when_a_later_commit_lands() {
        let root = temp("stable");
        git(&root, &["init", "-b", "main"]);
        git(&root, &["config", "user.email", "outcomes@example.com"]);
        git(&root, &["config", "user.name", "outcomes"]);
        std::fs::write(root.join("note.txt"), "one\n").unwrap();
        git(&root, &["add", "note.txt"]);
        git_at(
            &root,
            "2026-08-12T05:00:30+00:00",
            &["commit", "-m", "in window"],
        );
        let engine = engine_with(|url| {
            if url.contains("/api/history/") {
                return (
                    200,
                    br#"{"id":"eng-1","created_at":"2026-08-12T05:00:00+00:00","ended_at":"2026-08-12T06:00:00+00:00","exit_code":0}"#.to_vec(),
                );
            }
            (200, b"[]".to_vec())
        });
        let first = collect(
            engine,
            vec![record_in(&root, "ses_d", "2026-08-12T05:00:00+00:00", None)],
            &root,
        );
        std::fs::write(root.join("later.txt"), "two\n").unwrap();
        git(&root, &["add", "later.txt"]);
        git_at(
            &root,
            "2026-08-13T05:00:30+00:00",
            &["commit", "-m", "after"],
        );
        let engine = engine_with(|url| {
            if url.contains("/api/history/") {
                return (
                    200,
                    br#"{"id":"eng-1","created_at":"2026-08-12T05:00:00+00:00","ended_at":"2026-08-12T06:00:00+00:00","exit_code":0}"#.to_vec(),
                );
            }
            (200, b"[]".to_vec())
        });
        let second = collect(
            engine,
            vec![record_in(&root, "ses_d", "2026-08-12T05:00:00+00:00", None)],
            &root,
        );
        assert_eq!(first[0].content_digest, second[0].content_digest);
        assert_eq!(second[0].payload["delta"]["commits_in_window"], 1);
    }

    #[test]
    fn an_unbound_task_is_ignored_and_a_work_item_binding_is_kept() {
        let root = temp("tasks");
        let config = crate::config::VogtConfig::default();
        let mut context = CollectorContext { config: &config };
        let mut now = || Moment::from_unix(0, 0);
        let engine = engine_with(|url| {
            if url.ends_with("/api/agent-tasks") {
                return (
                    200,
                    br#"[{"id":"t-free","name":"free","runs":[{"id":"r-free","session_id":"s","started_at":"2026-08-12T05:00:00+00:00","status":"done"}]},{"id":"t-bound","name":"bound","vogt_work_item":"WI-7","runs":[{"id":"r-bound","session_id":"s","started_at":"2026-08-12T05:00:00+00:00","status":"running","findings":[{"at":"2026-08-12T05:01:00+00:00","text":"looked"}]}]}]"#.to_vec(),
                );
            }
            (200, b"[]".to_vec())
        });
        let mut collector = SessionOutcomeCollector::new(
            engine,
            FixedSessions {
                records: Vec::new(),
                work_items: BTreeMap::from([("WI-7".to_string(), "fixture".to_string())]),
            },
        );
        let found = collector
            .collect(&mut context, &project(&root), &mut now)
            .findings;
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].kind, KIND_TASK_RUN);
        assert_eq!(found[0].subject_key, "task-run:r-bound");
        assert_eq!(found[0].payload["state"], "running");
        assert_eq!(found[0].payload["provisional"], true);
        assert!(found[0].payload.get("exit_code").is_none());
        assert!(found[0].payload.get("duration_seconds").is_none());
        assert_eq!(found[0].payload["findings"][0]["source"], "notify-phrase");
    }

    #[test]
    fn a_dead_engine_fails_the_collect() {
        let root = temp("dead");
        let config = crate::config::VogtConfig::default();
        let mut context = CollectorContext { config: &config };
        let mut now = || Moment::from_unix(0, 0);
        let engine = EngineClient::new("http://127.0.0.1:1", None, None);
        let mut collector = SessionOutcomeCollector::new(
            engine,
            FixedSessions {
                records: vec![record_in(&root, "ses_d", "2026-08-12T05:00:00+00:00", None)],
                work_items: BTreeMap::new(),
            },
        );
        let collected = collector.collect(&mut context, &project(&root), &mut now);
        let error = collected.error.expect("a dead engine fails the collect");
        assert!(error.to_string().contains("not answering"), "{error}");
    }
}
