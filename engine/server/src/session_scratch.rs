//! What a session leaves outside its worktree (WI-1810).
//!
//! Claude Code gives each session a scratchpad at
//! `<tmp>/claude-<uid>/<cwd slug>/<session id>/`, and the engine pins the
//! session id to its own. Nothing deleted these: merge-train and reviewer
//! sessions built a scratch worktree with its own Cargo `target` in each one,
//! and on 2026-10-18 they held 292 GB and filled Node B's disk.
//!
//! A stop or remove deletes the session's scratch directories. A hibernation
//! keeps them (the session is woken to carry on) but drops each Cargo
//! `target` in them and any top-level entry larger than a limit.
//!
//! Also the session's escaped background processes: anything still running
//! with the session's `VOGT_ENGINE_SESSION_ID` after its process tree was
//! stopped (a `setsid` or `nohup` job leaves the tree but keeps the
//! environment), and the free-space check a new session must pass.

use std::path::{Path, PathBuf};

use uuid::Uuid;

/// What one cleanup removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Removed {
    pub path: PathBuf,
    pub bytes: u64,
}

/// The `claude-<uid>` directories a session's scratchpad can live under: the
/// engine's temp dir and `/tmp`, once each.
pub fn roots() -> Vec<PathBuf> {
    #[cfg(unix)]
    let uid = unsafe { libc::getuid() };
    #[cfg(not(unix))]
    let uid = 0u32;
    let name = format!("claude-{uid}");
    let mut out = vec![std::env::temp_dir().join(&name)];
    let tmp = Path::new("/tmp").join(&name);
    if !out.contains(&tmp) {
        out.push(tmp);
    }
    out
}

/// Every scratch directory of session `id` under `roots`: a directory named
/// for the id one level below a root. A symlink is never one.
pub fn dirs(roots: &[PathBuf], id: Uuid) -> Vec<PathBuf> {
    let id = id.to_string();
    let mut out = Vec::new();
    for root in roots {
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten() {
            let candidate = entry.path().join(&id);
            if std::fs::symlink_metadata(&candidate).is_ok_and(|m| m.is_dir()) {
                out.push(candidate);
            }
        }
    }
    out
}

/// Delete session `id`'s scratch directories. For a stopped session.
pub fn remove(roots: &[PathBuf], id: Uuid) -> Vec<Removed> {
    let mut out = Vec::new();
    for dir in dirs(roots, id) {
        let repos = worktree_repos(&dir);
        let bytes = dir_bytes(&dir);
        if std::fs::remove_dir_all(&dir).is_ok() {
            out.push(Removed { path: dir, bytes });
        }
        for repo in repos {
            git_worktree_prune(&repo);
        }
    }
    out
}

/// The main repositories of the linked worktrees under `dir`, at most four
/// levels down, so a deleted scratch worktree's record can be pruned.
fn worktree_repos(dir: &Path) -> Vec<PathBuf> {
    let mut repos = Vec::new();
    let mut stack = vec![(dir.to_path_buf(), 0u8)];
    while let Some((d, depth)) = stack.pop() {
        let git = d.join(".git");
        if std::fs::symlink_metadata(&git).is_ok_and(|m| m.is_file()) {
            if let Some(main) = std::fs::read_to_string(&git).ok().and_then(|t| {
                let gitdir = t.lines().find_map(|l| l.trim().strip_prefix("gitdir:"))?;
                Some(
                    d.join(gitdir.trim())
                        .parent()?
                        .parent()?
                        .parent()?
                        .to_path_buf(),
                )
            }) {
                if !repos.contains(&main) {
                    repos.push(main);
                }
            }
            continue;
        }
        if depth >= 4 {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            if std::fs::symlink_metadata(e.path()).is_ok_and(|m| m.is_dir()) {
                stack.push((e.path(), depth + 1));
            }
        }
    }
    repos
}

fn git_worktree_prune(repo: &Path) {
    let _ = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["worktree", "prune"])
        .output();
}

