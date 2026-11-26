//! Daily sweep of kept per-session graft token files (WI-4755).
//!
//! `GraftProvider::revoke_credential` keeps a session's token file in
//! `<state_dir>/pod-graft-tokens/<uuid>` when no minter is configured, so a
//! later teardown with a minter can still revoke it. When no such teardown ever
//! comes, the file stays for good. The token inside is dead once its TTL has
//! passed (the session budget plus slack, capped at [`graft_mint::MAX_TTL_S`],
//! enforced by graft), so this sweep removes files older than that cap plus a
//! one hour margin, judged by modification time.
//!
//! It is deliberately narrow, because it deletes files that held credentials:
//! it looks only directly inside the credential directory (no recursion), only
//! at regular files (`symlink_metadata`, so a symlink is never followed and
//! never removed) whose name is a canonical lowercase hyphenated UUID, and a
//! file with a modification time in the future is left alone. A failure to read
//! the directory or to remove a file is counted and the sweep carries on.
//! Only counts are logged: no file name, content or token value.
//!
//! Scheduling is the worktree sweep's ([`worktree_sweep::drive_schedule_in`]),
//! with its own last-run file `<state_dir>/graft-token-sweep-last` and the same
//! interval and delay knobs. `ENGINE_GRAFT_TOKEN_SWEEP` picks the mode with the
//! worktree sweep's convention: `dry-run` (the default) counts and removes
//! nothing, `on` removes, `off` runs no sweep, and an unreadable value turns it
//! off.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use crate::{
    app::AppState,
    graft_mint::MAX_TTL_S,
    graft_provider::CREDENTIAL_DIR,
    worktree_sweep::{drive_schedule_in, Schedule, SweepMode},
};

/// The environment variable that picks the sweep mode.
pub const SWEEP_ENV: &str = "ENGINE_GRAFT_TOKEN_SWEEP";

/// File in the state dir holding the unix time of the last sweep.
const LAST_RUN_FILE: &str = "graft-token-sweep-last";

/// Added to the token TTL cap before a file counts as old, so a token minted at
/// the very end of a session is long dead when its file goes.
pub const MARGIN: Duration = Duration::from_secs(60 * 60);

/// How old a kept token file must be before the sweep removes it.
pub const fn max_age() -> Duration {
    Duration::from_secs(MAX_TTL_S + MARGIN.as_secs())
}

/// What one sweep found. Counts only.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TokenSweepReport {
    /// Regular files with a UUID name that were looked at.
    pub candidates: usize,
    /// Candidates old enough to remove.
    pub expired: usize,
    /// Expired files actually removed; zero in dry-run.
    pub removed: usize,
    /// Expired files whose removal failed, plus directory entries that could
    /// not be read.
    pub failed: usize,
    /// The directory could not be read at all (or is not a plain directory).
    pub unreadable_dir: bool,
}

fn parse_mode() -> SweepMode {
    match SweepMode::parse(std::env::var(SWEEP_ENV).ok().as_deref()) {
        Ok(mode) => mode,
        Err(_) => {
            tracing::warn!("{SWEEP_ENV} must be off, dry-run or on; the graft token sweep is off");
            SweepMode::Off
        }
    }
}

/// A canonical lowercase hyphenated UUID, which is how the provider names files.
fn is_uuid_name(name: &std::ffi::OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    uuid::Uuid::parse_str(name)
        .map(|u| u.hyphenated().to_string() == name)
        .unwrap_or(false)
}

/// Sweep `dir` (the credential directory): remove, when `apply`, the regular
/// files named by a UUID whose modification time is more than `max_age` before
/// `now`. Never panics and never stops at one failure.
pub fn sweep_dir(dir: &Path, max_age: Duration, now: SystemTime, apply: bool) -> TokenSweepReport {
    let mut report = TokenSweepReport::default();
    // The directory itself must be a real directory, not a link to somewhere else.
    match std::fs::symlink_metadata(dir) {
        Ok(m) if m.is_dir() => {}
        Ok(_) => {
            report.unreadable_dir = true;
            return report;
        }
        Err(e) => {
            // A missing directory just means no token was ever kept.
            report.unreadable_dir = e.kind() != std::io::ErrorKind::NotFound;
            return report;
        }
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => {
            report.unreadable_dir = true;
            return report;
        }
    };
    for entry in entries {
        let Ok(entry) = entry else {
            report.failed += 1;
            continue;
        };
        if !is_uuid_name(&entry.file_name()) {
            continue;
        }
        let path = entry.path();
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            report.failed += 1;
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        report.candidates += 1;
        let Ok(modified) = meta.modified() else {
            continue;
        };
        // A modification time in the future gives an Err: leave it alone.
        let Ok(age) = now.duration_since(modified) else {
            continue;
        };
        if age <= max_age {
            continue;
        }
        report.expired += 1;
        if apply {
            match std::fs::remove_file(&path) {
                Ok(()) => report.removed += 1,
                Err(_) => report.failed += 1,
            }
        }
    }
    report
}

