//! App state and the rules that change it, with no egui and no I/O.
//!
//! [`Model::apply`] folds a backend [`Reply`] into the state and [`Model::tick`]
//! advances time; both return the [`Command`]s the app must send to the
//! backend. Reconnect, reconcile-on-reconnect and sign-out-on-401 all live
//! here so they are tested without a window or a network.

use std::time::Duration;

use web_time::Instant;

use crate::api::{ApiError, Auth, ServerEvent, Session, WorkDetail, normalise_base};
use crate::backend::{Backend, Reply};
use crate::backoff::Backoff;
use crate::terminal::TerminalModel;

/// How long a live stream may go silent before it is treated as dead. The
/// engine sends a keep-alive every 15 s, so this is three missed beats.
pub const STREAM_STALE_AFTER: Duration = Duration::from_secs(45);

/// Something the backend must be asked to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    /// Log in with a password.
    Login {
        /// API base.
        base: String,
        /// Username.
        username: String,
        /// Password.
        password: String,
    },
    /// Read the session list.
    FetchSessions,
    /// Read a work item.
    FetchWorkItem(String),
    /// Open the event stream as this generation.
    OpenEvents(u64),
    /// Close the event stream.
    CloseEvents,
    /// Read a session's rendered screen.
    FetchScreen(String),
    /// Attach read-only to a session's terminal.
    Attach {
        /// Session id.
        id: String,
        /// The attach generation.
        generation: u64,
        /// Resume after this output position (a warm reattach).
        resume_from: Option<u64>,
    },
    /// Probe the terminal attach.
    AttachPing(u64),
    /// Close the terminal attach.
    Detach,
}

/// Which screen is showing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    /// Sign in.
    Login,
    /// The live session list.
    Sessions,
    /// One work item.
    WorkItem,
    /// One session's terminal, read-only.
    Terminal,
}

/// The state of the live event stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamState {
    /// Not signed in, so no stream.
    Off,
    /// Opening.
    Connecting,
    /// Open; the list is live.
    Live,
    /// Lost; the next try is at `retry_at`.
    Waiting {
        /// When to try again.
        retry_at: Instant,
        /// Tries since the stream was last open.
        attempt: u32,
    },
}

/// The login form.
#[derive(Clone, Debug, Default)]
pub struct LoginForm {
    /// API base, e.g. `https://vogt-dev.example`.
    pub base: String,
    /// Username.
    pub username: String,
    /// Password; cleared after every attempt.
    pub password: String,
    /// A pasted bearer, as an alternative to a password.
    pub token: String,
    /// Why the last attempt failed.
    pub error: Option<String>,
    /// An attempt is in flight.
    pub busy: bool,
}

/// What the work-item page shows.
#[derive(Clone, Debug, PartialEq)]
pub enum WorkView {
    /// Nothing asked for yet.
    Empty,
    /// Waiting for this reference.
    Loading(String),
    /// Loaded.
    Loaded(Box<WorkDetail>),
    /// The read failed.
    Failed(String, String),
}

/// The whole app state.
#[derive(Debug)]
pub struct Model {
    /// The screen showing.
    pub screen: Screen,
    /// The login form.
    pub login: LoginForm,
    /// The credential in use; `None` while signed out.
    pub auth: Option<Auth>,
    /// Who is signed in.
    pub actor: String,
    /// The session list.
    pub sessions: Vec<Session>,
    /// A session read is in flight.
    pub sessions_loading: bool,
    /// Why the last session read failed.
    pub sessions_error: Option<String>,
    /// When the list was last read in full.
    pub sessions_read_at: Option<Instant>,
    /// The event stream.
    pub stream: StreamState,
    /// The work-item reference typed in the page's box.
    pub work_ref: String,
    /// The work-item page.
    pub work: WorkView,
    /// The terminal screen.
    pub terminal: TerminalModel,
    generation: u64,
    backoff: Backoff,
    /// When the current stream last delivered anything.
    stream_seen: Option<Instant>,
}

impl Model {
    /// A signed-out model whose login form starts at `base`.
    pub fn new(base: impl Into<String>) -> Self {
        Self {
            screen: Screen::Login,
            login: LoginForm {
                base: base.into(),
                ..LoginForm::default()
            },
            auth: None,
            actor: String::new(),
            sessions: Vec::new(),
            sessions_loading: false,
            sessions_error: None,
            sessions_read_at: None,
            stream: StreamState::Off,
            work_ref: String::new(),
            work: WorkView::Empty,
            terminal: TerminalModel::default(),
            generation: 0,
            backoff: Backoff::default(),
            stream_seen: None,
        }
    }

