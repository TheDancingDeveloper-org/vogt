//! `POST /api/workspaces/clean` (WI-1325): drop a workspace's build output,
//! or remove the worktree once its work is safe elsewhere.
//!
//! `build_output` runs the WI-1304 cleanup on the tree and stamps
//! `cleaned_at`. `remove` takes the linked worktree away with
//! `git worktree remove` from the main checkout, and deletes its branch only
//! when that branch is merged. Both fail closed:
//!
//! - a tree a running session works in is refused (and, for `remove`, one a
//!   hibernated session would wake into);
//! - a dirty tree — any tracked change or untracked file; git-ignored files
//!   are regenerable and do not count — is never removed, `force` or not;
//! - a tree whose `HEAD` is in none of its base ref, its upstream,
//!   `origin/HEAD` or `origin/main`, or whose branch has stash entries, is
//!   unmerged work: refused unless the caller forces it, and a forced removal
//!   keeps the branch so its commits survive;
//! - a git command that fails is a refusal, never a guess;
//! - a path outside the engine's workspace root that is no workspace a
//!   session recorded is refused, symlinks resolved first, so the route
//!   cannot reach an arbitrary worktree on the host.
//!
//! Every outcome is an audit line (`event=workspace.clean`) with the path,
//! mode, caller and bytes freed — never a file list.

use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
};

use axum::{extract::State, Json};
use serde::{Deserialize, Serialize};

use crate::{app::AppState, error::ApiError, worktree_clean};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CleanMode {
    BuildOutput,
    Remove,
}

impl CleanMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::BuildOutput => "build_output",
            Self::Remove => "remove",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct CleanRequest {
    pub path: String,
    pub mode: CleanMode,
    #[serde(default)]
    pub force: bool,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CleanResult {
    pub path: String,
    pub mode: CleanMode,
    pub bytes_freed: u64,
    /// `remove`: the worktree is gone.
    pub removed: bool,
    /// `remove`: the merged branch that was deleted with it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch_deleted: Option<String>,
    /// `build_output`: when the build output was dropped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cleaned_at: Option<String>,
}

/// What a removal will do, once every check passed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Removal {
    pub root: PathBuf,
    pub repo_root: PathBuf,
    pub branch: Option<String>,
    pub merged: bool,
}

/// Why a removal was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    NotWorktree,
    Dirty,
    Unmerged(String),
    Git(String),
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotWorktree => f.write_str("not a linked git worktree; a main checkout is never removed"),
            Self::Dirty => f.write_str(
                "the worktree has uncommitted changes or untracked files; commit or discard them first",
            ),
            Self::Unmerged(why) => write!(f, "the worktree holds unmerged work ({why}); pass force to remove it anyway (the branch is kept)"),
            Self::Git(why) => write!(f, "git could not answer, so nothing is removed: {why}"),
        }
    }
}

fn git(dir: &Path, args: &[&str]) -> std::io::Result<std::process::Output> {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
}

