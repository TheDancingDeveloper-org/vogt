//! File operations the engine proxies to a pod over the session stream
//! (WI-4866, part of WI-4863). The engine sends [`crate::Frame::FileOp`] and
//! the agent answers with [`crate::Frame::FileResult`] carrying the same `id`.
//!
//! Every path is relative to the pod's work root (`VOGT_WORK_DIR`, default
//! `/work`) and goes through the engine's own `workspace_path` rules, shared
//! from the contract crate: `..`, absolute paths and symlinks that leave the
//! root are refused, and [`may_show`] refuses hidden and credential-looking
//! paths for every op, reads and writes alike. Listings leave such entries out.

use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};
use vogt_engine_contract::workspace_path::{
    may_show, resolve_existing, resolve_for_write, resolve_no_follow, PathError,
};

/// The largest file `read` serves, in total across its chunks.
pub const READ_MAX_BYTES: u64 = 16 * 1024 * 1024;
/// The largest chunk one `read` reply carries; also the default chunk.
pub const READ_CHUNK_MAX: u64 = 256 * 1024;
/// The largest body one `write` frame may carry.
pub const WRITE_MAX_BYTES: usize = 4 * 1024 * 1024;
/// The largest chunk one `upload` frame may carry.
pub const UPLOAD_CHUNK_MAX: usize = 1024 * 1024;
/// The largest file an `upload` may build.
pub const UPLOAD_MAX_BYTES: u64 = 256 * 1024 * 1024;
/// The most entries one `list` returns.
pub const LIST_MAX_ENTRIES: usize = 5000;

