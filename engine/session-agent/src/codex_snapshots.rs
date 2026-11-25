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

/// Remove the regular files directly inside `<codex_home>/shell_snapshots`.
/// Returns how many were removed. A missing directory, a symlinked directory
/// and any entry that is not a regular file (symlink, subdirectory) are left
/// alone.
pub fn scrub_shell_snapshots(codex_home: &Path) -> usize {
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
        let regular = std::fs::symlink_metadata(&path)
            .map(|meta| meta.file_type().is_file())
            .unwrap_or(false);
        if regular && std::fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// [`scrub_shell_snapshots`] plus a counts-only log line.
pub fn scrub_and_log(codex_home: &Path, when: &str) {
    let removed = scrub_shell_snapshots(codex_home);
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
}
