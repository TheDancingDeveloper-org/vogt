//! The error taxonomy. Ports `src/vogt/errors.py`.
//!
//! Adapters translate these into exit codes, HTTP statuses and MCP payloads.
//! The HTTP body is `{"error": {"code", "message"}}` (`adapters/http/app.py`);
//! `code` and `http_status` are what that handler reads. A subclass keeps its
//! parent's status unless it sets its own, which is how `InvalidParams` is 422
//! while every other `InvalidRequest` is 400.

#![allow(dead_code)]

use std::fmt;

/// One deliberate failure. The string is the message, `str(exc)` in Python.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VogtError {
    NotInitialized(String),
    AlreadyInitialized(String),
    NotFound(String),
    Conflict(String),
    BypassRefused(String),
    RoleRefused(String),
    GrantRefused(String),
    EngineOnly(String),
    InvalidRequest(String),
    InstallClosed(String),
    LoginThrottled(String),
    InvalidCursor(String),
    ForgeAccountsNotConfigured(String),
    ImportParityRefused(String),
    ImportWorkingTreeDirty(String),
    ImportBranchDiverged(String),
    PublishRefused(String),
    PublishSourceInvalid(String),
    PublishWorkingTreeDirty(String),
    PublishNonFastForward(String),
    RemoteRepoExists(String),
    NotLinked(String),
    LinkRefused(String),
    UpstreamWriteRefused(String),
    UpstreamWriteFailed(String),
    InboxEntryNotFound(String),
    InvalidTriageState(String),
    PreferenceVersionConflict(String),
    InvalidPreference(String),
    InvalidSnooze(String),
    MissingReason(String),
    InvalidParams(String),
    MigrationError(String),
    MigrationLocked(String),
    /// The session engine did not answer or refused the credential
    /// (`adapters/engine/client.py`). Optional by design: callers degrade, they
    /// do not crash. 502, the same status as every other upstream adapter.
    EngineUnavailable(String),
    /// `git` is missing, failed, or could not reach the remote
    /// (`adapters/git/clone.py`).
    GitUnavailable(String),
    /// git ran and exited non-zero. A subclass of `GitUnavailable` that keeps
    /// its parent's code and status, because "git said no" and "git could not
    /// be run" mean opposite things to a caller reading a checkout.
    GitCommandFailed(String),
    /// GitHub did not answer or refused the credential (`adapters/github/client.py`).
    GitHubUnavailable(String),
    /// A peer instance could not be reached, refused, or answered nonsense
    /// (`adapters/peer.py`). `status` is `unreachable`, `refused` or
    /// `invalid_response`.
    PeerUnavailable {
        status: String,
        message: String,
    },
    /// The Forgejo instance did not answer or refused the credential
    /// (`adapters/forgejo/client.py`).
    ForgejoUnavailable(String),
    /// `TransitionRejected` in `core/workflow.py`. The message already starts
    /// with the rule, and `rule` is the field the workflow tests read.
    TransitionRejected {
        rule: String,
        message: String,
    },
}

impl VogtError {
    pub fn message(&self) -> &str {
        match self {
            Self::NotInitialized(m)
            | Self::AlreadyInitialized(m)
            | Self::NotFound(m)
            | Self::Conflict(m)
            | Self::BypassRefused(m)
            | Self::RoleRefused(m)
            | Self::GrantRefused(m)
            | Self::EngineOnly(m)
            | Self::InvalidRequest(m)
            | Self::InstallClosed(m)
            | Self::LoginThrottled(m)
            | Self::InvalidCursor(m)
            | Self::ForgeAccountsNotConfigured(m)
            | Self::ImportParityRefused(m)
            | Self::ImportWorkingTreeDirty(m)
            | Self::ImportBranchDiverged(m)
            | Self::PublishRefused(m)
            | Self::PublishSourceInvalid(m)
            | Self::PublishWorkingTreeDirty(m)
            | Self::PublishNonFastForward(m)
            | Self::RemoteRepoExists(m)
            | Self::NotLinked(m)
            | Self::LinkRefused(m)
            | Self::UpstreamWriteRefused(m)
            | Self::UpstreamWriteFailed(m)
            | Self::InboxEntryNotFound(m)
            | Self::InvalidTriageState(m)
            | Self::PreferenceVersionConflict(m)
            | Self::InvalidPreference(m)
            | Self::InvalidSnooze(m)
            | Self::MissingReason(m)
            | Self::InvalidParams(m)
            | Self::MigrationError(m)
            | Self::MigrationLocked(m)
            | Self::EngineUnavailable(m)
            | Self::GitUnavailable(m)
            | Self::GitCommandFailed(m)
            | Self::GitHubUnavailable(m)
            | Self::ForgejoUnavailable(m) => m,
            Self::PeerUnavailable { message, .. } | Self::TransitionRejected { message, .. } => {
                message
            }
        }
    }

