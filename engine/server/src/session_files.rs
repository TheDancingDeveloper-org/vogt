//! Session-scoped file routes, `/api/sessions/{id}/files*` (WI-4867, part of
//! WI-4863). They mirror the global `/api/files*` set, but address one
//! session's own tree:
//!
//! - a **host** session is served from its own cwd on the engine's
//!   filesystem, through the same `workspace_path` rules as the global
//!   routes, and only when that cwd lies inside `workspace_root`, so a
//!   session route never widens what the engine already exposes;
//! - a **pod** session is served by its session agent: each request becomes
//!   one or more `FileOp` frames on the pod stream, matched back by id, each
//!   with [`FILE_OP_TIMEOUT`];
//! - a **hibernated** session, or a pod with no agent attached, is `409`
//!   with code `session_hibernated` and a hint to wake it.
//!
//! Auth: the gate requires `sessions` for the reads and `filesystem-write`
//! for the writes (`auth::required_capability`); every handler then calls
//! [`authorize`], which also requires `sessions` on a write and refuses a
//! session-bound token (`agent:session:…`, `agent:engine:…`, `agent:pod:…`)
//! any session but its own.

use std::{path::PathBuf, sync::Arc, time::Duration};

use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{header, HeaderValue},
    response::Response,
    Extension, Json,
};
use base64::Engine as _;
use futures_util::StreamExt as _;
use sha2::{Digest, Sha256};
use uuid::Uuid;
use vogt_engine_contract::{FileEntry, FileRead, TreeNode, WriteFileResponse, WriteReq};
use vogt_session_agent::file_ops::{
    EntryKind, FileEntry as PodEntry, FileErrorCode, FileOp, FileOpResult, READ_CHUNK_MAX,
    UPLOAD_CHUNK_MAX, WRITE_MAX_BYTES,
};

use crate::{
    agent_stream::FileOpFailure,
    app::AppState,
    auth::{AuthorizedIdentity, TokenCapability},
    error::{ApiError, Result},
    files::{self, FileOpReq, FileOpResponse, ListQuery, ReadQuery, TreeQuery, UploadQuery},
};

/// How long one proxied `FileOp` may wait for the agent's answer.
pub const FILE_OP_TIMEOUT: Duration = Duration::from_secs(30);

/// The largest file `GET …/files` returns, as the global route caps it.
const MAX_READ_BYTES: u64 = 5 * 1024 * 1024;

/// Where a session's files are served from.
enum Target {
    /// A host session: its canonical cwd, inside `workspace_root`.
    Host(PathBuf),
    /// A pod session, by its engine id.
    Pod(Uuid),
}

/// Whether `identity` may use the file routes of session `target`. `caller`
/// is the engine session the identity's name resolves to (see
/// `git::caller_session`), `None` when it resolves to none.
///
/// A session-bound token reaches only its own session; a token that is not
/// session-bound (a person, the break-glass token, the stack secret, an
/// oversight driver's actor) reaches any session it could already drive.
/// Reads need `sessions`; writes need `sessions` and `filesystem-write`.
pub fn authorize(
    identity: &AuthorizedIdentity,
    caller: Option<Uuid>,
    target: Uuid,
    write: bool,
) -> std::result::Result<(), String> {
    if !identity.allows(TokenCapability::Sessions) {
        return Err(crate::auth::capability_refusal(
            identity,
            TokenCapability::Sessions,
        ));
    }
    if write && !identity.allows(TokenCapability::FilesystemWrite) {
        return Err(crate::auth::capability_refusal(
            identity,
            TokenCapability::FilesystemWrite,
        ));
    }
    if is_session_bound(&identity.name) && caller != Some(target) {
        return Err(format!(
            "{} is a session token and may reach only its own session's files, not session {target}",
            identity.name
        ));
    }
    Ok(())
}

/// Whether a token name belongs to one session, so it may reach only that
/// session's tree.
pub fn is_session_bound(name: &str) -> bool {
    name.starts_with("agent:session:")
        || name.starts_with("agent:engine:")
        || name.starts_with("agent:pod:")
}

