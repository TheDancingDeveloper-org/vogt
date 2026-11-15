//! Keeping a session pod's unlanded work as a graft draft (WI-3370, WI-4267).
//!
//! The session agent checkpoints the pod's graft checkout into a separate
//! save workspace (`vogt-save-<session>-<gen>`) as one draft, amended in place
//! by every checkpoint, and reports a [`SavedWorkReport`] to the engine. A
//! draft is readable by every holder of the repository's `read` scope, so the
//! agent's pre-commit filter is the security boundary: secret-named files,
//! hardlinks, symlinks leaving the checkout, build directories, files over the
//! caps and files whose bytes match [`TOKEN_SHAPE_PATTERNS`] never reach graft.

use serde::{Deserialize, Serialize};

use crate::salvage::{SalvageSkip, SkipReason};

/// Default cap on one saved file's content.
pub const DEFAULT_SAVE_MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;
/// Default cap on everything one checkpoint copies.
pub const DEFAULT_SAVE_MAX_BYTES: u64 = 64 * 1024 * 1024;
/// Default seconds between periodic checkpoints.
pub const DEFAULT_SAVE_INTERVAL_SECS: u64 = 300;
/// The lease every checkpoint sets on its save workspace: graft's 24 h maximum.
pub const SAVE_LEASE_SECS: u64 = 86_400;

/// `0` turns pod saves off (the pod keeps salvage-only behaviour).
pub const ENV_SAVE: &str = "ENGINE_POD_SAVE";
/// Seconds between periodic checkpoints; `0` turns the timer off.
pub const ENV_SAVE_INTERVAL: &str = "ENGINE_POD_SAVE_INTERVAL";
pub const ENV_SAVE_MAX_FILE_BYTES: &str = "ENGINE_POD_SAVE_MAX_FILE_BYTES";
pub const ENV_SAVE_MAX_BYTES: &str = "ENGINE_POD_SAVE_MAX_BYTES";
/// The pod incarnation counter the engine bumps on each wake; it names the
/// save workspace. Absent means `0`.
pub const ENV_POD_GEN: &str = "VOGT_POD_GEN";
/// Where the save scratch lives (the pod's `/save` emptyDir).
pub const ENV_SAVE_DIR: &str = "VOGT_SAVE_DIR";
pub const DEFAULT_SAVE_DIR: &str = "/save";

/// Directory names never saved at any depth: build output and tool state.
/// They are skipped even when the repository's `.gitignore` forgets them.
pub const SAVE_EXCLUDED_DIRS: &[&str] = &[
    "target",
    "node_modules",
    ".venv",
    "__pycache__",
    ".m2",
    ".cargo",
    "dist",
    "build",
    ".gradle",
    ".next",
    ".graft",
    ".git",
    ".vogt",
];

/// Content shapes that mark a credential. A file whose bytes match any of
/// these is never saved. They mirror the engine's redaction token shapes,
/// plus graft's PASETO tokens. Patterns are for `regex::bytes` and are
/// matched against at most the per-file cap.
pub const TOKEN_SHAPE_PATTERNS: &[&str] = &[
    // GitHub classic and fine-grained tokens.
    r"\bgh[pousr]_[A-Za-z0-9]{20,}",
    r"\bgithub_pat_[A-Za-z0-9_]{20,}",
    // GitLab.
    r"\bglpat-[A-Za-z0-9_-]{16,}",
    // OpenAI and Anthropic (`sk-ant-…`) keys.
    r"\bsk-[A-Za-z0-9_-]{16,}",
    // Slack.
    r"\bxox[abposr]-[A-Za-z0-9-]{10,}",
    // AWS access key ids.
    r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b",
    // Google API keys and OAuth access tokens.
    r"\bAIza[0-9A-Za-z_-]{30,}",
    r"\bya29\.[0-9A-Za-z_-]{20,}",
    // npm, Hugging Face, Stripe, age.
    r"\bnpm_[A-Za-z0-9]{30,}",
    r"\bhf_[A-Za-z0-9]{30,}",
    r"\b(?:pk|rk|sk)_(?:live|test)_[A-Za-z0-9]{16,}",
    r"\bAGE-SECRET-KEY-1[0-9A-Z]{20,}",
    // A PEM private key of any kind.
    r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----",
    // JWT-shaped tokens (Kubernetes service-account tokens among them).
    r"\beyJ[A-Za-z0-9_-]{6,}\.[A-Za-z0-9_-]{6,}\.[A-Za-z0-9_-]{6,}",
    // graft tokens (PASETO v4.public).
    r"\bv4\.public\.[A-Za-z0-9_-]{40,}",
];

/// What asked for a checkpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SaveTrigger {
    /// The agent's own timer (`ENGINE_POD_SAVE_INTERVAL`).
    Periodic,
    /// The engine saw the session go idle.
    Idle,
    /// The final save before a hibernation.
    Hibernate,
    /// The final save before a stop.
    Stop,
}

impl SaveTrigger {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Periodic => "periodic",
            Self::Idle => "idle",
            Self::Hibernate => "hibernate",
            Self::Stop => "stop",
        }
    }
}

