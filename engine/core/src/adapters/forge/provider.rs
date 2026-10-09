//! The forge provider contract. Ports `adapters/forge/provider.py`.
//!
//! Capabilities are declared, not probed, and the write surface is append-only
//! by construction: comment, create, add labels, toggle state, rewrite a body.
//! No method deletes, forces, or replaces an existing set.
//!
//! `create_repo` refuses a name that already exists with
//! [`crate::errors::VogtError::RemoteRepoExists`] rather than adopting it;
//! attaching to an existing repository is a link, a separate explicit act.

use super::models::{
    ForgeActor, ForgeCapabilities, ForgeCheck, ForgeComparison, ForgeIssue, ForgeJob, ForgeLabel,
    ForgeNotification, ForgePosture, ForgePull, ForgeRelease, ForgeRepo, RepoRef,
};
use super::writeback::WriteBackResult;
use crate::errors::VogtError;

pub trait ForgeProvider {
    // -- identity ----------------------------------------------------------

    /// What this forge can do, for a caller to read before it asks.
    fn capabilities(&self) -> &ForgeCapabilities;

    /// Repositories the credential can see. A provider with no usable
    /// credential yields nothing; the caller tells that apart from "the
    /// account has no repositories" from its own knowledge of the token.
    fn list_repos(&self) -> Result<Vec<ForgeRepo>, VogtError>;

    /// Resolve a repository URL. `None` means "not this forge", not malformed.
    fn parse(&self, repo_url: Option<&str>) -> Option<RepoRef>;

    /// The stable key for issue/PR number `n`. The provider owns the scheme so
    /// a second forge cannot collide with github.com's keys.
    fn subject_key(&self, repo: &RepoRef, number: i64) -> String;

    /// The issue/PR number a subject key names, or `None`.
    fn number_of(&self, subject_key: Option<&str>) -> Option<i64>;

    /// The credential-free canonical clone URL.
    fn clone_url(&self, repo: &RepoRef) -> String;

    /// The canonical browser URL.
    fn web_url(&self, repo: &RepoRef) -> String;

    /// Repository metadata, or `None` when it is not visible.
    fn describe(
        &self,
        repo: &RepoRef,
    ) -> Result<Option<serde_json::Map<String, serde_json::Value>>, VogtError>;

    /// The token a git clone should use, if one is configured.
    fn clone_token(&self) -> Option<&str>;

    /// The token owner and reported scopes, or `None` when invalid.
    fn identity(&self) -> Result<Option<(String, String)>, VogtError>;

    // -- read surface ------------------------------------------------------

    /// Issues touched since `since` (all states), or all when `since` is
    /// `None`. `since` is honoured only when `capabilities.supports_since`.
    fn issues_updated_since(
        &self,
        repo: &RepoRef,
        since: Option<&str>,
    ) -> Result<Vec<ForgeIssue>, VogtError>;

    /// Pull/merge requests touched since `since` (all states).
    fn pulls_updated_since(
        &self,
        repo: &RepoRef,
        since: Option<&str>,
    ) -> Result<Vec<ForgePull>, VogtError>;

    /// Published releases, newest first.
    fn releases(&self, repo: &RepoRef) -> Result<Vec<ForgeRelease>, VogtError>;

    /// Recent CI checks, as generic per-revision facts.
    fn checks(&self, repo: &RepoRef) -> Result<Vec<ForgeCheck>, VogtError>;

    /// Recent CI on pushed refs only (branches and tags, never pull requests),
    /// supplementing [`checks`](Self::checks). Empty when the forge has
    /// nothing extra to add.
    fn watched_ref_checks(&self, repo: &RepoRef) -> Result<Vec<ForgeCheck>, VogtError>;

    /// The failed jobs of one run, each with its log link. Empty when the
    /// forge cannot say, or the run has no failed job.
    fn failed_jobs(&self, repo: &RepoRef, run_id: i64) -> Result<Vec<ForgeJob>, VogtError>;

    /// One file's bytes on the default branch, or `None` when absent.
    fn read_file(&self, repo: &RepoRef, path: &str) -> Result<Option<Vec<u8>>, VogtError>;

    /// How `head` relates to `base`, or `None` when the forge cannot compare.
    fn compare(
        &self,
        repo: &RepoRef,
        base: &str,
        head: &str,
    ) -> Result<Option<ForgeComparison>, VogtError>;

    fn labels(&self, repo: &RepoRef) -> Result<Vec<ForgeLabel>, VogtError>;

    /// Meaningful only when `capabilities.supports_posture`; the collector
    /// gates on the capability.
    fn posture(&self, repo: &RepoRef) -> Result<ForgePosture, VogtError>;

    /// Gated on `capabilities.supports_notifications`.
    fn notifications(&self, repo: &RepoRef) -> Result<Vec<ForgeNotification>, VogtError>;

    /// The author of the API resource at `api_url`, or `None`. Only a URL
    /// under this provider's own API root is followed — the URL comes from a
    /// forge payload, and the credential must never be sent to an address the
    /// payload chose.
    fn resolve_actor(&self, api_url: &str) -> Result<Option<ForgeActor>, VogtError>;

    /// Lower-cased member logins, or `None` when unknown. `None` is "unknown",
    /// never "empty".
    fn org_members(&self, owner: &str) -> Result<Option<Vec<String>>, VogtError>;

    // -- write surface (append-only by construction) -----------------------

    fn comment(
        &self,
        repo: &RepoRef,
        number: i64,
        body: &str,
    ) -> Result<WriteBackResult, VogtError>;

    /// Open a new issue. Never edits or replaces an existing one.
    fn create_issue(
        &self,
        repo: &RepoRef,
        title: &str,
        body: &str,
        labels: Option<&[String]>,
    ) -> Result<WriteBackResult, VogtError>;

    /// Add labels. Adds only — never replaces the existing set.
    fn add_labels(
        &self,
        repo: &RepoRef,
        number: i64,
        labels: &[String],
    ) -> Result<WriteBackResult, VogtError>;

    /// Close or reopen. Both directions are recoverable by the other.
    fn set_state(
        &self,
        repo: &RepoRef,
        number: i64,
        state: &str,
    ) -> Result<WriteBackResult, VogtError>;

    /// Re-render an issue body — the one edit verb, bounded to the body.
    fn update_issue_body(
        &self,
        repo: &RepoRef,
        number: i64,
        body: &str,
    ) -> Result<WriteBackResult, VogtError>;

    /// Create a new, empty repository. A name that already exists is
    /// [`VogtError::RemoteRepoExists`], never a clobber.
    fn create_repo(
        &self,
        name: &str,
        private: bool,
        description: Option<&str>,
    ) -> Result<ForgeRepo, VogtError>;
}
