//! An in-memory backend for tests and offline demos.

use std::collections::{BTreeMap, VecDeque};

use super::{AttachEvent, Backend, Reply};
use crate::api::{
    ApiError, AttachControl, Auth, Comment, ScreenCursor, ServerEvent, Session, SessionScreen, WorkDetail, WorkItem,
};

/// Answers every call from memory and records what it was asked, so a test can
/// drive the app frame by frame and check both the screen and the traffic.
#[derive(Debug, Default)]
pub struct FixtureBackend {
    /// Accepted `(username, password)`; anything else fails with 401.
    pub credentials: (String, String),
    /// The bearer a good login returns, and the only one other calls accept.
    pub token: String,
    /// The session list served.
    pub sessions: Vec<Session>,
    /// Work items served, by reference.
    pub work_items: BTreeMap<String, WorkDetail>,
    /// Every call, in order, as a short string (`"sessions"`, `"work WI-7"`...).
    pub calls: Vec<String>,
    /// The open stream generation, if any.
    pub stream: Option<u64>,
    /// When set, `open_events` fails with this error instead of opening.
    pub refuse_stream: Option<ApiError>,
    /// Rendered screens served, by session id.
    pub screens: BTreeMap<String, SessionScreen>,
    /// Each session's whole PTY output, by session id. An attach snapshot is
    /// cut from it, as the engine cuts one from its scrollback ring.
    pub pty: BTreeMap<String, Vec<u8>>,
    /// The open attach as `(generation, session id)`, if any.
    pub attached: Option<(u64, String)>,
    queue: VecDeque<Reply>,
}

impl FixtureBackend {
    /// A fixture with a demo user, two sessions and one work item.
    pub fn demo() -> Self {
        let mut work_items = BTreeMap::new();
        work_items.insert(
            "WI-1806".to_owned(),
            WorkDetail {
                item: WorkItem {
                    id: "wrk_demo".into(),
                    reference: "WI-1806".into(),
                    kind: "feature".into(),
                    title: "Vogt-on-fluent2 POC".into(),
                    body: "Umbrella for the **egui** port.\n\n- Sessions\n- Work-item detail\n\n\
                           | gap | size |\n|---|---|\n| G23 | M |\n"
                        .into(),
                    state: "open".into(),
                    priority: "p2".into(),
                    effort: Some("xl".into()),
                    project_slug: Some("vogt".into()),
                    labels: vec!["gui".into()],
                    updated_at: "2026-10-20T04:00:00Z".into(),
                },
                comments: vec![Comment {
                    id: "c1".into(),
                    body: "Split into *child items*.".into(),
                    created_at: "2026-10-20T04:10:00Z".into(),
                }],
            },
        );
        let mut screens = BTreeMap::new();
        screens.insert("s1".to_owned(), screen("s1", &["$ cargo test", "ok"], "running"));
        screens.insert("s2".to_owned(), screen("s2", &["idle prompt", "> "], "idle"));
        let mut pty = BTreeMap::new();
        pty.insert("s1".to_owned(), b"$ cargo test\r\nok\r\n".to_vec());
        pty.insert("s2".to_owned(), b"idle prompt\r\n> ".to_vec());
        Self {
            credentials: ("demo".into(), "demo".into()),
            token: "fixture-token".into(),
            sessions: vec![session("s1", "oversight", "running"), session("s2", "WI-1806", "idle")],
            work_items,
            screens,
            pty,
            ..Self::default()
        }
    }

    /// The session `id` wrote `bytes`: record them, and stream them to an
    /// attach on that session.
    pub fn pty_output(&mut self, id: &str, bytes: &[u8]) {
        self.pty.entry(id.to_owned()).or_default().extend_from_slice(bytes);
        if let Some((generation, _)) = self.attached.as_ref().filter(|(_, s)| s == id) {
            self.queue
                .push_back(Reply::Attach(*generation, AttachEvent::Bytes(bytes.to_vec())));
        }
    }

    /// Send a control frame on the open attach.
    pub fn attach_control(&mut self, control: AttachControl) {
        if let Some((generation, _)) = &self.attached {
            self.queue
                .push_back(Reply::Attach(*generation, AttachEvent::Control(control)));
        }
    }