fn check(
    state: &AppState,
    identity: Option<Extension<AuthorizedIdentity>>,
    id: Uuid,
    write: bool,
) -> Result<()> {
    let Some(Extension(identity)) = identity else {
        return Err(ApiError::Unauthorized);
    };
    let caller = crate::git::caller_session(state, &identity.name);
    authorize(&identity, caller, id, write).map_err(|reason| {
        tracing::warn!(
            target: "vogt::audit",
            event = "session.files",
            outcome = "refused",
            caller = %identity.name,
            session = %id,
            write,
            reason = %reason,
            "session file access refused"
        );
        ApiError::Forbidden(reason)
    })
}

fn hibernated(id: Uuid, reason: &str) -> ApiError {
    ApiError::SessionHibernated {
        id: id.to_string(),
        reason: reason.into(),
    }
}

fn target(state: &AppState, id: Uuid) -> Result<Target> {
    if state.sessions.is_hibernated(id) {
        return Err(hibernated(id, "the session is hibernated"));
    }
    let session = state.sessions.get(id)?;
    if let Some(link) = crate::agent_stream::global().pod(id) {
        if !link.is_connected() || link.is_finished() {
            return Err(hibernated(id, "the pod's session agent is not attached"));
        }
        return Ok(Target::Pod(id));
    }
    let cwd = std::path::Path::new(&session.cwd)
        .canonicalize()
        .map_err(|e| ApiError::BadRequest(format!("session cwd is unreadable: {e}")))?;
    let root = &state.config.workspace_root;
    if !cwd.starts_with(root) {
        return Err(ApiError::Forbidden(
            "the session's cwd is outside the engine workspace; its files are not served".into(),
        ));
    }
    Ok(Target::Host(cwd))
}

/// Send one op to a pod session's agent; a protocol error becomes an
/// [`ApiError`] with the status the global routes would give.
async fn op(id: Uuid, op: FileOp) -> Result<FileOpResult> {
    let result = crate::agent_stream::global()
        .request_file_op(id, op, FILE_OP_TIMEOUT)
        .await
        .map_err(|e| match e {
            FileOpFailure::NotAttached => hibernated(id, "the pod's session agent is not attached"),
            FileOpFailure::TimedOut => ApiError::Unavailable {
                message: format!(
                    "the pod's session agent did not answer within {}s",
                    FILE_OP_TIMEOUT.as_secs()
                ),
                retry_after: Some(5),
            },
            FileOpFailure::Unavailable(m) => ApiError::Unavailable {
                message: m,
                retry_after: Some(5),
            },
        })?;
    match result {
        FileOpResult::Error { code, message } => Err(map_error(code, message)),
        other => Ok(other),
    }
}

fn map_error(code: FileErrorCode, message: String) -> ApiError {
    match code {
        FileErrorCode::Refused => ApiError::BadRequest(message),
        FileErrorCode::NotFound => ApiError::NotFound,
        FileErrorCode::TooLarge => ApiError::TooLarge(message),
        FileErrorCode::Invalid if message.contains("exists") => ApiError::Conflict(message),
        FileErrorCode::Invalid => ApiError::BadRequest(message),
        FileErrorCode::Conflict => ApiError::Conflict(message),
        FileErrorCode::Io => ApiError::Internal(message),
    }
}

fn unexpected(what: &str) -> ApiError {
    ApiError::BadGateway(format!(
        "the pod's session agent sent an unexpected answer to {what}"
    ))
}

/// The pod contract wants relative paths; the global routes treat an empty
/// path as the root.
fn pod_path(path: &str) -> String {
    let p = path.trim().trim_start_matches("./");
    if p.is_empty() {
        ".".into()
    } else {
        p.into()
    }
}

fn hash_bytes(bytes: &[u8]) -> String {
    files::hex_digest(Sha256::digest(bytes))
}

async fn pod_stat(id: Uuid, path: &str) -> Result<PodEntry> {
    match op(
        id,
        FileOp::Stat {
            path: pod_path(path),
        },
    )
    .await?
    {
        FileOpResult::Stat { entry } => Ok(entry),
        _ => Err(unexpected("stat")),
    }
}

async fn pod_list(id: Uuid, path: &str) -> Result<Vec<PodEntry>> {
    match op(
        id,
        FileOp::List {
            path: pod_path(path),
        },
    )
    .await?
    {
        FileOpResult::Entries { entries, .. } => Ok(entries),
        _ => Err(unexpected("list")),
    }
}

