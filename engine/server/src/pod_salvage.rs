//! Keep a stopped pod session's uncommitted work (WI-4132, WI-3370).
//!
//! The pod's `/work` is deleted with it. Before an explicit stop or a
//! hibernation tears the pod down, the session agent builds a
//! [`SalvageUpload`](vogt_engine_contract::salvage::SalvageUpload) from the
//! checkout and POSTs it to
//! `/api/agent-auth/store/bundle/salvage`. This module writes it under
//! `<state_dir>/pod-salvage/<session_id>/<timestamp>/` (directories `0700`,
//! files `0600`), enforces the deployment's own bounds rather than trusting
//! the agent's, and prunes old archives.
//!
//! Nothing here lands, commits or pushes: the archive is the whole durable
//! copy, and only a person or a worker chooses what to restore from it.
//!
//! Retention: `ENGINE_POD_SALVAGE_KEEP` archives per session (default 5) and
//! `ENGINE_POD_SALVAGE_MAX_BYTES` across them (default 128 MiB). Both are
//! documented in `docs/ENGINE.md`.

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};
use uuid::Uuid;
use vogt_engine_contract::salvage::{
    is_secret_path, repo_slug, SalvageLimits, SalvageReport, SalvageState, SalvageUpload,
    DEFAULT_KEEP_PER_SESSION, DEFAULT_MAX_SESSION_BYTES,
};

use vogt_session_agent::salvage::safe_relative;

/// The route the session agent POSTs a salvage to.
pub const SALVAGE_STORE_ROUTE: &str = "/api/agent-auth/store/bundle/salvage";

/// The most one salvage request may carry, before base64 overhead.
pub const MAX_SALVAGE_BODY_BYTES: usize = 64 * 1024 * 1024;

/// Where archives live for one session.
pub fn session_dir(state_dir: &Path, session: Uuid) -> PathBuf {
    state_dir.join("pod-salvage").join(session.to_string())
}

/// Write `upload` for `session` and prune old archives.
pub fn store(
    state_dir: &Path,
    session: Uuid,
    upload: &SalvageUpload,
    limits: &SalvageLimits,
    keep: usize,
    max_session_bytes: u64,
    now: OffsetDateTime,
) -> Result<SalvageReport, String> {
    if upload.is_empty() {
        return Ok(SalvageReport {
            state: SalvageState::Saved,
            path: None,
            files: 0,
            bytes: 0,
            repos: Vec::new(),
            reason: Some("nothing to save".into()),
            at: now.format(&Rfc3339).unwrap_or_default(),
        });
    }
    let root = session_dir(state_dir, session);
    create_private_dir(&root)?;
    let stamp = archive_stamp(&root, now);
    let archive = root.join(stamp);
    create_private_dir(&archive)?;

    let mut files = 0u64;
    let mut bytes = 0u64;
    let mut repos = Vec::new();
    let mut notes = Vec::new();
    let mut total = 0u64;
    for repo in &upload.repos {
        let slug = repo_slug(&repo.name);
        if repos.contains(&slug) {
            notes.push(format!("{}: duplicate repository name skipped", repo.name));
            continue;
        }
        let dir = archive.join("repos").join(&slug);
        create_private_dir(&dir)?;
        let mut wrote = false;
        if !repo.patch.is_empty() {
            let patch = repo.patch.as_bytes();
            total = total.saturating_add(patch.len() as u64);
            if total > limits.max_total_bytes {
                notes.push(format!("{}: patch left out (archive size cap)", repo.name));
            } else {
                write_private(&dir.join("patch.diff"), patch)?;
                files += 1;
                bytes += patch.len() as u64;
                wrote = true;
            }
        }
        if !repo.untracked.is_empty() {
            let mut text = String::new();
            for path in &repo.untracked {
                if safe_relative(path) && !is_secret_path(path) {
                    text.push_str(path);
                    text.push('\n');
                } else {
                    notes.push(format!("{}: excluded from the untracked list", path));
                }
            }
            if !text.is_empty() {
                write_private(&dir.join("untracked.txt"), text.as_bytes())?;
                files += 1;
                bytes += text.len() as u64;
                wrote = true;
            }
        }
        for file in &repo.untracked_files {
            if !safe_relative(&file.path) {
                notes.push(format!("{}: unsafe path skipped", file.path));
                continue;
            }
            if is_secret_path(&file.path) {
                notes.push(format!("{}: credential skipped", file.path));
                continue;
            }
            let Ok(decoded) = BASE64.decode(file.bytes.as_bytes()) else {
                notes.push(format!("{}: invalid data skipped", file.path));
                continue;
            };
            if decoded.len() as u64 > limits.max_untracked_file_bytes {
                notes.push(format!("{}: over the per-file cap", file.path));
                continue;
            }
            total = total.saturating_add(decoded.len() as u64);
            if total > limits.max_total_bytes {
                notes.push(format!("{}: over the archive size cap", file.path));
                continue;
            }
            let dest = dir.join("untracked").join(&file.path);
            if let Some(parent) = dest.parent() {
                create_private_dir(parent)?;
            }
            write_private(&dest, &decoded)?;
            files += 1;
            bytes += decoded.len() as u64;
            wrote = true;
        }
        if !repo.skipped.is_empty() {
            let text = repo
                .skipped
                .iter()
                .map(|s| format!("{}\t{:?}\n", s.path, s.reason))
                .collect::<String>();
            write_private(&dir.join("skipped.txt"), text.as_bytes())?;
            files += 1;
            bytes += text.len() as u64;
        }
        if !wrote && !repo.skipped.is_empty() {
            notes.push(format!("{}: paths listed only, nothing written", repo.name));
        }
        if wrote || !repo.skipped.is_empty() || repo.truncated {
            repos.push(slug);
        }
    }
    let report = SalvageReport {
        state: SalvageState::Saved,
        path: Some(archive.display().to_string()),
        files,
        bytes,
        repos,
        reason: (!notes.is_empty()).then(|| notes.join("; ")),
        at: now.format(&Rfc3339).unwrap_or_default(),
    };
    let manifest = serde_json::to_vec_pretty(&Manifest {
        version: 1,
        session,
        report: &report,
        upload,
    })
    .map_err(|e| format!("could not serialize the salvage manifest: {e}"))?;
    write_private(&archive.join("manifest.json"), &manifest)?;
    // The copy inside the manifest counts the content files it describes;
    // the report a caller reads also counts the manifest itself.
    let mut report = report;
    report.files = count_files(&archive);
    prune(&root, keep, max_session_bytes, &archive);
    Ok(report)
}

