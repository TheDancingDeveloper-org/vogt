use std::sync::Arc;

use axum::{
    extract::{Query, State},
    Json,
};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::process::Command;

use crate::{
    app::AppState,
    error::{ApiError, Result},
};

/// Tracks GUI processes we've launched via `/api/gui/launch`. Read-only from
/// the perspective of the launched process — when it exits naturally, the
/// next call to `processes` cleans the stale entry up.
#[derive(Debug, Clone, Serialize)]
pub struct GuiProc {
    pub pid: u32,
    pub command: Vec<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub launched_at: time::OffsetDateTime,
}

pub struct GuiRegistry {
    procs: Mutex<Vec<GuiProc>>,
}

impl GuiRegistry {
    pub fn new() -> Self {
        Self {
            procs: Mutex::new(Vec::new()),
        }
    }

    fn add(&self, p: GuiProc) {
        self.procs.lock().push(p);
    }

    /// Snapshot the list, dropping entries whose pid no longer exists.
    fn list_alive(&self) -> Vec<GuiProc> {
        let mut g = self.procs.lock();
        g.retain(|p| pid_alive(p.pid));
        g.clone()
    }

    pub fn count_alive(&self) -> usize {
        self.list_alive().len()
    }
}

impl Default for GuiRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    // Signal 0 returns ESRCH if the process is gone, EPERM if it exists but
    // we lack permission. Either case other than "alive and ours" we treat
    // as alive=false to be safe.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

#[cfg(not(unix))]
fn pid_alive(_pid: u32) -> bool {
    true
}

#[derive(Debug, Deserialize)]
pub struct LaunchReq {
    /// argv to exec. Treated as opaque — we do NOT pass through a shell, so
    /// shell metacharacters in args are literal.
    pub command: Vec<String>,
    /// If true, prefix with `swaymsg exec --` so the process is owned by sway
    /// and inherits its WAYLAND_DISPLAY. Requires sway running.
    #[serde(default)]
    pub via_sway: bool,
}

pub async fn launch(
    State(state): State<Arc<AppState>>,
    Json(req): Json<LaunchReq>,
) -> Result<Json<GuiProc>> {
    if req.command.is_empty() {
        return Err(ApiError::BadRequest("command must not be empty".into()));
    }
    let (exe, args): (String, Vec<String>) = if req.via_sway {
        // swaymsg exec -- <argv...>
        let mut all = vec!["exec".to_string(), "--".to_string()];
        all.extend(req.command.iter().cloned());
        ("swaymsg".to_string(), all)
    } else {
        (req.command[0].clone(), req.command[1..].to_vec())
    };

    let child = Command::new(&exe)
        .args(&args)
        // Detach from the parent's stdio so a backgrounded GUI app doesn't
        // wedge the parent terminal if it writes to stderr.
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| ApiError::Internal(format!("spawn {exe}: {e}")))?;

    let pid = match child.id() {
        Some(p) => p,
        None => return Err(ApiError::Internal("no pid for spawned child".into())),
    };
    let proc = GuiProc {
        pid,
        command: req.command,
        launched_at: time::OffsetDateTime::now_utc(),
    };
    state.gui.add(proc.clone());

    // We intentionally don't await the child — it lives until killed externally
    // (X11/Wayland window close, swaykill, kill_proc endpoint). The blocking
    // wait task ensures we don't accumulate zombies; portable_pty uses
    // process_id() differently — here it's a tokio Child so we just spawn the
    // wait in the background.
    tokio::spawn(async move {
        let mut child = child;
        let _ = child.wait().await;
    });

    Ok(Json(proc))
}

pub async fn processes(State(state): State<Arc<AppState>>) -> Json<Vec<GuiProc>> {
    Json(state.gui.list_alive())
}

#[derive(Debug, Deserialize)]
pub struct KillQuery {
    pub pid: u32,
}

pub async fn kill_proc(
    State(_state): State<Arc<AppState>>,
    Query(q): Query<KillQuery>,
) -> Result<Json<serde_json::Value>> {
    #[cfg(unix)]
    unsafe {
        let rc = libc::kill(q.pid as libc::pid_t, libc::SIGTERM);
        if rc != 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() != Some(libc::ESRCH) {
                return Err(ApiError::Internal(format!("kill({}): {err}", q.pid)));
            }
        }
    }
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// Public-ish endpoint exposing the bits of config the web UI needs at boot.
/// Intentionally NOT behind bearer auth: it returns no secrets, and the
/// browser fetches it before the user has typed the token into Settings.
#[derive(Debug, Serialize)]
pub struct PublicConfig {
    pub gui_stream_url: Option<String>,
    pub version: &'static str,
    /// Build-time feature availability, read from `/etc/mydevenv2/features.json`.
    /// `{"selkies": "1.6.2"}` when present, `{"selkies": null}` when the image
    /// was built without Selkies. UI hides the GUI tab when selkies is null.
    pub features: serde_json::Value,
    /// Session templates for quick session creation.
    pub session_templates: Vec<crate::config::SessionTemplate>,
    /// Whether the conversational assistant is provisioned (key present).
    /// Presence only — never the key itself.
    pub assistant_enabled: bool,
    /// Whether this front door has a vogt-core behind it, and where its
    /// surfaces are mounted. Presence only, never a token: a client that has
    /// to provoke a 503 to find out whether Vogt exists cannot render an
    /// honest absent state (FR-U21), and a Vogt tab that appears and then
    /// errors is worse than one that says why it is disabled.
    pub vogt: serde_json::Value,
    /// Model id the assistant uses, for display. None when disabled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assistant_model: Option<String>,
}

pub async fn public_config(State(state): State<Arc<AppState>>) -> Json<PublicConfig> {
    Json(PublicConfig {
        gui_stream_url: state.config.gui_stream_url.clone(),
        version: env!("CARGO_PKG_VERSION"),
        features: load_features(),
        session_templates: state.config.session_templates.clone(),
        vogt: crate::vogt_core::public_status(&state),
        assistant_enabled: state.assistant.is_some(),
        assistant_model: state.assistant.as_ref().map(|a| a.model().to_string()),
    })
}

fn load_features() -> serde_json::Value {
    // Read once per request — the file is tiny and avoids needing a refresh
    // on process restart if the operator updates it out of band.
    std::fs::read_to_string("/etc/mydevenv2/features.json")
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| serde_json::json!({}))
}