    /// The current stream generation; replies from older ones are ignored.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The user pressed "Sign in".
    pub fn submit_password(&mut self) -> Vec<Command> {
        let base = normalise_base(&self.login.base);
        if base.is_empty() || self.login.username.trim().is_empty() {
            self.login.error = Some("Enter the API address and a username.".into());
            return Vec::new();
        }
        self.login.busy = true;
        self.login.error = None;
        let password = std::mem::take(&mut self.login.password);
        vec![Command::Login {
            base,
            username: self.login.username.trim().to_owned(),
            password,
        }]
    }

    /// The user pasted a bearer token instead. It is checked by the first read:
    /// a 401 comes straight back to this form.
    pub fn submit_token(&mut self) -> Vec<Command> {
        let base = normalise_base(&self.login.base);
        let token = self.login.token.trim().to_owned();
        if base.is_empty() || token.is_empty() {
            self.login.error = Some("Enter the API address and a token.".into());
            return Vec::new();
        }
        self.login.token.clear();
        self.signed_in(Auth { base, token }, "token".into())
    }

    /// Sign out and drop the credential.
    pub fn sign_out(&mut self, why: Option<String>) -> Vec<Command> {
        self.auth = None;
        self.actor.clear();
        self.sessions.clear();
        self.sessions_read_at = None;
        self.work = WorkView::Empty;
        self.stream = StreamState::Off;
        self.screen = Screen::Login;
        self.login.busy = false;
        self.login.error = why;
        self.generation += 1;
        let mut commands = vec![Command::CloseEvents];
        commands.extend(self.terminal.close());
        commands
    }

    /// Show a screen. Opening the work-item page with a reference loads it.
    pub fn open_work_item(&mut self, reference: &str) -> Vec<Command> {
        let reference = reference.trim().to_owned();
        self.screen = Screen::WorkItem;
        let mut commands = self.terminal.close();
        if reference.is_empty() {
            return commands;
        }
        self.work_ref = reference.clone();
        self.work = WorkView::Loading(reference.clone());
        commands.push(Command::FetchWorkItem(reference));
        commands
    }

    /// Show the session list, leaving any terminal.
    pub fn show_sessions(&mut self) -> Vec<Command> {
        if self.auth.is_none() {
            return Vec::new();
        }
        self.screen = Screen::Sessions;
        self.terminal.close()
    }

    /// Show a session's terminal, read-only.
    pub fn open_terminal(&mut self, id: &str) -> Vec<Command> {
        if self.auth.is_none() {
            return Vec::new();
        }
        self.screen = Screen::Terminal;
        self.terminal.open(id)
    }

    /// Re-read the session list now.
    pub fn refresh_sessions(&mut self) -> Vec<Command> {
        if self.auth.is_none() || self.sessions_loading {
            return Vec::new();
        }
        self.sessions_loading = true;
        vec![Command::FetchSessions]
    }

    fn signed_in(&mut self, auth: Auth, actor: String) -> Vec<Command> {
        self.auth = Some(auth);
        self.actor = actor;
        self.login.busy = false;
        self.login.error = None;
        self.screen = Screen::Sessions;
        self.backoff.reset();
        let mut commands = self.refresh_sessions();
        commands.extend(self.connect());
        commands
    }

    fn connect(&mut self) -> Vec<Command> {
        self.generation += 1;
        self.stream = StreamState::Connecting;
        vec![Command::OpenEvents(self.generation)]
    }

    /// Any 401 means the credential is dead: sign out rather than retry it.
    fn refused(&mut self, error: &ApiError) -> Option<Vec<Command>> {
        error
            .is_unauthorized()
            .then(|| self.sign_out(Some("Your sign-in expired. Sign in again.".into())))
    }

