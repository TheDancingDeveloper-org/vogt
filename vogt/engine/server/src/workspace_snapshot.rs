//! The installed workspace tree, served to a session pod as a filtered
//! tarball (WI-4127).
//!
//! WI-3373/WI-3374 gave every session pod a read-only workspace tree, built by
//! a pod-local init container that cloned two repositories over HTTPS. That
//! premise was false: `agent-workspace-config` is private (the engine clones it
//! with a read-only deploy key) and the Forgejo skills repository refuses an
//! anonymous clone. Session pods must not receive either credential.
//!
//! The engine already holds the installed tree — its own init container writes
//! it into the shared agent-config `emptyDir` — so the engine serves a filtered
//! snapshot of that tree to its own pods instead:
//!
//! - `GET /api/agent-auth/sessions/{id}/workspace`, authenticated with the
//!   same per-pod bootstrap credential the session container presents. The
//!   check is read-only, so the init container's fetch does not spend the
//!   credential the session container still needs for `bootstrap`.
//! - The snapshot is an allowlisted set of paths — the two workspace guidance
//!   files, the installed rule/skill/agent directories, and the shared tool
//!   trees. Whole dot-directories are not allowed: `.claude` holds
//!   `settings.local.json` (exact commands, sometimes inline tokens) and
//!   `.codex` holds `auth.json`. Everything else in the engine's workspace --
//!   `Active/`, `.local/`, checkouts, caches -- is not part of the tree a
//!   session gets.
//! - `.git` directories, SSH keys, credential-helper and token files are never
//!   emitted, symlinks are never followed, and a symlink that resolves outside
//!   the source is an error rather than a silent omission. A size cap bounds
//!   memory; gzip is deterministic (fixed mtime, sorted entries, normalised
//!   modes) so an ETag is stable between identical builds. The built tarball
//!   is cached against a content fingerprint and builds are serialised, so
//!   concurrent pod starts do not rebuild the same tree in memory.

use std::{
    collections::HashSet,
    fmt, fs,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::{Component, Path, PathBuf},
    sync::{Mutex as StdMutex, OnceLock},
};

use axum::{
    body::Body,
    extract::{Path as AxumPath, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use bytes::Bytes;
use flate2::{write::GzEncoder, Compression};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{agent_stream::AgentAuth, app::AppState};

/// The route a session pod's init container fetches.
pub const WORKSPACE_ROUTE: &str = "/api/agent-auth/sessions/{id}/workspace";

/// The only paths a snapshot may carry, relative to the workspace root.
///
/// These are the paths `agent-workspace-config`'s `bootstrap/install.sh`
/// materialises: the two guidance files it writes, the rule, skill and agent
/// trees it copies, and the shared tools it syncs. `UsefulClaudeSkills` is a
/// checkout of the configuration repository, not an installed asset, and is
/// deliberately absent. `.agent-tools/{bin,lib,etc}` is the whole installed
/// tool tree (its `tests/` and caches are not needed in a session).
pub const SNAPSHOT_ALLOWLIST: &[&str] = &[
    "AGENTS.md",
    "CLAUDE.md",
    ".claude/rules",
    ".claude/skills",
    ".claude/agents",
    ".codex/skills",
    ".agent-tools/bin",
    ".agent-tools/lib",
    ".agent-tools/etc",
];

/// The default cap on the uncompressed snapshot.
pub const DEFAULT_MAX_SNAPSHOT_BYTES: u64 = 64 * 1024 * 1024;
/// An operator may raise the cap, but not without bound: the snapshot is built
/// in memory.
pub const HARD_MAX_SNAPSHOT_BYTES: u64 = 512 * 1024 * 1024;

/// Directories never walked, at any depth.
const EXCLUDED_DIRS: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    "node_modules",
    "target",
    ".venv",
    "__pycache__",
    ".pytest_cache",
    ".ruff_cache",
    ".mypy_cache",
    ".tox",
];

/// File or directory names never emitted, case-insensitively. The workspace
/// holds no credential, and the snapshot must not become one.
const EXCLUDED_NAMES: &[&str] = &[
    ".ssh",
    ".aws",
    ".kube",
    ".docker",
    ".netrc",
    ".git-credentials",
    ".gitconfig",
    ".npmrc",
    ".pypirc",
    ".pgpass",
    ".my.cnf",
    ".credentials.json",
    ".claude.json",
    ".env",
    "auth.json",
    "settings.json",
    "settings.local.json",
    "scheduled_tasks.lock",
    "credentials",
    "credentials.json",
    "id_rsa",
    "id_dsa",
    "id_ecdsa",
    "id_ed25519",
    "known_hosts",
    "authorized_keys",
];

const EXCLUDED_SUFFIXES: &[&str] = &[".pem", ".key", ".p12", ".pfx", ".token", ".kdbx", ".lock"];

#[derive(Debug)]
pub enum SnapshotError {
    /// The configured source is missing, is not a directory, or holds none of
    /// the allowed roots.
    Unavailable(String),
    /// The snapshot would exceed the deployment's cap.
    TooLarge {
        limit: u64,
    },
    /// A path is not a plain, relative, normalised path.
    UnsafePath(String),
    /// A symlink resolves outside the source tree.
    SymlinkEscape(String),
    /// A path does not fit the ustar name fields.
    PathTooLong(String),
    /// The tree holds more entries than the builder will emit.
    TooManyEntries {
        limit: usize,
    },
    Io(std::io::Error),
}

impl fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SnapshotError::Unavailable(why) => write!(f, "workspace snapshot unavailable: {why}"),
            SnapshotError::TooLarge { limit } => {
                write!(f, "workspace snapshot exceeds the {limit}-byte cap")
            }
            SnapshotError::UnsafePath(path) => {
                write!(f, "workspace snapshot refused an unsafe path: {path:?}")
            }
            SnapshotError::SymlinkEscape(path) => write!(
                f,
                "workspace snapshot refused a symlink that leaves the source: {path:?}"
            ),
            SnapshotError::PathTooLong(path) => {
                write!(f, "workspace snapshot path is too long for tar: {path:?}")
            }
            SnapshotError::TooManyEntries { limit } => {
                write!(f, "workspace snapshot exceeds the {limit}-entry cap")
            }
            SnapshotError::Io(e) => write!(f, "workspace snapshot I/O: {e}"),
        }
    }
}

