//! `git-local` — what the checkout itself says.
//!
//! Ports `src/vogt/collectors/git_local.py`.
//!
//! Offline by construction: it shells out to the local `git` binary and reads
//! nothing over the network. A project that is not a git repository is not an
//! error — it is a folder, which is a first-class kind of project, and the
//! collector simply reports that. git missing, unrunnable, or hung is not a
//! fact about the checkout and becomes a `CollectorError`, so the sweep is
//! recorded partial rather than the tree being reported clean.

use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

use serde_json::{json, Value};

use crate::branches::{compile_patterns, match_branch};
use crate::core::{Moment, Project};
use crate::errors::VogtError;

use super::base::{collector_error, finding, Collection, Collector, CollectorContext, Finding};

const KIND_CHECKOUT: &str = "git.checkout";
const KIND_TAG: &str = "git.tag";
const KIND_BRANCH: &str = "git.branch";

/// Long enough for a slow filesystem, short enough that a hung git does not
/// hold a sweep open. A timeout is a partial sweep, not a crash.
const GIT_TIMEOUT: Duration = Duration::from_secs(20);

pub struct GitLocalCollector;

impl Collector for GitLocalCollector {
    fn name(&self) -> &str {
        "git-local"
    }

    fn requires_network(&self) -> bool {
        false
    }

    fn collect(
        &mut self,
        ctx: &mut CollectorContext<'_>,
        project: &Project,
        _now: &mut dyn FnMut() -> Moment,
    ) -> Collection {
        // Python's `collect` is a generator: the checkout and tag findings are
        // yielded before the branch walk, and each branch before the next, so a
        // later failure keeps what came before it. Push as we go and hand back
        // the prefix with the error.
        let mut found = Vec::new();
        match collect_into(ctx, project, &mut found) {
            Ok(()) => Collection::ok(found),
            Err(error) => Collection::partial(found, error),
        }
    }
}

fn collect_into(
    ctx: &mut CollectorContext<'_>,
    project: &Project,
    found: &mut Vec<Finding>,
) -> Result<(), VogtError> {
    let root = expanduser(Path::new(&project.root_path));
    if !root.join(".git").exists() {
        found.push(finding(
            KIND_CHECKOUT,
            &format!("git:{}", project.slug),
            json!({
                "is_git_repository": false,
                "detail": "no .git directory; this project is a plain folder",
            }),
            Some(project),
            None,
            false,
        ));
        return Ok(());
    }

    let branch = git_output(&root, &["rev-parse", "--abbrev-ref", "HEAD"], false)?;
    let head = git_output(&root, &["rev-parse", "HEAD"], false)?;
    // Required: `dirty` is the one field here that is a claim about the
    // tree rather than a value read off it, so it must never be inferred
    // from a question that failed.
    let status = git_output(&root, &["status", "--porcelain"], true)?;
    let describe = git_output(&root, &["describe", "--tags", "--abbrev=0"], false)?;

    found.push(finding(
        KIND_CHECKOUT,
        &format!("git:{}", project.slug),
        json!({
            "is_git_repository": true,
            "branch": branch,
            "head": head,
            "dirty": !status.is_empty(),
            "dirty_files": if status.is_empty() { 0 } else { status.lines().count() },
            "latest_tag": optional(&describe),
        }),
        Some(project),
        None,
        false,
    ));
    if !describe.is_empty() {
        found.push(finding(
            KIND_TAG,
            &format!("release:{}@{describe}", project.slug),
            json!({"tag": describe, "source": "git describe"}),
            Some(project),
            None,
            false,
        ));
    }
    branch_findings(ctx, project, &root, found)
}

