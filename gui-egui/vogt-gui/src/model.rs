//! App state and the rules that change it, with no egui and no I/O.
//!
//! [`Model::apply`] folds a backend [`Reply`] into the state and [`Model::tick`]
//! advances time; both return the [`Command`]s the app must send to the
//! backend. Reconnect, reconcile-on-reconnect and sign-out-on-401 all live
//! here so they are tested without a window or a network.

use std::time::Duration;

use web_time::Instant;

use crate::api::{ApiError, Auth, GitDiff, GitStatus, ServerEvent, Session, WorkDetail, normalise_base, repo_for_cwd};
use crate::backend::{Backend, DiffRequest, Reply};
use crate::backoff::Backoff;

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
    /// Read the engine's workspace root.
    FetchWorkspaceRoot,
    /// Read a repository's changed files.
    FetchGitStatus(String),
    /// Read one file's diff.
    FetchGitDiff(DiffRequest),
    /// Open the event stream as this generation.
    OpenEvents(u64),
    /// Close the event stream.
    CloseEvents,
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
    /// A session's changed files and their diffs.
    Git,
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

/// The changed-file list on the Git page.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum GitListView {
    /// No session chosen.
    #[default]
    Empty,
    /// Waiting for the workspace root or the status.
    Loading,
    /// The session's directory is outside the workspace, so the engine cannot
    /// read it.
    Outside,
    /// The directory is not in a Git repository.
    NotRepo,
    /// Loaded.
    Loaded(GitStatus),
    /// The read failed.
    Failed(String),
}

/// The diff pane on the Git page.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum DiffPane {
    /// No file selected.
    #[default]
    Empty,
    /// Waiting for this file.
    Loading(DiffRequest),
    /// Loaded.
    Loaded(DiffRequest, GitDiff),
    /// The read failed.
    Failed(DiffRequest, String),
}

impl DiffPane {
    /// The file this pane is for, if any.
    pub fn request(&self) -> Option<&DiffRequest> {
        match self {
            Self::Empty => None,
            Self::Loading(r) | Self::Loaded(r, _) | Self::Failed(r, _) => Some(r),
        }
    }
}