impl std::error::Error for SnapshotError {}

impl From<std::io::Error> for SnapshotError {
    fn from(e: std::io::Error) -> Self {
        SnapshotError::Io(e)
    }
}

/// How many entries the builder will emit, so a pathological tree cannot make
/// the snapshot unbounded in time or memory.
const MAX_ENTRIES: usize = 20_000;

fn canonical_root(source: &Path) -> Result<PathBuf, SnapshotError> {
    let root = source
        .canonicalize()
        .map_err(|e| SnapshotError::Unavailable(format!("{}: {e}", source.display())))?;
    if !root.is_dir() {
        return Err(SnapshotError::Unavailable(format!(
            "{} is not a directory",
            root.display()
        )));
    }
    Ok(root)
}

/// Build the gzipped snapshot of `source`. Blocking; callers run it off the
/// async runtime. Entries are emitted in sorted order with fixed metadata, so
/// two builds of an unchanged tree are byte-identical.
pub fn build_tarball(source: &Path, max_bytes: u64) -> Result<Vec<u8>, SnapshotError> {
    let root = canonical_root(source)?;
    let mut tar = Vec::new();
    let mut total = 0u64;
    let count;
    {
        let mut visit =
            |rel: &Path, path: &Path, meta: &fs::Metadata| -> Result<(), SnapshotError> {
                if meta.is_dir() {
                    return append_header(&mut tar, rel, 0o755, 0, b'5');
                }
                if meta.len() > max_bytes.saturating_sub(total) {
                    return Err(SnapshotError::TooLarge { limit: max_bytes });
                }
                let data = fs::read(path)?;
                let len = data.len() as u64;
                if len > max_bytes.saturating_sub(total) {
                    return Err(SnapshotError::TooLarge { limit: max_bytes });
                }
                total += len;
                let mode = if meta.permissions().mode() & 0o111 != 0 {
                    // The owner's exec bit, carried as a normalised 0755.
                    0o755
                } else {
                    0o644
                };
                append_header(&mut tar, rel, mode, len, b'0')?;
                tar.extend_from_slice(&data);
                let pad = (512 - (data.len() % 512)) % 512;
                tar.resize(tar.len() + pad, 0);
                Ok(())
            };
        count = walk_allowlisted(&root, &mut visit)?;
    }
    if count == 0 {
        return Err(SnapshotError::Unavailable(format!(
            "{} holds none of {SNAPSHOT_ALLOWLIST:?}",
            root.display()
        )));
    }
    tar.extend_from_slice(&[0u8; 1024]);
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&tar)?;
    Ok(encoder.finish()?)
}