/// Claude Code's directory name for a session started in `cwd`: every
/// character other than an ASCII letter or digit becomes `-`.
pub fn cwd_slug(cwd: &Path) -> String {
    cwd.to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// Point scratch Cargo builds of a session in `cwd` at one shared target per
/// repository (WI-1810): write `<root>/<slug>/.cargo/config.toml` with
/// `build.target-dir = <shared>/<repo>`. Cargo reads it for any build under
/// the session's scratch directory and for nothing else, so the session's
/// own worktree keeps its own target and concurrent workers never wait on
/// one lock; scratch builds of one repo share it (and its lock).
/// A repository's own `.cargo/config.toml` or a `CARGO_TARGET_DIR` wins.
pub fn write_scratch_cargo_config(root: &Path, cwd: &Path, shared: &Path) -> Option<PathBuf> {
    let repo = repo_name(cwd)?;
    let dir = root.join(cwd_slug(cwd)).join(".cargo");
    std::fs::create_dir_all(&dir).ok()?;
    let target = shared.join(repo);
    let path = dir.join("config.toml");
    let body = format!(
        "# Written by the Vogt engine (WI-1810): scratch builds share one target.\n\
         [build]\ntarget-dir = {:?}\n",
        target.to_string_lossy()
    );
    std::fs::write(&path, body).ok()?;
    Some(path)
}

/// Remove each repository target under `shared` larger than `limit` bytes;
/// the next scratch build starts it again.
pub fn cap_shared_targets(shared: &Path, limit: u64) -> Vec<Removed> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(shared) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_dir()) {
            continue;
        }
        let bytes = dir_bytes(&path);
        if bytes > limit && std::fs::remove_dir_all(&path).is_ok() {
            out.push(Removed { path, bytes });
        }
    }
    out
}

/// Shrink session `id`'s scratch directories for a hibernation: remove every
/// Cargo `target` (a `target` directory beside a `Cargo.toml`, or one holding
/// Cargo's `CACHEDIR.TAG`), then every top-level entry still larger than
/// `limit` bytes. Symlinks are never followed.
pub fn prune(roots: &[PathBuf], id: Uuid, limit: u64) -> Vec<Removed> {
    let mut out = Vec::new();
    for dir in dirs(roots, id) {
        let repos = worktree_repos(&dir);
        out.extend(remove_cargo_targets(&dir));
        // Claude Code keeps everything under `<id>/scratchpad/`, so its
        // children are the entries weighed, not `scratchpad/` as a whole.
        let mut candidates = Vec::new();
        for parent in [dir.clone(), dir.join("scratchpad")] {
            let Ok(entries) = std::fs::read_dir(&parent) else {
                continue;
            };
            candidates.extend(
                entries
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| p.file_name().is_none_or(|n| n != "scratchpad")),
            );
        }
        for path in candidates {
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            let bytes = if meta.is_dir() {
                dir_bytes(&path)
            } else {
                meta.len()
            };
            if bytes <= limit {
                continue;
            }
            let gone = if meta.is_dir() {
                std::fs::remove_dir_all(&path)
            } else {
                std::fs::remove_file(&path)
            };
            if gone.is_ok() {
                out.push(Removed { path, bytes });
            }
        }
        for repo in repos {
            git_worktree_prune(&repo);
        }
    }
    out
}

fn remove_cargo_targets(root: &Path) -> Vec<Removed> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    let mut seen = 0u32;
    while let Some(dir) = stack.pop() {
        seen += 1;
        if seen > 100_000 {
            break;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if !meta.is_dir() {
                continue;
            }
            let cargo = path.file_name().is_some_and(|n| n == "target")
                && (dir.join("Cargo.toml").is_file() || path.join("CACHEDIR.TAG").is_file());
            if cargo {
                let bytes = dir_bytes(&path);
                if std::fs::remove_dir_all(&path).is_ok() {
                    out.push(Removed { path, bytes });
                }
            } else if path.file_name().is_none_or(|n| n != ".git") {
                stack.push(path);
            }
        }
    }
    out
}

fn dir_bytes(path: &Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(meta) = std::fs::symlink_metadata(entry.path()) else {
                continue;
            };
            if meta.is_dir() {
                stack.push(entry.path());
            } else {
                total = total.saturating_add(meta.len());
            }
        }
    }
    total
}

/// Processes of this user, other than the engine, whose environment names
/// session `id` (`VOGT_ENGINE_SESSION_ID`). Empty off Linux.
pub fn strays(id: Uuid) -> Vec<u32> {
    #[cfg(target_os = "linux")]
    {
        let me = std::process::id();
        let needle = format!("VOGT_ENGINE_SESSION_ID={id}");
        let Ok(entries) = std::fs::read_dir("/proc") else {
            return Vec::new();
        };
        entries
            .flatten()
            .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
            .filter(|&pid| pid != me && crate::hibernation::running(pid))
            .filter(|pid| {
                // /proc/<pid>/environ of another user is unreadable, so this
                // only ever finds the engine's own user's processes.
                std::fs::read(format!("/proc/{pid}/environ"))
                    .is_ok_and(|env| env.split(|&b| b == 0).any(|var| var == needle.as_bytes()))
            })
            .collect()
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = id;
        Vec::new()
    }
}

