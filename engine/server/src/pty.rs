use std::{
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::Instant,
};

use bytes::Bytes;
use parking_lot::Mutex;
use portable_pty::{CommandBuilder, PtySize};
use tokio::sync::{broadcast, Notify};
use uuid::Uuid;
pub use vogt_engine_contract::{SessionSpec, SessionSummary};

use crate::{
    activity::{classify, strip_ansi, ActivityState},
    error::{ApiError, Result},
    events::{EventBus, ServerEvent},
    history::{ArchiveRecord, SessionHistory},
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

pub struct Session {
    pub id: Uuid,
    pub name: Mutex<String>,
    pub created_at: time::OffsetDateTime,
    activity_changed_at: Mutex<time::OffsetDateTime>,
    ended_at: Mutex<Option<time::OffsetDateTime>>,
    pub cwd: String,
    command: Option<String>,
    idle_after_ms: u64,

    scrollback: Mutex<Scrollback>,
    writer: Mutex<Box<dyn Write + Send>>,
    master: Mutex<Box<dyn portable_pty::MasterPty + Send>>,
    child: Mutex<Option<Box<dyn portable_pty::Child + Send>>>,
    /// Cached child PID — `child` is taken by the wait task, so SIGKILL needs
    /// its own handle on Unix.
    pid: Option<u32>,

    output_tx: broadcast::Sender<OutputChunk>,
    spawned_at: Instant,
    last_output: Mutex<Option<Instant>>,
    activity: Mutex<ActivityState>,
    /// When `activity` last changed. Used by the idle-stall watcher to tell
    /// "just went idle" apart from "has been idle for a long time".
    activity_since: Mutex<Instant>,
    activity_epoch: AtomicU64,
    activity_notify: Notify,
    exit_code: Mutex<Option<i32>>,
    history_log_path: Option<PathBuf>,
    reader_done: AtomicBool,
    archive_started: AtomicBool,
}

impl Session {
    pub fn name(&self) -> String {
        self.name.lock().clone()
    }

    pub fn activity(&self) -> ActivityState {
        *self.activity.lock()
    }

    /// How long the session has been continuously in its current activity
    /// state.
    pub fn activity_duration(&self) -> std::time::Duration {
        self.activity_since.lock().elapsed()
    }

    pub fn exit_code(&self) -> Option<i32> {
        *self.exit_code.lock()
    }

    pub fn command(&self) -> Option<String> {
        self.command.clone()
    }

    /// Last `n` bytes of scrollback (or fewer if the buffer holds less).
    pub fn tail(&self, n: usize) -> Bytes {
        Bytes::copy_from_slice(self.scrollback.lock().tail(n))
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
            command: self.command.clone(),
            created_at: format_rfc3339(self.created_at),
            activity_changed_at: format_rfc3339(*self.activity_changed_at.lock()),
        }
    }

    /// Snapshot scrollback plus the byte position immediately after the
    /// snapshot. Attached clients use the position to drop already-replayed
    /// broadcast chunks.
    pub fn snapshot(&self) -> (Bytes, u64) {
        let sb = self.scrollback.lock();
        (sb.snapshot(), sb.total_written())
    }

    /// Snapshot at most the last `limit` bytes of scrollback, aligned forward
    /// to a ground-state boundary (#532). The position returned is still the
    /// absolute `total_written`, so a client that later attaches by cursor
    /// resumes without a gap. Copies only the tail while holding the lock,
    /// rather than up to the full ring.
    pub fn snapshot_tail(&self, limit: usize) -> (Bytes, u64) {
        let sb = self.scrollback.lock();
        (sb.snapshot_tail(limit), sb.total_written())
    }

    /// Snapshot only output newer than a client cursor. The boolean tells the
    /// client whether its existing terminal state must be reset.
    ///
    /// `tail_bytes` caps a **cold** attach only (#474): a fresh client with no
    /// cache sends no `resume_from`, and without a cap the full snapshot is the
    /// entire scrollback ring. When present it bounds that full snapshot to at
    /// most that many trailing bytes. A warm reattach (`resume_from` present)
    /// ignores it entirely, so its `reset: false` delta stays byte-for-byte
    /// unchanged; the returned position (`total_written`) is unaffected by
    /// trimming the front, so the live stream still resumes with no gap.
    pub fn snapshot_for_attach(
        &self,
        resume_from: Option<u64>,
        tail_bytes: Option<usize>,
    ) -> (Bytes, u64, bool) {
        let sb = self.scrollback.lock();
        let pos = sb.total_written();
        if let Some(cursor) = resume_from {
            if let Some(delta) = sb.snapshot_since(cursor) {
                return (delta, pos, false);
            }
            // Cursor aged out of the ring: a full reset snapshot, untrimmed —
            // the tail cap is a cold-attach affordance and a warm reattach is
            // never quietly narrowed.
            return (sb.snapshot(), pos, true);
        }
        // Cold attach: bound the snapshot to the client's tail hint so first
        // open never ships the whole ring buffer.
        let snapshot = match tail_bytes {
            Some(limit) => sb.snapshot_tail(limit),
            None => sb.snapshot(),
        };
        (snapshot, pos, true)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<OutputChunk> {
        self.output_tx.subscribe()
    }

    /// Current absolute output position, used by the WebSocket liveness probe
    /// to detect a socket that is open but no longer delivering output.
    pub fn scrollback_position(&self) -> u64 {
        self.scrollback.lock().total_written()
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

fn format_rfc3339(ts: time::OffsetDateTime) -> String {
    ts.format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| ts.to_string())
}

pub struct SpawnedSession {
    pub session: Arc<Session>,
}

pub struct SpawnDefaults<'a> {
    pub default_shell: &'a str,
    pub auto_agent_auth: bool,
    pub agent_auth_helper: &'a Path,
    pub default_cwd: &'a Path,
    pub scrollback_bytes: usize,
    pub activity_idle_after_ms: u64,
}

fn command_display(spec: &SessionSpec, defaults: &SpawnDefaults<'_>) -> String {
    if let Some(argv) = spec.command.as_ref().filter(|argv| !argv.is_empty()) {
        return argv
            .iter()
            .map(|arg| shell_escape(arg))
            .collect::<Vec<_>>()
            .join(" ");
    }
    if defaults.auto_agent_auth {
        return format!("{} shell", defaults.agent_auth_helper.display());
    }
    defaults.default_shell.to_string()
}

fn shell_escape(arg: &str) -> String {
    if arg.bytes().all(|b| {
        b.is_ascii_alphanumeric() || matches!(b, b'/' | b'.' | b'_' | b'-' | b':' | b'=' | b'+')
    }) {
        arg.to_string()
    } else {
        format!("'{}'", arg.replace('\'', "'\\''"))
    }
}

/// Spawn a PTY-backed session running `spec.command` (or the default shell if
/// `None`). Starts the reader thread, the exit waiter, and the activity watcher.
///
/// `id` is allocated by the caller: the registry needs it before the spawn so
/// per-session artifacts (the prompt file) can be named for the session and
/// exist by the time the child's first line runs.
pub fn spawn(
    id: Uuid,
    spec: &SessionSpec,
    defaults: SpawnDefaults<'_>,
    bus: EventBus,
    history: Option<Arc<SessionHistory>>,
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
        _ if defaults.auto_agent_auth => {
            let mut c = CommandBuilder::new(defaults.agent_auth_helper);
            c.arg("shell");
            c
        }
        _ => CommandBuilder::new(defaults.default_shell),
    };
    let cwd = spec
        .cwd
        .as_deref()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| defaults.default_cwd.to_path_buf());
    let cwd_display = cwd.to_string_lossy().into_owned();
    cmd.cwd(cwd);
    cmd.env("TERM", "xterm-256color");
    cmd.env("MYDEVENV2_SESSION", &spec.name);
    // The session id is allocated before the spawn so the child can be told
    // which session it is. `MYDEVENV2_SESSION` is the display name and is not
    // unique, so it cannot be used to identify a session.
    cmd.env("MYDEVENV2_SESSION_ID", id.to_string());
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
    let command = Some(command_display(spec, &defaults));
    let runtime = tokio::runtime::Handle::current();
    let history_log_path = history.as_ref().map(|h| h.log_path(id));
    let history_log = history.as_ref().and_then(|h| match h.open_log_writer(id) {
        Ok(file) => Some(file),
        Err(e) => {
            tracing::warn!(session = %id, error = %e, "session history log disabled");
            None
        }
    });

    let created_at = time::OffsetDateTime::now_utc();
    let session = Arc::new(Session {
        id,
        name: Mutex::new(spec.name.clone()),
        created_at,
        activity_changed_at: Mutex::new(created_at),
        ended_at: Mutex::new(None),
        cwd: cwd_display,
        command,
        idle_after_ms: defaults.activity_idle_after_ms,
        scrollback: Mutex::new(Scrollback::new(
            spec.scrollback_bytes.unwrap_or(defaults.scrollback_bytes),
        )),
        writer: Mutex::new(writer),
        master: Mutex::new(pair.master),
        child: Mutex::new(Some(child)),
        pid,
        output_tx: tx.clone(),
        spawned_at: Instant::now(),
        last_output: Mutex::new(None),
        activity: Mutex::new(ActivityState::Running),
        activity_since: Mutex::new(Instant::now()),
        activity_epoch: AtomicU64::new(0),
        activity_notify: Notify::new(),
        exit_code: Mutex::new(None),
        history_log_path,
        reader_done: AtomicBool::new(false),
        archive_started: AtomicBool::new(false),
    });

    spawn_reader_thread(
        Arc::clone(&session),
        reader,
        tx,
        bus.clone(),
        history.clone(),
        history_log,
        runtime.clone(),
    )?;
    spawn_exit_waiter(Arc::clone(&session), bus.clone(), history.clone(), runtime);
    spawn_activity_watcher(Arc::clone(&session), bus);

    Ok(SpawnedSession { session })
}