fn git_ok(dir: &Path, args: &[&str]) -> Result<String, Refusal> {
    let out = git(dir, args).map_err(|e| Refusal::Git(format!("git {}: {e}", args[0])))?;
    if !out.status.success() {
        return Err(Refusal::Git(format!(
            "git {} exited {}",
            args[0],
            out.status.code().unwrap_or(-1)
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Whether the canonical `path` is a tree the engine may clean: inside its
/// `workspace_root`, or exactly a workspace a session recorded. Both sides
/// are canonicalised, so a symlink under the root that points outside it
/// resolves outside and is refused.
pub fn confined<I, S>(workspace_root: &Path, known: I, path: &Path) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let canonical = |p: &Path| std::fs::canonicalize(p).ok();
    let Some(path) = canonical(path) else {
        return false;
    };
    if canonical(workspace_root).is_some_and(|root| path.starts_with(root)) {
        return true;
    }
    known
        .into_iter()
        .any(|ws| canonical(Path::new(ws.as_ref())).is_some_and(|ws| ws == path))
}

/// Check whether the linked worktree at `path` may be removed. `base_ref` is
/// the ref the engine cut it from, when known. With `force`, unmerged work is
/// no refusal; a dirty tree always is.
pub fn plan_removal(path: &Path, base_ref: Option<&str>, force: bool) -> Result<Removal, Refusal> {
    let root = worktree_clean::linked_worktree(path).ok_or(Refusal::NotWorktree)?;
    let status = git_ok(
        &root,
        &["status", "--porcelain=v1", "--untracked-files=all"],
    )?;
    if !status.trim().is_empty() {
        return Err(Refusal::Dirty);
    }
    let common = git_ok(
        &root,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    let repo_root = Path::new(common.trim())
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| Refusal::Git("no main checkout above the git dir".into()))?;
    let branch = git(&root, &["symbolic-ref", "-q", "--short", "HEAD"])
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|b| !b.is_empty());
    let merged = head_merged(&root, base_ref)?;
    let stashed = stash_held(&root, branch.as_deref())?;
    if !force {
        if !merged {
            return Err(Refusal::Unmerged(
                "HEAD is in none of its base, upstream, origin/HEAD or origin/main".into(),
            ));
        }
        if stashed {
            return Err(Refusal::Unmerged("the branch has stash entries".into()));
        }
    }
    Ok(Removal {
        root,
        repo_root,
        branch,
        merged: merged && !stashed,
    })
}

/// Whether `HEAD` is reachable from any ref its work could have landed on.
fn head_merged(root: &Path, base_ref: Option<&str>) -> Result<bool, Refusal> {
    let mut candidates: Vec<&str> = Vec::new();
    if let Some(base) = base_ref.filter(|b| !b.is_empty() && !b.starts_with('-')) {
        candidates.push(base);
    }
    candidates.extend(["@{upstream}", "origin/HEAD", "origin/main"]);
    for candidate in candidates {
        let spec = format!("{candidate}^{{commit}}");
        let exists = git(root, &["rev-parse", "--verify", "-q", &spec])
            .map_err(|e| Refusal::Git(e.to_string()))?;
        if !exists.status.success() {
            continue;
        }
        let tip = String::from_utf8_lossy(&exists.stdout).trim().to_string();
        let out = git(root, &["merge-base", "--is-ancestor", "HEAD", &tip])
            .map_err(|e| Refusal::Git(e.to_string()))?;
        match out.status.code() {
            Some(0) => return Ok(true),
            Some(1) => {}
            code => return Err(Refusal::Git(format!("git merge-base exited {code:?}"))),
        }
    }
    Ok(false)
}

/// Whether the repo's stash holds an entry made on `branch` (or on a
/// detached HEAD, when there is no branch). The stash is shared by every
/// worktree of the repo, so an entry from this branch may be the only copy
/// of work done here.
fn stash_held(root: &Path, branch: Option<&str>) -> Result<bool, Refusal> {
    let list = git_ok(root, &["stash", "list", "--format=%gs"])?;
    let marker = format!("on {}:", branch.unwrap_or("(no branch)"));
    Ok(list.lines().any(|l| l.contains(&marker)))
}

/// Remove the planned worktree; delete its branch when merged. Returns the
/// bytes the tree held and the branch deleted.
pub fn remove(plan: &Removal) -> Result<(u64, Option<String>), Refusal> {
    let bytes = crate::workspaces::tree_bytes(&plan.root);
    let root = plan.root.to_string_lossy().into_owned();
    git_ok(&plan.repo_root, &["worktree", "remove", "--force", &root])?;
    let mut deleted = None;
    if plan.merged {
        if let Some(branch) = plan.branch.as_deref() {
            match git_ok(&plan.repo_root, &["branch", "-D", "--", branch]) {
                Ok(_) => deleted = Some(branch.to_string()),
                Err(e) => {
                    tracing::warn!(branch, error = %e, "removed a worktree but kept its branch")
                }
            }
        }
    }
    Ok((bytes, deleted))
}

fn audit(
    path: &str,
    mode: CleanMode,
    actor: &str,
    outcome: &str,
    bytes: u64,
    reason: Option<&str>,
) {
    tracing::info!(
        target: "vogt::audit",
        event = "workspace.clean",
        path,
        mode = mode.as_str(),
        actor,
        outcome,
        bytes_freed = bytes,
        reason = reason.unwrap_or(""),
        "workspace clean"
    );
}

pub async fn clean_workspace(
    State(state): State<Arc<AppState>>,
    identity: Option<axum::Extension<crate::auth::AuthorizedIdentity>>,
    Json(req): Json<CleanRequest>,
) -> crate::error::Result<Json<CleanResult>> {
    let actor = identity
        .as_deref()
        .map(|i| i.name.clone())
        .unwrap_or_else(|| "anonymous".into());
    if req.force {
        match identity.as_deref() {
            Some(i) if i.person || i.break_glass || i.stack_secret => {}
            Some(_) => {
                audit(
                    &req.path,
                    req.mode,
                    &actor,
                    "refused",
                    0,
                    req.reason.as_deref(),
                );
                return Err(ApiError::Forbidden(
                    "only a person forces a workspace removal, not an agent; ask a person to do it"
                        .into(),
                ));
            }
            None => return Err(ApiError::Unauthorized),
        }
    }
    let path = std::fs::canonicalize(&req.path)
        .map_err(|_| ApiError::BadRequest(format!("no directory at {}", req.path)))?;
    let workspaces = state.sessions.workspaces();
    if !confined(
        &state.config.workspace_root,
        workspaces.iter().map(|(_, _, ws)| ws.path.as_str()),
        &path,
    ) {
        audit(
            &req.path,
            req.mode,
            &actor,
            "refused",
            0,
            req.reason.as_deref(),
        );
        return Err(ApiError::Forbidden(
            "the path is outside the engine's workspace root and is no session's workspace".into(),
        ));
    }
    let in_use = state
        .sessions
        .cwds_in_use(None, req.mode == CleanMode::Remove);
    if worktree_clean::shared_with(&path, in_use.iter().map(String::as_str)) {
        audit(
            &req.path,
            req.mode,
            &actor,
            "refused",
            0,
            req.reason.as_deref(),
        );
        return Err(ApiError::Conflict(
            "a live or hibernated session works in this tree; stop it first".into(),
        ));
    }
    let shown = path.to_string_lossy().into_owned();
    let base_ref = workspaces
        .into_iter()
        .find(|(_, _, ws)| Path::new(&ws.path) == path)
        .and_then(|(_, _, ws)| ws.base_ref);
    let result = match req.mode {
        CleanMode::BuildOutput => {
            let target = path.clone();
            let removed = tokio::task::spawn_blocking(move || worktree_clean::clean(&target))
                .await
                .map_err(|e| ApiError::Internal(e.to_string()))?;
            let at = time::OffsetDateTime::now_utc()
                .format(&time::format_description::well_known::Rfc3339)
                .unwrap_or_default();
            state.sessions.mark_workspace_cleaned(&shown, &at);
            CleanResult {
                path: shown.clone(),
                mode: req.mode,
                bytes_freed: removed.iter().map(|c| c.bytes).sum(),
                removed: false,
                branch_deleted: None,
                cleaned_at: Some(at),
            }
        }
        CleanMode::Remove => {
            let force = req.force;
            let target = path.clone();
            let done = tokio::task::spawn_blocking(move || {
                let plan = plan_removal(&target, base_ref.as_deref(), force)?;
                remove(&plan)
            })
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?;
            match done {
                Ok((bytes, branch_deleted)) => CleanResult {
                    path: shown.clone(),
                    mode: req.mode,
                    bytes_freed: bytes,
                    removed: true,
                    branch_deleted,
                    cleaned_at: None,
                },
                Err(refusal) => {
                    audit(
                        &shown,
                        req.mode,
                        &actor,
                        "refused",
                        0,
                        req.reason.as_deref(),
                    );
                    return Err(match refusal {
                        Refusal::NotWorktree => ApiError::BadRequest(refusal.to_string()),
                        _ => ApiError::Conflict(refusal.to_string()),
                    });
                }
            }
        }
    };
    audit(
        &shown,
        req.mode,
        &actor,
        "done",
        result.bytes_freed,
        req.reason.as_deref(),
    );
    Ok(Json(result))
}

#[cfg(test)]
mod confinement_tests {
    use super::confined;

    #[test]
    fn confined_to_the_root_or_a_known_workspace() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let inside = root.path().join("wt");
        std::fs::create_dir(&inside).unwrap();
        let foreign = outside.path().join("wt");
        std::fs::create_dir(&foreign).unwrap();
        let link = root.path().join("link");
        std::os::unix::fs::symlink(&foreign, &link).unwrap();
        let none: [&str; 0] = [];

        assert!(confined(root.path(), none, &inside));
        assert!(!confined(root.path(), none, &foreign));
        assert!(
            !confined(root.path(), none, &link),
            "a symlink out of the root"
        );
        let known = [foreign.to_string_lossy().into_owned()];
        assert!(
            confined(root.path(), &known, &foreign),
            "a recorded workspace"
        );
        assert!(
            confined(root.path(), &known, &link),
            "resolves to the recorded workspace"
        );
        assert!(!confined(root.path(), none, &root.path().join("missing")));
    }
}
