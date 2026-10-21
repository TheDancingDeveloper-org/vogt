//! The read-only terminal screen's state and rules, with no egui and no I/O
//! (WI-2005).
//!
//! Opening a session reads `GET /api/sessions/{id}/screen` and paints it into
//! a [`TerminalGrid`] at once, then attaches to the session's WebSocket. The
//! attach snapshot replaces that first paint and live output follows. A lost
//! attach reconnects with backoff and resumes after the last output it
//! rendered, so the server sends only the gap. Activity events for the
//! session re-read the screen.
//!
//! The view never types and never resizes the PTY: the grid takes the size
//! the server reports, so watching cannot fight the program's own clients.

use std::time::Duration;

use fluent2_egui::data::terminal::{TerminalGrid, TerminalState};
use web_time::Instant;

use crate::api::{ApiError, AttachControl, SessionScreen};
use crate::backend::AttachEvent;
use crate::backoff::Backoff;
use crate::model::Command;

/// How often a live attach is probed.
pub const ATTACH_PING_EVERY: Duration = Duration::from_secs(15);
/// How long a live attach may go silent, pongs included, before it is
/// treated as dead.
pub const ATTACH_STALE_AFTER: Duration = Duration::from_secs(45);

/// The state of the terminal attach.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttachState {
    /// No session open.
    Off,
    /// Opening.
    Connecting,
    /// Open; output is live.
    Live,
    /// Lost; the next try is at `retry_at`.
    Waiting {
        /// When to try again.
        retry_at: Instant,
        /// Tries since the attach was last open.
        attempt: u32,
    },
    /// The session is hibernated: the screen is its last one and nothing
    /// live follows until it wakes.
    Hibernated,
    /// The session is gone; no more tries.
    Gone,
}

/// The terminal screen for one session.
#[derive(Debug)]
pub struct TerminalModel {
    /// The session showing, if any.
    pub session_id: Option<String>,
    /// The last `/screen` read: title, activity and liveness for the header.
    pub screen: Option<SessionScreen>,
    /// Why the last `/screen` read failed.
    pub screen_error: Option<String>,
    /// The attach.
    pub attach: AttachState,
    /// The painted terminal.
    pub grid: TerminalGrid,
    /// Selection and search state for the view.
    pub view: TerminalState,
    /// The absolute output position the grid has rendered up to, once an
    /// attach snapshot has arrived. A reconnect resumes from here.
    pub pos: Option<u64>,
    generation: u64,
    backoff: Backoff,
    /// Inside a snapshot: its bytes do not advance `pos`, which the snapshot
    /// header already set to the snapshot's end.
    in_snapshot: bool,
    /// When the attach last delivered anything.
    seen: Option<Instant>,
    /// When the next liveness probe is due.
    ping_at: Option<Instant>,
    ping_id: u64,
}

impl Default for TerminalModel {
    fn default() -> Self {
        Self {
            session_id: None,
            screen: None,
            screen_error: None,
            attach: AttachState::Off,
            grid: TerminalGrid::new(80, 24),
            view: TerminalState::default(),
            pos: None,
            generation: 0,
            backoff: Backoff::default(),
            in_snapshot: false,
            seen: None,
            ping_at: None,
            ping_id: 0,
        }
    }
}

impl TerminalModel {
    /// The current attach generation; replies from older ones are ignored.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Show session `id`: read its screen and attach.
    pub fn open(&mut self, id: &str) -> Vec<Command> {
        if self.session_id.as_deref() == Some(id) && !matches!(self.attach, AttachState::Off) {
            return Vec::new();
        }
        let mut commands = self.close();
        self.session_id = Some(id.to_owned());
        self.backoff.reset();
        commands.push(Command::FetchScreen(id.to_owned()));
        commands.extend(self.connect());
        commands
    }

