//! Normalized forge objects, shared across every provider. Ports
//! `adapters/forge/models.py`.
//!
//! A provider reads its own API and returns one of these; a caller never
//! learns which forge answered from the shape it gets back. `None` means "the
//! forge did not say", never false or empty, wherever the Python docstring
//! says so.

/// A repository, resolved to the identity its host understands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoRef {
    pub host: String,
    pub owner: String,
    pub repo: String,
}

impl RepoRef {
    /// `owner/repo`, the half of the identity a person recognises.
    pub fn slug(&self) -> String {
        format!("{}/{}", self.owner, self.repo)
    }
}

/// One repository a credential can see, for the import picker.
///
/// `already_registered` is deliberately absent: a provider knows nothing of
/// what this instance has registered, so the service computes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForgeRepo {
    pub owner: String,
    pub name: String,
    pub default_branch: Option<String>,
    /// "public" or "private", as the forge reports its visibility.
    pub visibility: String,
    pub web_url: String,
}

impl ForgeRepo {
    pub fn slug(&self) -> String {
        format!("{}/{}", self.owner, self.name)
    }
}

/// What a provider can and cannot do, declared rather than discovered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForgeCapabilities {
    pub hosts: Vec<String>,
    pub supports_since: bool,
    pub supports_posture: bool,
    pub supports_notifications: bool,
    pub supports_webhooks: bool,
}

/// One issue, normalized. `state` is `open` or `closed`, never a code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForgeIssue {
    pub number: i64,
    pub title: String,
    pub state: String,
    pub repo: String,
    pub labels: Vec<String>,
    pub author: Option<String>,
    pub author_type: Option<String>,
    pub author_association: Option<String>,
    pub assignees: Vec<String>,
    pub comments: i64,
    /// `None` is "the forge did not include it", never "empty".
    pub body: Option<String>,
    pub updated_at: Option<String>,
    pub closed_at: Option<String>,
    pub source_url: Option<String>,
}

impl ForgeIssue {
    pub fn new(
        number: i64,
        title: impl Into<String>,
        state: impl Into<String>,
        repo: impl Into<String>,
    ) -> Self {
        Self {
            number,
            title: title.into(),
            state: state.into(),
            repo: repo.into(),
            labels: Vec::new(),
            author: None,
            author_type: None,
            author_association: None,
            assignees: Vec::new(),
            comments: 0,
            body: None,
            updated_at: None,
            closed_at: None,
            source_url: None,
        }
    }
}

/// One pull/merge request. `state` is `open` or `closed`; `merged` is the
/// separate fact, because a merged PR and an abandoned one both read `closed`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForgePull {
    pub number: i64,
    pub title: String,
    pub state: String,
    pub repo: String,
    pub draft: bool,
    pub merged: bool,
    pub author: Option<String>,
    pub author_type: Option<String>,
    pub author_association: Option<String>,
    pub head: Option<String>,
    pub head_ref: Option<String>,
    pub base: Option<String>,
    pub body: Option<String>,
    pub labels: Vec<String>,
    pub review_state: Option<String>,
    pub mergeable: Option<String>,
    pub checks: Option<String>,
    pub updated_at: Option<String>,
    pub closed_at: Option<String>,
    pub source_url: Option<String>,
}

impl ForgePull {
    pub fn new(
        number: i64,
        title: impl Into<String>,
        state: impl Into<String>,
        repo: impl Into<String>,
    ) -> Self {
        Self {
            number,
            title: title.into(),
            state: state.into(),
            repo: repo.into(),
            draft: false,
            merged: false,
            author: None,
            author_type: None,
            author_association: None,
            head: None,
            head_ref: None,
            base: None,
            body: None,
            labels: Vec::new(),
            review_state: None,
            mergeable: None,
            checks: None,
            updated_at: None,
            closed_at: None,
            source_url: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForgeRelease {
    pub tag: String,
    pub repo: String,
    pub name: Option<String>,
    pub draft: bool,
    pub prerelease: bool,
    pub published_at: Option<String>,
    pub source_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForgeLabel {
    pub name: String,
    pub repo: String,
    pub color: Option<String>,
    pub description: Option<String>,
}

/// Update-automation posture. `None` is "unknown", `Some(false)` is "off".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForgePosture {
    pub version_updates_config: Option<String>,
    pub vulnerability_alerts: Option<bool>,
    pub automated_security_fixes: Option<bool>,
    pub repo: String,
}

impl ForgePosture {
    /// True when a version-update config file was found.
    pub fn version_updates(&self) -> bool {
        self.version_updates_config.is_some()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForgeNotification {
    pub thread: String,
    pub repo: String,
    pub reason: Option<String>,
    pub unread: bool,
    pub title: String,
    pub subject_type: Option<String>,
    pub updated_at: Option<String>,
    pub last_read_at: Option<String>,
    pub source_url: Option<String>,
    pub subject_api_url: Option<String>,
    pub latest_comment_api_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForgeActor {
    pub login: Option<String>,
    pub user_type: Option<String>,
    pub association: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForgeCheck {
    pub revision: String,
    pub check: String,
    pub repo: String,
    pub status: Option<String>,
    pub conclusion: Option<String>,
    pub branch: Option<String>,
    pub event: Option<String>,
    pub run_number: Option<i64>,
    pub updated_at: Option<String>,
    pub source_url: Option<String>,
    pub extra: serde_json::Map<String, serde_json::Value>,
    pub workflow_path: Option<String>,
    pub run_id: Option<i64>,
    pub run_attempt: Option<i64>,
}

impl ForgeCheck {
    pub fn new(
        revision: impl Into<String>,
        check: impl Into<String>,
        repo: impl Into<String>,
    ) -> Self {
        Self {
            revision: revision.into(),
            check: check.into(),
            repo: repo.into(),
            status: None,
            conclusion: None,
            branch: None,
            event: None,
            run_number: None,
            updated_at: None,
            source_url: None,
            extra: serde_json::Map::new(),
            workflow_path: None,
            run_id: None,
            run_attempt: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForgeJob {
    pub name: String,
    pub conclusion: Option<String>,
    pub source_url: Option<String>,
}

/// How `head` relates to `base`. `ahead_by` may exceed `commits.len()` because
/// the forge pages the commit list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForgeComparison {
    pub base: String,
    pub head: String,
    pub head_sha: Option<String>,
    pub status: Option<String>,
    pub ahead_by: i64,
    pub behind_by: i64,
    /// `(sha, first message line)`, oldest first.
    pub commits: Vec<(String, String)>,
}
