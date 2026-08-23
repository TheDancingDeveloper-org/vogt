//! Provider-pluggable *workflow-engine* backend for agent tasks (#293,
//! increment 1).
//!
//! An agent task normally drives a vendor CLI in a PTY. A task that carries a
//! [`WorkflowEngineConfig`] instead hands its run to an external **workflow
//! engine** — a service that owns the agent loop, sandboxes, checkpoints and
//! gates — and Vogt tracks the run as an observation with a typed conclusion.
//! Fabro (<https://github.com/fabro-sh/fabro>) is the concrete provider this
//! increment targets; see `docs/local/FABRO_COMPARISON.md` for why Vogt
//! integrates such an engine rather than re-building its execution features.
//!
//! ## Scope of this increment
//!
//! This module is the **seam plus the Fabro client**, fully unit/integration
//! tested against a *fake* HTTP server whose responses match the documented
//! Fabro contract below. The estate has no running Fabro instance, so live
//! end-to-end validation is deliberately **deferred to increment 2**. SSE event
//! mirroring, gate bridging (#289) and checkpoint-branch collection (#283/#284)
//! also land in increment 2 — this increment tracks a run by polling.
//!
//! ## ASSUMED Fabro REST contract
//!
//! The shapes below are **assumed** from the shallow-clone reading captured in
//! `docs/local/FABRO_COMPARISON.md` (Fabro exposes REST + SSE and is an MCP
//! server; its runs end `succeeded | failed | partially_succeeded | skipped`
//! with a terminal `Conclusion` carrying timing, billing in `usd_micros`, final
//! sha and diff). They are **not yet verified against a live Fabro** — that
//! verification is increment-2 work, and the exact routes/field names may move.
//!
//! * **create run** — `POST {base}/api/runs`
//!   request  `{ "workflow": <name>, "goal": <goal>, "repo_ref": <ref?> }`
//!   response `{ "id": <run_id>, "url": <url?> }`
//! * **poll run** — `GET {base}/api/runs/{id}`
//!   response
//!   ```json
//!   {
//!     "state": "running" | "succeeded" | "failed"
//!            | "partially_succeeded" | "skipped",
//!     "conclusion": {
//!       "final_sha": "<sha>?",
//!       "diff": { "files": <u32>, "insertions": <u64>, "deletions": <u64> }?,
//!       "cost_usd_micros": <u64>?,
//!       "summary": "<text>?"
//!     }?
//!   }
//!   ```
//!
//! A `Bearer` token is sent when configured. Connection/timeout failures map to
//! [`WorkflowEngineError::Unreachable`] — the "engine is absent" case, which the
//! caller treats as non-fatal (the run is recorded errored, never a panic).

use std::future::Future;

use serde::{Deserialize, Serialize};

use crate::agent_tasks::{AgentTaskRunConclusion, AgentTaskRunOutcome, DiffStat, RunCost};
use time::OffsetDateTime;

/// How a task's runs are handed to an external workflow engine instead of a PTY
/// (#293). Present on a task means "run this in the engine"; absent means the
/// ordinary PTY path is unchanged.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowEngineConfig {
    /// Base URL of the engine, e.g. `https://fabro.internal`. The provider
    /// appends its own routes (`/api/runs`, …).
    pub engine_url: String,
    /// The workflow to run — for Fabro, the name of a `.fabro/workflows/<name>`
    /// checked into the target repo.
    pub workflow: String,
    /// A file the engine's bearer token is read from at run time, following the
    /// same token-from-file pattern as the core token (`read_token_path`). When
    /// unset the engine is called without an `Authorization` header.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_file: Option<String>,
    /// The repo ref (branch, tag, sha) the engine should run against, passed
    /// through as `repo_ref`. When unset the engine uses its own default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_ref: Option<String>,
}

/// What can go wrong talking to a workflow engine.
///
/// [`Unreachable`](WorkflowEngineError::Unreachable) is called out from the
/// other two because it is the "engine is simply not there" case the caller
/// must treat as **non-fatal**: a connection refused or a timeout records the
/// run errored with a written reason, it never aborts the scheduler.
#[derive(Debug, Clone)]
pub enum WorkflowEngineError {
    /// The engine could not be reached — connection refused, DNS failure, or a
    /// timeout. Non-fatal at the call site.
    Unreachable(String),
    /// The engine answered, but with a non-success status.
    Api { status: u16, body: String },
    /// The request could not even be built — a bad base URL, an unreadable
    /// token file, or a response that did not parse.
    Config(String),
}

