use std::{
    io::{Read, Write},
    sync::Arc,
    time::Instant,
};

use bytes::Bytes;
use parking_lot::Mutex;
use portable_pty::{CommandBuilder, PtySize};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::{
    activity::{classify, ActivityState},
    error::{ApiError, Result},
    events::{EventBus, ServerEvent},
    scrollback::Scrollback,
};

/// One chunk of PTY output. `pos` is the byte offset (in the lifetime stream)
/// at which `data` begins. Lets re-attaching clients de-duplicate scrollback
/// replay vs live broadcast.
#[derive(Debug, Clone)]
pub struct OutputChunk {
    pub pos: u64,
    pub data: Bytes,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSpec {
    pub name: String,
    #[serde(default)]
    pub command: Option<Vec<String>>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub env: Option<Vec<(String, String)>>,
    #[serde(default)]
    pub cols: Option<u16>,
    #[serde(default)]
    pub rows: Option<u16>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionSummary {
    pub id: Uuid,
    pub name: String,
    pub activity: ActivityState,
    pub exit_code: Option<i32>,
    pub scrollback_bytes: u64,
    pub cwd: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: time::OffsetDateTime,
}

pub struct Session {
    pub id: Uuid,
    pub name: Mutex<String>,
    pub created_at: time::OffsetDateTime,
    pub cwd: String,
    idle_after_ms: u64,

    scrollback: Mutex<Scrollback>,
    writer: Mutex<Box<dyn Write + Send>>,
    master: Mutex<Box<dyn portable_pty::MasterPty + Send>>,
    child: Mutex<Option<Box<dyn portable_pty::Child + Send>>>,
    /// Cached child PID — `child` is taken by the wait task, so SIGKILL needs
    /// its own handle on Unix.
    pid: Option<u32>,

    output_tx: broadcast::Sender<OutputChunk>,
    last_output: Mutex<Option<Instant>>,
    activity: Mutex<ActivityState>,
    exit_code: Mutex<Option<i32>>,
}

impl Session {
    pub fn name(&self) -> String {
        self.name.lock().clone()
    }

    pub fn activity(&self) -> ActivityState {
        *self.activity.lock()
    }

    pub fn exit_code(&self) -> Option<i32> {
        *self.exit_code.lock()
    }

    pub fn summary(&self) -> SessionSummary {
        let sb = self.scrollback.lock();
        SessionSummary {
            id: self.id,
            name: self.name.lock().clone(),
            activity: *self.activity.lock(),
            exit_code: *self.exit_code.lock(),
            scrollback_bytes: sb.total_written(),
            cwd: self.cwd.clone(),
            created_at: self.created_at,
        }
    }

    /// Snapshot scrollback plus the byte position immediately after the
    /// snapshot. Attached clients use the position to drop already-replayed
    /// broadcast chunks.
    pub fn snapshot(&self) -> (Bytes, u64) {
        let sb = self.scrollback.lock();
        (sb.snapshot(), sb.total_written())
    }

    pub fn subscribe(&self) -> broadcast::Receiver<OutputChunk> {
        self.output_tx.subscribe()
    }

    pub fn write_input(&self, data: &[u8]) -> std::io::Result<()> {
        let mut w = self.writer.lock();
        w.write_all(data)?;
        w.flush()
    }

    pub fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        let master = self.master.lock();
        master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| ApiError::Pty(format!("resize: {e}")))?;
        Ok(())
    }

    pub fn rename(&self, new_name: String) {
        *self.name.lock() = new_name;
    }

    pub fn kill(&self) -> Result<()> {
        // Prefer signalling by pid: the wait task has typically already taken
        // ownership of the `Child` to call `wait()`, leaving the Mutex empty.
        #[cfg(unix)]
        if let Some(pid) = self.pid {
            // SIGKILL — same semantics as portable_pty::Child::kill on unix.
            // Ignore ESRCH (child already reaped).
            unsafe {
                let rc = libc::kill(pid as libc::pid_t, libc::SIGKILL);
                if rc != 0 {
                    let err = std::io::Error::last_os_error();
                    if err.raw_os_error() != Some(libc::ESRCH) {
                        return Err(ApiError::Pty(format!("kill({pid}): {err}")));
                    }
                }
            }
            return Ok(());
        }
        if let Some(child) = self.child.lock().as_mut() {
            child
                .kill()
                .map_err(|e| ApiError::Pty(format!("kill: {e}")))?;
        }
        Ok(())
    }
}

pub struct SpawnedSession {
    pub session: Arc<Session>,
}

