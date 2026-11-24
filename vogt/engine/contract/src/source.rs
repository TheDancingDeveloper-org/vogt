//! A project's source, described once (WI-3392, docs/design/source-providers.md).
//!
//! The descriptor says where a project's source lives and how work lands. It
//! carries no credentials: tokens are minted at runtime by the provider for
//! the descriptor's kind and never appear here.

use serde::{Deserialize, Serialize};

/// Which provider owns a source. Git is the default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SourceKind {
    #[default]
    Git,
    Graft,
}

/// Where a source lives, by kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum SourceLocation {
    /// A clone URL and the repository's stable identity (for example
    /// `github.com/org/repo`).
    Git { url: String, repo: String },
    /// A provider endpoint and the repository name on it.
    Graft { endpoint: String, repo: String },
}

impl SourceLocation {
    pub fn kind(&self) -> SourceKind {
        match self {
            SourceLocation::Git { .. } => SourceKind::Git,
            SourceLocation::Graft { .. } => SourceKind::Graft,
        }
    }

    /// The stable, non-secret repository identity used for grants, labels,
    /// audit and cache keys.
    pub fn repo(&self) -> &str {
        match self {
            SourceLocation::Git { repo, .. } | SourceLocation::Graft { repo, .. } => repo,
        }
    }
}

/// How work on a source lands.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LandingMode {
    #[default]
    PushBranch,
    OpenPr,
    /// Land on the source's ref through its provider (a graft line).
    Line,
}

/// One project's source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceDescriptor {
    pub location: SourceLocation,
    /// A branch for git, a line for graft.
    #[serde(rename = "ref")]
    pub reference: String,
    #[serde(default)]
    pub landing: LandingMode,
}

impl SourceDescriptor {
    pub fn kind(&self) -> SourceKind {
        self.location.kind()
    }
}

/// A checkout's state, the same shape for every provider.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceStatus {
    /// The checkout's current ref, when it is on one.
    #[serde(rename = "ref")]
    pub reference: Option<String>,
    pub clean: bool,
    pub changed: Vec<String>,
}

/// One entry of a checkout's history. `id` is the provider's own identifier
/// (a commit sha, a changeset id).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceLogEntry {
    pub id: String,
    pub summary: String,
}

/// Recoverable work saved by a provider; `id` restores it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedWork {
    pub id: String,
}

/// What a land produced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum LandOutcome {
    /// Submitted for review (a pushed branch, an open pull request).
    Pending {
        id: String,
    },
    Landed {
        id: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_round_trips_and_defaults_landing() {
        let json = r#"{"location":{"kind":"git","url":"https://example.test/o/r.git","repo":"example.test/o/r"},"ref":"main"}"#;
        let d: SourceDescriptor = serde_json::from_str(json).unwrap();
        assert_eq!(d.kind(), SourceKind::Git);
        assert_eq!(d.reference, "main");
        assert_eq!(d.landing, LandingMode::PushBranch);
        assert_eq!(d.location.repo(), "example.test/o/r");
        let back: SourceDescriptor =
            serde_json::from_str(&serde_json::to_string(&d).unwrap()).unwrap();
        assert_eq!(back, d);
    }

    #[test]
    fn graft_location_names_its_kind() {
        let json = r#"{"location":{"kind":"graft","endpoint":"http://h:1","repo":"vogt"},"ref":"rust-2","landing":"line"}"#;
        let d: SourceDescriptor = serde_json::from_str(json).unwrap();
        assert_eq!(d.kind(), SourceKind::Graft);
        assert_eq!(d.landing, LandingMode::Line);
    }

    #[test]
    fn kind_defaults_to_git() {
        assert_eq!(SourceKind::default(), SourceKind::Git);
    }
}
