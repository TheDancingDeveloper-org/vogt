//! Value types the observed-store interface speaks in.
//!
//! Ports `src/vogt/storage/observed_types.py`. Separate from both the interface
//! and the SQLite backend so that neither has to import the other: collectors
//! build `PendingObservation`s, the application builds `DepRefRow`s, and any
//! backend consumes them.

use std::collections::BTreeMap;

use crate::core::Moment;

/// A finding on its way into the store, before it has an id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingObservation {
    pub kind: String,
    pub subject_key: String,
    pub payload: serde_json::Value,
    pub content_digest: String,
    pub project_id: Option<String>,
    pub source_url: Option<String>,
    pub promoted: bool,
}

/// What appending a collector's findings actually changed.
///
/// `unchanged` is the interesting number: it is the evidence that digest dedup
/// is working, and a sweep that reports thousands of new rows for an unchanged
/// repository is a bug in a collector's subject keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AppendStats {
    pub new: i64,
    pub unchanged: i64,
}

impl AppendStats {
    pub fn total(self) -> i64 {
        self.new + self.unchanged
    }
}

/// A resolved dependency reference, ready to replace the projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DepRefRow {
    pub subject_key: String,
    pub from_project_id: String,
    pub ref_kind: String,
    pub raw_target: String,
    pub manifest: Option<String>,
    pub to_project_id: Option<String>,
    pub observed_at: Moment,
}

/// What retention removed, and what protected the rest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PruneReport {
    pub removed: i64,
    pub kept_latest: i64,
    pub kept_referenced: i64,
}

/// The outcome of running one collector over one scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SweepReport {
    pub collector: String,
    pub sweep_id: String,
    pub outcome: String,
    pub projects: i64,
    pub new: i64,
    pub unchanged: i64,
    pub failures: BTreeMap<String, String>,
    pub detail: Option<String>,
}

/// How far one transcript file has been indexed.
///
/// `offset` is a byte position at a line boundary: everything before it has
/// been read and its calls stored in the same transaction that moved it, so a
/// crash re-reads at most the batch that did not commit. `agent_session_id`
/// and `cwd` are carried because Codex states them once, at the top of the
/// file, and a later batch starting mid-file still needs them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptCursor {
    pub path: String,
    pub agent: String,
    pub offset: i64,
    pub size: i64,
    pub agent_session_id: Option<String>,
    pub cwd: Option<String>,
}

/// One tool call, already redacted, on its way into the index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivityCall {
    pub source_path: String,
    pub call_id: String,
    pub agent: String,
    pub agent_session_id: String,
    pub cwd: Option<String>,
    pub tool: String,
    pub summary: String,
    pub services: Vec<String>,
    /// The call dumps configuration or environment, so whatever its result
    /// says is never excerpted — even when the result lands in a later batch.
    pub withheld: bool,
    pub at: Moment,
}

/// The outcome of a call, matched to it by (file, call id).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivityResult {
    pub source_path: String,
    pub call_id: String,
    pub error: bool,
    pub excerpt: Option<String>,
    pub at: Option<Moment>,
}

/// What one bounded read of the transcript roots produced.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ActivityBatch {
    pub calls: Vec<ActivityCall>,
    pub results: Vec<ActivityResult>,
    pub cursors: Vec<TranscriptCursor>,
    pub files: i64,
    pub bytes_read: i64,
    /// Bytes known to be waiting past this batch's budget.
    pub backlog_bytes: i64,
    pub skipped: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ActivityIndexStats {
    pub calls: i64,
    pub results: i64,
}

/// A filter over the index. Every field narrows; none widens.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ActivityQuery {
    pub q: Option<String>,
    pub service: Option<String>,
    pub tool: Option<String>,
    pub errors_only: bool,
    pub since: Option<Moment>,
    pub until: Option<Moment>,
    pub agent_session_ids: Option<Vec<String>>,
    /// Working directories a call must be in or under, e.g. a project root.
    pub cwd_roots: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivityEventRow {
    pub id: String,
    pub agent: String,
    pub agent_session_id: String,
    pub cwd: Option<String>,
    pub tool: String,
    pub summary: String,
    pub services: Vec<String>,
    pub error: bool,
    pub excerpt: Option<String>,
    pub at: Moment,
    pub finished_at: Option<Moment>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivitySessionRow {
    pub agent: String,
    pub agent_session_id: String,
    pub cwd: Option<String>,
    pub first_at: Moment,
    pub last_at: Moment,
    pub calls: i64,
    pub errors: i64,
    /// Calls whose result has been seen.
    pub finished: i64,
    /// Sum of call→result time over finished calls, in milliseconds.
    pub wait_ms: i64,
    pub tools: BTreeMap<String, i64>,
    pub services: BTreeMap<String, i64>,
}