    /// Stop showing the session: detach and forget it.
    pub fn close(&mut self) -> Vec<Command> {
        let was_open = self.session_id.take().is_some();
        // Keep the generation: the next attach bumps it past any late reply.
        *self = Self {
            generation: self.generation,
            ..Self::default()
        };
        if was_open { vec![Command::Detach] } else { Vec::new() }
    }

    fn connect(&mut self) -> Vec<Command> {
        let Some(id) = self.session_id.clone() else {
            return Vec::new();
        };
        self.generation += 1;
        self.attach = AttachState::Connecting;
        self.in_snapshot = false;
        vec![Command::Attach {
            id,
            generation: self.generation,
            resume_from: self.pos,
        }]
    }

    /// The attach has not painted anything, so a `/screen` read is the best
    /// picture there is.
    fn screen_is_authoritative(&self) -> bool {
        self.pos.is_none()
    }

    /// Fold a `/screen` reply in. Returns the error when the credential is
    /// refused, so the caller can sign out.
    pub fn apply_screen(&mut self, id: &str, result: Result<Box<SessionScreen>, ApiError>) -> Result<(), ApiError> {
        if self.session_id.as_deref() != Some(id) {
            return Ok(()); // the user has moved on
        }
        match result {
            Ok(screen) => {
                if self.screen_is_authoritative() {
                    paint_screen(&mut self.grid, &screen);
                }
                self.screen = Some(*screen);
                self.screen_error = None;
                Ok(())
            }
            Err(error) if error.is_unauthorized() => Err(error),
            Err(error) => {
                if error.status == Some(404) {
                    // The session is gone: stop trying to attach to it.
                    self.generation += 1;
                    self.attach = AttachState::Gone;
                }
                self.screen_error = Some(error.to_string());
                Ok(())
            }
        }
    }

    /// Fold an attach event in.
    pub fn apply_attach(&mut self, generation: u64, event: AttachEvent, now: Instant) -> Vec<Command> {
        if generation != self.generation || self.session_id.is_none() {
            return Vec::new(); // from an attach this model has already replaced
        }
        match event {
            AttachEvent::Opened => {
                self.seen = Some(now);
                Vec::new()
            }
            AttachEvent::Bytes(bytes) => {
                self.seen = Some(now);
                self.grid.process(&bytes);
                if !self.in_snapshot
                    && let Some(pos) = &mut self.pos
                {
                    *pos += bytes.len() as u64;
                }
                Vec::new()
            }
            AttachEvent::Control(control) => {
                self.seen = Some(now);
                self.on_control(control, now)
            }
            AttachEvent::Closed(_) => {
                if !matches!(self.attach, AttachState::Connecting | AttachState::Live) {
                    return Vec::new(); // already handled (hibernated, or an error then a close)
                }
                self.wait_to_reconnect(now);
                // Close codes are not visible here: re-read the screen to learn
                // whether the credential (401) or the session (404) is gone.
                self.session_id.clone().map(Command::FetchScreen).into_iter().collect()
            }
        }
    }

    fn on_control(&mut self, control: AttachControl, now: Instant) -> Vec<Command> {
        match control {
            AttachControl::SnapshotStart {
                scrollback_pos,
                reset,
                cols,
                rows,
            } => {
                let (cols, rows) = (
                    cols.map_or(self.grid.cols(), usize::from).max(1),
                    rows.map_or(self.grid.rows(), usize::from).max(1),
                );
                if reset || self.pos.is_none() {
                    self.grid = TerminalGrid::new(cols, rows);
                    self.view = TerminalState::default();
                } else if (cols, rows) != (self.grid.cols(), self.grid.rows()) {
                    self.grid.resize(cols, rows);
                }
                self.pos = Some(scrollback_pos);
                self.in_snapshot = true;
                Vec::new()
            }
            AttachControl::SnapshotDone => {
                self.in_snapshot = false;
                if self.attach == AttachState::Connecting {
                    self.attach = AttachState::Live;
                    self.backoff.reset();
                    self.ping_at = Some(now + ATTACH_PING_EVERY);
                }
                Vec::new()
            }
            AttachControl::Resize { cols, rows } => {
                self.grid.resize(usize::from(cols).max(1), usize::from(rows).max(1));
                Vec::new()
            }
            AttachControl::Lag => {
                // The server gives up on this socket; start again from a full snapshot.
                self.pos = None;
                Vec::new()
            }
            AttachControl::Hibernated => {
                self.attach = AttachState::Hibernated;
                self.in_snapshot = false;
                Vec::new()
            }
            AttachControl::Pong { .. } | AttachControl::Other => Vec::new(),
        }
    }

