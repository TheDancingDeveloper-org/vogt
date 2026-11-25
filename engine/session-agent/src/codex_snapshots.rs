//! Scrub Codex shell snapshots (WI-4051).
//!
//! Codex's `shell_snapshot` feature writes the shell environment, exported
//! variables included, to `$CODEX_HOME/shell_snapshots/<id>.sh`. For a Vogt
//! session that means the session's own tokens in a file that outlives it. The
//! launch turns the feature off ([`crate::codex_args`]); this removes what an
//! earlier launch already left behind. Best effort: it never follows a
//! symlink, removes regular files only, and reports a count, never a name or
//! any content.

use std::path::Path;

/// The directory under `$CODEX_HOME` Codex writes snapshots to.
pub const SHELL_SNAPSHOT_DIR: &str = "shell_snapshots";

/// Remove the `*.sh` regular files directly inside `<codex_home>/shell_snapshots`.
/// Returns how many were removed. Nothing is touched unless `codex_home` is an
/// absolute path: an empty or relative home would resolve `shell_snapshots`
/// against the current directory, which is the workspace. A missing directory,
/// a symlinked directory, any entry that is not a regular file (symlink,
/// subdirectory) and any name not ending in `.sh` are left alone.
pub fn scrub_shell_snapshots(codex_home: &Path) -> usize {
    if !codex_home.is_absolute() {
        return 0;
    }
    let dir = codex_home.join(SHELL_SNAPSHOT_DIR);
    match std::fs::symlink_metadata(&dir) {
        Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {}
        _ => return 0,
    }
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("sh") {
            continue;
        }
        let regular = std::fs::symlink_metadata(&path)
            .map(|meta| meta.file_type().is_file())
            .unwrap_or(false);
        if regular && std::fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// Whether `codex_home` is the user's shared default `<user_home>/.codex`.
/// Host sessions all use it, so another live Codex session may still need its
/// snapshot there. Compared lexically and, where both exist, by canonical path.
pub fn is_shared_default_home(codex_home: &Path, user_home: Option<&Path>) -> bool {
    let Some(user_home) = user_home else {
        return false;
    };
    let shared = user_home.join(".codex");
    if codex_home.components().eq(shared.components()) {
        return true;
    }
    match (codex_home.canonicalize(), shared.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Scrub only a per-session home: absolute and not the shared default. Returns
/// how many files were removed; 0 and no deletion for anything else.
pub fn scrub_per_session_home(codex_home: &Path, user_home: Option<&Path>) -> usize {
    if is_shared_default_home(codex_home, user_home) {
        return 0;
    }
    scrub_shell_snapshots(codex_home)
}

/// [`scrub_per_session_home`] (user home from `$HOME`) plus a counts-only log line.
pub fn scrub_and_log(codex_home: &Path, when: &str) {
    let user_home = std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(std::path::PathBuf::from);
    let removed = scrub_per_session_home(codex_home, user_home.as_deref());
    if removed > 0 {
        tracing::info!(
            removed,
            when,
            "session-agent: removed Codex shell snapshots"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_snapshot_files_and_leaves_everything_else() {
        let home = tempfile::tempdir().unwrap();
        let snaps = home.path().join(SHELL_SNAPSHOT_DIR);
        std::fs::create_dir_all(snaps.join("nested")).unwrap();
        std::fs::write(snaps.join("a.sh"), "export FAKE_TOKEN=not-a-real-token\n").unwrap();
        std::fs::write(snaps.join("b.sh"), "export OTHER=fake\n").unwrap();
        std::fs::write(snaps.join("nested/keep.sh"), "x").unwrap();
        std::fs::write(home.path().join("config.toml"), "keep").unwrap();
        let outside = home.path().join("outside.txt");
        std::fs::write(&outside, "keep").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, snaps.join("link.sh")).unwrap();

        assert_eq!(scrub_shell_snapshots(home.path()), 2);
        assert!(!snaps.join("a.sh").exists());
        assert!(!snaps.join("b.sh").exists());
        assert!(snaps.join("nested/keep.sh").exists());
        assert!(home.path().join("config.toml").exists());
        assert!(outside.exists());
        #[cfg(unix)]
        assert!(std::fs::symlink_metadata(snaps.join("link.sh")).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_snapshot_directory_is_not_followed() {
        let home = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        std::fs::write(elsewhere.path().join("x.sh"), "keep").unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), home.path().join(SHELL_SNAPSHOT_DIR)).unwrap();
        assert_eq!(scrub_shell_snapshots(home.path()), 0);
        assert!(elsewhere.path().join("x.sh").exists());
    }

    #[test]
    fn a_missing_directory_is_zero() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(scrub_shell_snapshots(home.path()), 0);
    }

    #[test]
    fn an_empty_home_deletes_nothing_in_the_current_directory() {
        // An empty home joins to a cwd-relative `shell_snapshots`.
        let cwd = tempfile::tempdir().unwrap();
        let snaps = cwd.path().join(SHELL_SNAPSHOT_DIR);
        std::fs::create_dir_all(&snaps).unwrap();
        std::fs::write(snaps.join("a.sh"), "keep").unwrap();
        let _guard = CwdGuard::enter(cwd.path());
        assert_eq!(scrub_shell_snapshots(Path::new("")), 0);
        assert!(snaps.join("a.sh").exists());
    }

    #[test]
    fn a_relative_home_deletes_nothing() {
        let cwd = tempfile::tempdir().unwrap();
        let snaps = cwd.path().join("rel").join(SHELL_SNAPSHOT_DIR);
        std::fs::create_dir_all(&snaps).unwrap();
        std::fs::write(snaps.join("a.sh"), "keep").unwrap();
        let _guard = CwdGuard::enter(cwd.path());
        assert_eq!(scrub_shell_snapshots(Path::new("rel")), 0);
        assert!(snaps.join("a.sh").exists());
    }

    #[test]
    fn a_non_sh_file_is_left_alone() {
        let home = tempfile::tempdir().unwrap();
        let snaps = home.path().join(SHELL_SNAPSHOT_DIR);
        std::fs::create_dir_all(&snaps).unwrap();
        std::fs::write(snaps.join("a.sh"), "x").unwrap();
        std::fs::write(snaps.join("notes.txt"), "keep").unwrap();
        std::fs::write(snaps.join("noext"), "keep").unwrap();
        assert_eq!(scrub_shell_snapshots(home.path()), 1);
        assert!(!snaps.join("a.sh").exists());
        assert!(snaps.join("notes.txt").exists());
        assert!(snaps.join("noext").exists());
    }

    fn seed(home: &Path) -> std::path::PathBuf {
        let snaps = home.join(SHELL_SNAPSHOT_DIR);
        std::fs::create_dir_all(&snaps).unwrap();
        std::fs::write(snaps.join("s.sh"), "export FAKE=not-a-real-token\n").unwrap();
        snaps.join("s.sh")
    }

    #[test]
    fn the_shared_default_home_is_not_scrubbed() {
        let user = tempfile::tempdir().unwrap();
        let shared = user.path().join(".codex");
        let file = seed(&shared);
        assert!(is_shared_default_home(&shared, Some(user.path())));
        assert_eq!(scrub_per_session_home(&shared, Some(user.path())), 0);
        assert!(file.exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_to_the_shared_default_home_is_not_scrubbed() {
        let user = tempfile::tempdir().unwrap();
        let shared = user.path().join(".codex");
        let file = seed(&shared);
        let alias = user.path().join("alias");
        std::os::unix::fs::symlink(&shared, &alias).unwrap();
        assert_eq!(scrub_per_session_home(&alias, Some(user.path())), 0);
        assert!(file.exists());
    }

    #[test]
    fn an_explicit_per_session_home_is_scrubbed() {
        let user = tempfile::tempdir().unwrap();
        let session_home = tempfile::tempdir().unwrap();
        let file = seed(session_home.path());
        assert_eq!(
            scrub_per_session_home(session_home.path(), Some(user.path())),
            1
        );
        assert!(!file.exists());
    }

    #[test]
    fn a_pod_style_home_is_scrubbed() {
        // A pod sets CODEX_HOME to its own directory and may have no HOME.
        let pod = tempfile::tempdir().unwrap();
        let home = pod.path().join("codex-home");
        let file = seed(&home);
        assert_eq!(scrub_per_session_home(&home, None), 1);
        assert!(!file.exists());
    }

    /// Run a test body with the process cwd switched (restored on drop). The
    /// cwd is process-wide, so these tests share a lock.
    struct CwdGuard {
        prev: std::path::PathBuf,
        _lock: std::sync::MutexGuard<'static, ()>,
    }
    static CWD_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    impl CwdGuard {
        fn enter(dir: &Path) -> Self {
            let lock = CWD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let prev = std::env::current_dir().unwrap();
            std::env::set_current_dir(dir).unwrap();
            Self { prev, _lock: lock }
        }
    }
    impl Drop for CwdGuard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.prev);
        }
    }
}