impl std::fmt::Display for WorkflowEngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WorkflowEngineError::Unreachable(msg) => {
                write!(f, "workflow engine unreachable: {msg}")
            }
            WorkflowEngineError::Api { status, body } => {
                write!(f, "workflow engine returned HTTP {status}: {body}")
            }
            WorkflowEngineError::Config(msg) => write!(f, "workflow engine misconfigured: {msg}"),
        }
    }
}

impl std::error::Error for WorkflowEngineError {}

/// A run the provider created, as this seam sees it: an opaque id and an
/// optional human URL into the engine's UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderRun {
    pub run_id: String,
    pub url: Option<String>,
}

/// The state of a provider run, mapped onto Vogt's own outcome vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderRunState {
    Running,
    Succeeded,
    Failed,
    PartiallySucceeded,
    Skipped,
}

impl ProviderRunState {
    /// Whether this is a terminal state — the poller stops once it sees one.
    pub fn is_terminal(self) -> bool {
        !matches!(self, ProviderRunState::Running)
    }
}

/// The typed conclusion the engine reported for a terminal run.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProviderConclusion {
    pub final_sha: Option<String>,
    pub diff: Option<DiffStat>,
    pub cost_usd_micros: Option<u64>,
    pub summary: Option<String>,
}

/// A poll result: the current state and, for a terminal state, its conclusion.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderRunStatus {
    pub state: ProviderRunState,
    pub conclusion: Option<ProviderConclusion>,
}

/// The seam: create a run, then poll it to a terminal state. Implemented per
/// engine; [`FabroProvider`] is the only implementation this increment ships.
///
/// The crate has no `async_trait` dependency, so the async methods are written
/// with return-position `impl Future` (stable since Rust 1.75) rather than a
/// boxed future — the provider is dispatched on its concrete type here, not
/// behind `dyn`.
pub trait WorkflowProvider {
    /// Start a run of the configured workflow with a goal (and optional repo
    /// ref), returning the engine's run id.
    fn create_run(
        &self,
        goal: &str,
        repo_ref: Option<&str>,
    ) -> impl Future<Output = Result<ProviderRun, WorkflowEngineError>> + Send;

    /// Fetch the current state of a run by its engine id.
    fn poll(
        &self,
        run_id: &str,
    ) -> impl Future<Output = Result<ProviderRunStatus, WorkflowEngineError>> + Send;
}

/// The Fabro provider — a thin REST client for the assumed contract documented
/// at the top of this module.
///
/// Carries the `workflow` name alongside the base URL: the trait's `create_run`
/// signature takes a goal and a repo ref but not a workflow (a workflow is a
/// property of *this* configured backend, not of each run), so the provider
/// holds it and puts it in the create body. It is built from a
/// [`WorkflowEngineConfig`] once per run.
pub struct FabroProvider {
    base_url: String,
    workflow: String,
    token: Option<String>,
    client: reqwest::Client,
}

impl FabroProvider {
    /// Build a provider against `engine_url` running `workflow`, optionally
    /// carrying a bearer `token`. The base URL's trailing slash is trimmed so
    /// route joining is unambiguous.
    pub fn new(engine_url: &str, workflow: &str, token: Option<String>) -> Self {
        Self::with_client(engine_url, workflow, token, reqwest::Client::new())
    }

    /// Build a provider from a [`WorkflowEngineConfig`] and an already-resolved
    /// token (read from the config's `token_file` by the caller).
    pub fn from_config(cfg: &WorkflowEngineConfig, token: Option<String>) -> Self {
        Self::new(&cfg.engine_url, &cfg.workflow, token)
    }

    /// Build a provider with a caller-supplied [`reqwest::Client`] — the seam
    /// the tests inject a short-timeout client through.
    pub fn with_client(
        engine_url: &str,
        workflow: &str,
        token: Option<String>,
        client: reqwest::Client,
    ) -> Self {
        Self {
            base_url: engine_url.trim_end_matches('/').to_string(),
            workflow: workflow.to_string(),
            token: token.filter(|t| !t.trim().is_empty()),
            client,
        }
    }