    /// An `activity` or `session-killed` event for this session: re-read the
    /// screen for the header, and wake a hibernated view.
    pub fn on_activity(&mut self, id: &str) -> Vec<Command> {
        if self.session_id.as_deref() != Some(id) {
            return Vec::new();
        }
        let mut commands = vec![Command::FetchScreen(id.to_owned())];
        if self.attach == AttachState::Hibernated {
            self.pos = None;
            commands.extend(self.connect());
        }
        commands
    }

    fn wait_to_reconnect(&mut self, now: Instant) {
        let attempt = self.backoff.attempt() + 1;
        let retry_at = now + self.backoff.next_delay();
        self.attach = AttachState::Waiting { retry_at, attempt };
        self.seen = None;
        self.ping_at = None;
        self.in_snapshot = false;
    }

    fn stale_at(&self) -> Option<Instant> {
        match (self.attach, self.seen) {
            (AttachState::Live, Some(seen)) => Some(seen + ATTACH_STALE_AFTER),
            _ => None,
        }
    }

    /// Advance time: probe a live attach, drop a silent one, and reconnect
    /// when a wait is over.
    pub fn tick(&mut self, now: Instant) -> Vec<Command> {
        if self.stale_at().is_some_and(|at| now >= at) {
            self.generation += 1;
            self.wait_to_reconnect(now);
            return vec![Command::Detach];
        }
        match self.attach {
            AttachState::Waiting { retry_at, .. } if now >= retry_at => self.connect(),
            AttachState::Live if self.ping_at.is_some_and(|at| now >= at) => {
                self.ping_id += 1;
                self.ping_at = Some(now + ATTACH_PING_EVERY);
                vec![Command::AttachPing(self.ping_id)]
            }
            _ => Vec::new(),
        }
    }

    /// How long until [`TerminalModel::tick`] has something to do, if anything.
    pub fn next_wake(&self, now: Instant) -> Option<Duration> {
        let at = match self.attach {
            AttachState::Waiting { retry_at, .. } => Some(retry_at),
            AttachState::Live => [self.stale_at(), self.ping_at].into_iter().flatten().min(),
            _ => None,
        };
        at.map(|at| at.saturating_duration_since(now))
    }
}

