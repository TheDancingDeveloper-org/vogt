//! `GET /api/workspaces` (WI-1340): one row per live or hibernated session
//! that has a workspace block (WI-1324), with whether it is alive and how
//! many bytes its tree holds.
//!
//! A size is a walk of the whole tree, and a worktree with a `target/` can
//! hold millions of files, so the walk never runs on the request path: each
//! path is rescanned on a blocking thread at most once per
//! `ENGINE_WORKSPACE_SIZE_TTL` (60 s), a scan already running is joined
//! rather than repeated, and a request waits at most [`SIZE_WAIT`] for fresh
//! numbers. A path still being walked reports its last size, or `null` before
//! the first walk finishes.

use std::{
    collections::HashMap,
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use axum::{extract::State, Json};
use parking_lot::Mutex;
use serde::Serialize;
use uuid::Uuid;

use crate::app::AppState;

/// The longest a request waits for stale sizes to be rescanned.
pub const SIZE_WAIT: Duration = Duration::from_secs(2);

/// The default rescan interval when `ENGINE_WORKSPACE_SIZE_TTL` is unset.
pub const DEFAULT_SIZE_TTL: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceRow {
    pub session_id: Uuid,
    pub alive: bool,
    /// Bytes under `path`, or `null` before the first walk finishes.
    pub size_bytes: Option<u64>,
    #[serde(flatten)]
    pub workspace: vogt_engine_contract::Workspace,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceList {
    pub workspaces: Vec<WorkspaceRow>,
}

#[derive(Default)]
struct Entry {
    scanned_at: Option<Instant>,
    bytes: Option<u64>,
    scanning: Option<tokio::sync::watch::Receiver<bool>>,
}

/// Per-path size cache shared by every request.
pub struct WorkspaceSizes {
    ttl: Duration,
    entries: Mutex<HashMap<String, Entry>>,
}

impl WorkspaceSizes {
    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// `ENGINE_WORKSPACE_SIZE_TTL` in seconds; unset or unparsable is 60.
    pub fn from_env() -> Self {
        let ttl = std::env::var("ENGINE_WORKSPACE_SIZE_TTL")
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .map(Duration::from_secs)
            .unwrap_or(DEFAULT_SIZE_TTL);
        Self::new(ttl)
    }

    /// The size of each path, rescanning the stale ones in the background
    /// and waiting at most `wait` for them.
    pub async fn sizes(self: &Arc<Self>, paths: &[String], wait: Duration) -> Vec<Option<u64>> {
        let mut pending = Vec::new();
        {
            let mut entries = self.entries.lock();
            entries.retain(|k, _| paths.contains(k));
            for path in paths {
                let entry = entries.entry(path.clone()).or_default();
                if let Some(done) = &entry.scanning {
                    pending.push(done.clone());
                    continue;
                }
                let fresh = entry.scanned_at.is_some_and(|at| at.elapsed() < self.ttl);
                if fresh {
                    continue;
                }
                let (tx, rx) = tokio::sync::watch::channel(false);
                entry.scanning = Some(rx.clone());
                pending.push(rx);
                self.spawn_scan(path.clone(), tx);
            }
        }
        if !pending.is_empty() {
            let all = async {
                for mut done in pending {
                    let _ = done.wait_for(|finished| *finished).await;
                }
            };
            let _ = tokio::time::timeout(wait, all).await;
        }
        let entries = self.entries.lock();
        paths
            .iter()
            .map(|p| entries.get(p).and_then(|e| e.bytes))
            .collect()
    }

    fn spawn_scan(self: &Arc<Self>, path: String, done: tokio::sync::watch::Sender<bool>) {
        let this = Arc::clone(self);
        tokio::spawn(async move {
            let walk_path = path.clone();
            let bytes = tokio::task::spawn_blocking(move || tree_bytes(Path::new(&walk_path)))
                .await
                .ok();
            {
                let mut entries = this.entries.lock();
                let entry = entries.entry(path).or_default();
                entry.scanned_at = Some(Instant::now());
                if let Some(bytes) = bytes {
                    entry.bytes = Some(bytes);
                }
                entry.scanning = None;
            }
            let _ = done.send(true);
        });
    }
}

/// Bytes of every regular file under `root`, symlinks not followed and
/// nothing excluded. An unreadable entry counts as zero.
pub fn tree_bytes(root: &Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(read) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in read.flatten() {
            let Ok(meta) = entry.file_type() else {
                continue;
            };
            if meta.is_dir() {
                stack.push(entry.path());
            } else if meta.is_file() {
                total += entry.metadata().map(|m| m.len()).unwrap_or(0);
            }
        }
    }
    total
}

pub async fn list_workspaces(State(state): State<Arc<AppState>>) -> Json<WorkspaceList> {
    let found = state.sessions.workspaces();
    let paths: Vec<String> = found.iter().map(|(_, _, ws)| ws.path.clone()).collect();
    let sizes = state.workspace_sizes.sizes(&paths, SIZE_WAIT).await;
    let workspaces = found
        .into_iter()
        .zip(sizes)
        .map(
            |((session_id, alive, workspace), size_bytes)| WorkspaceRow {
                session_id,
                alive,
                size_bytes,
                workspace,
            },
        )
        .collect();
    Json(WorkspaceList { workspaces })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tree_bytes_sums_nested_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a"), b"12345").unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub/b"), b"123").unwrap();
        assert_eq!(tree_bytes(dir.path()), 8);
    }

    #[tokio::test]
    async fn sizes_are_cached_for_the_ttl() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a"), b"12345").unwrap();
        let cache = Arc::new(WorkspaceSizes::new(Duration::from_secs(3600)));
        let path = vec![dir.path().display().to_string()];
        assert_eq!(cache.sizes(&path, SIZE_WAIT).await, vec![Some(5)]);
        std::fs::write(dir.path().join("b"), b"123").unwrap();
        assert_eq!(cache.sizes(&path, SIZE_WAIT).await, vec![Some(5)]);
        let fresh = Arc::new(WorkspaceSizes::new(Duration::ZERO));
        assert_eq!(fresh.sizes(&path, SIZE_WAIT).await, vec![Some(8)]);
    }
}