/// `""` is an absence; JSON `null` is what Python's `or None` produces.
fn optional(value: &str) -> Option<&str> {
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

/// Why git could not be run, worded as Python words it.
///
/// Python's `FileNotFoundError` for a missing `git` is
/// `[Errno 2] No such file or directory: 'git'`. Rust's `NotFound` Display is
/// `No such file or directory (os error 2)`, which names neither the errno
/// bracket nor the program. Only that spawn failure is rewritten; a timeout
/// and every other OS error keep their own text.
fn spawn_failure(exc: &std::io::Error) -> String {
    if exc.kind() == std::io::ErrorKind::NotFound {
        "[Errno 2] No such file or directory: 'git'".to_string()
    } else {
        exc.to_string()
    }
}

/// Run one git command, returning "" when git ran and had no answer.
///
/// Deliberately forgiving about answers: a repository with no commits, no
/// tags, or a detached HEAD is a normal thing to observe, and git says so by
/// exiting non-zero. Not forgiving about being unable to ask. `required`
/// extends that to a non-zero exit, for the questions whose empty answer would
/// become a claim rather than an absence.
///
/// Public, and shared with `session_outcomes`, because both collectors ask the
/// same kind of question and the rule about running git in a collector is that
/// it reads and never writes. Two copies of this function would be two places
/// for that rule to be forgotten in.
pub fn git_output(root: &Path, args: &[&str], required: bool) -> Result<String, VogtError> {
    let mut command = Command::new("git");
    command.arg("-C").arg(root).args(args);
    let output = run_bounded(&mut command, GIT_TIMEOUT).map_err(|exc| {
        collector_error(format!(
            "git could not be run against {}: {}",
            root.display(),
            spawn_failure(&exc)
        ))
    })?;
    if !output.status.success() {
        if required {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let detail = stderr.trim();
            let detail = if detail.is_empty() {
                format!("exit {}", output.status.code().unwrap_or(-1))
            } else {
                detail.to_string()
            };
            let subcommand = args.first().copied().unwrap_or("");
            return Err(collector_error(format!(
                "git {subcommand} could not be read in {}: {detail}",
                root.display()
            )));
        }
        return Ok(String::new());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// `subprocess.run(timeout=...)`. A command that overruns is killed and
/// reported as the same class of failure as an unrunnable git.
fn run_bounded(command: &mut Command, timeout: Duration) -> std::io::Result<Output> {
    let mut child = command
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    let started = std::time::Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            let stdout = read_pipe(child.stdout.take());
            let stderr = read_pipe(child.stderr.take());
            return Ok(Output {
                status,
                stdout,
                stderr,
            });
        }
        if started.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "git timed out",
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn read_pipe(pipe: Option<impl std::io::Read>) -> Vec<u8> {
    let mut bytes = Vec::new();
    if let Some(mut pipe) = pipe {
        let _ = std::io::Read::read_to_end(&mut pipe, &mut bytes);
    }
    bytes
}

/// The branch ahead/behind is measured against, best effort. `None` when it
/// cannot be established — in which case ahead/behind is reported absent
/// rather than measured against a guess.
fn default_branch(root: &Path, names: &[String]) -> Result<Option<String>, VogtError> {
    let head = git_output(root, &["rev-parse", "--abbrev-ref", "origin/HEAD"], false)?;
    if let Some(candidate) = head.strip_prefix("origin/") {
        if !candidate.is_empty() {
            return Ok(Some(candidate.to_string()));
        }
    }
    for name in ["main", "master", "trunk"] {
        if names.iter().any(|branch| branch == name) {
            return Ok(Some(name.to_string()));
        }
    }
    Ok(None)
}

/// How far `name` is ahead of and behind `default`. An unreadable answer is
/// absent, not zero.
fn ahead_behind(
    root: &Path,
    default: &str,
    name: &str,
) -> Result<(Option<i64>, Option<i64>), VogtError> {
    if name == default {
        return Ok((Some(0), Some(0)));
    }
    let counts = git_output(
        root,
        &[
            "rev-list",
            "--left-right",
            "--count",
            &format!("{default}...{name}"),
        ],
        false,
    )?;
    let parts: Vec<&str> = counts.split_whitespace().collect();
    if parts.len() != 2
        || !parts
            .iter()
            .all(|part| part.chars().all(|ch| ch.is_ascii_digit()))
    {
        return Ok((None, None));
    }
    let behind = parts[0].parse::<i64>().unwrap_or(0);
    let ahead = parts[1].parse::<i64>().unwrap_or(0);
    Ok((Some(ahead), Some(behind)))
}

/// One finding per local branch, with the work item its name binds to.
fn branch_findings(
    ctx: &CollectorContext<'_>,
    project: &Project,
    root: &Path,
    found: &mut Vec<Finding>,
) -> Result<(), VogtError> {
    let listing = git_output(
        root,
        &[
            "for-each-ref",
            "--format=%(refname:short)%09%(objectname)%09%(committerdate:iso-strict)",
            "refs/heads",
        ],
        false,
    )?;
    if listing.is_empty() {
        return Ok(());
    }
    let mut rows = Vec::new();
    for line in listing.lines() {
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() == 3 {
            rows.push((
                parts[0].to_string(),
                parts[1].to_string(),
                parts[2].to_string(),
            ));
        }
    }
    let names: Vec<String> = rows.iter().map(|(name, _, _)| name.clone()).collect();
    let default = default_branch(root, &names)?;
    let raw: Vec<&str> = ctx
        .config
        .branch_binding_patterns
        .iter()
        .map(String::as_str)
        .collect();
    let patterns = compile_patterns(&raw);

    for (name, sha, committed) in rows {
        let (ahead, behind) = match &default {
            None => (None, None),
            Some(default) => ahead_behind(root, default, &name)?,
        };
        let matched = match_branch(&name, &patterns);
        let payload = json!({
            "project": project.slug,
            "name": name,
            "tip": sha,
            "default_branch": default,
            "ahead": ahead,
            "behind": behind,
            "last_commit_at": optional(&committed),
            "work_item_ref": matched.as_ref().and_then(|matched| matched.work_ref.clone()),
            "forge_number": matched.as_ref().and_then(|matched| matched.forge_number.clone()),
        });
        found.push(finding(
            KIND_BRANCH,
            &format!("git-branch:{}:{name}", project.slug),
            payload,
            Some(project),
            None,
            false,
        ));
    }
    Ok(())
}

/// The top-level names this repository actually carries, or `None`.
///
/// `None` means the question could not be asked — `root` is not a git
/// checkout, or git is not runnable here — which is different from "it carries
/// nothing" and is what lets the contract fall back to reading the filesystem.
pub fn tracked_names(root: &Path) -> Option<std::collections::BTreeSet<String>> {
    if !root.join(".git").exists() {
        return None;
    }
    let listing = git_output(root, &["ls-files", "-z"], true).ok()?;
    let mut names = std::collections::BTreeSet::new();
    for entry in listing.split('\0') {
        if entry.is_empty() {
            continue;
        }
        let first = entry.split('/').next().unwrap_or(entry);
        names.insert(first.to_string());
    }
    Some(names)
}

/// Python's `Path.expanduser`: only a leading `~` on its own expands.
fn expanduser(path: &Path) -> PathBuf {
    let mut components = path.components();
    match components.next() {
        Some(Component::Normal(first)) if first == "~" => std::env::var_os("HOME")
            .map(|home| PathBuf::from(home).join(components.as_path()))
            .unwrap_or_else(|| path.to_path_buf()),
        _ => path.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::*;
    use crate::config::VogtConfig;
    use serde_json::Map;

    fn project(root: &Path) -> Project {
        Project::new(
            "prj_fixture",
            "fixture",
            "fixture",
            &root.display().to_string(),
            Moment::from_unix(0, 0),
        )
    }

    fn collect(root: &Path) -> Result<Vec<Finding>, VogtError> {
        let config = VogtConfig::default();
        let mut context = CollectorContext {
            config: &config,
            options: Map::new(),
        };
        let mut now = || Moment::from_unix(0, 0);
        let collected = GitLocalCollector.collect(&mut context, &project(root), &mut now);
        match collected.error {
            Some(error) => Err(error),
            None => Ok(collected.findings),
        }
    }

    fn temp(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("vogt-git-{}-{label}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn git(root: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }

    #[test]
    fn a_plain_folder_is_reported_as_one() {
        let root = temp("plain");
        let found = collect(&root).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].payload["is_git_repository"], Value::Bool(false));
        assert_eq!(found[0].subject_key, "git:fixture");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_real_repository_reports_branch_and_tag() {
        let root = temp("repo");
        git(&root, &["init", "-q", "-b", "main"]);
        git(&root, &["config", "user.email", "t@example.com"]);
        git(&root, &["config", "user.name", "Test"]);
        std::fs::write(root.join("a.txt"), "hello\n").unwrap();
        git(&root, &["add", "a.txt"]);
        git(&root, &["commit", "-qm", "first"]);
        git(&root, &["tag", "v1.2.3"]);

        let found = collect(&root).unwrap();
        let checkout = found
            .iter()
            .find(|finding| finding.kind == "git.checkout")
            .unwrap();
        assert_eq!(checkout.payload["is_git_repository"], Value::Bool(true));
        assert_eq!(checkout.payload["branch"], "main");
        assert_eq!(checkout.payload["latest_tag"], "v1.2.3");
        assert_eq!(checkout.payload["dirty"], Value::Bool(false));
        assert!(found.iter().any(|finding| finding.kind == "git.tag"));
        let branch = found
            .iter()
            .find(|finding| finding.kind == "git.branch")
            .unwrap();
        assert_eq!(branch.payload["name"], "main");
        assert_eq!(branch.payload["default_branch"], "main");
        assert_eq!(branch.payload["ahead"], 0);
        assert_eq!(branch.payload["behind"], 0);
        assert_eq!(branch.subject_key, "git-branch:fixture:main");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_status_that_cannot_be_read_is_never_reported_as_clean() {
        let root = temp("badgit");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        let error = collect(&root).unwrap_err();
        let VogtError::CollectorFailed(message) = error else {
            panic!("an unreadable checkout is a collector failure, not a clean tree");
        };
        assert!(message.starts_with("CollectorError:"), "{message}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_missing_git_binary_names_errno_2_the_way_python_does() {
        // Python: FileNotFoundError -> "[Errno 2] No such file or directory: 'git'".
        // Tested on the mapping, not by emptying PATH: tests run on parallel
        // threads, and a process-wide PATH change would break every other
        // test that runs git meanwhile.
        let missing = std::io::Error::from(std::io::ErrorKind::NotFound);
        assert_eq!(
            spawn_failure(&missing),
            "[Errno 2] No such file or directory: 'git'"
        );
        let other = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        assert_eq!(spawn_failure(&other), other.to_string());
    }

    #[test]
    fn tracked_names_reads_the_first_segment_only() {
        let root = temp("tracked");
        git(&root, &["init", "-q", "-b", "main"]);
        git(&root, &["config", "user.email", "t@example.com"]);
        git(&root, &["config", "user.name", "Test"]);
        std::fs::create_dir_all(root.join("docs")).unwrap();
        std::fs::write(root.join("docs/guide.md"), "x\n").unwrap();
        std::fs::write(root.join("AGENTS.md"), "x\n").unwrap();
        git(&root, &["add", "."]);
        git(&root, &["commit", "-qm", "first"]);
        let names = tracked_names(&root).unwrap();
        assert_eq!(names, ["AGENTS.md".to_string(), "docs".to_string()].into());
        assert!(tracked_names(&temp("notgit")).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }
}