/// A stable fingerprint of exactly the bytes and paths a build would emit.
///
/// It is content-based, not mtime-based: `install.sh` copies assets with
/// `copy2`, which preserves timestamps, so a metadata-only key could serve a
/// stale tarball after an update.
pub fn fingerprint(source: &Path) -> Result<[u8; 32], SnapshotError> {
    let root = canonical_root(source)?;
    let mut hasher = Sha256::new();
    let count;
    {
        let mut visit =
            |rel: &Path, path: &Path, meta: &fs::Metadata| -> Result<(), SnapshotError> {
                hasher.update(rel.as_os_str().as_encoded_bytes());
                hasher.update([if meta.is_dir() { b'd' } else { b'f' }]);
                hasher.update((meta.permissions().mode() & 0o111).to_le_bytes());
                if meta.is_file() {
                    let data = fs::read(path)?;
                    hasher.update((data.len() as u64).to_le_bytes());
                    hasher.update(&data);
                }
                Ok(())
            };
        count = walk_allowlisted(&root, &mut visit)?;
    }
    if count == 0 {
        return Err(SnapshotError::Unavailable(format!(
            "{} holds none of {SNAPSHOT_ALLOWLIST:?}",
            root.display()
        )));
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&hasher.finalize());
    Ok(out)
}

type Visit<'a> = &'a mut dyn FnMut(&Path, &Path, &fs::Metadata) -> Result<(), SnapshotError>;

/// Walk only the allowlisted paths, visiting each emitted entry in sorted
/// order. Parent directories of an allowed path are emitted first so a tar
/// extractor does not have to invent them.
fn walk_allowlisted(root: &Path, visit: Visit<'_>) -> Result<usize, SnapshotError> {
    let mut count = 0usize;
    let mut emitted_parents = HashSet::new();
    for entry in SNAPSHOT_ALLOWLIST {
        let rel = Path::new(entry);
        let mut usable = true;
        let mut ancestors = Vec::new();
        if let Some(parent) = rel.parent() {
            let mut ancestor = PathBuf::new();
            for component in parent.components() {
                ancestor.push(component.as_os_str());
                let meta = match fs::symlink_metadata(root.join(&ancestor)) {
                    Ok(meta) => meta,
                    // An absent allowed root is not an error: the installed
                    // tree grows, and an older one may lack a directory.
                    Err(_) => {
                        usable = false;
                        break;
                    }
                };
                // A symlinked ancestor is never followed, even when it points
                // inside: the archive entry it would produce is not a path a
                // pod should receive.
                if meta.file_type().is_symlink() || !meta.is_dir() {
                    usable = false;
                    break;
                }
                ancestors.push(ancestor.clone());
            }
        }
        if !usable {
            continue;
        }
        let path = root.join(rel);
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        // Emit an allowed path's parents only when the path itself exists, so
        // a workspace that holds neither the file nor the directory stays
        // "unavailable" rather than serving a tarball of empty directories.
        for ancestor in ancestors {
            if emitted_parents.insert(ancestor.clone()) {
                let ancestor_meta = fs::symlink_metadata(root.join(&ancestor))?;
                bump(&mut count)?;
                visit(&ancestor, &root.join(&ancestor), &ancestor_meta)?;
            }
        }
        walk_tree(root, &path, rel, &meta, &mut *visit, &mut count)?;
    }
    Ok(count)
}

