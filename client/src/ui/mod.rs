//! GPUI application shell: server connection, session list sidebar, persistent
//! terminal tabs, and lightweight file/git utility panels.

mod terminal_view;

use fluent_app::FluentApp;
use fluent_core::ThemeProvider as _;
use fluent_layout::{MessageBar, MessageIntent};
use fluent_primitives::{Button, ButtonAppearance, Field, Label, LabelSize, TextInput};
use gpui::{
    div, prelude::*, px, ClickEvent, Context, Entity, FontWeight, IntoElement, Render,
    SharedString, StatefulInteractiveElement, Window,
};
use uuid::Uuid;

use futures_util::StreamExt as _;
use mydevenv2_client::{
    bridge,
    client::ApiClient,
    config::ClientConfig,
    protocol::{FileEntry, GitStatus, LogEntry, ServerEvent, SessionSpec, SessionSummary},
};

use terminal_view::TerminalView;

/// Launch the desktop client. Blocks on the GPUI event loop until the window
/// closes.
pub fn run(cfg: ClientConfig) {
    FluentApp::new("MyDevEnv2")
        .window_size(1240.0, 820.0)
        .window_min_size(760.0, 500.0)
        .app_id("com.sprooty.mydevenv2-client")
        .dark_theme()
        .run(move |cx| cx.new(|cx| RootView::new(cfg, cx)));
}

#[derive(Clone)]
enum NativeTab {
    Terminal {
        session_id: Uuid,
        title: SharedString,
        view: Entity<TerminalView>,
    },
    Files,
    Git,
}

impl NativeTab {
    fn id(&self) -> String {
        match self {
            Self::Terminal { session_id, .. } => format!("term:{session_id}"),
            Self::Files => "files".into(),
            Self::Git => "git".into(),
        }
    }

    fn label(&self) -> SharedString {
        match self {
            Self::Terminal { title, .. } => title.clone(),
            Self::Files => "Files".into(),
            Self::Git => "Git".into(),
        }
    }
}

struct FilePanelState {
    path_input: Entity<TextInput>,
    read_input: Entity<TextInput>,
    search_input: Entity<TextInput>,
    entries: Vec<FileEntry>,
    file_preview: Option<(String, String)>,
    search_hits: Vec<(String, u64, String)>,
}

struct GitPanelState {
    repo_input: Entity<TextInput>,
    status: Option<GitStatus>,
    log: Vec<LogEntry>,
}

struct RootView {
    api: ApiClient,
    configured: bool,
    sessions: Vec<SessionSummary>,
    status: SharedString,
    tabs: Vec<NativeTab>,
    active_tab: Option<String>,
    url_input: Entity<TextInput>,
    token_input: Entity<TextInput>,
    new_name_input: Entity<TextInput>,
    new_cwd_input: Entity<TextInput>,
    new_command_input: Entity<TextInput>,
    rename_input: Entity<TextInput>,
    search_input: Entity<TextInput>,
    show_settings: bool,
    show_new_session: bool,
    file_panel: FilePanelState,
    git_panel: GitPanelState,
    /// Bumped each time the SSE stream is (re)started so a stale stream from a
    /// previous server/token stops applying updates.
    events_gen: u64,
}

impl RootView {
    fn new(cfg: ClientConfig, cx: &mut Context<Self>) -> Self {
        let api = ApiClient::new(cfg.base(), cfg.token.clone());
        let configured = cfg.is_configured();
        let status: SharedString = if configured {
            "Connecting...".into()
        } else {
            "Enter the server URL and API token".into()
        };
        let url_input = cx.new(|_| {
            TextInput::new()
                .placeholder("https://mydevenv2.sprooty.com")
                .value(cfg.server_url.clone())
        });
        let token_input = cx.new(|_| {
            TextInput::new()
                .placeholder("API token")
                .masked(true)
                .value(cfg.token.clone())
        });
        let new_name_input = cx.new(|_| TextInput::new().placeholder("terminal").value("terminal"));
        let new_cwd_input = cx.new(|_| TextInput::new().placeholder("/home/sprooty/Working"));
        let new_command_input = cx.new(|_| TextInput::new().placeholder("optional command"));
        let rename_input = cx.new(|_| TextInput::new().placeholder("session name"));
        let search_input = cx.new(|_| TextInput::new().placeholder("search scrollback"));
        let file_panel = FilePanelState {
            path_input: cx.new(|_| TextInput::new().placeholder(".").value(".")),
            read_input: cx.new(|_| TextInput::new().placeholder("path/to/file")),
            search_input: cx.new(|_| TextInput::new().placeholder("search text")),
            entries: Vec::new(),
            file_preview: None,
            search_hits: Vec::new(),
        };
        let git_panel = GitPanelState {
            repo_input: cx.new(|_| TextInput::new().placeholder(".").value(".")),
            status: None,
            log: Vec::new(),
        };
        let mut view = Self {
            api,
            configured,
            sessions: Vec::new(),
            status,
            tabs: Vec::new(),
            active_tab: None,
            url_input,
            token_input,
            new_name_input,
            new_cwd_input,
            new_command_input,
            rename_input,
            search_input,
            show_settings: !configured,
            show_new_session: false,
            file_panel,
            git_panel,
            events_gen: 0,
        };
        if configured {
            view.refresh_sessions(cx);
            view.start_events(cx);
        }
        view
    }

