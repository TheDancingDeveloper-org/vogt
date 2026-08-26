//! Provider-pluggable *workflow-engine* backend for agent tasks (#293).
//!
//! An agent task normally drives a vendor CLI in a PTY. A task that carries a
//! [`WorkflowEngineConfig`] instead hands its run to an external **workflow
//! engine** — a service that owns the agent loop, sandboxes, checkpoints and
//! gates — and Vogt tracks the run as an observation with a typed conclusion.
//! The provider supports the vendor-neutral REST + SSE contract used by the
//! original seam and an explicit Fabro-compatible backend. Vogt integrates
//! these engines rather than re-building their execution features (sandboxes,
//! checkpoints, gates, billing).
//!
//! ## Scope
//!
//! This module is the **seam plus a concrete HTTP client**, fully unit/integration
//! tested against a *fake* HTTP server whose responses match the assumed
//! contract below. The implementation tracks runs over polling or SSE, mirrors
//! progress, checkpoints, and approval gates, forwards gate answers and steering,
//! and preserves checkpoint/gate evidence when SSE falls back to polling.
//!
//! The generic contract below remains provisional. Fabro's concrete manifest
//! routes and payloads are documented in [`FabroConfig`], covered by wire-shape
//! tests, and live-smoked against Fabro 0.254. Its newer immutable workflow
//! intent lane was live-smoked against Fabro 0.337.0-nightly.1 on 2026-08-26
//! against Vogt's `dev` branch, covering version-backed Git-target creation,
//! run start, durable run-branch/timeline checkpoints, attach SSE, and a
//! terminal conclusion. The generic contract remains separate and unverified.
//!
//! ## PROVISIONAL generic workflow-engine REST contract
//!
//! The shapes below are **assumed** from a reference workflow engine exposing
//! REST + SSE: its runs end `succeeded | failed | partially_succeeded | skipped | blocked`
//! with a terminal `Conclusion` carrying timing, billing in `usd_micros`, final
//! sha and diff. They are **not yet verified against a live engine** — that
//! verification is deferred to a live smoke, and the exact routes/field names
//! may move.
//!
//! * **create run** — `POST {base}/api/runs`
//!   request  `{ "workflow": <name>, "goal": <goal>, "repo_ref": <ref?> }`
//!   response `{ "id": <run_id>, "url": <url?> }`
//! * **poll run** — `GET {base}/api/runs/{id}`
//!   response
//!   ```json
//!   {
//!     "state": "running" | "succeeded" | "failed"
//!            | "partially_succeeded" | "skipped" | "blocked",
//!     "conclusion": {
//!       "final_sha": "<sha>?",
//!       "diff": { "files": <u32>, "insertions": <u64>, "deletions": <u64> }?,
//!       "cost_usd_micros": <u64>?,
//!       "summary": "<text>?"
//!     }?
//!   }
//!   ```
//! * **stream run events** — `GET {base}/api/runs/{id}/events`
//!   with `Accept: text/event-stream`. A `text/event-stream` (SSE) response
//!   whose `data:` lines carry a JSON envelope. The exact envelope schema is
//!   **not specified** by the reference (only "one event envelope everywhere
//!   (SSE, NDJSON, export)" is documented), so the shape below is a
//!   **PROVISIONAL envelope, unverified against a live engine** — a live smoke
//!   test in a later slice may rename these fields. See [`SseEnvelope`].
//!   ```json
//!   {
//!     "type": "<event kind>",          // e.g. "node.started", "run.completed"
//!     "message": "<human text>?",
//!     "node_id": "<step/node id>?",
//!     "state": "running" | "succeeded" | "failed"
//!            | "partially_succeeded" | "skipped",   // present on a terminal event
//!     "checkpoint_branch": "<branch>?",  // the stage's git checkpoint branch (#284)
//!     "ts_ms": <u64>?                    // event time, Unix epoch milliseconds
//!   }
//!   ```
//! * **answer gate** — `POST {base}/api/runs/{id}/gates/{gate_id}/answer`
//!   request `{ "option": <zero-based option index> }`
//! * **steer run** — `POST {base}/api/runs/{id}/steer`
//!   request `{ "text": <text>, "interrupt": <bool> }`
//!
//! A `Bearer` token is sent when configured. Connection/timeout failures map to
//! [`WorkflowEngineError::Unreachable`] — the "engine is absent" case, which the
//! caller treats as non-fatal (the run is recorded errored, never a panic).

//! ## Verified Fabro 0.254 manifest contract
//!
//! The compatibility lane sends a self-contained manifest to
//! `POST {base}/api/v1/runs`, then starts it with
//! `POST {base}/api/v1/runs/{id}/start`. The manifest contains the explicit
//! `workflow_source`, `workflow_path`, folder target, goal, model/provider,
//! environment, and optional dry-run flag. Lifecycle polling uses
//! `GET {base}/api/v1/runs/{id}` and terminal diff enrichment may read the
//! documented state projection at `/state`; the ordered event stream is
//! `GET {base}/api/v1/runs/{id}/attach?since_seq=1`. Questions and steering use
//! the corresponding `/questions` and `/steer` routes. This path was exercised
//! with a local Fabro 0.254 server on 2026-08-26; the ignored live test below
//! documents the required environment variables for repeating that smoke. A
//! folder target is intentionally reported without checkpoint branches: the
//! Fabro 0.254 contract executes it in place and explicitly does not create
//! Fabro Git checkpoints.
//!
//! The immutable workflow-intent lane was live-smoked against Fabro
//! 0.337.0-nightly.1 on 2026-08-26 using the public Vogt `dev` target. The
//! run cloned the repository, completed successfully, exposed its
//! `fabro/run/<run-id>` branch and `start@1` timeline checkpoint, and replayed
//! `run.started`, `stage.completed`, `checkpoint.completed`, and
//! `run.completed` records through `/attach`. The no-model smoke incurred no
//! charge; billing projection when Fabro supplies a value remains covered by
//! the wire-shape tests below.

use std::future::Future;
use std::pin::Pin;

use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use serde::{
    de::{DeserializeOwned, Deserializer},
    Deserialize, Serialize,
};

use crate::agent_tasks::{AgentTaskRunConclusion, AgentTaskRunOutcome, DiffStat, RunCost};
use time::OffsetDateTime;

/// How a task's runs are handed to an external workflow engine instead of a PTY
/// (#293). Present on a task means "run this in the engine"; absent means the
/// ordinary PTY path is unchanged.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowEngineConfig {
    /// Base URL of the engine, e.g. `https://engine.internal`. The provider
    /// appends its own routes (`/api/runs`, …).
    pub engine_url: String,
    /// The workflow to run — e.g. the name of a workflow definition checked
    /// into the target repo that the engine runs.
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
    /// An explicit Fabro-compatible provider configuration. When present the
    /// provider uses Fabro's versioned workflow-intent API; when absent the
    /// original vendor-neutral REST/SSE contract remains in force.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fabro: Option<FabroConfig>,
}

/// Configuration required by the Fabro run API. The workflow version and
/// target are deliberately separate: a caller cannot accidentally turn a
/// branch/ref string into an implicit workflow source or target.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FabroConfig {
    /// SHA-256 identity of an immutable workflow version stored by Fabro.
    /// When omitted, `workflow_source` selects the compatible manifest lane.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_version_id: Option<String>,
    /// Inline Graphviz workflow source for Fabro servers that expose the
    /// self-contained manifest create API (including Fabro 0.254).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_source: Option<String>,
    /// Relative workflow path used by the manifest lane. If omitted, the
    /// provider uses `.fabro/workflows/<workflow>/workflow.fabro`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_path: Option<String>,
    /// Workspace target on the Fabro server.
    pub target: FabroTarget,
    /// Optional Fabro environment catalog id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment_id: Option<String>,
    /// Optional model override passed in the run intent args.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Optional model-provider override passed in the run intent args.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Manifest-lane execution mode. Keep this false for real runs; the
    /// opt-in live smoke sets it true so no external model is contacted.
    #[serde(default)]
    pub dry_run: bool,
}

/// A target understood by Fabro's `RunIntent` API.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind")]
#[serde(rename_all = "snake_case")]
pub enum FabroTarget {
    /// A public GitHub repository and required branch.
    Git {
        repo: String,
        branch: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sha: Option<String>,
    },
    /// An existing absolute directory on the Fabro server.
    Folder { path: String },
    /// An empty workspace target.
    None,
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
    Blocked,
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

/// One checkpoint observation reported by a poll response. The timestamp is
/// optional because an engine may only expose the branch name in its summary;
/// the tracker supplies its receipt time in that case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderCheckpoint {
    pub branch: String,
    pub step_id: Option<String>,
    pub at: Option<OffsetDateTime>,
}

/// A poll result: the current state, any checkpoint observations, and, for a
/// terminal state, its conclusion.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderRunStatus {
    pub state: ProviderRunState,
    pub checkpoints: Vec<ProviderCheckpoint>,
    pub gates: Vec<ProviderGate>,
    pub conclusion: Option<ProviderConclusion>,
}

/// An approval gate reported by a workflow engine. The provider id is kept
/// separately from Vogt's UUID so an engine may use any stable string while
/// the existing `/gates/{uuid}/answer` route remains unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderGate {
    pub id: String,
    pub question: String,
    pub options: Vec<ProviderGateOption>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderGateOption {
    pub label: String,
    pub input: String,
    pub approve: bool,
    /// Provider-native option key, used by providers whose answer API is
    /// key-based rather than index-based. It is not exposed as a Vogt gate
    /// option field; the provider id/key mapping stays inside this seam.
    pub key: Option<String>,
}

/// One event mirrored off the engine's live SSE stream (#293, increment 2).
///
/// A run's progress made observable without waiting for the poll interval: each
/// record the engine streams is parsed into one of these. `kind` is the event's
/// name (the SSE `event:` field, or the envelope's `type`); `message` and
/// `step_id` carry the human text and the node the event is about when present;
/// `at` is the event's own timestamp when the envelope carried one. When the
/// event signals the run has *ended*, `terminal_state` is the mapped
/// [`ProviderRunState`] — the tracker uses that to record the conclusion the
/// instant the stream reports it, instead of on the next poll.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderEvent {
    /// The event's kind/name, e.g. `node.started`, `run.completed`. Falls back
    /// to `"message"` when neither the SSE `event:` field nor an envelope
    /// `type` was present.
    pub kind: String,
    /// A human-readable line for the event, when the envelope carried one.
    pub message: Option<String>,
    /// The step/node the event is about, when the envelope named one.
    pub step_id: Option<String>,
    /// The event's own timestamp, when the envelope carried one.
    pub at: Option<OffsetDateTime>,
    /// The mapped terminal state, present only on an event that signals the run
    /// has ended. `None` for an in-flight progress event.
    pub terminal_state: Option<ProviderRunState>,
    /// The per-stage git checkpoint branch this event reports, when the envelope
    /// named one (#284). A workflow engine writes a checkpoint branch per stage
    /// as it runs; the event that announces a stage carries that branch's name,
    /// and the tracker collects it as a run observation with provenance and age.
    /// `None` for an event that reports no checkpoint — most events — which is
    /// the ordinary, non-fatal case.
    pub checkpoint_branch: Option<String>,
    /// An approval gate opened by this event, when the engine supplied a
    /// complete gate payload. Malformed/incomplete gate payloads remain plain
    /// progress events rather than creating an unanswerable local gate.
    pub gate: Option<ProviderGate>,
}