fn walk_tree(
    root: &Path,
    path: &Path,
    rel: &Path,
    meta: &fs::Metadata,
    visit: Visit<'_>,
    count: &mut usize,
) -> Result<(), SnapshotError> {
    validate_relative(rel)?;
    let file_type = meta.file_type();
    let name = rel
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| SnapshotError::UnsafePath(rel.display().to_string()))?;
    if file_type.is_symlink() {
        if symlink_escapes(root, path)? {
            return Err(SnapshotError::SymlinkEscape(rel.display().to_string()));
        }
        // A link that stays inside the tree is still never followed: a tarball
        // entry could point elsewhere when the archive is unpacked.
        return Ok(());
    }
    if file_type.is_dir() {
        if is_excluded_dir(name) {
            return Ok(());
        }
        bump(count)?;
        visit(rel, path, meta)?;
        let mut children = fs::read_dir(path)?.collect::<std::io::Result<Vec<_>>>()?;
        children.sort_by_key(|entry| entry.file_name());
        for child in children {
            let child_name = child.file_name();
            let Some(child_name) = child_name.to_str() else {
                return Err(SnapshotError::UnsafePath(rel.display().to_string()));
            };
            if is_excluded_name(child_name) {
                continue;
            }
            let child_rel = rel.join(child_name);
            validate_relative(&child_rel)?;
            let child_path = child.path();
            let child_meta = fs::symlink_metadata(&child_path)?;
            walk_tree(
                root,
                &child_path,
                &child_rel,
                &child_meta,
                &mut *visit,
                count,
            )?;
        }
        return Ok(());
    }
    if !file_type.is_file() || is_excluded_name(name) {
        return Ok(());
    }
    bump(count)?;
    visit(rel, path, meta)
}

fn bump(count: &mut usize) -> Result<(), SnapshotError> {
    *count += 1;
    if *count > MAX_ENTRIES {
        return Err(SnapshotError::TooManyEntries { limit: MAX_ENTRIES });
    }
    Ok(())
}

fn is_excluded_dir(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    EXCLUDED_DIRS.contains(&lower.as_str())
}

fn is_excluded_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    EXCLUDED_NAMES.contains(&lower.as_str())
        || lower.starts_with(".env.")
        || EXCLUDED_SUFFIXES.iter().any(|s| lower.ends_with(s))
}

/// Whether a symlink's target, resolved lexically, leaves `root`. The link is
/// never followed; this only decides whether an escaping one is an error
/// rather than a silent omission.
fn symlink_escapes(root: &Path, link: &Path) -> Result<bool, SnapshotError> {
    let target = fs::read_link(link)?;
    let joined = if target.is_absolute() {
        target
    } else {
        link.parent().unwrap_or(root).join(target)
    };
    Ok(!normalize_lexically(&joined).starts_with(root))
}

fn normalize_lexically(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::RootDir => out.push("/"),
            Component::Prefix(_) | Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(part) => out.push(part),
        }
    }
    out
}

fn append_header(
    out: &mut Vec<u8>,
    rel: &Path,
    mode: u32,
    size: u64,
    typeflag: u8,
) -> Result<(), SnapshotError> {
    let full = rel
        .to_str()
        .ok_or_else(|| SnapshotError::UnsafePath(rel.display().to_string()))?;
    let (name, prefix) = split_name(full, rel)?;
    let mut header = [0u8; 512];
    header[..name.len()].copy_from_slice(name.as_bytes());
    octal(&mut header[100..108], u64::from(mode));
    octal(&mut header[108..116], 0);
    octal(&mut header[116..124], 0);
    octal(&mut header[124..136], size);
    octal(&mut header[136..148], 0);
    // The checksum is computed with this field eight spaces, then written.
    header[148..156].copy_from_slice(b"        ");
    header[156] = typeflag;
    header[257..262].copy_from_slice(b"ustar");
    header[262] = 0;
    header[263..265].copy_from_slice(b"00");
    if !prefix.is_empty() {
        header[345..345 + prefix.len()].copy_from_slice(prefix.as_bytes());
    }
    let sum: u64 = header.iter().map(|byte| u64::from(*byte)).sum();
    let text = format!("{sum:06o}");
    header[148..154].copy_from_slice(text.as_bytes());
    header[154] = 0;
    header[155] = b' ';
    out.extend_from_slice(&header);
    Ok(())
}

