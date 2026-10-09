//! Cloning and pushing a repository, without ever writing the token down.
//!
//! Ports `src/vogt/adapters/git/clone.py`. The credential travels by
//! `GIT_ASKPASS`: a short-lived helper script that prints the token it reads
//! from its own environment. The token reaches the `git` child through its
//! environment and appears in no command line, no configuration file and no
//! stored URL.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::errors::VogtError;

/// Long enough for a large repository on a slow link, bounded so an import
/// cannot hold a request open indefinitely.
pub const CLONE_TIMEOUT: Duration = Duration::from_secs(600);
pub const GIT_TIMEOUT: Duration = Duration::from_secs(20);

/// The username half of a GitHub token credential. GitHub ignores it — the
/// token is the password — but git insists on asking for both.
pub const TOKEN_USERNAME: &str = "x-access-token";

const ASKPASS_SCRIPT: &str = "\
#!/bin/sh
# Written by Vogt for one clone and deleted immediately afterwards.
# Prints the credential git asks for, reading it from this process's own
# environment so that it never reaches a command line.
case \"$1\" in
    Username*) printf '%s\\n' \"$VOGT_GIT_USERNAME\" ;;
    *) printf '%s\\n' \"$VOGT_GIT_TOKEN\" ;;
esac
";

/// What git says when a plain push is refused because the remote is ahead.
const NON_FAST_FORWARD_MARKS: [&str; 3] = ["non-fast-forward", "fetch first", "[rejected]"];

/// One repository to put on disk.
pub struct CloneRequest {
    pub remote: String,
    pub destination: PathBuf,
    pub token: Option<String>,
    /// Where the `GIT_ASKPASS` helper may live. Must be executable at runtime.
    pub helper_dir: Option<PathBuf>,
}

/// What ended up on disk, and whether this call is what put it there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloneOutcome {
    pub destination: PathBuf,
    pub revision: Option<String>,
    pub default_branch: Option<String>,
    /// True when the destination was already a clone of the same remote.
    pub reused: bool,
}

/// What a publishable checkout looks like: one branch, one commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishSource {
    pub root: PathBuf,
    pub branch: String,
    pub revision: String,
}

/// One branch to push to one remote, authenticated per-invocation.
pub struct PushRequest {
    pub root: PathBuf,
    pub remote: String,
    pub branch: String,
    pub token: Option<String>,
    pub helper_dir: Option<PathBuf>,
}

/// What was pushed, for the operation's receipt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushOutcome {
    pub remote: String,
    pub branch: String,
    pub revision: Option<String>,
}

/// Clone `request.remote` to `request.destination`.
pub fn clone_repository(request: &CloneRequest) -> Result<CloneOutcome, VogtError> {
    let destination = expand_user(&request.destination);
    // Before anything reads a checkout. Asking git what the origin is with no
    // git comes back empty and is indistinguishable from a checkout that
    // genuinely has no origin (#21).
    if which_git().is_none() {
        return Err(VogtError::GitUnavailable(
            "git is not installed, so a repository cannot be imported".to_string(),
        ));
    }
    if let Some(existing) = reuse_existing(&destination, &request.remote)? {
        enforce_import_parity(
            &destination,
            request.token.as_deref(),
            request.helper_dir.as_deref(),
        )?;
        return Ok(existing);
    }
    fs::create_dir_all(destination.parent().unwrap_or(Path::new("."))).map_err(|error| {
        VogtError::GitUnavailable(format!(
            "could not create {}: {error}",
            destination.display()
        ))
    })?;
    let env = askpass(request.token.as_deref(), request.helper_dir.as_deref())?;
    run_git(
        &[
            "clone",
            "--origin",
            "origin",
            &request.remote,
            &destination.to_string_lossy(),
        ],
        destination.parent().unwrap_or(Path::new(".")),
        Some(&env),
        CLONE_TIMEOUT,
    )?;
    drop_askpass(&env);
    Ok(CloneOutcome {
        destination: destination.clone(),
        revision: read(&destination, &["rev-parse", "HEAD"])?,
        default_branch: read(&destination, &["rev-parse", "--abbrev-ref", "HEAD"])?,
        reused: false,
    })
}