    /// Fold one backend reply in.
    pub fn apply(&mut self, reply: Reply, now: Instant) -> Vec<Command> {
        match reply {
            Reply::LoggedIn { base, token, actor } => self.signed_in(Auth { base, token }, actor),
            Reply::LoginFailed(error) => {
                self.login.busy = false;
                self.login.error = Some(match error.status {
                    Some(401) => "Wrong username or password.".into(),
                    Some(429) => "Too many attempts. Wait a moment and try again.".into(),
                    _ => format!("Could not sign in: {error}"),
                });
                Vec::new()
            }
            Reply::Sessions(result) => {
                self.sessions_loading = false;
                match result {
                    Ok(mut sessions) => {
                        sort_sessions(&mut sessions);
                        self.sessions = sessions;
                        self.sessions_error = None;
                        self.sessions_read_at = Some(now);
                        Vec::new()
                    }
                    Err(error) => self.refused(&error).unwrap_or_else(|| {
                        self.sessions_error = Some(error.to_string());
                        Vec::new()
                    }),
                }
            }
            Reply::WorkItem(reference, result) => {
                if self.work != WorkView::Loading(reference.clone())
                    && !matches!(&self.work, WorkView::Loaded(d) if d.item.reference == reference)
                {
                    return Vec::new(); // the user has moved on
                }
                match result {
                    Ok(detail) => {
                        self.work = WorkView::Loaded(detail);
                        Vec::new()
                    }
                    Err(error) => self.refused(&error).unwrap_or_else(|| {
                        self.work = WorkView::Failed(reference, error.to_string());
                        Vec::new()
                    }),
                }
            }
            Reply::StreamOpened(generation) if generation == self.generation => {
                self.stream = StreamState::Live;
                self.stream_seen = Some(now);
                self.backoff.reset();
                // Anything may have changed while the stream was down: reconcile.
                self.refresh_sessions()
            }
            Reply::StreamEvent(generation, event) if generation == self.generation => {
                self.stream_seen = Some(now);
                self.on_event(event)
            }
            Reply::StreamAlive(generation) if generation == self.generation => {
                self.stream_seen = Some(now);
                Vec::new()
            }
            Reply::StreamClosed(generation, error) if generation == self.generation => {
                if let Some(commands) = self.refused(&error) {
                    return commands;
                }
                self.wait_to_reconnect(now);
                Vec::new()
            }
            Reply::Screen(id, result) => match self.terminal.apply_screen(&id, result) {
                Ok(()) => Vec::new(),
                Err(error) => self.refused(&error).unwrap_or_default(),
            },
            Reply::Attach(generation, event) => self.terminal.apply_attach(generation, event, now),
            // A reply from a stream this model has already replaced.
            Reply::StreamOpened(_) | Reply::StreamEvent(..) | Reply::StreamAlive(_) | Reply::StreamClosed(..) => {
                Vec::new()
            }
        }
    }

    /// Schedule the next connect after the stream was lost.
    fn wait_to_reconnect(&mut self, now: Instant) {
        let attempt = self.backoff.attempt() + 1;
        let retry_at = now + self.backoff.next_delay();
        self.stream = StreamState::Waiting { retry_at, attempt };
        self.stream_seen = None;
    }

    /// When a live stream will count as stale if nothing more arrives.
    fn stale_at(&self) -> Option<Instant> {
        match (self.stream, self.stream_seen) {
            (StreamState::Live, Some(seen)) => Some(seen + STREAM_STALE_AFTER),
            _ => None,
        }
    }

    /// Advance time: reconnect when a wait is over, and drop a live stream
    /// that has gone silent (a half-open connection never reports a close).
    pub fn tick(&mut self, now: Instant) -> Vec<Command> {
        let mut commands = self.terminal.tick(now);
        commands.extend(self.tick_stream(now));
        commands
    }

    fn tick_stream(&mut self, now: Instant) -> Vec<Command> {
        if self.stale_at().is_some_and(|at| now >= at) {
            // Retire this generation so a late chunk from it is ignored.
            self.generation += 1;
            self.wait_to_reconnect(now);
            return vec![Command::CloseEvents];
        }
        match self.stream {
            StreamState::Waiting { retry_at, .. } if now >= retry_at && self.auth.is_some() => self.connect(),
            _ => Vec::new(),
        }
    }

    /// How long until [`Model::tick`] has something to do, if anything.
    pub fn next_wake(&self, now: Instant) -> Option<Duration> {
        let stream = match self.stream {
            StreamState::Waiting { retry_at, .. } => Some(retry_at.saturating_duration_since(now)),
            _ => self.stale_at().map(|at| at.saturating_duration_since(now)),
        };
        [stream, self.terminal.next_wake(now)].into_iter().flatten().min()
    }