/// The whole file, chunk by chunk, refused past `cap`.
async fn pod_read_all(id: Uuid, path: &str, cap: u64) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let reply = op(
            id,
            FileOp::Read {
                path: pod_path(path),
                offset: out.len() as u64,
                len: Some(READ_CHUNK_MAX),
            },
        )
        .await?;
        let FileOpResult::Data {
            data, size, eof, ..
        } = reply
        else {
            return Err(unexpected("read"));
        };
        if size > cap {
            return Err(ApiError::BadRequest(format!(
                "file too large (max {cap} bytes)"
            )));
        }
        let empty = data.is_empty();
        out.extend_from_slice(&data);
        if out.len() as u64 > cap {
            return Err(ApiError::BadRequest(format!(
                "file too large (max {cap} bytes)"
            )));
        }
        if eof || empty {
            return Ok(out);
        }
    }
}

/// One read chunk at `offset`: its bytes, the file's size and whether it was
/// the last.
async fn pod_read_chunk(id: Uuid, path: &str, offset: u64) -> Result<(Vec<u8>, u64, bool)> {
    match op(
        id,
        FileOp::Read {
            path: pod_path(path),
            offset,
            len: Some(READ_CHUNK_MAX),
        },
    )
    .await?
    {
        FileOpResult::Data {
            data, size, eof, ..
        } => Ok((data, size, eof)),
        _ => Err(unexpected("read")),
    }
}

/// A body stream that yields `first`, then asks `fetch(offset)` for each
/// following chunk (bytes, eof) until eof or an empty chunk. One chunk is in
/// memory at a time. A failed fetch ends the stream with an error, which
/// aborts the HTTP body instead of passing off a short file as complete.
fn chunk_stream<F, Fut>(
    first: Vec<u8>,
    eof: bool,
    fetch: F,
) -> impl futures_util::Stream<Item = std::result::Result<Vec<u8>, std::io::Error>>
where
    F: FnMut(u64) -> Fut,
    Fut: std::future::Future<Output = Result<(Vec<u8>, bool)>>,
{
    struct State<F> {
        pending: Option<Vec<u8>>,
        offset: u64,
        done: bool,
        fetch: F,
    }
    let state = State {
        pending: Some(first),
        offset: 0,
        done: eof,
        fetch,
    };
    futures_util::stream::unfold(state, |mut st| async move {
        let data = match st.pending.take() {
            Some(first) => first,
            None if st.done => return None,
            None => match (st.fetch)(st.offset).await {
                Ok((data, eof)) => {
                    st.done = eof || data.is_empty();
                    if data.is_empty() {
                        return None;
                    }
                    data
                }
                Err(e) => {
                    st.done = true;
                    return Some((Err(std::io::Error::other(e.to_string())), st));
                }
            },
        };
        if data.is_empty() {
            return None;
        }
        st.offset += data.len() as u64;
        Some((Ok(data), st))
    })
}

/// The current content hash, or the empty hash for a missing file, as the
/// global routes' `if_match` guard computes it.
async fn pod_current_hash(id: Uuid, path: &str) -> Result<String> {
    match pod_read_all(id, path, u64::MAX).await {
        Ok(bytes) => Ok(hash_bytes(&bytes)),
        Err(ApiError::NotFound) => Ok(hash_bytes(&[])),
        Err(e) => Err(e),
    }
}

async fn pod_if_match(id: Uuid, path: &str, expected: Option<&str>) -> Result<()> {
    let Some(expected) = expected else {
        return Ok(());
    };
    let current = pod_current_hash(id, path).await?;
    if current != expected {
        return Err(ApiError::Conflict(format!(
            "file changed on disk since it was read (expected {expected}, found {current}); \
             reload before saving to avoid clobbering newer content"
        )));
    }
    Ok(())
}

fn to_entry(e: PodEntry) -> FileEntry {
    FileEntry {
        name: e.name,
        path: e.path,
        is_dir: e.kind == EntryKind::Dir,
        size: e.size,
    }
}

fn mtime_ms(e: &PodEntry) -> u64 {
    e.modified
        .map_or(0, |s| (s.max(0) as u64).saturating_mul(1000))
}

