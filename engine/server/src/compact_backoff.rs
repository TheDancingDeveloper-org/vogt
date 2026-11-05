//! Back-off for Codex auto-compaction (WI-2832).
//!
//! Codex compacts when a request reaches `model_auto_compact_token_limit`. When
//! the context that survives compaction (the fixed prefix: tool definitions,
//! developer instructions) is already over that limit, it compacts again on the
//! next request, forever. The Economics tailer spots that loop; this module
//! remembers the size the context came back to and turns it into a higher limit
//! for the next launch of the same session, so Codex does not re-trigger until
//! the context has grown past the post-compaction level plus a margin.

use std::path::{Path, PathBuf};

use uuid::Uuid;

/// Tokens of growth after a compaction before the next one may fire.
pub const COMPACT_MARGIN_TOKENS: u64 = 20_000;

fn floor_path(state_dir: &Path, session: Uuid) -> PathBuf {
    state_dir.join("compact-floor").join(session.to_string())
}

/// Remember the context size a compaction left behind. Only a larger floor
/// replaces the stored one, so a later, smaller reading never lowers the limit.
pub fn record_floor(state_dir: &Path, session: Uuid, post_compaction: u64) -> std::io::Result<()> {
    if post_compaction == 0 || load_floor(state_dir, session).is_some_and(|f| f >= post_compaction)
    {
        return Ok(());
    }
    let path = floor_path(state_dir, session);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, post_compaction.to_string())
}

pub fn load_floor(state_dir: &Path, session: Uuid) -> Option<u64> {
    std::fs::read_to_string(floor_path(state_dir, session))
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// Compactions in a row, each with real work between them, after which the
/// floor is dropped: the context no longer comes back over the default limit.
pub const CLEAN_COMPACTIONS_TO_RESET: i64 = 3;

/// The context window of the models Codex runs here, in tokens.
pub const MODEL_WINDOW_TOKENS: u64 = 272_000;

/// What mcp-bootstrap writes for `model_auto_compact_token_limit` when
/// `ENGINE_CODEX_AUTO_COMPACT_TOKENS` is unset.
pub const DEFAULT_COMPACT_LIMIT: u64 = 200_000;

/// The value of `model_auto_compact_token_limit_scope` Vogt sets. Present in
/// the pinned fork (`VOGT_CODEX_SHA`, `AutoCompactTokenLimitScope` in
/// `codex-rs/core/config.schema.json`, variants `total` and a post-prefix one).
pub const COMPACT_SCOPE: &str = "total";

/// The limit the deployment runs Codex with when Vogt does not override it.
pub fn default_limit() -> u64 {
    std::env::var("ENGINE_CODEX_AUTO_COMPACT_TOKENS")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .filter(|v| *v > 0)
        .unwrap_or(DEFAULT_COMPACT_LIMIT)
}

/// Codex compacts at `min(limit, 90% of the window)`; a limit above that does
/// nothing, so a backed-off limit stops there.
fn limit_ceiling() -> u64 {
    MODEL_WINDOW_TOKENS / 10 * 9
}

/// Forget the floor: the session compacts cleanly again.
pub fn clear_floor(state_dir: &Path, session: Uuid) {
    let _ = std::fs::remove_file(floor_path(state_dir, session));
}

/// The `model_auto_compact_token_limit` to launch with: `Some(floor + margin)`
/// when a recorded post-compaction size means Codex would otherwise keep
/// compacting, else `None` and the configured limit stands.
///
/// `default` is the limit the deployment already runs with. The result is
/// never at or below it (that would lower the limit and make Codex compact
/// more), and never above the ceiling where Codex stops compacting at all.
pub fn launch_limit(state_dir: &Path, session: Uuid, default: u64) -> Option<u64> {
    let wanted = load_floor(state_dir, session)?.saturating_add(COMPACT_MARGIN_TOKENS);
    let limit = wanted.min(limit_ceiling());
    (limit > default).then_some(limit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_floor_leaves_the_limit_alone() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(launch_limit(dir.path(), Uuid::nil(), 128_000), None);
    }

    #[test]
    fn a_floor_over_the_limit_raises_it_past_floor_plus_margin() {
        let dir = tempfile::tempdir().unwrap();
        let id = Uuid::new_v4();
        record_floor(dir.path(), id, 132_000).unwrap();
        assert_eq!(
            launch_limit(dir.path(), id, 128_000),
            Some(132_000 + COMPACT_MARGIN_TOKENS)
        );
    }

    #[test]
    fn a_limit_already_clear_of_the_floor_is_not_touched() {
        let dir = tempfile::tempdir().unwrap();
        let id = Uuid::new_v4();
        record_floor(dir.path(), id, 50_000).unwrap();
        assert_eq!(launch_limit(dir.path(), id, 200_000), None);
    }

    /// Review note 1: a small floor must never lower the deployment's limit.
    #[test]
    fn a_small_floor_never_lowers_the_default_limit() {
        let dir = tempfile::tempdir().unwrap();
        let id = Uuid::new_v4();
        record_floor(dir.path(), id, 30_000).unwrap();
        assert_eq!(launch_limit(dir.path(), id, 128_000), None);
        // Exactly at the default is no raise either.
        record_floor(dir.path(), id, 108_000).unwrap();
        assert_eq!(launch_limit(dir.path(), id, 128_000), None);
    }

    /// Review note 2: past the window Codex would never compact.
    #[test]
    fn the_limit_is_clamped_below_the_context_window() {
        let dir = tempfile::tempdir().unwrap();
        let id = Uuid::new_v4();
        record_floor(dir.path(), id, 260_000).unwrap();
        let limit = launch_limit(dir.path(), id, 200_000).unwrap();
        assert_eq!(limit, MODEL_WINDOW_TOKENS / 10 * 9);
        assert!(limit < MODEL_WINDOW_TOKENS);
        // A default already at the ceiling has nothing to raise to.
        assert_eq!(launch_limit(dir.path(), id, limit), None);
    }

    #[test]
    fn the_floor_only_grows() {
        let dir = tempfile::tempdir().unwrap();
        let id = Uuid::new_v4();
        record_floor(dir.path(), id, 140_000).unwrap();
        record_floor(dir.path(), id, 90_000).unwrap();
        assert_eq!(load_floor(dir.path(), id), Some(140_000));
    }

    /// Review note 3: a cleared floor stops backing the limit off.
    #[test]
    fn a_cleared_floor_restores_the_default_limit() {
        let dir = tempfile::tempdir().unwrap();
        let id = Uuid::new_v4();
        record_floor(dir.path(), id, 150_000).unwrap();
        assert!(launch_limit(dir.path(), id, 128_000).is_some());
        clear_floor(dir.path(), id);
        assert_eq!(load_floor(dir.path(), id), None);
        assert_eq!(launch_limit(dir.path(), id, 128_000), None);
        // Clearing again is harmless.
        clear_floor(dir.path(), id);
    }
}