/// Run one sweep over the state dir's credential directory and audit the counts.
pub async fn run_sweep(state_dir: std::path::PathBuf, apply: bool) -> TokenSweepReport {
    let dir = state_dir.join(CREDENTIAL_DIR);
    let report =
        tokio::task::spawn_blocking(move || sweep_dir(&dir, max_age(), SystemTime::now(), apply))
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(error = %e, "graft token sweep task failed");
                TokenSweepReport {
                    unreadable_dir: true,
                    ..TokenSweepReport::default()
                }
            });
    tracing::info!(
        target: "vogt::audit",
        event = "graft_token.sweep",
        apply,
        candidates = report.candidates,
        expired = report.expired,
        removed = report.removed,
        failed = report.failed,
        unreadable_dir = report.unreadable_dir,
        "graft token sweep"
    );
    report
}

/// Run the sweep on the worktree sweep's schedule for the life of the process.
/// Off spawns nothing.
pub fn spawn_sweep(state: Arc<AppState>) {
    let mode = parse_mode();
    if mode == SweepMode::Off {
        return;
    }
    let schedule = Schedule::from_env();
    let state_dir = state.config.state_dir.clone();
    Arc::clone(&state.boot_tasks).spawn("graft-token-sweep", async move {
        let dir = state_dir.clone();
        drive_schedule_in(
            schedule,
            &state_dir,
            LAST_RUN_FILE,
            || time::OffsetDateTime::now_utc().unix_timestamp(),
            tokio::time::sleep,
            || {
                let dir = dir.clone();
                async move {
                    run_sweep(dir, mode == SweepMode::On).await;
                }
            },
            None,
        )
        .await;
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{File, FileTimes};

    const OLD: &str = "11111111-2222-4333-8444-555555555555";
    const FRESH: &str = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";

    fn age(path: &Path, by: Duration) {
        let f = File::options().write(true).open(path).unwrap();
        f.set_times(FileTimes::new().set_modified(SystemTime::now() - by))
            .unwrap();
    }

    fn setup() -> (tempfile::TempDir, std::path::PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join(CREDENTIAL_DIR);
        std::fs::create_dir(&dir).unwrap();
        (tmp, dir)
    }

    fn put(dir: &Path, name: &str, old: bool) -> std::path::PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, "secret-token").unwrap();
        if old {
            age(&p, max_age() + Duration::from_secs(60));
        }
        p
    }

    #[test]
    fn max_age_is_the_ttl_cap_plus_one_hour() {
        assert_eq!(max_age(), Duration::from_secs(13 * 3600));
    }

    #[test]
    fn removes_old_file_keeps_fresh_one() {
        let (_t, dir) = setup();
        let old = put(&dir, OLD, true);
        let fresh = put(&dir, FRESH, false);
        // Just inside the window stays.
        let edge = put(&dir, "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb", false);
        age(&edge, max_age() - Duration::from_secs(120));
        let r = sweep_dir(&dir, max_age(), SystemTime::now(), true);
        assert_eq!((r.candidates, r.expired, r.removed, r.failed), (3, 1, 1, 0));
        assert!(!old.exists());
        assert!(fresh.exists());
        assert!(edge.exists());
    }

    #[test]
    fn dry_run_removes_nothing() {
        let (_t, dir) = setup();
        let old = put(&dir, OLD, true);
        let r = sweep_dir(&dir, max_age(), SystemTime::now(), false);
        assert_eq!((r.expired, r.removed), (1, 0));
        assert!(old.exists());
    }

    #[test]
    fn leaves_non_uuid_names_alone() {
        let (_t, dir) = setup();
        let a = put(&dir, "notes.txt", true);
        let b = put(&dir, "11111111222243338444555555555555", true); // simple form
        let c = put(&dir, "11111111-2222-4333-8444-55555555555X", true);
        let d = put(&dir, &FRESH.to_uppercase(), true);
        let r = sweep_dir(&dir, max_age(), SystemTime::now(), true);
        assert_eq!((r.candidates, r.removed), (0, 0));
        assert!(a.exists() && b.exists() && c.exists() && d.exists());
    }

    #[test]
    fn never_follows_or_removes_a_symlink() {
        let (tmp, dir) = setup();
        let outside = tmp.path().join("outside");
        std::fs::write(&outside, "precious").unwrap();
        age(&outside, max_age() + Duration::from_secs(60));
        let link = dir.join(OLD);
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        let r = sweep_dir(&dir, max_age(), SystemTime::now(), true);
        assert_eq!((r.candidates, r.removed), (0, 0));
        assert!(std::fs::symlink_metadata(&link).is_ok());
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "precious");
    }

    #[test]
    fn leaves_directories_and_nested_files_alone() {
        let (_t, dir) = setup();
        let sub = dir.join(OLD);
        std::fs::create_dir(&sub).unwrap();
        let nested = put(&sub, FRESH, true);
        let r = sweep_dir(&dir, max_age(), SystemTime::now(), true);
        assert_eq!((r.candidates, r.removed), (0, 0));
        assert!(sub.is_dir() && nested.exists());
    }

    #[test]
    fn future_mtime_is_left_alone() {
        let (_t, dir) = setup();
        let p = put(&dir, OLD, false);
        let f = File::options().write(true).open(&p).unwrap();
        f.set_times(FileTimes::new().set_modified(SystemTime::now() + Duration::from_secs(7200)))
            .unwrap();
        let r = sweep_dir(&dir, max_age(), SystemTime::now(), true);
        assert_eq!((r.candidates, r.removed), (1, 0));
        assert!(p.exists());
    }

    #[test]
    fn missing_or_unreadable_dir_does_not_panic() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("absent");
        let r = sweep_dir(&missing, max_age(), SystemTime::now(), true);
        assert_eq!(r, TokenSweepReport::default());

        // A plain file where the directory should be.
        let file = tmp.path().join("afile");
        std::fs::write(&file, "x").unwrap();
        assert!(sweep_dir(&file, max_age(), SystemTime::now(), true).unreadable_dir);

        // A directory with no read permission (skipped when running as root).
        use std::os::unix::fs::PermissionsExt;
        let (_t, dir) = setup();
        let old = put(&dir, OLD, true);
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o000)).unwrap();
        let r = sweep_dir(&dir, max_age(), SystemTime::now(), true);
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        if std::fs::read_dir(&dir).is_ok() && r.removed == 1 {
            return; // root ignores the mode; nothing to assert
        }
        assert!(r.unreadable_dir);
        assert!(old.exists());
    }

    #[test]
    fn a_symlinked_credential_dir_is_not_entered() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let old = put(&real, OLD, true);
        let link = tmp.path().join(CREDENTIAL_DIR);
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let r = sweep_dir(&link, max_age(), SystemTime::now(), true);
        assert!(r.unreadable_dir && r.removed == 0);
        assert!(old.exists());
    }

    #[test]
    fn nothing_outside_the_credential_dir_is_touched() {
        let (tmp, dir) = setup();
        let sibling = tmp.path().join(OLD); // UUID-named, old, but in the state dir
        std::fs::write(&sibling, "x").unwrap();
        age(&sibling, max_age() + Duration::from_secs(60));
        let other = tmp.path().join("other");
        std::fs::create_dir(&other).unwrap();
        let other_file = put(&other, OLD, true);
        put(&dir, OLD, true);
        let r = sweep_dir(&dir, max_age(), SystemTime::now(), true);
        assert_eq!(r.removed, 1);
        assert!(sibling.exists() && other_file.exists());
    }

    #[test]
    fn run_sweep_uses_the_credential_dir_under_the_state_dir() {
        let (tmp, dir) = setup();
        let old = put(&dir, OLD, true);
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let dry = rt.block_on(run_sweep(tmp.path().to_path_buf(), false));
        assert_eq!((dry.expired, dry.removed), (1, 0));
        assert!(old.exists());
        let wet = rt.block_on(run_sweep(tmp.path().to_path_buf(), true));
        assert_eq!(wet.removed, 1);
        assert!(!old.exists());
    }

    #[test]
    fn own_last_run_file_does_not_collide_with_the_worktree_sweep() {
        let tmp = tempfile::tempdir().unwrap();
        crate::worktree_sweep::record_last_run_in(tmp.path(), LAST_RUN_FILE, 42).unwrap();
        assert_eq!(
            crate::worktree_sweep::load_last_run_in(tmp.path(), LAST_RUN_FILE),
            Some(42)
        );
        assert_eq!(crate::worktree_sweep::load_last_run(tmp.path()), None);
    }
}
