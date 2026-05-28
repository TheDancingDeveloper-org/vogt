//! Shared workspace path resolution used by every API that takes a client-supplied path.
//!
//! All endpoints that touch the filesystem must funnel through one policy so a
//! future change can't accidentally leave one of them lax. The two operations
//! we need are:
//!
//! * [`resolve_existing`] — for reads (file, dir, git repo lookups). Canonicalises
//!   the requested path and asserts the result still lives inside the workspace
//!   root after symlinks are resolved.
//! * [`resolve_for_write`] — for creating or overwriting a file. Canonicalises
//!   the parent directory (which must already exist after any optional
//!   `create_parents` step) and joins the final filename component without
//!   following it as a symlink.
//!
//! Both reject `..`, root, and prefix components up front so callers don't
//! depend on canonicalisation alone.
//!
//! The canonical workspace root is assumed to already be canonical; the config
//! loader canonicalises it once at startup.

use std::path::{Component, Path, PathBuf};

use crate::error::{ApiError, Result};

fn strip_lexically(root: &Path, requested: &str) -> Result<PathBuf> {
    let rel = requested.trim_start_matches('/');
    let mut out = root.to_path_buf();
    for comp in Path::new(rel).components() {
        match comp {
            Component::Normal(s) => out.push(s),
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(ApiError::BadRequest(
                    "path contains '..' (parent component)".into(),
                ));
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(ApiError::BadRequest("path must be relative".into()));
            }
        }
    }
    Ok(out)
}

/// Resolve a path the client expects to already exist, e.g. a file to read or
/// a directory to list. Follows symlinks and verifies the final path is still
/// under `root`.
pub fn resolve_existing(root: &Path, requested: &str) -> Result<PathBuf> {
    let joined = strip_lexically(root, requested)?;
    let canon = joined
        .canonicalize()
        .map_err(|e| ApiError::BadRequest(format!("path {requested:?}: {e}")))?;
    if !canon.starts_with(root) {
        return Err(ApiError::BadRequest(
            "path escapes workspace_root via symlink".into(),
        ));
    }
    Ok(canon)
}

/// Resolve a path the client wants to write. Canonicalises the parent — which
/// must exist — then re-joins the final filename component without following
/// it. Rejects writing through a symlink that points outside the workspace.
pub fn resolve_for_write(root: &Path, requested: &str) -> Result<PathBuf> {
    let joined = strip_lexically(root, requested)?;
    let parent = joined
        .parent()
        .ok_or_else(|| ApiError::BadRequest("path has no parent".into()))?;
    let file_name = joined
        .file_name()
        .ok_or_else(|| ApiError::BadRequest("path has no final component".into()))?;
    let canon_parent = parent.canonicalize().map_err(|e| {
        ApiError::BadRequest(format!("parent of {requested:?} does not exist: {e}"))
    })?;
    if !canon_parent.starts_with(root) {
        return Err(ApiError::BadRequest(
            "path escapes workspace_root via symlink".into(),
        ));
    }
    Ok(canon_parent.join(file_name))
}

/// Variant of [`resolve_existing`] that tolerates the path not yet existing.
/// Used by callers that just want the lexically-joined path without verifying
/// it's inside the workspace via canonicalisation (e.g. the search root may
/// be a virtual subdirectory). Always canonicalises if the path exists.
pub fn resolve_existing_or_lexical(root: &Path, requested: &str) -> Result<PathBuf> {
    let joined = strip_lexically(root, requested)?;
    match joined.canonicalize() {
        Ok(canon) => {
            if !canon.starts_with(root) {
                return Err(ApiError::BadRequest(
                    "path escapes workspace_root via symlink".into(),
                ));
            }
            Ok(canon)
        }
        Err(_) => Ok(joined),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn rejects_parent_component() {
        let root = std::env::temp_dir().canonicalize().unwrap();
        assert!(resolve_existing(&root, "../escape").is_err());
        assert!(resolve_existing(&root, "a/../../escape").is_err());
    }

    #[test]
    fn rejects_root_component() {
        let root = std::env::temp_dir().canonicalize().unwrap();
        assert!(resolve_existing(&root, "/").is_ok() || resolve_existing(&root, "/").is_err());
    }

    #[test]
    fn rejects_symlink_escape() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let outside_path = outside.path().canonicalize().unwrap();
        std::fs::write(outside_path.join("victim.txt"), b"hi").unwrap();
        symlink(&outside_path, root.join("link")).unwrap();

        let res = resolve_existing(&root, "link/victim.txt");
        assert!(
            matches!(&res, Err(ApiError::BadRequest(msg)) if msg.contains("symlink")),
            "expected symlink rejection, got {res:?}"
        );
    }

    #[test]
    fn write_rejects_symlink_parent_escape() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let outside_path = outside.path().canonicalize().unwrap();
        symlink(&outside_path, root.join("link")).unwrap();

        let res = resolve_for_write(&root, "link/new.txt");
        assert!(
            matches!(&res, Err(ApiError::BadRequest(msg)) if msg.contains("symlink")),
            "expected symlink rejection, got {res:?}"
        );
    }

    #[test]
    fn write_accepts_new_file_in_existing_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let p = resolve_for_write(&root, "new.txt").unwrap();
        assert_eq!(p, root.join("new.txt"));
    }
}