#[derive(serde::Serialize)]
struct Manifest<'a> {
    version: u32,
    session: Uuid,
    report: &'a SalvageReport,
    upload: &'a SalvageUpload,
}

fn archive_stamp(root: &Path, now: OffsetDateTime) -> String {
    let base = format!(
        "{:04}{:02}{:02}T{:02}{:02}{:02}Z",
        now.year(),
        u8::from(now.month()),
        now.day(),
        now.hour(),
        now.minute(),
        now.second()
    );
    let mut stamp = base.clone();
    let mut n = 1;
    while root.join(&stamp).exists() {
        stamp = format!("{base}-{n}");
        n += 1;
    }
    stamp
}

fn create_private_dir(path: &Path) -> Result<(), String> {
    fs::create_dir_all(path).map_err(|e| format!("could not create {}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o700));
    }
    Ok(())
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|e| format!("could not write {}: {e}", path.display()))?;
    file.write_all(bytes)
        .map_err(|e| format!("could not write {}: {e}", path.display()))
}

fn count_files(dir: &Path) -> u64 {
    let mut n = 0;
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            n += count_files(&entry.path());
        } else if kind.is_file() {
            n += 1;
        }
    }
    n
}

/// Keep the newest `keep` archives and at most `max_bytes` across them. The
/// archive just written is always kept; a person reading the report can
/// recover it before the next stop.
pub fn prune(root: &Path, keep: usize, max_bytes: u64, current: &Path) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    let mut archives: Vec<(String, PathBuf, u64)> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| {
            let path = e.path();
            (
                e.file_name().to_string_lossy().into_owned(),
                path.clone(),
                dir_size(&path),
            )
        })
        .collect();
    // The stamp sorts chronologically; `-N` suffixes after it keep order too.
    archives.sort_by(|a, b| b.0.cmp(&a.0));
    let mut total = 0u64;
    for (index, (_, path, size)) in archives.iter().enumerate() {
        let is_current = path == current;
        let keep_it = index < keep || is_current;
        let fits = total.saturating_add(*size) <= max_bytes || is_current;
        if keep_it && fits {
            total = total.saturating_add(*size);
            continue;
        }
        let _ = fs::remove_dir_all(path);
    }
}