/// The read-only gate before `forge.publish` creates anything.
pub fn inspect_publish_source(root: &Path) -> Result<PublishSource, VogtError> {
    let resolved = expand_user(root);
    if which_git().is_none() {
        return Err(VogtError::GitUnavailable(
            "git is not installed, so a repository cannot be published".to_string(),
        ));
    }
    if !resolved.join(".git").exists() {
        return Err(VogtError::PublishSourceInvalid(format!(
            "{resolved} is not a git repository; `forge.publish` pushes the project's local history, so there must be one — run `git init` and commit, then retry",
            resolved = resolved.display()
        )));
    }
    let dirty = working_tree_changes(&resolved)?;
    if !dirty.is_empty() {
        return Err(VogtError::PublishWorkingTreeDirty(format!(
            "the working tree at {root} has uncommitted changes ({count} path(s), e.g. {example:?}); Vogt publishes only state you have settled — commit, stash or discard them yourself and retry",
            root = resolved.display(),
            count = dirty.len(),
            example = dirty[0],
        )));
    }
    let branch = read(&resolved, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    let revision = read(&resolved, &["rev-parse", "HEAD"])?;
    let Some(revision) = revision else {
        return Err(VogtError::PublishSourceInvalid(format!(
            "{root} has no commits, so there is nothing to push; commit first, then retry",
            root = resolved.display()
        )));
    };
    match branch.as_deref() {
        None | Some("HEAD") => Err(VogtError::PublishSourceInvalid(format!(
            "{root} is on a detached HEAD, so there is no branch to publish as the remote's default; check out a branch and retry",
            root = resolved.display()
        ))),
        Some(branch) => Ok(PublishSource { root: resolved, branch: branch.to_string(), revision }),
    }
}

/// Push one branch, plainly — never `--force`, on any path.
pub fn push_branch(request: &PushRequest) -> Result<PushOutcome, VogtError> {
    let refspec = format!("{}:refs/heads/{}", request.branch, request.branch);
    let env = askpass(request.token.as_deref(), request.helper_dir.as_deref())?;
    let pushed = run_git(
        &["push", &request.remote, &refspec],
        &request.root,
        Some(&env),
        CLONE_TIMEOUT,
    );
    drop_askpass(&env);
    if let Err(error) = pushed {
        if let VogtError::GitCommandFailed(detail) = &error {
            let lower = detail.to_lowercase();
            if NON_FAST_FORWARD_MARKS
                .iter()
                .any(|mark| lower.contains(mark))
            {
                return Err(VogtError::PublishNonFastForward(format!(
                    "the remote refused branch {:?} as non-fast-forward, and Vogt never forces a push; the remote's history stands — reconcile it yourself if it is really yours to move",
                    request.branch
                )));
            }
        }
        return Err(error);
    }
    Ok(PushOutcome {
        remote: request.remote.clone(),
        branch: request.branch.clone(),
        revision: read(&request.root, &["rev-parse", "HEAD"])?,
    })
}

/// A clone of the same remote is the repeat-import case. Anything else fails
/// without being touched.
fn reuse_existing(destination: &Path, remote: &str) -> Result<Option<CloneOutcome>, VogtError> {
    if !destination.exists() {
        return Ok(None);
    }
    if !destination.join(".git").exists() {
        let occupied = fs::read_dir(destination)
            .map(|entries| entries.flatten().next().is_some())
            .unwrap_or(false);
        if occupied {
            return Err(VogtError::Conflict(format!(
                "{destination} already exists and is not a git repository; import will not write into it",
                destination = destination.display()
            )));
        }
        return Ok(None);
    }
    let origin = read(destination, &["remote", "get-url", "origin"])?;
    if origin
        .as_deref()
        .is_none_or(|origin| !same_remote(origin, remote))
    {
        return Err(VogtError::Conflict(format!(
            "{destination} is a clone of {origin}, not of {remote}",
            destination = destination.display(),
            origin = origin.as_deref().unwrap_or("an unknown remote"),
        )));
    }
    Ok(Some(CloneOutcome {
        destination: destination.to_path_buf(),
        revision: read(destination, &["rev-parse", "HEAD"])?,
        default_branch: read(destination, &["rev-parse", "--abbrev-ref", "HEAD"])?,
        reused: true,
    }))
}

/// Refuse a re-import onto a checkout that is not clean and at parity. Reads
/// only: no fetch, no merge, no rebase, no stash.
fn enforce_import_parity(
    destination: &Path,
    token: Option<&str>,
    helper_dir: Option<&Path>,
) -> Result<(), VogtError> {
    let branch = default_branch(destination)?;
    let dirty = working_tree_changes(destination)?;
    if !dirty.is_empty() {
        return Err(VogtError::ImportWorkingTreeDirty(format!(
            "the working tree at {root} has uncommitted changes ({count} path(s), e.g. {example:?}); Vogt does not touch your working tree, so commit, stash or discard them yourself and retry the import",
            root = destination.display(),
            count = dirty.len(),
            example = dirty[0],
        )));
    }
    let local = read(destination, &["rev-parse", &branch])?;
    let origin = origin_head(destination, &branch, token, helper_dir);
    if local != origin {
        return Err(VogtError::ImportBranchDiverged(format!(
            "local HEAD {local} on branch {branch:?} has diverged from origin HEAD {origin}; Vogt performs no merge, rebase or stash, so push or pull the branch yourself and retry the import",
            local = local.as_deref().unwrap_or("unknown"),
            origin = origin.as_deref().unwrap_or("unknown"),
        )));
    }
    Ok(())
}

/// The branch the gate inspects: the remote's default, as the clone tracks it.
fn default_branch(destination: &Path) -> Result<String, VogtError> {
    if let Some(tracked) = read(
        destination,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    )? {
        if !tracked.is_empty() {
            return Ok(tracked.trim_start_matches("origin/").to_string());
        }
    }
    Ok(read(destination, &["rev-parse", "--abbrev-ref", "HEAD"])?
        .unwrap_or_else(|| "HEAD".to_string()))
}

/// The porcelain lines for a dirty tree, empty when it is clean.
fn working_tree_changes(destination: &Path) -> Result<Vec<String>, VogtError> {
    Ok(read(destination, &["status", "--porcelain"])?
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(str::to_string)
        .collect())
}

/// The commit origin's default branch points at, read live (no fetch).
fn origin_head(
    destination: &Path,
    branch: &str,
    token: Option<&str>,
    helper_dir: Option<&Path>,
) -> Option<String> {
    let refspec = format!("refs/heads/{branch}");
    let env = askpass(token, helper_dir).ok()?;
    let out = run_git(
        &["ls-remote", "origin", &refspec],
        destination,
        Some(&env),
        GIT_TIMEOUT,
    )
    .ok();
    drop_askpass(&env);
    out.filter(|text| !text.is_empty())
        .and_then(|text| text.split_whitespace().next().map(str::to_string))
}

/// Compare two remotes ignoring the ways git spells the same one.
fn same_remote(left: &str, right: &str) -> bool {
    normalise(left) == normalise(right)
}

fn normalise(remote: &str) -> String {
    let mut candidate = remote.trim().trim_start_matches("git+").to_string();
    for prefix in ["https://", "http://", "ssh://"] {
        if let Some(rest) = candidate.strip_prefix(prefix) {
            candidate = rest.to_string();
        }
    }
    candidate = candidate.replace("git@github.com:", "github.com/");
    // A credential embedded in a URL we are comparing against is somebody
    // else's doing; strip it rather than fail to match.
    let tail = candidate
        .rsplit_once('@')
        .map_or(candidate.as_str(), |(_, tail)| tail);
    tail.trim_end_matches(".git")
        .trim_matches('/')
        .to_lowercase()
}

/// The environment one git invocation runs under, with the askpass helper in
/// place when there is a token. `VOGT_ASKPASS_DIR` names the directory the
/// caller must remove afterwards.
fn askpass(
    token: Option<&str>,
    helper_dir: Option<&Path>,
) -> Result<std::collections::HashMap<String, String>, VogtError> {
    let mut env: std::collections::HashMap<String, String> = std::env::vars().collect();
    // Never let git stop for input: in a server process an interactive prompt
    // is an indefinite hang.
    env.insert("GIT_TERMINAL_PROMPT".to_string(), "0".to_string());
    let Some(token) = token else {
        env.remove("GIT_ASKPASS");
        return Ok(env);
    };
    // Unique per invocation, never per process and never a predictable path:
    // concurrent clones must not share a dir (the first cleanup would delete
    // the other's helper), and a fixed /tmp path lets another local user
    // pre-create it or swap askpass.sh so it runs with VOGT_GIT_TOKEN set.
    let _ = helper_dir;
    let mut random = [0u8; 8];
    File::open("/dev/urandom")
        .and_then(|mut source| source.read_exact(&mut random))
        .map_err(|error| VogtError::GitUnavailable(format!("no randomness for askpass: {error}")))?;
    let dir = std::env::temp_dir().join(format!(
        "vogt-askpass-{}",
        random.iter().map(|b| format!("{b:02x}")).collect::<String>()
    ));
    fs::create_dir(&dir).map_err(|error| {
        VogtError::GitUnavailable(format!("could not create the askpass helper: {error}"))
    })?;
    set_mode(&dir, 0o700);
    let script = dir.join("askpass.sh");
    fs::File::create(&script)
        .and_then(|mut file| file.write_all(ASKPASS_SCRIPT.as_bytes()))
        .map_err(|error| {
            VogtError::GitUnavailable(format!("could not write the askpass helper: {error}"))
        })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).ok();
    }
    env.insert(
        "GIT_ASKPASS".to_string(),
        script.to_string_lossy().to_string(),
    );
    env.insert("VOGT_GIT_USERNAME".to_string(), TOKEN_USERNAME.to_string());
    env.insert("VOGT_GIT_TOKEN".to_string(), token.to_string());
    env.insert(
        "VOGT_ASKPASS_DIR".to_string(),
        dir.to_string_lossy().to_string(),
    );
    Ok(env)
}