/// `SIGKILL` every stray of session `id`; the pids signalled.
pub fn kill_strays(id: Uuid) -> Vec<u32> {
    let pids = strays(id);
    #[cfg(unix)]
    for &pid in &pids {
        unsafe {
            libc::kill(pid as i32, libc::SIGKILL);
        }
    }
    pids
}

/// The name of the repository `cwd` is in: the main checkout's directory
/// name, for a linked worktree too. `None` outside a git checkout.
pub fn repo_name(cwd: &Path) -> Option<String> {
    let mut dir = Some(cwd);
    while let Some(d) = dir {
        let git = d.join(".git");
        if let Ok(meta) = std::fs::symlink_metadata(&git) {
            let main = if meta.is_dir() {
                d.to_path_buf()
            } else {
                // `gitdir: <main>/.git/worktrees/<name>`
                let text = std::fs::read_to_string(&git).ok()?;
                let gitdir = text
                    .lines()
                    .find_map(|l| l.trim().strip_prefix("gitdir:"))?;
                let gitdir = d.join(gitdir.trim());
                gitdir.parent()?.parent()?.parent()?.to_path_buf()
            };
            return main
                .file_name()?
                .to_str()
                .filter(|n| !n.is_empty() && *n != "." && *n != "..")
                .map(str::to_string);
        }
        dir = d.parent();
    }
    None
}

