//! The one way the engine presses Enter on its own (WI-1001, WI-4459).
//!
//! A blind Enter accepts whatever the agent CLI highlights, so a permission
//! dialog drawn after the engine's last look would be approved by it. Every
//! automated Enter goes through [`send_enter`] (or [`type_then_enter`], which
//! types a line first). Both read the screen twice, [`STABLE_GAP`] apart, and
//! press Enter only when both reads are dialog-free. A dialog, or a read that
//! fails, skips the Enter and logs the skip, so nothing is dropped silently.
//!
//! The last read still comes before the write, not at the same instant, so a
//! dialog drawn in that sliver is not seen. The stable window narrows that to
//! a single redraw, which is the best a terminal screen read can give.

use std::io;
use std::sync::Arc;
use std::time::Duration;

use crate::pty::Session;

/// Dialog-free reads needed before an automated Enter is sent.
const STABLE_READS: usize = 2;

/// Gap between two reads, so a dialog still being drawn shows up in the second.
pub(crate) const STABLE_GAP: Duration = Duration::from_millis(200);

/// Pause between typing a line and its Enter, so a TUI reads the Enter as a
/// keypress rather than the tail of a paste.
const TYPE_TO_ENTER: Duration = Duration::from_millis(300);

/// What the screen showed across the stable window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Clearance {
    Clear,
    Dialog,
    ReadFailed,
}

/// What became of one automated Enter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnterOutcome {
    Sent,
    Skipped(Clearance),
}

/// Read the screen [`STABLE_READS`] times, [`STABLE_GAP`] apart. Any dialog
/// or failed read ends the check.
pub(crate) async fn clearance(session: &Arc<Session>) -> Clearance {
    for read in 0..STABLE_READS {
        if read > 0 {
            tokio::time::sleep(STABLE_GAP).await;
        }
        let reading = Arc::clone(session);
        match tokio::task::spawn_blocking(move || reading.current_dialog()).await {
            Ok(None) => {}
            Ok(Some(_)) => return Clearance::Dialog,
            Err(_) => return Clearance::ReadFailed,
        }
    }
    Clearance::Clear
}

/// Press Enter on its own, if the screen is clear. `what` names the path for
/// the log line, so a skip says which automated Enter it was.
pub(crate) async fn send_enter(session: &Arc<Session>, what: &str) -> io::Result<EnterOutcome> {
    let clear = clearance(session).await;
    if clear != Clearance::Clear {
        skipped(session, what, clear);
        return Ok(EnterOutcome::Skipped(clear));
    }
    session.write_input(b"\r")?;
    Ok(EnterOutcome::Sent)
}

/// Type one line, then Enter on its own. Nothing is typed over a dialog: the
/// screen is checked before the text goes in, and again before the Enter.
pub(crate) async fn type_then_enter(
    session: &Arc<Session>,
    line: &str,
    what: &str,
) -> io::Result<EnterOutcome> {
    let clear = clearance(session).await;
    if clear != Clearance::Clear {
        skipped(session, what, clear);
        return Ok(EnterOutcome::Skipped(clear));
    }
    session.write_input(line.as_bytes())?;
    tokio::time::sleep(TYPE_TO_ENTER).await;
    send_enter(session, what).await
}

fn skipped(session: &Session, what: &str, clear: Clearance) {
    let reason = match clear {
        Clearance::Dialog => "a permission dialog is open",
        Clearance::ReadFailed => "the screen could not be read",
        Clearance::Clear => return,
    };
    tracing::warn!(
        session = %session.id,
        what,
        reason,
        event = "enter.skipped",
        "skipped an automatic Enter: {reason}"
    );
}

#[cfg(test)]
pub(crate) mod tests {
    use std::io::Read;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{channel, Receiver, SendError, Sender};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use super::*;
    use crate::events::EventBus;
    use crate::pty::{SessionSpec, SpawnDefaults};
    use crate::session_backend::SessionBackend;

    /// Stands in for the agent process: everything the engine types into the
    /// session lands in `writes`. The child exits once `done` is set, so the
    /// runtime can shut down after the test.
    pub(crate) struct RecordingBackend {
        pub(crate) writes: Arc<Mutex<Vec<u8>>>,
        done: Arc<AtomicBool>,
    }

    impl SessionBackend for RecordingBackend {
        fn write_input(&self, data: &[u8]) -> io::Result<()> {
            self.writes.lock().unwrap().extend_from_slice(data);
            Ok(())
        }
        fn resize(&self, _cols: u16, _rows: u16) -> crate::error::Result<()> {
            Ok(())
        }
        fn signal_stop(&self) -> crate::error::Result<()> {
            Ok(())
        }
        fn current_size(&self) -> crate::error::Result<(u16, u16)> {
            Ok((80, 24))
        }
        fn wait(&self) -> io::Result<portable_pty::ExitStatus> {
            while !self.done.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(portable_pty::ExitStatus::with_exit_code(0))
        }
    }