fn spawn_reader_thread(
    session: Arc<Session>,
    mut reader: Box<dyn Read + Send>,
    tx: broadcast::Sender<OutputChunk>,
    bus: EventBus,
    history: Option<Arc<SessionHistory>>,
    mut history_log: Option<File>,
    runtime: tokio::runtime::Handle,
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

                        if let Some(log) = history_log.as_mut() {
                            if let Err(e) = log.write_all(&data) {
                                tracing::warn!(
                                    session = %session.id,
                                    error = %e,
                                    "failed to append session history log"
                                );
                                history_log = None;
                            }
                        }

                        let _ = tx.send(OutputChunk { pos, data });

                        let new_state = compute_activity(&session, false);
                        update_activity_if_changed(&session, new_state, &bus);
                        wake_activity_watcher(&session);
                    }
                    Err(e) => {
                        tracing::debug!(session = %session.id, error = %e, "pty reader closed");
                        break;
                    }
                }
            }
            if let Some(mut log) = history_log {
                let _ = log.flush();
            }
            session.reader_done.store(true, Ordering::Release);
            try_spawn_archive(&session, history, &runtime);
        })
        .map_err(|e| ApiError::Pty(format!("spawn pty reader thread: {e}")))?;
    Ok(())
}

fn spawn_exit_waiter(
    session: Arc<Session>,
    bus: EventBus,
    history: Option<Arc<SessionHistory>>,
    runtime: tokio::runtime::Handle,
) {
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
        *session.ended_at.lock() = Some(time::OffsetDateTime::now_utc());
        *session.exit_code.lock() = Some(code);
        let new_state = compute_activity(&session, code != 0);
        update_activity_if_changed(&session, new_state, &bus);
        wake_activity_watcher(&session);

        // The child can exit before the PTY reader has consumed the final
        // bytes.  Publish the terminal event only after that reader has
        // drained its stream; agent-task conclusions inspect scrollback when
        // they receive `SessionKilled` (including VOGT_SKIP and VOGT_COST
        // sentinels printed immediately before exit).
        while !session.reader_done.load(Ordering::Acquire) {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        bus.publish(ServerEvent::SessionKilled {
            id: session.id,
            exit_code: Some(code),
        });
        try_spawn_archive(&session, history, &runtime);
    });
}

