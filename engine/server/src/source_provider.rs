//! The source provider seam (WI-3392, docs/design/source-providers.md).
//!
//! A provider checks out, inspects, saves and lands one kind of source. The
//! registry picks one by the descriptor's kind, so session code never names a
//! provider. Nothing calls this yet: session start still takes its existing
//! path until the descriptor is carried through `session.start`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use vogt_engine_contract::source::{
    LandOutcome, SavedWork, SourceDescriptor, SourceKind, SourceLocation, SourceLogEntry,
    SourceStatus,
};

/// A provider failure. Raw subprocess output never crosses this boundary.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SourceError {
    #[error("source provider unavailable: {0}")]
    Unavailable(String),
    #[error("not authorized for this source")]
    Unauthorized,
    #[error("invalid source: {0}")]
    InvalidSource(String),
    #[error("checkout has unsaved work")]
    DirtyCheckout,
    #[error("checkout conflicts with its ref")]
    Conflict,
    #[error("work has not landed")]
    NotLanded,
    #[error("{0} is not supported by this provider yet")]
    Unsupported(&'static str),
}

/// A scoped credential: a path to a secret file, never the value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialHandle {
    pub path: PathBuf,
}

/// What to land and how.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LandRequest {
    pub message: String,
    pub work_item: Option<String>,
}

/// The verbs every provider implements.
pub trait SourceProvider: Send + Sync {
    fn kind(&self) -> SourceKind;

    /// Create a clean checkout of the source's ref at `dest`, which must be
    /// missing or empty.
    fn checkout(
        &self,
        source: &SourceDescriptor,
        dest: &Path,
        credential: Option<&CredentialHandle>,
    ) -> Result<(), SourceError>;

    fn mint_credential(
        &self,
        source: &SourceDescriptor,
        session: &str,
    ) -> Result<CredentialHandle, SourceError>;

    fn revoke_credential(&self, credential: &CredentialHandle) -> Result<(), SourceError>;

    fn status(&self, checkout: &Path) -> Result<SourceStatus, SourceError>;

    fn diff(&self, checkout: &Path) -> Result<String, SourceError>;

    fn log(&self, checkout: &Path, limit: usize) -> Result<Vec<SourceLogEntry>, SourceError>;

    /// Persist recoverable work without changing the checkout.
    fn save_unlanded(&self, checkout: &Path, reason: &str) -> Result<SavedWork, SourceError>;

    fn land(
        &self,
        source: &SourceDescriptor,
        checkout: &Path,
        request: &LandRequest,
    ) -> Result<LandOutcome, SourceError>;

    fn report_landed(
        &self,
        source: &SourceDescriptor,
        selector: &str,
    ) -> Result<Vec<SourceLogEntry>, SourceError>;
}

/// Providers by kind, built from what the deployment can do.
#[derive(Clone, Default)]
pub struct SourceRegistry {
    providers: HashMap<SourceKind, Arc<dyn SourceProvider>>,
}

impl SourceRegistry {
    /// A registry holding only the default git provider.
    pub fn with_defaults() -> Self {
        let mut r = Self::default();
        r.register(Arc::new(GitProvider));
        r
    }

    pub fn register(&mut self, provider: Arc<dyn SourceProvider>) {
        self.providers.insert(provider.kind(), provider);
    }

    pub fn for_source(
        &self,
        source: &SourceDescriptor,
    ) -> Result<Arc<dyn SourceProvider>, SourceError> {
        self.providers.get(&source.kind()).cloned().ok_or_else(|| {
            SourceError::Unavailable(format!("no provider for {:?} sources", source.kind()))
        })
    }
}

/// Plain git, the default provider. Credential minting and landing go through
/// the forge broker, which later slices wire in.
pub struct GitProvider;

const WIP_REF_PREFIX: &str = "refs/vogt/wip/";