fn drop_askpass(env: &std::collections::HashMap<String, String>) {
    if let Some(dir) = env.get("VOGT_ASKPASS_DIR") {
        let _ = fs::remove_dir_all(dir);
    }
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).ok();
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) {}

fn run_git(
    args: &[&str],
    cwd: &Path,
    env: Option<&std::collections::HashMap<String, String>>,
    timeout: Duration,
) -> Result<String, VogtError> {
    let mut command = Command::new("git");
    command
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(env) = env {
        command.env_clear().envs(env);
    }
    let mut child = command.spawn().map_err(|error| {
        VogtError::GitUnavailable(format!(
            "git {} failed: {error}",
            args.first().copied().unwrap_or("")
        ))
    })?;
    // Drain both pipes on their own threads while the wait is polled. Polling
    // try_wait on unread pipes deadlocks once git writes more than a pipe
    // buffer — a `status` over a few thousand untracked files.
    let stdout = child.stdout.take().expect("stdout piped");
    let stderr = child.stderr.take().expect("stderr piped");
    let out_thread = std::thread::spawn(move || read_capped(stdout));
    let err_thread = std::thread::spawn(move || read_capped(stderr));
    let started = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() > timeout => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = out_thread.join();
                let _ = err_thread.join();
                return Err(VogtError::GitUnavailable(format!(
                    "git {} timed out after {}s",
                    args.first().copied().unwrap_or(""),
                    timeout.as_secs()
                )));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(error) => {
                let _ = out_thread.join();
                let _ = err_thread.join();
                return Err(VogtError::GitUnavailable(format!(
                    "git {} failed: {error}",
                    args.first().copied().unwrap_or("")
                )));
            }
        }
    };
    let out = out_thread.join().unwrap_or_default();
    let err = err_thread.join().unwrap_or_default();
    if !status.success() {
        return Err(VogtError::GitCommandFailed(format!(
            "git {} failed: {}",
            args.first().copied().unwrap_or(""),
            redact(String::from_utf8_lossy(&err).trim())
        )));
    }
    Ok(String::from_utf8_lossy(&out).trim().to_string())
}

