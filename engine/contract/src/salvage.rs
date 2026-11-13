//! Salvaging a session pod's uncommitted work (WI-4132, WI-3370).
//!
//! A pod's `/work` goes with the pod. Before an explicit stop or a
//! hibernation deletes it, the session agent builds this structure from the
//! checkout and uploads it to the engine, which keeps it under
//! `pod-salvage/<session>/<timestamp>/` (see `docs/ENGINE.md`).
//!
//! A patch is a diff against the checkout's base, produced by the source's
//! own tool (`git diff --binary`, or `graft diff` for a graft working copy).
//! Untracked files travel separately so a caller can restore content the
//! patch format does not carry; ignored files and anything that looks like a
//! credential are never included.

use serde::{Deserialize, Serialize};

/// Default cap on one repository's patch.
pub const DEFAULT_MAX_REPO_BYTES: u64 = 8 * 1024 * 1024;
/// Default cap on one salvage archive.
pub const DEFAULT_MAX_TOTAL_BYTES: u64 = 32 * 1024 * 1024;
/// Default cap on one untracked file's content.
pub const DEFAULT_MAX_UNTRACKED_FILE_BYTES: u64 = 1024 * 1024;
/// How many archives one session keeps.
pub const DEFAULT_KEEP_PER_SESSION: usize = 5;
/// Cap on everything kept for one session, across its archives.
pub const DEFAULT_MAX_SESSION_BYTES: u64 = 128 * 1024 * 1024;

/// The bounds a planner applies. The engine applies its own copy to whatever
/// an agent sends; neither side trusts the other's counting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SalvageLimits {
    pub max_repo_bytes: u64,
    pub max_total_bytes: u64,
    pub max_untracked_file_bytes: u64,
}

impl Default for SalvageLimits {
    fn default() -> Self {
        Self {
            max_repo_bytes: DEFAULT_MAX_REPO_BYTES,
            max_total_bytes: DEFAULT_MAX_TOTAL_BYTES,
            max_untracked_file_bytes: DEFAULT_MAX_UNTRACKED_FILE_BYTES,
        }
    }
}

/// Why a path was left out of a salvage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    /// The name is a credential, a key, or a token store.
    Secret,
    /// The file or the repository's patch is over the size bound.
    TooLarge,
    /// The checkout's own tool does not list it (ignored, or a submodule).
    Ignored,
    /// The file could not be read.
    Unreadable,
}

/// One path a salvage deliberately does not carry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SalvageSkip {
    pub path: String,
    pub reason: SkipReason,
}

/// One untracked file whose content travels with the archive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SalvageUntrackedFile {
    /// Relative to the repository root, always with `/`.
    pub path: String,
    /// Standard base64 of the file's bytes.
    pub bytes: String,
}

/// One repository in the upload.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SalvageRepoUpload {
    /// The repository's path relative to the session's `/work`, `/` for the
    /// root itself. The engine turns it into a directory name.
    pub name: String,
    /// The source's reference (a branch or a graft line) at salvage time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    /// The checkout's base identity (a commit sha or a graft base).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    /// A unified, binary-safe patch against the base.
    #[serde(default)]
    pub patch: String,
    /// Paths the provider reported as new and patch-carried (informational).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub untracked: Vec<String>,
    /// Untracked content the provider's patch does not carry (git).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub untracked_files: Vec<SalvageUntrackedFile>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skipped: Vec<SalvageSkip>,
    /// The patch was over the bound and is not in `patch`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
}

/// What the session agent sends the engine.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SalvageUpload {
    #[serde(default)]
    pub repos: Vec<SalvageRepoUpload>,
}

impl SalvageUpload {
    /// Whether the checkout had nothing to save.
    pub fn is_empty(&self) -> bool {
        self.repos.is_empty()
    }

    pub fn repo_names(&self) -> Vec<String> {
        self.repos.iter().map(|r| r.name.clone()).collect()
    }
}

/// Whether an archive was written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SalvageState {
    #[default]
    Saved,
    /// The archive could not be made; `reason` says why.
    SalvageFailed,
}

/// What the engine recorded and what the stop result and the PWA show.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SalvageReport {
    #[serde(default)]
    pub state: SalvageState,
    /// The archive directory on the engine's state volume, when saved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Files in the archive, manifest included.
    #[serde(default)]
    pub files: u64,
    /// Bytes of patch and untracked content written.
    #[serde(default)]
    pub bytes: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub repos: Vec<String>,
    /// Why it failed, or what it left out, in words a person can use.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// RFC 3339, set by the engine.
    #[serde(default)]
    pub at: String,
}

impl SalvageReport {
    pub fn failed(reason: impl Into<String>, at: impl Into<String>) -> Self {
        Self {
            state: SalvageState::SalvageFailed,
            reason: Some(reason.into()),
            at: at.into(),
            ..Self::default()
        }
    }

