use std::{
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use axum::{
    body::Body,
    extract::{Query, State},
    http::{header, HeaderValue},
    response::Response,
    Json,
};
use serde::{Deserialize, Serialize};
use tokio::process::Command;

use crate::{
    app::AppState,
    error::{ApiError, Result},
};

/// Resolve a client-supplied path *strictly* under `workspace_root`.
///
/// Rules:
/// - Treat all paths as relative to `workspace_root`, stripping any leading `/`.
/// - Reject `..` components outright (no clever escapes via `a/../../b`).
/// - The final path need not exist; that's the handler's call.
fn safe_resolve(root: &Path, requested: &str) -> Result<PathBuf> {
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

/// Relativise a resolved absolute path back to the workspace root.
fn rel_to(root: &Path, p: &Path) -> String {
    match p.strip_prefix(root) {
        Ok(r) => r.to_string_lossy().into_owned(),
        Err(_) => p.to_string_lossy().into_owned(),
    }
}

#[derive(Debug, Serialize)]
pub struct FileEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
}

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    #[serde(default)]
    pub path: String,
}

pub async fn list_dir(
    State(state): State<Arc<AppState>>,
    Query(q): Query<ListQuery>,
) -> Result<Json<Vec<FileEntry>>> {
    let dir = safe_resolve(&state.config.workspace_root, &q.path)?;
    let meta = tokio::fs::metadata(&dir).await?;
    if !meta.is_dir() {
        return Err(ApiError::BadRequest(format!("not a directory: {}", q.path)));
    }
    let mut rd = tokio::fs::read_dir(&dir).await?;
    let mut entries = Vec::new();
    while let Some(ent) = rd.next_entry().await? {
        let path = ent.path();
        let name = ent.file_name().to_string_lossy().into_owned();
        // Skip dotfiles by default — they're noise in a workspace browser.
        // Clients can opt in by listing the hidden directory explicitly.
        if name.starts_with('.') {
            continue;
        }
        let ft = match ent.file_type().await {
            Ok(ft) => ft,
            Err(_) => continue,
        };
        let size = ent.metadata().await.map(|m| m.len()).unwrap_or(0);
        entries.push(FileEntry {
            name,
            path: rel_to(&state.config.workspace_root, &path),
            is_dir: ft.is_dir(),
            size,
        });
    }
    // Directories first, then alphabetical.
    entries.sort_by(|a, b| match (a.is_dir, b.is_dir) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
    });
    Ok(Json(entries))
}

#[derive(Debug, Deserialize)]
pub struct ReadQuery {
    pub path: String,
}

#[derive(Debug, Serialize)]
pub struct FileRead {
    pub path: String,
    pub size: u64,
    /// UTF-8 content. If the file isn't valid UTF-8, the bytes are returned
    /// base64-encoded in `content_base64` instead and `content` is null.
    pub content: Option<String>,
    pub content_base64: Option<String>,
    pub is_binary: bool,
}

/// Hard cap on a single read — refuse if larger so we don't OOM serving a
/// pathological file. ~5 MiB matches typical editor comfort.
const MAX_READ_BYTES: u64 = 5 * 1024 * 1024;

/// Hard cap for downloads — 512 MiB. Prevents trivially OOM-ing the server
/// via download of a huge file from a workspace.
const MAX_DOWNLOAD_BYTES: u64 = 512 * 1024 * 1024;

pub async fn read_file(
    State(state): State<Arc<AppState>>,
    Query(q): Query<ReadQuery>,
) -> Result<Json<FileRead>> {
    let p = safe_resolve(&state.config.workspace_root, &q.path)?;
    let meta = tokio::fs::metadata(&p).await?;
    if !meta.is_file() {
        return Err(ApiError::BadRequest(format!("not a file: {}", q.path)));
    }
    if meta.len() > MAX_READ_BYTES {
        return Err(ApiError::BadRequest(format!(
            "file too large: {} bytes (max {})",
            meta.len(),
            MAX_READ_BYTES
        )));
    }
    let bytes = tokio::fs::read(&p).await?;
    let is_binary = looks_binary(&bytes);
    let resp = if is_binary {
        FileRead {
            path: rel_to(&state.config.workspace_root, &p),
            size: meta.len(),
            content: None,
            content_base64: Some(base64(&bytes)),
            is_binary: true,
        }
    } else {
        // Replace invalid UTF-8 with U+FFFD rather than refusing — most "text"
        // files with sprinkled garbage should still load.
        let s = String::from_utf8_lossy(&bytes).into_owned();
        FileRead {
            path: rel_to(&state.config.workspace_root, &p),
            size: meta.len(),
            content: Some(s),
            content_base64: None,
            is_binary: false,
        }
    };
    Ok(Json(resp))
}