/// A live stream of a run's [`ProviderEvent`]s. Boxed (rather than an
/// `impl Stream` associated type) so the trait method's return type is a plain
/// concrete type — the crate has no `async_trait`, and a nested `impl Trait`
/// inside a future's `Output` is not expressible. `Send` so the tracker can own
/// it inside a `tokio::spawn`.
pub type ProviderEventStream =
    Pin<Box<dyn Stream<Item = Result<ProviderEvent, WorkflowEngineError>> + Send>>;

/// The seam: create a run, then poll it to a terminal state. Implemented per
/// engine; [`HttpWorkflowProvider`] is the only implementation this increment ships.
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

    /// Subscribe to a run's live event stream (#293, increment 2), yielding
    /// typed [`ProviderEvent`]s until the engine ends the stream. The future
    /// resolves once the subscription is established (headers received); the
    /// returned stream then yields events as they arrive.
    ///
    /// Absence is the caller's concern, not this method's: an unreachable engine
    /// or a non-success status is an `Err` here, and a stream that breaks mid-way
    /// yields an `Err` item — in both cases the caller degrades to polling rather
    /// than failing the run.
    fn stream_events(
        &self,
        run_id: &str,
    ) -> impl Future<Output = Result<ProviderEventStream, WorkflowEngineError>> + Send;

    /// Answer an approval gate in the external run. Vogt records its local
    /// gate only after this succeeds, so a failed upstream request cannot make
    /// the PWA display a decision the provider never received.
    fn answer_gate(
        &self,
        run_id: &str,
        gate_id: &str,
        option_index: usize,
    ) -> impl Future<Output = Result<(), WorkflowEngineError>> + Send;

    /// Queue steering text in the external run. `interrupt` has provider
    /// semantics equivalent to Vogt's PTY Ctrl-C-before-text operation.
    fn steer(
        &self,
        run_id: &str,
        text: &str,
        interrupt: bool,
    ) -> impl Future<Output = Result<(), WorkflowEngineError>> + Send;
}

/// The HTTP workflow-engine provider — a thin REST client for the assumed contract documented
/// at the top of this module.
///
/// Carries the `workflow` name alongside the base URL: the trait's `create_run`
/// signature takes a goal and a repo ref but not a workflow (a workflow is a
/// property of *this* configured backend, not of each run), so the provider
/// holds it and puts it in the create body. It is built from a
/// [`WorkflowEngineConfig`] once per run.
#[derive(Clone)]
pub struct HttpWorkflowProvider {
    base_url: String,
    workflow: String,
    token: Option<String>,
    client: reqwest::Client,
    fabro: Option<FabroConfig>,
}

impl HttpWorkflowProvider {
    /// Build a provider against `engine_url` running `workflow`, optionally
    /// carrying a bearer `token`. The base URL's trailing slash is trimmed so
    /// route joining is unambiguous.
    pub fn new(engine_url: &str, workflow: &str, token: Option<String>) -> Self {
        Self::with_client(engine_url, workflow, token, reqwest::Client::new())
    }

