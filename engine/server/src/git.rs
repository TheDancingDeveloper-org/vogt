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
use vogt_engine_contract::{
    BranchInfo, DiffResp, GitStatus, LogEntry, SessionRole, StatusEntry, StatusKind,
};

use crate::{
    app::AppState,
    auth::AuthorizedIdentity,
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

/// A ref name safe to hand git as one argument: no whitespace, no control
/// characters, and none of the characters git treats as starting an option or
/// a path. The caller is asking for a branch and a base, not a command.
///
/// Slashes are allowed (`integrate/vogt-next`) but each component is checked
/// the way `git check-ref-format` checks one: none may be empty, `.`, end in
/// `.lock`, or start or end with `.`. `..` and `@{` are already refused above.
fn clean_ref_name(what: &str, name: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty() {
        return Err(ApiError::BadRequest(format!("{what} must not be empty")));
    }
    if name.starts_with('-')
        || name.starts_with('/')
        || name.ends_with('/')
        || name.contains("//")
        || name.contains("..")
    {
        return Err(ApiError::BadRequest(format!(
            "{what} {name:?} is not a ref name"
        )));
    }
    if name.chars().any(|c| {
        c.is_whitespace()
            || c.is_control()
            || matches!(c, '~' | '^' | ':' | '\\' | '?' | '*' | '[' | '@' | '{')
    }) {
        return Err(ApiError::BadRequest(format!(
            "{what} {name:?} is not a ref name"
        )));
    }
    if name.split('/').any(|part| {
        part.is_empty()
            || part == "."
            || part.ends_with(".lock")
            || part.starts_with('.')
            || part.ends_with('.')
    }) {
        return Err(ApiError::BadRequest(format!(
            "{what} {name:?} is not a ref name"
        )));
    }
    Ok(name.to_string())
}

/// A directory name for a worktree: one path component, so the worktree can
/// only land beside the checkout and never outside it or over a nested path.
fn clean_worktree_dirname(branch: &str) -> Result<String> {
    if branch.contains(['/', '\\']) || branch == "." {
        return Err(ApiError::BadRequest(format!(
            "branch {branch:?} is not a single worktree directory name"
        )));
    }
    Ok(branch.to_string())
}

/// Where [`create_worktree`] would put a worktree for `branch`: a sibling of
/// `checkout`, named for the branch. Checks the names without touching git or
/// the disk, so a caller can vet the destination before anything is cut.
pub fn worktree_destination(checkout: &Path, branch: &str) -> Result<PathBuf> {
    let branch = clean_ref_name("branch", branch)?;
    let dirname = clean_worktree_dirname(&branch)?;
    let parent = checkout.parent().ok_or_else(|| {
        ApiError::BadRequest(format!(
            "worktree checkout {} has no parent directory",
            checkout.display()
        ))
    })?;
    Ok(parent.join(dirname))
}

/// Create a git worktree of `checkout` for a session (WI-1298).
///
/// The worktree is [`worktree_destination`], on a new branch of that name cut
/// from `base_ref`. `checkout` is the main working tree or a worktree of it;
/// git cuts either way. The checkout's own tree is never written to: a dirty
/// tree is left exactly as it was, because the new branch comes from
/// `base_ref` rather than from the working tree.
///
/// The base is fetched from `origin` first and the new branch is cut from
/// `origin/<branch>`, not from the checkout's local ref of the same name
/// (WI-1630). The shared checkout's local branch is often far behind the
/// remote, and cutting from it silently starts the session on stale code. A
/// fetch that fails refuses the start: falling back to the local ref is the
/// bug this closes. A `base_ref` that already names the remote
/// (`origin/main`, `origin/integrate/vogt-next`) has that one prefix stripped
/// and is fetched the same way. The fetch runs as the engine process (its
/// uid and credentials), not as the session user: the session does not exist
/// yet.
///
/// Refuses before running git when the checkout is not a git tree, when
/// either name is not a ref name, when the branch already exists, or when the
/// destination already exists. A fetch that fails, or a remote ref that is
/// still absent after it, refuses before the worktree is cut. When git itself
/// refuses part way, neither the directory nor the branch is left behind.
pub fn create_worktree(checkout: &Path, branch: &str, base_ref: &str) -> Result<PathBuf> {
    let destination = worktree_destination(checkout, branch)?;
    let branch = clean_ref_name("branch", branch)?;
    let base_ref = clean_ref_name("base ref", base_ref)?;
    if !checkout.is_dir() {
        return Err(ApiError::BadRequest(format!(
            "worktree checkout {} does not exist",
            checkout.display()
        )));
    }
    if git_output(checkout, &["rev-parse", "--is-inside-work-tree"])
        .map(|out| out.trim() != "true")
        .unwrap_or(true)
    {
        return Err(ApiError::BadRequest(format!(
            "{} is not a git checkout to cut a worktree from",
            checkout.display()
        )));
    }
    if destination.exists() {
        return Err(ApiError::BadRequest(format!(
            "worktree {} already exists",
            destination.display()
        )));
    }
    // Checked here rather than left to git so that, past this point, a branch
    // of this name is one this call made and is safe to delete on failure.
    let local = format!("refs/heads/{branch}");
    if git_output(checkout, &["rev-parse", "--verify", "--quiet", &local]).is_ok() {
        return Err(ApiError::BadRequest(format!(
            "branch {branch:?} already exists in {}",
            checkout.display()
        )));
    }
    // Cut from the remote's tip. The local ref of the same name is the shared
    // checkout's, and it lags the remote (WI-1630). A fetch failure refuses
    // rather than silently cutting from that stale ref.
    let remote_ref = fetch_base_ref(checkout, &base_ref)?;
    git_output(
        checkout,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            &branch,
            &destination.display().to_string(),
            &remote_ref,
        ],
    )
    .map_err(|error| {
        // git can leave the directory, the branch, or both when it fails
        // part way.
        remove_worktree(checkout, &destination, &branch);
        ApiError::BadRequest(format!("could not create the worktree: {error}"))
    })?;
    Ok(destination)
}