    /// End the open attach as a dropped connection would.
    pub fn drop_attach(&mut self) {
        if let Some((generation, _)) = self.attached.take() {
            self.queue.push_back(Reply::Attach(
                generation,
                AttachEvent::Closed("the connection was reset".into()),
            ));
        }
    }

    /// Push an event onto the open stream. Does nothing when no stream is open.
    pub fn emit(&mut self, event: ServerEvent) {
        if let Some(generation) = self.stream {
            self.queue.push_back(Reply::StreamEvent(generation, event));
        }
    }

    /// Send a keep-alive on the open stream. Does nothing when no stream is open.
    pub fn keep_alive(&mut self) {
        if let Some(generation) = self.stream {
            self.queue.push_back(Reply::StreamAlive(generation));
        }
    }

    /// End the open stream as a dropped connection would.
    pub fn drop_stream(&mut self) {
        if let Some(generation) = self.stream.take() {
            self.queue.push_back(Reply::StreamClosed(
                generation,
                ApiError::transport("the event stream ended"),
            ));
        }
    }

    fn authorised(&self, auth: &Auth) -> Result<(), ApiError> {
        if auth.token == self.token {
            Ok(())
        } else {
            Err(ApiError {
                status: Some(401),
                message: "unknown token".into(),
            })
        }
    }
}

/// A rendered screen for fixtures, 40 columns by 6 rows, cursor after the
/// last line.
pub fn screen(id: &str, lines: &[&str], activity: &str) -> SessionScreen {
    let rows = 6;
    let mut all: Vec<String> = lines.iter().map(|l| (*l).to_owned()).collect();
    all.resize(rows, String::new());
    SessionScreen {
        id: id.into(),
        cols: 40,
        rows: rows as u16,
        lines: all,
        cursor: ScreenCursor {
            row: lines.len().saturating_sub(1) as u16,
            col: lines.last().map_or(0, |l| l.len() as u16),
        },
        title: Some(format!("{id} title")),
        activity: activity.into(),
        alive: true,
    }
}

/// A session row for fixtures.
pub fn session(id: &str, name: &str, activity: &str) -> Session {
    Session {
        id: id.into(),
        name: name.into(),
        activity: activity.into(),
        cwd: format!("/work/{name}"),
        created_at: "2026-10-20T04:00:00Z".into(),
        ..Session::default()
    }
}

impl Backend for FixtureBackend {
    fn login(&mut self, base: &str, username: &str, password: &str) {
        self.calls.push(format!("login {username}"));
        let reply = if (username, password) == (self.credentials.0.as_str(), self.credentials.1.as_str()) {
            Reply::LoggedIn {
                base: base.to_owned(),
                token: self.token.clone(),
                actor: username.to_owned(),
            }
        } else {
            Reply::LoginFailed(ApiError {
                status: Some(401),
                message: "wrong username or password".into(),
            })
        };
        self.queue.push_back(reply);
    }

    fn fetch_sessions(&mut self, auth: &Auth) {
        self.calls.push("sessions".into());
        let reply = self.authorised(auth).map(|()| self.sessions.clone());
        self.queue.push_back(Reply::Sessions(reply));
    }

    fn fetch_work_item(&mut self, auth: &Auth, reference: &str) {
        self.calls.push(format!("work {reference}"));
        let reply = self.authorised(auth).and_then(|()| {
            self.work_items.get(reference).cloned().ok_or_else(|| ApiError {
                status: Some(404),
                message: format!("{reference} not found"),
            })
        });
        self.queue
            .push_back(Reply::WorkItem(reference.to_owned(), reply.map(Box::new)));
    }

    fn open_events(&mut self, auth: &Auth, generation: u64) {
        self.calls.push(format!("events {generation}"));
        if let Err(error) = self.authorised(auth) {
            self.queue.push_back(Reply::StreamClosed(generation, error));
            return;
        }
        if let Some(error) = self.refuse_stream.clone() {
            self.queue.push_back(Reply::StreamClosed(generation, error));
            return;
        }
        self.stream = Some(generation);
        self.queue.push_back(Reply::StreamOpened(generation));
    }