    /// Build a provider from a [`WorkflowEngineConfig`] and an already-resolved
    /// token (read from the config's `token_file` by the caller).
    pub fn from_config(cfg: &WorkflowEngineConfig, token: Option<String>) -> Self {
        let mut provider = Self::new(&cfg.engine_url, &cfg.workflow, token);
        provider.fabro = cfg.fabro.clone();
        provider
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
            fabro: None,
        }
    }

    /// Attach the bearer header when a token is configured.
    fn auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.token {
            Some(token) => req.bearer_auth(token),
            None => req,
        }
    }

    fn is_fabro(&self) -> bool {
        self.fabro.is_some()
    }

    async fn create_fabro_run(
        &self,
        cfg: &FabroConfig,
        goal: &str,
    ) -> Result<ProviderRun, WorkflowEngineError> {
        validate_fabro_config(cfg)?;
        let url = format!("{}/api/v1/runs", self.base_url);
        let resp = if let Some(workflow_version_id) = &cfg.workflow_version_id {
            let body = FabroRunIntent {
                workflow_version_id: workflow_version_id.clone(),
                target: cfg.target.clone(),
                args: FabroRunArgs {
                    model: cfg.model.clone(),
                    provider: cfg.provider.clone(),
                },
                goal: goal.to_string(),
                title: (!self.workflow.trim().is_empty()).then(|| self.workflow.clone()),
                environment_id: cfg.environment_id.clone(),
            };
            self.auth(self.client.post(&url))
                .json(&body)
                .send()
                .await
                .map_err(|e| classify_reqwest(&e))?
        } else {
            let body = fabro_manifest(cfg, &self.workflow, goal)?;
            self.auth(self.client.post(&url))
                .json(&body)
                .send()
                .await
                .map_err(|e| classify_reqwest(&e))?
        };
        let created: FabroCreateRunResp = parse_json_success(resp).await?;
        if created.id.trim().is_empty() {
            return Err(WorkflowEngineError::Config(
                "Fabro create response contained an empty run id".into(),
            ));
        }

        let start_url = format!("{}/api/v1/runs/{}/start", self.base_url, created.id);
        let response = self
            .auth(self.client.post(&start_url))
            .json(&serde_json::json!({}))
            .send()
            .await
            .map_err(|e| classify_reqwest(&e))?;
        ensure_success(response).await?;
        Ok(ProviderRun {
            run_id: created.id,
            url: created.links.web,
        })
    }

    async fn poll_fabro_run(&self, run_id: &str) -> Result<ProviderRunStatus, WorkflowEngineError> {
        let url = format!("{}/api/v1/runs/{run_id}", self.base_url);
        let resp = self
            .auth(self.client.get(&url))
            .send()
            .await
            .map_err(|e| classify_reqwest(&e))?;
        let parsed: FabroRunResp = parse_json_success(resp).await?;
        let state = fabro_state(&parsed.lifecycle.status)?;
        let checkpoints = self.poll_fabro_checkpoints(run_id).await?;
        let gates = self.poll_fabro_questions(run_id).await?;
        let conclusion = if state.is_terminal() {
            self.fabro_conclusion(run_id, &parsed).await?
        } else {
            fabro_conclusion(&parsed)
        };
        Ok(ProviderRunStatus {
            state,
            checkpoints,
            gates,
            conclusion,
        })
    }

    /// Read Fabro's durable checkpoint timeline and associate it with the
    /// provider's run branch. The timeline is authoritative for which stages
    /// checkpointed; the branch comes from the run projection's `start` record.
    /// Folder runs have no run branch by contract, so they deliberately produce
    /// no Vogt checkpoint observations.
    async fn poll_fabro_checkpoints(
        &self,
        run_id: &str,
    ) -> Result<Vec<ProviderCheckpoint>, WorkflowEngineError> {
        let state_url = format!("{}/api/v1/runs/{run_id}/state", self.base_url);
        let state_response = match self.auth(self.client.get(&state_url)).send().await {
            Ok(response) => response,
            Err(_) => return Ok(vec![]),
        };
        if !state_response.status().is_success() {
            return Ok(vec![]);
        }
        let state: FabroStateResp = match state_response.json().await {
            Ok(state) => state,
            Err(_) => return Ok(vec![]),
        };
        let Some(branch) = state
            .start
            .and_then(|start| start.run_branch)
            .filter(|branch| !branch.trim().is_empty())
        else {
            return Ok(vec![]);
        };

        let timeline_url = format!("{}/api/v1/runs/{run_id}/timeline", self.base_url);
        let timeline_response = match self.auth(self.client.get(&timeline_url)).send().await {
            Ok(response) => response,
            Err(_) => return Ok(vec![]),
        };
        if !timeline_response.status().is_success() {
            return Ok(vec![]);
        }
        let timeline: Vec<FabroTimelineEntry> = match timeline_response.json().await {
            Ok(timeline) => timeline,
            Err(_) => return Ok(vec![]),
        };
        Ok(timeline
            .into_iter()
            .filter_map(|entry| {
                let node_name = entry.node_name.trim();
                if node_name.is_empty() {
                    return None;
                }
                let step_id = if entry.visit > 0 {
                    format!("{node_name}@{}", entry.visit)
                } else {
                    node_name.to_string()
                };
                Some(ProviderCheckpoint {
                    branch: branch.clone(),
                    step_id: Some(step_id),
                    at: None,
                })
            })
            .collect())
    }

    async fn fabro_conclusion(
        &self,
        run_id: &str,
        run: &FabroRunResp,
    ) -> Result<Option<ProviderConclusion>, WorkflowEngineError> {
        let mut conclusion = fabro_conclusion(run);
        let needs_state_diff = conclusion
            .as_ref()
            .map(|value| value.diff.is_none())
            .unwrap_or(true);
        if needs_state_diff {
            let url = format!("{}/api/v1/runs/{run_id}/state", self.base_url);
            let response = self
                .auth(self.client.get(&url))
                .send()
                .await
                .map_err(|e| classify_reqwest(&e))?;
            if response.status().is_success() {
                let state: FabroStateResp = response
                    .json()
                    .await
                    .map_err(|e| WorkflowEngineError::Config(e.to_string()))?;
                if let Some(state_conclusion) = state.conclusion {
                    let diff = state_conclusion
                        .diff
                        .as_ref()
                        .and_then(FabroDiffPayload::as_stat)
                        .filter(|diff| diff.files > 0 || diff.insertions > 0 || diff.deletions > 0);
                    let state_sha = state_conclusion.final_git_commit_sha;
                    let state_cost = state_conclusion
                        .billing
                        .and_then(|value| value.total_usd_micros);
                    if diff.is_none() && state_sha.is_none() && state_cost.is_none() {
                        return Ok(conclusion);
                    }
                    let value = conclusion.get_or_insert_with(|| ProviderConclusion {
                        summary: run
                            .lifecycle
                            .status
                            .reason
                            .as_ref()
                            .map(|reason| format!("Fabro run {reason}")),
                        ..ProviderConclusion::default()
                    });
                    if let Some(diff) = diff {
                        value.diff = Some(diff);
                    }
                    if value.final_sha.is_none() {
                        value.final_sha = state_sha;
                    }
                    if value.cost_usd_micros.is_none() {
                        value.cost_usd_micros = state_cost;
                    }
                }
            }
        }
        Ok(conclusion)
    }

    async fn poll_fabro_questions(
        &self,
        run_id: &str,
    ) -> Result<Vec<ProviderGate>, WorkflowEngineError> {
        let url = format!("{}/api/v1/runs/{run_id}/questions", self.base_url);
        let resp = self
            .auth(self.client.get(&url))
            .send()
            .await
            .map_err(|e| classify_reqwest(&e))?;
        let parsed: FabroQuestionList = parse_json_success(resp).await?;
        Ok(parsed
            .data
            .into_iter()
            .filter_map(FabroQuestion::into_provider)
            .collect())
    }

    async fn answer_fabro_question(
        &self,
        run_id: &str,
        question_id: &str,
        option_index: usize,
    ) -> Result<(), WorkflowEngineError> {
        let questions = self.poll_fabro_questions(run_id).await?;
        let question = questions
            .into_iter()
            .find(|question| question.id == question_id)
            .ok_or_else(|| {
                WorkflowEngineError::Config(format!(
                    "Fabro question {question_id:?} is not pending"
                ))
            })?;
        let option = question.options.get(option_index).ok_or_else(|| {
            WorkflowEngineError::Config(format!(
                "Fabro question {question_id:?} has no option {option_index}"
            ))
        })?;
        let body = if matches!(option.key.as_deref(), Some("yes" | "no")) {
            serde_json::json!({ "kind": option.key })
        } else {
            serde_json::json!({ "kind": "selected", "option_key": option.key })
        };
        let url = format!(
            "{}/api/v1/runs/{run_id}/questions/{question_id}/answer",
            self.base_url
        );
        let response = self
            .auth(self.client.post(&url))
            .json(&body)
            .send()
            .await
            .map_err(|e| classify_reqwest(&e))?;
        ensure_success(response).await
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

fn validate_fabro_config(cfg: &FabroConfig) -> Result<(), WorkflowEngineError> {
    match (&cfg.workflow_version_id, &cfg.workflow_source) {
        (Some(id), None) if id.len() == 64 && id.chars().all(|c| c.is_ascii_hexdigit()) => {}
        (Some(_), None) => {
            return Err(WorkflowEngineError::Config(
                "Fabro workflow_version_id must be a 64-character hexadecimal SHA-256 id".into(),
            ));
        }
        (None, Some(source)) if !source.trim().is_empty() => {}
        (None, Some(_)) => {
            return Err(WorkflowEngineError::Config(
                "Fabro workflow_source must not be empty".into(),
            ));
        }
        (Some(_), Some(_)) => {
            return Err(WorkflowEngineError::Config(
                "Fabro configuration must choose workflow_version_id or workflow_source, not both"
                    .into(),
            ));
        }
        (None, None) => {
            return Err(WorkflowEngineError::Config(
                "Fabro configuration requires workflow_version_id or workflow_source".into(),
            ));
        }
    }
    if cfg.workflow_version_id.is_none() && !matches!(cfg.target, FabroTarget::Folder { .. }) {
        return Err(WorkflowEngineError::Config(
            "Fabro manifest mode requires a folder target; git and none targets require workflow_version_id".into(),
        ));
    }
    match &cfg.target {
        FabroTarget::Git { repo, branch, sha } => {
            if repo.trim().is_empty() || branch.trim().is_empty() {
                return Err(WorkflowEngineError::Config(
                    "Fabro git target requires repo and branch".into(),
                ));
            }
            if let Some(sha) = sha {
                if sha.len() != 40 || !sha.chars().all(|c| c.is_ascii_hexdigit()) {
                    return Err(WorkflowEngineError::Config(
                        "Fabro git target sha must be a 40-character hexadecimal commit".into(),
                    ));
                }
            }
        }
        FabroTarget::Folder { path } if path.trim().is_empty() => {
            return Err(WorkflowEngineError::Config(
                "Fabro folder target path must not be empty".into(),
            ));
        }
        FabroTarget::Folder { path } if !path.starts_with('/') => {
            return Err(WorkflowEngineError::Config(
                "Fabro folder target path must be absolute".into(),
            ));
        }
        FabroTarget::Folder { .. } | FabroTarget::None => {}
    }
    Ok(())
}

fn fabro_manifest(
    cfg: &FabroConfig,
    workflow: &str,
    goal: &str,
) -> Result<serde_json::Value, WorkflowEngineError> {
    let source = cfg.workflow_source.as_deref().ok_or_else(|| {
        WorkflowEngineError::Config("Fabro manifest mode requires workflow_source".into())
    })?;
    let workflow_name = workflow.trim();
    if workflow_name.is_empty() {
        return Err(WorkflowEngineError::Config(
            "Fabro manifest mode requires a workflow label".into(),
        ));
    }
    let workflow_path = cfg
        .workflow_path
        .clone()
        .unwrap_or_else(|| format!(".fabro/workflows/{workflow_name}/workflow.fabro"));
    let target_identifier = workflow_name.to_string();
    let mut args = serde_json::Map::new();
    if let Some(model) = &cfg.model {
        args.insert("model".into(), serde_json::Value::String(model.clone()));
    }
    if let Some(provider) = &cfg.provider {
        args.insert(
            "provider".into(),
            serde_json::Value::String(provider.clone()),
        );
    }
    if let Some(environment_id) = &cfg.environment_id {
        args.insert(
            "environment".into(),
            serde_json::Value::String(environment_id.clone()),
        );
    }
    if cfg.dry_run {
        args.insert("dry_run".into(), serde_json::Value::Bool(true));
    }
    let mut workflows = serde_json::Map::new();
    workflows.insert(
        workflow_path.clone(),
        serde_json::json!({
            "source": source,
            "files": {}
        }),
    );
    Ok(serde_json::json!({
        "version": 1,
        "cwd": match &cfg.target {
            FabroTarget::Folder { path } => path,
            _ => "/",
        },
        "title": workflow_name,
        "goal": {"type": "value", "text": goal},
        "args": serde_json::Value::Object(args),
        "target": {"identifier": target_identifier, "path": workflow_path},
        "workflows": serde_json::Value::Object(workflows)
    }))
}

async fn parse_json_success<T: DeserializeOwned>(
    response: reqwest::Response,
) -> Result<T, WorkflowEngineError> {
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(WorkflowEngineError::Api {
            status: status.as_u16(),
            body,
        });
    }
    response
        .json()
        .await
        .map_err(|error| WorkflowEngineError::Config(error.to_string()))
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

#[derive(Serialize)]
struct FabroRunArgs {
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    provider: Option<String>,
}

#[derive(Serialize)]
struct FabroRunIntent {
    workflow_version_id: String,
    target: FabroTarget,
    args: FabroRunArgs,
    goal: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    environment_id: Option<String>,
}

#[derive(Deserialize)]
struct FabroCreateRunResp {
    id: String,
    #[serde(default)]
    links: FabroLinks,
}

#[derive(Deserialize, Default)]
struct FabroLinks {
    #[serde(default)]
    web: Option<String>,
}

#[derive(Deserialize)]
struct FabroRunResp {
    lifecycle: FabroLifecycle,
    #[serde(default)]
    billing: Option<FabroBilling>,
    #[serde(default)]
    diff: Option<FabroDiffPayload>,
    #[serde(default)]
    conclusion: Option<FabroConclusion>,
    #[serde(default)]
    final_git_commit_sha: Option<String>,
}

#[derive(Deserialize)]
struct FabroStateResp {
    #[serde(default)]
    start: Option<FabroStartRecord>,
    #[serde(default)]
    conclusion: Option<FabroStateConclusion>,
}

#[derive(Deserialize)]
struct FabroStartRecord {
    #[serde(default)]
    run_branch: Option<String>,
}

#[derive(Deserialize)]
struct FabroTimelineEntry {
    node_name: String,
    #[serde(default)]
    visit: u32,
}

#[derive(Deserialize)]
struct FabroStateConclusion {
    #[serde(default)]
    diff: Option<FabroDiffPayload>,
    #[serde(default)]
    billing: Option<FabroBilling>,
    #[serde(default)]
    final_git_commit_sha: Option<String>,
}

#[derive(Deserialize)]
struct FabroLifecycle {
    status: FabroStatus,
}

#[derive(Deserialize)]
struct FabroStatus {
    kind: String,
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Deserialize)]
struct FabroBilling {
    #[serde(default)]
    total_usd_micros: Option<u64>,
}

#[derive(Deserialize)]
struct FabroDiff {
    #[serde(default)]
    summary: Option<FabroDiffSummary>,
}

enum FabroDiffPayload {
    /// Canonical run responses expose the summary directly.
    Summary(FabroDiffSummary),
    /// State/conclusion projections wrap it in `summary`.
    Detailed(FabroDiff),
}

impl<'de> Deserialize<'de> for FabroDiffPayload {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        if value.get("summary").is_some() {
            serde_json::from_value::<FabroDiff>(value)
                .map(Self::Detailed)
                .map_err(serde::de::Error::custom)
        } else {
            serde_json::from_value::<FabroDiffSummary>(value)
                .map(Self::Summary)
                .map_err(serde::de::Error::custom)
        }
    }
}

impl FabroDiffPayload {
    fn as_stat(&self) -> Option<DiffStat> {
        match self {
            Self::Summary(summary) => Some(DiffStat {
                files: summary.files_changed,
                insertions: summary.additions,
                deletions: summary.deletions,
            }),
            Self::Detailed(diff) => diff.summary.as_ref().map(|summary| DiffStat {
                files: summary.files_changed,
                insertions: summary.additions,
                deletions: summary.deletions,
            }),
        }
    }
}

#[derive(Deserialize)]
struct FabroDiffSummary {
    #[serde(default)]
    files_changed: u32,
    #[serde(default)]
    additions: u64,
    #[serde(default)]
    deletions: u64,
}

#[derive(Deserialize)]
struct FabroConclusion {
    #[serde(default)]
    final_git_commit_sha: Option<String>,
    #[serde(default)]
    final_sha: Option<String>,
    #[serde(default)]
    billing: Option<FabroBilling>,
    #[serde(default)]
    diff: Option<FabroDiffPayload>,
}

#[derive(Deserialize)]
struct FabroQuestionList {
    #[serde(default)]
    data: Vec<FabroQuestion>,
}

#[derive(Deserialize)]
struct FabroQuestion {
    id: String,
    text: String,
    #[serde(default)]
    question_type: String,
    #[serde(default)]
    options: Vec<FabroQuestionOption>,
}

#[derive(Deserialize)]
struct FabroQuestionOption {
    key: String,
    label: String,
}