fn git(dir: &Path, args: &[&str]) -> Result<String, SourceError> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|_| SourceError::Unavailable("git could not be run".into()))?;
    if !out.status.success() {
        return Err(SourceError::Unavailable(format!("git {} failed", args[0])));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn clean_wip_name(reason: &str) -> String {
    let name: String = reason
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let name = name.trim_matches('-');
    if name.is_empty() {
        "wip".into()
    } else {
        name.to_string()
    }
}

impl SourceProvider for GitProvider {
    fn kind(&self) -> SourceKind {
        SourceKind::Git
    }

    fn checkout(
        &self,
        source: &SourceDescriptor,
        dest: &Path,
        credential: Option<&CredentialHandle>,
    ) -> Result<(), SourceError> {
        let SourceLocation::Git { url, .. } = &source.location else {
            return Err(SourceError::InvalidSource("not a git location".into()));
        };
        if url.starts_with('-') || source.reference.starts_with('-') {
            return Err(SourceError::InvalidSource(
                "url or ref starts with '-'".into(),
            ));
        }
        if credential.is_some() {
            return Err(SourceError::Unsupported("credentialed git checkout"));
        }
        if dest.exists()
            && std::fs::read_dir(dest)
                .map_err(|_| SourceError::InvalidSource("destination unreadable".into()))?
                .next()
                .is_some()
        {
            return Err(SourceError::InvalidSource(
                "destination is not empty".into(),
            ));
        }
        let parent = dest
            .parent()
            .ok_or_else(|| SourceError::InvalidSource("destination has no parent".into()))?;
        std::fs::create_dir_all(parent)
            .map_err(|_| SourceError::Unavailable("cannot create destination".into()))?;
        let dest_s = dest
            .to_str()
            .ok_or_else(|| SourceError::InvalidSource("destination is not utf-8".into()))?;
        git(
            parent,
            &[
                "clone",
                "--quiet",
                "--branch",
                &source.reference,
                "--",
                url,
                dest_s,
            ],
        )
        .map(|_| ())
    }

    fn mint_credential(
        &self,
        _source: &SourceDescriptor,
        _session: &str,
    ) -> Result<CredentialHandle, SourceError> {
        Err(SourceError::Unsupported("git credential minting"))
    }

    fn revoke_credential(&self, _credential: &CredentialHandle) -> Result<(), SourceError> {
        Err(SourceError::Unsupported("git credential revocation"))
    }

    fn status(&self, checkout: &Path) -> Result<SourceStatus, SourceError> {
        let out = git(checkout, &["status", "--porcelain=v1", "--branch", "-z"])?;
        let mut status = SourceStatus {
            clean: true,
            ..Default::default()
        };
        for entry in out.split('\0').filter(|e| !e.is_empty()) {
            if let Some(head) = entry.strip_prefix("## ") {
                let name = head.split("...").next().unwrap_or(head);
                let name = name.strip_prefix("No commits yet on ").unwrap_or(name);
                if !name.starts_with("HEAD (no branch)") {
                    status.reference = Some(name.to_string());
                }
            } else if entry.len() > 3 {
                status.clean = false;
                status.changed.push(entry[3..].to_string());
            }
        }
        Ok(status)
    }

    fn diff(&self, checkout: &Path) -> Result<String, SourceError> {
        git(checkout, &["diff", "HEAD", "--no-color", "--no-ext-diff"])
    }

    fn log(&self, checkout: &Path, limit: usize) -> Result<Vec<SourceLogEntry>, SourceError> {
        let n = format!("--max-count={limit}");
        let out = git(checkout, &["log", &n, "--format=%H%x09%s"])?;
        Ok(out
            .lines()
            .filter_map(|l| l.split_once('\t'))
            .map(|(id, summary)| SourceLogEntry {
                id: id.into(),
                summary: summary.into(),
            })
            .collect())
    }

    fn save_unlanded(&self, checkout: &Path, reason: &str) -> Result<SavedWork, SourceError> {
        // `stash create` snapshots tracked changes without touching the tree.
        let sha = git(checkout, &["stash", "create", reason])?
            .trim()
            .to_string();
        if sha.is_empty() {
            // Nothing unsaved: the checkout's HEAD is the recoverable state.
            let head = git(checkout, &["rev-parse", "HEAD"])?.trim().to_string();
            return Ok(SavedWork { id: head });
        }
        let name = format!("{WIP_REF_PREFIX}{}", clean_wip_name(reason));
        git(checkout, &["update-ref", &name, &sha])?;
        Ok(SavedWork { id: sha })
    }

    fn land(
        &self,
        _source: &SourceDescriptor,
        _checkout: &Path,
        _request: &LandRequest,
    ) -> Result<LandOutcome, SourceError> {
        Err(SourceError::Unsupported("git landing"))
    }

    fn report_landed(
        &self,
        _source: &SourceDescriptor,
        _selector: &str,
    ) -> Result<Vec<SourceLogEntry>, SourceError> {
        Err(SourceError::Unsupported("git landed reporting"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vogt_engine_contract::source::LandingMode;

    fn run(dir: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.test")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.test")
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?}");
    }

    fn origin(root: &Path) -> PathBuf {
        let o = root.join("origin");
        std::fs::create_dir(&o).unwrap();
        run(&o, &["init", "--quiet", "--initial-branch=main"]);
        std::fs::write(o.join("a.txt"), "one\n").unwrap();
        run(&o, &["add", "a.txt"]);
        run(&o, &["commit", "--quiet", "-m", "first"]);
        o
    }

    fn source(url: &Path) -> SourceDescriptor {
        SourceDescriptor {
            location: SourceLocation::Git {
                url: url.to_str().unwrap().into(),
                repo: "example.test/o/r".into(),
            },
            reference: "main".into(),
            landing: LandingMode::PushBranch,
        }
    }

    #[test]
    fn registry_picks_git_by_kind() {
        let r = SourceRegistry::with_defaults();
        let p = r.for_source(&source(Path::new("/x"))).unwrap();
        assert_eq!(p.kind(), SourceKind::Git);
    }

    #[test]
    fn registry_refuses_a_kind_it_lacks() {
        let r = SourceRegistry::default();
        let err = r.for_source(&source(Path::new("/x"))).err().unwrap();
        assert!(matches!(err, SourceError::Unavailable(_)));
    }

    #[test]
    fn checkout_status_log_diff_and_save() {
        let tmp = tempfile::tempdir().unwrap();
        let o = origin(tmp.path());
        let dest = tmp.path().join("work");
        let p = GitProvider;
        p.checkout(&source(&o), &dest, None).unwrap();

        let s = p.status(&dest).unwrap();
        assert!(s.clean);
        assert_eq!(s.reference.as_deref(), Some("main"));

        let log = p.log(&dest, 5).unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].summary, "first");
        // A clean checkout saves as its HEAD.
        assert_eq!(p.save_unlanded(&dest, "clean").unwrap().id, log[0].id);

        std::fs::write(dest.join("a.txt"), "two\n").unwrap();
        let s = p.status(&dest).unwrap();
        assert!(!s.clean);
        assert_eq!(s.changed, vec!["a.txt".to_string()]);
        assert!(p.diff(&dest).unwrap().contains("+two"));

        let saved = p.save_unlanded(&dest, "hibernate now").unwrap();
        let wip = git(&dest, &["rev-parse", "refs/vogt/wip/hibernate-now"]).unwrap();
        assert_eq!(wip.trim(), saved.id);
        // Saving leaves the work in place.
        assert!(!p.status(&dest).unwrap().clean);
    }

    #[test]
    fn checkout_refuses_a_non_empty_destination() {
        let tmp = tempfile::tempdir().unwrap();
        let o = origin(tmp.path());
        let dest = tmp.path().join("work");
        std::fs::create_dir(&dest).unwrap();
        std::fs::write(dest.join("x"), "").unwrap();
        let err = GitProvider.checkout(&source(&o), &dest, None).unwrap_err();
        assert!(matches!(err, SourceError::InvalidSource(_)));
    }

    #[test]
    fn checkout_refuses_option_like_urls() {
        let tmp = tempfile::tempdir().unwrap();
        let s = source(Path::new("--upload-pack=x"));
        let dest = tmp.path().join("w");
        assert!(matches!(
            GitProvider.checkout(&s, &dest, None),
            Err(SourceError::InvalidSource(_))
        ));
    }

    #[test]
    fn unwired_verbs_say_so() {
        let s = source(Path::new("/x"));
        assert!(matches!(
            GitProvider.mint_credential(&s, "ses"),
            Err(SourceError::Unsupported(_))
        ));
        let req = LandRequest {
            message: "m".into(),
            work_item: None,
        };
        assert!(matches!(
            GitProvider.land(&s, Path::new("/x"), &req),
            Err(SourceError::Unsupported(_))
        ));
    }
}