    pub fn is_failed(&self) -> bool {
        self.state == SalvageState::SalvageFailed
    }
}

/// The engine's answer to `POST /api/sessions/{id}/kill` (WI-4132). `ok`
/// stays for callers that only check the stop happened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StopResponse {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub salvage: Option<SalvageReport>,
}

/// What the agent reports on the stream once the upload is done (`Salvaged`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SalvageAck {
    pub ok: bool,
    #[serde(default)]
    pub files: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Names never written into an archive, at any depth. Case-insensitive, and
/// matched against the basename. This is a floor, not a scanner: the planner
/// also refuses paths whose components are well-known credential directories.
pub const SECRET_NAMES: &[&str] = &[
    ".env",
    ".netrc",
    ".npmrc",
    ".pypirc",
    ".git-credentials",
    ".htpasswd",
    "id_rsa",
    "id_dsa",
    "id_ecdsa",
    "id_ed25519",
    "credentials",
    "credentials.json",
    "auth.json",
    "known_hosts",
];

/// Directory components that mark a credential store wherever they appear.
pub const SECRET_DIRS: &[&str] = &[
    ".ssh",
    ".aws",
    ".gnupg",
    ".docker",
    ".kube",
    "secrets",
    "credentials",
    "serviceaccount",
    "service-account",
];

/// File suffixes never written into an archive.
pub const SECRET_SUFFIXES: &[&str] = &[
    ".pem",
    ".key",
    ".p12",
    ".pfx",
    ".jks",
    ".keystore",
    ".kdbx",
    ".token",
    ".secret",
    ".crt",
];

/// Whether `path` (relative, `/` separated) looks like a credential. Errs on
/// the side of leaving the file out: a missed edit is recoverable from the
/// transcript; a leaked key is not.
pub fn is_secret_path(path: &str) -> bool {
    let normalized = path.replace('\\', "/");
    let mut components = normalized.split('/').peekable();
    while let Some(component) = components.next() {
        let lower = component.to_ascii_lowercase();
        if SECRET_DIRS.contains(&lower.as_str()) {
            return true;
        }
        if lower.contains("credential") || lower.contains("service-account") {
            return true;
        }
        // A dotenv file with a suffix is still a dotenv file.
        if lower == ".env" || lower.starts_with(".env.") {
            return true;
        }
        if components.peek().is_none() {
            if SECRET_NAMES.contains(&lower.as_str()) {
                return true;
            }
            if SECRET_SUFFIXES.iter().any(|suffix| lower.ends_with(suffix)) {
                return true;
            }
        }
    }
    false
}

/// A safe archive directory name for a repository's relative path.
pub fn repo_slug(name: &str) -> String {
    let trimmed = name.trim_matches('/');
    if trimmed.is_empty() || trimmed == "." {
        return "root".to_string();
    }
    trimmed
        .split('/')
        .filter(|c| !c.is_empty() && *c != "." && *c != "..")
        .map(|c| {
            c.chars()
                .map(|ch| {
                    if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                        ch
                    } else {
                        '_'
                    }
                })
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("__")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_are_recognized_by_name_and_directory() {
        for path in [
            ".env",
            ".env.local",
            "config/.env.production",
            "keys/server.pem",
            "id_rsa",
            "home/.ssh/config",
            ".aws/credentials",
            "var/run/secrets/kubernetes.io/serviceaccount/token",
            ".config/graft/sessions/ses_TEST/trial-1.token",
            "private/client-credential.txt",
            "auth.json",
            "deploy.p12",
            ".git-credentials",
        ] {
            assert!(is_secret_path(path), "{path} should be secret");
        }
        for path in [
            "src/main.rs",
            "docs/ENGINE.md",
            "environment.md",
            "keyboard.rs",
            "certs/README.md",
        ] {
            assert!(!is_secret_path(path), "{path} should not be secret");
        }
    }

    #[test]
    fn repo_slugs_are_safe_and_stable() {
        assert_eq!(repo_slug("."), "root");
        assert_eq!(repo_slug("./"), "root");
        assert_eq!(repo_slug("sub/repo"), "sub__repo");
        assert_eq!(repo_slug("../evil"), "evil");
        assert_eq!(repo_slug("a b/c.d"), "a_b__c.d");
    }

    #[test]
    fn reports_round_trip_and_failures_carry_a_reason() {
        let saved = SalvageReport {
            state: SalvageState::Saved,
            path: Some("/state/pod-salvage/s/1".into()),
            files: 2,
            bytes: 40,
            repos: vec!["root".into()],
            reason: None,
            at: "2026-11-12T00:00:00Z".into(),
        };
        let json = serde_json::to_string(&saved).unwrap();
        assert_eq!(serde_json::from_str::<SalvageReport>(&json).unwrap(), saved);
        let failed = SalvageReport::failed("agent not attached", "now");
        assert!(failed.is_failed());
        assert_eq!(failed.reason.as_deref(), Some("agent not attached"));
    }
}