    fn close_events(&mut self) {
        self.calls.push("close events".into());
        self.stream = None;
    }

    fn fetch_screen(&mut self, auth: &Auth, id: &str) {
        self.calls.push(format!("screen {id}"));
        let reply = self.authorised(auth).and_then(|()| {
            self.screens.get(id).cloned().ok_or_else(|| ApiError {
                status: Some(404),
                message: "no such session".into(),
            })
        });
        self.queue.push_back(Reply::Screen(id.to_owned(), reply.map(Box::new)));
    }

    fn attach(&mut self, auth: &Auth, id: &str, generation: u64, resume_from: Option<u64>) {
        let resume = resume_from.map_or("cold".to_owned(), |p| format!("from {p}"));
        self.calls.push(format!("attach {id} {generation} {resume}"));
        self.attached = None;
        // The engine closes without a reason the client can read; so does this.
        let Some(output) = self.pty.get(id).filter(|_| self.authorised(auth).is_ok()) else {
            self.queue
                .push_back(Reply::Attach(generation, AttachEvent::Closed("closed".into())));
            return;
        };
        let end = output.len() as u64;
        // A warm resume inside the buffer gets the delta; anything else the whole lot.
        let (payload, reset) = match resume_from {
            Some(from) if from <= end => (output[from as usize..].to_vec(), false),
            _ => (output.clone(), true),
        };
        let (cols, rows) = self.screens.get(id).map_or((40, 6), |s| (s.cols, s.rows));
        let events = [
            AttachEvent::Opened,
            AttachEvent::Control(AttachControl::SnapshotStart {
                scrollback_pos: end,
                reset,
                cols: Some(cols),
                rows: Some(rows),
            }),
            AttachEvent::Bytes(payload),
            AttachEvent::Control(AttachControl::SnapshotDone),
        ];
        for event in events {
            self.queue.push_back(Reply::Attach(generation, event));
        }
        self.attached = Some((generation, id.to_owned()));
    }

    fn attach_ping(&mut self, id: u64) {
        self.calls.push(format!("ping {id}"));
        if let Some((generation, session)) = &self.attached {
            let pos = self.pty.get(session).map_or(0, |o| o.len() as u64);
            self.queue.push_back(Reply::Attach(
                *generation,
                AttachEvent::Control(AttachControl::Pong { id, pos }),
            ));
        }
    }

    fn detach(&mut self) {
        self.calls.push("detach".into());
        self.attached = None;
    }

    fn poll(&mut self) -> Vec<Reply> {
        self.queue.drain(..).collect()
    }
}

/// A shared fixture, so a test can hand the app a backend and still reach in
/// to emit events or read the calls.
impl Backend for std::rc::Rc<std::cell::RefCell<FixtureBackend>> {
    fn login(&mut self, base: &str, username: &str, password: &str) {
        self.borrow_mut().login(base, username, password);
    }
    fn fetch_sessions(&mut self, auth: &Auth) {
        self.borrow_mut().fetch_sessions(auth);
    }
    fn fetch_work_item(&mut self, auth: &Auth, reference: &str) {
        self.borrow_mut().fetch_work_item(auth, reference);
    }
    fn open_events(&mut self, auth: &Auth, generation: u64) {
        self.borrow_mut().open_events(auth, generation);
    }
    fn close_events(&mut self) {
        self.borrow_mut().close_events();
    }
    fn fetch_screen(&mut self, auth: &Auth, id: &str) {
        self.borrow_mut().fetch_screen(auth, id);
    }
    fn attach(&mut self, auth: &Auth, id: &str, generation: u64, resume_from: Option<u64>) {
        self.borrow_mut().attach(auth, id, generation, resume_from);
    }
    fn attach_ping(&mut self, id: u64) {
        self.borrow_mut().attach_ping(id);
    }
    fn detach(&mut self) {
        self.borrow_mut().detach();
    }
    fn poll(&mut self) -> Vec<Reply> {
        self.borrow_mut().poll()
    }
}
