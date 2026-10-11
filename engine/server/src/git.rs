use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use axum::{
    extract::{Query, State},
    Json,
};
use serde::{Deserialize, Serialize};
use tokio::process::Command;
use vogt_engine_contract::{BranchInfo, DiffResp, GitStatus, LogEntry, StatusEntry, StatusKind};

use crate::{
    app::AppState,
    error::{ApiError, Result},
    workspace_path,
};

/// Walk upwards from `start` (inclusive) until we find a `.git` entry.
/// Stops at `boundary` (exclusive) so we can't walk past the workspace root.
async fn find_repo_root(start: &Path, boundary: &Path) -> Result<PathBuf> {
    let mut cur = start.to_path_buf();
    loop {
        if tokio::fs::try_exists(cur.join(".git"))
            .await
            .unwrap_or(false)
            && is_repo_root(&cur).await
        {
            return Ok(cur);
        }
        if cur == boundary {
            return Err(ApiError::NotFound);
        }
        match cur.parent() {
            Some(p) if p.starts_with(boundary) || p == boundary => cur = p.to_path_buf(),
            _ => return Err(ApiError::NotFound),
        }
    }
}

/// A `.git` path is only a marker, not proof that Git considers the directory
/// a repository. Workspace roots can contain an empty placeholder directory,
/// and treating one as a repository turns an ordinary empty selection into a
/// later `git status` 500.
async fn is_repo_root(path: &Path) -> bool {
    let Ok(output) = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(path)
        .output()
        .await
    else {
        return false;
    };
    if !output.status.success() {
        return false;
    }

    let reported = Path::new(std::str::from_utf8(&output.stdout).unwrap_or("").trim());
    reported == path
}

/// Resolve a repo from query: ?repo= relative to workspace_root, else
/// workspace_root itself. Returns `None` when the path exists but no Git repo is
/// present at or above it inside the workspace boundary.
async fn resolve_repo(state: &Arc<AppState>, repo: &str) -> Result<Option<PathBuf>> {
    let root = &state.config.workspace_root;
    let candidate = if repo.trim().is_empty() {
        root.clone()
    } else {
        workspace_path::resolve_existing(root, repo)?
    };
    match find_repo_root(&candidate, root).await {
        Ok(repo) => Ok(Some(repo)),
        Err(ApiError::NotFound) => Ok(None),
        Err(e) => Err(e),
    }
}