/// The read-only Git page for one session.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GitPage {
    /// The session's name.
    pub session: String,
    /// The session's working directory.
    pub cwd: String,
    /// Its `repo` parameter, once the workspace root is known.
    pub repo: Option<String>,
    /// The changed files.
    pub list: GitListView,
    /// The selected file's diff.
    pub diff: DiffPane,
    /// Show the diff side by side rather than inline.
    pub side_by_side: bool,
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
    /// The Git page.
    pub git: GitPage,
    /// The engine's workspace root, once read.
    pub workspace_root: Option<String>,
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
            git: GitPage {
                side_by_side: true,
                ..GitPage::default()
            },
            workspace_root: None,
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
        self.git = GitPage {
            side_by_side: self.git.side_by_side,
            ..GitPage::default()
        };
        self.workspace_root = None;
        self.stream = StreamState::Off;
        self.screen = Screen::Login;
        self.login.busy = false;
        self.login.error = why;
        self.generation += 1;
        vec![Command::CloseEvents]
    }

    /// Show a screen. Opening the work-item page with a reference loads it.
    pub fn open_work_item(&mut self, reference: &str) -> Vec<Command> {
        let reference = reference.trim().to_owned();
        self.screen = Screen::WorkItem;
        if reference.is_empty() {
            return Vec::new();
        }
        self.work_ref = reference.clone();
        self.work = WorkView::Loading(reference.clone());
        vec![Command::FetchWorkItem(reference)]
    }

    /// Open the Git page for session `id`. The engine reads repositories by
    /// their path under its workspace root, so the root is read first if it
    /// is not known yet.
    pub fn open_git(&mut self, id: &str) -> Vec<Command> {
        let Some(session) = self.sessions.iter().find(|s| s.id == id) else {
            return Vec::new();
        };
        self.screen = Screen::Git;
        self.git = GitPage {
            session: session.name.clone(),
            cwd: session.cwd.clone(),
            side_by_side: self.git.side_by_side,
            list: GitListView::Loading,
            ..GitPage::default()
        };
        match self.workspace_root.clone() {
            Some(root) => self.resolve_repo(&root),
            None => vec![Command::FetchWorkspaceRoot],
        }
    }

    /// Re-read the Git page's file list and the open diff.
    pub fn refresh_git(&mut self) -> Vec<Command> {
        let Some(repo) = self.git.repo.clone() else {
            return Vec::new();
        };
        self.git.list = GitListView::Loading;
        let mut commands = vec![Command::FetchGitStatus(repo)];
        if let Some(request) = self.git.diff.request().cloned() {
            self.git.diff = DiffPane::Loading(request.clone());
            commands.push(Command::FetchGitDiff(request));
        }
        commands
    }

    /// Show the diff of the changed file at `path`.
    pub fn select_file(&mut self, path: &str) -> Vec<Command> {
        let (Some(repo), GitListView::Loaded(status)) = (&self.git.repo, &self.git.list) else {
            return Vec::new();
        };
        let Some(entry) = status.entries.iter().find(|e| e.path == path) else {
            return Vec::new();
        };
        let request = DiffRequest {
            repo: repo.clone(),
            path: entry.path.clone(),
            staged: entry.is_staged(),
        };
        if self.git.diff.request() == Some(&request) && !matches!(self.git.diff, DiffPane::Failed(..)) {
            return Vec::new();
        }
        self.git.diff = DiffPane::Loading(request.clone());
        vec![Command::FetchGitDiff(request)]
    }

    fn resolve_repo(&mut self, root: &str) -> Vec<Command> {
        match repo_for_cwd(root, &self.git.cwd) {
            Some(repo) => {
                self.git.repo = Some(repo.clone());
                vec![Command::FetchGitStatus(repo)]
            }
            None => {
                self.git.list = GitListView::Outside;
                Vec::new()
            }
        }
    }

    /// Show the session list.
    pub fn show_sessions(&mut self) {
        if self.auth.is_some() {
            self.screen = Screen::Sessions;
        }
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
            Reply::WorkspaceRoot(result) => match result {
                Ok(root) => {
                    self.workspace_root = Some(root.clone());
                    if self.git.list == GitListView::Loading && self.git.repo.is_none() {
                        self.resolve_repo(&root)
                    } else {
                        Vec::new()
                    }
                }
                Err(error) => self.refused(&error).unwrap_or_else(|| {
                    if self.git.repo.is_none() && self.git.list == GitListView::Loading {
                        self.git.list = GitListView::Failed(format!("Could not read the workspace root: {error}"));
                    }
                    Vec::new()
                }),
            },
            Reply::GitStatus(repo, result) => {
                if self.git.repo.as_deref() != Some(repo.as_str()) {
                    return Vec::new(); // another session's page
                }
                match result {
                    Ok(status) if !status.is_repo => {
                        self.git.list = GitListView::NotRepo;
                        self.git.diff = DiffPane::Empty;
                        Vec::new()
                    }
                    Ok(status) => {
                        // Drop the open diff if its file is no longer changed.
                        if let Some(request) = self.git.diff.request()
                            && !status.entries.iter().any(|e| e.path == request.path)
                        {
                            self.git.diff = DiffPane::Empty;
                        }
                        self.git.list = GitListView::Loaded(status);
                        Vec::new()
                    }
                    Err(error) => self.refused(&error).unwrap_or_else(|| {
                        self.git.list = GitListView::Failed(error.to_string());
                        Vec::new()
                    }),
                }
            }
            Reply::GitDiff(request, result) => {
                if self.git.diff != DiffPane::Loading(request.clone()) {
                    return Vec::new(); // the user has moved on
                }
                match result {
                    Ok(diff) => {
                        self.git.diff = DiffPane::Loaded(request, diff);
                        Vec::new()
                    }
                    Err(error) => self.refused(&error).unwrap_or_else(|| {
                        self.git.diff = DiffPane::Failed(request, error.to_string());
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
        match self.stream {
            StreamState::Waiting { retry_at, .. } => Some(retry_at.saturating_duration_since(now)),
            _ => self.stale_at().map(|at| at.saturating_duration_since(now)),
        }
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
                Vec::new()
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
                Vec::new()
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
            Command::FetchWorkspaceRoot => {
                if let Some(auth) = &model.auth {
                    backend.fetch_workspace_root(auth);
                }
            }
            Command::FetchGitStatus(repo) => {
                if let Some(auth) = &model.auth {
                    backend.fetch_git_status(auth, &repo);
                }
            }
            Command::FetchGitDiff(request) => {
                if let Some(auth) = &model.auth {
                    backend.fetch_git_diff(auth, &request);
                }
            }
            Command::OpenEvents(generation) => {
                if let Some(auth) = &model.auth {
                    backend.open_events(auth, generation);
                }
            }
            Command::CloseEvents => backend.close_events(),
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
    fn late_work_item_reply_for_an_old_ref_is_dropped() {
        let (mut model, _backend, now) = signed_in();
        let _ = model.open_work_item("WI-2");
        model.apply(Reply::WorkItem("WI-1".into(), Ok(Box::default())), now);
        assert_eq!(model.work, WorkView::Loading("WI-2".into()));
    }

    fn open_git(model: &mut Model, backend: &mut FixtureBackend, now: Instant, id: &str) {
        let commands = model.open_git(id);
        run(commands, model, backend);
        settle(model, backend, now);
    }

    #[test]
    fn git_page_lists_changed_files_of_the_session() {
        let (mut model, mut backend, now) = signed_in();
        let before = backend.calls.len();
        open_git(&mut model, &mut backend, now, "s2");
        assert_eq!(model.screen, Screen::Git);
        assert_eq!(backend.calls[before..], ["status", "git status WI-1806"]);
        assert_eq!(model.git.repo.as_deref(), Some("WI-1806"));
        let GitListView::Loaded(status) = &model.git.list else {
            panic!("{:?}", model.git.list)
        };
        let paths: Vec<_> = status.entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, ["src/app.rs", "README.md", "notes.txt"]);

        // The root is read once.
        let before = backend.calls.len();
        open_git(&mut model, &mut backend, now, "s1");
        assert_eq!(backend.calls[before..], ["git status oversight"]);
        assert_eq!(model.git.list, GitListView::NotRepo);
    }

    #[test]
    fn selecting_a_file_loads_its_diff_staged_or_not() {
        let (mut model, mut backend, now) = signed_in();
        open_git(&mut model, &mut backend, now, "s2");
        let commands = model.select_file("README.md");
        assert_eq!(
            commands,
            vec![Command::FetchGitDiff(DiffRequest {
                repo: "WI-1806".into(),
                path: "README.md".into(),
                staged: true,
            })]
        );
        run(commands, &model, &mut backend);
        settle(&mut model, &mut backend, now);
        let DiffPane::Loaded(request, diff) = &model.git.diff else {
            panic!("{:?}", model.git.diff)
        };
        assert!(request.staged);
        assert_eq!(diff.head, "# Vogt\n");
        // Selecting it again does not re-read.
        assert!(model.select_file("README.md").is_empty());
        // Unknown paths are ignored.
        assert!(model.select_file("nope").is_empty());

        let commands = model.select_file("src/app.rs");
        run(commands, &model, &mut backend);
        settle(&mut model, &mut backend, now);
        assert!(matches!(&model.git.diff, DiffPane::Loaded(r, d) if !r.staged && d.current.contains("Vogt")));
    }

    #[test]
    fn late_diff_for_another_file_is_dropped() {
        let (mut model, mut backend, now) = signed_in();
        open_git(&mut model, &mut backend, now, "s2");
        let _ = model.select_file("src/app.rs");
        let stale = DiffRequest {
            repo: "WI-1806".into(),
            path: "README.md".into(),
            staged: true,
        };
        model.apply(Reply::GitDiff(stale, Ok(GitDiff::default())), now);
        assert!(matches!(&model.git.diff, DiffPane::Loading(r) if r.path == "src/app.rs"));
    }

    #[test]
    fn refresh_drops_a_diff_whose_file_is_clean_now() {
        let (mut model, mut backend, now) = signed_in();
        open_git(&mut model, &mut backend, now, "s2");
        let commands = model.select_file("notes.txt");
        run(commands, &model, &mut backend);
        settle(&mut model, &mut backend, now);
        if let Some(status) = backend.git.get_mut("WI-1806") {
            status.entries.retain(|e| e.path != "notes.txt");
        }
        backend.diffs.clear();
        let commands = model.refresh_git();
        run(commands, &model, &mut backend);
        settle(&mut model, &mut backend, now);
        assert_eq!(model.git.diff, DiffPane::Empty);
        assert!(matches!(&model.git.list, GitListView::Loaded(s) if s.entries.len() == 2));
    }

    #[test]
    fn session_outside_the_workspace_is_said_so() {
        let (mut model, mut backend, now) = signed_in();
        backend.workspace_root = "/elsewhere".into();
        open_git(&mut model, &mut backend, now, "s2");
        assert_eq!(model.git.list, GitListView::Outside);
        assert!(model.git.repo.is_none());
    }

    #[test]
    fn git_401_signs_out() {
        let (mut model, mut backend, now) = signed_in();
        open_git(&mut model, &mut backend, now, "s2");
        let commands = model.apply(
            Reply::GitStatus(
                "WI-1806".into(),
                Err(ApiError {
                    status: Some(401),
                    message: "no".into(),
                }),
            ),
            now,
        );
        assert_eq!(commands, vec![Command::CloseEvents]);
        assert_eq!(model.screen, Screen::Login);
        assert_eq!(model.git.list, GitListView::Empty);
        assert!(model.workspace_root.is_none());
    }
}