fn sort_dirs_first<T>(items: &mut [T], key: impl Fn(&T) -> (bool, String)) {
    items.sort_by(|a, b| {
        let (ad, an) = key(a);
        let (bd, bn) = key(b);
        bd.cmp(&ad).then_with(|| an.cmp(&bn))
    });
}

pub async fn list_dir(
    State(state): State<Arc<AppState>>,
    identity: Option<Extension<AuthorizedIdentity>>,
    Path(id): Path<Uuid>,
    Query(q): Query<ListQuery>,
) -> Result<Json<Vec<FileEntry>>> {
    check(&state, identity, id, false)?;
    match target(&state, id)? {
        Target::Host(root) => files::list_dir_in(&root, q).await,
        Target::Pod(id) => {
            if !pod_path(&q.path).eq(".") && pod_stat(id, &q.path).await?.kind != EntryKind::Dir {
                return Err(ApiError::BadRequest(format!("not a directory: {}", q.path)));
            }
            let mut entries: Vec<FileEntry> = pod_list(id, &q.path)
                .await?
                .into_iter()
                .filter(|e| !e.name.starts_with('.'))
                .map(to_entry)
                .collect();
            sort_dirs_first(&mut entries, |e| (e.is_dir, e.name.to_lowercase()));
            Ok(Json(entries))
        }
    }
}

pub async fn tree(
    State(state): State<Arc<AppState>>,
    identity: Option<Extension<AuthorizedIdentity>>,
    Path(id): Path<Uuid>,
    Query(q): Query<TreeQuery>,
) -> Result<Json<Vec<TreeNode>>> {
    check(&state, identity, id, false)?;
    match target(&state, id)? {
        Target::Host(root) => files::tree_in(&root, q).await,
        Target::Pod(id) => {
            let depth = q.depth.unwrap_or(0).min(3);
            Ok(Json(pod_walk(id, pod_path(&q.path), depth).await?))
        }
    }
}

async fn pod_walk(id: Uuid, path: String, depth: u32) -> Result<Vec<TreeNode>> {
    let mut out = Vec::new();
    for e in pod_list(id, &path).await? {
        // Symlinks are skipped, as the host walk skips them.
        if e.name.starts_with('.') || e.kind == EntryKind::Symlink {
            continue;
        }
        let is_dir = e.kind == EntryKind::Dir;
        let children = if is_dir && depth > 0 {
            Some(Box::pin(pod_walk(id, e.path.clone(), depth - 1)).await?)
        } else if is_dir {
            Some(Vec::new())
        } else {
            None
        };
        out.push(TreeNode {
            name: e.name,
            path: e.path,
            is_dir,
            children,
        });
    }
    sort_dirs_first(&mut out, |n| (n.is_dir, n.name.to_lowercase()));
    Ok(out)
}

pub async fn read_file(
    State(state): State<Arc<AppState>>,
    identity: Option<Extension<AuthorizedIdentity>>,
    Path(id): Path<Uuid>,
    Query(q): Query<ReadQuery>,
) -> Result<Json<FileRead>> {
    check(&state, identity, id, false)?;
    match target(&state, id)? {
        Target::Host(root) => files::read_file_in(&root, q).await,
        Target::Pod(id) => {
            let entry = pod_stat(id, &q.path).await?;
            if entry.kind != EntryKind::File {
                return Err(ApiError::BadRequest(format!("not a file: {}", q.path)));
            }
            let bytes = pod_read_all(id, &q.path, MAX_READ_BYTES).await?;
            let hash = hash_bytes(&bytes);
            let is_binary = bytes[..bytes.len().min(8192)].contains(&0);
            let (content, content_base64) = if is_binary {
                (
                    None,
                    Some(base64::engine::general_purpose::STANDARD.encode(&bytes)),
                )
            } else {
                (Some(String::from_utf8_lossy(&bytes).into_owned()), None)
            };
            Ok(Json(FileRead {
                path: entry.path.clone(),
                size: bytes.len() as u64,
                content,
                content_base64,
                is_binary,
                mtime: mtime_ms(&entry),
                hash,
            }))
        }
    }
}