    fn active_terminal_id(&self) -> Option<Uuid> {
        let active = self.active_tab.as_deref()?;
        self.tabs.iter().find_map(|tab| match tab {
            NativeTab::Terminal { session_id, .. } if tab.id() == active => Some(*session_id),
            _ => None,
        })
    }

    fn active_terminal_entity(&self) -> Option<Entity<TerminalView>> {
        let active = self.active_tab.as_deref()?;
        self.tabs.iter().find_map(|tab| match tab {
            NativeTab::Terminal { view, .. } if tab.id() == active => Some(view.clone()),
            _ => None,
        })
    }

    /// Subscribe to the server's `/api/events` SSE stream and apply live
    /// session/activity updates. Reconnects automatically until superseded by a
    /// newer generation (server/token change).
    fn start_events(&mut self, cx: &mut Context<Self>) {
        self.events_gen += 1;
        let generation = self.events_gen;
        let api = self.api.clone();
        let handle = bridge::handle();
        cx.spawn(
            move |weak: gpui::WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                let mut cx = cx.clone();
                async move {
                    loop {
                        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ServerEvent>();
                        let api2 = api.clone();
                        let pump = handle.spawn(async move {
                            if let Ok(stream) = api2.events().await {
                                futures_util::pin_mut!(stream);
                                while let Some(ev) = stream.next().await {
                                    if tx.send(ev).is_err() {
                                        break;
                                    }
                                }
                            }
                        });

                        let mut superseded = false;
                        while let Some(ev) = rx.recv().await {
                            let stop = weak
                                .update(&mut cx, |v, cx| {
                                    if v.events_gen != generation {
                                        return true;
                                    }
                                    v.apply_event(ev, cx);
                                    false
                                })
                                .unwrap_or(true);
                            if stop {
                                superseded = true;
                                break;
                            }
                        }
                        let _ = pump.await;

                        let current = weak
                            .update(&mut cx, |v, _| v.events_gen == generation)
                            .unwrap_or(false);
                        if superseded || !current {
                            break;
                        }
                        cx.background_executor()
                            .timer(std::time::Duration::from_secs(3))
                            .await;
                    }
                }
            },
        )
        .detach();
    }

    fn apply_event(&mut self, ev: ServerEvent, cx: &mut Context<Self>) {
        match ev {
            ServerEvent::Activity { id, state } => {
                if let Some(s) = self.sessions.iter_mut().find(|s| s.id == id) {
                    s.activity = state;
                    cx.notify();
                }
            }
            ServerEvent::SessionRenamed { id, name } => {
                if let Some(s) = self.sessions.iter_mut().find(|s| s.id == id) {
                    s.name = name.clone();
                }
                for tab in &mut self.tabs {
                    if let NativeTab::Terminal {
                        session_id, title, ..
                    } = tab
                    {
                        if *session_id == id {
                            *title = name.clone().into();
                        }
                    }
                }
                cx.notify();
            }
            ServerEvent::SessionCreated { .. } | ServerEvent::SessionKilled { .. } => {
                self.refresh_sessions(cx);
            }
        }
    }

    fn save_settings(&mut self, cx: &mut Context<Self>) {
        let server_url = self.url_input.read(cx).text().trim().to_string();
        let token = self.token_input.read(cx).text().trim().to_string();
        let cfg = ClientConfig { server_url, token };
        if let Err(e) = cfg.save() {
            self.status = SharedString::from(format!("save failed: {e}"));
            cx.notify();
            return;
        }
        for tab in &self.tabs {
            if let NativeTab::Terminal { view, .. } = tab {
                view.update(cx, |v, _| v.close());
            }
        }
        self.api = ApiClient::new(cfg.base(), cfg.token.clone());
        self.configured = cfg.is_configured();
        self.tabs.clear();
        self.active_tab = None;
        self.show_settings = false;
        if self.configured {
            self.status = "Connecting...".into();
            self.refresh_sessions(cx);
            self.start_events(cx);
        } else {
            self.status = "Enter the server URL and API token".into();
            self.show_settings = true;
        }
        cx.notify();
    }

    fn refresh_sessions(&self, cx: &mut Context<Self>) {
        let api = self.api.clone();
        let handle = bridge::handle();
        cx.spawn(
            move |weak: gpui::WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                let mut cx = cx.clone();
                async move {
                    let res = handle.spawn(async move { api.list_sessions().await }).await;
                    let _ = weak.update(&mut cx, |v, cx| {
                        match res {
                            Ok(Ok(list)) => {
                                v.status = SharedString::from(format!("{} session(s)", list.len()));
                                v.sessions = list;
                            }
                            Ok(Err(e)) => {
                                v.status = SharedString::from(format!("list failed: {e}"))
                            }
                            Err(e) => v.status = SharedString::from(format!("task failed: {e}")),
                        }
                        cx.notify();
                    });
                }
            },
        )
        .detach();
    }

    fn create_session(&mut self, cx: &mut Context<Self>) {
        let name = self.new_name_input.read(cx).text().trim().to_string();
        let cwd = non_empty(self.new_cwd_input.read(cx).text());
        let command = shell_command(self.new_command_input.read(cx).text());
        let api = self.api.clone();
        let handle = bridge::handle();
        let spec = SessionSpec {
            name: if name.is_empty() {
                "terminal".into()
            } else {
                name
            },
            command,
            cwd,
            ..Default::default()
        };
        cx.spawn(
            move |weak: gpui::WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                let mut cx = cx.clone();
                async move {
                    let res = handle
                        .spawn(async move { api.create_session(&spec).await })
                        .await;
                    let _ = weak.update(&mut cx, |v, cx| {
                        match res {
                            Ok(Ok(summary)) => {
                                let id = summary.id;
                                v.sessions.push(summary);
                                v.show_new_session = false;
                                v.attach(id, cx);
                            }
                            Ok(Err(e)) => {
                                v.status = SharedString::from(format!("create failed: {e}"))
                            }
                            Err(e) => v.status = SharedString::from(format!("task failed: {e}")),
                        }
                        cx.notify();
                    });
                }
            },
        )
        .detach();
    }

    fn attach(&mut self, id: Uuid, cx: &mut Context<Self>) {
        let tab_id = format!("term:{id}");
        if self.tabs.iter().any(|tab| tab.id() == tab_id) {
            self.active_tab = Some(tab_id);
            self.status = SharedString::from(format!("attached {id}"));
            cx.notify();
            return;
        }
        let label = self
            .sessions
            .iter()
            .find(|s| s.id == id)
            .map(|s| s.name.clone())
            .unwrap_or_else(|| id.to_string());
        let ws_url = self.api.attach_ws_url(id);
        let token = self.api.token().to_string();
        let term = cx.new(|cx| TerminalView::new(ws_url, token, id, cx));
        self.tabs.push(NativeTab::Terminal {
            session_id: id,
            title: label.into(),
            view: term,
        });
        self.active_tab = Some(tab_id);
        self.status = SharedString::from(format!("attached {id}"));
        cx.notify();
    }

    fn close_tab(&mut self, tab_id: &str, cx: &mut Context<Self>) {
        if let Some(idx) = self.tabs.iter().position(|t| t.id() == tab_id) {
            let tab = self.tabs.remove(idx);
            if let NativeTab::Terminal { view, .. } = tab {
                view.update(cx, |v, _| v.close());
            }
            if self.active_tab.as_deref() == Some(tab_id) {
                self.active_tab = self
                    .tabs
                    .get(idx.saturating_sub(1).min(self.tabs.len().saturating_sub(1)))
                    .map(NativeTab::id);
            }
            cx.notify();
        }
    }

    fn open_files_tab(&mut self, cx: &mut Context<Self>) {
        if !self.tabs.iter().any(|t| matches!(t, NativeTab::Files)) {
            self.tabs.push(NativeTab::Files);
        }
        self.active_tab = Some("files".into());
        self.load_dir(cx);
        cx.notify();
    }

    fn open_git_tab(&mut self, cx: &mut Context<Self>) {
        if !self.tabs.iter().any(|t| matches!(t, NativeTab::Git)) {
            self.tabs.push(NativeTab::Git);
        }
        self.active_tab = Some("git".into());
        self.load_git(cx);
        cx.notify();
    }

    fn set_active_tab(&mut self, id: String, cx: &mut Context<Self>) {
        self.active_tab = Some(id);
        cx.notify();
    }

    fn active_session(&self) -> Option<&SessionSummary> {
        let id = self.active_terminal_id()?;
        self.sessions.iter().find(|s| s.id == id)
    }

    fn rename_active_session(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.active_terminal_id() else {
            self.status = "No active terminal session".into();
            cx.notify();
            return;
        };
        let name = self.rename_input.read(cx).text().trim().to_string();
        if name.is_empty() {
            self.status = "Rename needs a non-empty name".into();
            cx.notify();
            return;
        }
        let api = self.api.clone();
        let handle = bridge::handle();
        cx.spawn(
            move |weak: gpui::WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                let mut cx = cx.clone();
                async move {
                    let res = handle
                        .spawn(async move { api.rename_session(id, &name).await.map(|_| name) })
                        .await;
                    let _ = weak.update(&mut cx, |v, cx| {
                        match res {
                            Ok(Ok(name)) => {
                                if let Some(s) = v.sessions.iter_mut().find(|s| s.id == id) {
                                    s.name = name.clone();
                                }
                                for tab in &mut v.tabs {
                                    if let NativeTab::Terminal {
                                        session_id, title, ..
                                    } = tab
                                    {
                                        if *session_id == id {
                                            *title = name.clone().into();
                                        }
                                    }
                                }
                                v.status = "Renamed session".into();
                            }
                            Ok(Err(e)) => {
                                v.status = SharedString::from(format!("rename failed: {e}"))
                            }
                            Err(e) => v.status = SharedString::from(format!("task failed: {e}")),
                        }
                        cx.notify();
                    });
                }
            },
        )
        .detach();
    }

    fn duplicate_active_session(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.active_session().cloned() else {
            self.status = "No active terminal session".into();
            cx.notify();
            return;
        };
        let api = self.api.clone();
        let handle = bridge::handle();
        let spec = SessionSpec {
            name: format!("{}-copy", session.name),
            cwd: non_empty(&session.cwd),
            ..Default::default()
        };
        cx.spawn(
            move |weak: gpui::WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                let mut cx = cx.clone();
                async move {
                    let res = handle
                        .spawn(async move { api.create_session(&spec).await })
                        .await;
                    let _ = weak.update(&mut cx, |v, cx| {
                        match res {
                            Ok(Ok(summary)) => {
                                let id = summary.id;
                                v.sessions.push(summary);
                                v.attach(id, cx);
                            }
                            Ok(Err(e)) => {
                                v.status = SharedString::from(format!("duplicate failed: {e}"))
                            }
                            Err(e) => v.status = SharedString::from(format!("task failed: {e}")),
                        }
                        cx.notify();
                    });
                }
            },
        )
        .detach();
    }

    fn kill_active_session(&mut self, delete_after: bool, cx: &mut Context<Self>) {
        let Some(id) = self.active_terminal_id() else {
            self.status = "No active terminal session".into();
            cx.notify();
            return;
        };
        let api = self.api.clone();
        let handle = bridge::handle();
        cx.spawn(
            move |weak: gpui::WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                let mut cx = cx.clone();
                async move {
                    let res = handle
                        .spawn(async move {
                            api.kill_session(id).await?;
                            if delete_after {
                                api.delete_session(id).await?;
                            }
                            Ok::<(), anyhow::Error>(())
                        })
                        .await;
                    let _ = weak.update(&mut cx, |v, cx| {
                        match res {
                            Ok(Ok(())) => {
                                if delete_after {
                                    v.sessions.retain(|s| s.id != id);
                                    v.close_tab(&format!("term:{id}"), cx);
                                    v.status = "Killed and removed session".into();
                                } else {
                                    v.status = "Kill sent".into();
                                }
                                v.refresh_sessions(cx);
                            }
                            Ok(Err(e)) => {
                                v.status = SharedString::from(format!("kill failed: {e}"))
                            }
                            Err(e) => v.status = SharedString::from(format!("task failed: {e}")),
                        }
                        cx.notify();
                    });
                }
            },
        )
        .detach();
    }

    fn reattach_active_terminal(&mut self, cx: &mut Context<Self>) {
        if let Some(term) = self.active_terminal_entity() {
            term.update(cx, |v, cx| v.reattach(cx));
        }
    }

    fn clear_active_terminal(&mut self, cx: &mut Context<Self>) {
        if let Some(term) = self.active_terminal_entity() {
            term.update(cx, |v, cx| v.clear(cx));
        }
    }

    fn apply_terminal_search(&mut self, cx: &mut Context<Self>) {
        let query = self.search_input.read(cx).text().to_string();
        if let Some(term) = self.active_terminal_entity() {
            term.update(cx, |v, cx| v.set_search_query(query, cx));
        }
    }

    fn load_dir(&mut self, cx: &mut Context<Self>) {
        let path = self
            .file_panel
            .path_input
            .read(cx)
            .text()
            .trim()
            .to_string();
        let api = self.api.clone();
        let handle = bridge::handle();
        cx.spawn(
            move |weak: gpui::WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                let mut cx = cx.clone();
                async move {
                    let res = handle.spawn(async move { api.list_dir(&path).await }).await;
                    let _ = weak.update(&mut cx, |v, cx| {
                        match res {
                            Ok(Ok(entries)) => {
                                v.file_panel.entries = entries;
                                v.status = "Directory loaded".into();
                            }
                            Ok(Err(e)) => v.status = SharedString::from(format!("dir failed: {e}")),
                            Err(e) => v.status = SharedString::from(format!("task failed: {e}")),
                        }
                        cx.notify();
                    });
                }
            },
        )
        .detach();
    }

    fn read_file(&mut self, cx: &mut Context<Self>) {
        let path = self
            .file_panel
            .read_input
            .read(cx)
            .text()
            .trim()
            .to_string();
        if path.is_empty() {
            self.status = "File path required".into();
            cx.notify();
            return;
        }
        let api = self.api.clone();
        let handle = bridge::handle();
        cx.spawn(
            move |weak: gpui::WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                let mut cx = cx.clone();
                async move {
                    let res = handle.spawn(async move { api.read_file(&path).await }).await;
                    let _ = weak.update(&mut cx, |v, cx| {
                        match res {
                            Ok(Ok(file)) => {
                                let content = if file.is_binary {
                                    format!(
                                        "Binary file, {} bytes. Base64 payload omitted from native preview.",
                                        file.size
                                    )
                                } else {
                                    file.content.unwrap_or_default()
                                };
                                v.file_panel.file_preview = Some((file.path, content));
                                v.status = "File loaded".into();
                            }
                            Ok(Err(e)) => v.status = SharedString::from(format!("read failed: {e}")),
                            Err(e) => v.status = SharedString::from(format!("task failed: {e}")),
                        }
                        cx.notify();
                    });
                }
            },
        )
        .detach();
    }

    fn search_files(&mut self, cx: &mut Context<Self>) {
        let query = self
            .file_panel
            .search_input
            .read(cx)
            .text()
            .trim()
            .to_string();
        let path = self
            .file_panel
            .path_input
            .read(cx)
            .text()
            .trim()
            .to_string();
        if query.is_empty() {
            self.file_panel.search_hits.clear();
            cx.notify();
            return;
        }
        let api = self.api.clone();
        let handle = bridge::handle();
        cx.spawn(
            move |weak: gpui::WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                let mut cx = cx.clone();
                async move {
                    let res = handle
                        .spawn(async move { api.search(&query, &path, 50).await })
                        .await;
                    let _ = weak.update(&mut cx, |v, cx| {
                        match res {
                            Ok(Ok(hits)) => {
                                v.file_panel.search_hits =
                                    hits.into_iter().map(|h| (h.path, h.line, h.text)).collect();
                                v.status = "Search complete".into();
                            }
                            Ok(Err(e)) => {
                                v.status = SharedString::from(format!("search failed: {e}"))
                            }
                            Err(e) => v.status = SharedString::from(format!("task failed: {e}")),
                        }
                        cx.notify();
                    });
                }
            },
        )
        .detach();
    }

    fn load_git(&mut self, cx: &mut Context<Self>) {
        let repo = self.git_panel.repo_input.read(cx).text().trim().to_string();
        let api = self.api.clone();
        let handle = bridge::handle();
        cx.spawn(
            move |weak: gpui::WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                let mut cx = cx.clone();
                async move {
                    let repo_for_status = repo.clone();
                    let api_for_status = api.clone();
                    let status = handle
                        .spawn(async move { api_for_status.git_status(&repo_for_status).await })
                        .await;
                    let log = handle
                        .spawn(async move { api.git_log(&repo, 12).await })
                        .await;
                    let _ = weak.update(&mut cx, |v, cx| {
                        match status {
                            Ok(Ok(status)) => v.git_panel.status = Some(status),
                            Ok(Err(e)) => {
                                v.status = SharedString::from(format!("git status failed: {e}"))
                            }
                            Err(e) => v.status = SharedString::from(format!("task failed: {e}")),
                        }
                        if let Ok(Ok(log)) = log {
                            v.git_panel.log = log;
                        }
                        cx.notify();
                    });
                }
            },
        )
        .detach();
    }

    fn settings_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors.clone();
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(14.0))
            .bg(colors.surface)
            .child(Label::new("Server settings").size(LabelSize::Title))
            .child(
                div()
                    .w(px(460.0))
                    .flex()
                    .flex_col()
                    .gap(px(12.0))
                    .child(Field::new(self.url_input.clone()).label("Server URL"))
                    .child(Field::new(self.token_input.clone()).label("API token"))
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(colors.on_subtle)
                            .child("The config file is written with owner-only permissions on Unix. OS keychain storage is still tracked in the native backlog."),
                    )
                    .child(
                        Button::new("save-settings")
                            .label("Save & Connect")
                            .appearance(ButtonAppearance::Accent)
                            .on_click(
                                cx.listener(|view, _: &ClickEvent, _, cx| view.save_settings(cx)),
                            ),
                    ),
            )
    }

    fn new_session_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors.clone();
        div()
            .flex()
            .flex_col()
            .gap(px(10.0))
            .p(px(10.0))
            .border_1()
            .border_color(colors.stroke_neutral_subtle)
            .child(Label::new("New session").size(LabelSize::Subtitle))
            .child(Field::new(self.new_name_input.clone()).label("Name"))
            .child(Field::new(self.new_cwd_input.clone()).label("Working directory"))
            .child(Field::new(self.new_command_input.clone()).label("Command"))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(8.0))
                    .child(
                        Button::new("create-session")
                            .label("Create")
                            .appearance(ButtonAppearance::Accent)
                            .on_click(cx.listener(|view, _, _, cx| view.create_session(cx))),
                    )
                    .child(
                        Button::new("cancel-create")
                            .label("Cancel")
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.show_new_session = false;
                                cx.notify();
                            })),
                    ),
            )
    }

    fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors.clone();
        let active_id = self.active_terminal_id();

        let mut list = div().flex().flex_col().gap(px(4.0)).w_full();
        for s in &self.sessions {
            let id = s.id;
            let is_active = active_id == Some(id);
            let label = format!("{} {}", s.activity.badge(), s.name);
            list = list.child(
                Button::new(SharedString::from(format!("sess-{id}")))
                    .label(label)
                    .appearance(if is_active {
                        ButtonAppearance::Accent
                    } else {
                        ButtonAppearance::Subtle
                    })
                    .on_click(cx.listener(move |view, _, _, cx| view.attach(id, cx))),
            );
        }

        let mut sidebar = div()
            .flex()
            .flex_col()
            .h_full()
            .w(px(288.0))
            .p(px(12.0))
            .gap(px(10.0))
            .bg(colors.surface)
            .child(Label::new("MyDevEnv2").size(LabelSize::Title))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(6.0))
                    .flex_wrap()
                    .child(
                        Button::new("new-session")
                            .label("New")
                            .appearance(ButtonAppearance::Accent)
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.show_new_session = !view.show_new_session;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("refresh")
                            .label("Refresh")
                            .on_click(cx.listener(|view, _, _, cx| view.refresh_sessions(cx))),
                    )
                    .child(
                        Button::new("files")
                            .label("Files")
                            .on_click(cx.listener(|view, _, _, cx| view.open_files_tab(cx))),
                    )
                    .child(
                        Button::new("git")
                            .label("Git")
                            .on_click(cx.listener(|view, _, _, cx| view.open_git_tab(cx))),
                    )
                    .child(
                        Button::new("settings")
                            .label("Settings")
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.show_settings = !view.show_settings;
                                cx.notify();
                            })),
                    ),
            );

        if self.show_new_session {
            sidebar = sidebar.child(self.new_session_panel(cx));
        }

        sidebar.child(list).child(div().flex_grow()).child(
            div()
                .text_color(colors.on_subtle)
                .text_size(px(12.0))
                .child(self.status.clone()),
        )
    }

    fn terminal_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors.clone();
        let active_session_name = self
            .active_session()
            .map(|s| s.name.clone())
            .unwrap_or_else(|| "No terminal".into());

        div()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .p(px(10.0))
            .border_b_1()
            .border_color(colors.stroke_neutral_subtle)
            .bg(colors.surface)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .gap(px(8.0))
                    .child(
                        Label::new(active_session_name)
                            .size(LabelSize::Subtitle)
                            .truncate(),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap(px(6.0))
                            .child(Button::new("reattach").label("Reattach").on_click(
                                cx.listener(|view, _, _, cx| view.reattach_active_terminal(cx)),
                            ))
                            .child(Button::new("clear-term").label("Clear").on_click(
                                cx.listener(|view, _, _, cx| view.clear_active_terminal(cx)),
                            ))
                            .child(
                                Button::new("duplicate-session")
                                    .label("Duplicate")
                                    .on_click(cx.listener(|view, _, _, cx| {
                                        view.duplicate_active_session(cx)
                                    })),
                            )
                            .child(
                                Button::new("kill-session")
                                    .label("Kill")
                                    .appearance(ButtonAppearance::Danger)
                                    .on_click(cx.listener(|view, _, _, cx| {
                                        view.kill_active_session(false, cx)
                                    })),
                            )
                            .child(
                                Button::new("delete-session")
                                    .label("Kill & Remove")
                                    .appearance(ButtonAppearance::Danger)
                                    .on_click(cx.listener(|view, _, _, cx| {
                                        view.kill_active_session(true, cx)
                                    })),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .child(
                        div()
                            .w(px(220.0))
                            .child(Field::new(self.rename_input.clone()).label("Rename")),
                    )
                    .child(
                        Button::new("rename-active")
                            .label("Rename")
                            .on_click(cx.listener(|view, _, _, cx| view.rename_active_session(cx))),
                    )
                    .child(
                        div()
                            .w(px(260.0))
                            .child(Field::new(self.search_input.clone()).label("Search")),
                    )
                    .child(
                        Button::new("search-active")
                            .label("Apply")
                            .on_click(cx.listener(|view, _, _, cx| view.apply_terminal_search(cx))),
                    ),
            )
    }

    fn tab_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors.clone();
        let active = self.active_tab.clone();
        let mut row = div()
            .flex()
            .flex_row()
            .items_center()
            .h(px(38.0))
            .bg(colors.tab_strip_bg)
            .border_b_1()
            .border_color(colors.stroke_neutral_subtle)
            .overflow_hidden();

        for tab in &self.tabs {
            let id = tab.id();
            let close_id = id.clone();
            let is_active = active.as_deref() == Some(id.as_str());
            let closable = true;
            let mut tab_el = div()
                .id(SharedString::from(format!("tab-{id}")))
                .h_full()
                .min_w(px(120.0))
                .max_w(px(220.0))
                .px(px(10.0))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .border_r_1()
                .border_color(colors.stroke_neutral_subtle)
                .bg(if is_active {
                    colors.tab_active_bg
                } else {
                    colors.tab_strip_bg
                })
                .on_click(cx.listener(move |view, _, _, cx| view.set_active_tab(id.clone(), cx)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .font_weight(if is_active {
                            FontWeight::SEMIBOLD
                        } else {
                            FontWeight::NORMAL
                        })
                        .child(tab.label()),
                );
            if closable {
                tab_el = tab_el.child(
                    Button::new(SharedString::from(format!("close-{close_id}")))
                        .label("x")
                        .appearance(ButtonAppearance::Subtle)
                        .on_click(cx.listener(move |view, _, _, cx| view.close_tab(&close_id, cx))),
                );
            }
            row = row.child(tab_el);
        }
        row
    }

    fn files_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors.clone();
        let mut entries = div()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .overflow_hidden()
            .h(px(220.0))
            .border_1()
            .border_color(colors.stroke_neutral_subtle)
            .p(px(8.0));
        for entry in &self.file_panel.entries {
            entries = entries.child(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(8.0))
                    .text_size(px(12.0))
                    .child(if entry.is_dir { "dir" } else { "file" })
                    .child(entry.path.clone()),
            );
        }

        let mut hits = div().flex().flex_col().gap(px(2.0));
        for (path, line, text) in &self.file_panel.search_hits {
            hits = hits.child(
                div()
                    .text_size(px(12.0))
                    .font_family(".ZedMono")
                    .child(format!("{path}:{line}: {text}")),
            );
        }

        let preview = self
            .file_panel
            .file_preview
            .as_ref()
            .map(|(path, content)| {
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .child(
                        Label::new(path.clone())
                            .size(LabelSize::Subtitle)
                            .truncate(),
                    )
                    .child(
                        div()
                            .h(px(280.0))
                            .overflow_hidden()
                            .border_1()
                            .border_color(colors.stroke_neutral_subtle)
                            .p(px(8.0))
                            .font_family(".ZedMono")
                            .text_size(px(12.0))
                            .child(content.clone()),
                    )
            });

        let mut panel =
            div()
                .size_full()
                .overflow_hidden()
                .p(px(14.0))
                .flex()
                .flex_col()
                .gap(px(12.0))
                .bg(colors.surface)
                .child(Label::new("Files").size(LabelSize::Title))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .gap(px(8.0))
                        .items_end()
                        .child(div().w(px(320.0)).child(
                            Field::new(self.file_panel.path_input.clone()).label("Directory"),
                        ))
                        .child(
                            Button::new("load-dir")
                                .label("Load")
                                .appearance(ButtonAppearance::Accent)
                                .on_click(cx.listener(|view, _, _, cx| view.load_dir(cx))),
                        ),
                )
                .child(entries)
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .gap(px(8.0))
                        .items_end()
                        .child(div().w(px(360.0)).child(
                            Field::new(self.file_panel.read_input.clone()).label("Read file"),
                        ))
                        .child(
                            Button::new("read-file")
                                .label("Preview")
                                .on_click(cx.listener(|view, _, _, cx| view.read_file(cx))),
                        ),
                );

        if let Some(preview) = preview {
            panel = panel.child(preview);
        }

        panel
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(8.0))
                    .items_end()
                    .child(
                        div().w(px(360.0)).child(
                            Field::new(self.file_panel.search_input.clone()).label("Search"),
                        ),
                    )
                    .child(
                        Button::new("search-files")
                            .label("Search")
                            .on_click(cx.listener(|view, _, _, cx| view.search_files(cx))),
                    ),
            )
            .child(hits)
    }

    fn git_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors.clone();
        let mut status_entries = div().flex().flex_col().gap(px(3.0));
        if let Some(status) = &self.git_panel.status {
            status_entries =
                status_entries.child(div().font_weight(FontWeight::SEMIBOLD).child(format!(
                    "{} on {} (+{} / -{})",
                    status.repo, status.branch, status.ahead, status.behind
                )));
            for entry in &status.entries {
                status_entries = status_entries.child(
                    div()
                        .font_family(".ZedMono")
                        .text_size(px(12.0))
                        .child(format!("{:?} {} {}", entry.kind, entry.index, entry.path)),
                );
            }
        }

        let mut log_list = div().flex().flex_col().gap(px(4.0));
        for entry in &self.git_panel.log {
            log_list = log_list.child(
                div()
                    .font_family(".ZedMono")
                    .text_size(px(12.0))
                    .child(format!("{} {} {}", entry.short, entry.date, entry.subject)),
            );
        }

        div()
            .size_full()
            .overflow_hidden()
            .p(px(14.0))
            .flex()
            .flex_col()
            .gap(px(12.0))
            .bg(colors.surface)
            .child(Label::new("Git").size(LabelSize::Title))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(8.0))
                    .items_end()
                    .child(div().w(px(320.0)).child(Field::new(self.git_panel.repo_input.clone()).label("Repository")))
                    .child(
                        Button::new("load-git")
                            .label("Refresh")
                            .appearance(ButtonAppearance::Accent)
                            .on_click(cx.listener(|view, _, _, cx| view.load_git(cx))),
                    ),
            )
            .child(MessageBar::new("Status and log are native read-only views; diff/editor actions remain backlog work.").intent(MessageIntent::Info))
            .child(Label::new("Status").size(LabelSize::Subtitle))
            .child(status_entries)
            .child(Label::new("Recent commits").size(LabelSize::Subtitle))
            .child(log_list)
    }
}