/// Undo [`create_worktree`]: remove the worktree at `path` and delete
/// `branch`. Best effort — each half runs whether or not the other did, and a
/// failure is logged rather than returned, because the caller is already
/// reporting the error that made it undo the cut.
pub fn remove_worktree(checkout: &Path, path: &Path, branch: &str) {
    if path.exists() {
        let removed = std::process::Command::new("git")
            .args(["worktree", "remove", "--force"])
            .arg(path)
            .current_dir(checkout)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output();
        if !removed.as_ref().is_ok_and(|o| o.status.success()) {
            let _ = std::fs::remove_dir_all(path);
        }
    }
    let _ = git_output(checkout, &["worktree", "prune"]);
    let local = format!("refs/heads/{branch}");
    if git_output(checkout, &["rev-parse", "--verify", "--quiet", &local]).is_ok() {
        if let Err(e) = git_output(checkout, &["branch", "-D", branch]) {
            tracing::warn!(branch, error = %e, "could not delete a session worktree's branch");
        }
    }
}

/// Fetch `base_ref` from `origin` and return the remote-tracking ref to cut
/// from: always `origin/<branch>`, where `<branch>` is `base_ref` with one
/// leading `origin/` stripped when the caller already wrote the remote form.
///
/// A slash in the branch name is a branch (`integrate/vogt-next`), not a
/// reason to refuse. `origin/integrate/vogt-next` and `integrate/vogt-next`
/// fetch the same ref. Refuses when the name left after that strip is not a
/// branch name, when the fetch fails, or when the remote ref is not there
/// afterwards. The local ref is deliberately never a fallback — a start on a
/// base the remote could not confirm is worse than no start (WI-1630). The
/// fetch moves only the one remote-tracking ref, never a local branch.
///
/// The fetch runs as the engine process. `create_worktree` is called before
/// the session process exists, so a private origin is reached with a GitHub
/// token the engine holds, not a token the session process would later
/// receive (WI-1666). That token is the same one agent-auth aliases to
/// `GH_TOKEN` for sessions: `ENGINE_WORKTREE_GITHUB_TOKEN`, or `GH_TOKEN`
/// when the engine process itself already has it. It is handed to git only
/// through `GIT_ASKPASS` for this one fetch, never written to a ref, a
/// remote URL, argv, or a log line.
///
/// A GitHub `origin` with no such token refuses before git runs, naming the
/// missing credential. A non-GitHub origin (a local bare, a forge that does
/// not speak this token) is fetched with the engine user's own git
/// credentials, as before.
fn fetch_base_ref(checkout: &Path, base_ref: &str) -> Result<String> {
    // One prefix only. `origin/origin/main` is a branch literally named that,
    // which the remote will not have, and the fetch then refuses.
    let source = base_ref.strip_prefix("origin/").unwrap_or(base_ref);
    if source.is_empty()
        || source.starts_with('/')
        || source.ends_with('/')
        || source.contains("//")
        || clean_ref_name("base ref", source).is_err()
    {
        return Err(ApiError::BadRequest(format!(
            "base ref {base_ref:?} is not a branch on origin"
        )));
    }
    let remote_ref = format!("origin/{source}");
    // An explicit refspec. `git fetch origin <branch>` with a matching local
    // branch updates that local branch, not `refs/remotes/origin/<branch>`,
    // and the local branch is exactly the stale ref this must not follow.
    let spec = format!("+refs/heads/{source}:refs/remotes/origin/{source}");
    let github_https = origin_is_github_https(checkout);
    let token = worktree_github_token();
    if github_https && token.is_none() {
        return Err(ApiError::BadRequest(
            "could not fetch base ref from origin: this origin is https GitHub and the engine has no GitHub token for the worktree fetch (set ENGINE_WORKTREE_GITHUB_TOKEN to the same token sessions get as GH_TOKEN, or export GH_TOKEN on the engine process)".into(),
        ));
    }
    // Only an https GitHub origin uses the session token. A local path, another
    // forge, or git@github.com SSH keeps the engine user's own credentials.
    let fetch_token = token.as_deref().filter(|_| github_https);
    git_fetch(checkout, &spec, fetch_token).map_err(|error| {
        let detail = redact_token(error.to_string(), fetch_token);
        if github_https {
            ApiError::BadRequest(format!(
                "could not fetch base ref {base_ref:?} from origin using the engine GitHub token (ENGINE_WORKTREE_GITHUB_TOKEN, or GH_TOKEN on the engine process). The token may not be able to read this private repository: {detail}"
            ))
        } else {
            ApiError::BadRequest(format!(
                "could not fetch base ref {base_ref:?} from origin: {detail}"
            ))
        }
    })?;
    if git_output(checkout, &["rev-parse", "--verify", "--quiet", &remote_ref]).is_err() {
        return Err(ApiError::BadRequest(format!(
            "base ref {base_ref:?} is not on origin"
        )));
    }
    Ok(remote_ref)
}