pub async fn download_file(
    State(state): State<Arc<AppState>>,
    identity: Option<Extension<AuthorizedIdentity>>,
    Path(id): Path<Uuid>,
    Query(q): Query<ReadQuery>,
) -> Result<Response> {
    check(&state, identity, id, false)?;
    match target(&state, id)? {
        Target::Host(root) => files::download_file_in(&root, q).await,
        Target::Pod(id) => {
            let entry = pod_stat(id, &q.path).await?;
            if entry.kind != EntryKind::File {
                return Err(ApiError::BadRequest(format!("not a file: {}", q.path)));
            }
            // The first chunk is read before the response starts, so a refusal
            // (too large, vanished) is still an error status; the rest is
            // streamed chunk by chunk, never gathered in engine memory.
            let (first, size, eof) = pod_read_chunk(id, &q.path, 0).await?;
            if size > vogt_session_agent::file_ops::READ_MAX_BYTES {
                return Err(ApiError::BadRequest(format!(
                    "file too large (max {} bytes)",
                    vogt_session_agent::file_ops::READ_MAX_BYTES
                )));
            }
            let path = q.path.clone();
            let body = Body::from_stream(chunk_stream(first, eof, move |offset| {
                let path = path.clone();
                async move {
                    let (data, _, eof) = pod_read_chunk(id, &path, offset).await?;
                    Ok((data, eof))
                }
            }));
            let disposition = format!(
                "attachment; filename=\"{}\"",
                files::attachment_name(&entry.name)
            );
            Response::builder()
                .header(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("application/octet-stream"),
                )
                .header(
                    header::CONTENT_DISPOSITION,
                    HeaderValue::from_str(&disposition)
                        .unwrap_or_else(|_| HeaderValue::from_static("attachment")),
                )
                .header(header::CONTENT_LENGTH, HeaderValue::from(size))
                .body(body)
                .map_err(|e| ApiError::Internal(e.to_string()))
        }
    }
}

pub async fn write_file(
    State(state): State<Arc<AppState>>,
    identity: Option<Extension<AuthorizedIdentity>>,
    Path(id): Path<Uuid>,
    Json(req): Json<WriteReq>,
) -> Result<Json<WriteFileResponse>> {
    check(&state, identity, id, true)?;
    match target(&state, id)? {
        Target::Host(root) => files::write_file_in(&root, req).await,
        Target::Pod(id) => {
            let bytes: Vec<u8> = match &req.content_base64 {
                Some(b64) => base64::engine::general_purpose::STANDARD
                    .decode(b64.as_bytes())
                    .map_err(|e| ApiError::BadRequest(format!("invalid content_base64: {e}")))?,
                None => req.content.clone().into_bytes(),
            };
            if bytes.len() > WRITE_MAX_BYTES {
                return Err(ApiError::TooLarge(format!(
                    "a session file write carries at most {WRITE_MAX_BYTES} bytes; use the upload route"
                )));
            }
            pod_if_match(id, &req.path, req.if_match.as_deref()).await?;
            let hash = hash_bytes(&bytes);
            let n = bytes.len();
            match op(
                id,
                FileOp::Write {
                    path: pod_path(&req.path),
                    data: bytes,
                    create_parents: req.create_parents,
                },
            )
            .await?
            {
                FileOpResult::Written { .. } => {}
                _ => return Err(unexpected("write")),
            }
            let mtime = pod_stat(id, &req.path).await.map_or(0, |e| mtime_ms(&e));
            Ok(Json(WriteFileResponse {
                ok: true,
                bytes: n,
                hash,
                mtime,
            }))
        }
    }
}

/// The streaming upload. On a pod the body is cut into `Upload` chunks of at
/// most [`UPLOAD_CHUNK_MAX`]. The agent builds them in a temp sibling and
/// renames it onto the target when the `last` chunk lands, checking
/// `if_match` again then (it is also checked up front, to refuse early). On
/// any failure the engine sends `UploadAbort` so no temp file is left.
pub async fn upload_file(
    State(state): State<Arc<AppState>>,
    identity: Option<Extension<AuthorizedIdentity>>,
    Path(id): Path<Uuid>,
    Query(q): Query<UploadQuery>,
    body: Body,
) -> Result<Json<WriteFileResponse>> {
    check(&state, identity, id, true)?;
    match target(&state, id)? {
        Target::Host(root) => files::upload_file_in(&root, q, body).await,
        Target::Pod(id) => {
            pod_if_match(id, &q.path, q.if_match.as_deref()).await?;
            let (bytes, hash) = match pod_upload_body(id, &q, body).await {
                Ok(done) => done,
                Err(e) => {
                    // Best effort: the agent also drops the temp file itself
                    // when one of its chunks fails.
                    let _ = op(
                        id,
                        FileOp::UploadAbort {
                            path: pod_path(&q.path),
                        },
                    )
                    .await;
                    return Err(e);
                }
            };
            let mtime = pod_stat(id, &q.path).await.map_or(0, |e| mtime_ms(&e));
            Ok(Json(WriteFileResponse {
                ok: true,
                bytes: bytes as usize,
                hash,
                mtime,
            }))
        }
    }
}

