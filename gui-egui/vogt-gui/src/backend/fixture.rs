//! An in-memory backend for tests and offline demos.

use std::collections::{BTreeMap, VecDeque};

use super::{Backend, Reply};
use crate::api::{ApiError, Auth, Comment, ServerEvent, Session, WorkDetail, WorkItem};

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
        Self {
            credentials: ("demo".into(), "demo".into()),
            token: "fixture-token".into(),
            sessions: vec![session("s1", "oversight", "running"), session("s2", "WI-1806", "idle")],
            work_items,
            ..Self::default()
        }
    }

    /// Push an event onto the open stream. Does nothing when no stream is open.
    pub fn emit(&mut self, event: ServerEvent) {
        if let Some(generation) = self.stream {
            self.queue.push_back(Reply::StreamEvent(generation, event));
        }
    }

    /// End the open stream as a dropped connection would.
    pub fn drop_stream(&mut self) {
        if let Some(generation) = self.stream.take() {
            self.queue.push_back(Reply::StreamClosed(generation, ApiError::transport("the event stream ended")));
        }
    }

    fn authorised(&self, auth: &Auth) -> Result<(), ApiError> {
        if auth.token == self.token {
            Ok(())
        } else {
            Err(ApiError { status: Some(401), message: "unknown token".into() })
        }
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
            Reply::LoggedIn { base: base.to_owned(), token: self.token.clone(), actor: username.to_owned() }
        } else {
            Reply::LoginFailed(ApiError { status: Some(401), message: "wrong username or password".into() })
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
            self.work_items
                .get(reference)
                .cloned()
                .ok_or_else(|| ApiError { status: Some(404), message: format!("{reference} not found") })
        });
        self.queue.push_back(Reply::WorkItem(reference.to_owned(), reply));
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

    fn poll(&mut self) -> Vec<Reply> {
        self.queue.drain(..).collect()
    }
}