impl Render for RootView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors.clone();
        let sidebar = self.sidebar(cx);

        let main: gpui::AnyElement = if self.show_settings {
            self.settings_panel(cx).into_any_element()
        } else if self.tabs.is_empty() {
            div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(8.0))
                .bg(gpui::rgb(0x1e1e2e))
                .child(
                    div()
                        .text_color(colors.on_subtle)
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(if self.configured {
                            "Select or create a session"
                        } else {
                            "Configure the server token to begin"
                        }),
                )
                .into_any_element()
        } else {
            let active = self.active_tab.clone();
            let active_tab = active
                .as_deref()
                .and_then(|id| self.tabs.iter().find(|tab| tab.id() == id))
                .or_else(|| self.tabs.first());
            let content: gpui::AnyElement = match active_tab {
                Some(NativeTab::Terminal { view, .. }) => div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .child(self.terminal_toolbar(cx))
                    .child(div().flex_grow().min_h_0().child(view.clone()))
                    .into_any_element(),
                Some(NativeTab::Files) => self.files_panel(cx).into_any_element(),
                Some(NativeTab::Git) => self.git_panel(cx).into_any_element(),
                None => div().size_full().into_any_element(),
            };
            div()
                .size_full()
                .flex()
                .flex_col()
                .child(self.tab_bar(cx))
                .child(div().flex_grow().min_h_0().child(content))
                .into_any_element()
        };

        div()
            .flex()
            .flex_row()
            .size_full()
            .bg(colors.surface)
            .child(sidebar)
            .child(div().flex_grow().min_w_0().h_full().child(main))
    }
}

fn non_empty(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn shell_command(value: &str) -> Option<Vec<String>> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(vec!["bash".into(), "-lc".into(), trimmed.to_string()])
    }
}