/// Split a path into ustar's 100-byte name and 155-byte prefix fields at a
/// slash, preferring the longest prefix.
fn split_name<'a>(full: &'a str, rel: &Path) -> Result<(&'a str, &'a str), SnapshotError> {
    if full.len() <= 100 {
        return Ok((full, ""));
    }
    if full.len() > 255 {
        return Err(SnapshotError::PathTooLong(rel.display().to_string()));
    }
    for (index, _) in full.match_indices('/').rev() {
        let (prefix, name) = (&full[..index], &full[index + 1..]);
        if prefix.len() <= 155 && !name.is_empty() && name.len() <= 100 {
            return Ok((name, prefix));
        }
    }
    Err(SnapshotError::PathTooLong(rel.display().to_string()))
}

/// Write `value` as zero-padded octal into all but the last byte of `field`,
/// which becomes NUL — the ustar numeric encoding.
fn octal(field: &mut [u8], value: u64) {
    let width = field.len() - 1;
    let text = format!("{value:0>width$o}", width = width);
    debug_assert!(text.len() <= width);
    let text = if text.len() > width {
        &text[text.len() - width..]
    } else {
        &text[..]
    };
    let padded = format!("{text:0>width$}", width = width);
    field[..width].copy_from_slice(padded.as_bytes());
    field[width] = 0;
}

/// Reject anything that is not a plain relative path without `.`, `..`, or a
/// root. The walk generates these itself; this is the second line of defence
/// at the one place a name becomes an archive entry.
pub fn validate_relative(path: &Path) -> Result<(), SnapshotError> {
    if path.as_os_str().is_empty() {
        return Err(SnapshotError::UnsafePath(path.display().to_string()));
    }
    for component in path.components() {
        match component {
            Component::Normal(_) => {}
            _ => return Err(SnapshotError::UnsafePath(path.display().to_string())),
        }
    }
    Ok(())
}

struct CachedSnapshot {
    source: PathBuf,
    limit: u64,
    fingerprint: [u8; 32],
    bytes: Bytes,
    etag: String,
}

/// The last built snapshot, keyed by source, cap and a content fingerprint.
///
/// One build runs at a time: every pod start asks for the same tree, and a
/// fetch must never multiply a 64 MiB tarball in memory. A request that loses
/// the race waits for the build rather than starting another.
#[derive(Default)]
pub struct SnapshotCache {
    gate: tokio::sync::Mutex<()>,
    cached: StdMutex<Option<CachedSnapshot>>,
    #[cfg(test)]
    builds: std::sync::atomic::AtomicUsize,
}

impl SnapshotCache {
    /// The built tarball and its ETag for `source`.
    pub async fn get(&self, source: &Path, limit: u64) -> Result<(Bytes, String), SnapshotError> {
        let _gate = self.gate.lock().await;
        let source = source.to_path_buf();
        let probe = source.clone();
        let fingerprint = tokio::task::spawn_blocking(move || fingerprint(&probe))
            .await
            .map_err(|e| {
                SnapshotError::Unavailable(format!("snapshot fingerprint task failed: {e}"))
            })??;
        {
            let cached = self.cached.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(cached) = cached.as_ref() {
                if cached.source == source
                    && cached.limit == limit
                    && cached.fingerprint == fingerprint
                {
                    return Ok((cached.bytes.clone(), cached.etag.clone()));
                }
            }
        }
        let build_source = source.clone();
        let bytes = tokio::task::spawn_blocking(move || build_tarball(&build_source, limit))
            .await
            .map_err(|e| SnapshotError::Unavailable(format!("snapshot task failed: {e}")))??;
        let bytes = Bytes::from(bytes);
        #[cfg(test)]
        self.builds
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let etag = format!("\"{}\"", hex(&Sha256::digest(&bytes[..])));
        let mut cached = self.cached.lock().unwrap_or_else(|e| e.into_inner());
        *cached = Some(CachedSnapshot {
            source,
            limit,
            fingerprint,
            bytes: bytes.clone(),
            etag: etag.clone(),
        });
        Ok((bytes, etag))
    }

    #[cfg(test)]
    fn build_count(&self) -> usize {
        self.builds.load(std::sync::atomic::Ordering::SeqCst)
    }
}

fn snapshot_cache() -> &'static SnapshotCache {
    static CACHE: OnceLock<SnapshotCache> = OnceLock::new();
    CACHE.get_or_init(SnapshotCache::default)
}

