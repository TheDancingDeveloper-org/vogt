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
        let full = repo.join(&path);
        tokio::fs::read_to_string(&full).await.unwrap_or_default()
    };

    Ok(Json(DiffResp {
        path,
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
            Ok(Json(GitOpResp {
                ok: true,
                branch: None,
                commit: None,
            }))
        }
        GitOpReq::Unstage { repo, path } => {
            let repo = resolve_repo(&state, &repo)
                .await?
                .ok_or(ApiError::NotFound)?;
            let path = clean_repo_rel(&path)?;
            run_git(&repo, &["reset", "HEAD", "--", &path]).await?;
            Ok(Json(GitOpResp {
                ok: true,
                branch: None,
                commit: None,
            }))
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
            Ok(Json(GitOpResp {
                ok: true,
                branch: None,
                commit: None,
            }))
        }
        GitOpReq::Commit { repo, message } => {
            let repo = resolve_repo(&state, &repo)
                .await?
                .ok_or(ApiError::NotFound)?;
            let message = message.trim();
            if message.is_empty() {
                return Err(ApiError::BadRequest("message must not be empty".into()));
            }
            run_git(&repo, &["commit", "-m", message]).await?;
            let commit = run_git(&repo, &["rev-parse", "HEAD"])
                .await?
                .trim()
                .to_string();
            Ok(Json(GitOpResp {
                ok: true,
                branch: None,
                commit: Some(commit),
            }))
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
            if create {
                run_git(&repo, &["checkout", "-b", branch]).await?;
            } else {
                run_git(&repo, &["checkout", branch]).await?;
            }
            Ok(Json(GitOpResp {
                ok: true,
                branch: Some(branch.to_string()),
                commit: None,
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
}