    fn on_event(&mut self, event: ServerEvent) -> Vec<Command> {
        match event {
            ServerEvent::SessionCreated { id, name } => {
                if !self.sessions.iter().any(|s| s.id == id) {
                    self.sessions.push(Session {
                        id,
                        name,
                        activity: "starting".into(),
                        ..Session::default()
                    });
                    sort_sessions(&mut self.sessions);
                }
                // The event is thin; read the full row.
                self.refresh_sessions()
            }
            ServerEvent::SessionRenamed { id, name } => {
                if let Some(s) = self.sessions.iter_mut().find(|s| s.id == id) {
                    s.name = name;
                    sort_sessions(&mut self.sessions);
                }
                Vec::new()
            }
            ServerEvent::SessionKilled { id, exit_code } => {
                if let Some(s) = self.sessions.iter_mut().find(|s| s.id == id) {
                    s.activity = "exited".into();
                    s.exit_code = exit_code;
                }
                self.terminal.on_activity(&id)
            }
            ServerEvent::Activity {
                id,
                state,
                activity_changed_at,
            } => {
                if let Some(s) = self.sessions.iter_mut().find(|s| s.id == id) {
                    s.activity = state;
                    if activity_changed_at.is_some() {
                        s.activity_changed_at = activity_changed_at;
                    }
                }
                self.terminal.on_activity(&id)
            }
            ServerEvent::Lagged { .. } => self.refresh_sessions(),
            ServerEvent::VogtChanged { entity_id, .. } => match &self.work {
                WorkView::Loaded(detail) if detail.item.id == entity_id || detail.item.reference == entity_id => {
                    vec![Command::FetchWorkItem(detail.item.reference.clone())]
                }
                _ => Vec::new(),
            },
            ServerEvent::Other => Vec::new(),
        }
    }
}

/// Live sessions first, then by name.
fn sort_sessions(sessions: &mut [Session]) {
    sessions.sort_by(|a, b| (a.activity == "exited", &a.name).cmp(&(b.activity == "exited", &b.name)));
}

