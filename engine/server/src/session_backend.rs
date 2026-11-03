//! Execution backends for engine sessions.
//!
//! The registry and session contract deliberately do not know whether a
//! session is backed by a local PTY or a future remote pod.

use std::io::{Read, Write};

use portable_pty::{Child, ExitStatus, MasterPty, PtySize};

use crate::error::{ApiError, Result};

/// The process-facing portion of a session. Scrollback, activity, events and
/// history remain owned by [`crate::pty::Session`].
pub trait SessionBackend: Send + Sync {
    fn write_input(&self, data: &[u8]) -> std::io::Result<()>;
    fn resize(&self, cols: u16, rows: u16) -> Result<()>;
    fn signal_stop(&self) -> Result<()>;
    fn current_size(&self) -> Result<(u16, u16)>;
    fn wait(&self) -> std::io::Result<ExitStatus>;
}

/// The unchanged local portable-pty implementation.
pub struct LocalBackend {
    writer: parking_lot::Mutex<Box<dyn Write + Send>>,
    master: parking_lot::Mutex<Box<dyn MasterPty + Send>>,
    child: parking_lot::Mutex<Option<Box<dyn Child + Send>>>,
    pid: Option<u32>,
}

impl LocalBackend {
    pub fn new(
        master: Box<dyn MasterPty + Send>,
        writer: Box<dyn Write + Send>,
        child: Box<dyn Child + Send>,
    ) -> Self {
        let pid = child.process_id();
        Self {
            writer: parking_lot::Mutex::new(writer),
            master: parking_lot::Mutex::new(master),
            child: parking_lot::Mutex::new(Some(child)),
            pid,
        }
    }

    pub fn pid(&self) -> Option<u32> {
        self.pid
    }
}

impl SessionBackend for LocalBackend {
    fn write_input(&self, data: &[u8]) -> std::io::Result<()> {
        let mut writer = self.writer.lock();
        writer.write_all(data)?;
        writer.flush()
    }

    fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        self.master
            .lock()
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| ApiError::Pty(format!("resize: {e}")))
    }

    fn signal_stop(&self) -> Result<()> {
        #[cfg(unix)]
        if let Some(pid) = self.pid {
            // Preserve portable-pty's kill semantics, including ignoring an
            // already-reaped process.
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

    fn current_size(&self) -> Result<(u16, u16)> {
        self.master
            .lock()
            .get_size()
            .map(|size| (size.rows, size.cols))
            .map_err(|e| ApiError::Pty(format!("size: {e}")))
    }

    fn wait(&self) -> std::io::Result<ExitStatus> {
        let mut child = self.child.lock().take().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::NotFound, "child already waited")
        })?;
        child.wait()
    }
}

/// The backend's output reader is intentionally acquired only during spawn;
/// the registry consumes the resulting stream exactly as it did for a PTY.
pub fn local(
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send>,
) -> Result<(LocalBackend, Box<dyn Read + Send>)> {
    let reader = master
        .try_clone_reader()
        .map_err(|e| ApiError::Pty(format!("clone_reader: {e}")))?;
    let writer = master
        .take_writer()
        .map_err(|e| ApiError::Pty(format!("take_writer: {e}")))?;
    Ok((LocalBackend::new(master, writer, child), reader))
}