pub async fn download_file(
    State(state): State<Arc<AppState>>,
    Query(q): Query<ReadQuery>,
) -> Result<Response> {
    let p = safe_resolve(&state.config.workspace_root, &q.path)?;
    let meta = tokio::fs::metadata(&p).await?;
    if !meta.is_file() {
        return Err(ApiError::BadRequest(format!("not a file: {}", q.path)));
    }
    if meta.len() > MAX_DOWNLOAD_BYTES {
        return Err(ApiError::BadRequest(format!(
            "file too large to download: {} bytes (max {})",
            meta.len(),
            MAX_DOWNLOAD_BYTES
        )));
    }
    let bytes = tokio::fs::read(&p).await?;
    let filename = p
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "download".into());
    let disposition = format!("attachment; filename=\"{}\"", filename);
    let response = Response::builder()
        .header(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/octet-stream"),
        )
        .header(
            header::CONTENT_DISPOSITION,
            HeaderValue::from_str(&disposition)
                .unwrap_or_else(|_| HeaderValue::from_static("attachment")),
        )
        .body(Body::from(bytes))
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(response)
}

#[derive(Debug, Deserialize)]
pub struct WriteReq {
    pub path: String,
    pub content: String,
    /// If true, create parent dirs; default false.
    #[serde(default)]
    pub create_parents: bool,
}

pub async fn write_file(
    State(state): State<Arc<AppState>>,
    Json(req): Json<WriteReq>,
) -> Result<Json<serde_json::Value>> {
    let p = safe_resolve(&state.config.workspace_root, &req.path)?;
    if req.create_parents {
        if let Some(parent) = p.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
    }
    tokio::fs::write(&p, req.content.as_bytes()).await?;
    Ok(Json(
        serde_json::json!({ "ok": true, "bytes": req.content.len() }),
    ))
}

#[derive(Debug, Serialize)]
pub struct TreeNode {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub children: Option<Vec<TreeNode>>,
}

#[derive(Debug, Deserialize)]
pub struct TreeQuery {
    #[serde(default)]
    pub path: String,
    /// 0 = this dir's children only. 1 = grandchildren. etc.
    /// Hard-capped at 3 to keep responses bounded.
    #[serde(default)]
    pub depth: Option<u32>,
}

pub async fn tree(
    State(state): State<Arc<AppState>>,
    Query(q): Query<TreeQuery>,
) -> Result<Json<Vec<TreeNode>>> {
    let depth = q.depth.unwrap_or(0).min(3);
    let dir = safe_resolve(&state.config.workspace_root, &q.path)?;
    let kids = walk_dir(&state.config.workspace_root, &dir, depth).await?;
    Ok(Json(kids))
}

async fn walk_dir(root: &Path, dir: &Path, depth: u32) -> Result<Vec<TreeNode>> {
    let mut out = Vec::new();
    let mut rd = tokio::fs::read_dir(dir).await?;
    while let Some(ent) = rd.next_entry().await? {
        let name = ent.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let path = ent.path();
        let ft = match ent.file_type().await {
            Ok(ft) => ft,
            Err(_) => continue,
        };
        let children = if ft.is_dir() && depth > 0 {
            // Recurse — `Box::pin` to break the async-fn recursion size issue.
            Some(Box::pin(walk_dir(root, &path, depth - 1)).await?)
        } else if ft.is_dir() {
            Some(Vec::new())
        } else {
            None
        };
        out.push(TreeNode {
            name,
            path: rel_to(root, &path),
            is_dir: ft.is_dir(),
            children,
        });
    }
    out.sort_by(|a, b| match (a.is_dir, b.is_dir) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
    });
    Ok(out)
}