fn dir_size(dir: &Path) -> u64 {
    let mut bytes = 0;
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            bytes += dir_size(&entry.path());
        } else if kind.is_file() {
            bytes += entry.metadata().map(|m| m.len()).unwrap_or(0);
        }
    }
    bytes
}

/// Defaults a deployment gets when it configures nothing.
pub fn defaults() -> (usize, u64) {
    (DEFAULT_KEEP_PER_SESSION, DEFAULT_MAX_SESSION_BYTES)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vogt_engine_contract::salvage::{
        SalvageRepoUpload, SalvageSkip, SalvageUntrackedFile, SkipReason,
    };

    fn state() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "vogt-pod-salvage-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn upload(patch: &str, name: &str) -> SalvageUpload {
        SalvageUpload {
            repos: vec![SalvageRepoUpload {
                name: name.into(),
                patch: patch.into(),
                untracked: vec!["new.txt".into()],
                ..Default::default()
            }],
        }
    }

    fn at(offset_seconds: i64) -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(1_800_000_000 + offset_seconds).unwrap()
    }

    #[test]
    fn an_archive_holds_the_patch_and_is_private() {
        let state = state();
        let session = Uuid::new_v4();
        let report = store(
            &state,
            session,
            &upload("--- a/x\n+++ b/x\n", "root"),
            &SalvageLimits::default(),
            5,
            DEFAULT_MAX_SESSION_BYTES,
            at(0),
        )
        .unwrap();
        let path = Path::new(report.path.as_deref().unwrap());
        assert!(path.join("repos/root/patch.diff").is_file());
        assert!(path.join("manifest.json").is_file());
        assert_eq!(report.files, 3);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o700
            );
            assert_eq!(
                fs::metadata(path.join("manifest.json"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn secrets_and_escapes_are_never_written() {
        let state = state();
        let session = Uuid::new_v4();
        let mut up = upload("", "root");
        up.repos[0].untracked_files = vec![
            SalvageUntrackedFile {
                path: "../escape".into(),
                bytes: BASE64.encode(b"x"),
            },
            SalvageUntrackedFile {
                path: "keys/server.pem".into(),
                bytes: BASE64.encode(b"x"),
            },
            SalvageUntrackedFile {
                path: "src/ok.txt".into(),
                bytes: BASE64.encode(b"ok"),
            },
        ];
        let report = store(
            &state,
            session,
            &up,
            &SalvageLimits::default(),
            5,
            DEFAULT_MAX_SESSION_BYTES,
            at(0),
        )
        .unwrap();
        let path = Path::new(report.path.as_deref().unwrap());
        assert!(!path.join("repos/root/untracked/escape").exists());
        assert!(!path.join("repos/root/untracked/keys/server.pem").exists());
        assert!(path.join("repos/root/untracked/src/ok.txt").is_file());
    }

    #[test]
    fn retention_keeps_the_newest_n() {
        let state = state();
        let session = Uuid::new_v4();
        for i in 0..6 {
            store(
                &state,
                session,
                &upload(&format!("patch {i}"), "root"),
                &SalvageLimits::default(),
                3,
                DEFAULT_MAX_SESSION_BYTES,
                at(i),
            )
            .unwrap();
        }
        let dir = session_dir(&state, session);
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 3);
    }

    #[test]
    fn retention_drops_the_oldest_when_the_total_size_is_over() {
        let state = state();
        let session = Uuid::new_v4();
        let big = "x".repeat(2000);
        for i in 0..4 {
            store(
                &state,
                session,
                &upload(&big, "root"),
                &SalvageLimits::default(),
                10,
                3000,
                at(i),
            )
            .unwrap();
        }
        let dir = session_dir(&state, session);
        let kept: Vec<_> = fs::read_dir(&dir).unwrap().flatten().collect();
        assert!(kept.len() <= 2, "{} archives kept", kept.len());
        assert!(!kept.is_empty(), "the newest archive must survive");
    }

    #[test]
    fn a_skipped_path_still_produces_an_archive_with_a_note() {
        let state = state();
        let session = Uuid::new_v4();
        let mut up = upload("", "root");
        up.repos[0].untracked.clear();
        up.repos[0].skipped = vec![SalvageSkip {
            path: ".env".into(),
            reason: SkipReason::Secret,
        }];
        let report = store(
            &state,
            session,
            &up,
            &SalvageLimits::default(),
            5,
            DEFAULT_MAX_SESSION_BYTES,
            at(0),
        )
        .unwrap();
        assert_eq!(report.state, SalvageState::Saved);
        assert!(report
            .reason
            .as_deref()
            .unwrap_or_default()
            .contains("nothing written"));
    }

    #[test]
    fn project_memory_is_restored_under_the_pods_claude_project() {
        let state = state();
        let root = memory_dir(&state).join(project_key("org/repo"));
        create_private_dir(&root.join("notes")).unwrap();
        write_private(&root.join("notes/todo.md"), b"remember me").unwrap();
        write_private(&root.join(".env"), b"SECRET=1").unwrap();
        let bundle = memory_bundle(&state, "org/repo", "/work");
        assert_eq!(
            bundle,
            vec![(
                "projects/-work/memory/notes/todo.md".to_string(),
                b"remember me".to_vec()
            )]
        );
        assert!(memory_bundle(&state, "org/other", "/work").is_empty());
        assert_eq!(
            claude_project_slug("/home/vogt/Working"),
            "-home-vogt-Working"
        );
    }

    #[test]
    fn project_keys_are_stable_and_safe() {
        assert_eq!(project_key("org/repo"), "org_repo");
        assert_eq!(project_key("../evil"), "evil");
        assert_eq!(project_key(""), "project");
    }

    #[test]
    fn an_archive_over_the_total_cap_keeps_paths_only() {
        let state = state();
        let session = Uuid::new_v4();
        let limits = SalvageLimits {
            max_total_bytes: 1,
            ..SalvageLimits::default()
        };
        let report = store(
            &state,
            session,
            &upload("a patch", "root"),
            &limits,
            5,
            DEFAULT_MAX_SESSION_BYTES,
            at(0),
        )
        .unwrap();
        let path = Path::new(report.path.as_deref().unwrap());
        assert!(!path.join("repos/root/patch.diff").exists());
        assert!(path.join("repos/root/untracked.txt").is_file());
    }
}

/// Where project memory lives on the engine's state volume (WI-3371).
pub fn memory_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("pod-memory")
}