/// Send the body as `Upload` chunks; returns its length and SHA-256.
async fn pod_upload_body(id: Uuid, q: &UploadQuery, body: Body) -> Result<(u64, String)> {
    let mut hasher = Sha256::new();
    let mut offset: u64 = 0;
    let mut pending: Vec<u8> = Vec::with_capacity(UPLOAD_CHUNK_MAX);
    let mut stream = body.into_data_stream();
    loop {
        let next = stream.next().await;
        let done = next.is_none();
        if let Some(chunk) = next {
            let chunk =
                chunk.map_err(|e| ApiError::BadRequest(format!("upload stream error: {e}")))?;
            hasher.update(&chunk);
            pending.extend_from_slice(&chunk);
        }
        // Until the body ends, keep back at least one byte: the final chunk
        // must be sent flagged `last`, and only the end of the body says
        // which one that is. An empty body sends one empty `last` chunk.
        while (!done && pending.len() > UPLOAD_CHUNK_MAX)
            || (done && (!pending.is_empty() || offset == 0))
        {
            let take = pending.len().min(UPLOAD_CHUNK_MAX);
            let data: Vec<u8> = pending.drain(..take).collect();
            let len = data.len() as u64;
            let last = done && pending.is_empty();
            match op(
                id,
                FileOp::Upload {
                    path: pod_path(&q.path),
                    offset,
                    data,
                    create_parents: q.create_parents && offset == 0,
                    last,
                    if_match: if last { q.if_match.clone() } else { None },
                },
            )
            .await?
            {
                FileOpResult::Written { .. } => {}
                _ => return Err(unexpected("upload")),
            }
            offset += len;
        }
        if done {
            return Ok((offset, files::hex_digest(hasher.finalize())));
        }
    }
}

pub async fn operate(
    State(state): State<Arc<AppState>>,
    identity: Option<Extension<AuthorizedIdentity>>,
    Path(id): Path<Uuid>,
    Json(req): Json<FileOpReq>,
) -> Result<Json<FileOpResponse>> {
    check(&state, identity, id, true)?;
    match target(&state, id)? {
        Target::Host(root) => files::operate_in(&root, req).await,
        Target::Pod(id) => pod_operate(id, req).await.map(Json),
    }
}

fn require_nonempty(path: &str) -> Result<()> {
    if path.trim().is_empty() {
        return Err(ApiError::BadRequest("path must not be empty".into()));
    }
    Ok(())
}

async fn pod_mkdir_parent(id: Uuid, path: &str) -> Result<()> {
    if let Some(parent) = std::path::Path::new(path.trim()).parent() {
        let parent = parent.to_string_lossy();
        if !parent.is_empty() {
            op(
                id,
                FileOp::Mkdir {
                    path: parent.into_owned(),
                    parents: true,
                },
            )
            .await?;
        }
    }
    Ok(())
}

