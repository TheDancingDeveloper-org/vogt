//! Write-back policy and result, provider-agnostic. Ports
//! `adapters/forge/writeback.py`.
//!
//! The actions a level permits are `create`, `comment`, `label`,
//! `close`/`reopen` — append, append, append, and a reversible toggle. There
//! is no destructive verb, so "no deletion, no force, ever" holds by
//! construction.

/// `none`, `comment_only`, or `full`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteBackPolicy {
    None,
    CommentOnly,
    Full,
}

impl WriteBackPolicy {
    /// Parse a stored policy string. An unknown string is `None` — "this
    /// project has no policy" and "this project's policy allows nothing" are
    /// the same code path.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "none" => Some(Self::None),
            "comment_only" => Some(Self::CommentOnly),
            "full" => Some(Self::Full),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::CommentOnly => "comment_only",
            Self::Full => "full",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteBackAction {
    Create,
    Comment,
    Label,
    Close,
    Reopen,
}

impl WriteBackAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Comment => "comment",
            Self::Label => "label",
            Self::Close => "close",
            Self::Reopen => "reopen",
        }
    }
}

/// Whether `policy` permits `action`. An unrecognised policy permits nothing,
/// exactly as `permits` does for a string it has no entry for.
pub fn permits(policy: &str, action: &str) -> bool {
    let Some(policy) = WriteBackPolicy::parse(policy) else {
        return false;
    };
    match (policy, action) {
        (WriteBackPolicy::None, _) => false,
        (WriteBackPolicy::CommentOnly, "comment") => true,
        (WriteBackPolicy::CommentOnly, _) => false,
        (WriteBackPolicy::Full, "create" | "comment" | "label" | "close" | "reopen") => true,
        (WriteBackPolicy::Full, _) => false,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteBackOutcome {
    Succeeded,
    Failed,
    Skipped,
}

impl WriteBackOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
        }
    }
}

/// What happened upstream, for the ledger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteBackResult {
    pub outcome: WriteBackOutcome,
    pub detail: Option<String>,
    pub source_url: Option<String>,
    pub subject_key: Option<String>,
}

impl WriteBackResult {
    pub fn failed(detail: impl Into<String>) -> Self {
        Self {
            outcome: WriteBackOutcome::Failed,
            detail: Some(detail.into()),
            source_url: None,
            subject_key: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_table_matches_python() {
        for action in ["create", "comment", "label", "close", "reopen"] {
            assert!(!permits("none", action), "{action}");
            assert!(!permits("no-such-policy", action));
        }
        assert!(permits("comment_only", "comment"));
        assert!(!permits("comment_only", "create"));
        assert!(!permits("comment_only", "close"));
        for action in ["create", "comment", "label", "close", "reopen"] {
            assert!(permits("full", action), "{action}");
        }
        assert!(!permits("full", "delete"));
    }
}
