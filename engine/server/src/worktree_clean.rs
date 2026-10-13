//! Regenerable build output in a session's linked git worktree (WI-1304).
//!
//! A finished worker's `target/`, `node_modules` and `.venv` are what filled
//! the host disk. When `ENGINE_WORKTREE_CLEAN_ON_HIBERNATE` is on, hibernating
//! or stopping a session whose directory is a *linked* worktree removes those
//! directories. A repo's main checkout is never touched, nothing outside the
//! worktree is, and a tracked file is never deleted.
//!
//! Also the workspace volume's usage, which sweep and status report, with a
//! needs-attention flag once used space passes a threshold (85% by default).

use std::path::{Path, PathBuf};
use std::process::Command;

/// What one cleanup removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cleaned {
    pub path: PathBuf,
    pub bytes: u64,
}

/// One volume's usage, as `statvfs` reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VolumeUsage {
    pub total_bytes: u64,
    pub free_bytes: u64,
    /// Whole percent used, 0 to 100.
    pub used_percent: u8,
}

/// `path` when it is a linked git worktree — one `git worktree add` created,
/// whose `.git` is a file pointing at the main repo — and not a main checkout.
///
/// A main checkout's `.git` is a directory, so it is excluded by that test
/// alone. A path that is not a checkout, or whose git dir cannot be read, is
/// not a linked worktree either.
pub fn linked_worktree(cwd: &Path) -> Option<PathBuf> {
    let git = cwd.join(".git");
    let meta = std::fs::symlink_metadata(&git).ok()?;
    if !meta.is_file() {
        return None;
    }
    let text = std::fs::read_to_string(&git).ok()?;
    let gitdir = text
        .lines()
        .find_map(|line| line.trim().strip_prefix("gitdir:"))?;
    let gitdir = cwd.join(gitdir.trim());
    // `gitdir` of a linked worktree lives at `<main>/.git/worktrees/<name>`.
    let worktrees = gitdir.parent()?.file_name()?.to_str()?;
    if worktrees != "worktrees" {
        return None;
    }
    Some(std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf()))
}

/// Remove regenerable build output under `cwd` when it is a linked worktree.
///
/// Returns what was removed. An empty list means the directory was not a
/// linked worktree, or it held nothing regenerable. A directory that is a
/// symlink, or whose canonical path escapes the worktree, is left alone.
pub fn clean(cwd: &Path) -> Vec<Cleaned> {
    let Some(root) = linked_worktree(cwd) else {
        return Vec::new();
    };
    let mut removed = Vec::new();
    let mut stack = vec![root.clone()];
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
            if meta.file_type().is_symlink() {
                // A symlink could point anywhere, including out of the
                // worktree or at a tracked file. Never follow one.
                continue;
            }
            let Ok(canonical) = std::fs::canonicalize(&path) else {
                continue;
            };
            if !canonical.starts_with(&root) {
                continue;
            }
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if meta.is_dir() && matches!(name, "target" | "node_modules" | ".venv") {
                if name == "target" && !dir.join("Cargo.toml").is_file() {
                    stack.push(path);
                    continue;
                }
                if tracked(&root, &canonical) {
                    continue;
                }
                let bytes = dir_bytes(&canonical);
                if std::fs::remove_dir_all(&canonical).is_ok() {
                    tracing::info!(
                        path = %canonical.display(),
                        bytes,
                        "removed regenerable build output from a linked worktree"
                    );
                    removed.push(Cleaned {
                        path: canonical,
                        bytes,
                    });
                }
            } else if meta.is_dir() {
                stack.push(path);
            }
        }
    }
    removed
}

/// Whether `path` is tracked in the checkout at `root`. A git failure is
/// treated as tracked: when the answer is uncertain, nothing is deleted.
fn tracked(root: &Path, path: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(root) else {
        return true;
    };
    Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["ls-files", "--error-unmatch", "--"])
        .arg(relative)
        .output()
        .map(|out| out.status.success())
        .unwrap_or(true)
}

fn dir_bytes(path: &Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let child = entry.path();
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                stack.push(child);
            } else {
                total = total.saturating_add(meta.len());
            }
        }
    }
    total
}