/// Read a child's pipe, keeping the first 8 MiB so a runaway command cannot
/// grow memory without bound.
fn read_capped(mut pipe: impl Read) -> Vec<u8> {
    const CAP: usize = 8 * 1024 * 1024;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    while buf.len() < CAP {
        match pipe.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => buf.extend_from_slice(&chunk[..read.min(CAP - buf.len())]),
            Err(_) => break,
        }
    }
    buf
}

/// Keep git's diagnostics useful without repeating a credential.
fn redact(message: &str) -> String {
    message
        .split_whitespace()
        .map(|word| {
            if word.contains('@') && word.contains("://") {
                let (scheme, rest) = word.split_once("://").unwrap_or((word, ""));
                format!(
                    "{scheme}://{}",
                    rest.rsplit_once('@').map_or(rest, |(_, tail)| tail)
                )
            } else {
                word.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The answer, or nothing when git ran and said no. A missing or hung git is
/// raised, because that is not a fact about the checkout.
fn read(destination: &Path, args: &[&str]) -> Result<Option<String>, VogtError> {
    match run_git(args, destination, None, GIT_TIMEOUT) {
        Ok(text) if !text.is_empty() => Ok(Some(text)),
        Ok(_) => Ok(None),
        Err(VogtError::GitCommandFailed(_)) => Ok(None),
        Err(error) => Err(error),
    }
}

fn which_git() -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path).find_map(|dir| {
            let candidate = dir.join("git");
            candidate.is_file().then_some(candidate)
        })
    })
}