fn try_spawn_archive(
    session: &Arc<Session>,
    history: Option<Arc<SessionHistory>>,
    runtime: &tokio::runtime::Handle,
) {
    let Some(history) = history else {
        return;
    };
    if !session.reader_done.load(Ordering::Acquire) {
        return;
    }
    let Some(exit_code) = session.exit_code() else {
        return;
    };
    if session
        .archive_started
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }

    let session = Arc::clone(session);
    runtime.spawn(async move {
        let output_bytes = read_history_output(&session).await;
        let visible_output = strip_ansi(&output_bytes);
        let output_text = String::from_utf8_lossy(&visible_output);
        let scrollback_bytes = session.scrollback.lock().total_written();
        let record = ArchiveRecord {
            id: session.id,
            name: session.name(),
            created_at: session.created_at,
            ended_at: (*session.ended_at.lock()).or(Some(time::OffsetDateTime::now_utc())),
            exit_code: Some(exit_code),
            cwd: Some(session.cwd.clone()),
            command: session.command.clone(),
            scrollback_bytes,
        };

        if let Err(e) = history
            .archive_session_with_output(record, output_text.as_ref())
            .await
        {
            tracing::warn!(session = %session.id, error = %e, "failed to archive session history");
        }
    });
}

/// Archive a still-live session to history immediately (#475).
///
/// Used by the graceful-shutdown drain: a long-lived agent shell that never
/// runs `exit` would otherwise be SIGKILLed on redeploy with nothing archived,
/// because the ordinary archive path (`try_spawn_archive`) waits for the exit
/// waiter to set `exit_code`. This records the row directly. `ended_at` is the
/// child's real end time if it already exited, otherwise now; `exit_code` is
/// whatever the child reported (`None` == terminated/unknown), which the
/// upsert's `COALESCE` guard will not clobber for an already-finalized row.
pub async fn archive_live_session(session: &Arc<Session>, history: &SessionHistory) {
    let output_bytes = read_history_output(session).await;
    let visible_output = strip_ansi(&output_bytes);
    let output_text = String::from_utf8_lossy(&visible_output);
    let scrollback_bytes = session.scrollback.lock().total_written();
    let record = ArchiveRecord {
        id: session.id,
        name: session.name(),
        created_at: session.created_at,
        ended_at: (*session.ended_at.lock()).or(Some(time::OffsetDateTime::now_utc())),
        exit_code: session.exit_code(),
        cwd: Some(session.cwd.clone()),
        command: session.command.clone(),
        scrollback_bytes,
    };
    if let Err(e) = history
        .archive_session_with_output(record, output_text.as_ref())
        .await
    {
        tracing::warn!(session = %session.id, error = %e, "failed to archive live session during drain");
    }
}