/// A filesystem-safe, stable key for a project's source identity.
pub fn project_key(project: &str) -> String {
    let key: String = project
        .trim()
        .trim_matches('/')
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '_'
            }
        })
        .collect();
    let key = key
        .trim_start_matches(['_', '.'])
        .trim_end_matches('_')
        .to_string();
    if key.is_empty() {
        "project".to_string()
    } else {
        key.chars().take(180).collect()
    }
}

/// Claude Code's project directory name for `cwd`: the path with separators
/// turned into dashes (`/work` -> `-work`).
pub fn claude_project_slug(cwd: &str) -> String {
    cwd.trim_end_matches('/').replace('/', "-")
}

/// The memory bundle entries a pod gets at launch, keyed exactly as the
/// entrypoint writes them under `CLAUDE_CONFIG_DIR`. Restore is read-only
/// from the pod's point of view: it only happens at bootstrap, and the files
/// are the engine's copy.
pub fn memory_bundle(state_dir: &Path, project: &str, pod_cwd: &str) -> Vec<(String, Vec<u8>)> {
    let root = memory_dir(state_dir).join(project_key(project));
    let slug = claude_project_slug(pod_cwd);
    let mut out = Vec::new();
    if !root.is_dir() {
        return out;
    }
    collect_memory(&root, &slug, &mut out);
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn collect_memory(root: &Path, slug: &str, out: &mut Vec<(String, Vec<u8>)>) {
    collect_memory_from(root, root, slug, out);
}

fn collect_memory_from(root: &Path, dir: &Path, slug: &str, out: &mut Vec<(String, Vec<u8>)>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            collect_memory_from(root, &path, slug, out);
            continue;
        }
        if !kind.is_file() {
            continue;
        }
        let Ok(rel) = path.strip_prefix(root) else {
            continue;
        };
        let rel = rel.to_string_lossy().replace('\\', "/");
        if !safe_relative(&rel) || is_secret_path(&rel) {
            continue;
        }
        let Ok(bytes) = fs::read(&path) else {
            continue;
        };
        out.push((format!("projects/{slug}/memory/{rel}"), bytes));
    }
}

#[derive(serde::Deserialize)]
struct MemoryUpload {
    project: String,
    path: String,
    bytes: String,
}

fn bearer(request: &axum::extract::Request<axum::body::Body>) -> Option<String> {
    request
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
}