async fn pod_operate(id: Uuid, req: FileOpReq) -> Result<FileOpResponse> {
    let (frame, path) = match req {
        FileOpReq::Move {
            from,
            to,
            create_parents,
        } => {
            require_nonempty(&from)?;
            require_nonempty(&to)?;
            if create_parents {
                pod_mkdir_parent(id, &to).await?;
            }
            (
                FileOp::Move {
                    from: pod_path(&from),
                    to: pod_path(&to),
                },
                Some(pod_path(&to)),
            )
        }
        FileOpReq::Duplicate {
            from,
            to,
            create_parents,
        } => {
            require_nonempty(&from)?;
            require_nonempty(&to)?;
            if create_parents {
                pod_mkdir_parent(id, &to).await?;
            }
            (
                FileOp::Copy {
                    from: pod_path(&from),
                    to: pod_path(&to),
                },
                Some(pod_path(&to)),
            )
        }
        FileOpReq::Delete { path, recursive } => {
            require_nonempty(&path)?;
            // The agent deletes a whole tree; without `recursive` a
            // non-empty directory is refused here, as `remove_dir` would.
            if !recursive
                && pod_stat(id, &path).await?.kind == EntryKind::Dir
                && !pod_list(id, &path).await?.is_empty()
            {
                return Err(ApiError::BadRequest(format!(
                    "directory not empty: {path} (pass recursive)"
                )));
            }
            (
                FileOp::Delete {
                    path: pod_path(&path),
                },
                None,
            )
        }
        FileOpReq::Mkdir { path, parents } => {
            require_nonempty(&path)?;
            (
                FileOp::Mkdir {
                    path: pod_path(&path),
                    parents,
                },
                Some(pod_path(&path)),
            )
        }
    };
    op(id, frame).await?;
    Ok(FileOpResponse { ok: true, path })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    type Fetched =
        std::pin::Pin<Box<dyn std::future::Future<Output = Result<(Vec<u8>, bool)>> + Send>>;

    /// A fake file served in `chunk`-byte reads; counts the fetches.
    fn fake(
        data: Vec<u8>,
        chunk: usize,
        fetches: Arc<AtomicUsize>,
    ) -> (Vec<u8>, bool, impl FnMut(u64) -> Fetched) {
        let first = data[..chunk.min(data.len())].to_vec();
        let eof = first.len() >= data.len();
        let fetch = move |offset: u64| -> Fetched {
            fetches.fetch_add(1, Ordering::SeqCst);
            let start = (offset as usize).min(data.len());
            let end = (start + chunk).min(data.len());
            let part = data[start..end].to_vec();
            let eof = end >= data.len();
            Box::pin(async move { Ok((part, eof)) })
        };
        (first, eof, fetch)
    }

    #[tokio::test]
    async fn download_streams_more_than_one_chunk() {
        let data: Vec<u8> = (0..10_000u32).map(|i| (i % 251) as u8).collect();
        let fetches = Arc::new(AtomicUsize::new(0));
        let (first, eof, fetch) = fake(data.clone(), 4096, fetches.clone());
        let chunks: Vec<_> = chunk_stream(first, eof, fetch).collect().await;
        let chunks: Vec<Vec<u8>> = chunks.into_iter().map(|c| c.unwrap()).collect();
        // 4096 + 4096 + 1808: three body chunks, none larger than one read.
        assert_eq!(chunks.len(), 3);
        assert!(chunks.iter().all(|c| c.len() <= 4096));
        assert_eq!(chunks.concat(), data);
        assert_eq!(fetches.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn download_stream_is_lazy_and_stops_at_eof() {
        let data = vec![7u8; 9000];
        let fetches = Arc::new(AtomicUsize::new(0));
        let (first, eof, fetch) = fake(data, 4096, fetches.clone());
        let mut stream = Box::pin(chunk_stream(first, eof, fetch));
        // Nothing past the first chunk is read until the body is polled.
        assert_eq!(fetches.load(Ordering::SeqCst), 0);
        assert_eq!(stream.next().await.unwrap().unwrap().len(), 4096);
        assert_eq!(fetches.load(Ordering::SeqCst), 0);
        assert_eq!(stream.next().await.unwrap().unwrap().len(), 4096);
        assert_eq!(fetches.load(Ordering::SeqCst), 1);
        assert_eq!(stream.next().await.unwrap().unwrap().len(), 808);
        assert!(stream.next().await.is_none());
        assert_eq!(fetches.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn download_stream_errors_rather_than_truncating() {
        let fetch = |_offset: u64| async { Err::<(Vec<u8>, bool), _>(ApiError::NotFound) };
        let items: Vec<_> = chunk_stream(vec![1, 2, 3], false, fetch).collect().await;
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].as_ref().unwrap(), &vec![1, 2, 3]);
        assert!(items[1].is_err());
    }

    #[tokio::test]
    async fn empty_file_streams_nothing() {
        let fetch = |_offset: u64| async { Ok::<_, ApiError>((vec![], true)) };
        let items: Vec<_> = chunk_stream(vec![], true, fetch).collect().await;
        assert!(items.is_empty());
    }
}