/// Usage of the volume `path` lives on, or `None` when it cannot be read.
pub fn volume_usage(path: &Path) -> Option<VolumeUsage> {
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    let c_path = std::ffi::CString::new(path.to_string_lossy().as_bytes()).ok()?;
    if unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) } != 0 {
        return None;
    }
    let block = stat.f_frsize as u64;
    let total = (stat.f_blocks as u64).saturating_mul(block);
    let free = (stat.f_bavail as u64).saturating_mul(block);
    let used = total.saturating_sub(free);
    let percent = u8::try_from(
        (used.saturating_mul(100))
            .checked_div(total)
            .unwrap_or(0)
            .min(100),
    )
    .unwrap_or(100);
    Some(VolumeUsage {
        total_bytes: total,
        free_bytes: free,
        used_percent: percent,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(dir: &Path) {
        for args in [
            &["init", "-q", "-b", "main"][..],
            &["config", "user.email", "t@example.com"],
            &["config", "user.name", "t"],
        ] {
            let status = Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(args)
                .status()
                .unwrap();
            assert!(status.success());
        }
        std::fs::write(dir.join("README"), "r").unwrap();
        for args in [&["add", "README"][..], &["commit", "-qm", "init"]] {
            let status = Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(args)
                .status()
                .unwrap();
            assert!(status.success());
        }
    }

    #[test]
    fn cleans_a_linked_worktree_and_nothing_else() {
        let tmp = tempfile::tempdir().unwrap();
        let main = tmp.path().join("repo");
        std::fs::create_dir(&main).unwrap();
        repo(&main);

        // The main checkout's own build output is never touched.
        let main_target = main.join("target");
        std::fs::create_dir(&main_target).unwrap();
        std::fs::write(main_target.join("out"), "keep").unwrap();

        let linked = tmp.path().join("linked");
        let status = Command::new("git")
            .arg("-C")
            .arg(&main)
            .args(["worktree", "add", "-q"])
            .arg(&linked)
            .status()
            .unwrap();
        assert!(status.success());
        assert!(linked_worktree(&linked).is_some());
        assert!(linked_worktree(&main).is_none());

        // A Cargo target, a node_modules, a .venv, and a target that is not
        // next to a Cargo.toml (left alone).
        std::fs::write(linked.join("Cargo.toml"), "[package]\nname=\"t\"\n").unwrap();
        let target = linked.join("target");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("out"), "built").unwrap();
        let modules = linked.join("web").join("node_modules");
        std::fs::create_dir_all(&modules).unwrap();
        std::fs::write(modules.join("pkg"), "js").unwrap();
        let venv = linked.join(".venv");
        std::fs::create_dir(&venv).unwrap();
        std::fs::write(venv.join("py"), "py").unwrap();
        let stray = linked.join("notes").join("target");
        std::fs::create_dir_all(&stray).unwrap();
        std::fs::write(stray.join("note"), "kept").unwrap();
        // A tracked file named like build output is not deleted.
        std::fs::create_dir(linked.join("tracked")).unwrap();
        std::fs::write(linked.join("tracked").join("Cargo.toml"), "").unwrap();
        std::fs::create_dir(linked.join("tracked").join("target")).unwrap();
        std::fs::write(linked.join("tracked").join("target").join("src"), "s").unwrap();
        let status = Command::new("git")
            .arg("-C")
            .arg(&linked)
            .args(["add", "tracked", "Cargo.toml"])
            .status()
            .unwrap();
        assert!(status.success());

        // A symlink escaping the worktree, named like build output.
        std::os::unix::fs::symlink(&main_target, linked.join("node_modules_link")).ok();
        let escape = linked.join("escape");
        std::fs::create_dir(&escape).unwrap();
        std::os::unix::fs::symlink(&main_target, escape.join("node_modules")).unwrap();

        let removed = clean(&linked);
        let removed_names: Vec<_> = removed
            .iter()
            .map(|c| c.path.file_name().unwrap().to_str().unwrap().to_string())
            .collect();
        assert!(removed_names.contains(&"target".to_string()));
        assert!(removed_names.contains(&"node_modules".to_string()));
        assert!(removed_names.contains(&".venv".to_string()));
        assert!(!target.exists());
        assert!(!modules.exists());
        assert!(!venv.exists());
        assert!(stray.join("note").exists(), "no Cargo.toml beside it");
        assert!(
            linked.join("tracked").join("target").join("src").exists(),
            "tracked files stay"
        );
        assert!(main_target.join("out").exists(), "the main checkout stays");
        assert!(
            escape.join("node_modules").exists(),
            "a symlink is never followed"
        );

        assert!(clean(&main).is_empty());
    }

    #[test]
    fn volume_usage_reads_a_real_directory() {
        let usage = volume_usage(Path::new(".")).unwrap();
        assert!(usage.total_bytes > 0);
        assert!(usage.used_percent <= 100);
    }
}