    /// Attach the bearer header when a token is configured.
    fn auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.token {
            Some(token) => req.bearer_auth(token),
            None => req,
        }
    }
}

/// Classify a reqwest error: a connect or timeout failure is the "engine is not
/// there" case ([`WorkflowEngineError::Unreachable`]); anything else that got
/// far enough to be a request-shape or decode problem is a `Config` error.
fn classify_reqwest(err: &reqwest::Error) -> WorkflowEngineError {
    if err.is_connect() || err.is_timeout() {
        WorkflowEngineError::Unreachable(err.to_string())
    } else {
        WorkflowEngineError::Config(err.to_string())
    }
}

#[derive(Serialize)]
struct CreateRunBody<'a> {
    workflow: &'a str,
    goal: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    repo_ref: Option<&'a str>,
}

#[derive(Deserialize)]
struct CreateRunResp {
    id: String,
    #[serde(default)]
    url: Option<String>,
}

#[derive(Deserialize)]
struct PollResp {
    state: String,
    #[serde(default)]
    conclusion: Option<PollConclusion>,
}

#[derive(Deserialize)]
struct PollConclusion {
    #[serde(default)]
    final_sha: Option<String>,
    #[serde(default)]
    diff: Option<PollDiff>,
    #[serde(default)]
    cost_usd_micros: Option<u64>,
    #[serde(default)]
    summary: Option<String>,
}

#[derive(Deserialize)]
struct PollDiff {
    #[serde(default)]
    files: u32,
    #[serde(default)]
    insertions: u64,
    #[serde(default)]
    deletions: u64,
}

/// Parse the wire state token into a [`ProviderRunState`]. An unknown token is
/// a `Config` error rather than a silent default — a state Vogt cannot map is a
/// contract mismatch to surface in increment 2, not a run to guess about.
fn parse_state(raw: &str) -> Result<ProviderRunState, WorkflowEngineError> {
    match raw {
        "running" => Ok(ProviderRunState::Running),
        "succeeded" => Ok(ProviderRunState::Succeeded),
        "failed" => Ok(ProviderRunState::Failed),
        "partially_succeeded" => Ok(ProviderRunState::PartiallySucceeded),
        "skipped" => Ok(ProviderRunState::Skipped),
        other => Err(WorkflowEngineError::Config(format!(
            "unknown workflow-engine run state {other:?}"
        ))),
    }
}

impl WorkflowProvider for FabroProvider {
    async fn create_run(
        &self,
        goal: &str,
        repo_ref: Option<&str>,
    ) -> Result<ProviderRun, WorkflowEngineError> {
        let url = format!("{}/api/runs", self.base_url);
        let body = CreateRunBody {
            workflow: &self.workflow,
            goal,
            repo_ref,
        };
        let resp = self
            .auth(self.client.post(&url))
            .json(&body)
            .send()
            .await
            .map_err(|e| classify_reqwest(&e))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(WorkflowEngineError::Api {
                status: status.as_u16(),
                body,
            });
        }
        let parsed: CreateRunResp = resp.json().await.map_err(|e| classify_reqwest(&e))?;
        Ok(ProviderRun {
            run_id: parsed.id,
            url: parsed.url,
        })
    }

    async fn poll(&self, run_id: &str) -> Result<ProviderRunStatus, WorkflowEngineError> {
        let url = format!("{}/api/runs/{run_id}", self.base_url);
        let resp = self
            .auth(self.client.get(&url))
            .send()
            .await
            .map_err(|e| classify_reqwest(&e))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(WorkflowEngineError::Api {
                status: status.as_u16(),
                body,
            });
        }
        let parsed: PollResp = resp.json().await.map_err(|e| classify_reqwest(&e))?;
        let state = parse_state(&parsed.state)?;
        let conclusion = parsed.conclusion.map(|c| ProviderConclusion {
            final_sha: c.final_sha,
            diff: c.diff.map(|d| DiffStat {
                files: d.files,
                insertions: d.insertions,
                deletions: d.deletions,
            }),
            cost_usd_micros: c.cost_usd_micros,
            summary: c.summary,
        });
        Ok(ProviderRunStatus { state, conclusion })
    }
}