#[derive(Debug, Deserialize)]
pub struct SearchQuery {
    pub q: String,
    #[serde(default)]
    pub path: String,
    /// Cap hits returned. Server also enforces a hard ceiling.
    #[serde(default)]
    pub max: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct SearchHit {
    pub path: String,
    pub line: u64,
    pub text: String,
}

const SEARCH_HARD_CAP: usize = 500;

/// Search via `rg --json`. Requires `rg` on $PATH (TOOLING.md lists it as a
/// baseline tool — fail loudly if it isn't installed).
pub async fn search(
    State(state): State<Arc<AppState>>,
    Query(q): Query<SearchQuery>,
) -> Result<Json<Vec<SearchHit>>> {
    if q.q.is_empty() {
        return Err(ApiError::BadRequest("q must not be empty".into()));
    }
    let dir = safe_resolve(&state.config.workspace_root, &q.path)?;
    let cap = q.max.unwrap_or(200).min(SEARCH_HARD_CAP);

    let mut cmd = Command::new("rg");
    cmd.arg("--json")
        .arg("--no-messages")
        .arg("--max-count")
        .arg(cap.to_string())
        .arg("--regexp")
        .arg(&q.q)
        .arg(".")
        .current_dir(&dir);
    let output = cmd.output().await.map_err(|e| {
        ApiError::Internal(format!("failed to run rg (is ripgrep installed?): {e}"))
    })?;

    let mut hits = Vec::new();
    for line in output.stdout.split(|&b| b == b'\n') {
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(line) else {
            continue;
        };
        if v["type"] != "match" {
            continue;
        }
        let path_rel = v["data"]["path"]["text"]
            .as_str()
            .unwrap_or("")
            .trim_start_matches("./")
            .to_string();
        let line_no = v["data"]["line_number"].as_u64().unwrap_or(0);
        let text = v["data"]["lines"]["text"]
            .as_str()
            .unwrap_or("")
            .to_string();
        // Stitch path back relative to workspace_root (rg ran in `dir`).
        let full = dir.join(&path_rel);
        hits.push(SearchHit {
            path: rel_to(&state.config.workspace_root, &full),
            line: line_no,
            text: text.trim_end_matches('\n').to_string(),
        });
        if hits.len() >= cap {
            break;
        }
    }
    Ok(Json(hits))
}

fn looks_binary(b: &[u8]) -> bool {
    // Standard "is binary" heuristic: NUL byte in the first 8 KiB.
    let n = b.len().min(8192);
    b[..n].contains(&0)
}

fn base64(b: &[u8]) -> String {
    use std::fmt::Write as _;
    const TBL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(b.len().div_ceil(3) * 4);
    let mut i = 0;
    while i + 3 <= b.len() {
        let n = ((b[i] as u32) << 16) | ((b[i + 1] as u32) << 8) | (b[i + 2] as u32);
        out.push(TBL[((n >> 18) & 0x3f) as usize] as char);
        out.push(TBL[((n >> 12) & 0x3f) as usize] as char);
        out.push(TBL[((n >> 6) & 0x3f) as usize] as char);
        out.push(TBL[(n & 0x3f) as usize] as char);
        i += 3;
    }
    let rem = b.len() - i;
    if rem == 1 {
        let n = (b[i] as u32) << 16;
        let _ = write!(
            out,
            "{}{}==",
            TBL[((n >> 18) & 0x3f) as usize] as char,
            TBL[((n >> 12) & 0x3f) as usize] as char
        );
    } else if rem == 2 {
        let n = ((b[i] as u32) << 16) | ((b[i + 1] as u32) << 8);
        let _ = write!(
            out,
            "{}{}{}=",
            TBL[((n >> 18) & 0x3f) as usize] as char,
            TBL[((n >> 12) & 0x3f) as usize] as char,
            TBL[((n >> 6) & 0x3f) as usize] as char
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_resolve_accepts_relative() {
        let root = PathBuf::from("/tmp/foo");
        assert_eq!(
            safe_resolve(&root, "a/b").unwrap(),
            PathBuf::from("/tmp/foo/a/b")
        );
        assert_eq!(
            safe_resolve(&root, "/a/b").unwrap(),
            PathBuf::from("/tmp/foo/a/b")
        );
    }

    #[test]
    fn safe_resolve_rejects_parent_dir() {
        let root = PathBuf::from("/tmp/foo");
        assert!(safe_resolve(&root, "../escape").is_err());
        assert!(safe_resolve(&root, "a/../../escape").is_err());
    }

    #[test]
    fn binary_detection() {
        assert!(!looks_binary(b"hello world"));
        assert!(looks_binary(b"hi\0there"));
    }
}
