use std::{path::Path, sync::Arc};

use axum::{
    body::Body,
    extract::{Query, State},
    http::{header, HeaderValue},
    response::Response,
    Json,
};
use base64::Engine as _;
use vogt_engine_contract::{
    FileEntry, FileRead, FileSearchResult, SearchHit, TreeNode, WriteFileResponse, WriteReq,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::process::Command;
use tokio_util::io::ReaderStream;

use crate::{
    app::AppState,
    error::{ApiError, Result},
    workspace_path,
};

/// Relativise a resolved absolute path back to the workspace root.
fn rel_to(root: &Path, p: &Path) -> String {
    match p.strip_prefix(root) {
        Ok(r) => r.to_string_lossy().into_owned(),
        Err(_) => p.to_string_lossy().into_owned(),
    }
}

/// SHA-256 of a byte slice, hex-encoded. Content-based ETag for a file — the
/// robust half of the editor's optimistic-concurrency guard.
fn hash_bytes(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    let digest = h.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for b in digest {
        use std::fmt::Write as _;
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// A file's modified time as milliseconds since the Unix epoch, or 0 when the
/// platform can't report it (never a hard failure — mtime is advisory).
fn mtime_millis(meta: &std::fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
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
    let dir = if q.path.trim().is_empty() {
        state.config.workspace_root.clone()
    } else {
        workspace_path::resolve_existing(&state.config.workspace_root, &q.path)?
    };
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

/// Hard cap on a single read — refuse if larger so we don't OOM serving a
/// pathological file. ~5 MiB matches typical editor comfort.
const MAX_READ_BYTES: u64 = 5 * 1024 * 1024;

/// Hard cap for downloads — 512 MiB. The body is streamed, so this caps
/// transfer size rather than memory usage.
const MAX_DOWNLOAD_BYTES: u64 = 512 * 1024 * 1024;

pub async fn read_file(
    State(state): State<Arc<AppState>>,
    Query(q): Query<ReadQuery>,
) -> Result<Json<FileRead>> {
    let p = workspace_path::resolve_existing(&state.config.workspace_root, &q.path)?;
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
    let hash = hash_bytes(&bytes);
    let mtime = mtime_millis(&meta);
    let is_binary = looks_binary(&bytes);
    let resp = if is_binary {
        FileRead {
            path: rel_to(&state.config.workspace_root, &p),
            size: meta.len(),
            content: None,
            content_base64: Some(base64::engine::general_purpose::STANDARD.encode(&bytes)),
            is_binary: true,
            mtime,
            hash,
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
            mtime,
            hash,
        }
    };
    Ok(Json(resp))
}

pub async fn download_file(
    State(state): State<Arc<AppState>>,
    Query(q): Query<ReadQuery>,
) -> Result<Response> {
    let p = workspace_path::resolve_existing(&state.config.workspace_root, &q.path)?;
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
    let file = tokio::fs::File::open(&p).await?;
    let stream = ReaderStream::with_capacity(file, 64 * 1024);
    let body = Body::from_stream(stream);
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
        .header(header::CONTENT_LENGTH, HeaderValue::from(meta.len()))
        .body(body)
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(response)
}

pub async fn write_file(
    State(state): State<Arc<AppState>>,
    Json(req): Json<WriteReq>,
) -> Result<Json<WriteFileResponse>> {
    // Decode the payload first so a bad base64 body fails before any mkdir.
    let bytes: Vec<u8> = match &req.content_base64 {
        Some(b64) => base64::engine::general_purpose::STANDARD
            .decode(b64.as_bytes())
            .map_err(|e| ApiError::BadRequest(format!("invalid content_base64: {e}")))?,
        None => req.content.into_bytes(),
    };

    if req.create_parents {
        // Need to materialise the parent before resolve_for_write can canonicalise it.
        // strip_lexically equivalent via resolve_existing_or_lexical handles the validation
        // of components; we then pull the parent and mkdir-p, asserting it's still under root.
        let lexical =
            workspace_path::resolve_existing_or_lexical(&state.config.workspace_root, &req.path)?;
        if let Some(parent) = lexical.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
    }
    let p = workspace_path::resolve_for_write(&state.config.workspace_root, &req.path)?;

    // Optimistic-concurrency guard. When the client sends the hash it last
    // read, compare it against the current on-disk bytes and refuse the write
    // if the file changed underneath it. A missing file hashes as empty, so a
    // client that read real content and then finds the file gone also conflicts.
    if let Some(expected) = req.if_match.as_deref() {
        let current_hash = match tokio::fs::read(&p).await {
            Ok(cur) => hash_bytes(&cur),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => hash_bytes(&[]),
            Err(e) => return Err(ApiError::Io(e)),
        };
        if current_hash != expected {
            return Err(ApiError::Conflict(format!(
                "file changed on disk since it was read (expected {expected}, found {current_hash}); \
                 reload before saving to avoid clobbering newer content"
            )));
        }
    }

    let n = bytes.len();
    tokio::fs::write(&p, &bytes).await?;
    let hash = hash_bytes(&bytes);
    let mtime = tokio::fs::metadata(&p)
        .await
        .map(|m| mtime_millis(&m))
        .unwrap_or(0);
    Ok(Json(WriteFileResponse {
        ok: true,
        bytes: n,
        hash,
        mtime,
    }))
}

#[derive(Debug, Serialize)]
pub struct FileOpResponse {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum FileOpReq {
    Move {
        from: String,
        to: String,
        #[serde(default)]
        create_parents: bool,
    },
    Delete {
        path: String,
        #[serde(default)]
        recursive: bool,
    },
    Mkdir {
        path: String,
        #[serde(default)]
        parents: bool,
    },
    Duplicate {
        from: String,
        to: String,
        #[serde(default)]
        create_parents: bool,
    },
}

pub async fn operate(
    State(state): State<Arc<AppState>>,
    Json(req): Json<FileOpReq>,
) -> Result<Json<FileOpResponse>> {
    let root = &state.config.workspace_root;
    match req {
        FileOpReq::Move {
            from,
            to,
            create_parents,
        } => {
            require_nonempty_path(&from)?;
            require_nonempty_path(&to)?;
            let src = workspace_path::resolve_existing(root, &from)?;
            let dst = resolve_target(root, &to, create_parents).await?;
            reject_same_or_nested_destination(&src, &dst).await?;
            reject_existing_destination(&dst).await?;
            tokio::fs::rename(&src, &dst).await?;
            Ok(Json(FileOpResponse {
                ok: true,
                path: Some(rel_to(root, &dst)),
            }))
        }
        FileOpReq::Delete { path, recursive } => {
            require_nonempty_path(&path)?;
            let resolved = workspace_path::resolve_existing(root, &path)?;
            let meta = tokio::fs::metadata(&resolved).await?;
            if meta.is_dir() {
                if recursive {
                    tokio::fs::remove_dir_all(&resolved).await?;
                } else {
                    tokio::fs::remove_dir(&resolved).await?;
                }
            } else {
                tokio::fs::remove_file(&resolved).await?;
            }
            Ok(Json(FileOpResponse {
                ok: true,
                path: None,
            }))
        }
        FileOpReq::Mkdir { path, parents } => {
            require_nonempty_path(&path)?;
            let dst = resolve_target(root, &path, parents).await?;
            reject_existing_destination(&dst).await?;
            if parents {
                tokio::fs::create_dir_all(&dst).await?;
            } else {
                tokio::fs::create_dir(&dst).await?;
            }
            Ok(Json(FileOpResponse {
                ok: true,
                path: Some(rel_to(root, &dst)),
            }))
        }
        FileOpReq::Duplicate {
            from,
            to,
            create_parents,
        } => {
            require_nonempty_path(&from)?;
            require_nonempty_path(&to)?;
            let src = workspace_path::resolve_existing(root, &from)?;
            let dst = resolve_target(root, &to, create_parents).await?;
            reject_same_or_nested_destination(&src, &dst).await?;
            reject_existing_destination(&dst).await?;
            let meta = tokio::fs::metadata(&src).await?;
            if meta.is_dir() {
                copy_dir_recursive(&src, &dst).await?;
            } else {
                tokio::fs::copy(&src, &dst).await?;
            }
            Ok(Json(FileOpResponse {
                ok: true,
                path: Some(rel_to(root, &dst)),
            }))
        }
    }
}

fn require_nonempty_path(path: &str) -> Result<()> {
    if path.trim().is_empty() {
        return Err(ApiError::BadRequest("path must not be empty".into()));
    }
    Ok(())
}

async fn resolve_target(
    root: &Path,
    path: &str,
    create_parents: bool,
) -> Result<std::path::PathBuf> {
    if create_parents {
        let lexical = workspace_path::resolve_existing_or_lexical(root, path)?;
        if let Some(parent) = lexical.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
    }
    workspace_path::resolve_for_write(root, path)
}

async fn reject_existing_destination(path: &Path) -> Result<()> {
    match tokio::fs::metadata(path).await {
        Ok(_) => Err(ApiError::Conflict(format!(
            "destination already exists: {}",
            path.display()
        ))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(ApiError::Io(e)),
    }
}

async fn reject_same_or_nested_destination(src: &Path, dst: &Path) -> Result<()> {
    if src == dst {
        return Err(ApiError::Conflict(
            "source and destination are the same".into(),
        ));
    }
    let meta = tokio::fs::metadata(src).await?;
    if meta.is_dir() && dst.starts_with(src) {
        return Err(ApiError::BadRequest(
            "destination may not be inside the source directory".into(),
        ));
    }
    Ok(())
}

async fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    let mut stack = vec![(src.to_path_buf(), dst.to_path_buf())];
    while let Some((current_src, current_dst)) = stack.pop() {
        tokio::fs::create_dir(&current_dst).await?;
        let mut rd = tokio::fs::read_dir(&current_src).await?;
        while let Some(ent) = rd.next_entry().await? {
            let src_path = ent.path();
            let dst_path = current_dst.join(ent.file_name());
            let ft = ent.file_type().await?;
            if ft.is_dir() {
                stack.push((src_path, dst_path));
            } else {
                tokio::fs::copy(src_path, dst_path).await?;
            }
        }
    }
    Ok(())
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
    let dir = if q.path.trim().is_empty() {
        state.config.workspace_root.clone()
    } else {
        workspace_path::resolve_existing(&state.config.workspace_root, &q.path)?
    };
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
        // Don't follow symlinks during a tree walk — could escape the workspace
        // root via an outward-pointing link.
        if ft.is_symlink() {
            continue;
        }
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

const SEARCH_HARD_CAP: usize = 500;
const FILE_SEARCH_HARD_CAP: usize = 500;

/// Search via `rg --json`. Requires `rg` on $PATH (TOOLING.md lists it as a
/// baseline tool — fail loudly if it isn't installed).
pub async fn search(
    State(state): State<Arc<AppState>>,
    Query(q): Query<SearchQuery>,
) -> Result<Json<Vec<SearchHit>>> {
    if q.q.is_empty() {
        return Err(ApiError::BadRequest("q must not be empty".into()));
    }
    let dir = if q.path.trim().is_empty() {
        state.config.workspace_root.clone()
    } else {
        workspace_path::resolve_existing(&state.config.workspace_root, &q.path)?
    };
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

#[derive(Debug, Deserialize)]
pub struct FileSearchQuery {
    pub q: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub max: Option<usize>,
}

pub async fn search_files(
    State(state): State<Arc<AppState>>,
    Query(q): Query<FileSearchQuery>,
) -> Result<Json<Vec<FileSearchResult>>> {
    let needle = q.q.trim().to_ascii_lowercase();
    if needle.is_empty() {
        return Err(ApiError::BadRequest("q must not be empty".into()));
    }
    let dir = if q.path.trim().is_empty() {
        state.config.workspace_root.clone()
    } else {
        workspace_path::resolve_existing(&state.config.workspace_root, &q.path)?
    };
    let cap = q.max.unwrap_or(100).min(FILE_SEARCH_HARD_CAP);
    let mut stack = vec![dir.clone()];
    let mut out = Vec::new();

    while let Some(current) = stack.pop() {
        let mut rd = match tokio::fs::read_dir(&current).await {
            Ok(rd) => rd,
            Err(_) => continue,
        };
        while let Some(ent) = rd.next_entry().await? {
            let path = ent.path();
            let name = ent.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let ft = match ent.file_type().await {
                Ok(ft) => ft,
                Err(_) => continue,
            };
            if ft.is_symlink() {
                continue;
            }
            if ft.is_dir() {
                stack.push(path);
                continue;
            }
            if !ft.is_file() {
                continue;
            }
            let rel = rel_to(&state.config.workspace_root, &path);
            let rel_lc = rel.to_ascii_lowercase();
            if !name.to_ascii_lowercase().contains(&needle) && !rel_lc.contains(&needle) {
                continue;
            }
            out.push(FileSearchResult { path: rel, name });
            if out.len() >= cap {
                break;
            }
        }
        if out.len() >= cap {
            break;
        }
    }

    out.sort_by(|a, b| {
        let a_name = a.name.to_ascii_lowercase();
        let b_name = b.name.to_ascii_lowercase();
        let a_starts = a_name.starts_with(&needle);
        let b_starts = b_name.starts_with(&needle);
        match (a_starts, b_starts) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a
                .path
                .len()
                .cmp(&b.path.len())
                .then_with(|| a.path.cmp(&b.path)),
        }
    });

    Ok(Json(out))
}

fn looks_binary(b: &[u8]) -> bool {
    // Standard "is binary" heuristic: NUL byte in the first 8 KiB.
    let n = b.len().min(8192);
    b[..n].contains(&0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_detection() {
        assert!(!looks_binary(b"hello world"));
        assert!(looks_binary(b"hi\0there"));
    }

    #[test]
    fn file_search_prefers_prefix_and_shorter_paths() {
        let needle = "read".to_string();
        let mut results = [
            FileSearchResult {
                path: "docs/notes/readme.txt".into(),
                name: "readme.txt".into(),
            },
            FileSearchResult {
                path: "read-this-first.md".into(),
                name: "read-this-first.md".into(),
            },
            FileSearchResult {
                path: "src/bread.rs".into(),
                name: "bread.rs".into(),
            },
        ];
        results.sort_by(|a, b| {
            let a_name = a.name.to_ascii_lowercase();
            let b_name = b.name.to_ascii_lowercase();
            let a_starts = a_name.starts_with(&needle);
            let b_starts = b_name.starts_with(&needle);
            match (a_starts, b_starts) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => a
                    .path
                    .len()
                    .cmp(&b.path.len())
                    .then_with(|| a.path.cmp(&b.path)),
            }
        });
        assert_eq!(results[0].path, "read-this-first.md");
        assert_eq!(results[1].path, "docs/notes/readme.txt");
    }
}