async fn read_history_output(session: &Arc<Session>) -> Vec<u8> {
    if let Some(path) = session.history_log_path.as_ref() {
        match tokio::fs::read(path).await {
            Ok(bytes) => return bytes,
            Err(e) => {
                tracing::warn!(
                    session = %session.id,
                    path = %path.display(),
                    error = %e,
                    "failed to read session history log; falling back to scrollback"
                );
            }
        }
    }
    session.snapshot().0.to_vec()
}

fn spawn_activity_watcher(session: Arc<Session>, bus: EventBus) {
    tokio::spawn(async move {
        loop {
            if session.exit_code.lock().is_some() {
                break;
            }

            if session.activity() != ActivityState::Running {
                session.activity_notify.notified().await;
                continue;
            }

            let reference = (*session.last_output.lock()).unwrap_or(session.spawned_at);
            let elapsed_ms = reference.elapsed().as_millis() as u64;
            if elapsed_ms >= session.idle_after_ms {
                let new_state = if session.last_output.lock().is_some() {
                    compute_activity(&session, false)
                } else {
                    ActivityState::Idle
                };
                update_activity_if_changed(&session, new_state, &bus);
                continue;
            }

            let epoch = session.activity_epoch.load(Ordering::Acquire);
            let sleep = tokio::time::sleep(std::time::Duration::from_millis(
                session.idle_after_ms - elapsed_ms,
            ));
            tokio::pin!(sleep);

            tokio::select! {
                _ = session.activity_notify.notified() => {}
                _ = &mut sleep => {
                    if session.activity_epoch.load(Ordering::Acquire) != epoch {
                        continue;
                    }
                    if session.exit_code.lock().is_some() {
                        break;
                    }
                    let new_state = if session.last_output.lock().is_some() {
                        compute_activity(&session, false)
                    } else {
                        ActivityState::Idle
                    };
                    update_activity_if_changed(&session, new_state, &bus);
                }
            }
        }
    });
}

fn wake_activity_watcher(session: &Session) {
    session.activity_epoch.fetch_add(1, Ordering::AcqRel);
    session.activity_notify.notify_one();
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
        *session.activity_since.lock() = Instant::now();
        *session.activity_changed_at.lock() = time::OffsetDateTime::now_utc();
        let activity_changed_at = format_rfc3339(*session.activity_changed_at.lock());
        bus.publish(ServerEvent::Activity {
            id: session.id,
            state: new,
            activity_changed_at,
        });
    }
}
