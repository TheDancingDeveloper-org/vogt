//! Build the salvage upload for the checkout under a session pod's `/work`
//! (WI-4132, WI-3370).
//!
//! The engine asks for this before an explicit stop or hibernation deletes
//! the pod. Each checkout is handled by the tool that owns it: a graft
//! working copy through `graft status`/`graft diff`, a git clone through
//! `git status`/`git diff`. Nothing is landed, pushed or committed — the
//! upload is the whole durable copy.
//!
//! Ignored files never appear in either provider's change list, so they are
//! not carried. Credential-looking names are refused even when they do
//! appear, and size bounds bound what one archive may hold.

use std::{
    fs,
    path::{Component, Path, PathBuf},
    process::Command,
};

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use serde::Deserialize;
use vogt_engine_contract::salvage::{
    is_secret_path, SalvageLimits, SalvageRepoUpload, SalvageSkip, SalvageUntrackedFile,
    SalvageUpload, SkipReason,
};

/// How deep below the session's `/work` a checkout may sit.
const MAX_REPO_DEPTH: usize = 3;

/// Directories never descended into while looking for checkouts.
const EXCLUDED_DIRS: &[&str] = &[
    ".graft",
    ".git",
    "node_modules",
    "target",
    ".venv",
    "__pycache__",
    ".m2",
    ".cargo",
];

/// The outcome of one salvage attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanOutcome {
    pub upload: SalvageUpload,
    /// Repositories found but left out, with the reason.
    pub failed: Vec<String>,
}

impl PlanOutcome {
    pub fn is_empty(&self) -> bool {
        self.upload.is_empty()
    }
}

/// Build the salvage for every checkout at or below `root`.
pub fn plan(root: &Path, limits: &SalvageLimits) -> PlanOutcome {
    let mut upload = SalvageUpload::default();
    let mut failed = Vec::new();
    let mut budget = limits.max_total_bytes;
    for repo in discover(root) {
        let name = relative_name(root, &repo);
        match plan_repo(&repo, &name, limits, &mut budget) {
            Ok(Some(entry)) => upload.repos.push(entry),
            Ok(None) => {}
            Err(reason) => failed.push(format!("{name}: {reason}")),
        }
    }
    PlanOutcome { upload, failed }
}

fn relative_name(root: &Path, repo: &Path) -> String {
    let rel = repo.strip_prefix(root).unwrap_or(repo);
    let text = rel.to_string_lossy().replace('\\', "/");
    if text.is_empty() {
        ".".to_string()
    } else {
        text
    }
}

/// Checkouts at or below `root`: `root` itself when it is one, otherwise its
/// descendants up to [`MAX_REPO_DEPTH`]. A checkout is not entered: nested
/// checkouts inside it are not part of its changes.
fn discover(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    if is_checkout(root) {
        return vec![root.to_path_buf()];
    }
    let mut level = vec![(root.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = level.pop() {
        if depth >= MAX_REPO_DEPTH {
            continue;
        }
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if !entry.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') || EXCLUDED_DIRS.contains(&name.as_ref()) {
                continue;
            }
            if is_checkout(&path) {
                found.push(path);
            } else {
                level.push((path, depth + 1));
            }
        }
    }
    found.sort();
    found
}

fn is_checkout(dir: &Path) -> bool {
    dir.join(".git").exists() || dir.join(".graft").exists()
}

fn plan_repo(
    repo: &Path,
    name: &str,
    limits: &SalvageLimits,
    budget: &mut u64,
) -> Result<Option<SalvageRepoUpload>, String> {
    if repo.join(".graft").exists() {
        plan_graft(repo, name, limits, budget)
    } else {
        plan_git(repo, name, limits, budget)
    }
}

/// What a provider read from one repository.
struct Finished {
    name: String,
    reference: Option<String>,
    base: Option<String>,
    patch: String,
    untracked: Vec<String>,
    untracked_files: Vec<SalvageUntrackedFile>,
    skipped: Vec<SalvageSkip>,
    truncated: bool,
}

/// One repository's entry, or `None` when it is clean.
fn finish(
    finished: Finished,
    limits: &SalvageLimits,
    budget: &mut u64,
) -> Option<SalvageRepoUpload> {
    let Finished {
        name,
        reference,
        base,
        patch,
        untracked,
        untracked_files,
        skipped,
        mut truncated,
    } = finished;
    let carried: u64 = untracked_files.iter().map(|f| f.bytes.len() as u64).sum();
    let mut patch = patch;
    let left = budget.saturating_sub(carried);
    if patch.len() as u64 > limits.max_repo_bytes || patch.len() as u64 > left {
        patch.clear();
        truncated = true;
    }
    *budget = budget.saturating_sub(patch.len() as u64 + carried);
    if patch.is_empty()
        && untracked_files.is_empty()
        && untracked.is_empty()
        && skipped.is_empty()
        && !truncated
    {
        return None;
    }
    Some(SalvageRepoUpload {
        name,
        reference,
        base,
        patch,
        untracked,
        untracked_files,
        skipped,
        truncated,
    })
}