/// Send each command to the backend.
pub fn run(commands: Vec<Command>, model: &Model, backend: &mut dyn Backend) {
    for command in commands {
        match command {
            Command::Login {
                base,
                username,
                password,
            } => backend.login(&base, &username, &password),
            Command::FetchSessions => {
                if let Some(auth) = &model.auth {
                    backend.fetch_sessions(auth);
                }
            }
            Command::FetchWorkItem(reference) => {
                if let Some(auth) = &model.auth {
                    backend.fetch_work_item(auth, &reference);
                }
            }
            Command::OpenEvents(generation) => {
                if let Some(auth) = &model.auth {
                    backend.open_events(auth, generation);
                }
            }
            Command::CloseEvents => backend.close_events(),
            Command::FetchScreen(id) => {
                if let Some(auth) = &model.auth {
                    backend.fetch_screen(auth, &id);
                }
            }
            Command::Attach {
                id,
                generation,
                resume_from,
            } => {
                if let Some(auth) = &model.auth {
                    backend.attach(auth, &id, generation, resume_from);
                }
            }
            Command::AttachPing(id) => backend.attach_ping(id),
            Command::Detach => backend.detach(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::FixtureBackend;
    use crate::backend::session;

    /// Pump replies until the backend is quiet, like frames would.
    fn settle(model: &mut Model, backend: &mut FixtureBackend, now: Instant) {
        for _ in 0..10 {
            let replies = backend.poll();
            if replies.is_empty() {
                return;
            }
            for reply in replies {
                let commands = model.apply(reply, now);
                run(commands, model, backend);
            }
        }
    }

    fn signed_in() -> (Model, FixtureBackend, Instant) {
        let mut model = Model::new("http://dev/");
        let mut backend = FixtureBackend::demo();
        let now = Instant::now();
        model.login.username = "demo".into();
        model.login.password = "demo".into();
        let commands = model.submit_password();
        run(commands, &model, &mut backend);
        settle(&mut model, &mut backend, now);
        (model, backend, now)
    }

    #[test]
    fn login_loads_sessions_and_goes_live() {
        let (model, backend, _) = signed_in();
        assert_eq!(model.screen, Screen::Sessions);
        assert_eq!(model.auth.as_ref().map(|a| a.base.as_str()), Some("http://dev"));
        assert_eq!(model.stream, StreamState::Live);
        assert_eq!(model.sessions.len(), 2);
        assert!(model.login.password.is_empty());
        assert_eq!(backend.calls[..3], ["login demo", "sessions", "events 1"]);
    }

    #[test]
    fn wrong_password_stays_on_login() {
        let mut model = Model::new("http://dev");
        let mut backend = FixtureBackend::demo();
        model.login.username = "demo".into();
        model.login.password = "nope".into();
        let commands = model.submit_password();
        run(commands, &model, &mut backend);
        settle(&mut model, &mut backend, Instant::now());
        assert_eq!(model.screen, Screen::Login);
        assert_eq!(model.login.error.as_deref(), Some("Wrong username or password."));
        assert!(!model.login.busy);
    }

    #[test]
    fn events_update_the_list_in_place() {
        let (mut model, mut backend, now) = signed_in();
        backend.emit(ServerEvent::Activity {
            id: "s1".into(),
            state: "awaiting-approval".into(),
            activity_changed_at: None,
        });
        backend.emit(ServerEvent::SessionRenamed {
            id: "s2".into(),
            name: "alpha".into(),
        });
        backend.emit(ServerEvent::SessionKilled {
            id: "s1".into(),
            exit_code: Some(0),
        });
        settle(&mut model, &mut backend, now);
        let names: Vec<_> = model
            .sessions
            .iter()
            .map(|s| (s.name.as_str(), s.activity.as_str()))
            .collect();
        assert_eq!(names, vec![("alpha", "idle"), ("oversight", "exited")]);
    }

    #[test]
    fn created_session_appears_and_is_reread() {
        let (mut model, mut backend, now) = signed_in();
        backend.sessions.push(session("s3", "new", "running"));
        let before = backend.calls.len();
        backend.emit(ServerEvent::SessionCreated {
            id: "s3".into(),
            name: "new".into(),
        });
        settle(&mut model, &mut backend, now);
        assert_eq!(backend.calls[before..], ["sessions"]);
        assert!(model.sessions.iter().any(|s| s.id == "s3" && s.activity == "running"));
    }

    #[test]
    fn dropped_stream_backs_off_reconnects_and_reconciles() {
        let (mut model, mut backend, now) = signed_in();
        backend.sessions.push(session("s9", "missed", "running"));
        backend.drop_stream();
        settle(&mut model, &mut backend, now);
        let StreamState::Waiting { retry_at, attempt } = model.stream else {
            panic!("not waiting: {:?}", model.stream)
        };
        assert_eq!(attempt, 1);
        assert_eq!(model.next_wake(now), Some(Duration::from_millis(500)));
        // Too early: nothing happens.
        assert!(model.tick(now).is_empty());
        let commands = model.tick(retry_at);
        assert_eq!(commands, vec![Command::OpenEvents(2)]);
        run(commands, &model, &mut backend);
        settle(&mut model, &mut backend, retry_at);
        assert_eq!(model.stream, StreamState::Live);
        // Reconnect re-read the list, so the session created during the gap shows.
        assert!(model.sessions.iter().any(|s| s.id == "s9"));
    }

    #[test]
    fn repeated_failures_grow_the_delay() {
        let (mut model, mut backend, mut now) = signed_in();
        backend.refuse_stream = Some(ApiError::transport("connection refused"));
        backend.drop_stream();
        let mut delays = Vec::new();
        for _ in 0..4 {
            settle(&mut model, &mut backend, now);
            let wait = model.next_wake(now).unwrap();
            delays.push(wait.as_millis());
            now += wait;
            let commands = model.tick(now);
            run(commands, &model, &mut backend);
        }
        assert_eq!(delays, vec![500, 1000, 2000, 4000]);
    }

    #[test]
    fn silent_stream_is_dropped_and_reopened() {
        let (mut model, mut backend, now) = signed_in();
        let old = model.generation();
        assert_eq!(model.next_wake(now), Some(STREAM_STALE_AFTER));
        // A keep-alive pushes the deadline out.
        let later = now + Duration::from_secs(30);
        backend.keep_alive();
        settle(&mut model, &mut backend, later);
        assert!(model.tick(now + STREAM_STALE_AFTER).is_empty());
        assert_eq!(model.stream, StreamState::Live);
        // Then silence past the deadline.
        let dead = later + STREAM_STALE_AFTER;
        assert_eq!(model.tick(dead), vec![Command::CloseEvents]);
        assert!(matches!(model.stream, StreamState::Waiting { attempt: 1, .. }));
        assert!(model.generation() > old);
        // A late chunk from the retired stream changes nothing.
        assert!(model.apply(Reply::StreamAlive(old), dead).is_empty());
        let retry = dead + model.next_wake(dead).unwrap();
        let commands = model.tick(retry);
        assert_eq!(commands, vec![Command::OpenEvents(model.generation())]);
    }

    #[test]
    fn stale_stream_replies_are_ignored() {
        let (mut model, _backend, now) = signed_in();
        let live = model.generation();
        let commands = model.apply(Reply::StreamClosed(live - 1, ApiError::transport("old")), now);
        assert!(commands.is_empty());
        assert_eq!(model.stream, StreamState::Live);
    }

    #[test]
    fn unauthorised_anything_signs_out() {
        let (mut model, _backend, now) = signed_in();
        let commands = model.apply(
            Reply::Sessions(Err(ApiError {
                status: Some(401),
                message: "no".into(),
            })),
            now,
        );
        assert_eq!(commands, vec![Command::CloseEvents]);
        assert_eq!(model.screen, Screen::Login);
        assert!(model.auth.is_none());
        assert!(model.login.error.is_some());
    }

    #[test]
    fn bad_pasted_token_returns_to_login() {
        let mut model = Model::new("http://dev");
        let mut backend = FixtureBackend::demo();
        model.login.token = "wrong".into();
        let commands = model.submit_token();
        run(commands, &model, &mut backend);
        settle(&mut model, &mut backend, Instant::now());
        assert_eq!(model.screen, Screen::Login);
        assert!(model.auth.is_none());
    }

    #[test]
    fn work_item_loads_fails_and_follows_changes() {
        let (mut model, mut backend, now) = signed_in();
        let commands = model.open_work_item(" WI-1806 ");
        run(commands, &model, &mut backend);
        settle(&mut model, &mut backend, now);
        let WorkView::Loaded(detail) = &model.work else {
            panic!("{:?}", model.work)
        };
        assert_eq!(detail.item.title, "Vogt-on-fluent2 POC");

        let before = backend.calls.len();
        backend.emit(ServerEvent::VogtChanged {
            kind: "work.updated".into(),
            entity_kind: "work_item".into(),
            entity_id: "wrk_demo".into(),
        });
        settle(&mut model, &mut backend, now);
        assert_eq!(backend.calls[before..], ["work WI-1806"]);

        let commands = model.open_work_item("WI-404");
        run(commands, &model, &mut backend);
        settle(&mut model, &mut backend, now);
        assert!(matches!(&model.work, WorkView::Failed(r, _) if r == "WI-404"));
    }

    #[test]
    fn terminal_follows_attach_activity_and_leaves_cleanly() {
        let (mut model, mut backend, now) = signed_in();
        let commands = model.open_terminal("s1");
        run(commands, &model, &mut backend);
        settle(&mut model, &mut backend, now);
        assert_eq!(model.screen, Screen::Terminal);
        assert_eq!(model.terminal.attach, crate::terminal::AttachState::Live);
        assert!(model.terminal.grid.visible_text().starts_with("$ cargo test\nok"));

        backend.pty_output("s1", b"done\r\n");
        settle(&mut model, &mut backend, now);
        assert!(model.terminal.grid.visible_text().contains("done"));

        let before = backend.calls.len();
        backend.emit(ServerEvent::Activity {
            id: "s1".into(),
            state: "idle".into(),
            activity_changed_at: None,
        });
        settle(&mut model, &mut backend, now);
        assert_eq!(backend.calls[before..], ["screen s1"]);

        // The attach drops; the reconnect resumes and gets only the gap.
        backend.drop_attach();
        backend.pty_output("s1", b"gap\r\n");
        settle(&mut model, &mut backend, now);
        let retry = now + model.next_wake(now).unwrap();
        let commands = model.tick(retry);
        run(commands, &model, &mut backend);
        settle(&mut model, &mut backend, retry);
        assert_eq!(backend.calls.last().map(String::as_str), Some("attach s1 2 from 24"));
        let text = model.terminal.grid.visible_text();
        assert!(text.contains("done") && text.contains("gap"));

        let commands = model.show_sessions();
        assert_eq!(commands, vec![Command::Detach]);
        assert!(model.terminal.session_id.is_none());
    }

    #[test]
    fn late_work_item_reply_for_an_old_ref_is_dropped() {
        let (mut model, _backend, now) = signed_in();
        let _ = model.open_work_item("WI-2");
        model.apply(Reply::WorkItem("WI-1".into(), Ok(Box::default())), now);
        assert_eq!(model.work, WorkView::Loading("WI-2".into()));
    }
}