fn expand_user(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    path.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remotes_match_across_the_ways_git_spells_them() {
        assert!(same_remote(
            "https://github.com/org/repo.git",
            "git@github.com:org/repo"
        ));
        assert!(same_remote(
            "https://x-access-token:sekret@github.com/org/repo",
            "https://github.com/org/repo.git"
        ));
        assert!(!same_remote(
            "https://github.com/org/repo",
            "https://github.com/org/other"
        ));
    }

    #[test]
    fn redact_strips_a_credential_from_a_remote_url() {
        let message = redact("fatal: could not read https://user:sekret@github.com/org/repo");
        assert!(!message.contains("sekret"));
        assert!(message.contains("github.com/org/repo"));
    }

    #[test]
    fn a_non_fast_forward_refusal_names_the_marks() {
        for mark in ["non-fast-forward", "fetch first", "[rejected]"] {
            let detail = format!("git push failed: {mark}");
            assert!(NON_FAST_FORWARD_MARKS
                .iter()
                .any(|known| detail.contains(known)));
        }
    }

    #[test]
    fn an_occupied_non_repo_is_a_conflict_and_untouched() {
        let dir = std::env::temp_dir().join(format!("vogt-clone-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("notes.txt"), "mine").unwrap();
        let request = CloneRequest {
            remote: "https://example.test/repo.git".to_string(),
            destination: dir.clone(),
            token: None,
            helper_dir: None,
        };
        let error = clone_repository(&request).unwrap_err();
        assert!(matches!(error, VogtError::Conflict(_)), "{error:?}");
        assert_eq!(fs::read_to_string(dir.join("notes.txt")).unwrap(), "mine");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_empty_directory_is_accepted_for_a_clone() {
        let dir = std::env::temp_dir().join(format!("vogt-clone-empty-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let request = CloneRequest {
            remote: "https://example.invalid/no-such-repo.git".to_string(),
            destination: dir.clone(),
            token: None,
            helper_dir: None,
        };
        // The directory is accepted: the failure, if git is present, is the
        // clone itself rather than "already exists".
        if which_git().is_some() {
            let error = clone_repository(&request).unwrap_err();
            assert!(
                matches!(error, VogtError::GitCommandFailed(_)),
                "an empty dir must not be a conflict: {error:?}"
            );
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_askpass_helper_carries_the_credential_by_environment() {
        let dir = std::env::temp_dir().join(format!("vogt-askpass-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let env = askpass(Some("sekret"), Some(&dir)).unwrap();
        let script = PathBuf::from(env.get("GIT_ASKPASS").unwrap());
        assert!(script.is_file(), "the helper must exist");
        assert_eq!(
            env.get("VOGT_GIT_TOKEN").map(String::as_str),
            Some("sekret")
        );
        assert_eq!(
            env.get("VOGT_GIT_USERNAME").map(String::as_str),
            Some(TOKEN_USERNAME)
        );
        assert_eq!(
            env.get("GIT_TERMINAL_PROMPT").map(String::as_str),
            Some("0")
        );
        let text = fs::read_to_string(&script).unwrap();
        assert!(text.contains("VOGT_GIT_TOKEN"));
        assert!(
            !text.contains("sekret"),
            "the token must not be written into the script"
        );
        drop_askpass(&env);
        assert!(!script.exists(), "the helper is removed after the call");
        let _ = fs::remove_dir_all(&dir);
    }
}