    /// Stable machine-readable code, the `code` class attribute in Python.
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotInitialized(_) => "not_initialized",
            Self::AlreadyInitialized(_) => "already_initialized",
            Self::NotFound(_) => "not_found",
            Self::Conflict(_) => "conflict",
            Self::BypassRefused(_) => "bypass_refused",
            Self::RoleRefused(_) => "role_refused",
            Self::GrantRefused(_) => "grant_refused",
            Self::EngineOnly(_) => "engine_only",
            Self::InvalidRequest(_) => "invalid_request",
            Self::InstallClosed(_) => "install_closed",
            Self::LoginThrottled(_) => "login_throttled",
            Self::InvalidCursor(_) => "invalid_cursor",
            Self::ForgeAccountsNotConfigured(_) => "forge_accounts_not_configured",
            Self::ImportParityRefused(_) => "import_parity_refused",
            Self::ImportWorkingTreeDirty(_) => "import_working_tree_dirty",
            Self::ImportBranchDiverged(_) => "import_branch_diverged",
            Self::PublishRefused(_) => "forge_publish_refused",
            Self::PublishSourceInvalid(_) => "publish_source_invalid",
            Self::PublishWorkingTreeDirty(_) => "publish_working_tree_dirty",
            Self::PublishNonFastForward(_) => "publish_non_fast_forward",
            Self::RemoteRepoExists(_) => "remote_repo_exists",
            Self::NotLinked(_) => "project_not_linked",
            Self::LinkRefused(_) => "forge_link_refused",
            Self::UpstreamWriteRefused(_) => "upstream_write_refused",
            Self::UpstreamWriteFailed(_) => "upstream_write_failed",
            Self::InboxEntryNotFound(_) => "inbox_entry_not_found",
            Self::InvalidTriageState(_) => "invalid_triage_state",
            Self::PreferenceVersionConflict(_) => "preference_version_conflict",
            Self::InvalidPreference(_) => "invalid_preference",
            Self::InvalidSnooze(_) => "invalid_snooze",
            Self::MissingReason(_) => "missing_reason",
            Self::InvalidParams(_) => "invalid_params",
            Self::MigrationError(_) => "migration_error",
            Self::MigrationLocked(_) => "migration_locked",
            Self::EngineUnavailable(_) => "engine_unavailable",
            Self::GitUnavailable(_) | Self::GitCommandFailed(_) => "git_unavailable",
            Self::GitHubUnavailable(_) => "github_unavailable",
            Self::PeerUnavailable { .. } => "peer_unavailable",
            Self::ForgejoUnavailable(_) => "forgejo_unavailable",
            Self::TransitionRejected { .. } => "transition_rejected",
        }
    }

    /// HTTP status the REST adapter uses. Inherited statuses match the Python
    /// class hierarchy: a subclass that does not set `http_status` keeps its
    /// parent's.
    pub fn http_status(&self) -> u16 {
        match self {
            Self::NotInitialized(_) | Self::AlreadyInitialized(_) | Self::Conflict(_) => 409,
            Self::InstallClosed(_)
            | Self::ImportParityRefused(_)
            | Self::ImportWorkingTreeDirty(_)
            | Self::ImportBranchDiverged(_)
            | Self::PublishRefused(_)
            | Self::PublishSourceInvalid(_)
            | Self::PublishWorkingTreeDirty(_)
            | Self::PublishNonFastForward(_)
            | Self::RemoteRepoExists(_)
            | Self::NotLinked(_)
            | Self::UpstreamWriteRefused(_)
            | Self::InvalidTriageState(_)
            | Self::PreferenceVersionConflict(_) => 409,
            Self::NotFound(_) | Self::InboxEntryNotFound(_) => 404,
            Self::BypassRefused(_)
            | Self::RoleRefused(_)
            | Self::GrantRefused(_)
            | Self::EngineOnly(_) => 403,
            Self::InvalidRequest(_)
            | Self::InvalidCursor(_)
            | Self::ForgeAccountsNotConfigured(_)
            | Self::LinkRefused(_)
            | Self::InvalidPreference(_)
            | Self::InvalidSnooze(_)
            | Self::MissingReason(_) => 400,
            Self::LoginThrottled(_) => 429,
            Self::UpstreamWriteFailed(_)
            | Self::EngineUnavailable(_)
            | Self::GitUnavailable(_)
            | Self::GitCommandFailed(_)
            | Self::GitHubUnavailable(_)
            | Self::PeerUnavailable { .. }
            | Self::ForgejoUnavailable(_) => 502,
            Self::InvalidParams(_) => 422,
            Self::MigrationError(_) => 500,
            Self::MigrationLocked(_) => 503,
            Self::TransitionRejected { .. } => 409,
        }
    }

    /// The body `adapters/http/app.py` writes for a `VogtError`.
    pub fn http_body(&self) -> String {
        serde_json::json!({
            "error": {"code": self.code(), "message": self.message()}
        })
        .to_string()
    }
}