impl FabroQuestion {
    fn into_provider(self) -> Option<ProviderGate> {
        let question = self.text.trim().to_string();
        if self.id.trim().is_empty() || question.is_empty() {
            return None;
        }
        let mut options = self
            .options
            .into_iter()
            .filter_map(|option| {
                let key = option.key.trim().to_string();
                let label = option.label.trim().to_string();
                if key.is_empty() || label.is_empty() {
                    return None;
                }
                Some(ProviderGateOption {
                    approve: matches!(key.as_str(), "yes" | "approve" | "accept"),
                    input: key.clone(),
                    label,
                    key: Some(key),
                })
            })
            .collect::<Vec<_>>();
        if options.is_empty() && matches!(self.question_type.as_str(), "yes_no" | "confirmation") {
            options = vec![
                ProviderGateOption {
                    label: "Yes".into(),
                    input: "yes".into(),
                    approve: true,
                    key: Some("yes".into()),
                },
                ProviderGateOption {
                    label: "No".into(),
                    input: "no".into(),
                    approve: false,
                    key: Some("no".into()),
                },
            ];
        }
        (!options.is_empty()).then_some(ProviderGate {
            id: self.id,
            question,
            options,
        })
    }
}

#[derive(Deserialize)]
struct FabroSseEnvelope {
    #[serde(default)]
    event: Option<String>,
    #[serde(default)]
    ts: Option<String>,
    #[serde(default)]
    node_id: Option<String>,
    #[serde(default)]
    properties: serde_json::Value,
}

fn fabro_state(status: &FabroStatus) -> Result<ProviderRunState, WorkflowEngineError> {
    match status.kind.as_str() {
        "submitted" | "pending" | "runnable" | "starting" | "running" | "paused" | "removing" => {
            Ok(ProviderRunState::Running)
        }
        "blocked" => Ok(ProviderRunState::Blocked),
        "succeeded" => match status.reason.as_deref() {
            Some("partial_success") => Ok(ProviderRunState::PartiallySucceeded),
            _ => Ok(ProviderRunState::Succeeded),
        },
        "failed" | "dead" => Ok(ProviderRunState::Failed),
        other => Err(WorkflowEngineError::Config(format!(
            "unknown Fabro run status {other:?}"
        ))),
    }
}

fn fabro_conclusion(run: &FabroRunResp) -> Option<ProviderConclusion> {
    let conclusion = run.conclusion.as_ref();
    let diff = run.diff.as_ref().and_then(FabroDiffPayload::as_stat);
    let diff = diff.or_else(|| {
        conclusion.and_then(|value| value.diff.as_ref().and_then(FabroDiffPayload::as_stat))
    });
    let final_sha = conclusion
        .and_then(|c| {
            c.final_git_commit_sha
                .clone()
                .or_else(|| c.final_sha.clone())
        })
        .or_else(|| run.final_git_commit_sha.clone());
    let cost_usd_micros = run
        .billing
        .as_ref()
        .and_then(|b| b.total_usd_micros)
        .or_else(|| conclusion.and_then(|c| c.billing.as_ref()?.total_usd_micros));
    (diff.is_some()
        || final_sha.is_some()
        || cost_usd_micros.is_some()
        || run.lifecycle.status.reason.is_some())
    .then_some(ProviderConclusion {
        final_sha,
        diff,
        cost_usd_micros,
        summary: run
            .lifecycle
            .status
            .reason
            .as_ref()
            .map(|reason| format!("Fabro run {reason}")),
    })
}

fn fabro_sse_state(properties: &serde_json::Value) -> Option<ProviderRunState> {
    let status = properties
        .get("status")
        .and_then(serde_json::Value::as_str)?;
    match status {
        "succeeded" | "completed" => Some(ProviderRunState::Succeeded),
        "partial_success" | "partially_succeeded" => Some(ProviderRunState::PartiallySucceeded),
        "failed" | "dead" => Some(ProviderRunState::Failed),
        "blocked" => Some(ProviderRunState::Blocked),
        "skipped" => Some(ProviderRunState::Skipped),
        _ => None,
    }
}

fn parse_fabro_sse_record(data: &str) -> ProviderEvent {
    let trimmed = data.trim();
    let Some(envelope) = serde_json::from_str::<FabroSseEnvelope>(trimmed).ok() else {
        return ProviderEvent {
            kind: "message".into(),
            message: (!trimmed.is_empty()).then(|| trimmed.to_string()),
            step_id: None,
            at: None,
            terminal_state: None,
            checkpoint_branch: None,
            gate: None,
        };
    };
    let properties = &envelope.properties;
    let message = properties
        .get("message")
        .and_then(serde_json::Value::as_str)
        .or_else(|| properties.get("reason").and_then(serde_json::Value::as_str))
        .map(str::to_string);
    let at = envelope.ts.as_deref().and_then(|ts| {
        OffsetDateTime::parse(ts, &time::format_description::well_known::Rfc3339).ok()
    });
    let kind = envelope.event.unwrap_or_else(|| "message".into());
    let terminal_state = (kind == "run.completed")
        .then(|| fabro_sse_state(properties))
        .flatten();
    ProviderEvent {
        kind,
        message,
        step_id: envelope.node_id,
        at,
        terminal_state,
        checkpoint_branch: None,
        gate: None,
    }
}

fn parse_fabro_sse_dispatch(_event_name: Option<&str>, data: &str) -> ProviderEvent {
    parse_fabro_sse_record(data)
}

#[derive(Deserialize)]
struct PollResp {
    state: String,
    /// Engines commonly call these either `checkpoints` or
    /// `checkpoint_branches`; accepting both keeps this provisional seam
    /// tolerant without inventing a second provider API.
    #[serde(default, alias = "checkpoint_branches")]
    checkpoints: Vec<PollCheckpoint>,
    #[serde(default, alias = "approval_gates", alias = "pending_gates")]
    gates: Vec<PollGate>,
    #[serde(default)]
    conclusion: Option<PollConclusion>,
}