/// A pod uploads the salvage its agent built. Authenticated with the pod's
/// own stream credential, like the account bundle: it is the only credential
/// a pod holds. The engine applies its own bounds and never trusts the
/// agent's counts.
pub async fn upload(
    axum::extract::State(state): axum::extract::State<std::sync::Arc<crate::app::AppState>>,
    request: axum::extract::Request<axum::body::Body>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let Some(presented) = bearer(&request) else {
        return crate::error::ApiError::Unauthorized.into_response();
    };
    let Some(session) = crate::secret_broker::presenting_session(&state, &presented).await else {
        return crate::error::ApiError::Unauthorized.into_response();
    };
    let declared = request
        .headers()
        .get(axum::http::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<usize>().ok());
    if declared.is_some_and(|n| n > MAX_SALVAGE_BODY_BYTES) {
        return crate::error::ApiError::BadRequest("the salvage is too large".into())
            .into_response();
    }
    let body = match axum::body::to_bytes(request.into_body(), MAX_SALVAGE_BODY_BYTES).await {
        Ok(body) => body,
        Err(_) => {
            return crate::error::ApiError::BadRequest("the salvage is too large".into())
                .into_response()
        }
    };
    let Ok(upload) = serde_json::from_slice::<SalvageUpload>(&body) else {
        return crate::error::ApiError::BadRequest("invalid salvage request".into())
            .into_response();
    };
    let (_, keep, max) = crate::sessions::salvage_settings();
    match store(
        state.sessions.state_dir(),
        session,
        &upload,
        &SalvageLimits::default(),
        keep,
        max,
        OffsetDateTime::now_utc(),
    ) {
        Ok(report) => {
            state.sessions.set_salvage(session, report.clone());
            axum::Json(report).into_response()
        }
        Err(reason) => {
            tracing::warn!(session = %session, reason = %reason, "could not write a salvage archive");
            let report = SalvageReport::failed(
                reason,
                OffsetDateTime::now_utc()
                    .format(&Rfc3339)
                    .unwrap_or_default(),
            );
            state.sessions.set_salvage(session, report.clone());
            axum::Json(report).into_response()
        }
    }
}

/// Project memory a Claude pod writes back (WI-3371), only while
/// `ENGINE_POD_PERSIST_MEMORY` is on.
pub async fn memory_upload(
    axum::extract::State(state): axum::extract::State<std::sync::Arc<crate::app::AppState>>,
    request: axum::extract::Request<axum::body::Body>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    if !persist_memory_enabled() {
        return crate::error::ApiError::NotFound.into_response();
    }
    let Some(presented) = bearer(&request) else {
        return crate::error::ApiError::Unauthorized.into_response();
    };
    let Some(_session) = crate::secret_broker::presenting_session(&state, &presented).await else {
        return crate::error::ApiError::Unauthorized.into_response();
    };
    let body = match axum::body::to_bytes(request.into_body(), 2 * 1024 * 1024).await {
        Ok(body) => body,
        Err(_) => {
            return crate::error::ApiError::BadRequest("the memory file is too large".into())
                .into_response()
        }
    };
    let Ok(upload) = serde_json::from_slice::<MemoryUpload>(&body) else {
        return crate::error::ApiError::BadRequest("invalid memory request".into()).into_response();
    };
    if !safe_relative(&upload.path) || is_secret_path(&upload.path) {
        return crate::error::ApiError::BadRequest("unsafe memory path".into()).into_response();
    }
    let Ok(bytes) = BASE64.decode(upload.bytes.as_bytes()) else {
        return crate::error::ApiError::BadRequest("invalid memory data".into()).into_response();
    };
    if bytes.len() as u64 > vogt_engine_contract::salvage::DEFAULT_MAX_UNTRACKED_FILE_BYTES {
        return crate::error::ApiError::BadRequest("the memory file is too large".into())
            .into_response();
    }
    let root = memory_dir(state.sessions.state_dir()).join(project_key(&upload.project));
    let dest = root.join(&upload.path);
    let result = dest
        .parent()
        .ok_or_else(|| "no parent".to_string())
        .and_then(create_private_dir)
        .and_then(|_| write_private(&dest, &bytes));
    match result {
        Ok(()) => axum::Json(serde_json::json!({"ok": true})).into_response(),
        Err(reason) => {
            tracing::warn!(reason = %reason, "could not store project memory");
            crate::error::ApiError::Internal(reason).into_response()
        }
    }
}

/// `ENGINE_POD_PERSIST_MEMORY` is off unless the operator turned it on.
pub fn persist_memory_enabled() -> bool {
    std::env::var("ENGINE_POD_PERSIST_MEMORY")
        .is_ok_and(|v| matches!(v.trim(), "1" | "true" | "yes" | "on"))
}
