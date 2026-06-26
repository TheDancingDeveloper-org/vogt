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

#[derive(Debug, Serialize)]
pub struct GitStatus {
    pub repo: String,
    pub is_repo: bool,
    pub branch: String,
    pub ahead: u32,
    pub behind: u32,
    pub entries: Vec<StatusEntry>,
}

#[derive(Debug, Serialize, Clone)]
pub struct StatusEntry {
    pub path: String,
    /// XY codes per `git status --porcelain=v1` (index, worktree).
    pub index: String,
    pub worktree: String,
    pub kind: StatusKind,
}

#[derive(Debug, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum StatusKind {
    Untracked,
    Modified,
    Staged,
    Conflicted,
    Renamed,
    Deleted,
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

#[derive(Debug, Serialize)]
pub struct DiffResp {
    pub path: String,
    /// Working-tree (or staged) content of the file. Empty string if the
    /// file has been deleted.
    pub current: String,
    /// HEAD content for diff. Empty string if the file is newly added.
    pub head: String,
}

pub async fn diff(
    State(state): State<Arc<AppState>>,
    Query(q): Query<DiffQuery>,
) -> Result<Json<DiffResp>> {
    let repo = resolve_repo(&state, &q.repo)
        .await?
        .ok_or(ApiError::NotFound)?;

    // HEAD content via `git show HEAD:path`. Returns empty if untracked.
    let head_arg = format!("HEAD:{}", q.path);
    let head: String = run_git(&repo, &["show", &head_arg])
        .await
        .unwrap_or_default();

    let current = if q.staged {
        // Staged content — `git show :path`
        let staged_arg = format!(":{}", q.path);
        run_git(&repo, &["show", &staged_arg])
            .await
            .unwrap_or_default()
    } else {
        let full = repo.join(&q.path);
        tokio::fs::read_to_string(&full).await.unwrap_or_default()
    };

    Ok(Json(DiffResp {
        path: q.path,
        current,
        head,
    }))
}

#[derive(Debug, Deserialize)]
pub struct LogQuery {
    #[serde(default)]
    pub repo: String,
    pub n: Option<u32>,
}

#[derive(Debug, Serialize)]
pub struct LogEntry {
    pub hash: String,
    pub short: String,
    pub author: String,
    pub date: String,
    pub subject: String,
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

#[derive(Debug, Serialize)]
pub struct BranchInfo {
    pub current: String,
    pub all: Vec<String>,
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
}
