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
//! - The snapshot is an allowlisted top-level set (`AGENTS.md`, `CLAUDE.md`,
//!   `.claude`, `.codex`, `.agent-tools`, `UsefulClaudeSkills`). Everything
//!   else in the engine's workspace — `Active/`, `.local/`, other dotfiles —
//!   is not part of the tree a session gets.
//! - `.git` directories, SSH keys, credential-helper and token files are never
//!   emitted, symlinks are never followed, and a symlink that resolves outside
//!   the source is an error rather than a silent omission. A size cap bounds
//!   memory; gzip is deterministic (fixed mtime, sorted entries, normalised
//!   modes) so an ETag is stable between identical builds.

use std::{
    fmt, fs,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::{Component, Path, PathBuf},
};

use axum::{
    body::Body,
    extract::{Path as AxumPath, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use flate2::{write::GzEncoder, Compression};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{agent_stream::AgentAuth, app::AppState};

/// The route a session pod's init container fetches.
pub const WORKSPACE_ROUTE: &str = "/api/agent-auth/sessions/{id}/workspace";

/// The only top-level names a snapshot may carry. Everything else in the
/// engine's workspace is engine state, a checkout, or unrelated material.
pub const SNAPSHOT_ROOTS: &[&str] = &[
    "AGENTS.md",
    "CLAUDE.md",
    ".claude",
    ".codex",
    ".agent-tools",
    "UsefulClaudeSkills",
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
    ".env",
    "credentials",
    "credentials.json",
    "id_rsa",
    "id_dsa",
    "id_ecdsa",
    "id_ed25519",
    "known_hosts",
    "authorized_keys",
];

const EXCLUDED_SUFFIXES: &[&str] = &[".pem", ".key", ".p12", ".pfx", ".token", ".kdbx"];

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

/// Build the gzipped snapshot of `source`. Blocking; callers run it off the
/// async runtime. Entries are emitted in sorted order with fixed metadata, so
/// two builds of an unchanged tree are byte-identical.
pub fn build_tarball(source: &Path, max_bytes: u64) -> Result<Vec<u8>, SnapshotError> {
    let root = source
        .canonicalize()
        .map_err(|e| SnapshotError::Unavailable(format!("{}: {e}", source.display())))?;
    if !root.is_dir() {
        return Err(SnapshotError::Unavailable(format!(
            "{} is not a directory",
            root.display()
        )));
    }
    let mut tar = Vec::new();
    let mut total = 0u64;
    let mut count = 0usize;
    for name in SNAPSHOT_ROOTS {
        let path = root.join(name);
        // An absent allowed root is not an error: the installed tree grows.
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        append_tree(
            &root,
            &path,
            Path::new(name),
            &meta,
            &mut tar,
            &mut total,
            max_bytes,
            &mut count,
        )?;
    }
    if count == 0 {
        return Err(SnapshotError::Unavailable(format!(
            "{} holds none of {SNAPSHOT_ROOTS:?}",
            root.display()
        )));
    }
    tar.extend_from_slice(&[0u8; 1024]);
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&tar)?;
    Ok(encoder.finish()?)
}

#[allow(clippy::too_many_arguments)]
fn append_tree(
    root: &Path,
    path: &Path,
    rel: &Path,
    meta: &fs::Metadata,
    tar: &mut Vec<u8>,
    total: &mut u64,
    max_bytes: u64,
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
        append_header(tar, rel, 0o755, 0, b'5')?;
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
            append_tree(
                root,
                &child_path,
                &child_rel,
                &child_meta,
                tar,
                total,
                max_bytes,
                count,
            )?;
        }
        return Ok(());
    }
    if !file_type.is_file() || is_excluded_name(name) {
        return Ok(());
    }
    bump(count)?;
    if meta.len() > max_bytes.saturating_sub(*total) {
        return Err(SnapshotError::TooLarge { limit: max_bytes });
    }
    let data = fs::read(path)?;
    let len = data.len() as u64;
    if len > max_bytes.saturating_sub(*total) {
        return Err(SnapshotError::TooLarge { limit: max_bytes });
    }
    *total += len;
    let mode = if meta.permissions().mode() & 0o111 != 0 {
        // The owner's exec bit, carried as a normalised 0755.
        0o755
    } else {
        0o644
    };
    append_header(tar, rel, mode, len, b'0')?;
    tar.extend_from_slice(&data);
    let pad = (512 - (data.len() % 512)) % 512;
    tar.resize(tar.len() + pad, 0);
    Ok(())
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
    let built = tokio::task::spawn_blocking(move || build_tarball(&source, limit)).await;
    let bytes = match built {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(e)) => {
            tracing::warn!(session = %id, error = %e, "could not build the workspace snapshot");
            return (StatusCode::SERVICE_UNAVAILABLE, e.to_string()).into_response();
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("snapshot task failed: {e}"),
            )
                .into_response()
        }
    };
    let etag = format!("\"{}\"", hex(&Sha256::digest(&bytes)));
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
        write(root, ".codex/skills/c/SKILL.md", "skill");
        write(root, ".agent-tools/bin/redact", "#!/bin/sh\n");
        fs::set_permissions(
            root.join(".agent-tools/bin/redact"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        write(root, "UsefulClaudeSkills/u/SKILL.md", "skill");
        // Excluded: unlisted top-level entries, other dotfiles, and secrets.
        write(root, "notes.txt", "not workspace");
        write(root, "Active/WI/x", "checkout");
        write(root, "agent-workspace-config/AGENTS.md", "unlisted");
        write(root, ".ssh/id_ed25519", "key");
        write(root, ".env", "TOKEN=1");
        write(root, ".claude/.credentials.json", "{}");
        write(root, ".claude/.git/config", "[remote]");
        write(root, "UsefulClaudeSkills/.git/config", "[remote]");
        write(root, ".claude/id_rsa", "key");
        write(root, ".codex/credentials.json", "{}");
        write(root, ".agent-tools/release.pem", "key");

        let built = build_tarball(root, 16 * 1024 * 1024).unwrap();
        let names = names(&built);
        for want in [
            "AGENTS.md",
            "CLAUDE.md",
            ".claude/rules/rule.md",
            ".claude/skills/s/SKILL.md",
            ".codex/skills/c/SKILL.md",
            ".agent-tools/bin/redact",
            "UsefulClaudeSkills/u/SKILL.md",
        ] {
            assert!(names.iter().any(|n| n == want), "missing {want}: {names:?}");
        }
        for unwanted in [
            "notes.txt",
            "Active/WI/x",
            "agent-workspace-config/AGENTS.md",
            ".ssh/id_ed25519",
            ".env",
            ".claude/.credentials.json",
            ".claude/.git/config",
            "UsefulClaudeSkills/.git/config",
            ".claude/id_rsa",
            ".codex/credentials.json",
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