/// git, synchronously, with its stdout on success and its stderr in the error.
///
/// The session start that calls [`create_worktree`] is itself synchronous, and
/// the worktree has to exist before the session's cwd is resolved, so this
/// cannot wait on the async runtime.
fn git_output(repo: &Path, args: &[&str]) -> Result<String> {
    git_command(repo, args, None)
}

/// `git fetch` of one refspec. When `token` is set it is the only credential
/// git may use for this command: askpass answers GitHub with
/// `x-access-token` / the token, and the checkout's credential helper is
/// blanked so it cannot supply a different one. `insteadOf` rewrites in the
/// repo config still apply. The token is an environment value, never an
/// argument.
fn git_fetch(repo: &Path, spec: &str, token: Option<&str>) -> Result<String> {
    git_command(repo, &["fetch", "--quiet", "origin", spec], token)
}

fn git_command(repo: &Path, args: &[&str], token: Option<&str>) -> Result<String> {
    let mut command = std::process::Command::new("git");
    command
        .args(args)
        .current_dir(repo)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0");
    if let Some(token) = token {
        // This fetch's only credential. Askpass answers GitHub with the
        // token; inherited token variables are removed so a different
        // GH_TOKEN in the engine environment cannot win, and the checkout's
        // credential helper is blanked via the environment config (which git
        // applies on top of the repo config). The token is not an argument.
        // `env_clear` is required: `Command` keeps an inherited GIT_ASKPASS
        // even after `env_remove` + `env`, and git then uses that one.
        let mut kept: Vec<(String, String)> = std::env::vars()
            .filter(|(key, _)| {
                !matches!(
                    key.as_str(),
                    "GH_TOKEN"
                        | "GITHUB_TOKEN"
                        | "GIT_AUTH_TOKEN"
                        | "GIT_ASKPASS"
                        | "ENGINE_WORKTREE_FETCH_TOKEN"
                )
            })
            .collect();
        kept.push(("GIT_CONFIG_NOSYSTEM".into(), "1".into()));
        kept.push(("GIT_TERMINAL_PROMPT".into(), "0".into()));
        kept.push(("GIT_ASKPASS".into(), worktree_askpass_path()));
        kept.push(("GIT_ASKPASS_REQUIRE".into(), "force".into()));
        kept.push(("GIT_CONFIG_COUNT".into(), "1".into()));
        kept.push(("GIT_CONFIG_KEY_0".into(), "credential.helper".into()));
        kept.push(("GIT_CONFIG_VALUE_0".into(), String::new()));
        kept.push(("ENGINE_WORKTREE_FETCH_TOKEN".into(), token.to_string()));
        command.env_clear();
        command.envs(kept);
    }
    let output = command
        .output()
        .map_err(|e| ApiError::Internal(format!("spawn git: {e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        let stderr = redact_token(stderr, token);
        return Err(ApiError::Internal(format!(
            "git {}: {}",
            args.join(" "),
            stderr.trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn origin_is_github_https(repo: &Path) -> bool {
    let Ok(url) = git_output(repo, &["remote", "get-url", "origin"]) else {
        return false;
    };
    origin_url_is_github_https(url.trim())
}

fn origin_url_is_github_https(url: &str) -> bool {
    let url = url.trim();
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("HTTPS://"))
        .or_else(|| url.strip_prefix("http://"))
        .or_else(|| url.strip_prefix("HTTP://"));
    let Some(rest) = rest else {
        return false;
    };
    let host = rest
        .rsplit_once('@')
        .map(|(_, hostpath)| hostpath)
        .unwrap_or(rest)
        .split('/')
        .next()
        .unwrap_or("")
        .split(':')
        .next()
        .unwrap_or("");
    host.eq_ignore_ascii_case("github.com")
}

/// The GitHub token the worktree fetch may use. `ENGINE_WORKTREE_GITHUB_TOKEN`
/// is the deployment's explicit copy of the token sessions receive as
/// `GH_TOKEN`. `GH_TOKEN` on the engine process is the same name, used when
/// the engine was started with it directly. Empty values are absent.
/// The value is never logged.
fn worktree_github_token() -> Option<String> {
    for key in ["ENGINE_WORKTREE_GITHUB_TOKEN", "GH_TOKEN"] {
        if let Ok(value) = std::env::var(key) {
            let value = value.trim().to_string();
            if !value.is_empty() {
                return Some(value);
            }
        }
    }
    None
}

fn worktree_askpass_path() -> String {
    std::env::var("ENGINE_WORKTREE_ASKPASS")
        .ok()
        .filter(|path| !path.trim().is_empty())
        .unwrap_or_else(|| "/usr/local/bin/vogt-git-askpass".to_string())
}

/// Drop a token that git echoed back (a URL, an askpass trace) before the
/// text can reach an error or a log line.
fn redact_token(text: String, token: Option<&str>) -> String {
    match token {
        Some(token) if !token.is_empty() && text.contains(token) => {
            text.replace(token, "[redacted]")
        }
        _ => text,
    }
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
///
/// A worker's commit never lands in the repo's main checkout (WI-1255): pool
/// workers are launched in the shared main checkout, so a commit made for one
/// without `worktree` would land on whatever branch that checkout — every
/// other session's too — has out. The worker's own linked worktree is where
/// its work is; name it.
async fn session_commit_dir(
    state: &Arc<AppState>,
    id: &str,
    worktree: Option<&str>,
) -> Result<PathBuf> {
    let (repo, role) = session_repo(state, id).await?;
    let dir = match worktree.map(str::trim).filter(|s| !s.is_empty()) {
        None => repo,
        Some(worktree) => linked_worktree_of(state, &repo, worktree).await?,
    };
    if role == SessionRole::Worker && is_main_checkout(&dir).await? {
        return Err(ApiError::BadRequest(format!(
            "refusing to commit for worker session {id} in the repo's main checkout \
             ({}): pass `worktree` naming the worker's own linked worktree",
            dir.display()
        )));
    }
    Ok(dir)
}

/// `worktree`, resolved under the workspace, when it is a worktree of the same
/// repo as `repo`. Workspace-relative or absolute (inside the workspace): an
/// overseer names a worktree by the path it sees.
async fn linked_worktree_of(state: &Arc<AppState>, repo: &Path, worktree: &str) -> Result<PathBuf> {
    let root = &state.config.workspace_root;
    let candidate = workspace_path::resolve_existing_allow_absolute(root, worktree)?;
    let worktree_repo = find_repo_root(&candidate, root).await.map_err(|_| {
        ApiError::BadRequest(format!(
            "worktree {worktree:?} is not inside a git repo under the workspace"
        ))
    })?;
    let session_common = git_common_dir(repo).await?;
    let worktree_common = git_common_dir(&worktree_repo).await?;
    if session_common != worktree_common {
        return Err(ApiError::BadRequest(format!(
            "worktree {worktree:?} is not a worktree of the session's repo"
        )));
    }
    Ok(worktree_repo)
}

/// Whether `repo` is the repo's main checkout rather than a linked worktree:
/// its own git dir is the common one.
async fn is_main_checkout(repo: &Path) -> Result<bool> {
    let own = PathBuf::from(
        run_git(repo, &["rev-parse", "--absolute-git-dir"])
            .await?
            .trim(),
    );
    let own = own.canonicalize().unwrap_or(own);
    Ok(own == git_common_dir(repo).await?)
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

/// The repo the session works in, with the session's role: its directory,
/// walked up to a git root inside the workspace. A session the engine does not
/// know is a 404; one whose directory is not a repo is a 400 naming the
/// directory.
async fn session_repo(state: &Arc<AppState>, id: &str) -> Result<(PathBuf, SessionRole)> {
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
    let repo = find_repo_root(&cwd, &state.config.workspace_root)
        .await
        .map_err(|_| {
            ApiError::BadRequest(format!(
                "session {id} works in {}, which is not inside a git repo",
                summary.cwd
            ))
        })?;
    Ok((repo, summary.role))
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

impl GitOpReq {
    /// The op as the wire names it, for audit lines and refusals.
    fn name(&self) -> &'static str {
        match self {
            GitOpReq::Stage { .. } => "stage",
            GitOpReq::Unstage { .. } => "unstage",
            GitOpReq::Discard { .. } => "discard",
            GitOpReq::Commit { .. } => "commit",
            GitOpReq::Checkout { .. } => "checkout",
        }
    }

    /// The session a commit is made on behalf of, when it is one (C14).
    fn on_behalf_of(&self) -> Option<&str> {
        match self {
            GitOpReq::Commit {
                session: Some(session),
                ..
            } => Some(session.trim()).filter(|s| !s.is_empty()),
            _ => None,
        }
    }
}

/// Who may run a git op here (WI-1255).
///
/// `GitWrite` alone decided this, and every `work.write` token holds it — so
/// every driven session could commit, check out or discard in any repo under
/// the workspace, and commit for any other session without the oversight
/// check `session.commit_for` makes in the core. Now:
///
/// - a person (a core actor of kind `human`) and the break-glass operator
///   token run any op: they are who the GUI's Git panel serves;
/// - vogt-core's own credential runs only a commit for a session — the core's
///   `session.commit_for`, which has already decided its principal may;
/// - any other caller is a session's token, and may only commit for another
///   session, and only when its own engine role is `oversight` — the rule
///   `session.commit_for` applies, so calling the engine directly gains
///   nothing over calling the core.
///
/// Decided by the flags the authentication gate set, never by `name` alone
/// (a core actor's `identity_ref` can be any string): `name` is only read to
/// find which session an agent token belongs to, after the flags have ruled
/// out a person.
fn authorize_op(state: &AppState, identity: &AuthorizedIdentity, req: &GitOpReq) -> Result<()> {
    if identity.person || identity.break_glass {
        return Ok(());
    }
    let Some(target) = req.on_behalf_of() else {
        return Err(ApiError::Forbidden(format!(
            "{} may not run git {} through the engine; only a person \
             uses the Git panel. A session commits a worker's staged work with \
             session_commit_for, from an oversight session",
            identity.name,
            req.name()
        )));
    };
    if identity.stack_secret {
        return Ok(());
    }
    let Some(caller) = caller_session(state, &identity.name) else {
        return Err(ApiError::Forbidden(format!(
            "only a person, or an oversight session, commits for a \
             session ({} is neither)",
            identity.name
        )));
    };
    if target.parse::<uuid::Uuid>().ok() == Some(caller) {
        return Err(ApiError::Forbidden(format!(
            "a session does not commit its own work; an oversight \
             session, or a person, commits for it ({caller})"
        )));
    }
    let role = state.sessions.summary_of(caller).map(|s| s.role);
    if role != Some(SessionRole::Oversight) {
        let role = role.map_or("unknown", |r| r.as_str());
        return Err(ApiError::Forbidden(format!(
            "only an oversight session commits for another session \
             ({caller} is {role})"
        )));
    }
    Ok(())
}

/// The engine session an agent token belongs to. A session vogt-core started
/// is `agent:session:<ses_…>`, found through the `VOGT_SESSION_ID` its record
/// carries, and only when exactly one session carries it; one the engine
/// started is `agent:engine:<uuid>`, and that uuid is the engine's id.
fn caller_session(state: &AppState, name: &str) -> Option<uuid::Uuid> {
    if let Some(id) = name.strip_prefix("agent:engine:") {
        return id.parse().ok().filter(|id| state.sessions.knows(*id));
    }
    let vogt_id = name.strip_prefix("agent:session:")?;
    match state
        .sessions
        .engine_ids_for_vogt_session(vogt_id)
        .as_slice()
    {
        [only] => Some(*only),
        _ => None,
    }
}

pub async fn operate(
    State(state): State<Arc<AppState>>,
    identity: Option<axum::Extension<AuthorizedIdentity>>,
    Json(req): Json<GitOpReq>,
) -> Result<Json<GitOpResp>> {
    let Some(axum::Extension(identity)) = identity else {
        return Err(ApiError::Unauthorized);
    };
    if let Err(refused) = authorize_op(&state, &identity, &req) {
        tracing::warn!(
            target: "vogt::audit",
            event = "git.op",
            outcome = "refused",
            caller = %identity.name,
            op = req.name(),
            session = req.on_behalf_of().unwrap_or("-"),
            reason = %refused,
            "git op refused"
        );
        return Err(refused);
    }
    if let Some(target) = req.on_behalf_of() {
        tracing::info!(
            target: "vogt::audit",
            event = "git.op",
            outcome = "allowed",
            caller = %identity.name,
            op = req.name(),
            session = target,
            "commit for a session allowed"
        );
    }
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

    /// Process environment is process-global. Worktree-token tests take this
    /// lock so they do not clobber each other or a caller's token.
    struct EnvLock;
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    impl EnvLock {
        fn acquire() -> std::sync::MutexGuard<'static, ()> {
            ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner())
        }
    }

    /// Saves and restores the token variables a test touches.
    struct TokenEnv {
        saved: Vec<(&'static str, Option<String>)>,
    }

    impl TokenEnv {
        fn apply(pairs: &[(&'static str, Option<&str>)]) -> Self {
            let saved = pairs
                .iter()
                .map(|(key, _)| (*key, std::env::var(key).ok()))
                .collect();
            for (key, value) in pairs {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
            Self { saved }
        }

        fn clear() -> Self {
            Self::apply(&[
                ("ENGINE_WORKTREE_GITHUB_TOKEN", None),
                ("GH_TOKEN", None),
                ("GITHUB_TOKEN", None),
                ("GIT_AUTH_TOKEN", None),
                ("ENGINE_WORKTREE_ASKPASS", None),
            ])
        }
    }

    impl Drop for TokenEnv {
        fn drop(&mut self) {
            for (key, value) in &self.saved {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
    }

    fn repo_askpass() -> std::path::PathBuf {
        let mut path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        path.push("../deploy/git-askpass.sh");
        path
    }

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
        git(
            repo,
            &[
                "remote",
                "add",
                "origin",
                &origin.path().display().to_string(),
            ],
        );
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
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-b",
                "side",
                &linked.display().to_string(),
            ],
        );
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

    /// A committed checkout whose local `topic` lags `origin/topic`,
    /// plus one uncommitted edit (WI-1298, WI-1630). The bare origin is a
    /// sibling, so the fetch stays on disk.
    fn dirty_checkout(tmp: &tempfile::TempDir) -> std::path::PathBuf {
        let origin = tmp.path().join("origin.git");
        git(
            tmp.path(),
            &[
                "init",
                "--bare",
                "-b",
                "main",
                &origin.display().to_string(),
            ],
        );
        let repo = tmp.path().join("repo");
        git(
            tmp.path(),
            &[
                "clone",
                "--quiet",
                &origin.display().to_string(),
                &repo.display().to_string(),
            ],
        );
        git(&repo, &["config", "user.email", "t@example"]);
        git(&repo, &["config", "user.name", "t"]);
        std::fs::write(repo.join("f.txt"), "base\n").unwrap();
        git(&repo, &["add", "f.txt"]);
        git(&repo, &["commit", "-m", "base"]);
        git(&repo, &["branch", "topic"]);
        git(&repo, &["push", "--quiet", "origin", "main", "topic"]);
        // A slashed base, the shape of `integrate/vogt-next`. Its first component
        // is not itself a branch: git refuses a ref that would be both a file
        // and a directory. Local lags origin.
        git(&repo, &["branch", "lines/codex-next"]);
        git(&repo, &["push", "--quiet", "origin", "lines/codex-next"]);
        // The remote moves on; the local ref of the base is left behind.
        std::fs::write(repo.join("f.txt"), "remote\n").unwrap();
        git(&repo, &["add", "f.txt"]);
        git(&repo, &["commit", "-m", "remote"]);
        git(&repo, &["push", "--quiet", "origin", "main"]);
        git(&repo, &["branch", "-f", "topic", "HEAD"]);
        git(&repo, &["branch", "-f", "lines/codex-next", "HEAD"]);
        git(
            &repo,
            &["push", "--quiet", "origin", "topic", "lines/codex-next"],
        );
        let remote_tip = super::git_output(&repo, &["rev-parse", "HEAD"]).expect("remote tip");
        git(&repo, &["reset", "--hard", "HEAD~1"]);
        git(&repo, &["branch", "-f", "topic", "HEAD"]);
        git(&repo, &["branch", "-f", "lines/codex-next", "HEAD"]);
        // `reset --hard` on a branch that tracks origin moves the
        // remote-tracking ref with it. Put those back at the pushed tip, so
        // the checkout looks like one whose local base lags origin.
        let remote_tip = remote_tip.trim();
        for tracking in [
            "refs/remotes/origin/main",
            "refs/remotes/origin/topic",
            "refs/remotes/origin/lines/codex-next",
        ] {
            git(&repo, &["update-ref", tracking, remote_tip]);
        }
        std::fs::write(repo.join("f.txt"), "dirty\n").unwrap();
        repo
    }

    #[test]
    fn a_worktree_is_cut_from_the_base_and_leaves_the_dirty_checkout_alone() {
        let _guard = EnvLock::acquire();
        let tmp = tempfile::tempdir().unwrap();
        let repo = dirty_checkout(&tmp);
        let before = std::fs::read_to_string(repo.join("f.txt")).unwrap();

        let worktree = create_worktree(&repo, "wi-1298-worker", "topic").unwrap();

        assert_eq!(worktree, tmp.path().join("wi-1298-worker"));
        assert_eq!(
            std::fs::read_to_string(worktree.join("f.txt")).unwrap(),
            "remote\n",
            "the worktree is cut from origin, not the stale local ref"
        );
        // The checkout the worktree was cut from is byte-for-byte as it was.
        assert_eq!(std::fs::read_to_string(repo.join("f.txt")).unwrap(), before);
        let head = git_output(&worktree, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap();
        assert_eq!(head.trim(), "wi-1298-worker");
    }

    #[test]
    fn a_worktree_is_refused_for_a_bad_name_an_unknown_base_or_a_taken_path() {
        let _guard = EnvLock::acquire();
        let tmp = tempfile::tempdir().unwrap();
        let repo = dirty_checkout(&tmp);

        let bad_name = create_worktree(&repo, "has space", "topic").unwrap_err();
        assert!(
            bad_name.to_string().contains("not a ref name"),
            "{bad_name}"
        );
        let option = create_worktree(&repo, "--upload-pack=evil", "topic").unwrap_err();
        assert!(option.to_string().contains("not a ref name"), "{option}");
        let unknown = create_worktree(&repo, "wi-1", "no-such-ref").unwrap_err();
        assert!(unknown.to_string().contains("could not fetch"), "{unknown}");

        std::fs::create_dir(tmp.path().join("taken")).unwrap();
        let taken = create_worktree(&repo, "taken", "topic").unwrap_err();
        assert!(taken.to_string().contains("already exists"), "{taken}");
        // None of the refusals created a branch or a directory.
        assert!(git_output(&repo, &["rev-parse", "--verify", "wi-1"]).is_err());
        assert!(!tmp.path().join("has space").exists());
    }

    #[test]
    fn an_existing_branch_is_refused_and_a_removed_worktree_takes_its_branch() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = dirty_checkout(&tmp);
        let taken = create_worktree(&repo, "topic", "main").unwrap_err();
        assert!(taken.to_string().contains("already exists"), "{taken}");
        assert!(!tmp.path().join("topic").exists());

        let worktree = create_worktree(&repo, "wi-2-gone", "topic").unwrap();
        remove_worktree(&repo, &worktree, "wi-2-gone");
        assert!(!worktree.exists());
        assert!(git_output(&repo, &["rev-parse", "--verify", "refs/heads/wi-2-gone"]).is_err());
        // The branch it was cut from is not touched.
        assert!(git_output(&repo, &["rev-parse", "--verify", "refs/heads/topic"]).is_ok());
    }

    #[test]
    fn a_worktree_can_be_cut_from_a_worktree() {
        let _guard = EnvLock::acquire();
        let tmp = tempfile::tempdir().unwrap();
        let repo = dirty_checkout(&tmp);
        let first = create_worktree(&repo, "first", "topic").unwrap();
        git(&first, &["push", "--quiet", "-u", "origin", "first"]);
        let second = create_worktree(&first, "second", "first").unwrap();
        assert_eq!(second, tmp.path().join("second"));
        assert_eq!(
            git_output(&second, &["rev-parse", "--abbrev-ref", "HEAD"])
                .unwrap()
                .trim(),
            "second"
        );
    }

    #[test]
    fn a_fetch_failure_refuses_the_worktree_instead_of_cutting_from_the_stale_local_ref() {
        let _guard = EnvLock::acquire();
        let tmp = tempfile::tempdir().unwrap();
        let repo = dirty_checkout(&tmp);
        let local = git_output(&repo, &["rev-parse", "topic"]).unwrap();
        // The remote goes away. The local ref is still there and behind.
        std::fs::remove_dir_all(tmp.path().join("origin.git")).unwrap();

        let failed = create_worktree(&repo, "wi-1630-worker", "topic").unwrap_err();

        assert!(failed.to_string().contains("could not fetch"), "{failed}");
        assert!(!tmp.path().join("wi-1630-worker").exists());
        assert!(git_output(
            &repo,
            &["rev-parse", "--verify", "refs/heads/wi-1630-worker"]
        )
        .is_err());
        // The stale local ref is untouched, and was not what got cut.
        assert_eq!(git_output(&repo, &["rev-parse", "topic"]).unwrap(), local);
    }

    #[test]
    fn github_https_is_the_host_not_a_substring() {
        assert!(origin_url_is_github_https(
            "https://github.com/TheDancingDeveloper-org/example.git"
        ));
        assert!(origin_url_is_github_https(
            "https://x-access-token:secret@github.com/org/example.git"
        ));
        assert!(!origin_url_is_github_https(
            "https://github.com.example.net/org/example.git"
        ));
        assert!(!origin_url_is_github_https(
            "https://notgithub.com/org/example.git"
        ));
        assert!(!origin_url_is_github_https(
            "git@github.com:TheDancingDeveloper-org/example.git"
        ));
        assert!(!origin_url_is_github_https("/tmp/origin.git"));
    }

    #[test]
    fn a_github_origin_without_a_token_names_the_missing_credential() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = dirty_checkout(&tmp);
        git(
            &repo,
            &[
                "remote",
                "set-url",
                "origin",
                "https://github.com/TheDancingDeveloper-org/example.git",
            ],
        );
        let _guard = EnvLock::acquire();
        let _env = TokenEnv::clear();

        let failed = create_worktree(&repo, "wi-1666-worker", "topic").unwrap_err();

        let message = failed.to_string();
        assert!(message.contains("no GitHub token"), "{message}");
        assert!(
            message.contains("ENGINE_WORKTREE_GITHUB_TOKEN"),
            "{message}"
        );
        assert!(
            !message.contains("  "),
            "no collapsed-line padding: {message}"
        );
        assert!(!tmp.path().join("wi-1666-worker").exists());
    }

    #[test]
    fn an_ssh_github_origin_does_not_require_the_token() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = dirty_checkout(&tmp);
        git(
            &repo,
            &[
                "remote",
                "set-url",
                "origin",
                "git@github.com:TheDancingDeveloper-org/example.git",
            ],
        );
        let _guard = EnvLock::acquire();
        let _env = TokenEnv::clear();

        let failed = create_worktree(&repo, "wi-1666-ssh", "topic").unwrap_err();
        let message = failed.to_string();
        assert!(
            !message.contains("no GitHub token"),
            "ssh must not demand the https token: {message}"
        );
        assert!(message.contains("could not fetch"), "{message}");
    }

    #[test]
    fn the_shipped_askpass_prints_the_worktree_token_for_github_only() {
        let tmp = tempfile::tempdir().unwrap();
        let seen = tmp.path().join("seen");
        let askpass = repo_askpass();
        let run = |prompt: &str, extra: &[(&str, &str)]| -> String {
            let mut cmd = std::process::Command::new(&askpass);
            cmd.env_clear();
            cmd.env("PATH", "/usr/bin:/bin");
            cmd.arg(prompt)
                .env("ENGINE_WORKTREE_FETCH_TOKEN", "worktree-token-secret")
                .env("GH_TOKEN", "session-token-should-not-be-used")
                .env("GIT_AUTH_TOKEN", "git-auth-token");
            for (k, v) in extra {
                cmd.env(k, v);
            }
            let out = cmd.output().expect("askpass");
            format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            )
        };
        let password = run("Password for 'https://github.com':", &[]);
        assert_eq!(
            password.trim(),
            "worktree-token-secret",
            "password={password:?} script={}",
            std::fs::read_to_string(&askpass).unwrap()
        );
        let user = run("Username for 'https://github.com':", &[]);
        assert_eq!(user.trim(), "x-access-token");
        let lookalike = run("Password for 'https://github.com.evil.test':", &[]);
        assert_eq!(lookalike.trim(), "git-auth-token");
        let not_github = run("Password for 'https://notgithub.com':", &[]);
        assert_eq!(not_github.trim(), "git-auth-token");
        let _ = seen;
    }

    #[test]
    fn a_github_https_fetch_refuses_when_only_a_lookalike_host_is_configured() {
        // Host matching is exact. A checkout whose origin merely contains the
        // substring github.com is not given the token and is not refused for
        // lacking one.
        assert!(!origin_url_is_github_https(
            "https://github.com.example.net/org/repo.git"
        ));
        assert!(origin_url_is_github_https(
            "https://user:token@github.com/org/repo.git"
        ));
    }

    #[test]
    fn a_slashed_base_is_cut_from_origin_bare_or_origin_prefixed() {
        let _guard = EnvLock::acquire();
        let tmp = tempfile::tempdir().unwrap();
        let repo = dirty_checkout(&tmp);
        let local_before = git_output(&repo, &["rev-parse", "lines/codex-next"]).unwrap();

        for (branch, base) in [
            ("wi-bare", "lines/codex-next"),
            ("wi-prefixed", "origin/lines/codex-next"),
        ] {
            let worktree = create_worktree(&repo, branch, base).unwrap();
            let remote = git_output(&repo, &["rev-parse", "origin/lines/codex-next"]).unwrap();
            assert_ne!(
                local_before, remote,
                "the fixture's local slashed base lags origin"
            );
            assert_eq!(
                std::fs::read_to_string(worktree.join("f.txt")).unwrap(),
                "remote\n",
                "{base} cuts from origin, not the stale local ref"
            );
            assert_eq!(
                git_output(&worktree, &["rev-parse", "HEAD"]).unwrap(),
                remote,
                "{base}"
            );
            // The fetch moved the remote-tracking ref only.
            assert_eq!(
                git_output(&repo, &["rev-parse", "lines/codex-next"]).unwrap(),
                local_before,
                "{base} left the local branch where it was"
            );
        }

        // A slash is a branch component, not a refusal — except a name that is
        // not a ref (`origin/` alone, an empty component). Both are refused
        // before any fetch, and neither leaves a branch or a directory.
        let bare = create_worktree(&repo, "wi-bad", "origin/").unwrap_err();
        assert!(bare.to_string().contains("not a ref name"), "{bare}");
        let empty = create_worktree(&repo, "wi-bad", "topic//next").unwrap_err();
        assert!(empty.to_string().contains("not a ref name"), "{empty}");
        assert!(!tmp.path().join("wi-bad").exists());
    }
}
