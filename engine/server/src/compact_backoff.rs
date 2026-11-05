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

/// The `model_auto_compact_token_limit` to launch with: `Some(floor + margin)`
/// when a recorded post-compaction size means Codex would otherwise keep
/// compacting, else `None` and the configured limit stands.
///
/// `configured` is the limit the deployment sets, when known. A limit that is
/// already above the backed-off one is left alone.
pub fn launch_limit(state_dir: &Path, session: Uuid, configured: Option<u64>) -> Option<u64> {
    let wanted = load_floor(state_dir, session)?.saturating_add(COMPACT_MARGIN_TOKENS);
    match configured {
        Some(limit) if limit >= wanted => None,
        _ => Some(wanted),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_floor_leaves_the_limit_alone() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(launch_limit(dir.path(), Uuid::nil(), Some(128_000)), None);
    }

    #[test]
    fn a_floor_over_the_limit_raises_it_past_floor_plus_margin() {
        let dir = tempfile::tempdir().unwrap();
        let id = Uuid::new_v4();
        record_floor(dir.path(), id, 132_000).unwrap();
        assert_eq!(
            launch_limit(dir.path(), id, Some(128_000)),
            Some(132_000 + COMPACT_MARGIN_TOKENS)
        );
        // Unknown configured limit: still raised, never left to the model default.
        assert_eq!(
            launch_limit(dir.path(), id, None),
            Some(132_000 + COMPACT_MARGIN_TOKENS)
        );
    }

    #[test]
    fn a_limit_already_clear_of_the_floor_is_not_touched() {
        let dir = tempfile::tempdir().unwrap();
        let id = Uuid::new_v4();
        record_floor(dir.path(), id, 50_000).unwrap();
        assert_eq!(launch_limit(dir.path(), id, Some(200_000)), None);
    }

    #[test]
    fn the_floor_only_grows() {
        let dir = tempfile::tempdir().unwrap();
        let id = Uuid::new_v4();
        record_floor(dir.path(), id, 140_000).unwrap();
        record_floor(dir.path(), id, 90_000).unwrap();
        assert_eq!(load_floor(dir.path(), id), Some(140_000));
    }
}