    /// The agent's output, as the session's reader sees it: each message sent
    /// on the channel is read in full.
    struct OutputPipe {
        rx: Receiver<Vec<u8>>,
        pending: io::Cursor<Vec<u8>>,
    }

    impl Read for OutputPipe {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.pending.position() as usize >= self.pending.get_ref().len() {
                match self.rx.recv() {
                    Ok(bytes) => self.pending = io::Cursor::new(bytes),
                    Err(_) => return Ok(0),
                }
            }
            self.pending.read(buf)
        }
    }

    /// The agent's output for a test. Dropping it ends the output and lets the
    /// session's child exit.
    pub(crate) struct Output {
        tx: Sender<Vec<u8>>,
        done: Arc<AtomicBool>,
    }

    impl Output {
        pub(crate) fn send(&self, bytes: Vec<u8>) -> Result<(), SendError<Vec<u8>>> {
            self.tx.send(bytes)
        }
    }

    impl Drop for Output {
        fn drop(&mut self) {
            self.done.store(true, Ordering::SeqCst);
        }
    }

    /// A remote-backed session that records its input. `Output` feeds the
    /// output the session's screen is drawn from.
    pub(crate) fn recording_session(writes: Arc<Mutex<Vec<u8>>>) -> (Arc<Session>, Output) {
        let (tx, rx) = channel();
        let done = Arc::new(AtomicBool::new(false));
        let dir = tempfile::tempdir().unwrap().keep();
        let spec = SessionSpec {
            name: "enter-guard-test".into(),
            ..Default::default()
        };
        let defaults = SpawnDefaults {
            default_shell: "/bin/sh",
            auto_agent_auth: false,
            agent_auth_helper: &dir,
            default_cwd: &dir,
            scrollback_bytes: 64 * 1024,
            activity_idle_after_ms: 1000,
            secret_broker: None,
            agent_auth_manifest: None,
            on_exit: None,
        };
        let spawned = crate::pty::spawn_remote(
            uuid::Uuid::new_v4(),
            &spec,
            defaults,
            EventBus::default(),
            None,
            Arc::new(RecordingBackend {
                writes,
                done: Arc::clone(&done),
            }),
            Box::new(OutputPipe {
                rx,
                pending: io::Cursor::new(Vec::new()),
            }),
        )
        .unwrap();
        (spawned.session, Output { tx, done })
    }

    /// Output that clears the screen and draws a permission dialog, as Claude
    /// Code does when it asks to run a command.
    pub(crate) fn dialog_output() -> Vec<u8> {
        concat!(
            "\x1b[2J\x1b[H",
            "Bash command\r\n",
            "  sh -c 'docker rm -f x'\r\n",
            "Do you want to proceed?\r\n",
            "> 1. Yes\r\n",
            "  2. No, and tell Claude what to do (esc)\r\n",
        )
        .as_bytes()
        .to_vec()
    }

    #[tokio::test]
    async fn an_enter_is_sent_on_a_clear_screen() {
        let writes = Arc::new(Mutex::new(Vec::new()));
        let (session, _output) = recording_session(Arc::clone(&writes));
        let outcome = send_enter(&session, "test").await.unwrap();
        assert_eq!(outcome, EnterOutcome::Sent);
        assert_eq!(*writes.lock().unwrap(), b"\r");
    }

    #[tokio::test]
    async fn nothing_is_typed_over_a_dialog_already_open() {
        let writes = Arc::new(Mutex::new(Vec::new()));
        let (session, output) = recording_session(Arc::clone(&writes));
        output.send(dialog_output()).unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        let outcome = type_then_enter(&session, "carry on", "test").await.unwrap();
        assert_eq!(outcome, EnterOutcome::Skipped(Clearance::Dialog));
        assert!(writes.lock().unwrap().is_empty());
    }

    /// The dialog is drawn after the text is typed, inside the gap before the
    /// Enter. The Enter must not follow it.
    #[tokio::test]
    async fn an_enter_is_held_back_when_a_dialog_appears_after_the_text() {
        let writes = Arc::new(Mutex::new(Vec::new()));
        let (session, output) = recording_session(Arc::clone(&writes));
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(350)).await;
            output.send(dialog_output()).unwrap();
            // Keep the pipe open for the session's life.
            std::future::pending::<()>().await;
        });
        let outcome = type_then_enter(&session, "carry on", "test").await.unwrap();
        assert_eq!(outcome, EnterOutcome::Skipped(Clearance::Dialog));
        assert_eq!(*writes.lock().unwrap(), b"carry on");
    }
}
