//! An in-memory backend for tests and offline demos.

use std::collections::{BTreeMap, VecDeque};

use super::{Backend, DiffRequest, Reply};
use crate::api::{ApiError, Auth, Comment, GitDiff, GitEntry, GitStatus, ServerEvent, Session, WorkDetail, WorkItem};

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
    /// The workspace root `/api/status` reports.
    pub workspace_root: String,
    /// Git status served, by `repo`; any other repo is not a repository.
    pub git: BTreeMap<String, GitStatus>,
    /// File texts served as diffs, by `(repo, path, staged)`: `(head, current)`.
    pub diffs: BTreeMap<(String, String, bool), (String, String)>,
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
        let entry = |path: &str, index: &str, worktree: &str, kind: &str| GitEntry {
            path: path.into(),
            index: index.into(),
            worktree: worktree.into(),
            kind: kind.into(),
        };
        let mut git = BTreeMap::new();
        git.insert(
            "WI-1806".to_owned(),
            GitStatus {
                repo: "WI-1806".into(),
                is_repo: true,
                branch: "wi-1806-gui".into(),
                ahead: 2,
                behind: 0,
                entries: vec![
                    entry("src/app.rs", " ", "M", "modified"),
                    entry("README.md", "M", " ", "staged"),
                    entry("notes.txt", "?", "?", "untracked"),
                ],
            },
        );
        let mut diffs = BTreeMap::new();
        let mut diff = |path: &str, staged: bool, head: &str, current: &str| {
            diffs.insert(
                ("WI-1806".to_owned(), path.to_owned(), staged),
                (head.to_owned(), current.to_owned()),
            );
        };
        diff(
            "src/app.rs",
            false,
            "fn main() {\n    println!(\"hello\");\n}\n",
            "fn main() {\n    println!(\"hello, Vogt\");\n    run();\n}\n",
        );
        diff("README.md", true, "# Vogt\n", "# Vogt\n\nThe egui GUI.\n");
        diff("notes.txt", false, "", "todo\n");
        Self {
            credentials: ("demo".into(), "demo".into()),
            workspace_root: "/work".into(),
            git,
            diffs,
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

    fn fetch_workspace_root(&mut self, auth: &Auth) {
        self.calls.push("status".into());
        let reply = self.authorised(auth).map(|()| self.workspace_root.clone());
        self.queue.push_back(Reply::WorkspaceRoot(reply));
    }

    fn fetch_git_status(&mut self, auth: &Auth, repo: &str) {
        self.calls.push(format!("git status {repo}"));
        let reply = self.authorised(auth).map(|()| {
            self.git.get(repo).cloned().unwrap_or_else(|| GitStatus {
                repo: repo.to_owned(),
                is_repo: false,
                ..GitStatus::default()
            })
        });
        self.queue.push_back(Reply::GitStatus(repo.to_owned(), reply));
    }

    fn fetch_git_diff(&mut self, auth: &Auth, request: &DiffRequest) {
        self.calls.push(format!("git diff {} {}", request.repo, request.path));
        let key = (request.repo.clone(), request.path.clone(), request.staged);
        let reply = self.authorised(auth).and_then(|()| {
            self.diffs
                .get(&key)
                .map(|(head, current)| GitDiff {
                    path: request.path.clone(),
                    head: head.clone(),
                    current: current.clone(),
                })
                .ok_or_else(|| ApiError {
                    status: Some(404),
                    message: format!("{} has no diff", request.path),
                })
        });
        self.queue.push_back(Reply::GitDiff(request.clone(), reply));
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
    fn fetch_workspace_root(&mut self, auth: &Auth) {
        self.borrow_mut().fetch_workspace_root(auth);
    }
    fn fetch_git_status(&mut self, auth: &Auth, repo: &str) {
        self.borrow_mut().fetch_git_status(auth, repo);
    }
    fn fetch_git_diff(&mut self, auth: &Auth, request: &DiffRequest) {
        self.borrow_mut().fetch_git_diff(auth, request);
    }
    fn open_events(&mut self, auth: &Auth, generation: u64) {
        self.borrow_mut().open_events(auth, generation);
    }
    fn close_events(&mut self) {
        self.borrow_mut().close_events();
    }
    fn poll(&mut self) -> Vec<Reply> {
        self.borrow_mut().poll()
    }
}
