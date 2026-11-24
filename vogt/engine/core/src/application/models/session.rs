//! The session operations' parameter and result models that are not chat
//! or grant models: the agent token, the output log and the commit.
//!
//! Ports `EngineSessionToken*`, `LogTail*` and `CommitForSession*` in
//! `src/vogt/application/models.py`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::integer;

fn log_tail_bytes_default() -> i64 {
    64 * 1024
}

fn empty_list() -> Vec<String> {
    Vec::new()
}

fn strip_ansi_default() -> bool {
    true
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EngineSessionTokenParams {
    /// The engine's id (UUID) for the session the token is for.
    #[schemars(pattern(r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$"))]
    pub engine_session_id: String,
    /// Revoke the session's tokens instead of minting one.
    #[serde(default)]
    pub revoke: bool,
    /// Why this write is being made (audited).
    #[schemars(length(min = 1))]
    pub reason: String,
}

/// A session's own agent credential (WI-926).
//
// Python's class is a plain `BaseModel`, not a `Result`, so its schema does
// not forbid extra keys.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct EngineSessionTokenResult {
    pub engine_session_id: String,
    // The agent actor the token is bound to (`agent:engine:<uuid>`).
    pub actor: String,
    // The secret, shown once; `None` when revoking.
    #[serde(default)]
    pub token: Option<String>,
    // How many earlier tokens of the actor were revoked.
    #[serde(default)]
    #[schemars(schema_with = "integer")]
    pub revoked: i64,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LogTailParams {
    /// Session id, in either form: Vogt's `ses_…` id or the engine's session UUID (`engine_session_id` in session.list; the only id an unlinked session has).
    pub id: String,
    /// Trailing bytes of the log to read; capped at 256 KiB.
    #[serde(default = "log_tail_bytes_default")]
    #[schemars(schema_with = "integer", range(min = 1, max = 262144), extend("default" = 65536))]
    pub tail_bytes: i64,
    /// Remove terminal escape codes so the text is readable. Pass false for the raw escape stream.
    #[serde(default = "strip_ansi_default")]
    #[schemars(extend("default" = true))]
    pub strip_ansi: bool,
}

/// The tail of one session's output log.
///
/// Works for a live session too — the engine reads the on-disk log by id, no
/// archive row required. `session_id` is null when the engine has no log for
/// that id (or could not be asked, with `engine` then set).
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LogTailResult {
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    #[schemars(schema_with = "integer")]
    pub bytes: i64,
    #[serde(default)]
    #[schemars(schema_with = "integer")]
    pub total_bytes: i64,
    #[serde(default)]
    pub truncated: bool,
    /// What the engine said, when it could not be asked. Empty history is returned rather than an error, so an outage never reads as 'no history' .
    #[serde(default)]
    pub engine: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CommitForSessionParams {
    /// Session id, in either form: Vogt's `ses_…` id or the engine's session UUID (`engine_session_id` in session.list; the only id an unlinked session has).
    pub id: String,
    /// The commit message. The repo's own git identity authors it.
    #[schemars(length(min = 1))]
    pub message: String,
    /// Paths to stage before committing, relative to the repo. Empty commits whatever the worker already staged.
    #[serde(default = "empty_list")]
    pub paths: Vec<String>,
    /// Return the full staged diff. Off by default, so the diff never enters a caller's context unasked; the digest and stat always do.
    #[serde(default)]
    pub full_diff: bool,
    /// Commit in this linked worktree instead of the session's own directory. It must be a worktree of the repo that directory belongs to; a stranger repo is refused. A worker's commit never lands in the repo's main checkout, where pool workers are launched: name the worker's own worktree (absolute or workspace-relative).
    #[serde(default)]
    pub worktree: Option<String>,
    /// Why this write is being made (audited).
    #[schemars(length(min = 1))]
    pub reason: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CommitForSessionResult {
    /// The new commit's full SHA.
    pub commit: String,
    /// One line naming what was committed, e.g. the diff stat summary.
    pub digest: String,
    /// `git diff --stat` of what was committed.
    pub diff_stat: String,
    /// The full staged diff, only when full_diff was set.
    #[serde(default)]
    pub diff: Option<String>,
    /// Hunks to read before trusting the commit: counts of dependency pins, `unsafe`, and foreign-function bindings.
    #[serde(default = "empty_list")]
    pub review: Vec<String>,
}