/// One file operation, engine to agent.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum FileOp {
    /// The directory's entries, hidden and credential names left out.
    List {
        path: String,
    },
    Stat {
        path: String,
    },
    /// Up to `len` bytes (capped at [`READ_CHUNK_MAX`]) from `offset`. Files
    /// over [`READ_MAX_BYTES`] are refused.
    Read {
        path: String,
        #[serde(default)]
        offset: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        len: Option<u64>,
    },
    /// Replace the file's contents in one frame.
    Write {
        path: String,
        data: Vec<u8>,
        #[serde(default)]
        create_parents: bool,
    },
    /// One chunk of a larger file. Chunks build a temp sibling of `path`
    /// (see [`upload_temp`]); the target is only replaced, by a rename, when
    /// the `last` chunk lands. `offset` 0 starts the temp file afresh; every
    /// later chunk must start where it currently ends. `if_match` (the
    /// SHA-256 hex the client last read, the empty hash for a file that
    /// should not exist) is checked against the target at rename time.
    Upload {
        path: String,
        offset: u64,
        data: Vec<u8>,
        #[serde(default)]
        create_parents: bool,
        /// Required: a chunk without it is refused (see [`upload`]). Kept an
        /// `Option` so a missing flag gets a clear error, not a decode failure.
        #[serde(default)]
        last: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        if_match: Option<String>,
    },
    /// Drop the temp file of an upload that will not finish.
    UploadAbort {
        path: String,
    },
    Mkdir {
        path: String,
        #[serde(default)]
        parents: bool,
    },
    Move {
        from: String,
        to: String,
    },
    Copy {
        from: String,
        to: String,
    },
    /// A file, a symlink (the link, not its target) or a directory tree.
    Delete {
        path: String,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    File,
    Dir,
    Symlink,
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileEntry {
    pub name: String,
    /// Relative to the work root, `/`-separated.
    pub path: String,
    pub kind: EntryKind,
    pub size: u64,
    /// Seconds since the Unix epoch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified: Option<i64>,
}

/// The answer to one [`FileOp`], agent to engine.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum FileOpResult {
    Entries {
        entries: Vec<FileEntry>,
        #[serde(default)]
        truncated: bool,
    },
    Stat {
        entry: FileEntry,
    },
    Data {
        offset: u64,
        data: Vec<u8>,
        size: u64,
        eof: bool,
    },
    /// A write or upload chunk landed; `size` is the file's length now (for an
    /// upload, the temp file's, until the last chunk renames it into place).
    Written {
        size: u64,
    },
    Done,
    Error {
        code: FileErrorCode,
        message: String,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FileErrorCode {
    /// The path breaks a confinement or exposure rule.
    Refused,
    NotFound,
    TooLarge,
    /// The request does not fit the target (a read of a directory, an
    /// upload chunk out of order, a move onto an existing path).
    Invalid,
    /// The target changed since the client read it (`if_match` mismatch).
    Conflict,
    Io,
}

type OpResult<T> = Result<T, FileOpResult>;

fn err(code: FileErrorCode, message: impl Into<String>) -> FileOpResult {
    FileOpResult::Error {
        code,
        message: message.into(),
    }
}

fn refused(e: PathError) -> FileOpResult {
    let code = if e.0.contains("No such file") || e.0.contains("does not exist") {
        FileErrorCode::NotFound
    } else {
        FileErrorCode::Refused
    };
    err(code, e.0)
}

fn io(e: std::io::Error) -> FileOpResult {
    let code = if e.kind() == std::io::ErrorKind::NotFound {
        FileErrorCode::NotFound
    } else {
        FileErrorCode::Io
    };
    err(code, e.to_string())
}

/// The work root the agent serves: `VOGT_WORK_DIR`, else `/work`, canonical.
pub fn work_root() -> std::io::Result<PathBuf> {
    std::env::var_os("VOGT_WORK_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(crate::DEFAULT_WORK_DIR))
        .canonicalize()
}

/// A request path must be relative. The shared resolver strips a leading `/`
/// for the engine's compatibility; the stream contract refuses it instead.
fn relative(requested: &str) -> OpResult<&str> {
    if requested.starts_with('/') || Path::new(requested).is_absolute() {
        return Err(err(FileErrorCode::Refused, "path must be relative"));
    }
    Ok(requested)
}

fn existing(root: &Path, requested: &str) -> OpResult<PathBuf> {
    let path = resolve_existing(root, relative(requested)?).map_err(refused)?;
    may_show(root, &path).map_err(refused)?;
    Ok(path)
}

fn no_follow(root: &Path, requested: &str) -> OpResult<PathBuf> {
    let path = resolve_no_follow(root, relative(requested)?).map_err(refused)?;
    may_show(root, &path).map_err(refused)?;
    Ok(path)
}

fn for_write(root: &Path, requested: &str, create_parents: bool) -> OpResult<PathBuf> {
    let requested = relative(requested)?;
    if create_parents {
        if let Some(parent) = Path::new(requested).parent() {
            mkdir_parents(root, &parent.to_string_lossy())?;
        }
    }
    let path = resolve_for_write(root, requested).map_err(refused)?;
    may_show(root, &path).map_err(refused)?;
    Ok(path)
}

/// `mkdir -p` one component at a time, so every step is checked under the root.
fn mkdir_parents(root: &Path, requested: &str) -> OpResult<PathBuf> {
    let mut so_far = PathBuf::new();
    for comp in Path::new(requested).components() {
        match comp {
            Component::Normal(s) => so_far.push(s),
            Component::CurDir => continue,
            _ => return Err(err(FileErrorCode::Refused, "path contains '..' or a root")),
        }
        let rel = so_far.to_string_lossy();
        let target = resolve_for_write(root, &rel).map_err(refused)?;
        may_show(root, &target).map_err(refused)?;
        match fs::symlink_metadata(&target) {
            Ok(m) if m.is_dir() => {}
            Ok(_) => {
                return Err(err(
                    FileErrorCode::Invalid,
                    format!("{rel:?} is not a directory"),
                ))
            }
            Err(_) => fs::create_dir(&target).map_err(io)?,
        }
    }
    resolve_existing(root, &so_far.to_string_lossy()).map_err(refused)
}

fn entry(root: &Path, path: &Path) -> OpResult<FileEntry> {
    let meta = fs::symlink_metadata(path).map_err(io)?;
    let ft = meta.file_type();
    let kind = if ft.is_symlink() {
        EntryKind::Symlink
    } else if ft.is_dir() {
        EntryKind::Dir
    } else if ft.is_file() {
        EntryKind::File
    } else {
        EntryKind::Other
    };
    let rel = path.strip_prefix(root).unwrap_or(path);
    Ok(FileEntry {
        name: path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        path: rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/"),
        kind,
        size: meta.len(),
        modified: meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64),
    })
}

fn list(root: &Path, path: &str) -> OpResult<FileOpResult> {
    let dir = if path.is_empty() || path == "." {
        root.to_path_buf()
    } else {
        existing(root, path)?
    };
    if !dir.is_dir() {
        return Err(err(FileErrorCode::Invalid, "not a directory"));
    }
    let mut entries = Vec::new();
    let mut truncated = false;
    for item in fs::read_dir(&dir).map_err(io)? {
        let item = item.map_err(io)?;
        let child = item.path();
        if may_show(root, &child).is_err() {
            continue;
        }
        if entries.len() == LIST_MAX_ENTRIES {
            truncated = true;
            break;
        }
        entries.push(entry(root, &child)?);
    }
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(FileOpResult::Entries { entries, truncated })
}

fn read(root: &Path, path: &str, offset: u64, len: Option<u64>) -> OpResult<FileOpResult> {
    let file = existing(root, path)?;
    let meta = fs::metadata(&file).map_err(io)?;
    if !meta.is_file() {
        return Err(err(FileErrorCode::Invalid, "not a regular file"));
    }
    let size = meta.len();
    if size > READ_MAX_BYTES {
        return Err(err(
            FileErrorCode::TooLarge,
            format!("file is {size} bytes; the read cap is {READ_MAX_BYTES}"),
        ));
    }
    let want = len.unwrap_or(READ_CHUNK_MAX).min(READ_CHUNK_MAX);
    let start = offset.min(size);
    let mut handle = fs::File::open(&file).map_err(io)?;
    handle.seek(SeekFrom::Start(start)).map_err(io)?;
    let mut data = Vec::new();
    handle.take(want).read_to_end(&mut data).map_err(io)?;
    let eof = start + data.len() as u64 >= size;
    Ok(FileOpResult::Data {
        offset: start,
        data,
        size,
        eof,
    })
}

fn write(root: &Path, path: &str, data: &[u8], create_parents: bool) -> OpResult<FileOpResult> {
    if data.len() > WRITE_MAX_BYTES {
        return Err(err(
            FileErrorCode::TooLarge,
            "write body over the cap; use upload",
        ));
    }
    let target = for_write(root, path, create_parents)?;
    if target.is_dir() {
        return Err(err(FileErrorCode::Invalid, "target is a directory"));
    }
    fs::write(&target, data).map_err(io)?;
    Ok(FileOpResult::Written {
        size: data.len() as u64,
    })
}

/// Temp files of uploads in progress, by target. Each upload gets a fresh,
/// randomly named temp sibling on its first chunk; later chunks and the abort
/// find it here, so nothing ever opens a name a pod process could predict and
/// pre-place a symlink at (WI-4930).
static UPLOADS: std::sync::Mutex<Option<std::collections::HashMap<PathBuf, PathBuf>>> =
    std::sync::Mutex::new(None);

fn uploads<R>(f: impl FnOnce(&mut std::collections::HashMap<PathBuf, PathBuf>) -> R) -> R {
    let mut guard = UPLOADS.lock().unwrap_or_else(|p| p.into_inner());
    f(guard.get_or_insert_with(Default::default))
}

/// 64 unpredictable bits: `RandomState` keys come from the OS RNG.
fn random_suffix() -> String {
    use std::hash::{BuildHasher, Hasher};
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
    h.write_u32(std::process::id());
    format!("{:016x}", h.finish())
}

/// A new temp sibling name for an upload to `target`: hidden, randomly
/// suffixed, and in the target's own directory so the final rename stays on
/// one filesystem.
fn new_upload_temp(target: &Path) -> PathBuf {
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    target.with_file_name(format!(".{name}.{}.vogt-upload", random_suffix()))
}

/// The temp file of the upload to `target` in progress, if any.
fn upload_temp(target: &Path) -> Option<PathBuf> {
    uploads(|m| m.get(target).cloned())
}

/// Opens an upload temp file without following a symlink at its name
/// (`O_NOFOLLOW`), creating it only if nothing is there (`create_new`, i.e.
/// `O_EXCL`), and checks the handle is a regular file.
fn open_temp(temp: &Path, create: bool) -> std::io::Result<fs::File> {
    let mut opts = fs::OpenOptions::new();
    if create {
        opts.write(true).create_new(true);
    } else {
        opts.append(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NOFOLLOW);
    }
    let handle = opts.open(temp)?;
    if !handle.metadata()?.is_file() {
        return Err(std::io::Error::other("upload temp is not a regular file"));
    }
    Ok(handle)
}

fn hex_sha256(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The target's content hash as the engine's `if_match` guard computes it:
/// the empty hash for a missing file.
fn target_hash(target: &Path) -> OpResult<String> {
    match fs::read(target) {
        Ok(bytes) => Ok(hex_sha256(&bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(hex_sha256(&[])),
        Err(e) => Err(io(e)),
    }
}

fn upload(
    root: &Path,
    path: &str,
    offset: u64,
    data: &[u8],
    create_parents: bool,
    last: Option<bool>,
    if_match: Option<&str>,
) -> OpResult<FileOpResult> {
    // An engine older than the flag sends no `last`. Guessing is unsafe
    // either way (as `false` the file never lands; as `true` the first chunk
    // of a multi-chunk upload would land a truncated file), so refuse it.
    let Some(last) = last else {
        return Err(err(
            FileErrorCode::Invalid,
            "upload chunk has no `last` flag; the engine is older than this session agent",
        ));
    };
    if data.len() > UPLOAD_CHUNK_MAX {
        return Err(err(FileErrorCode::TooLarge, "upload chunk over the cap"));
    }
    if offset + data.len() as u64 > UPLOAD_MAX_BYTES {
        return Err(err(FileErrorCode::TooLarge, "upload over the file cap"));
    }
    let target = for_write(root, path, create_parents && offset == 0)?;
    if target.is_dir() {
        return Err(err(FileErrorCode::Invalid, "target is a directory"));
    }
    let temp = if offset == 0 {
        // A fresh start drops any earlier upload to the same target.
        if let Some(old) = uploads(|m| m.remove(&target)) {
            let _ = fs::remove_file(old);
        }
        new_upload_temp(&target)
    } else {
        upload_temp(&target).ok_or_else(|| {
            err(
                FileErrorCode::Invalid,
                format!("chunk at {offset} but the upload has no earlier chunks"),
            )
        })?
    };
    let result = upload_chunk(&target, &temp, offset, data, last, if_match);
    let created = offset == 0 && result.is_ok();
    if result.is_err() || last {
        // A failed chunk ends the upload: nothing partial stays behind.
        uploads(|m| m.remove(&target));
        if result.is_err() {
            let _ = fs::remove_file(&temp);
        }
    } else if created {
        uploads(|m| m.insert(target.clone(), temp.clone()));
    }
    result
}

fn upload_chunk(
    target: &Path,
    temp: &Path,
    offset: u64,
    data: &[u8],
    last: bool,
    if_match: Option<&str>,
) -> OpResult<FileOpResult> {
    let mut handle = if offset == 0 {
        open_temp(temp, true).map_err(io)?
    } else {
        let handle = open_temp(temp, false).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => err(
                FileErrorCode::Invalid,
                format!("chunk at {offset} but the upload has no earlier chunks"),
            ),
            _ => io(e),
        })?;
        let have = handle.metadata().map_err(io)?.len();
        if have != offset {
            return Err(err(
                FileErrorCode::Invalid,
                format!("chunk at {offset} but the file holds {have} bytes"),
            ));
        }
        handle
    };
    handle.write_all(data).map_err(io)?;
    handle.flush().map_err(io)?;
    drop(handle);
    let size = offset + data.len() as u64;
    if last {
        if let Some(expected) = if_match {
            let current = target_hash(target)?;
            if current != expected {
                return Err(err(
                    FileErrorCode::Conflict,
                    format!(
                        "file changed on disk since it was read (expected {expected}, found {current}); \
                         reload before saving to avoid clobbering newer content"
                    ),
                ));
            }
        }
        fs::rename(temp, target).map_err(io)?;
    }
    Ok(FileOpResult::Written { size })
}

fn upload_abort(root: &Path, path: &str) -> OpResult<FileOpResult> {
    let target = for_write(root, path, false)?;
    let Some(temp) = uploads(|m| m.remove(&target)) else {
        return Ok(FileOpResult::Done);
    };
    match fs::remove_file(temp) {
        Ok(()) => Ok(FileOpResult::Done),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(FileOpResult::Done),
        Err(e) => Err(io(e)),
    }
}

fn mkdir(root: &Path, path: &str, parents: bool) -> OpResult<FileOpResult> {
    let path = relative(path)?;
    if parents {
        mkdir_parents(root, path)?;
    } else {
        let target = for_write(root, path, false)?;
        fs::create_dir(&target).map_err(io)?;
    }
    Ok(FileOpResult::Done)
}

fn fresh_target(root: &Path, to: &str) -> OpResult<PathBuf> {
    let target = for_write(root, to, false)?;
    if fs::symlink_metadata(&target).is_ok() {
        return Err(err(FileErrorCode::Invalid, "target already exists"));
    }
    Ok(target)
}

fn rename(root: &Path, from: &str, to: &str) -> OpResult<FileOpResult> {
    let source = no_follow(root, from)?;
    let target = fresh_target(root, to)?;
    if target.starts_with(&source) {
        return Err(err(
            FileErrorCode::Invalid,
            "cannot move a directory into itself",
        ));
    }
    fs::rename(&source, &target).map_err(io)?;
    Ok(FileOpResult::Done)
}

fn copy(root: &Path, from: &str, to: &str) -> OpResult<FileOpResult> {
    let source = existing(root, from)?;
    let target = fresh_target(root, to)?;
    if source.is_file() {
        fs::copy(&source, &target).map_err(io)?;
        return Ok(FileOpResult::Done);
    }
    if !source.is_dir() {
        return Err(err(
            FileErrorCode::Invalid,
            "can copy only files and directories",
        ));
    }
    if target.starts_with(&source) {
        return Err(err(
            FileErrorCode::Invalid,
            "cannot copy a directory into itself",
        ));
    }
    // Symlinks inside the tree are not followed or copied, so a link cannot
    // pull bytes from outside the root; hidden and credential entries stay
    // behind for the same reason the listing hides them.
    for item in walkdir::WalkDir::new(&source).follow_links(false) {
        let item = item.map_err(|e| err(FileErrorCode::Io, e.to_string()))?;
        let rel = item.path().strip_prefix(&source).unwrap_or(item.path());
        if may_show(root, item.path()).is_err() {
            continue;
        }
        let dest = target.join(rel);
        let ft = item.file_type();
        if ft.is_dir() {
            fs::create_dir_all(&dest).map_err(io)?;
        } else if ft.is_file() {
            fs::copy(item.path(), &dest).map_err(io)?;
        }
    }
    Ok(FileOpResult::Done)
}

fn delete(root: &Path, path: &str) -> OpResult<FileOpResult> {
    let target = no_follow(root, path)?;
    if target == root {
        return Err(err(
            FileErrorCode::Refused,
            "refusing to delete the work root",
        ));
    }
    let meta = fs::symlink_metadata(&target).map_err(io)?;
    if meta.is_dir() {
        fs::remove_dir_all(&target).map_err(io)?;
    } else {
        fs::remove_file(&target).map_err(io)?;
    }
    Ok(FileOpResult::Done)
}

/// Run one op against `root` (canonical). Blocking: call it off the stream
/// task. Never panics on a bad request; every failure is a
/// [`FileOpResult::Error`].
pub fn handle(root: &Path, op: &FileOp) -> FileOpResult {
    let result = match op {
        FileOp::List { path } => list(root, path),
        FileOp::Stat { path } => {
            if path.is_empty() || path == "." {
                entry(root, root).map(|entry| FileOpResult::Stat { entry })
            } else {
                no_follow(root, path)
                    .and_then(|p| entry(root, &p))
                    .map(|entry| FileOpResult::Stat { entry })
            }
        }
        FileOp::Read { path, offset, len } => read(root, path, *offset, *len),
        FileOp::Write {
            path,
            data,
            create_parents,
        } => write(root, path, data, *create_parents),
        FileOp::Upload {
            path,
            offset,
            data,
            create_parents,
            last,
            if_match,
        } => upload(
            root,
            path,
            *offset,
            data,
            *create_parents,
            *last,
            if_match.as_deref(),
        ),
        FileOp::UploadAbort { path } => upload_abort(root, path),
        FileOp::Mkdir { path, parents } => mkdir(root, path, *parents),
        FileOp::Move { from, to } => rename(root, from, to),
        FileOp::Copy { from, to } => copy(root, from, to),
        FileOp::Delete { path } => delete(root, path),
    };
    result.unwrap_or_else(|e| e)
}

/// [`handle`] against [`work_root`], for the stream loop.
pub fn handle_in_work_root(op: &FileOp) -> FileOpResult {
    match work_root() {
        Ok(root) => handle(&root, op),
        Err(e) => err(FileErrorCode::Io, format!("work root unavailable: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn root() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        (dir, root)
    }

    fn code(r: &FileOpResult) -> Option<FileErrorCode> {
        match r {
            FileOpResult::Error { code, .. } => Some(*code),
            _ => None,
        }
    }

    fn s(v: &str) -> String {
        v.to_string()
    }

    #[test]
    fn list_hides_hidden_and_credential_entries() {
        let (_d, root) = root();
        fs::write(root.join("a.txt"), "a").unwrap();
        fs::create_dir(root.join("src")).unwrap();
        fs::write(root.join(".env"), "x").unwrap();
        fs::create_dir(root.join(".git")).unwrap();
        fs::write(root.join("server.key"), "x").unwrap();
        let r = handle(&root, &FileOp::List { path: s("") });
        let FileOpResult::Entries { entries, truncated } = r else {
            panic!("{r:?}")
        };
        assert!(!truncated);
        let names: Vec<_> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["a.txt", "src"]);
        assert_eq!(entries[1].kind, EntryKind::Dir);
        assert_eq!(entries[0].path, "a.txt");
    }

    #[test]
    fn stat_reports_kind_and_size() {
        let (_d, root) = root();
        fs::create_dir(root.join("d")).unwrap();
        fs::write(root.join("d/f"), "hello").unwrap();
        let r = handle(&root, &FileOp::Stat { path: s("d/f") });
        let FileOpResult::Stat { entry } = r else {
            panic!("{r:?}")
        };
        assert_eq!(
            (entry.kind, entry.size, entry.path.as_str()),
            (EntryKind::File, 5, "d/f")
        );
        assert_eq!(
            code(&handle(&root, &FileOp::Stat { path: s("nope") })),
            Some(FileErrorCode::NotFound)
        );
    }

    #[test]
    fn read_is_chunked() {
        let (_d, root) = root();
        fs::write(root.join("f"), b"0123456789").unwrap();
        let r = handle(
            &root,
            &FileOp::Read {
                path: s("f"),
                offset: 2,
                len: Some(3),
            },
        );
        assert_eq!(
            r,
            FileOpResult::Data {
                offset: 2,
                data: b"234".to_vec(),
                size: 10,
                eof: false
            }
        );
        let r = handle(
            &root,
            &FileOp::Read {
                path: s("f"),
                offset: 7,
                len: None,
            },
        );
        assert_eq!(
            r,
            FileOpResult::Data {
                offset: 7,
                data: b"789".to_vec(),
                size: 10,
                eof: true
            }
        );
    }

    #[test]
    fn read_chunk_never_exceeds_the_chunk_cap() {
        let (_d, root) = root();
        fs::write(root.join("f"), vec![7u8; READ_CHUNK_MAX as usize + 10]).unwrap();
        let r = handle(
            &root,
            &FileOp::Read {
                path: s("f"),
                offset: 0,
                len: Some(u64::MAX),
            },
        );
        let FileOpResult::Data { data, eof, .. } = r else {
            panic!()
        };
        assert_eq!(data.len() as u64, READ_CHUNK_MAX);
        assert!(!eof);
    }

    #[test]
    fn read_refuses_a_file_over_the_cap() {
        let (_d, root) = root();
        let f = fs::File::create(root.join("big")).unwrap();
        f.set_len(READ_MAX_BYTES + 1).unwrap();
        let r = handle(
            &root,
            &FileOp::Read {
                path: s("big"),
                offset: 0,
                len: None,
            },
        );
        assert_eq!(code(&r), Some(FileErrorCode::TooLarge));
    }

    #[test]
    fn read_refuses_a_directory_and_a_credential() {
        let (_d, root) = root();
        fs::create_dir(root.join("d")).unwrap();
        fs::write(root.join("id_rsa"), "k").unwrap();
        let r = handle(
            &root,
            &FileOp::Read {
                path: s("d"),
                offset: 0,
                len: None,
            },
        );
        assert_eq!(code(&r), Some(FileErrorCode::Invalid));
        let r = handle(
            &root,
            &FileOp::Read {
                path: s("id_rsa"),
                offset: 0,
                len: None,
            },
        );
        assert_eq!(code(&r), Some(FileErrorCode::Refused));
    }

    #[test]
    fn write_replaces_and_creates_parents() {
        let (_d, root) = root();
        let r = handle(
            &root,
            &FileOp::Write {
                path: s("a/b/c.txt"),
                data: b"hi".to_vec(),
                create_parents: true,
            },
        );
        assert_eq!(r, FileOpResult::Written { size: 2 });
        assert_eq!(fs::read(root.join("a/b/c.txt")).unwrap(), b"hi");
        let r = handle(
            &root,
            &FileOp::Write {
                path: s("x/y.txt"),
                data: b"hi".to_vec(),
                create_parents: false,
            },
        );
        assert_eq!(code(&r), Some(FileErrorCode::NotFound));
    }

    fn upload_op(
        root: &Path,
        path: &str,
        offset: u64,
        data: &[u8],
        last: bool,
        if_match: Option<&str>,
    ) -> FileOpResult {
        handle(
            root,
            &FileOp::Upload {
                path: s(path),
                offset,
                data: data.to_vec(),
                create_parents: true,
                last: Some(last),
                if_match: if_match.map(s),
            },
        )
    }

    /// Every upload temp file beside `target`.
    fn temps(target: &Path) -> Vec<PathBuf> {
        let dir = target.parent().unwrap();
        let Ok(rd) = fs::read_dir(dir) else {
            return Vec::new();
        };
        rd.map(|e| e.unwrap().path())
            .filter(|p| p.to_string_lossy().ends_with(".vogt-upload"))
            .collect()
    }

    #[cfg(unix)]
    #[test]
    fn upload_ignores_symlink_preplaced_at_temp_name() {
        let (_d, root) = root();
        let outside = tempfile::tempdir().unwrap();
        let victim = outside.path().join("victim");
        fs::write(&victim, b"untouched").unwrap();
        // The pre-WI-4930 fixed temp name, pointed outside the root.
        let fixed = root.join(".f.bin.vogt-upload");
        std::os::unix::fs::symlink(&victim, &fixed).unwrap();
        assert_eq!(
            upload_op(&root, "f.bin", 0, b"abc", false, None),
            FileOpResult::Written { size: 3 }
        );
        assert_eq!(
            upload_op(&root, "f.bin", 3, b"def", true, None),
            FileOpResult::Written { size: 6 }
        );
        assert_eq!(fs::read(&victim).unwrap(), b"untouched");
        assert_eq!(fs::read(root.join("f.bin")).unwrap(), b"abcdef");
        assert!(fs::symlink_metadata(root.join("f.bin")).unwrap().is_file());
        // Even a symlink at the exact chosen temp name is refused, not followed.
        let temp = new_upload_temp(&root.join("g.bin"));
        std::os::unix::fs::symlink(&victim, &temp).unwrap();
        assert!(open_temp(&temp, true).is_err());
        assert!(open_temp(&temp, false).is_err());
        assert_eq!(fs::read(&victim).unwrap(), b"untouched");
    }

    #[test]
    fn upload_temp_names_are_random() {
        let t = Path::new("/work/a.bin");
        assert_ne!(new_upload_temp(t), new_upload_temp(t));
    }

    #[test]
    fn upload_without_last_flag_is_refused() {
        let (_d, root) = root();
        let r = handle(
            &root,
            &FileOp::Upload {
                path: s("old.bin"),
                offset: 0,
                data: b"abc".to_vec(),
                create_parents: false,
                last: None,
                if_match: None,
            },
        );
        assert_eq!(code(&r), Some(FileErrorCode::Invalid));
        assert!(!root.join("old.bin").exists());
        assert!(temps(&root.join("old.bin")).is_empty());
        // The wire form from an older engine decodes to the same refusal.
        let op: FileOp = serde_json::from_value(serde_json::json!({
            "op": "upload", "path": "old.bin", "offset": 0, "data": []
        }))
        .unwrap();
        assert_eq!(code(&handle(&root, &op)), Some(FileErrorCode::Invalid));
    }

    #[test]
    fn upload_appends_chunks_in_order() {
        let (_d, root) = root();
        let up =
            |offset, data: &[u8], last| upload_op(&root, "up/file.bin", offset, data, last, None);
        assert_eq!(up(0, b"abc", false), FileOpResult::Written { size: 3 });
        // Nothing lands on the target until the last chunk.
        assert!(!root.join("up/file.bin").exists());
        assert_eq!(up(3, b"def", true), FileOpResult::Written { size: 6 });
        assert_eq!(fs::read(root.join("up/file.bin")).unwrap(), b"abcdef");
        assert!(temps(&root.join("up/file.bin")).is_empty());
        // A chunk at the wrong offset ends the upload and leaves no temp file.
        assert_eq!(up(0, b"x", false), FileOpResult::Written { size: 1 });
        assert_eq!(code(&up(2, b"zz", false)), Some(FileErrorCode::Invalid));
        assert!(temps(&root.join("up/file.bin")).is_empty());
        assert_eq!(fs::read(root.join("up/file.bin")).unwrap(), b"abcdef");
        // Offset 0 starts again, and a one-chunk upload replaces the file.
        assert_eq!(up(0, b"x", true), FileOpResult::Written { size: 1 });
        assert_eq!(fs::read(root.join("up/file.bin")).unwrap(), b"x");
        let big = vec![0u8; UPLOAD_CHUNK_MAX + 1];
        assert_eq!(code(&up(0, &big, true)), Some(FileErrorCode::TooLarge));
    }

    #[test]
    fn aborted_upload_leaves_no_partial_target_or_temp_file() {
        let (_d, root) = root();
        fs::write(root.join("keep.bin"), b"original").unwrap();
        // Replacing an existing file: the original survives a mid-way abort.
        assert_eq!(
            upload_op(&root, "keep.bin", 0, b"new-part-1", false, None),
            FileOpResult::Written { size: 10 }
        );
        let temp = upload_temp(&root.join("keep.bin")).expect("upload in progress");
        assert!(temp.exists());
        assert_eq!(fs::read(root.join("keep.bin")).unwrap(), b"original");
        let abort = handle(
            &root,
            &FileOp::UploadAbort {
                path: s("keep.bin"),
            },
        );
        assert_eq!(abort, FileOpResult::Done);
        assert!(!temp.exists());
        assert_eq!(fs::read(root.join("keep.bin")).unwrap(), b"original");
        // A new file: an abort leaves neither target nor temp file.
        upload_op(&root, "fresh.bin", 0, b"half", false, None);
        handle(
            &root,
            &FileOp::UploadAbort {
                path: s("fresh.bin"),
            },
        );
        assert!(!root.join("fresh.bin").exists());
        assert!(temps(&root.join("fresh.bin")).is_empty());
        // Aborting with nothing to abort is fine.
        assert_eq!(
            handle(
                &root,
                &FileOp::UploadAbort {
                    path: s("fresh.bin")
                }
            ),
            FileOpResult::Done
        );
    }

    #[test]
    fn upload_if_match_is_checked_at_rename_time() {
        let (_d, root) = root();
        fs::write(root.join("f.txt"), b"v1").unwrap();
        let v1 = hex_sha256(b"v1");
        // The file changes between the first chunk and the last.
        assert_eq!(
            upload_op(&root, "f.txt", 0, b"ab", false, Some(&v1)),
            FileOpResult::Written { size: 2 }
        );
        fs::write(root.join("f.txt"), b"v2").unwrap();
        let r = upload_op(&root, "f.txt", 2, b"cd", true, Some(&v1));
        assert_eq!(code(&r), Some(FileErrorCode::Conflict));
        assert_eq!(fs::read(root.join("f.txt")).unwrap(), b"v2");
        assert!(temps(&root.join("f.txt")).is_empty());
        // A matching hash renames; the empty hash guards a new file.
        let v2 = hex_sha256(b"v2");
        upload_op(&root, "f.txt", 0, b"ok", true, Some(&v2));
        assert_eq!(fs::read(root.join("f.txt")).unwrap(), b"ok");
        let r = upload_op(&root, "f.txt", 0, b"no", true, Some(&hex_sha256(&[])));
        assert_eq!(code(&r), Some(FileErrorCode::Conflict));
        assert_eq!(fs::read(root.join("f.txt")).unwrap(), b"ok");
    }

    #[test]
    fn upload_temp_is_hidden_from_listings() {
        let (_d, root) = root();
        upload_op(&root, "a.bin", 0, b"x", false, None);
        let FileOpResult::Entries { entries, .. } = handle(&root, &FileOp::List { path: s("") })
        else {
            panic!("expected entries")
        };
        assert!(entries.is_empty());
    }

    #[test]
    fn mkdir_move_copy_delete() {
        let (_d, root) = root();
        assert_eq!(
            handle(
                &root,
                &FileOp::Mkdir {
                    path: s("p/q"),
                    parents: true
                }
            ),
            FileOpResult::Done
        );
        assert!(root.join("p/q").is_dir());
        assert_eq!(
            code(&handle(
                &root,
                &FileOp::Mkdir {
                    path: s("r/s"),
                    parents: false
                }
            )),
            Some(FileErrorCode::NotFound)
        );
        fs::write(root.join("p/q/f"), "1").unwrap();
        assert_eq!(
            handle(
                &root,
                &FileOp::Copy {
                    from: s("p"),
                    to: s("p2")
                }
            ),
            FileOpResult::Done
        );
        assert_eq!(fs::read(root.join("p2/q/f")).unwrap(), b"1");
        assert_eq!(
            code(&handle(
                &root,
                &FileOp::Copy {
                    from: s("p"),
                    to: s("p/inner")
                }
            )),
            Some(FileErrorCode::Invalid)
        );
        assert_eq!(
            code(&handle(
                &root,
                &FileOp::Move {
                    from: s("p"),
                    to: s("p2")
                }
            )),
            Some(FileErrorCode::Invalid)
        );
        assert_eq!(
            handle(
                &root,
                &FileOp::Move {
                    from: s("p"),
                    to: s("m")
                }
            ),
            FileOpResult::Done
        );
        assert!(!root.join("p").exists());
        assert_eq!(fs::read(root.join("m/q/f")).unwrap(), b"1");
        assert_eq!(
            handle(&root, &FileOp::Delete { path: s("m") }),
            FileOpResult::Done
        );
        assert!(!root.join("m").exists());
        assert_eq!(
            handle(&root, &FileOp::Delete { path: s("p2/q/f") }),
            FileOpResult::Done
        );
        assert!(root.join("p2/q").is_dir());
    }

    #[test]
    fn copy_skips_symlinks_inside_the_tree() {
        let (_d, root) = root();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("loot"), "secret").unwrap();
        fs::create_dir(root.join("d")).unwrap();
        symlink(outside.path().join("loot"), root.join("d/link")).unwrap();
        assert_eq!(
            handle(
                &root,
                &FileOp::Copy {
                    from: s("d"),
                    to: s("e")
                }
            ),
            FileOpResult::Done
        );
        assert!(fs::symlink_metadata(root.join("e/link")).is_err());
    }

    #[test]
    fn delete_removes_a_link_not_its_target() {
        let (_d, root) = root();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("keep"), "x").unwrap();
        symlink(outside.path().join("keep"), root.join("link")).unwrap();
        assert_eq!(
            handle(&root, &FileOp::Delete { path: s("link") }),
            FileOpResult::Done
        );
        assert!(outside.path().join("keep").exists());
    }

    #[test]
    fn parent_components_and_absolute_paths_are_refused() {
        let (_d, root) = root();
        let ops = [
            FileOp::List { path: s("../") },
            FileOp::Stat {
                path: s("a/../../etc"),
            },
            FileOp::Read {
                path: s("/etc/passwd"),
                offset: 0,
                len: None,
            },
            FileOp::Write {
                path: s("../x"),
                data: vec![],
                create_parents: true,
            },
            FileOp::Upload {
                path: s("/tmp/x"),
                offset: 0,
                data: vec![],
                create_parents: false,
                last: Some(true),
                if_match: None,
            },
            FileOp::Mkdir {
                path: s("../d"),
                parents: true,
            },
            FileOp::Move {
                from: s("/etc/hosts"),
                to: s("h"),
            },
            FileOp::Copy {
                from: s("../x"),
                to: s("y"),
            },
            FileOp::Delete { path: s("/work") },
            FileOp::Delete { path: s("..") },
        ];
        for op in ops {
            assert_eq!(
                code(&handle(&root, &op)),
                Some(FileErrorCode::Refused),
                "{op:?}"
            );
        }
    }

    #[test]
    fn symlinks_that_escape_the_root_are_refused() {
        let (_d, root) = root();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("loot"), "secret").unwrap();
        symlink(outside.path(), root.join("out")).unwrap();
        symlink(outside.path().join("loot"), root.join("file-link")).unwrap();
        let ops = [
            FileOp::List { path: s("out") },
            FileOp::Read {
                path: s("out/loot"),
                offset: 0,
                len: None,
            },
            FileOp::Read {
                path: s("file-link"),
                offset: 0,
                len: None,
            },
            FileOp::Write {
                path: s("out/new"),
                data: b"x".to_vec(),
                create_parents: false,
            },
            FileOp::Write {
                path: s("file-link"),
                data: b"x".to_vec(),
                create_parents: false,
            },
            FileOp::Upload {
                path: s("out/new"),
                offset: 0,
                data: vec![],
                create_parents: true,
                last: Some(true),
                if_match: None,
            },
            FileOp::Mkdir {
                path: s("out/sub"),
                parents: true,
            },
            FileOp::Copy {
                from: s("file-link"),
                to: s("copy"),
            },
            FileOp::Move {
                from: s("out/loot"),
                to: s("moved"),
            },
            FileOp::Delete {
                path: s("out/loot"),
            },
        ];
        for op in ops {
            assert_eq!(
                code(&handle(&root, &op)),
                Some(FileErrorCode::Refused),
                "{op:?}"
            );
        }
        assert_eq!(fs::read(outside.path().join("loot")).unwrap(), b"secret");
        assert!(!outside.path().join("new").exists());
        assert!(!outside.path().join("sub").exists());
    }

    #[test]
    fn writes_to_hidden_or_credential_paths_are_refused() {
        let (_d, root) = root();
        for path in [".git/config", ".env", "secrets/x.txt", "deploy.pem"] {
            let r = handle(
                &root,
                &FileOp::Write {
                    path: s(path),
                    data: b"x".to_vec(),
                    create_parents: true,
                },
            );
            assert_eq!(code(&r), Some(FileErrorCode::Refused), "{path}");
        }
    }

    #[test]
    fn deleting_the_root_is_refused() {
        let (_d, root) = root();
        for path in ["", "."] {
            assert!(code(&handle(&root, &FileOp::Delete { path: s(path) })).is_some());
        }
        assert!(root.exists());
    }

    #[test]
    fn file_frames_round_trip_on_the_stream() {
        let frame = crate::Frame::FileOp {
            id: 7,
            op: FileOp::Delete { path: s("x") },
        };
        let json = serde_json::to_string(&frame).unwrap();
        assert_eq!(
            json,
            r#"{"type":"file_op","id":7,"op":{"op":"delete","path":"x"}}"#
        );
        assert_eq!(serde_json::from_str::<crate::Frame>(&json).unwrap(), frame);
        let frame = crate::Frame::FileResult {
            id: 7,
            result: FileOpResult::Done,
        };
        let json = serde_json::to_string(&frame).unwrap();
        assert_eq!(
            json,
            r#"{"type":"file_result","id":7,"result":{"result":"done"}}"#
        );
        assert_eq!(serde_json::from_str::<crate::Frame>(&json).unwrap(), frame);
    }

    #[test]
    fn ops_round_trip_as_json() {
        let op = FileOp::Read {
            path: s("a"),
            offset: 1,
            len: Some(2),
        };
        let json = serde_json::to_string(&op).unwrap();
        assert_eq!(json, r#"{"op":"read","path":"a","offset":1,"len":2}"#);
        assert_eq!(serde_json::from_str::<FileOp>(&json).unwrap(), op);
        let r = FileOpResult::Error {
            code: FileErrorCode::Refused,
            message: s("m"),
        };
        let json = serde_json::to_string(&r).unwrap();
        assert_eq!(json, r#"{"result":"error","code":"refused","message":"m"}"#);
    }
}