impl fmt::Display for VogtError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for VogtError {}

/// Every `(code, http_status)` pair, in the order `errors.py` declares them.
/// The base `VogtError` (`error`, 500) is never raised on its own.
pub fn error_table() -> &'static [(&'static str, u16)] {
    &[
        ("not_initialized", 409),
        ("already_initialized", 409),
        ("not_found", 404),
        ("conflict", 409),
        ("bypass_refused", 403),
        ("role_refused", 403),
        ("grant_refused", 403),
        ("engine_only", 403),
        ("invalid_request", 400),
        ("install_closed", 409),
        ("login_throttled", 429),
        ("invalid_cursor", 400),
        ("forge_accounts_not_configured", 400),
        ("import_parity_refused", 409),
        ("import_working_tree_dirty", 409),
        ("import_branch_diverged", 409),
        ("forge_publish_refused", 409),
        ("publish_source_invalid", 409),
        ("publish_working_tree_dirty", 409),
        ("publish_non_fast_forward", 409),
        ("remote_repo_exists", 409),
        ("project_not_linked", 409),
        ("forge_link_refused", 400),
        ("upstream_write_refused", 409),
        ("upstream_write_failed", 502),
        ("inbox_entry_not_found", 404),
        ("invalid_triage_state", 409),
        ("preference_version_conflict", 409),
        ("invalid_preference", 400),
        ("invalid_snooze", 400),
        ("missing_reason", 400),
        ("invalid_params", 422),
        ("migration_error", 500),
        ("migration_locked", 503),
        ("engine_unavailable", 502),
        ("git_unavailable", 502),
        ("github_unavailable", 502),
        ("peer_unavailable", 502),
        ("forgejo_unavailable", 502),
        ("transition_rejected", 409),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(code: &str) -> VogtError {
        let message = format!("sample {code}");
        match code {
            "not_initialized" => VogtError::NotInitialized(message),
            "already_initialized" => VogtError::AlreadyInitialized(message),
            "not_found" => VogtError::NotFound(message),
            "conflict" => VogtError::Conflict(message),
            "bypass_refused" => VogtError::BypassRefused(message),
            "role_refused" => VogtError::RoleRefused(message),
            "grant_refused" => VogtError::GrantRefused(message),
            "engine_only" => VogtError::EngineOnly(message),
            "invalid_request" => VogtError::InvalidRequest(message),
            "install_closed" => VogtError::InstallClosed(message),
            "login_throttled" => VogtError::LoginThrottled(message),
            "invalid_cursor" => VogtError::InvalidCursor(message),
            "forge_accounts_not_configured" => VogtError::ForgeAccountsNotConfigured(message),
            "import_parity_refused" => VogtError::ImportParityRefused(message),
            "import_working_tree_dirty" => VogtError::ImportWorkingTreeDirty(message),
            "import_branch_diverged" => VogtError::ImportBranchDiverged(message),
            "forge_publish_refused" => VogtError::PublishRefused(message),
            "publish_source_invalid" => VogtError::PublishSourceInvalid(message),
            "publish_working_tree_dirty" => VogtError::PublishWorkingTreeDirty(message),
            "publish_non_fast_forward" => VogtError::PublishNonFastForward(message),
            "remote_repo_exists" => VogtError::RemoteRepoExists(message),
            "project_not_linked" => VogtError::NotLinked(message),
            "forge_link_refused" => VogtError::LinkRefused(message),
            "upstream_write_refused" => VogtError::UpstreamWriteRefused(message),
            "upstream_write_failed" => VogtError::UpstreamWriteFailed(message),
            "inbox_entry_not_found" => VogtError::InboxEntryNotFound(message),
            "invalid_triage_state" => VogtError::InvalidTriageState(message),
            "preference_version_conflict" => VogtError::PreferenceVersionConflict(message),
            "invalid_preference" => VogtError::InvalidPreference(message),
            "invalid_snooze" => VogtError::InvalidSnooze(message),
            "missing_reason" => VogtError::MissingReason(message),
            "invalid_params" => VogtError::InvalidParams(message),
            "migration_error" => VogtError::MigrationError(message),
            "migration_locked" => VogtError::MigrationLocked(message),
            "engine_unavailable" => VogtError::EngineUnavailable(message),
            "git_unavailable" => VogtError::GitUnavailable(message),
            "github_unavailable" => VogtError::GitHubUnavailable(message),
            "peer_unavailable" => VogtError::PeerUnavailable {
                status: "refused".to_string(),
                message,
            },
            "forgejo_unavailable" => VogtError::ForgejoUnavailable(message),
            "transition_rejected" => VogtError::TransitionRejected {
                rule: "transition.not_allowed".to_string(),
                message,
            },
            other => panic!("unmapped code {other}"),
        }
    }

    #[test]
    fn the_codes_match_pythons_error_table() {
        let raw = include_str!("../tests/error_table.json");
        let python: Vec<(String, u16)> = serde_json::from_str(raw).unwrap();
        let mut rust: Vec<(String, u16)> = error_table()
            .iter()
            .map(|(code, status)| ((*code).to_string(), *status))
            .filter(|(code, _)| {
                // The snapshot is `errors.py` as it stood when it was captured,
                // which predates `engine_unavailable` and the adapter codes,
                // and never had `transition_rejected`. They stay in the Rust
                // table and are excluded only from this comparison.
                !matches!(
                    code.as_str(),
                    "transition_rejected"
                        | "engine_unavailable"
                        | "git_unavailable"
                        | "github_unavailable"
                        | "peer_unavailable"
                        | "forgejo_unavailable"
                )
            })
            .collect();
        rust.sort();
        assert_eq!(rust.len(), python.len());
        for (left, right) in rust.iter().zip(&python) {
            assert_eq!(left, right);
        }
    }

    #[test]
    fn every_code_round_trips_with_its_status() {
        assert_eq!(error_table().len(), 40);
        let mut seen = std::collections::BTreeSet::new();
        for (code, status) in error_table() {
            assert!(seen.insert(*code), "duplicate code {code}");
            let error = sample(code);
            assert_eq!(error.code(), *code);
            assert_eq!(error.http_status(), *status);
            let body: serde_json::Value = serde_json::from_str(&error.http_body()).unwrap();
            assert_eq!(body["error"]["code"], *code);
            assert_eq!(body["error"]["message"], format!("sample {code}"));
        }
    }
}