/// Spawn a PTY-backed session running `spec.command` (or the default shell if
/// `None`). Starts the reader thread, the exit waiter, and the activity ticker.
pub fn spawn(
    spec: &SessionSpec,
    default_shell: &str,
    default_cwd: &std::path::Path,
    scrollback_bytes: usize,
    activity_idle_after_ms: u64,
    bus: EventBus,
) -> Result<SpawnedSession> {
    let pty_system = portable_pty::native_pty_system();
    let size = PtySize {
        rows: spec.rows.unwrap_or(24),
        cols: spec.cols.unwrap_or(80),
        pixel_width: 0,
        pixel_height: 0,
    };
    let pair = pty_system
        .openpty(size)
        .map_err(|e| ApiError::Pty(format!("openpty: {e}")))?;

    let mut cmd = match spec.command.as_ref() {
        Some(argv) if !argv.is_empty() => {
            let mut c = CommandBuilder::new(&argv[0]);
            for a in &argv[1..] {
                c.arg(a);
            }
            c
        }
        _ => CommandBuilder::new(default_shell),
    };
    let cwd = spec
        .cwd
        .as_deref()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| default_cwd.to_path_buf());
    let cwd_display = cwd.to_string_lossy().into_owned();
    cmd.cwd(cwd);
    cmd.env("TERM", "xterm-256color");
    cmd.env("MYDEVENV2_SESSION", &spec.name);
    if let Some(env) = spec.env.as_ref() {
        for (k, v) in env {
            cmd.env(k, v);
        }
    }

    let child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| ApiError::Pty(format!("spawn: {e}")))?;
    let pid = child.process_id();
    drop(pair.slave);

    let reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| ApiError::Pty(format!("clone_reader: {e}")))?;
    let writer = pair
        .master
        .take_writer()
        .map_err(|e| ApiError::Pty(format!("take_writer: {e}")))?;

    let (tx, _rx) = broadcast::channel::<OutputChunk>(1024);

    let session = Arc::new(Session {
        id: Uuid::new_v4(),
        name: Mutex::new(spec.name.clone()),
        created_at: time::OffsetDateTime::now_utc(),
        cwd: cwd_display,
        idle_after_ms: activity_idle_after_ms,
        scrollback: Mutex::new(Scrollback::new(scrollback_bytes)),
        writer: Mutex::new(writer),
        master: Mutex::new(pair.master),
        child: Mutex::new(Some(child)),
        pid,
        output_tx: tx.clone(),
        last_output: Mutex::new(None),
        activity: Mutex::new(ActivityState::Running),
        exit_code: Mutex::new(None),
    });

    spawn_reader_thread(Arc::clone(&session), reader, tx, bus.clone())?;
    spawn_exit_waiter(Arc::clone(&session), bus.clone());
    spawn_activity_ticker(Arc::clone(&session), bus);

    Ok(SpawnedSession { session })
}

fn spawn_reader_thread(
    session: Arc<Session>,
    mut reader: Box<dyn Read + Send>,
    tx: broadcast::Sender<OutputChunk>,
    bus: EventBus,
) -> Result<()> {
    std::thread::Builder::new()
        .name(format!("pty-read-{}", session.id))
        .spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        let data = Bytes::copy_from_slice(&buf[..n]);
                        let pos_after = {
                            let mut sb = session.scrollback.lock();
                            sb.push(&data);
                            sb.total_written()
                        };
                        let pos = pos_after - n as u64;
                        *session.last_output.lock() = Some(Instant::now());

                        let _ = tx.send(OutputChunk { pos, data });

                        let new_state = compute_activity(&session, false);
                        update_activity_if_changed(&session, new_state, &bus);
                    }
                    Err(e) => {
                        tracing::debug!(session = %session.id, error = %e, "pty reader closed");
                        break;
                    }
                }
            }
        })
        .map_err(|e| ApiError::Pty(format!("spawn pty reader thread: {e}")))?;
    Ok(())
}

fn spawn_exit_waiter(session: Arc<Session>, bus: EventBus) {
    tokio::task::spawn_blocking(move || {
        let mut child = match session.child.lock().take() {
            Some(c) => c,
            None => return,
        };
        let status = match child.wait() {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(session = %session.id, error = %e, "child wait failed");
                return;
            }
        };
        let code = status.exit_code() as i32;
        *session.exit_code.lock() = Some(code);
        let new_state = compute_activity(&session, code != 0);
        update_activity_if_changed(&session, new_state, &bus);
        bus.publish(ServerEvent::SessionKilled {
            id: session.id,
            exit_code: Some(code),
        });
    });
}

fn spawn_activity_ticker(session: Arc<Session>, bus: EventBus) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(std::time::Duration::from_millis(500));
        loop {
            ticker.tick().await;
            let exited = session.exit_code.lock().is_some();
            let nonzero = session.exit_code().map(|c| c != 0).unwrap_or(false);
            let new = compute_activity(&session, nonzero);
            update_activity_if_changed(&session, new, &bus);
            if exited {
                break;
            }
        }
    });
}

fn compute_activity(session: &Arc<Session>, exited_nonzero: bool) -> ActivityState {
    let tail = {
        let sb = session.scrollback.lock();
        sb.tail(2048).to_vec()
    };
    let last = *session.last_output.lock();
    classify(last, &tail, session.idle_after_ms, exited_nonzero)
}

fn update_activity_if_changed(session: &Arc<Session>, new: ActivityState, bus: &EventBus) {
    let mut a = session.activity.lock();
    if *a != new {
        *a = new;
        drop(a);
        bus.publish(ServerEvent::Activity {
            id: session.id,
            state: new,
        });
    }
}