async fn authorize(auth: &AgentAuth, id: Uuid, headers: &HeaderMap) -> Result<(), StatusCode> {
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .filter(|v| !v.is_empty())
        .ok_or(StatusCode::UNAUTHORIZED)?;
    if auth.bootstrap_ok(id, token.as_bytes()).await {
        Ok(())
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}

/// Serve the workspace snapshot to a session pod. The credential is the pod's
/// one-use bootstrap token, checked without consuming it.
pub async fn workspace(
    State(state): State<std::sync::Arc<AppState>>,
    AxumPath(id): AxumPath<Uuid>,
    headers: HeaderMap,
) -> Response {
    if let Err(status) = authorize(&state.agent_auth, id, &headers).await {
        return status.into_response();
    }
    let cfg = match state.sessions.pod_config() {
        Ok(cfg) => cfg,
        Err(e) => return (StatusCode::SERVICE_UNAVAILABLE, e.to_string()).into_response(),
    };
    let Some(workspace) = cfg.workspace else {
        return (StatusCode::NOT_FOUND, "the workspace tree is not enabled").into_response();
    };
    let source = PathBuf::from(workspace.source);
    let limit = workspace.max_bytes;
    let (bytes, etag) = match snapshot_cache().get(&source, limit).await {
        Ok(built) => built,
        Err(e) => {
            tracing::warn!(session = %id, error = %e, "could not build the workspace snapshot");
            return (StatusCode::SERVICE_UNAVAILABLE, e.to_string()).into_response();
        }
    };
    if headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v == etag || v == "*")
    {
        return Response::builder()
            .status(StatusCode::NOT_MODIFIED)
            .header(header::ETAG, &etag)
            .body(Body::empty())
            .expect("valid response");
    }
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/gzip")
        .header(
            header::CONTENT_DISPOSITION,
            "attachment; filename=\"workspace.tar.gz\"",
        )
        .header(header::ETAG, etag)
        .header(header::CACHE_CONTROL, "private, max-age=300")
        .body(Body::from(bytes))
        .expect("valid response")
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use fmt::Write as _;
        let _ = write!(out, "{b:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Read;
    use std::os::unix::fs::{symlink, PermissionsExt};
    use tempfile::TempDir;

    fn write(root: &Path, rel: &str, body: &str) {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    fn gunzip(bytes: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        flate2::read::GzDecoder::new(bytes)
            .read_to_end(&mut out)
            .unwrap();
        out
    }

    /// Just enough of ustar to inspect what the builder emitted.
    fn entries(bytes: &[u8]) -> Vec<(String, u8, u32)> {
        let tar = gunzip(bytes);
        let mut out = Vec::new();
        let mut at = 0usize;
        while at + 512 <= tar.len() {
            let header = &tar[at..at + 512];
            if header.iter().all(|b| *b == 0) {
                break;
            }
            let field = |range: std::ops::Range<usize>| -> String {
                let raw = &header[range];
                let end = raw.iter().position(|b| *b == 0).unwrap_or(raw.len());
                String::from_utf8_lossy(&raw[..end]).into_owned()
            };
            let name = field(0..100);
            let prefix = field(345..500);
            let name = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            let mode = u32::from_str_radix(field(100..108).trim(), 8).unwrap_or(0);
            let typeflag = header[156];
            let size = usize::from_str_radix(field(124..136).trim(), 8).unwrap_or(0);
            out.push((name, typeflag, mode));
            at += 512 + size.div_ceil(512) * 512;
        }
        out
    }

    fn names(bytes: &[u8]) -> Vec<String> {
        entries(bytes)
            .into_iter()
            .map(|(name, _, _)| name)
            .collect()
    }

    #[test]
    fn excludes_secrets_git_and_unlisted_dotfiles() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        write(root, "AGENTS.md", "guide");
        write(root, "CLAUDE.md", "pointer");
        write(root, ".claude/rules/rule.md", "rule");
        write(root, ".claude/skills/s/SKILL.md", "skill");
        write(root, ".claude/agents/reviewer.md", "agent");
        write(root, ".codex/skills/c/SKILL.md", "skill");
        write(root, ".agent-tools/bin/redact", "#!/bin/sh\n");
        fs::set_permissions(
            root.join(".agent-tools/bin/redact"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        write(root, ".agent-tools/lib/estate_ci/mod.py", "lib");
        write(root, ".agent-tools/etc/tracker.toml", "etc");
        // Excluded: unlisted top-level entries, other dotfiles, and secrets.
        write(root, "notes.txt", "not workspace");
        write(root, "Active/WI/x", "checkout");
        write(root, "agent-workspace-config/AGENTS.md", "unlisted");
        write(
            root,
            "UsefulClaudeSkills/u/SKILL.md",
            "a checkout, not a skill",
        );
        write(root, ".ssh/id_ed25519", "key");
        write(root, ".env", "TOKEN=1");
        // Claude Code's local permission list records exact commands and can
        // carry inline tokens; the other names hold agent credentials and
        // process state that must not reach a pod.
        write(root, ".claude/settings.json", "{}");
        write(root, ".claude/settings.local.json", "{\"permissions\":{}}");
        write(root, ".claude/history/h.jsonl", "session");
        write(root, ".claude/.credentials.json", "{}");
        write(root, ".claude/.git/config", "[remote]");
        write(root, "UsefulClaudeSkills/.git/config", "[remote]");
        write(root, ".claude/id_rsa", "key");
        write(root, ".codex/auth.json", "{\"token\":\"x\"}");
        write(root, ".codex/config.toml", "model = \"x\"");
        write(root, ".codex/scheduled_tasks.lock", "pid");
        write(root, ".codex/credentials.json", "{}");
        write(root, ".claude/skills/s/.ruff_cache/v", "cache");
        write(root, ".agent-tools/bin/__pycache__/x.pyc", "bytecode");
        write(root, ".agent-tools/.pytest_cache/v/cache", "cache");
        write(root, ".agent-tools/bin/tool.lock", "lock");
        write(root, ".agent-tools/release.pem", "key");

        let built = build_tarball(root, 16 * 1024 * 1024).unwrap();
        let names = names(&built);
        for want in [
            "AGENTS.md",
            "CLAUDE.md",
            ".claude/rules/rule.md",
            ".claude/skills/s/SKILL.md",
            ".claude/agents/reviewer.md",
            ".codex/skills/c/SKILL.md",
            ".agent-tools/bin/redact",
            ".agent-tools/lib/estate_ci/mod.py",
            ".agent-tools/etc/tracker.toml",
        ] {
            assert!(names.iter().any(|n| n == want), "missing {want}: {names:?}");
        }
        for unwanted in [
            "notes.txt",
            "Active/WI/x",
            "agent-workspace-config/AGENTS.md",
            "UsefulClaudeSkills/u/SKILL.md",
            ".ssh/id_ed25519",
            ".env",
            ".claude/settings.json",
            ".claude/settings.local.json",
            ".claude/history/h.jsonl",
            ".claude/.credentials.json",
            ".claude/.git/config",
            "UsefulClaudeSkills/.git/config",
            ".claude/id_rsa",
            ".codex/auth.json",
            ".codex/config.toml",
            ".codex/scheduled_tasks.lock",
            ".codex/credentials.json",
            ".claude/skills/s/.ruff_cache/v",
            ".agent-tools/bin/__pycache__/x.pyc",
            ".agent-tools/.pytest_cache/v/cache",
            ".agent-tools/bin/tool.lock",
            ".agent-tools/release.pem",
        ] {
            assert!(
                !names.iter().any(|n| n == unwanted),
                "included {unwanted}: {names:?}"
            );
        }
        let redact = entries(&built)
            .into_iter()
            .find(|(name, _, _)| name == ".agent-tools/bin/redact")
            .unwrap();
        assert_eq!(redact.2, 0o755, "the exec bit is preserved");
    }

    #[tokio::test]
    async fn caches_the_build_until_the_source_changes() {
        let dir = TempDir::new().unwrap();
        write(dir.path(), ".claude/rules/r.md", "one");
        write(dir.path(), ".agent-tools/bin/tool", "#!/bin/sh\n");
        let cache = SnapshotCache::default();

        let (first, first_etag) = cache.get(dir.path(), 1024 * 1024).await.unwrap();
        let (again, again_etag) = cache.get(dir.path(), 1024 * 1024).await.unwrap();
        assert_eq!(first_etag, again_etag);
        assert_eq!(&again[..], &first[..]);
        assert_eq!(cache.build_count(), 1, "an unchanged source is not rebuilt");

        write(dir.path(), ".claude/rules/r.md", "two");
        let (changed, changed_etag) = cache.get(dir.path(), 1024 * 1024).await.unwrap();
        assert_ne!(first_etag, changed_etag);
        assert_ne!(&first[..], &changed[..]);
        assert_eq!(cache.build_count(), 2, "a changed source is rebuilt");
    }

    #[test]
    fn rejects_a_symlink_that_escapes_the_snapshot() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join(".claude/rules")).unwrap();
        symlink("/etc/passwd", root.join(".claude/rules/escape")).unwrap();
        let error = build_tarball(root, 1024 * 1024).unwrap_err();
        assert!(matches!(error, SnapshotError::SymlinkEscape(_)), "{error}");
    }

    #[test]
    fn skips_an_in_tree_symlink() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        write(root, ".claude/rules/real.md", "real");
        symlink("real.md", root.join(".claude/rules/link.md")).unwrap();
        let names = names(&build_tarball(root, 1024 * 1024).unwrap());
        assert!(names.iter().any(|n| n == ".claude/rules/real.md"));
        assert!(
            !names.iter().any(|n| n == ".claude/rules/link.md"),
            "symlinks are not emitted: {names:?}"
        );
    }

    #[test]
    fn rejects_paths_that_are_not_plain_and_relative() {
        for bad in ["", "../x", "/etc/passwd", "a/../../b", "./a"] {
            assert!(validate_relative(Path::new(bad)).is_err(), "{bad:?}");
        }
        assert!(validate_relative(Path::new("a/b.md")).is_ok());
    }

    #[test]
    fn enforces_the_size_cap() {
        let dir = TempDir::new().unwrap();
        write(dir.path(), "AGENTS.md", &"x".repeat(1024));
        let error = build_tarball(dir.path(), 512).unwrap_err();
        assert!(matches!(error, SnapshotError::TooLarge { .. }), "{error}");
    }

    #[test]
    fn an_empty_source_is_unavailable_not_an_empty_tarball() {
        let dir = TempDir::new().unwrap();
        let error = build_tarball(dir.path(), 1024 * 1024).unwrap_err();
        assert!(matches!(error, SnapshotError::Unavailable(_)), "{error}");
        let error = build_tarball(&dir.path().join("missing"), 1024).unwrap_err();
        assert!(matches!(error, SnapshotError::Unavailable(_)), "{error}");
    }

    #[test]
    fn an_unlisted_dot_directory_is_not_a_snapshot() {
        let dir = TempDir::new().unwrap();
        write(dir.path(), ".claude/settings.local.json", "{}");
        let error = build_tarball(dir.path(), 1024 * 1024).unwrap_err();
        assert!(matches!(error, SnapshotError::Unavailable(_)), "{error}");
    }

    #[test]
    fn the_snapshot_is_deterministic() {
        let dir = TempDir::new().unwrap();
        write(dir.path(), "AGENTS.md", "a");
        write(dir.path(), ".claude/rules/r.md", "r");
        let first = build_tarball(dir.path(), 1024 * 1024).unwrap();
        let second = build_tarball(dir.path(), 1024 * 1024).unwrap();
        assert_eq!(first, second);
    }

    #[tokio::test]
    async fn the_route_requires_the_pods_bootstrap_credential() {
        let auth = AgentAuth::default();
        let id = Uuid::new_v4();
        auth.issue(id, b"right").await;
        let mut headers = HeaderMap::new();
        assert_eq!(
            authorize(&auth, id, &headers).await,
            Err(StatusCode::UNAUTHORIZED)
        );
        headers.insert(header::AUTHORIZATION, "Bearer wrong".parse().unwrap());
        assert_eq!(
            authorize(&auth, id, &headers).await,
            Err(StatusCode::UNAUTHORIZED)
        );
        headers.insert(header::AUTHORIZATION, "Bearer right".parse().unwrap());
        assert_eq!(authorize(&auth, id, &headers).await, Ok(()));
        // The check is read-only: the session container's bootstrap still
        // consumes the same credential afterwards.
        assert_eq!(
            authorize(&auth, Uuid::new_v4(), &headers).await,
            Err(StatusCode::UNAUTHORIZED)
        );
        assert!(auth.consume(id, b"right").await);
    }
}