/// The draft a previous checkpoint left, kept as a fallback.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedDraftRef {
    pub draft: String,
    pub base: String,
}

/// One successful checkpoint, as the agent reports it and the engine keeps it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedWorkReport {
    /// The source provider that holds the draft (`graft`).
    pub provider: String,
    pub repo: String,
    pub line: String,
    /// The save workspace, `vogt-save-<session>-<gen>`.
    pub workspace: String,
    /// The draft holding the work; `None` when there was nothing to save.
    #[serde(default)]
    pub draft: Option<String>,
    /// The line changeset the draft applies to.
    pub base: String,
    /// Rises with every successful checkpoint of one pod incarnation.
    pub seq: u64,
    /// The pod incarnation that made it.
    #[serde(default)]
    pub gen: u64,
    /// RFC 3339, set by the agent.
    pub at: String,
    /// Files written into the draft (deletes included).
    #[serde(default)]
    pub files: u64,
    /// Bytes copied into the draft.
    #[serde(default)]
    pub bytes: u64,
    /// Paths deliberately left out, with why. Names only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skipped: Vec<SalvageSkip>,
    /// Nothing was left out apart from excluded build directories.
    pub complete: bool,
    pub trigger: SaveTrigger,
    /// The draft before this one, set by the engine when it keeps a report.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous: Option<SavedDraftRef>,
}

impl SavedWorkReport {
    /// Whether a list of skips leaves the save complete: only excluded
    /// directories ([`SkipReason::Ignored`]) may be missing.
    pub fn complete_with(skipped: &[SalvageSkip]) -> bool {
        skipped.iter().all(|s| s.reason == SkipReason::Ignored)
    }

    /// Whether a skip came from a cap (what makes an explicit hibernate
    /// refuse), rather than from a deliberate secret or exclusion rule.
    pub fn capped(&self) -> bool {
        self.skipped
            .iter()
            .any(|s| s.reason == SkipReason::TooLarge)
    }
}

/// The outcome of one checkpoint, as the `Saved` and `Checkpointed` frames
/// carry it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SaveOutcome {
    Saved {
        report: Box<SavedWorkReport>,
    },
    /// The checkpoint could not be made; nothing new reached graft.
    SaveFailed {
        trigger: SaveTrigger,
        reason: String,
    },
    /// This pod does not save (a git checkout, saves turned off, no graft
    /// checkout at the clone root).
    Unsupported {
        reason: String,
    },
}

impl SaveOutcome {
    /// Why this outcome must hold an explicit hibernate back (design §2.8):
    /// the save failed, or a cap left files out. Secret and excluded skips
    /// are deliberate and never refuse; a pod that does not save (a git
    /// checkout, saves off) has nothing to refuse over.
    pub fn refusal(&self) -> Option<String> {
        match self {
            Self::SaveFailed { reason, .. } => Some(reason.clone()),
            Self::Saved { report } if report.capped() => {
                let names: Vec<&str> = report
                    .skipped
                    .iter()
                    .filter(|s| s.reason == SkipReason::TooLarge)
                    .take(5)
                    .map(|s| s.path.as_str())
                    .collect();
                Some(format!(
                    "files are over the save caps and were left out: {}",
                    names.join(", ")
                ))
            }
            Self::Saved { .. } | Self::Unsupported { .. } => None,
        }
    }

    /// The trigger the agent ran this checkpoint for, when it says.
    pub fn trigger(&self) -> Option<SaveTrigger> {
        match self {
            Self::Saved { report } => Some(report.trigger),
            Self::SaveFailed { trigger, .. } => Some(*trigger),
            Self::Unsupported { .. } => None,
        }
    }
}

/// A checkpoint that could not be made, as the engine keeps it (WI-4268).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SaveFailure {
    pub trigger: SaveTrigger,
    pub reason: String,
    /// RFC 3339, when the engine heard of it.
    pub at: String,
}

/// An idle-policy hibernation the engine held back because the session's
/// unlanded work could not be saved (WI-4268, design §2.8). The pod keeps
/// running; the policy retries with backoff.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HibernateDeferral {
    pub reason: String,
    /// Rounds deferred in a row.
    pub failures: u32,
    /// RFC 3339, the latest deferral.
    pub at: String,
    /// The policy stopped retrying after too many failures; it tries again
    /// once the session's work changes.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub paused: bool,
}

/// Seconds the engine waits for a pod's final checkpoint (and for the
/// non-final one an explicit hibernate checks first).
pub const ENV_FINAL_SAVE_TIMEOUT: &str = "ENGINE_POD_FINAL_SAVE_TIMEOUT";
pub const DEFAULT_FINAL_SAVE_TIMEOUT_SECS: u64 = 60;
/// `1` lets the reaper delete a pod whose session's save covers it. Off by
/// default until live acceptance passes (operator decision D6).
pub const ENV_REAP_COVERED: &str = "ENGINE_POD_REAP_COVERED";