async fn run_git(repo: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .await
        .map_err(|e| ApiError::Internal(format!("spawn git: {e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        return Err(ApiError::Internal(format!(
            "git {args:?}: {} (status {:?})",
            stderr.trim(),
            output.status.code()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn clean_repo_rel(path: &str) -> Result<String> {
    let path = path.trim();
    if path.is_empty() {
        return Err(ApiError::BadRequest("path must not be empty".into()));
    }
    for comp in Path::new(path).components() {
        match comp {
            std::path::Component::Normal(_) | std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                return Err(ApiError::BadRequest(
                    "path contains '..' (parent component)".into(),
                ));
            }
            std::path::Component::RootDir | std::path::Component::Prefix(_) => {
                return Err(ApiError::BadRequest("path must be relative".into()));
            }
        }
    }
    Ok(path.to_string())
}

fn repo_rel(root: &Path, repo: &Path) -> String {
    repo.strip_prefix(root)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| repo.to_string_lossy().into_owned())
}

fn requested_repo_label(repo: &str) -> String {
    repo.trim().trim_start_matches('/').to_string()
}

#[derive(Debug, Deserialize)]
pub struct RepoQuery {
    #[serde(default)]
    pub repo: String,
}

pub async fn status(
    State(state): State<Arc<AppState>>,
    Query(q): Query<RepoQuery>,
) -> Result<Json<GitStatus>> {
    let Some(repo) = resolve_repo(&state, &q.repo).await? else {
        return Ok(Json(GitStatus {
            repo: requested_repo_label(&q.repo),
            is_repo: false,
            branch: String::new(),
            ahead: 0,
            behind: 0,
            entries: Vec::new(),
        }));
    };
    let raw = run_git(
        &repo,
        &[
            "status",
            "--porcelain=v1",
            "--branch",
            "--untracked-files=all",
        ],
    )
    .await?;

    let mut branch = String::new();
    let mut ahead = 0u32;
    let mut behind = 0u32;
    let mut entries = Vec::new();

    for line in raw.lines() {
        if let Some(rest) = line.strip_prefix("## ") {
            // e.g. "main...origin/main [ahead 1, behind 2]" or "main"
            let head_part = rest.split_whitespace().next().unwrap_or(rest);
            branch = head_part
                .split("...")
                .next()
                .unwrap_or(head_part)
                .to_string();
            if let Some(bracket) = rest.find('[') {
                let inside = &rest[bracket + 1..rest.rfind(']').unwrap_or(rest.len())];
                for part in inside.split(',') {
                    let part = part.trim();
                    if let Some(n) = part.strip_prefix("ahead ") {
                        ahead = n.parse().unwrap_or(0);
                    } else if let Some(n) = part.strip_prefix("behind ") {
                        behind = n.parse().unwrap_or(0);
                    }
                }
            }
            continue;
        }
        if line.len() < 3 {
            continue;
        }
        let index = &line[0..1];
        let worktree = &line[1..2];
        let path = line[3..].to_string();
        let kind = classify_status(index, worktree);
        entries.push(StatusEntry {
            path,
            index: index.to_string(),
            worktree: worktree.to_string(),
            kind,
        });
    }

    Ok(Json(GitStatus {
        repo: repo_rel(&state.config.workspace_root, &repo),
        is_repo: true,
        branch,
        ahead,
        behind,
        entries,
    }))
}

fn classify_status(index: &str, worktree: &str) -> StatusKind {
    match (index, worktree) {
        ("?", "?") => StatusKind::Untracked,
        ("U", _) | (_, "U") | ("D", "D") | ("A", "A") => StatusKind::Conflicted,
        ("R", _) => StatusKind::Renamed,
        ("D", _) | (_, "D") => StatusKind::Deleted,
        (i, _) if i != " " && i != "?" => StatusKind::Staged,
        _ => StatusKind::Modified,
    }
}

#[derive(Debug, Deserialize)]
pub struct DiffQuery {
    #[serde(default)]
    pub repo: String,
    pub path: String,
    #[serde(default)]
    pub staged: bool,
}

pub async fn diff(
    State(state): State<Arc<AppState>>,
    Query(q): Query<DiffQuery>,
) -> Result<Json<DiffResp>> {
    let repo = resolve_repo(&state, &q.repo)
        .await?
        .ok_or(ApiError::NotFound)?;
    let path = clean_repo_rel(&q.path)?;
    diff_may_show(&state.config.workspace_root, &repo, &path).await?;

    // HEAD content via `git show HEAD:path`. Returns empty if untracked.
    let head_arg = format!("HEAD:{path}");
    let head: String = run_git(&repo, &["show", &head_arg])
        .await
        .unwrap_or_default();

    let current = if q.staged {
        // Staged content — `git show :path`
        let staged_arg = format!(":{path}");
        run_git(&repo, &["show", &staged_arg])
            .await
            .unwrap_or_default()
    } else {
        // Resolve through symlinks and refuse anything escaping the repo
        // A committed `evil-link -> /etc/passwd` would otherwise be
        // read straight into the diff's `current` for any valid token (this
        // read, unlike every files.rs read, had no root check). A path that
        // does not resolve — untracked or deleted — reads as empty, unchanged.
        let full = repo.join(&path);
        match (full.canonicalize(), repo.canonicalize()) {
            (Ok(canon), Ok(repo_canon)) if canon.starts_with(&repo_canon) => {
                // The working-tree side is held to the policy on the resolved
                // path too: a tracked `notes.txt -> .env` link must not read
                // the credential it points at.
                let lexical = repo_canon.join(&path);
                let root = &state.config.workspace_root;
                if canon != lexical && workspace_path::may_show(root, &canon).is_err() {
                    return Err(ApiError::BadRequest(
                        "refusing to diff a link to a hidden or credential file".into(),
                    ));
                }
                tokio::fs::read_to_string(&canon).await.unwrap_or_default()
            }
            _ => String::new(),
        }
    };

    Ok(Json(DiffResp {
        path,
        current,
        head,
    }))
}

/// The file API's content policy, applied to a diff. Credential names and
/// anything under `.git/` are refused outright — on both the HEAD and the
/// working-tree side, since a committed secret is as secret as an uncommitted
/// one. A hidden component inside the repo (`.github/workflows/ci.yml`,
/// `.gitignore`) is allowed only for a file git tracks: the Git panel lists
/// those, and their content is already in HEAD. Untracked dotfiles
/// (`.envrc`, `.claude/settings.local.json`) and a repo that itself sits
/// under a hidden directory are refused like the viewer refuses them.
async fn diff_may_show(root: &Path, repo: &Path, path: &str) -> Result<()> {
    let lexical = repo.join(path);
    if workspace_path::names_a_secret(root, &lexical) {
        return Err(ApiError::BadRequest(
            "refusing to diff a credential file".into(),
        ));
    }
    if workspace_path::has_hidden_component(root, repo) {
        return Err(ApiError::BadRequest(
            "refusing to diff a file in a hidden directory".into(),
        ));
    }
    if workspace_path::has_hidden_component(repo, &lexical) {
        let tracked = run_git(repo, &["ls-files", "--error-unmatch", "--", path])
            .await
            .is_ok();
        if !tracked {
            return Err(ApiError::BadRequest(
                "refusing to diff an untracked hidden file".into(),
            ));
        }
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
pub struct LogQuery {
    #[serde(default)]
    pub repo: String,
    pub n: Option<u32>,
}

pub async fn log(
    State(state): State<Arc<AppState>>,
    Query(q): Query<LogQuery>,
) -> Result<Json<Vec<LogEntry>>> {
    let Some(repo) = resolve_repo(&state, &q.repo).await? else {
        return Ok(Json(Vec::new()));
    };
    let n = q.n.unwrap_or(50).min(500);
    // Use a record separator that doesn't appear in normal commit messages.
    let fmt = "%H%x01%h%x01%an%x01%ad%x01%s";
    let raw = run_git(
        &repo,
        &[
            "log",
            "--date=iso-strict",
            &format!("--pretty=format:{fmt}"),
            &format!("-n{n}"),
        ],
    )
    .await?;

    let mut out = Vec::new();
    for line in raw.lines() {
        let parts: Vec<&str> = line.splitn(5, '\u{1}').collect();
        if parts.len() != 5 {
            continue;
        }
        out.push(LogEntry {
            hash: parts[0].to_string(),
            short: parts[1].to_string(),
            author: parts[2].to_string(),
            date: parts[3].to_string(),
            subject: parts[4].to_string(),
        });
    }
    Ok(Json(out))
}

pub async fn branch(
    State(state): State<Arc<AppState>>,
    Query(q): Query<RepoQuery>,
) -> Result<Json<BranchInfo>> {
    let Some(repo) = resolve_repo(&state, &q.repo).await? else {
        return Ok(Json(BranchInfo {
            current: String::new(),
            all: Vec::new(),
        }));
    };
    let current = run_git(&repo, &["branch", "--show-current"])
        .await?
        .trim()
        .to_string();
    let all_raw = run_git(
        &repo,
        &["for-each-ref", "--format=%(refname:short)", "refs/heads/"],
    )
    .await?;
    let all: Vec<String> = all_raw
        .lines()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    Ok(Json(BranchInfo { current, all }))
}

#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum GitOpReq {
    Stage {
        #[serde(default)]
        repo: String,
        path: String,
    },
    Unstage {
        #[serde(default)]
        repo: String,
        path: String,
    },
    Discard {
        #[serde(default)]
        repo: String,
        path: String,
    },
    Commit {
        #[serde(default)]
        repo: String,
        message: String,
        /// When set, commit on behalf of this session (C14): the engine runs
        /// the oversight checklist and commits in the repo that session's
        /// working directory belongs to. Absent, this is the GUI's plain
        /// commit and nothing below applies.
        #[serde(default)]
        session: Option<String>,
        /// Paths to stage before committing. Empty means commit whatever is
        /// already staged.
        #[serde(default)]
        paths: Vec<String>,
        /// Return the full staged diff, not only the stat. An overseer leaves
        /// this off so the diff never enters its context unasked.
        #[serde(default)]
        full_diff: bool,
        /// The project's forbidden-text patterns (C13). Checked against the
        /// message and the branch name. Empty until a project policy exists,
        /// which makes the check a no-op rather than a guess. Core sends `[]`
        /// until it can read `project.agent_policy.forbidden_text` itself.
        #[serde(default)]
        forbidden_text: Vec<String>,
        /// Commit in this linked worktree instead of the session's own
        /// directory. Must be a worktree of the same repo, under the workspace.
        #[serde(default)]
        worktree: Option<String>,
    },
    Checkout {
        #[serde(default)]
        repo: String,
        branch: String,
        #[serde(default)]
        create: bool,
    },
}

#[derive(Debug, Serialize)]
pub struct GitOpResp {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// One line naming what was committed, for an overseer that must not read
    /// the diff (C14). Set only by the session commit path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
    /// `git diff --cached --stat` of what was committed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff_stat: Option<String>,
    /// The full staged diff, only when the caller asked for it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
    /// Hunks a reviewer should look at before trusting the commit: pins,
    /// `unsafe`, and foreign-function bindings.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub review: Vec<String>,
}

impl GitOpResp {
    fn bare() -> Self {
        Self {
            ok: true,
            branch: None,
            commit: None,
            digest: None,
            diff_stat: None,
            diff: None,
            review: Vec::new(),
        }
    }
}

/// A line a reviewer should read before trusting a commit made for a worker.
const REVIEW_PATTERNS: &[(&str, &str)] = &[
    (
        "pin",
        r#"\b(rev|version|sha|commit)\s*[:=]\s*['"]?[0-9a-f]{7,}"#,
    ),
    ("unsafe", r"\bunsafe\b"),
    (
        "ffi",
        r#"extern\s+"C"|#\[no_mangle\]|\b(ctypes|cffi|cdll)\b"#,
    ),
];

/// Something that must not be committed: a conflict left unresolved, or a
/// secret staged by mistake. Names and shapes only; a match refuses the commit.
const REJECT_PATTERNS: &[(&str, &str)] = &[
    ("conflict marker", r"^(<<<<<<<|>>>>>>>|=======)"),
    ("private key", r"-----BEGIN [A-Z ]*PRIVATE KEY-----"),
    (
        "credential assignment",
        r#"(?i)\b(api[_-]?key|secret|token|password)\b\s*[:=]\s*["'][^"']{8,}"#,
    ),
];

/// The oversight commit (C14). With no `session` this is the GUI's plain
/// commit: stage nothing, check nothing, commit whatever is staged. With one,
/// the worker must be idle, hibernated, waiting for input or blocked — never
/// mid-turn — and the commit lands in the repo that worker's directory belongs
/// to (or a linked worktree of it), under that repo's own identity. Detached
/// HEAD and the repo's default branch are refused.
#[allow(clippy::too_many_arguments)]
async fn commit_op(
    state: &Arc<AppState>,
    repo: String,
    message: String,
    session: Option<String>,
    paths: Vec<String>,
    full_diff: bool,
    forbidden_text: Vec<String>,
    worktree: Option<String>,
) -> Result<Json<GitOpResp>> {
    let message = message.trim().to_string();
    if message.is_empty() {
        return Err(ApiError::BadRequest("message must not be empty".into()));
    }
    let repo = match session.as_deref() {
        Some(id) => session_commit_dir(state, id, worktree.as_deref()).await?,
        None => resolve_repo(state, &repo)
            .await?
            .ok_or(ApiError::NotFound)?,
    };
    for path in &paths {
        let path = clean_repo_rel(path)?;
        // `--literal-pathspecs` so `*`, `:(top)`, `:/` and `:(glob)**` are file
        // names, not pathspecs that widen what gets staged. A miss is a 400:
        // git exits 128, and `run_git` would otherwise report that as a 500.
        match run_git(&repo, &["--literal-pathspecs", "add", "--", &path]).await {
            Ok(_) => {}
            Err(ApiError::Internal(detail)) if detail.contains("did not match any files") => {
                return Err(ApiError::BadRequest(format!(
                    "path {path:?} is not a file in this repo"
                )));
            }
            Err(other) => return Err(other),
        }
    }
    if session.is_some() {
        let branch = run_git(&repo, &["rev-parse", "--abbrev-ref", "HEAD"]).await?;
        let branch = branch.trim();
        if branch.is_empty() || branch == "HEAD" {
            return Err(ApiError::BadRequest(
                "refusing to commit: HEAD is detached; check out a branch first".into(),
            ));
        }
        let default_branch = default_branch_name(&repo).await;
        if branch == default_branch {
            return Err(ApiError::BadRequest(format!(
                "refusing to commit on the default branch {default_branch:?}; \
                 check out a work branch first"
            )));
        }
        check_forbidden(&forbidden_text, &message, branch)?;
        let staged = run_git(&repo, &["diff", "--cached"]).await?;
        // A conflicted merge has no cached diff (`* Unmerged path`), so the
        // working-tree diff is what carries the markers.
        let conflicts = run_git(&repo, &["diff"]).await?;
        if let Some(why) = staged_rejection(&format!("{staged}\n{conflicts}")) {
            return Err(ApiError::BadRequest(why));
        }
        // `git add -N` leaves an intent-to-add: status shows ` A`, and
        // `diff --cached --raw` shows nothing at all for it.
        let status = run_git(&repo, &["status", "--porcelain"]).await?;
        if status.lines().any(is_intent_to_add) {
            return Err(ApiError::BadRequest(
                "staged diff has an intent-to-add entry (git add -N): add the file's \
                 contents, or unstage it, before committing"
                    .into(),
            ));
        }
        let review = review_flags(&staged);
        let stat = run_git(&repo, &["diff", "--cached", "--stat"]).await?;
        let digest = digest_line(&stat);
        run_git(&repo, &["commit", "-m", &message]).await?;
        let sha = run_git(&repo, &["rev-parse", "HEAD"]).await?;
        return Ok(Json(GitOpResp {
            commit: Some(sha.trim().to_string()),
            digest: Some(digest),
            diff_stat: Some(stat.trim_end().to_string()),
            diff: full_diff.then_some(staged),
            review,
            ..GitOpResp::bare()
        }));
    }
    run_git(&repo, &["commit", "-m", &message]).await?;
    let sha = run_git(&repo, &["rev-parse", "HEAD"]).await?;
    Ok(Json(GitOpResp {
        commit: Some(sha.trim().to_string()),
        ..GitOpResp::bare()
    }))
}

/// The directory an oversight commit lands in: the repo the session works in,
/// or `worktree` when the caller named a linked worktree of that same repo.
async fn session_commit_dir(
    state: &Arc<AppState>,
    id: &str,
    worktree: Option<&str>,
) -> Result<PathBuf> {
    let repo = session_repo(state, id).await?;
    let Some(worktree) = worktree.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(repo);
    };
    let root = &state.config.workspace_root;
    let candidate = workspace_path::resolve_existing(root, worktree)?;
    let worktree_repo = find_repo_root(&candidate, root).await.map_err(|_| {
        ApiError::BadRequest(format!(
            "worktree {worktree:?} is not inside a git repo under the workspace"
        ))
    })?;
    let session_common = git_common_dir(&repo).await?;
    let worktree_common = git_common_dir(&worktree_repo).await?;
    if session_common != worktree_common {
        return Err(ApiError::BadRequest(format!(
            "worktree {worktree:?} is not a worktree of the session's repo"
        )));
    }
    Ok(worktree_repo)
}

/// `git rev-parse --git-common-dir`, absolute, so two worktrees of one repo
/// compare equal and a different repo does not.
async fn git_common_dir(repo: &Path) -> Result<PathBuf> {
    let raw = run_git(repo, &["rev-parse", "--git-common-dir"]).await?;
    let reported = PathBuf::from(raw.trim());
    let absolute = if reported.is_absolute() {
        reported
    } else {
        repo.join(reported)
    };
    Ok(absolute.canonicalize().unwrap_or(absolute))
}

/// The repo's default branch: `refs/remotes/origin/HEAD`, else `main`. A repo
/// with no origin is not an error — there is nothing to read, so `main`.
async fn default_branch_name(repo: &Path) -> String {
    let output = Command::new("git")
        .args(["symbolic-ref", "--short", "refs/remotes/origin/HEAD"])
        .current_dir(repo)
        .output()
        .await;
    let Ok(output) = output else {
        return "main".to_string();
    };
    if !output.status.success() {
        return "main".to_string();
    }
    let name = String::from_utf8_lossy(&output.stdout);
    let short = name.trim().rsplit('/').next().unwrap_or("").trim();
    if short.is_empty() {
        "main".to_string()
    } else {
        short.to_string()
    }
}

/// The repo the session works in: its directory, walked up to a git root
/// inside the workspace. A session the engine does not know is a 404; one
/// whose directory is not a repo is a 400 naming the directory.
async fn session_repo(state: &Arc<AppState>, id: &str) -> Result<PathBuf> {
    let uuid: uuid::Uuid = id
        .parse()
        .map_err(|_| ApiError::BadRequest(format!("not a session id: {id}")))?;
    let summary = match state.sessions.get(uuid) {
        Ok(session) => session.summary(),
        Err(ApiError::Conflict(_)) => state
            .sessions
            .hibernated_summary(uuid)
            .ok_or(ApiError::NotFound)?,
        Err(other) => return Err(other),
    };
    if !commit_quiet(summary.activity, summary.blocked.is_some()) {
        return Err(ApiError::Conflict(format!(
            "session {id} is {:?}: an oversight commit needs the worker idle, \
             hibernated, waiting for input or blocked, not mid-turn",
            summary.activity
        )));
    }
    let cwd = PathBuf::from(&summary.cwd);
    find_repo_root(&cwd, &state.config.workspace_root)
        .await
        .map_err(|_| {
            ApiError::BadRequest(format!(
                "session {id} works in {}, which is not inside a git repo",
                summary.cwd
            ))
        })
}

/// Quiet enough to commit over: not running, and not sitting on an approval.
/// Waiting for the person is quiet — nothing is being written.
fn commit_quiet(activity: vogt_engine_contract::ActivityState, blocked: bool) -> bool {
    use vogt_engine_contract::ActivityState::{Hibernated, Idle, WaitingForInput};
    matches!(activity, Idle | Hibernated | WaitingForInput) || blocked
}

/// Reject a message or branch the project's policy forbids. An empty list —
/// what every project has until C13 lands — refuses nothing.
fn check_forbidden(patterns: &[String], message: &str, branch: &str) -> Result<()> {
    for pattern in patterns {
        let re = regex::Regex::new(pattern).map_err(|e| {
            ApiError::BadRequest(format!("forbidden_text {pattern:?} is not a regex: {e}"))
        })?;
        if re.is_match(message) {
            return Err(ApiError::BadRequest(format!(
                "commit message matches the project's forbidden text {pattern:?}"
            )));
        }
        if re.is_match(branch) {
            return Err(ApiError::BadRequest(format!(
                "branch {branch:?} matches the project's forbidden text {pattern:?}"
            )));
        }
    }
    Ok(())
}

/// What must not be in the staged diff: an unresolved conflict or a secret.
/// `None` when the diff is clean.
fn staged_rejection(staged: &str) -> Option<String> {
    for (name, pattern) in REJECT_PATTERNS {
        let re = regex::Regex::new(pattern).expect("static pattern");
        if staged.lines().any(|line| {
            let body = line.trim_start_matches('+');
            re.is_match(body)
        }) {
            return Some(format!(
                "staged diff has a {name}: resolve it before committing"
            ));
        }
    }
    None
}

/// `git status --porcelain` marks an intent-to-add (`git add -N`) as a new
/// file that is staged but has no content: a leading space, then `A`.
fn is_intent_to_add(line: &str) -> bool {
    line.starts_with(" A")
}

/// Added lines a reviewer should see: dependency pins, `unsafe`, FFI.
fn review_flags(staged: &str) -> Vec<String> {
    let mut found = Vec::new();
    for (name, pattern) in REVIEW_PATTERNS {
        let re = regex::Regex::new(pattern).expect("static pattern");
        let hits = staged
            .lines()
            .filter(|line| line.starts_with('+') && !line.starts_with("+++"))
            .filter(|line| re.is_match(line))
            .count();
        if hits > 0 {
            found.push(format!("{hits} {name}"));
        }
    }
    found
}

/// `3 files changed, +40 -2` from a diff stat, so the result names the size
/// without carrying the diff.
fn digest_line(stat: &str) -> String {
    stat.lines()
        .last()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .unwrap_or("nothing staged")
        .to_string()
}

pub async fn operate(
    State(state): State<Arc<AppState>>,
    Json(req): Json<GitOpReq>,
) -> Result<Json<GitOpResp>> {
    match req {
        GitOpReq::Stage { repo, path } => {
            let repo = resolve_repo(&state, &repo)
                .await?
                .ok_or(ApiError::NotFound)?;
            let path = clean_repo_rel(&path)?;
            run_git(&repo, &["add", "--", &path]).await?;
            Ok(Json(GitOpResp::bare()))
        }
        GitOpReq::Unstage { repo, path } => {
            let repo = resolve_repo(&state, &repo)
                .await?
                .ok_or(ApiError::NotFound)?;
            let path = clean_repo_rel(&path)?;
            run_git(&repo, &["reset", "HEAD", "--", &path]).await?;
            Ok(Json(GitOpResp::bare()))
        }
        GitOpReq::Discard { repo, path } => {
            let repo = resolve_repo(&state, &repo)
                .await?
                .ok_or(ApiError::NotFound)?;
            let path = clean_repo_rel(&path)?;
            let tracked = Command::new("git")
                .args(["ls-files", "--error-unmatch", "--", &path])
                .current_dir(&repo)
                .output()
                .await
                .map_err(|e| ApiError::Internal(format!("spawn git: {e}")))?
                .status
                .success();
            if tracked {
                run_git(
                    &repo,
                    &[
                        "restore",
                        "--source=HEAD",
                        "--staged",
                        "--worktree",
                        "--",
                        &path,
                    ],
                )
                .await?;
            } else {
                run_git(&repo, &["clean", "-fd", "--", &path]).await?;
            }
            Ok(Json(GitOpResp::bare()))
        }
        GitOpReq::Commit {
            repo,
            message,
            session,
            paths,
            full_diff,
            forbidden_text,
            worktree,
        } => {
            commit_op(
                &state,
                repo,
                message,
                session,
                paths,
                full_diff,
                forbidden_text,
                worktree,
            )
            .await
        }
        GitOpReq::Checkout {
            repo,
            branch,
            create,
        } => {
            let repo = resolve_repo(&state, &repo)
                .await?
                .ok_or(ApiError::NotFound)?;
            let branch = branch.trim();
            if branch.is_empty() {
                return Err(ApiError::BadRequest("branch must not be empty".into()));
            }
            // A leading-dash "branch" is parsed by git as a flag, not a ref:
            // `checkout -f` force-discards local modifications,
            // `--detach`/`--merge`/`--ours` are likewise reachable. Stage/
            // Unstage/Discard already pass `--`; Checkout did not. Reject it.
            if branch.starts_with('-') {
                return Err(ApiError::BadRequest(
                    "branch name must not begin with '-'".into(),
                ));
            }
            if create {
                run_git(&repo, &["checkout", "-b", branch]).await?;
            } else {
                run_git(&repo, &["checkout", branch]).await?;
            }
            Ok(Json(GitOpResp {
                branch: Some(branch.to_string()),
                ..GitOpResp::bare()
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_basics() {
        assert_eq!(classify_status("?", "?"), StatusKind::Untracked);
        assert_eq!(classify_status(" ", "M"), StatusKind::Modified);
        assert_eq!(classify_status("M", " "), StatusKind::Staged);
        assert_eq!(classify_status("M", "M"), StatusKind::Staged);
        assert_eq!(classify_status("U", "U"), StatusKind::Conflicted);
        assert_eq!(classify_status("R", " "), StatusKind::Renamed);
        assert_eq!(classify_status(" ", "D"), StatusKind::Deleted);
    }

    #[test]
    fn clean_repo_rel_rejects_parent_and_absolute_paths() {
        assert_eq!(clean_repo_rel("src/main.rs").unwrap(), "src/main.rs");
        assert!(clean_repo_rel("").is_err());
        assert!(clean_repo_rel("../outside").is_err());
        assert!(clean_repo_rel("/absolute/path").is_err());
    }

    #[test]
    fn staged_diff_rejects_conflicts_and_secrets() {
        let conflict = staged_rejection("++<<<<<<< HEAD\n+ours\n").unwrap();
        assert!(conflict.contains("conflict marker"), "{conflict}");
        assert!(staged_rejection("++=======\n")
            .unwrap()
            .contains("conflict"));

        let secret = staged_rejection("+token = \"abcd1234efgh5678\"\n").unwrap();
        assert!(secret.contains("credential"), "{secret}");
        let key = staged_rejection("+-----BEGIN OPENSSH PRIVATE KEY-----\n").unwrap();
        assert!(key.contains("private key"), "{key}");

        assert!(staged_rejection("+fn ordinary() {}\n").is_none());
    }

    #[test]
    fn intent_to_add_is_a_zero_mode_new_file() {
        assert!(is_intent_to_add(" A src/new.rs"));
        assert!(!is_intent_to_add("A  src/added.rs"));
        assert!(!is_intent_to_add(" M src/old.rs"));
    }

    #[test]
    fn forbidden_text_checks_message_and_branch_and_is_a_noop_when_empty() {
        check_forbidden(&[], "mentions grok", "feat/grok").unwrap();
        let patterns = vec!["(?i)grok".to_string()];
        assert!(check_forbidden(&patterns, "by grok", "feat/x").is_err());
        assert!(check_forbidden(&patterns, "fine", "feat/grok-thing").is_err());
        check_forbidden(&patterns, "fine", "feat/x").unwrap();
        assert!(check_forbidden(&["[".to_string()], "m", "b").is_err());
    }

    #[test]
    fn review_flags_count_pins_unsafe_and_ffi() {
        let diff = concat!(
            "+++ b/src/lib.rs\n",
            "+fn safe() {}\n",
            "+unsafe fn raw() {}\n",
            "+extern \"C\" fn c_entry() {}\n",
            "+rev = \"0123456789abcdef\"\n",
        );
        let flags = review_flags(diff);
        assert!(flags.iter().any(|f| f == "1 pin"), "{flags:?}");
        assert!(flags.iter().any(|f| f == "1 unsafe"), "{flags:?}");
        assert!(flags.iter().any(|f| f == "1 ffi"), "{flags:?}");
        assert!(review_flags("+fn plain() {}\n").is_empty());
    }

    #[test]
    fn digest_line_is_the_stat_summary() {
        assert_eq!(
            digest_line(" a.rs | 2 +-\n 1 file changed, 1 insertion(+), 1 deletion(-)\n"),
            "1 file changed, 1 insertion(+), 1 deletion(-)"
        );
        assert_eq!(digest_line(""), "nothing staged");
    }

    #[test]
    fn waiting_for_input_is_quiet_enough_to_commit() {
        use vogt_engine_contract::ActivityState;
        assert!(commit_quiet(ActivityState::WaitingForInput, false));
        assert!(commit_quiet(ActivityState::Idle, false));
        assert!(commit_quiet(ActivityState::Hibernated, false));
        assert!(commit_quiet(ActivityState::Running, true));
        assert!(!commit_quiet(ActivityState::Running, false));
        assert!(!commit_quiet(ActivityState::AwaitingApproval, false));
    }

    #[tokio::test]
    async fn a_literal_pathspec_does_not_widen_the_add() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        git(repo, &["init", "-b", "topic"]);
        git(repo, &["config", "user.email", "t@example"]);
        git(repo, &["config", "user.name", "t"]);
        std::fs::write(repo.join("keep.txt"), "keep\n").unwrap();
        git(repo, &["add", "keep.txt"]);
        git(repo, &["commit", "-m", "base"]);
        std::fs::write(repo.join("star.txt"), "nope\n").unwrap();
        std::fs::write(repo.join("real.txt"), "yes\n").unwrap();

        let missed = run_git(repo, &["--literal-pathspecs", "add", "--", "*"])
            .await
            .expect_err("a literal `*` names no file");
        let ApiError::Internal(detail) = &missed else {
            panic!("pathspec miss must stay a git failure, not {missed:?}");
        };
        assert!(
            detail.contains("did not match any files"),
            "expected a pathspec miss, got {detail}"
        );
        let staged = run_git(repo, &["diff", "--cached", "--name-only"])
            .await
            .unwrap();
        assert!(
            staged.trim().is_empty(),
            "`*` must not stage star.txt, staged: {staged:?}"
        );

        run_git(repo, &["--literal-pathspecs", "add", "--", "real.txt"])
            .await
            .unwrap();
        let staged = run_git(repo, &["diff", "--cached", "--name-only"])
            .await
            .unwrap();
        assert_eq!(staged.trim(), "real.txt");
    }

    #[tokio::test]
    async fn the_default_branch_comes_from_origin_head_else_main() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        git(repo, &["init", "-b", "topic"]);
        assert_eq!(default_branch_name(repo).await, "main");

        let origin = tempfile::tempdir().unwrap();
        git(origin.path(), &["init", "-b", "develop", "--bare"]);
        git(repo, &["remote", "add", "origin", &origin.path().display().to_string()]);
        git(repo, &["config", "user.email", "t@example"]);
        git(repo, &["config", "user.name", "t"]);
        std::fs::write(repo.join("f.txt"), "f\n").unwrap();
        git(repo, &["add", "f.txt"]);
        git(repo, &["commit", "-m", "base"]);
        git(repo, &["push", "-u", "origin", "topic"]);
        // `push` does not write `refs/remotes/origin/HEAD`. Point it at the one
        // remote branch this clone has, the way `git remote set-head` would.
        git(
            repo,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/topic",
            ],
        );
        assert_eq!(default_branch_name(repo).await, "topic");
    }

    #[tokio::test]
    async fn a_linked_worktree_shares_the_common_dir_and_a_stranger_does_not() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-b", "topic"]);
        git(&repo, &["config", "user.email", "t@example"]);
        git(&repo, &["config", "user.name", "t"]);
        std::fs::write(repo.join("f.txt"), "f\n").unwrap();
        git(&repo, &["add", "f.txt"]);
        git(&repo, &["commit", "-m", "base"]);

        let linked = tmp.path().join("linked");
        git(&repo, &["worktree", "add", "-b", "side", &linked.display().to_string()]);
        let repo_common = git_common_dir(&repo).await.unwrap();
        let linked_common = git_common_dir(&linked).await.unwrap();
        assert_eq!(repo_common, linked_common);

        let other = tmp.path().join("other");
        std::fs::create_dir(&other).unwrap();
        git(&other, &["init", "-b", "topic"]);
        let other_common = git_common_dir(&other).await.unwrap();
        assert_ne!(repo_common, other_common);
    }

    fn git(dir: &std::path::Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }
}