/// Paint a `/screen` read into `grid`: its size, its lines, its cursor.
pub fn paint_screen(grid: &mut TerminalGrid, screen: &SessionScreen) {
    let cols = usize::from(screen.cols).max(1);
    let rows = usize::from(screen.rows).max(1);
    *grid = TerminalGrid::new(cols, rows);
    let mut bytes = Vec::new();
    for (i, line) in screen.lines.iter().take(rows).enumerate() {
        if i > 0 {
            bytes.extend_from_slice(b"\r\n");
        }
        // Text only: a stray escape in the rendered text must not drive the grid.
        bytes.extend(line.chars().filter(|c| !c.is_control()).collect::<String>().bytes());
    }
    bytes.extend(format!("\x1b[{};{}H", screen.cursor.row + 1, screen.cursor.col + 1).bytes());
    grid.process(&bytes);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::screen;

    fn text(grid: &TerminalGrid) -> String {
        grid.visible_text()
    }

    fn opened() -> (TerminalModel, Instant) {
        let mut t = TerminalModel::default();
        let commands = t.open("s1");
        assert_eq!(
            commands,
            vec![
                Command::FetchScreen("s1".into()),
                Command::Attach {
                    id: "s1".into(),
                    generation: 1,
                    resume_from: None
                }
            ]
        );
        (t, Instant::now())
    }

    fn snapshot(t: &mut TerminalModel, now: Instant, pos: u64, reset: bool, bytes: &[u8]) {
        let g = t.generation();
        t.apply_attach(g, AttachEvent::Opened, now);
        t.apply_attach(
            g,
            AttachEvent::Control(AttachControl::SnapshotStart {
                scrollback_pos: pos,
                reset,
                cols: Some(20),
                rows: Some(4),
            }),
            now,
        );
        t.apply_attach(g, AttachEvent::Bytes(bytes.to_vec()), now);
        t.apply_attach(g, AttachEvent::Control(AttachControl::SnapshotDone), now);
    }

    #[test]
    fn screen_paints_until_the_attach_takes_over() {
        let (mut t, now) = opened();
        t.apply_screen("s1", Ok(Box::new(screen("s1", &["hello", "world"], "idle"))))
            .unwrap();
        assert!(text(&t.grid).starts_with("hello\nworld"));
        assert_eq!(t.grid.cursor(), (1, 5));
        assert_eq!((t.grid.cols(), t.grid.rows()), (40, 6));

        snapshot(&mut t, now, 7, true, b"live!\r\n");
        assert_eq!(t.attach, AttachState::Live);
        assert_eq!(t.pos, Some(7));
        assert_eq!((t.grid.cols(), t.grid.rows()), (20, 4));
        assert!(text(&t.grid).starts_with("live!"));

        // Once live, a later screen read updates the header only.
        t.apply_screen("s1", Ok(Box::new(screen("s1", &["stale"], "running"))))
            .unwrap();
        assert!(text(&t.grid).starts_with("live!"));
        assert_eq!(t.screen.as_ref().map(|s| s.activity.as_str()), Some("running"));
    }

    #[test]
    fn live_bytes_advance_the_position_and_resize_follows_the_server() {
        let (mut t, now) = opened();
        snapshot(&mut t, now, 10, true, b"0123456789");
        t.apply_attach(1, AttachEvent::Bytes(b"abc".to_vec()), now);
        assert_eq!(t.pos, Some(13));
        t.apply_attach(
            1,
            AttachEvent::Control(AttachControl::Resize { cols: 30, rows: 5 }),
            now,
        );
        assert_eq!((t.grid.cols(), t.grid.rows()), (30, 5));
    }

    #[test]
    fn a_dropped_attach_backs_off_and_resumes_warm() {
        let (mut t, now) = opened();
        snapshot(&mut t, now, 4, true, b"one\n");
        let commands = t.apply_attach(1, AttachEvent::Closed("reset".into()), now);
        assert_eq!(commands, vec![Command::FetchScreen("s1".into())]);
        let AttachState::Waiting { retry_at, attempt: 1 } = t.attach else {
            panic!("{:?}", t.attach)
        };
        assert_eq!(t.next_wake(now), Some(Duration::from_millis(500)));
        // A second close for the same attach changes nothing.
        assert!(t.apply_attach(1, AttachEvent::Closed("again".into()), now).is_empty());
        assert!(t.tick(now).is_empty());
        assert_eq!(
            t.tick(retry_at),
            vec![Command::Attach {
                id: "s1".into(),
                generation: 2,
                resume_from: Some(4)
            }]
        );
        // The delta is appended, not repainted.
        snapshot(&mut t, retry_at, 8, false, b"two\n");
        assert_eq!(t.pos, Some(8));
        assert!(text(&t.grid).contains("one") && text(&t.grid).contains("two"));
    }

    #[test]
    fn repeated_failures_grow_the_delay() {
        let (mut t, mut now) = opened();
        let mut delays = Vec::new();
        for _ in 0..4 {
            let g = t.generation();
            t.apply_attach(g, AttachEvent::Closed("refused".into()), now);
            let wait = t.next_wake(now).unwrap();
            delays.push(wait.as_millis());
            now += wait;
            assert!(matches!(t.tick(now)[..], [Command::Attach { .. }]));
        }
        assert_eq!(delays, vec![500, 1000, 2000, 4000]);
    }

    #[test]
    fn lag_forces_a_cold_reattach() {
        let (mut t, now) = opened();
        snapshot(&mut t, now, 4, true, b"one\n");
        t.apply_attach(1, AttachEvent::Control(AttachControl::Lag), now);
        t.apply_attach(1, AttachEvent::Closed("closed".into()), now);
        let retry = now + t.next_wake(now).unwrap();
        assert!(matches!(
            &t.tick(retry)[..],
            [Command::Attach { resume_from: None, .. }]
        ));
    }

    #[test]
    fn live_attach_is_pinged_and_dropped_when_silent() {
        let (mut t, now) = opened();
        snapshot(&mut t, now, 0, true, b"");
        assert_eq!(t.next_wake(now), Some(ATTACH_PING_EVERY));
        let ping = now + ATTACH_PING_EVERY;
        assert_eq!(t.tick(ping), vec![Command::AttachPing(1)]);
        // Nothing comes back: dead after the stale window.
        let dead = now + ATTACH_STALE_AFTER;
        assert_eq!(t.tick(dead), vec![Command::Detach]);
        assert!(matches!(t.attach, AttachState::Waiting { attempt: 1, .. }));
        // A late frame from the retired attach is ignored.
        assert!(t.apply_attach(1, AttachEvent::Bytes(b"late".to_vec()), dead).is_empty());
        assert_eq!(t.pos, Some(0));
    }

    #[test]
    fn hibernated_stops_until_activity_wakes_it() {
        let (mut t, now) = opened();
        snapshot(&mut t, now, 3, true, b"zzz");
        t.apply_attach(1, AttachEvent::Control(AttachControl::Hibernated), now);
        assert!(
            t.apply_attach(1, AttachEvent::Closed("hibernated".into()), now)
                .is_empty()
        );
        assert_eq!(t.attach, AttachState::Hibernated);
        assert_eq!(t.next_wake(now), None);
        assert!(t.on_activity("other").is_empty());
        let commands = t.on_activity("s1");
        assert_eq!(commands[0], Command::FetchScreen("s1".into()));
        assert!(matches!(&commands[1], Command::Attach { resume_from: None, .. }));
    }

    #[test]
    fn missing_session_stops_retrying_and_401_is_returned() {
        let (mut t, now) = opened();
        t.apply_attach(1, AttachEvent::Closed("closed".into()), now);
        let gone = ApiError {
            status: Some(404),
            message: "no such session".into(),
        };
        t.apply_screen("s1", Err(gone)).unwrap();
        assert_eq!(t.attach, AttachState::Gone);
        assert!(t.tick(now + Duration::from_secs(60)).is_empty());
        let refused = ApiError {
            status: Some(401),
            message: "no".into(),
        };
        assert!(t.apply_screen("s1", Err(refused)).is_err());
    }

    #[test]
    fn closing_detaches_and_ignores_late_replies() {
        let (mut t, now) = opened();
        assert_eq!(t.close(), vec![Command::Detach]);
        assert!(t.close().is_empty());
        assert!(t.apply_attach(1, AttachEvent::Opened, now).is_empty());
        t.apply_screen("s1", Ok(Box::new(screen("s1", &["late"], "idle"))))
            .unwrap();
        assert!(t.screen.is_none());
        assert_eq!(t.attach, AttachState::Off);
    }

    #[test]
    fn screen_text_cannot_inject_escapes() {
        let mut grid = TerminalGrid::new(10, 2);
        paint_screen(&mut grid, &screen("s", &["a\x1b[2Jb"], "idle"));
        assert!(grid.visible_text().starts_with("a[2Jb"));
    }
}
