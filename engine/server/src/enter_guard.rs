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

/// Ctrl-U: delete to the start of the line, the one erase both agent TUIs
/// read as a line kill.
const LINE_KILL: u8 = 0x15;

/// Tries to clear a stranded line before giving up, and the gap between them.
const CLEAR_ATTEMPTS: usize = 5;
const CLEAR_GAP: Duration = Duration::from_secs(1);

/// Pause between typing a line and its Enter, so a TUI reads the Enter as a
/// keypress rather than the tail of a paste.
const TYPE_TO_ENTER: Duration = Duration::from_millis(300);

/// What the screen showed across the stable window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Clearance {
    Clear,
    Dialog,
    ReadFailed,
    /// The session has a stranded line whose erase has not gone through, so
    /// the composer holds text an Enter would submit. Nothing is typed.
    Stranded,
}

/// While a session is stranded, every automated line and Enter defers until
/// the background erase clears the composer (WI-4459 re-review). The erase
/// itself does not defer: it is the one write that clears the mark.
fn held_for_stranded(session: &Session) -> Option<Clearance> {
    session
        .stranded_input()
        .is_some()
        .then_some(Clearance::Stranded)
}

/// Whether any text reached the PTY before a skip. A caller that requeues a
/// skipped line must drop the parts already written (a steer's cancel byte,
/// for one), or a retry sends them twice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Typed {
    /// Nothing was written.
    Nothing,
    /// The line was typed, the Enter was held back, and the line was erased
    /// with Ctrl-U. Every byte written, including any cancel, has gone out.
    Erased,
}

/// What became of one automated Enter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnterOutcome {
    /// The Enter went out.
    Sent,
    /// The line did not go out and the composer is clean: nothing was left
    /// behind, so the caller may requeue the line and try again. The [`Typed`]
    /// part says whether bytes were written first.
    Skipped(Clearance, Typed),
    /// The text was typed, the Enter was held back, and the composer could
    /// not be cleared (a dialog stayed open). The text is still in the
    /// composer, so the caller must not retype it. The session is marked
    /// stranded until the erase goes through.
    Stranded(Clearance),
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
    let clear = match held_for_stranded(session) {
        Some(held) => held,
        None => clearance(session).await,
    };
    if clear != Clearance::Clear {
        skipped(session, what, clear);
        return Ok(EnterOutcome::Skipped(clear, Typed::Nothing));
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
    type_bytes_then_enter(session, line.as_bytes(), what).await
}

/// [`type_then_enter`] for bytes that are not plain text, such as a steer's
/// cancel byte followed by its text. The bytes are written only after a clear
/// screen. If the Enter is then held back, the typed line is erased (Ctrl-U,
/// sent only on a clear screen, never into a dialog) and the caller gets
/// [`EnterOutcome::Skipped`] and may requeue. If it cannot be erased, the
/// caller gets [`EnterOutcome::Stranded`].
pub(crate) async fn type_bytes_then_enter(
    session: &Arc<Session>,
    typed: &[u8],
    what: &str,
) -> io::Result<EnterOutcome> {
    let clear = match held_for_stranded(session) {
        Some(held) => held,
        None => clearance(session).await,
    };
    if clear != Clearance::Clear {
        skipped(session, what, clear);
        return Ok(EnterOutcome::Skipped(clear, Typed::Nothing));
    }
    session.write_input(typed)?;
    tokio::time::sleep(TYPE_TO_ENTER).await;
    match send_enter(session, what).await? {
        EnterOutcome::Sent => Ok(EnterOutcome::Sent),
        EnterOutcome::Skipped(reason, _) => {
            if erase_line(session, what).await {
                Ok(EnterOutcome::Skipped(reason, Typed::Erased))
            } else {
                Ok(EnterOutcome::Stranded(reason))
            }
        }
        EnterOutcome::Stranded(reason) => Ok(EnterOutcome::Stranded(reason)),
    }
}

/// Erase the typed line with Ctrl-U once the screen reads clear. Retries a few
/// times, [`CLEAR_GAP`] apart, so a dialog that is answered soon does not
/// strand the text. Returns false if the line could not be erased, in which
/// case the session is marked stranded and the erase is retried in the
/// background until it goes through (WI-4459).
async fn erase_line(session: &Arc<Session>, what: &str) -> bool {
    for attempt in 0..CLEAR_ATTEMPTS {
        if attempt > 0 {
            tokio::time::sleep(CLEAR_GAP).await;
        }
        if clearance(session).await == Clearance::Clear {
            if session.write_input(&[LINE_KILL]).is_err() {
                break;
            }
            return true;
        }
    }
    tracing::warn!(
        session = %session.id,
        what,
        event = "enter.stranded",
        "held back an automatic Enter and could not clear the typed line; it stays in the composer"
    );
    if session.mark_stranded(what) {
        tokio::spawn(retry_erase_when_clear(Arc::clone(session)));
    }
    false
}