/// The save workspace's name for a session and pod incarnation. Graft takes
/// lowercase ASCII, digits and `._-/` only, at most 128 bytes; anything else
/// in the session id is replaced.
pub fn save_workspace_name(session: &str, gen: u64) -> String {
    let session: String = session
        .chars()
        .take(80)
        .map(|c| {
            let c = c.to_ascii_lowercase();
            if c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect();
    format!("vogt-save-{session}-{gen}")
}

/// Whether `path` (relative, `/` separated) lies under a directory that is
/// never saved. Returns the excluded directory prefix (`a/target`) when so.
pub fn excluded_dir_prefix(path: &str) -> Option<String> {
    let components: Vec<&str> = path.split('/').collect();
    let dirs = components.len().saturating_sub(1);
    for (index, component) in components.iter().take(dirs).enumerate() {
        if SAVE_EXCLUDED_DIRS.contains(component) {
            return Some(components[..=index].join("/"));
        }
    }
    // A top-level file named like tool state (`.vogt`, `.graft`) is excluded
    // too; a file merely named `build` or `dist` is ordinary source.
    match components.as_slice() {
        [single] if matches!(*single, ".graft" | ".git" | ".vogt") => Some(single.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_names_are_graft_safe() {
        assert_eq!(
            save_workspace_name("4F2c-Session_1", 3),
            "vogt-save-4f2c-session_1-3"
        );
        assert_eq!(save_workspace_name("a b/c", 0), "vogt-save-a-b-c-0");
        assert!(save_workspace_name(&"x".repeat(500), u64::MAX).len() <= 128);
    }

    #[test]
    fn excluded_directories_are_found_at_any_depth() {
        assert_eq!(excluded_dir_prefix("target/x.o"), Some("target".into()));
        assert_eq!(
            excluded_dir_prefix("web/node_modules/a/b.js"),
            Some("web/node_modules".into())
        );
        assert_eq!(excluded_dir_prefix(".vogt/restored"), Some(".vogt".into()));
        assert_eq!(excluded_dir_prefix(".graft"), Some(".graft".into()));
        assert_eq!(excluded_dir_prefix("src/build.rs"), None);
        assert_eq!(excluded_dir_prefix("build"), None);
        assert_eq!(excluded_dir_prefix("src/main.rs"), None);
    }

    #[test]
    fn completeness_ignores_only_exclusions() {
        let ignored = SalvageSkip {
            path: "target".into(),
            reason: SkipReason::Ignored,
        };
        let secret = SalvageSkip {
            path: ".env".into(),
            reason: SkipReason::Secret,
        };
        assert!(SavedWorkReport::complete_with(&[]));
        assert!(SavedWorkReport::complete_with(std::slice::from_ref(
            &ignored
        )));
        assert!(!SavedWorkReport::complete_with(&[ignored, secret]));
    }

    #[test]
    fn outcomes_round_trip_and_a_clean_save_has_no_draft() {
        let report = SavedWorkReport {
            provider: "graft".into(),
            repo: "vogt".into(),
            line: "rust-2".into(),
            workspace: save_workspace_name("s", 1),
            draft: None,
            base: "c1".into(),
            seq: 1,
            gen: 1,
            at: "2026-11-15T00:00:00Z".into(),
            files: 0,
            bytes: 0,
            skipped: Vec::new(),
            complete: true,
            trigger: SaveTrigger::Periodic,
            previous: None,
        };
        let outcome = SaveOutcome::Saved {
            report: Box::new(report.clone()),
        };
        let json = serde_json::to_value(&outcome).unwrap();
        assert_eq!(json["state"], "saved");
        assert_eq!(json["report"]["draft"], serde_json::Value::Null);
        assert_eq!(json["report"]["trigger"], "periodic");
        assert_eq!(
            serde_json::from_value::<SaveOutcome>(json).unwrap(),
            outcome
        );
        // An older engine's record without the newer fields still decodes.
        let old = serde_json::json!({
            "provider": "graft", "repo": "vogt", "line": "rust-2",
            "workspace": "w", "base": "c1", "seq": 2, "at": "now",
            "complete": false, "trigger": "idle"
        });
        let decoded: SavedWorkReport = serde_json::from_value(old).unwrap();
        assert_eq!(decoded.draft, None);
        assert_eq!(decoded.gen, 0);
        let failed = SaveOutcome::SaveFailed {
            trigger: SaveTrigger::Hibernate,
            reason: "graft down".into(),
        };
        let json = serde_json::to_string(&failed).unwrap();
        assert_eq!(serde_json::from_str::<SaveOutcome>(&json).unwrap(), failed);
    }

    #[test]
    fn capped_reports_only_size_skips() {
        let mut report = SavedWorkReport {
            provider: "graft".into(),
            repo: "r".into(),
            line: "l".into(),
            workspace: "w".into(),
            draft: Some("d".into()),
            base: "b".into(),
            seq: 1,
            gen: 0,
            at: "now".into(),
            files: 1,
            bytes: 1,
            skipped: vec![SalvageSkip {
                path: ".env".into(),
                reason: SkipReason::Secret,
            }],
            complete: false,
            trigger: SaveTrigger::Idle,
            previous: None,
        };
        assert!(!report.capped());
        report.skipped.push(SalvageSkip {
            path: "big.bin".into(),
            reason: SkipReason::TooLarge,
        });
        assert!(report.capped());
    }
}