#[derive(Deserialize)]
struct PollGate {
    id: String,
    #[serde(default, alias = "prompt", alias = "question")]
    question: String,
    #[serde(default)]
    options: Vec<PollGateOption>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum PollGateOption {
    Label(String),
    Detail {
        label: String,
        #[serde(default)]
        input: String,
        #[serde(default)]
        approve: bool,
    },
}

impl PollGate {
    fn into_provider(self) -> Option<ProviderGate> {
        let question = self.question.trim().to_string();
        let options = self
            .options
            .into_iter()
            .map(|option| match option {
                PollGateOption::Label(label) => ProviderGateOption {
                    input: label.clone(),
                    label,
                    approve: false,
                    key: None,
                },
                PollGateOption::Detail {
                    label,
                    input,
                    approve,
                } => ProviderGateOption {
                    label,
                    input,
                    approve,
                    key: None,
                },
            })
            .filter(|option| !option.label.trim().is_empty())
            .collect::<Vec<_>>();
        (!self.id.trim().is_empty() && !question.is_empty() && !options.is_empty()).then_some(
            ProviderGate {
                id: self.id,
                question,
                options,
            },
        )
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum PollCheckpoint {
    Branch(String),
    Detail(PollCheckpointDetail),
}

#[derive(Deserialize)]
struct PollCheckpointDetail {
    #[serde(default, alias = "checkpoint_branch", alias = "checkpoint")]
    branch: Option<String>,
    #[serde(default, alias = "node_id")]
    step_id: Option<String>,
    #[serde(default)]
    ts_ms: Option<i64>,
}

impl PollCheckpoint {
    fn into_provider(self) -> Option<ProviderCheckpoint> {
        let (branch, step_id, at) = match self {
            Self::Branch(branch) => (branch, None, None),
            Self::Detail(detail) => {
                let at = detail.ts_ms.and_then(|ms| {
                    OffsetDateTime::from_unix_timestamp_nanos((ms as i128) * 1_000_000).ok()
                });
                (detail.branch?, detail.step_id, at)
            }
        };
        let branch = branch.trim().to_string();
        (!branch.is_empty()).then_some(ProviderCheckpoint {
            branch,
            step_id,
            at,
        })
    }
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

/// The **provisional** JSON envelope carried on a workflow-engine SSE `data:` line (see
/// the module doc). Every field is optional and tolerant: a record that fails to
/// parse as this shape does not panic — it degrades to a bare progress event
/// carrying the raw `data` as its message (see [`parse_sse_record`]). The field
/// names here are a reasonable guess coded to a documented-but-unverified
/// contract, exactly as increment 1 coded the REST shapes; a live smoke test may
/// move them.
#[derive(Deserialize, Default)]
struct SseEnvelope {
    /// The event kind/name, when the envelope names one (else the SSE `event:`
    /// field is used, else `"message"`).
    #[serde(default, rename = "type")]
    kind: Option<String>,
    #[serde(default)]
    message: Option<String>,
    /// The node/step the event is about. Accepts `node_id` primarily; a `step_id`
    /// alias is tolerated for either spelling the live engine settles on.
    #[serde(default, alias = "step_id")]
    node_id: Option<String>,
    /// Present on a terminal event; parsed through [`parse_state`], and only a
    /// state that is actually terminal marks the event terminal.
    #[serde(default)]
    state: Option<String>,
    /// The per-stage checkpoint branch the event reports (#284). Accepts
    /// `checkpoint_branch` primarily; a bare `checkpoint` alias is tolerated for
    /// whichever spelling the live engine settles on. Absent on the vast
    /// majority of events, which is expected — collection is non-fatal.
    #[serde(default, alias = "checkpoint")]
    checkpoint_branch: Option<String>,
    /// Approval gate payload. Aliases cover the common `gate`, `approval`,
    /// and `approval_gate` envelope names without widening the provider API.
    #[serde(default, alias = "approval", alias = "approval_gate")]
    gate: Option<SseGate>,
    /// The event time as Unix epoch milliseconds, when present.
    #[serde(default)]
    ts_ms: Option<i64>,
}

#[derive(Deserialize)]
struct SseGate {
    #[serde(alias = "gate_id")]
    id: String,
    #[serde(default, alias = "prompt", alias = "question")]
    question: String,
    #[serde(default)]
    options: Vec<SseGateOption>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum SseGateOption {
    Label(String),
    Detail {
        label: String,
        #[serde(default)]
        input: String,
        #[serde(default)]
        approve: bool,
    },
}

impl SseGate {
    fn into_provider(self) -> Option<ProviderGate> {
        PollGate {
            id: self.id,
            question: self.question,
            options: self
                .options
                .into_iter()
                .map(|option| match option {
                    SseGateOption::Label(label) => PollGateOption::Label(label),
                    SseGateOption::Detail {
                        label,
                        input,
                        approve,
                    } => PollGateOption::Detail {
                        label,
                        input,
                        approve,
                    },
                })
                .collect(),
        }
        .into_provider()
    }
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
        "blocked" => Ok(ProviderRunState::Blocked),
        other => Err(WorkflowEngineError::Config(format!(
            "unknown workflow-engine run state {other:?}"
        ))),
    }
}

/// Build a [`ProviderEvent`] from one dispatched SSE record — its `event:` name
/// (if any) and its joined `data:` payload.
///
/// The `data` is parsed as the provisional [`SseEnvelope`]; when that fails (a
/// keep-alive comment, a non-JSON line, a shape we did not anticipate) the event
/// degrades to a bare progress event whose message is the raw data, rather than
/// panicking or dropping the record — the malformed-stream contract. An
/// envelope `state` that parses to a *terminal* state marks the event terminal;
/// a non-terminal or unknown state leaves `terminal_state` `None`.
fn parse_sse_record(event_name: Option<&str>, data: &str) -> ProviderEvent {
    let trimmed = data.trim();
    let envelope: Option<SseEnvelope> = if trimmed.is_empty() {
        None
    } else {
        serde_json::from_str(trimmed).ok()
    };
    match envelope {
        Some(env) => {
            let kind = env
                .kind
                .or_else(|| event_name.map(str::to_string))
                .unwrap_or_else(|| "message".to_string());
            let terminal_state = env
                .state
                .as_deref()
                .and_then(|s| parse_state(s).ok())
                .filter(|s| s.is_terminal());
            let at = env.ts_ms.and_then(|ms| {
                OffsetDateTime::from_unix_timestamp_nanos((ms as i128) * 1_000_000).ok()
            });
            let checkpoint_branch = env
                .checkpoint_branch
                .map(|b| b.trim().to_string())
                .filter(|b| !b.is_empty());
            let gate = env.gate.and_then(SseGate::into_provider);
            ProviderEvent {
                kind,
                message: env.message,
                step_id: env.node_id,
                at,
                terminal_state,
                checkpoint_branch,
                gate,
            }
        }
        None => ProviderEvent {
            kind: event_name.unwrap_or("message").to_string(),
            message: (!trimmed.is_empty()).then(|| trimmed.to_string()),
            step_id: None,
            at: None,
            terminal_state: None,
            checkpoint_branch: None,
            gate: None,
        },
    }
}

/// Turn a stream of raw response-body chunks into a stream of
/// [`ProviderEvent`]s by parsing `text/event-stream` **manually** — no
/// SSE-client crate is pulled in (the module deliberately stays on the crate's
/// existing deps).
///
/// A canonical line-oriented SSE parser: bytes are accumulated, split into lines
/// on `\n` (a trailing `\r` stripped), and each line updates the event being
/// built (`event:` sets the name, `data:` appends a payload line, a `:` line is
/// a comment, other fields are ignored). A **blank line dispatches** the
/// accumulated event; a record with no fields dispatches nothing. When the body
/// ends, any final unterminated line and any half-built event are flushed. A
/// transport error becomes an `Err` item and ends the stream — the tracker then
/// degrades to polling.
fn sse_event_stream(
    body: Pin<Box<dyn Stream<Item = reqwest::Result<Bytes>> + Send>>,
    fabro: bool,
) -> impl Stream<Item = Result<ProviderEvent, WorkflowEngineError>> + Send {
    struct SseState {
        body: Pin<Box<dyn Stream<Item = reqwest::Result<Bytes>> + Send>>,
        /// Bytes read but not yet split into a whole line.
        line_buf: Vec<u8>,
        /// Fields of the event currently being built.
        event_name: Option<String>,
        data_lines: Vec<String>,
        have_fields: bool,
        /// Events parsed out of the last chunk but not yet yielded.
        pending: std::collections::VecDeque<ProviderEvent>,
        /// The body stream has ended; only the queue drains after this.
        done: bool,
    }

    // Consume one whole line, updating the in-progress event; on a blank line,
    // dispatch it into `pending`.
    fn feed_line(st: &mut SseState, line: &str, fabro: bool) {
        if line.is_empty() {
            if st.have_fields {
                let data = st.data_lines.join("\n");
                let ev = if fabro {
                    parse_fabro_sse_dispatch(st.event_name.as_deref(), &data)
                } else {
                    parse_sse_record(st.event_name.as_deref(), &data)
                };
                st.pending.push_back(ev);
            }
            st.event_name = None;
            st.data_lines.clear();
            st.have_fields = false;
            return;
        }
        if line.starts_with(':') {
            // An SSE comment / keep-alive — ignored.
            return;
        }
        let (field, value) = match line.split_once(':') {
            // Per the SSE spec a single leading space after the colon is stripped.
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (line, ""),
        };
        match field {
            "event" => {
                st.event_name = Some(value.to_string());
                st.have_fields = true;
            }
            "data" => {
                st.data_lines.push(value.to_string());
                st.have_fields = true;
            }
            // `id`, `retry`, and any unknown field carry nothing this seam uses.
            _ => {}
        }
    }

    // Split whole lines (terminated by `\n`) out of the buffer, feeding each.
    fn drain_lines(st: &mut SseState, fabro: bool) {
        while let Some(pos) = st.line_buf.iter().position(|&b| b == b'\n') {
            let mut line: Vec<u8> = st.line_buf.drain(..=pos).collect();
            line.pop(); // drop the '\n'
            if line.last() == Some(&b'\r') {
                line.pop(); // drop a CR from a CRLF terminator
            }
            let line = String::from_utf8_lossy(&line).into_owned();
            feed_line(st, &line, fabro);
        }
    }

    let state = SseState {
        body,
        line_buf: Vec::new(),
        event_name: None,
        data_lines: Vec::new(),
        have_fields: false,
        pending: std::collections::VecDeque::new(),
        done: false,
    };

    futures_util::stream::try_unfold(state, move |mut st| async move {
        loop {
            if let Some(ev) = st.pending.pop_front() {
                return Ok(Some((ev, st)));
            }
            if st.done {
                return Ok(None);
            }
            match st.body.next().await {
                Some(Ok(chunk)) => {
                    st.line_buf.extend_from_slice(&chunk);
                    drain_lines(&mut st, fabro);
                }
                Some(Err(e)) => return Err(classify_reqwest(&e)),
                None => {
                    // Flush a final unterminated line, then any half-built event.
                    if !st.line_buf.is_empty() {
                        let line: Vec<u8> = std::mem::take(&mut st.line_buf);
                        let line = String::from_utf8_lossy(&line).into_owned();
                        let line = line.strip_suffix('\r').unwrap_or(&line).to_string();
                        feed_line(&mut st, &line, fabro);
                    }
                    if st.have_fields {
                        let data = st.data_lines.join("\n");
                        let ev = if fabro {
                            parse_fabro_sse_dispatch(st.event_name.as_deref(), &data)
                        } else {
                            parse_sse_record(st.event_name.as_deref(), &data)
                        };
                        st.pending.push_back(ev);
                        st.have_fields = false;
                    }
                    st.done = true;
                }
            }
        }
    })
}

impl WorkflowProvider for HttpWorkflowProvider {
    async fn create_run(
        &self,
        goal: &str,
        repo_ref: Option<&str>,
    ) -> Result<ProviderRun, WorkflowEngineError> {
        if let Some(fabro) = &self.fabro {
            return self.create_fabro_run(fabro, goal).await;
        }
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
        if self.is_fabro() {
            return self.poll_fabro_run(run_id).await;
        }
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
        let checkpoints = parsed
            .checkpoints
            .into_iter()
            .filter_map(PollCheckpoint::into_provider)
            .collect();
        let gates = parsed
            .gates
            .into_iter()
            .filter_map(PollGate::into_provider)
            .collect();
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
        Ok(ProviderRunStatus {
            state,
            checkpoints,
            gates,
            conclusion,
        })
    }

    async fn stream_events(
        &self,
        run_id: &str,
    ) -> Result<ProviderEventStream, WorkflowEngineError> {
        if self.is_fabro() {
            let url = format!("{}/api/v1/runs/{run_id}/attach?since_seq=1", self.base_url);
            let resp = self
                .auth(self.client.get(&url))
                .header(reqwest::header::ACCEPT, "text/event-stream")
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
            let body: Pin<Box<dyn Stream<Item = reqwest::Result<Bytes>> + Send>> =
                Box::pin(resp.bytes_stream());
            return Ok(Box::pin(sse_event_stream(body, true)));
        }
        let url = format!("{}/api/runs/{run_id}/events", self.base_url);
        let resp = self
            .auth(self.client.get(&url))
            .header(reqwest::header::ACCEPT, "text/event-stream")
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
        // Box the body stream so it is `Unpin` (needed to `.next()` it inside the
        // parser's state) and hand it to the manual SSE parser.
        let body: Pin<Box<dyn Stream<Item = reqwest::Result<Bytes>> + Send>> =
            Box::pin(resp.bytes_stream());
        Ok(Box::pin(sse_event_stream(body, false)))
    }

    async fn answer_gate(
        &self,
        run_id: &str,
        gate_id: &str,
        option_index: usize,
    ) -> Result<(), WorkflowEngineError> {
        if self.is_fabro() {
            return self
                .answer_fabro_question(run_id, gate_id, option_index)
                .await;
        }
        let url = format!("{}/api/runs/{run_id}/gates/{gate_id}/answer", self.base_url);
        let response = self
            .auth(self.client.post(&url))
            .json(&serde_json::json!({ "option": option_index }))
            .send()
            .await
            .map_err(|e| classify_reqwest(&e))?;
        ensure_success(response).await
    }

    async fn steer(
        &self,
        run_id: &str,
        text: &str,
        interrupt: bool,
    ) -> Result<(), WorkflowEngineError> {
        if self.is_fabro() {
            let url = format!("{}/api/v1/runs/{run_id}/steer", self.base_url);
            let response = self
                .auth(self.client.post(&url))
                .json(&serde_json::json!({ "text": text, "interrupt": interrupt }))
                .send()
                .await
                .map_err(|e| classify_reqwest(&e))?;
            return ensure_success(response).await;
        }
        let url = format!("{}/api/runs/{run_id}/steer", self.base_url);
        let response = self
            .auth(self.client.post(&url))
            .json(&serde_json::json!({ "text": text, "interrupt": interrupt }))
            .send()
            .await
            .map_err(|e| classify_reqwest(&e))?;
        ensure_success(response).await
    }
}

async fn ensure_success(response: reqwest::Response) -> Result<(), WorkflowEngineError> {
    let status = response.status();
    if status.is_success() {
        return Ok(());
    }
    let body = response.text().await.unwrap_or_default();
    Err(WorkflowEngineError::Api {
        status: status.as_u16(),
        body,
    })
}

/// Map a terminal provider state and its conclusion onto Vogt's own
/// [`AgentTaskRunOutcome`] and a durable [`AgentTaskRunConclusion`] (#291).
///
/// The terminal engine states line up one-to-one with existing outcome
/// variants — `succeeded → Succeeded`, `failed → Failed`,
/// `partially_succeeded → PartiallySucceeded`, `skipped → Skipped` — so nothing
/// is invented here. `blocked` maps to Vogt's `Blocked` outcome. A non-terminal
/// `Running` state is
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
        ProviderRunState::Blocked => AgentTaskRunOutcome::Blocked,
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
        body::Body,
        extract::Path,
        http::StatusCode,
        routing::{get, post},
        Json, Router,
    };
    use serde_json::{json, Value};
    use std::net::SocketAddr;
    use std::sync::{Arc, Mutex};
    use tokio::net::TcpListener;

    /// Spin up a fake engine on `127.0.0.1:0` that answers create-run with a
    /// fixed id and poll with the caller-provided terminal body. Returns the
    /// bound address; the server runs for the test's lifetime.
    async fn fake_engine(poll_body: Value) -> SocketAddr {
        let app = Router::new()
            .route(
                "/api/runs",
                post(|Json(body): Json<Value>| async move {
                    // Echo enough to prove the request shape reached the server.
                    assert!(body.get("workflow").is_some(), "workflow in body");
                    assert!(body.get("goal").is_some(), "goal in body");
                    Json(json!({ "id": "run-123", "url": "http://engine.test/runs/run-123" }))
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

    async fn fake_engine_controls() -> SocketAddr {
        let app = Router::new()
            .route(
                "/api/runs/{id}/gates/{gate_id}/answer",
                post(|Json(body): Json<Value>| async move {
                    assert_eq!(body, json!({ "option": 1 }));
                    StatusCode::NO_CONTENT
                }),
            )
            .route(
                "/api/runs/{id}/steer",
                post(|Json(body): Json<Value>| async move {
                    assert_eq!(body, json!({ "text": "focus here", "interrupt": true }));
                    StatusCode::NO_CONTENT
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

    fn fabro_config(target: FabroTarget) -> FabroConfig {
        FabroConfig {
            workflow_version_id: Some("a".repeat(64)),
            workflow_source: None,
            workflow_path: None,
            target,
            environment_id: Some("local".into()),
            model: Some("test-model".into()),
            provider: Some("test-provider".into()),
            dry_run: false,
        }
    }

    #[tokio::test]
    async fn fabro_create_sends_intent_then_starts_run() {
        let calls = Arc::new(Mutex::new(Vec::<String>::new()));
        let create_calls = Arc::clone(&calls);
        let start_calls = Arc::clone(&calls);
        let app = Router::new()
            .route(
                "/api/v1/runs",
                post(move |Json(body): Json<Value>| {
                    let calls = Arc::clone(&create_calls);
                    async move {
                        calls.lock().unwrap().push(body.to_string());
                        Json(json!({
                            "id": "fabro-run-1",
                            "links": {"web": "http://fabro.test/runs/fabro-run-1"}
                        }))
                    }
                }),
            )
            .route(
                "/api/v1/runs/{id}/start",
                post(move |Path(_id): Path<String>, Json(body): Json<Value>| {
                    let calls = Arc::clone(&start_calls);
                    async move {
                        calls.lock().unwrap().push(body.to_string());
                        StatusCode::OK
                    }
                }),
            );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let cfg = fabro_config(FabroTarget::Git {
            repo: "acme/project".into(),
            branch: "feature/fix".into(),
            sha: Some("b".repeat(40)),
        });
        let full_cfg = WorkflowEngineConfig {
            engine_url: format!("http://{addr}"),
            workflow: "nightly".into(),
            token_file: None,
            repo_ref: None,
            fabro: Some(cfg.clone()),
        };
        let provider = HttpWorkflowProvider::from_config(&full_cfg, None);
        let created = provider
            .create_run("repair the issue", None)
            .await
            .expect("Fabro create/start succeeds");
        assert_eq!(created.run_id, "fabro-run-1");
        assert_eq!(
            created.url.as_deref(),
            Some("http://fabro.test/runs/fabro-run-1")
        );
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(
            serde_json::from_str::<Value>(&calls[0]).unwrap(),
            json!({
                "workflow_version_id": "a".repeat(64),
                "target": {
                    "kind": "git",
                    "repo": "acme/project",
                    "branch": "feature/fix",
                    "sha": "b".repeat(40)
                },
                "args": {"model": "test-model", "provider": "test-provider"},
                "goal": "repair the issue",
                "title": "nightly",
                "environment_id": "local"
            })
        );
        assert_eq!(serde_json::from_str::<Value>(&calls[1]).unwrap(), json!({}));

        // Keep this assertion here so the fixture also covers serde's optional
        // provider configuration path used by task submissions.
        assert!(serde_json::to_value(full_cfg)
            .unwrap()
            .get("fabro")
            .is_some());
    }

    #[tokio::test]
    async fn fabro_poll_maps_status_conclusion_and_questions() {
        let app = Router::new()
            .route(
                "/api/v1/runs/{id}",
                get(|Path(_id): Path<String>| async {
                    Json(json!({
                        "lifecycle": {"status": {"kind": "succeeded", "reason": "completed"}},
                        "billing": {"total_usd_micros": 1250000},
                        "diff": {"summary": {"files_changed": 4, "additions": 18, "deletions": 3}},
                        "conclusion": {"final_git_commit_sha": "c".repeat(40)}
                    }))
                }),
            )
            .route(
                "/api/v1/runs/{id}/questions",
                get(|Path(_id): Path<String>| async {
                    Json(json!({"data": [], "meta": {"total": 0}}))
                }),
            );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let cfg = WorkflowEngineConfig {
            engine_url: format!("http://{addr}"),
            workflow: "nightly".into(),
            token_file: None,
            repo_ref: None,
            fabro: Some(fabro_config(FabroTarget::None)),
        };
        let provider = HttpWorkflowProvider::from_config(&cfg, None);
        let status = provider
            .poll("fabro-run-1")
            .await
            .expect("Fabro poll succeeds");
        assert_eq!(status.state, ProviderRunState::Succeeded);
        assert!(status.gates.is_empty());
        let conclusion = status.conclusion.expect("terminal conclusion");
        assert_eq!(conclusion.final_sha, Some("c".repeat(40)));
        assert_eq!(conclusion.cost_usd_micros, Some(1_250_000));
        assert_eq!(
            conclusion.diff,
            Some(DiffStat {
                files: 4,
                insertions: 18,
                deletions: 3,
            })
        );
        assert_eq!(conclusion.summary.as_deref(), Some("Fabro run completed"));
    }

    #[tokio::test]
    async fn fabro_poll_retains_a_terminal_conclusion_when_optional_fields_are_absent() {
        let app = Router::new()
            .route(
                "/api/v1/runs/{id}",
                get(|Path(_id): Path<String>| async {
                    Json(json!({
                        "lifecycle": {"status": {"kind": "succeeded", "reason": "completed"}}
                    }))
                }),
            )
            .route(
                "/api/v1/runs/{id}/questions",
                get(|Path(_id): Path<String>| async {
                    Json(json!({"data": [], "meta": {"total": 0}}))
                }),
            );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let cfg = WorkflowEngineConfig {
            engine_url: format!("http://{addr}"),
            workflow: "nightly".into(),
            token_file: None,
            repo_ref: None,
            fabro: Some(fabro_config(FabroTarget::None)),
        };
        let provider = HttpWorkflowProvider::from_config(&cfg, None);
        let status = provider
            .poll("fabro-run-1")
            .await
            .expect("Fabro poll succeeds");
        assert_eq!(status.state, ProviderRunState::Succeeded);
        assert_eq!(
            status.conclusion.and_then(|value| value.summary),
            Some("Fabro run completed".into())
        );
    }

    #[tokio::test]
    async fn fabro_poll_enriches_conclusion_from_state_projection() {
        let app = Router::new()
            .route(
                "/api/v1/runs/{id}",
                get(|Path(_id): Path<String>| async {
                    Json(json!({
                        "lifecycle": {"status": {"kind": "succeeded", "reason": "completed"}}
                    }))
                }),
            )
            .route(
                "/api/v1/runs/{id}/questions",
                get(|Path(_id): Path<String>| async {
                    Json(json!({"data": [], "meta": {"total": 0}}))
                }),
            )
            .route(
                "/api/v1/runs/{id}/state",
                get(|Path(_id): Path<String>| async {
                    Json(json!({
                        "conclusion": {
                            "final_git_commit_sha": "d".repeat(40),
                            "billing": {"total_usd_micros": 750000},
                            "diff": {"summary": {"files_changed": 2, "additions": 7, "deletions": 1}}
                        }
                    }))
                }),
            );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let cfg = WorkflowEngineConfig {
            engine_url: format!("http://{addr}"),
            workflow: "nightly".into(),
            token_file: None,
            repo_ref: None,
            fabro: Some(fabro_config(FabroTarget::None)),
        };
        let provider = HttpWorkflowProvider::from_config(&cfg, None);
        let conclusion = provider
            .poll("fabro-run-1")
            .await
            .expect("Fabro poll succeeds")
            .conclusion
            .expect("state projection enriches conclusion");
        assert_eq!(conclusion.final_sha, Some("d".repeat(40)));
        assert_eq!(conclusion.cost_usd_micros, Some(750_000));
        assert_eq!(
            conclusion.diff,
            Some(DiffStat {
                files: 2,
                insertions: 7,
                deletions: 1,
            })
        );
    }

    #[tokio::test]
    async fn fabro_poll_collects_timeline_entries_on_the_run_branch() {
        let app = Router::new()
            .route(
                "/api/v1/runs/{id}",
                get(|Path(_id): Path<String>| async {
                    Json(json!({
                        "lifecycle": {"status": {"kind": "running"}}
                    }))
                }),
            )
            .route(
                "/api/v1/runs/{id}/state",
                get(|Path(_id): Path<String>| async {
                    Json(json!({
                        "start": {"run_branch": "fabro/run-1"}
                    }))
                }),
            )
            .route(
                "/api/v1/runs/{id}/timeline",
                get(|Path(_id): Path<String>| async {
                    Json(json!([
                        {"ordinal": 1, "node_name": "start", "visit": 1, "checkpoint_seq": 14},
                        {"ordinal": 2, "node_name": "implement", "visit": 1, "checkpoint_seq": 18}
                    ]))
                }),
            )
            .route(
                "/api/v1/runs/{id}/questions",
                get(|Path(_id): Path<String>| async {
                    Json(json!({"data": [], "meta": {"total": 0}}))
                }),
            );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let cfg = WorkflowEngineConfig {
            engine_url: format!("http://{addr}"),
            workflow: "nightly".into(),
            token_file: None,
            repo_ref: None,
            fabro: Some(fabro_config(FabroTarget::Git {
                repo: "acme/project".into(),
                branch: "main".into(),
                sha: None,
            })),
        };
        let provider = HttpWorkflowProvider::from_config(&cfg, None);
        let status = provider
            .poll("fabro-run-1")
            .await
            .expect("Fabro poll succeeds");
        assert_eq!(status.state, ProviderRunState::Running);
        assert_eq!(status.checkpoints.len(), 2);
        assert_eq!(status.checkpoints[0].branch, "fabro/run-1");
        assert_eq!(status.checkpoints[0].step_id.as_deref(), Some("start@1"));
        assert_eq!(
            status.checkpoints[1].step_id.as_deref(),
            Some("implement@1")
        );
    }

    #[tokio::test]
    async fn fabro_stream_parses_events_and_terminal_state() {
        let body = concat!(
            "event: run.started\n",
            "data: {\"event\":\"run.started\",\"ts\":\"2026-08-26T12:00:00Z\",\"properties\":{\"message\":\"started\"}}\n\n",
            "data: {\"event\":\"run.completed\",\"node_id\":\"finish\",\"properties\":{\"status\":\"succeeded\",\"reason\":\"completed\"}}\n\n"
        );
        let app = Router::new().route(
            "/api/v1/runs/{id}/attach",
            get(move |Path(_id): Path<String>| async move {
                let chunks = body
                    .as_bytes()
                    .chunks(13)
                    .map(|chunk| Ok::<_, std::io::Error>(bytes::Bytes::copy_from_slice(chunk)));
                axum::response::Response::builder()
                    .header("content-type", "text/event-stream")
                    .body(Body::from_stream(futures_util::stream::iter(chunks)))
                    .unwrap()
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let cfg = WorkflowEngineConfig {
            engine_url: format!("http://{addr}"),
            workflow: "nightly".into(),
            token_file: None,
            repo_ref: None,
            fabro: Some(fabro_config(FabroTarget::None)),
        };
        let provider = HttpWorkflowProvider::from_config(&cfg, None);
        let events = collect_events(provider.stream_events("fabro-run-1").await.unwrap()).await;
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].kind, "run.started");
        assert_eq!(events[0].message.as_deref(), Some("started"));
        assert_eq!(
            events[0].at,
            Some(
                OffsetDateTime::parse(
                    "2026-08-26T12:00:00Z",
                    &time::format_description::well_known::Rfc3339,
                )
                .unwrap()
            )
        );
        assert_eq!(events[1].kind, "run.completed");
        assert_eq!(events[1].step_id.as_deref(), Some("finish"));
        assert_eq!(events[1].terminal_state, Some(ProviderRunState::Succeeded));
    }

    #[tokio::test]
    async fn fabro_answers_questions_by_provider_option_key() {
        let answer = Arc::new(Mutex::new(None::<Value>));
        let answer_capture = Arc::clone(&answer);
        let app = Router::new()
            .route(
                "/api/v1/runs/{id}/questions",
                get(|Path(_id): Path<String>| async {
                    Json(json!({
                        "data": [{
                            "id": "q-1",
                            "text": "Continue?",
                            "question_type": "multiple_choice",
                            "options": [
                                {"key": "accept", "label": "Continue"},
                                {"key": "reject", "label": "Stop"}
                            ],
                            "allow_freeform": false
                        }]
                    }))
                }),
            )
            .route(
                "/api/v1/runs/{id}/questions/{qid}/answer",
                post(move |Json(body): Json<Value>| {
                    let answer = Arc::clone(&answer_capture);
                    async move {
                        *answer.lock().unwrap() = Some(body);
                        StatusCode::NO_CONTENT
                    }
                }),
            );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let cfg = WorkflowEngineConfig {
            engine_url: format!("http://{addr}"),
            workflow: "nightly".into(),
            token_file: None,
            repo_ref: None,
            fabro: Some(fabro_config(FabroTarget::None)),
        };
        let provider = HttpWorkflowProvider::from_config(&cfg, None);
        provider
            .answer_gate("fabro-run-1", "q-1", 0)
            .await
            .expect("answer accepted");
        let expected = json!({
            "kind": "selected",
            "option_key": "accept"
        });
        assert_eq!(answer.lock().unwrap().clone(), Some(expected));
    }

    #[test]
    fn fabro_config_rejects_invalid_version_id() {
        let cfg = FabroConfig {
            workflow_version_id: Some("not-a-sha".into()),
            workflow_source: None,
            workflow_path: None,
            target: FabroTarget::None,
            environment_id: None,
            model: None,
            provider: None,
            dry_run: false,
        };
        let err = validate_fabro_config(&cfg).expect_err("invalid id must fail closed");
        assert!(
            matches!(err, WorkflowEngineError::Config(message) if message.contains("workflow_version_id"))
        );
    }

    #[test]
    fn fabro_manifest_mode_emits_the_0254_compatible_shape() {
        let cfg = FabroConfig {
            workflow_version_id: None,
            workflow_source: Some("digraph Smoke { start -> exit }".into()),
            workflow_path: Some(".fabro/workflows/smoke/workflow.fabro".into()),
            target: FabroTarget::Folder {
                path: "/srv/workspaces/project".into(),
            },
            environment_id: Some("local".into()),
            model: Some("gemini-test".into()),
            provider: Some("gemini".into()),
            dry_run: true,
        };
        validate_fabro_config(&cfg).expect("manifest configuration is valid");
        let manifest = fabro_manifest(&cfg, "smoke", "run a smoke").unwrap();
        assert_eq!(manifest["version"], 1);
        assert_eq!(manifest["cwd"], "/srv/workspaces/project");
        assert_eq!(manifest["target"]["identifier"], "smoke");
        assert_eq!(
            manifest["target"]["path"],
            ".fabro/workflows/smoke/workflow.fabro"
        );
        assert_eq!(
            manifest["workflows"][".fabro/workflows/smoke/workflow.fabro"]["source"],
            "digraph Smoke { start -> exit }"
        );
        assert_eq!(manifest["args"]["environment"], "local");
        assert_eq!(manifest["args"]["model"], "gemini-test");
        assert_eq!(manifest["args"]["provider"], "gemini");
        assert_eq!(manifest["args"]["dry_run"], true);
    }

    #[tokio::test]
    #[ignore = "requires FABRO_LIVE_URL, FABRO_LIVE_TOKEN, FABRO_LIVE_FOLDER, and FABRO_LIVE_WORKFLOW_SOURCE"]
    async fn fabro_live_smoke_create_poll_and_sse() {
        let url = std::env::var("FABRO_LIVE_URL").expect("FABRO_LIVE_URL");
        let token = std::env::var("FABRO_LIVE_TOKEN").expect("FABRO_LIVE_TOKEN");
        let folder = std::env::var("FABRO_LIVE_FOLDER").expect("FABRO_LIVE_FOLDER");
        let source =
            std::env::var("FABRO_LIVE_WORKFLOW_SOURCE").expect("FABRO_LIVE_WORKFLOW_SOURCE");
        let cfg = WorkflowEngineConfig {
            engine_url: url,
            workflow: "vogt-live-smoke".into(),
            token_file: None,
            repo_ref: None,
            fabro: Some(FabroConfig {
                workflow_version_id: None,
                workflow_source: Some(source),
                workflow_path: Some(".fabro/workflows/smoke/workflow.fabro".into()),
                target: FabroTarget::Folder { path: folder },
                environment_id: Some("local".into()),
                model: None,
                provider: None,
                dry_run: true,
            }),
        };
        let provider = HttpWorkflowProvider::from_config(&cfg, Some(token));
        let run = provider
            .create_run("Vogt live Fabro adapter smoke", None)
            .await
            .expect("Fabro create/start succeeds");
        let mut status = provider
            .poll(&run.run_id)
            .await
            .expect("Fabro poll succeeds");
        for _ in 0..30 {
            if status.state.is_terminal() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            status = provider
                .poll(&run.run_id)
                .await
                .expect("Fabro poll succeeds");
        }
        assert!(status.state.is_terminal());
        assert!(status.conclusion.is_some());
        let events = collect_events(
            provider
                .stream_events(&run.run_id)
                .await
                .expect("Fabro attach succeeds"),
        )
        .await;
        assert!(events.iter().any(|event| event.kind == "run.completed"));
        assert!(events.iter().any(|event| event.terminal_state.is_some()));
    }

    #[tokio::test]
    async fn create_then_poll_succeeded_maps_to_a_succeeded_conclusion() {
        let addr = fake_engine(json!({
            "state": "succeeded",
            "conclusion": {
                "final_sha": "abc1234",
                "diff": { "files": 3, "insertions": 40, "deletions": 5 },
                "cost_usd_micros": 250_000,
                "summary": "did the thing"
            },
            "checkpoints": [
                "workflow/run-123/stage-1",
                { "checkpoint_branch": "workflow/run-123/stage-2", "node_id": "n2", "ts_ms": 2000 }
            ],
            "gates": [{
                "id": "approval-1",
                "question": "Deploy?",
                "options": [{"label": "Approve", "input": "yes", "approve": true}, "Hold"]
            }]
        }))
        .await;
        let base = format!("http://{addr}");
        let provider = HttpWorkflowProvider::with_client(&base, "nightly", None, short_client());

        let created = provider
            .create_run("fix the bug", Some("main"))
            .await
            .expect("create ok");
        assert_eq!(created.run_id, "run-123");
        assert_eq!(
            created.url.as_deref(),
            Some("http://engine.test/runs/run-123")
        );

        let status = provider.poll(&created.run_id).await.expect("poll ok");
        assert_eq!(status.state, ProviderRunState::Succeeded);
        assert!(status.state.is_terminal());
        assert_eq!(
            status.checkpoints,
            vec![
                ProviderCheckpoint {
                    branch: "workflow/run-123/stage-1".into(),
                    step_id: None,
                    at: None,
                },
                ProviderCheckpoint {
                    branch: "workflow/run-123/stage-2".into(),
                    step_id: Some("n2".into()),
                    at: Some(OffsetDateTime::from_unix_timestamp(2).unwrap()),
                },
            ]
        );
        assert_eq!(status.gates.len(), 1);
        assert_eq!(status.gates[0].id, "approval-1");
        assert_eq!(status.gates[0].options[0].input, "yes");
        assert_eq!(status.gates[0].options[1].label, "Hold");

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
        let addr = fake_engine(json!({ "state": "failed" })).await;
        let base = format!("http://{addr}");
        let provider = HttpWorkflowProvider::with_client(&base, "nightly", None, short_client());

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
        let provider = HttpWorkflowProvider::with_client(
            "http://127.0.0.1:1",
            "nightly",
            None,
            short_client(),
        );
        let err = provider
            .create_run("goal", None)
            .await
            .expect_err("dead port must error");
        assert!(
            matches!(err, WorkflowEngineError::Unreachable(_)),
            "expected Unreachable, got {err:?}"
        );

        let err = provider
            .poll("run-123")
            .await
            .expect_err("dead port must error");
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
        let (blocked, _) = map_conclusion(ProviderRunState::Blocked, None, started, started);
        assert_eq!(blocked, AgentTaskRunOutcome::Blocked);
    }

    #[tokio::test]
    async fn provider_controls_forward_gate_answers_and_steers() {
        let addr = fake_engine_controls().await;
        let provider = HttpWorkflowProvider::with_client(
            &format!("http://{addr}"),
            "nightly",
            None,
            short_client(),
        );
        provider
            .answer_gate("run-1", "approval-1", 1)
            .await
            .expect("gate answer accepted");
        provider
            .steer("run-1", "focus here", true)
            .await
            .expect("steer accepted");
    }

    #[test]
    fn config_round_trips_through_serde() {
        let cfg = WorkflowEngineConfig {
            engine_url: "https://engine.internal".into(),
            workflow: "nightly-audit".into(),
            token_file: Some("/run/secrets/workflow-engine".into()),
            repo_ref: Some("main".into()),
            fabro: None,
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let back: WorkflowEngineConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.engine_url, cfg.engine_url);
        assert_eq!(back.workflow, cfg.workflow);
        assert_eq!(back.token_file, cfg.token_file);
        assert_eq!(back.repo_ref, cfg.repo_ref);
        assert_eq!(back.fabro, cfg.fabro);

        // The optional fields drop out of the wire form when unset.
        let minimal: WorkflowEngineConfig =
            serde_json::from_str(r#"{"engine_url":"http://e","workflow":"w"}"#).unwrap();
        assert!(minimal.token_file.is_none());
        assert!(minimal.repo_ref.is_none());
        assert!(minimal.fabro.is_none());
        let wire = serde_json::to_value(&minimal).unwrap();
        assert!(wire.get("token_file").is_none());
        assert!(wire.get("repo_ref").is_none());
    }

    /// Spin up a fake engine whose `/api/runs/{id}/events` streams `sse_body`
    /// back as `text/event-stream`, in small chunks so the parser is exercised
    /// across record and line boundaries. Returns the bound address.
    async fn fake_engine_events(sse_body: &'static str) -> SocketAddr {
        let app = Router::new().route(
            "/api/runs/{id}/events",
            get(move |Path(_id): Path<String>| async move {
                let chunks: Vec<std::result::Result<bytes::Bytes, std::io::Error>> = sse_body
                    .as_bytes()
                    .chunks(16)
                    .map(|c| Ok(bytes::Bytes::copy_from_slice(c)))
                    .collect();
                let stream = futures_util::stream::iter(chunks);
                axum::response::Response::builder()
                    .header("content-type", "text/event-stream")
                    .body(Body::from_stream(stream))
                    .unwrap()
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        addr
    }

    async fn collect_events(mut stream: ProviderEventStream) -> Vec<ProviderEvent> {
        let mut out = Vec::new();
        while let Some(item) = stream.next().await {
            out.push(item.expect("event item ok"));
        }
        out
    }

    #[tokio::test]
    async fn sse_stream_yields_progress_then_a_terminal_event() {
        // Two progress events, then a terminal `run.completed` carrying a
        // `succeeded` state. Blank lines delimit the records; the body is fed in
        // 16-byte chunks so records span chunk boundaries.
        let body = "event: node.started\n\
data: {\"type\":\"node.started\",\"message\":\"cloning repo\",\"node_id\":\"n1\",\"ts_ms\":1000}\n\
\n\
event: node.completed\n\
data: {\"message\":\"tests passed\",\"node_id\":\"n2\"}\n\
\n\
event: run.completed\n\
data: {\"type\":\"run.completed\",\"state\":\"succeeded\",\"message\":\"all done\",\"ts_ms\":2000}\n\
\n";
        let addr = fake_engine_events(body).await;
        let base = format!("http://{addr}");
        let provider = HttpWorkflowProvider::with_client(&base, "nightly", None, short_client());

        let stream = provider.stream_events("run-1").await.expect("subscribe ok");
        let events = collect_events(stream).await;

        assert_eq!(events.len(), 3, "one event per SSE record");

        assert_eq!(events[0].kind, "node.started");
        assert_eq!(events[0].message.as_deref(), Some("cloning repo"));
        assert_eq!(events[0].step_id.as_deref(), Some("n1"));
        assert_eq!(
            events[0].at,
            Some(OffsetDateTime::from_unix_timestamp(1).unwrap())
        );
        assert!(events[0].terminal_state.is_none());

        // No envelope `type`: the SSE `event:` name is used as the kind.
        assert_eq!(events[1].kind, "node.completed");
        assert_eq!(events[1].step_id.as_deref(), Some("n2"));
        assert!(events[1].terminal_state.is_none());

        // The terminal event carries the mapped state.
        let terminal = &events[2];
        assert_eq!(terminal.kind, "run.completed");
        assert_eq!(terminal.terminal_state, Some(ProviderRunState::Succeeded));
        assert!(terminal.terminal_state.unwrap().is_terminal());

        // …and that terminal state maps through the existing conclusion path.
        let started = OffsetDateTime::UNIX_EPOCH;
        let (outcome, _) = map_conclusion(
            terminal.terminal_state.unwrap(),
            None,
            started,
            started + time::Duration::seconds(1),
        );
        assert_eq!(outcome, AgentTaskRunOutcome::Succeeded);
    }

    #[tokio::test]
    async fn sse_events_surface_per_stage_checkpoint_branches() {
        // #284: a stage event names the git checkpoint branch the engine wrote
        // for that stage; a plain progress event names none. The parser lifts
        // the branch onto the event, and the `checkpoint` alias is accepted.
        let body = "event: node.completed\n\
data: {\"message\":\"stage 1 done\",\"node_id\":\"n1\",\"checkpoint_branch\":\"wf/run-1/stage-1\",\"ts_ms\":1000}\n\
\n\
event: node.completed\n\
data: {\"message\":\"stage 2 done\",\"node_id\":\"n2\",\"checkpoint\":\"  wf/run-1/stage-2  \"}\n\
\n\
event: node.started\n\
data: {\"message\":\"stage 3 running\",\"node_id\":\"n3\"}\n\
\n";
        let addr = fake_engine_events(body).await;
        let base = format!("http://{addr}");
        let provider = HttpWorkflowProvider::with_client(&base, "nightly", None, short_client());

        let events = collect_events(provider.stream_events("run-1").await.unwrap()).await;
        assert_eq!(events.len(), 3);

        // The primary spelling, with a step id and a timestamp for its age.
        assert_eq!(
            events[0].checkpoint_branch.as_deref(),
            Some("wf/run-1/stage-1")
        );
        assert_eq!(events[0].step_id.as_deref(), Some("n1"));
        assert_eq!(
            events[0].at,
            Some(OffsetDateTime::from_unix_timestamp(1).unwrap())
        );

        // The `checkpoint` alias is accepted and trimmed.
        assert_eq!(
            events[1].checkpoint_branch.as_deref(),
            Some("wf/run-1/stage-2")
        );

        // An event that names no checkpoint carries none — the ordinary case.
        assert!(events[2].checkpoint_branch.is_none());
    }

    #[tokio::test]
    async fn sse_events_surface_provider_gates() {
        let body = "event: gate.opened\n\\
data: {\"type\":\"gate.opened\",\"gate\":{\"gate_id\":\"approval-2\",\"question\":\"Deploy?\",\"options\":[{\"label\":\"Approve\",\"input\":\"yes\",\"approve\":true},\"Hold\"]}}\n\\
\n\\
";
        let addr = fake_engine_events(body).await;
        let provider = HttpWorkflowProvider::with_client(
            &format!("http://{addr}"),
            "nightly",
            None,
            short_client(),
        );
        let events = collect_events(provider.stream_events("run-1").await.unwrap()).await;
        let gate = events[0].gate.as_ref().expect("provider gate");
        assert_eq!(gate.id, "approval-2");
        assert_eq!(gate.question, "Deploy?");
        assert_eq!(gate.options[0].input, "yes");
        assert!(gate.options[0].approve);
        assert_eq!(gate.options[1].label, "Hold");
    }

    #[tokio::test]
    async fn a_terminal_failed_event_maps_to_failed() {
        let body = "event: run.completed\n\
data: {\"state\":\"failed\",\"message\":\"boom\"}\n\
\n";
        let addr = fake_engine_events(body).await;
        let base = format!("http://{addr}");
        let provider = HttpWorkflowProvider::with_client(&base, "nightly", None, short_client());

        let events = collect_events(provider.stream_events("run-1").await.unwrap()).await;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].terminal_state, Some(ProviderRunState::Failed));
    }

    #[tokio::test]
    async fn a_malformed_or_aborted_stream_degrades_without_panicking() {
        // A keep-alive comment (ignored), a non-JSON data line (degrades to a
        // bare message), and a final record that is truncated mid-JSON with no
        // trailing blank line (flushed on stream end, degrades). None of this
        // may panic; the parser yields what it can.
        let body = ": keep-alive\n\
\n\
data: not json at all\n\
\n\
event: weird\n\
data: {\"broken\": ";
        let addr = fake_engine_events(body).await;
        let base = format!("http://{addr}");
        let provider = HttpWorkflowProvider::with_client(&base, "nightly", None, short_client());

        let events = collect_events(provider.stream_events("run-1").await.unwrap()).await;
        // The comment dispatches nothing; the two data records both survive.
        assert_eq!(events.len(), 2);

        assert_eq!(events[0].kind, "message");
        assert_eq!(events[0].message.as_deref(), Some("not json at all"));
        assert!(events[0].terminal_state.is_none());

        // The truncated JSON falls back to the SSE event name and raw payload —
        // no terminal state is inferred from an unparseable record.
        assert_eq!(events[1].kind, "weird");
        assert!(events[1].message.is_some());
        assert!(events[1].terminal_state.is_none());
    }

    #[tokio::test]
    async fn stream_events_on_a_dead_port_is_unreachable() {
        let provider = HttpWorkflowProvider::with_client(
            "http://127.0.0.1:1",
            "nightly",
            None,
            short_client(),
        );
        // The Ok variant is a boxed stream (not `Debug`), so match rather than
        // `expect_err`.
        match provider.stream_events("run-1").await {
            Ok(_) => panic!("dead port must not yield a stream"),
            Err(err) => assert!(
                matches!(err, WorkflowEngineError::Unreachable(_)),
                "expected Unreachable, got {err:?}"
            ),
        }
    }
}