fn run(program: &str, dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new(program)
        .current_dir(dir)
        .args(args)
        .output()
        .map_err(|e| format!("could not run {program}: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "{program} {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[derive(Deserialize)]
struct GraftStatus {
    #[serde(default)]
    base: Option<String>,
    #[serde(default)]
    line: Option<String>,
    #[serde(default)]
    changes: Vec<GraftChange>,
    #[serde(default)]
    clean: bool,
}

#[derive(Deserialize)]
struct GraftChange {
    path: String,
    #[serde(default)]
    status: String,
}

fn plan_graft(
    repo: &Path,
    name: &str,
    limits: &SalvageLimits,
    budget: &mut u64,
) -> Result<Option<SalvageRepoUpload>, String> {
    let status_text = run("graft", repo, &["status", "--json"])?;
    let status: GraftStatus = serde_json::from_str(&status_text)
        .map_err(|e| format!("graft status returned invalid JSON: {e}"))?;
    if status.clean && status.changes.is_empty() {
        return Ok(None);
    }
    let mut skipped = Vec::new();
    let mut untracked = Vec::new();
    let mut secret = false;
    for change in &status.changes {
        if is_secret_path(&change.path) {
            skipped.push(SalvageSkip {
                path: change.path.clone(),
                reason: SkipReason::Secret,
            });
            secret = true;
        } else if change.status == "added" {
            untracked.push(change.path.clone());
        }
    }
    // A patch cannot be split per path without trusting the provider's
    // pagination; when any changed path is a credential, the archive keeps
    // no patch rather than risk carrying it.
    let patch = if secret {
        String::new()
    } else {
        run("graft", repo, &["diff", "--limit", "10000"])?
    };
    Ok(finish(
        Finished {
            name: name.to_string(),
            reference: status.line,
            base: status.base,
            patch,
            untracked,
            untracked_files: Vec::new(),
            skipped,
            truncated: secret,
        },
        limits,
        budget,
    ))
}

fn plan_git(
    repo: &Path,
    name: &str,
    limits: &SalvageLimits,
    budget: &mut u64,
) -> Result<Option<SalvageRepoUpload>, String> {
    let status = run(
        "git",
        repo,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )?;
    let (changed, untracked_paths) = parse_porcelain(&status);
    if changed.is_empty() && untracked_paths.is_empty() {
        return Ok(None);
    }
    let mut skipped = Vec::new();
    let mut secret = false;
    for path in &changed {
        if is_secret_path(path) {
            skipped.push(SalvageSkip {
                path: path.clone(),
                reason: SkipReason::Secret,
            });
            secret = true;
        }
    }
    let mut untracked_files = Vec::new();
    let mut untracked = Vec::new();
    // Untracked bytes count against the pod-wide total, not only the server's.
    let mut remaining = *budget;
    for path in untracked_paths {
        if is_secret_path(&path) {
            skipped.push(SalvageSkip {
                path,
                reason: SkipReason::Secret,
            });
            continue;
        }
        let full = repo.join(&path);
        // lstat, never stat: a link (to the pod's service-account or graft
        // token, say) is not carried, and its target is never read.
        let Ok(meta) = fs::symlink_metadata(&full) else {
            skipped.push(SalvageSkip {
                path,
                reason: SkipReason::Unreadable,
            });
            continue;
        };
        if !meta.file_type().is_file() {
            skipped.push(SalvageSkip {
                path,
                reason: SkipReason::Unreadable,
            });
            continue;
        }
        if meta.len() > limits.max_untracked_file_bytes || meta.len() > remaining {
            skipped.push(SalvageSkip {
                path,
                reason: SkipReason::TooLarge,
            });
            continue;
        }
        match read_no_follow(&full) {
            Ok(bytes) if bytes.len() as u64 > remaining => skipped.push(SalvageSkip {
                path,
                reason: SkipReason::TooLarge,
            }),
            Ok(bytes) => {
                remaining -= bytes.len() as u64;
                untracked.push(path.clone());
                untracked_files.push(SalvageUntrackedFile {
                    path,
                    bytes: BASE64.encode(bytes),
                });
            }
            Err(_) => skipped.push(SalvageSkip {
                path,
                reason: SkipReason::Unreadable,
            }),
        }
    }
    let patch = if secret {
        String::new()
    } else {
        run("git", repo, &["diff", "--binary", "--no-color", "HEAD"])
            .or_else(|_| run("git", repo, &["diff", "--binary", "--no-color"]))?
    };
    let base = run("git", repo, &["rev-parse", "HEAD"])
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let reference = run("git", repo, &["rev-parse", "--abbrev-ref", "HEAD"])
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    Ok(finish(
        Finished {
            name: name.to_string(),
            reference,
            base,
            patch,
            untracked,
            untracked_files,
            skipped,
            truncated: secret,
        },
        limits,
        budget,
    ))
}

/// Read a regular file without following a final-component symlink, so a
/// link swapped in after the lstat is refused rather than read through.
fn read_no_follow(path: &Path) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// Paths from `git status --porcelain=v1 -z`: (tracked changes, untracked).
fn parse_porcelain(raw: &str) -> (Vec<String>, Vec<String>) {
    let mut changed = Vec::new();
    let mut untracked = Vec::new();
    let mut parts = raw.split('\0');
    while let Some(entry) = parts.next() {
        if entry.len() < 3 {
            continue;
        }
        let code = &entry[..2];
        let path = entry[3..].to_string();
        if code == "??" {
            untracked.push(path);
        } else if code.starts_with('R') || code.starts_with('C') {
            // A rename's source follows in the next NUL entry; the
            // destination is what the patch carries.
            changed.push(path);
            let _ = parts.next();
        } else {
            changed.push(path);
        }
    }
    (changed, untracked)
}

/// Whether a path can safely be written below an archive directory: relative,
/// no parent, no absolute component and no empty component.
pub fn safe_relative(path: &str) -> bool {
    if path.is_empty() || path.starts_with('/') || path.contains('\\') {
        return false;
    }
    let mut any = false;
    for component in Path::new(path).components() {
        match component {
            Component::Normal(_) => any = true,
            _ => return false,
        }
    }
    any
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::process::Command as Proc;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "vogt-salvage-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn git(dir: &Path, args: &[&str]) {
        let out = Proc::new("git")
            .current_dir(dir)
            .args(args)
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "test@example.test")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "test@example.test")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn fixture(name: &str) -> PathBuf {
        let dir = scratch(name);
        fs::write(dir.join(".gitignore"), "target/\nignored.log\n").unwrap();
        fs::write(dir.join("keep.txt"), "one\n").unwrap();
        git(&dir, &["init", "-q"]);
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-qm", "init"]);
        dir
    }

    #[test]
    fn clean_checkout_produces_no_archive() {
        let dir = fixture("clean");
        let outcome = plan(&dir, &SalvageLimits::default());
        assert!(outcome.is_empty());
        assert!(outcome.failed.is_empty());
    }

    #[test]
    fn modified_untracked_and_secret_files_are_separated() {
        let dir = fixture("mixed");
        fs::write(dir.join("keep.txt"), "two\n").unwrap();
        fs::write(dir.join("new file.txt"), "new\n").unwrap();
        fs::write(dir.join(".env"), "TOKEN=secret\n").unwrap();
        fs::write(dir.join("deploy.pem"), "key\n").unwrap();
        fs::create_dir_all(dir.join("target")).unwrap();
        fs::write(dir.join("target/build.log"), "noise\n").unwrap();
        fs::write(dir.join("ignored.log"), "noise\n").unwrap();

        let outcome = plan(&dir, &SalvageLimits::default());
        assert!(outcome.failed.is_empty(), "{:?}", outcome.failed);
        let repo = &outcome.upload.repos[0];
        assert!(repo.patch.contains("keep.txt"), "{}", repo.patch);
        let names: Vec<&str> = repo
            .untracked_files
            .iter()
            .map(|f| f.path.as_str())
            .collect();
        assert_eq!(names, vec!["new file.txt"]);
        assert!(repo.untracked.contains(&"new file.txt".to_string()));
        let skipped: Vec<(&str, SkipReason)> = repo
            .skipped
            .iter()
            .map(|s| (s.path.as_str(), s.reason))
            .collect();
        assert!(skipped.contains(&(".env", SkipReason::Secret)));
        assert!(skipped.contains(&("deploy.pem", SkipReason::Secret)));
        assert!(!repo.patch.contains("TOKEN=secret"));
        assert!(!name_any(&repo.untracked_files, ".env"));
        assert!(!name_any(&repo.untracked_files, "target/build.log"));
        assert!(!name_any(&repo.untracked_files, "ignored.log"));
    }

    fn name_any(files: &[SalvageUntrackedFile], name: &str) -> bool {
        files.iter().any(|f| f.path == name)
    }

    #[test]
    fn a_clean_checkout_below_root_is_found() {
        let dir = scratch("nested");
        let repo = dir.join("projects/one");
        fs::create_dir_all(&repo).unwrap();
        fs::write(repo.join("a.txt"), "a\n").unwrap();
        git(&repo, &["init", "-q"]);
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-qm", "init"]);
        fs::write(repo.join("b.txt"), "b\n").unwrap();
        let outcome = plan(&dir, &SalvageLimits::default());
        assert_eq!(outcome.upload.repos.len(), 1);
        assert_eq!(outcome.upload.repos[0].name, "projects/one");
    }

    #[test]
    fn the_size_cap_drops_the_patch_but_keeps_the_paths() {
        let dir = fixture("cap");
        fs::write(dir.join("keep.txt"), "two\n").unwrap();
        fs::write(dir.join("new.txt"), "new\n").unwrap();
        let limits = SalvageLimits {
            max_repo_bytes: 1,
            ..SalvageLimits::default()
        };
        let outcome = plan(&dir, &limits);
        let repo = &outcome.upload.repos[0];
        assert!(repo.truncated);
        assert!(repo.patch.is_empty());
        assert!(repo.untracked.contains(&"new.txt".to_string()));
    }

    #[test]
    fn an_untracked_file_over_the_cap_is_listed_not_copied() {
        let dir = fixture("untracked-cap");
        let mut f = fs::File::create(dir.join("big.bin")).unwrap();
        f.write_all(&vec![b'x'; 4096]).unwrap();
        let limits = SalvageLimits {
            max_untracked_file_bytes: 16,
            ..SalvageLimits::default()
        };
        let outcome = plan(&dir, &limits);
        let repo = &outcome.upload.repos[0];
        assert!(repo.untracked_files.is_empty());
        assert!(repo
            .skipped
            .iter()
            .any(|s| s.path == "big.bin" && s.reason == SkipReason::TooLarge));
    }

    #[test]
    fn a_symlink_is_never_followed_into_the_archive() {
        let dir = fixture("symlink");
        // A secret-like file outside the work tree, as the pod's
        // service-account or graft token would be.
        let outside = scratch("symlink-outside");
        fs::write(outside.join("token"), "SA-TOKEN-BYTES\n").unwrap();
        fs::create_dir_all(outside.join("secrets")).unwrap();
        fs::write(outside.join("secrets/graft.token"), "GRAFT-TOKEN\n").unwrap();
        std::os::unix::fs::symlink(outside.join("token"), dir.join("notes.txt")).unwrap();
        std::os::unix::fs::symlink(outside.join("secrets"), dir.join("linked")).unwrap();
        fs::write(dir.join("real.txt"), "real\n").unwrap();

        let outcome = plan(&dir, &SalvageLimits::default());
        let repo = &outcome.upload.repos[0];
        let names: Vec<&str> = repo
            .untracked_files
            .iter()
            .map(|f| f.path.as_str())
            .collect();
        assert_eq!(names, vec!["real.txt"]);
        for file in &repo.untracked_files {
            let bytes = BASE64.decode(&file.bytes).unwrap();
            let text = String::from_utf8_lossy(&bytes);
            assert!(!text.contains("TOKEN"), "{}: {text}", file.path);
        }
        assert!(!repo.untracked.iter().any(|p| p.starts_with("linked/")));
        assert!(repo
            .skipped
            .iter()
            .any(|s| s.path == "notes.txt" && s.reason == SkipReason::Unreadable));
    }

    #[test]
    fn untracked_files_count_toward_the_total_budget() {
        let dir = fixture("total-cap");
        fs::write(dir.join("a.bin"), vec![b'a'; 40]).unwrap();
        fs::write(dir.join("b.bin"), vec![b'b'; 40]).unwrap();
        let limits = SalvageLimits {
            max_total_bytes: 50,
            ..SalvageLimits::default()
        };
        let outcome = plan(&dir, &limits);
        let repo = &outcome.upload.repos[0];
        assert_eq!(repo.untracked_files.len(), 1, "{:?}", repo.skipped);
        assert_eq!(repo.untracked_files[0].path, "a.bin");
        assert!(repo
            .skipped
            .iter()
            .any(|s| s.path == "b.bin" && s.reason == SkipReason::TooLarge));
    }

    #[test]
    fn a_missing_root_is_not_a_failure() {
        let outcome = plan(
            Path::new("/nonexistent/vogt-salvage"),
            &SalvageLimits::default(),
        );
        assert!(outcome.is_empty());
        assert!(outcome.failed.is_empty());
    }

    #[test]
    fn porcelain_parses_renames_and_untracked() {
        let raw = " M a.txt\0?? b.txt\0R  new.txt\0old.txt\0";
        let (changed, untracked) = parse_porcelain(raw);
        assert_eq!(changed, vec!["a.txt", "new.txt"]);
        assert_eq!(untracked, vec!["b.txt"]);
    }

    #[test]
    fn safe_relative_refuses_escapes() {
        assert!(safe_relative("a/b.txt"));
        assert!(!safe_relative("../a"));
        assert!(!safe_relative("/a"));
        assert!(!safe_relative("a/../../b"));
        assert!(!safe_relative(""));
    }
}