/// Map a terminal provider state and its conclusion onto Vogt's own
/// [`AgentTaskRunOutcome`] and a durable [`AgentTaskRunConclusion`] (#291).
///
/// The four terminal Fabro states line up one-to-one with existing outcome
/// variants — `succeeded → Succeeded`, `failed → Failed`,
/// `partially_succeeded → PartiallySucceeded`, `skipped → Skipped` — so nothing
/// is invented here. Vogt's `Blocked` outcome has no Fabro counterpart in this
/// increment (Fabro gates would map to it once gate bridging lands in #289) and
/// is simply never produced by this provider. A non-terminal `Running` state is
/// a caller bug and is mapped to `Failed` with a note rather than panicking.
pub fn map_conclusion(
    state: ProviderRunState,
    conclusion: Option<ProviderConclusion>,
    started: OffsetDateTime,
    finished: OffsetDateTime,
) -> (AgentTaskRunOutcome, AgentTaskRunConclusion) {
    let outcome = match state {
        ProviderRunState::Succeeded => AgentTaskRunOutcome::Succeeded,
        ProviderRunState::Failed => AgentTaskRunOutcome::Failed,
        ProviderRunState::PartiallySucceeded => AgentTaskRunOutcome::PartiallySucceeded,
        ProviderRunState::Skipped => AgentTaskRunOutcome::Skipped,
        // Not terminal — the poller only calls this on a terminal state; treat a
        // stray `Running` as a failure with the conclusion we have.
        ProviderRunState::Running => AgentTaskRunOutcome::Failed,
    };
    let duration_ms = (finished - started).whole_milliseconds().max(0) as u64;
    let conclusion = conclusion.unwrap_or_default();
    let cost = conclusion.cost_usd_micros.map(|micros| RunCost {
        total_usd: Some(micros as f64 / 1_000_000.0),
        input_tokens: None,
        output_tokens: None,
    });
    let record = AgentTaskRunConclusion {
        started,
        finished,
        duration_ms,
        outcome,
        exit_code: None,
        retries: 0,
        branch: None,
        final_sha: conclusion.final_sha,
        base_sha: None,
        diffstat: conclusion.diff,
        cost,
        findings: vec![],
    };
    (outcome, record)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        extract::Path,
        routing::{get, post},
        Json, Router,
    };
    use serde_json::{json, Value};
    use std::net::SocketAddr;
    use tokio::net::TcpListener;

    /// Spin up a fake Fabro on `127.0.0.1:0` that answers create-run with a
    /// fixed id and poll with the caller-provided terminal body. Returns the
    /// bound address; the server runs for the test's lifetime.
    async fn fake_fabro(poll_body: Value) -> SocketAddr {
        let app = Router::new()
            .route(
                "/api/runs",
                post(|Json(body): Json<Value>| async move {
                    // Echo enough to prove the request shape reached the server.
                    assert!(body.get("workflow").is_some(), "workflow in body");
                    assert!(body.get("goal").is_some(), "goal in body");
                    Json(json!({ "id": "run-123", "url": "http://fabro.test/runs/run-123" }))
                }),
            )
            .route(
                "/api/runs/{id}",
                get(move |Path(_id): Path<String>| {
                    let body = poll_body.clone();
                    async move { Json(body) }
                }),
            );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        addr
    }

    fn short_client() -> reqwest::Client {
        reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_millis(300))
            .timeout(std::time::Duration::from_secs(2))
            .build()
            .unwrap()
    }

    #[tokio::test]
    async fn create_then_poll_succeeded_maps_to_a_succeeded_conclusion() {
        let addr = fake_fabro(json!({
            "state": "succeeded",
            "conclusion": {
                "final_sha": "abc1234",
                "diff": { "files": 3, "insertions": 40, "deletions": 5 },
                "cost_usd_micros": 250_000,
                "summary": "did the thing"
            }
        }))
        .await;
        let base = format!("http://{addr}");
        let provider = FabroProvider::with_client(&base, "nightly", None, short_client());

        let created = provider
            .create_run("fix the bug", Some("main"))
            .await
            .expect("create ok");
        assert_eq!(created.run_id, "run-123");
        assert_eq!(created.url.as_deref(), Some("http://fabro.test/runs/run-123"));

        let status = provider.poll(&created.run_id).await.expect("poll ok");
        assert_eq!(status.state, ProviderRunState::Succeeded);
        assert!(status.state.is_terminal());

        let started = OffsetDateTime::UNIX_EPOCH;
        let finished = started + time::Duration::seconds(42);
        let (outcome, conclusion) =
            map_conclusion(status.state, status.conclusion, started, finished);
        assert_eq!(outcome, AgentTaskRunOutcome::Succeeded);
        assert_eq!(conclusion.final_sha.as_deref(), Some("abc1234"));
        assert_eq!(
            conclusion.diffstat,
            Some(DiffStat {
                files: 3,
                insertions: 40,
                deletions: 5,
            })
        );
        assert_eq!(conclusion.cost.and_then(|c| c.total_usd), Some(0.25));
        assert_eq!(conclusion.duration_ms, 42_000);
    }

    #[tokio::test]
    async fn poll_failed_maps_to_a_failed_conclusion() {
        let addr = fake_fabro(json!({ "state": "failed" })).await;
        let base = format!("http://{addr}");
        let provider = FabroProvider::with_client(&base, "nightly", None, short_client());

        let status = provider.poll("run-123").await.expect("poll ok");
        assert_eq!(status.state, ProviderRunState::Failed);

        let started = OffsetDateTime::UNIX_EPOCH;
        let (outcome, conclusion) =
            map_conclusion(status.state, status.conclusion, started, started);
        assert_eq!(outcome, AgentTaskRunOutcome::Failed);
        assert!(conclusion.final_sha.is_none());
        assert!(conclusion.diffstat.is_none());
    }

    #[tokio::test]
    async fn a_dead_port_is_unreachable_not_a_hard_error() {
        // Nothing is listening here — the connect must fail fast and classify
        // as Unreachable (the non-fatal case), never panic.
        let provider =
            FabroProvider::with_client("http://127.0.0.1:1", "nightly", None, short_client());
        let err = provider
            .create_run("goal", None)
            .await
            .expect_err("dead port must error");
        assert!(
            matches!(err, WorkflowEngineError::Unreachable(_)),
            "expected Unreachable, got {err:?}"
        );

        let err = provider.poll("run-123").await.expect_err("dead port must error");
        assert!(
            matches!(err, WorkflowEngineError::Unreachable(_)),
            "expected Unreachable, got {err:?}"
        );
    }

    #[test]
    fn partially_succeeded_and_skipped_map_cleanly() {
        let started = OffsetDateTime::UNIX_EPOCH;
        let (partial, _) =
            map_conclusion(ProviderRunState::PartiallySucceeded, None, started, started);
        assert_eq!(partial, AgentTaskRunOutcome::PartiallySucceeded);
        let (skipped, _) = map_conclusion(ProviderRunState::Skipped, None, started, started);
        assert_eq!(skipped, AgentTaskRunOutcome::Skipped);
    }

    #[test]
    fn config_round_trips_through_serde() {
        let cfg = WorkflowEngineConfig {
            engine_url: "https://fabro.internal".into(),
            workflow: "nightly-audit".into(),
            token_file: Some("/run/secrets/fabro".into()),
            repo_ref: Some("main".into()),
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let back: WorkflowEngineConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.engine_url, cfg.engine_url);
        assert_eq!(back.workflow, cfg.workflow);
        assert_eq!(back.token_file, cfg.token_file);
        assert_eq!(back.repo_ref, cfg.repo_ref);

        // The optional fields drop out of the wire form when unset.
        let minimal: WorkflowEngineConfig =
            serde_json::from_str(r#"{"engine_url":"http://e","workflow":"w"}"#).unwrap();
        assert!(minimal.token_file.is_none());
        assert!(minimal.repo_ref.is_none());
        let wire = serde_json::to_value(&minimal).unwrap();
        assert!(wire.get("token_file").is_none());
        assert!(wire.get("repo_ref").is_none());
    }
}