/// Keep trying the Ctrl-U erase of a stranded line until the dialog closes,
/// then clear the session's stranded mark. Ends early if the session exits.
async fn retry_erase_when_clear(session: Arc<Session>) {
    loop {
        tokio::time::sleep(CLEAR_GAP).await;
        if !session.is_alive() {
            return;
        }
        if clearance(&session).await == Clearance::Clear {
            if session.write_input(&[LINE_KILL]).is_ok() {
                session.clear_stranded();
                tracing::info!(
                    session = %session.id,
                    event = "enter.unstranded",
                    "the dialog closed; erased the stranded line"
                );
            }
            return;
        }
    }
}

fn skipped(session: &Session, what: &str, clear: Clearance) {
    let reason = match clear {
        Clearance::Dialog => "a permission dialog is open",
        Clearance::ReadFailed => "the screen could not be read",
        Clearance::Stranded => "a stranded line is waiting to be erased",
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
        assert_eq!(
            outcome,
            EnterOutcome::Skipped(Clearance::Dialog, Typed::Nothing)
        );
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
        // The dialog never clears, so the line cannot be erased: it is stranded
        // in the composer and nothing more is written.
        assert_eq!(outcome, EnterOutcome::Stranded(Clearance::Dialog));
        assert_eq!(*writes.lock().unwrap(), b"carry on");
    }

    /// WI-4459: a dialog holds the Enter back after the text, then clears
    /// before the erase gives up. The typed line is erased with Ctrl-U and the
    /// caller gets `Skipped`, so it may requeue the line without a double.
    #[tokio::test]
    async fn a_held_back_line_is_erased_once_the_dialog_clears() {
        let writes = Arc::new(Mutex::new(Vec::new()));
        let (session, output) = recording_session(Arc::clone(&writes));
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(350)).await;
            output.send(dialog_output()).unwrap();
            tokio::time::sleep(Duration::from_millis(1100)).await;
            output.send(b"\x1b[2J\x1b[H".to_vec()).unwrap();
            std::future::pending::<()>().await;
        });
        let outcome = type_then_enter(&session, "carry on", "test").await.unwrap();
        assert_eq!(
            outcome,
            EnterOutcome::Skipped(Clearance::Dialog, Typed::Erased)
        );
        assert_eq!(*writes.lock().unwrap(), b"carry on\x15");
        assert!(session.stranded_input().is_none());
    }

    /// WI-4459: the dialog holds the line through every erase try. The session
    /// is marked stranded, and the Ctrl-U goes out once the dialog closes.
    #[tokio::test]
    async fn a_stranded_line_marks_the_session_until_the_erase_goes_through() {
        let writes = Arc::new(Mutex::new(Vec::new()));
        let (session, output) = recording_session(Arc::clone(&writes));
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(350)).await;
            output.send(dialog_output()).unwrap();
            // Long enough that every erase try (1 s apart) reads the dialog.
            tokio::time::sleep(Duration::from_secs(6)).await;
            output.send(b"\x1b[2J\x1b[H".to_vec()).unwrap();
            std::future::pending::<()>().await;
        });
        let outcome = type_then_enter(&session, "carry on", "test").await.unwrap();
        assert_eq!(outcome, EnterOutcome::Stranded(Clearance::Dialog));
        assert!(session.stranded_input().is_some());
        assert_eq!(*writes.lock().unwrap(), b"carry on");

        // The background retry erases the line once the dialog is gone.
        for _ in 0..200 {
            if session.stranded_input().is_none() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(session.stranded_input().is_none());
        assert_eq!(*writes.lock().unwrap(), b"carry on\x15");
    }

    /// WI-4459: an orchestrator steer types its cancel byte and text through
    /// the guard, then sends the Enter. A dialog drawn after the text holds
    /// the Enter back. The steer used to write text and Enter in one write
    /// with no screen check.
    #[tokio::test]
    async fn a_steer_enter_is_held_back_when_a_dialog_is_drawn_after_the_text() {
        let writes = Arc::new(Mutex::new(Vec::new()));
        let (session, output) = recording_session(Arc::clone(&writes));
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(350)).await;
            output.send(dialog_output()).unwrap();
            std::future::pending::<()>().await;
        });
        let typed = b"\x03stop";
        let outcome = type_bytes_then_enter(&session, typed, "orchestrator steer")
            .await
            .unwrap();
        assert_eq!(outcome, EnterOutcome::Stranded(Clearance::Dialog));
        assert_eq!(*writes.lock().unwrap(), typed);
    }

    /// WI-4459 re-review: while a session is stranded, another automated line
    /// must not be typed onto the stranded text, and a bare Enter must not
    /// submit it. Both defer until the erase clears the mark. Without the
    /// stranded check the line is typed and the Enter sent.
    #[tokio::test]
    async fn an_automated_line_is_deferred_while_the_session_is_stranded() {
        let writes = Arc::new(Mutex::new(Vec::new()));
        let (session, _output) = recording_session(Arc::clone(&writes));
        assert!(session.mark_stranded("carry on"));
        let outcome = type_then_enter(&session, "nudge", "test").await.unwrap();
        assert_eq!(
            outcome,
            EnterOutcome::Skipped(Clearance::Stranded, Typed::Nothing)
        );
        let outcome = send_enter(&session, "test").await.unwrap();
        assert_eq!(
            outcome,
            EnterOutcome::Skipped(Clearance::Stranded, Typed::Nothing)
        );
        assert!(writes.lock().unwrap().is_empty());
    }
}