/// The first of `paths` whose volume has less than `min_free_percent` free,
/// with its free percent. `None` when every readable volume has room, or the
/// guard is off (`0`).
pub fn short_of_space(paths: &[PathBuf], min_free_percent: u8) -> Option<(PathBuf, u8)> {
    if min_free_percent == 0 {
        return None;
    }
    paths.iter().find_map(|p| {
        let usage = crate::worktree_clean::volume_usage(p)?;
        let free = 100u8.saturating_sub(usage.used_percent);
        (free < min_free_percent).then(|| (p.clone(), free))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session_dir(root: &Path, id: Uuid) -> PathBuf {
        let dir = root.join("-home-x-repo").join(id.to_string());
        std::fs::create_dir_all(dir.join("scratchpad")).unwrap();
        dir
    }

    #[test]
    fn remove_deletes_only_the_sessions_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let roots = vec![tmp.path().to_path_buf()];
        let id = Uuid::new_v4();
        let other = Uuid::new_v4();
        let mine = session_dir(tmp.path(), id);
        std::fs::write(mine.join("scratchpad").join("f"), "1234").unwrap();
        let theirs = session_dir(tmp.path(), other);
        // A symlink named for the session is not its directory.
        let linked = tmp.path().join("-elsewhere");
        std::fs::create_dir(&linked).unwrap();
        std::os::unix::fs::symlink(&theirs, linked.join(id.to_string())).unwrap();

        assert_eq!(dirs(&roots, id), vec![mine.clone()]);
        let removed = remove(&roots, id);
        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0].bytes, 4);
        assert!(!mine.exists());
        assert!(theirs.exists());
        assert!(remove(&[tmp.path().join("missing")], id).is_empty());
    }

    #[test]
    fn prune_drops_cargo_targets_and_large_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let roots = vec![tmp.path().to_path_buf()];
        let id = Uuid::new_v4();
        let dir = session_dir(tmp.path(), id);
        let pad = dir.join("scratchpad");
        // A scratch worktree's target, and a CARGO_TARGET_DIR elsewhere.
        let wt = pad.join("wt");
        std::fs::create_dir_all(wt.join("target").join("debug")).unwrap();
        std::fs::write(wt.join("Cargo.toml"), "").unwrap();
        std::fs::write(wt.join("target").join("debug").join("bin"), "b").unwrap();
        let tagged = pad.join("t").join("target");
        std::fs::create_dir_all(&tagged).unwrap();
        std::fs::write(tagged.join("CACHEDIR.TAG"), "").unwrap();
        // A `target` that is not Cargo's stays.
        let plain = pad.join("notes").join("target");
        std::fs::create_dir_all(&plain).unwrap();
        std::fs::write(plain.join("n"), "n").unwrap();
        // A large entry inside scratchpad/ goes on its own; the rest stays.
        std::fs::write(pad.join("download.bin"), vec![0u8; 2048]).unwrap();
        // A large top-level entry goes, a small one stays.
        std::fs::write(dir.join("big.bin"), vec![0u8; 2048]).unwrap();
        std::fs::write(dir.join("small.txt"), "s").unwrap();

        let removed = prune(&roots, id, 1024);
        assert!(!wt.join("target").exists());
        assert!(!tagged.exists());
        assert!(plain.join("n").exists());
        assert!(!dir.join("big.bin").exists());
        assert!(dir.join("small.txt").exists());
        assert!(wt.join("Cargo.toml").exists());
        assert!(!pad.join("download.bin").exists());
        assert!(pad.exists(), "scratchpad/ itself is kept");
        assert_eq!(removed.len(), 4);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn strays_are_found_by_session_id() {
        let id = Uuid::new_v4();
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .env("VOGT_ENGINE_SESSION_ID", id.to_string())
            .spawn()
            .unwrap();
        assert_eq!(strays(id), vec![child.id()]);
        assert!(strays(Uuid::new_v4()).is_empty());
        assert_eq!(kill_strays(id), vec![child.id()]);
        child.wait().unwrap();
        assert!(strays(id).is_empty());
    }

    #[test]
    fn repo_name_names_the_main_checkout() {
        let tmp = tempfile::tempdir().unwrap();
        let main = tmp.path().join("vogt");
        std::fs::create_dir_all(main.join(".git").join("worktrees").join("wt")).unwrap();
        std::fs::create_dir_all(main.join("engine")).unwrap();
        assert_eq!(repo_name(&main.join("engine")).as_deref(), Some("vogt"));
        let wt = tmp.path().join("WI-1-x");
        std::fs::create_dir(&wt).unwrap();
        std::fs::write(
            wt.join(".git"),
            format!("gitdir: {}\n", main.join(".git/worktrees/wt").display()),
        )
        .unwrap();
        assert_eq!(repo_name(&wt).as_deref(), Some("vogt"));
        assert_eq!(repo_name(Path::new("/")), None);
    }

    #[test]
    fn scratch_cargo_config_and_cap() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("vogt");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let root = tmp.path().join("claude-1");
        let shared = tmp.path().join("shared");
        let path = write_scratch_cargo_config(&root, &repo, &shared).unwrap();
        assert_eq!(path, root.join(cwd_slug(&repo)).join(".cargo/config.toml"));
        let text = std::fs::read_to_string(&path).unwrap();
        let parsed: toml::Table = toml::from_str(&text).unwrap();
        assert_eq!(
            parsed["build"]["target-dir"].as_str().unwrap(),
            shared.join("vogt").to_str().unwrap()
        );
        assert!(write_scratch_cargo_config(&root, Path::new("/"), &shared).is_none());
        assert_eq!(cwd_slug(Path::new("/a/b_c.d")), "-a-b-c-d");

        std::fs::create_dir_all(shared.join("vogt")).unwrap();
        std::fs::write(shared.join("vogt").join("x"), vec![0u8; 100]).unwrap();
        std::fs::create_dir_all(shared.join("small")).unwrap();
        assert_eq!(cap_shared_targets(&shared, 50).len(), 1);
        assert!(!shared.join("vogt").exists());
        assert!(shared.join("small").exists());
    }

    #[test]
    fn remove_prunes_the_worktree_record() {
        let tmp = tempfile::tempdir().unwrap();
        let main = tmp.path().join("repo");
        std::fs::create_dir(&main).unwrap();
        let git = |dir: &Path, args: &[&str]| {
            assert!(std::process::Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(args)
                .status()
                .unwrap()
                .success());
        };
        git(&main, &["init", "-q", "-b", "main"]);
        git(
            &main,
            &[
                "-c",
                "user.email=t@e",
                "-c",
                "user.name=t",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "i",
            ],
        );
        let id = Uuid::new_v4();
        let roots = vec![tmp.path().join("claude")];
        let dir = session_dir(&roots[0], id);
        let wt = dir.join("scratchpad").join("wt");
        git(&main, &["worktree", "add", "-q", wt.to_str().unwrap()]);
        assert!(main.join(".git/worktrees/wt").exists());
        remove(&roots, id);
        assert!(!dir.exists());
        assert!(!main.join(".git/worktrees/wt").exists());
    }

    #[test]
    fn short_of_space_is_off_at_zero_and_trips_at_100() {
        let here = vec![PathBuf::from(".")];
        assert!(short_of_space(&here, 0).is_none());
        // 100 needs a completely empty volume.
        assert!(short_of_space(&here, 100).is_some());
        assert!(short_of_space(&[PathBuf::from("/no/such/path")], 50).is_none());
    }
}
