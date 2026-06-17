//! GPUI application shell: server connection, session list sidebar, persistent
//! terminal tabs, and lightweight file/git utility panels.

mod terminal_view;

use fluent_app::FluentApp;
use fluent_core::ThemeProvider as _;
use fluent_layout::{MessageBar, MessageIntent, TabItem, TabStrip};
use fluent_primitives::{Button, ButtonAppearance, Field, Label, LabelSize, TextInput};
#[cfg(target_os = "windows")]
use gpui::WindowControlArea;
use gpui::{
    div, prelude::*, px, svg, ClickEvent, Context, Entity, FontWeight, IntoElement, Render,
    SharedString, Window,
};
use uuid::Uuid;

use futures_util::StreamExt as _;
use mydevenv2_client::{
    bridge,
    client::ApiClient,
    config::ClientConfig,
    protocol::{FileEntry, GitStatus, LogEntry, ServerEvent, SessionSpec, SessionSummary},
    terminal,
};

use terminal_view::TerminalView;

const APP_TITLE: &str = "MyDevEnv2";
const APP_ID: &str = "com.sprooty.mydevenv2-client";
const WINDOW_W: f32 = 1240.0;
const WINDOW_H: f32 = 820.0;
const WINDOW_MIN_W: f32 = 760.0;
const WINDOW_MIN_H: f32 = 500.0;

/// Sidebar resize bounds and the collapsed icon-rail width. (#8)
const SIDEBAR_MIN_W: f32 = 180.0;
const SIDEBAR_MAX_W: f32 = 520.0;
const SIDEBAR_RAIL_W: f32 = 56.0;

/// Launch the desktop client. Blocks on the GPUI event loop until the window
/// closes.
pub fn run(cfg: ClientConfig) {
    FluentApp::new(APP_TITLE)
        .window_size(WINDOW_W, WINDOW_H)
        .window_min_size(WINDOW_MIN_W, WINDOW_MIN_H)
        .app_id(APP_ID)
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
    tab_strip: Option<Entity<TabStrip>>,
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
    /// Persisted terminal font size (zoom). New terminals open at this size and
    /// zoom changes are written back here. (#3)
    font_size: f32,
    /// Left-sidebar width and collapsed state. (#8)
    sidebar_width: f32,
    sidebar_collapsed: bool,
    /// True while the user is dragging the sidebar resize handle. (#8)
    sidebar_dragging: bool,
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
        let font_size = cfg
            .font_size
            .clamp(terminal::MIN_FONT_SIZE, terminal::MAX_FONT_SIZE);
        let sidebar_width = cfg.sidebar_width.clamp(SIDEBAR_MIN_W, SIDEBAR_MAX_W);
        let sidebar_collapsed = cfg.sidebar_collapsed;
        let mut view = Self {
            api,
            configured,
            sessions: Vec::new(),
            status,
            tabs: Vec::new(),
            active_tab: None,
            tab_strip: None,
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
            font_size,
            sidebar_width,
            sidebar_collapsed,
            sidebar_dragging: false,
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

    fn rebuild_tab_strip(&mut self, cx: &mut Context<Self>) {
        if self.tabs.is_empty() {
            self.tab_strip = None;
            cx.notify();
            return;
        }

        let tabs = self.tabs.clone();
        let active_index = self
            .active_tab
            .as_deref()
            .and_then(|id| tabs.iter().position(|tab| tab.id() == id))
            .unwrap_or(0);
        let root = cx.entity().downgrade();
        let strip = cx.new(move |cx: &mut Context<TabStrip>| {
            let mut ts = TabStrip::new(cx);
            for tab in &tabs {
                ts.add_tab(TabItem::new(tab.id()).label(tab.label()).closable(true));
            }
            ts.active = active_index.min(ts.tabs.len().saturating_sub(1));

            let select_root = root.clone();
            ts = ts.on_select(move |idx, _strip, _window, cx| {
                let _ = select_root.update(cx, |root, cx| root.set_active_tab_by_index(idx, cx));
            });

            let close_root = root.clone();
            ts.on_close(move |idx, _strip, _window, cx| {
                let _ = close_root.update(cx, |root, cx| root.close_tab_by_index(idx, cx));
            })
        });
        self.tab_strip = Some(strip);
        cx.notify();
    }

    fn set_active_tab_by_index(&mut self, idx: usize, cx: &mut Context<Self>) {
        if let Some(tab) = self.tabs.get(idx) {
            self.active_tab = Some(tab.id());
            self.rebuild_tab_strip(cx);
        }
    }

    fn close_tab_by_index(&mut self, idx: usize, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(idx).cloned() else {
            return;
        };
        if let NativeTab::Terminal { view, .. } = &tab {
            view.update(cx, |v, cx| v.close(cx));
        }
        self.tabs.remove(idx);
        if self.tabs.is_empty() {
            self.active_tab = None;
        } else if self.active_tab.as_deref() == Some(tab.id().as_str()) {
            let next = idx.saturating_sub(1).min(self.tabs.len().saturating_sub(1));
            self.active_tab = self.tabs.get(next).map(NativeTab::id);
        }
        self.rebuild_tab_strip(cx);
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
                self.rebuild_tab_strip(cx);
                cx.notify();
            }
            ServerEvent::SessionCreated { .. } | ServerEvent::SessionKilled { .. } => {
                self.refresh_sessions(cx);
            }
        }
    }

    /// Current persistable config built from live UI state. The server URL and
    /// token come from the saved config on disk (they only change via the
    /// settings panel), so callers that change zoom/sidebar layout can persist
    /// without clobbering credentials.
    fn config_snapshot(&self) -> ClientConfig {
        let mut cfg = ClientConfig::load();
        cfg.font_size = self.font_size;
        cfg.sidebar_width = self.sidebar_width;
        cfg.sidebar_collapsed = self.sidebar_collapsed;
        cfg
    }

    /// Persist the current zoom + sidebar layout to disk (best-effort).
    fn persist_layout(&mut self, cx: &mut Context<Self>) {
        if let Err(e) = self.config_snapshot().save() {
            self.status = SharedString::from(format!("save layout failed: {e}"));
            cx.notify();
        }
    }

    fn save_settings(&mut self, cx: &mut Context<Self>) {
        let server_url = self.url_input.read(cx).text().trim().to_string();
        let token = self.token_input.read(cx).text().trim().to_string();
        let cfg = ClientConfig {
            server_url,
            token,
            ..self.config_snapshot()
        };
        if let Err(e) = cfg.save() {
            self.status = SharedString::from(format!("save failed: {e}"));
            cx.notify();
            return;
        }
        for tab in &self.tabs {
            if let NativeTab::Terminal { view, .. } = tab {
                view.update(cx, |v, cx| v.close(cx));
            }
        }
        self.api = ApiClient::new(cfg.base(), cfg.token.clone());
        self.configured = cfg.is_configured();
        self.tabs.clear();
        self.active_tab = None;
        self.tab_strip = None;
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
            self.rebuild_tab_strip(cx);
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
        let font_size = self.font_size;
        let term = cx.new(|cx| TerminalView::new(ws_url, token, id, font_size, cx));
        self.tabs.push(NativeTab::Terminal {
            session_id: id,
            title: label.into(),
            view: term,
        });
        self.active_tab = Some(tab_id);
        self.status = SharedString::from(format!("attached {id}"));
        self.rebuild_tab_strip(cx);
    }

    fn close_tab(&mut self, tab_id: &str, cx: &mut Context<Self>) {
        if let Some(idx) = self.tabs.iter().position(|t| t.id() == tab_id) {
            self.close_tab_by_index(idx, cx);
        }
    }

    fn open_files_tab(&mut self, cx: &mut Context<Self>) {
        if !self.tabs.iter().any(|t| matches!(t, NativeTab::Files)) {
            self.tabs.push(NativeTab::Files);
        }
        self.active_tab = Some("files".into());
        self.load_dir(cx);
        self.rebuild_tab_strip(cx);
    }

    fn open_git_tab(&mut self, cx: &mut Context<Self>) {
        if !self.tabs.iter().any(|t| matches!(t, NativeTab::Git)) {
            self.tabs.push(NativeTab::Git);
        }
        self.active_tab = Some("git".into());
        self.load_git(cx);
        self.rebuild_tab_strip(cx);
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
                                v.rebuild_tab_strip(cx);
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

    /// Zoom the active terminal and remember the new size for future terminals.
    /// `delta` of 0.0 resets to the default. (#3)
    fn zoom_active_terminal(&mut self, delta: f32, cx: &mut Context<Self>) {
        let Some(term) = self.active_terminal_entity() else {
            return;
        };
        let new_size = term.update(cx, |v, cx| {
            if delta == 0.0 {
                v.zoom_reset(cx);
            } else {
                v.zoom_by(delta, cx);
            }
            v.font_size()
        });
        if (new_size - self.font_size).abs() > f32::EPSILON {
            self.font_size = new_size;
            self.persist_layout(cx);
        }
    }

    /// Apply the persisted zoom level to every other open terminal so all tabs
    /// stay visually consistent. (#3)
    fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        self.sidebar_collapsed = !self.sidebar_collapsed;
        self.persist_layout(cx);
        cx.notify();
    }

    fn set_sidebar_width(&mut self, width: f32, cx: &mut Context<Self>) {
        let clamped = width.clamp(SIDEBAR_MIN_W, SIDEBAR_MAX_W);
        if (clamped - self.sidebar_width).abs() > 0.5 {
            self.sidebar_width = clamped;
            cx.notify();
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

    /// Set the Files panel's current directory and reload it. (#5)
    fn navigate_dir(&mut self, path: String, cx: &mut Context<Self>) {
        self.file_panel
            .path_input
            .update(cx, |input, cx| input.set_value(path, cx));
        self.load_dir(cx);
    }

    /// Navigate into a child entry: descend into dirs, preview files. (#5)
    fn open_entry(&mut self, entry: &FileEntry, cx: &mut Context<Self>) {
        if entry.is_dir {
            self.navigate_dir(entry.path.clone(), cx);
        } else {
            self.file_panel
                .read_input
                .update(cx, |input, cx| input.set_value(entry.path.clone(), cx));
            self.read_file(cx);
        }
    }

    /// Go up one directory from the current Files path. (#5)
    fn navigate_parent(&mut self, cx: &mut Context<Self>) {
        let cur = self
            .file_panel
            .path_input
            .read(cx)
            .text()
            .trim()
            .trim_end_matches('/')
            .to_string();
        let parent = match cur.rsplit_once('/') {
            Some((head, _)) if !head.is_empty() => head.to_string(),
            _ => ".".to_string(),
        };
        self.navigate_dir(parent, cx);
    }

    /// Open the native OS file picker and upload the chosen files into the
    /// current workspace directory via the typed REST client. (#5)
    fn pick_and_upload(&mut self, cx: &mut Context<Self>) {
        let dest_dir = self
            .file_panel
            .path_input
            .read(cx)
            .text()
            .trim()
            .trim_end_matches('/')
            .to_string();
        let paths_rx = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Upload".into()),
        });
        let api = self.api.clone();
        let handle = bridge::handle();
        cx.spawn(
            move |weak: gpui::WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                let mut cx = cx.clone();
                async move {
                    let picked = match paths_rx.await {
                        Ok(Ok(Some(paths))) => paths,
                        Ok(Ok(None)) => return, // user cancelled
                        Ok(Err(e)) => {
                            let _ = weak.update(&mut cx, |v, cx| {
                                v.status = SharedString::from(format!("file picker failed: {e}"));
                                cx.notify();
                            });
                            return;
                        }
                        Err(_) => return,
                    };

                    let total = picked.len();
                    let mut uploaded = 0usize;
                    let mut last_err: Option<String> = None;
                    for local in picked {
                        let name = local
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| "upload.bin".into());
                        let dest = if dest_dir.is_empty() || dest_dir == "." {
                            name.clone()
                        } else {
                            format!("{dest_dir}/{name}")
                        };
                        let api2 = api.clone();
                        let res = handle
                            .spawn(async move {
                                let bytes = tokio::fs::read(&local).await?;
                                api2.upload_file(&dest, &bytes).await
                            })
                            .await;
                        match res {
                            Ok(Ok(())) => uploaded += 1,
                            Ok(Err(e)) => last_err = Some(format!("{name}: {e}")),
                            Err(e) => last_err = Some(format!("{name}: task failed: {e}")),
                        }
                    }

                    let _ = weak.update(&mut cx, |v, cx| {
                        v.status = match &last_err {
                            Some(e) => SharedString::from(format!(
                                "uploaded {uploaded}/{total}; error: {e}"
                            )),
                            None => {
                                SharedString::from(format!("uploaded {uploaded}/{total} file(s)"))
                            }
                        };
                        v.load_dir(cx);
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

    #[cfg(target_os = "windows")]
    fn windows_title_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors.clone();
        let fg = colors.on_neutral;
        let ctrl_hover = colors.subtle_hover;
        let close_hover: gpui::Hsla = gpui::rgb(0xC42B1C).into();

        // GPUI's Windows backend consumes these regions during WM_NCHITTEST.
        div()
            .flex()
            .flex_col()
            .h(px(36.0))
            .w_full()
            .bg(colors.surface_dim)
            .border_b_1()
            .border_color(colors.stroke_neutral_subtle)
            .child(div().h(px(6.0)).w_full())
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .h(px(30.0))
                    .w_full()
                    .px(px(6.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .h_full()
                            .flex_1()
                            .min_w_0()
                            .pl(px(6.0))
                            .text_size(px(12.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(fg)
                            .window_control_area(WindowControlArea::Drag)
                            .child(APP_TITLE),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .h_full()
                            .child(
                                div()
                                    .id("windows-titlebar-min")
                                    .w(px(46.0))
                                    .h_full()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .hover(move |s| s.bg(ctrl_hover))
                                    .window_control_area(WindowControlArea::Min)
                                    .child(
                                        svg()
                                            .path("icons/minimize.svg")
                                            .size(px(10.0))
                                            .text_color(fg),
                                    ),
                            )
                            .child(
                                div()
                                    .id("windows-titlebar-max")
                                    .w(px(46.0))
                                    .h_full()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .hover(move |s| s.bg(ctrl_hover))
                                    .window_control_area(WindowControlArea::Max)
                                    .child(
                                        svg()
                                            .path("icons/maximize.svg")
                                            .size(px(10.0))
                                            .text_color(fg),
                                    ),
                            )
                            .child(
                                div()
                                    .id("windows-titlebar-close")
                                    .w(px(46.0))
                                    .h_full()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .hover(move |s| s.bg(close_hover))
                                    .window_control_area(WindowControlArea::Close)
                                    .child(
                                        svg()
                                            .path("icons/dismiss.svg")
                                            .size(px(10.0))
                                            .text_color(fg),
                                    ),
                            ),
                    ),
            )
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

    /// Collapsed icon-rail variant of the sidebar: narrow column of session
    /// badges plus an expand button. (#8)
    fn sidebar_rail(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors.clone();
        let active_id = self.active_terminal_id();

        let mut rail = div()
            .flex()
            .flex_col()
            .items_center()
            .h_full()
            .w(px(SIDEBAR_RAIL_W))
            .py(px(10.0))
            .gap(px(8.0))
            .bg(colors.surface)
            .border_r_1()
            .border_color(colors.stroke_neutral_subtle)
            .child(
                Button::new("sidebar-expand")
                    .icon("icons/panel_open.svg")
                    .on_click(cx.listener(|view, _, _, cx| view.toggle_sidebar(cx))),
            )
            .child(
                Button::new("rail-new")
                    .icon("icons/add.svg")
                    .appearance(ButtonAppearance::Accent)
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.sidebar_collapsed = false;
                        view.persist_layout(cx);
                        view.show_new_session = true;
                        cx.notify();
                    })),
            );

        let mut sessions_col = div()
            .id("rail-session-list")
            .flex()
            .flex_col()
            .items_center()
            .gap(px(6.0))
            .min_h_0()
            .flex_grow()
            .overflow_y_scroll();

        for session in &self.sessions {
            let id = session.id;
            let is_active = active_id == Some(id);
            let bg = if is_active {
                colors.neutral_selected
            } else {
                colors.surface
            };
            let hover = if is_active {
                colors.neutral_selected
            } else {
                colors.neutral_hover
            };
            sessions_col = sessions_col.child(
                div()
                    .id(SharedString::from(format!("rail-row-{id}")))
                    .flex()
                    .items_center()
                    .justify_center()
                    .w(px(38.0))
                    .h(px(38.0))
                    .rounded(px(6.0))
                    .bg(bg)
                    .border_1()
                    .border_color(if is_active {
                        colors.stroke_neutral
                    } else {
                        colors.stroke_neutral_subtle
                    })
                    .cursor_pointer()
                    .text_color(colors.on_neutral)
                    .hover(move |s| s.bg(hover))
                    .on_click(cx.listener(move |view, _, _, cx| view.attach(id, cx)))
                    .child(session.activity.badge()),
            );
        }

        rail = rail.child(sessions_col).child(
            Button::new("rail-settings")
                .icon("icons/settings.svg")
                .on_click(cx.listener(|view, _, _, cx| {
                    view.show_settings = !view.show_settings;
                    cx.notify();
                })),
        );
        rail
    }

    fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors.clone();
        let active_id = self.active_terminal_id();

        let mut list = div()
            .id("session-list")
            .flex()
            .flex_col()
            .gap(px(6.0))
            .w_full()
            .min_h_0()
            .flex_grow()
            .overflow_y_scroll();

        if self.sessions.is_empty() {
            let message = if self.configured {
                "No sessions"
            } else {
                "Configure server settings"
            };
            list = list.child(
                div()
                    .min_h(px(116.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(colors.on_subtle_disabled)
                    .child(message),
            );
        } else {
            for session in &self.sessions {
                let id = session.id;
                let is_active = active_id == Some(id);
                let row_bg = if is_active {
                    colors.neutral_selected
                } else {
                    colors.surface
                };
                let hover_bg = if is_active {
                    colors.neutral_selected
                } else {
                    colors.neutral_hover
                };

                list = list.child(
                    div()
                        .id(SharedString::from(format!("session-row-{id}")))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(8.0))
                        .w_full()
                        .min_h(px(58.0))
                        .px(px(10.0))
                        .py(px(8.0))
                        .rounded(px(6.0))
                        .bg(row_bg)
                        .border_1()
                        .border_color(if is_active {
                            colors.stroke_neutral
                        } else {
                            colors.stroke_neutral_subtle
                        })
                        .cursor_pointer()
                        .hover(move |style| style.bg(hover_bg))
                        .on_click(cx.listener(move |view, _, _, cx| view.attach(id, cx)))
                        .child(
                            div()
                                .flex_none()
                                .w(px(18.0))
                                .text_color(colors.on_subtle)
                                .child(session.activity.badge()),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .justify_center()
                                .gap(px(2.0))
                                .flex_1()
                                .min_w_0()
                                .overflow_hidden()
                                .child(
                                    div()
                                        .overflow_hidden()
                                        .whitespace_nowrap()
                                        .text_ellipsis()
                                        .font_weight(if is_active {
                                            FontWeight::SEMIBOLD
                                        } else {
                                            FontWeight::NORMAL
                                        })
                                        .child(session.name.clone()),
                                )
                                .child(
                                    div()
                                        .overflow_hidden()
                                        .whitespace_nowrap()
                                        .text_ellipsis()
                                        .text_size(px(12.0))
                                        .text_color(colors.on_subtle)
                                        .child(session.cwd.clone()),
                                ),
                        )
                        .child(
                            div()
                                .flex_none()
                                .text_size(px(12.0))
                                .text_color(colors.on_subtle)
                                .child(format!("{} KiB", session.scrollback_bytes / 1024)),
                        ),
                );
            }
        }

        let mut sidebar = div()
            .flex()
            .flex_col()
            .h_full()
            .w(px(self.sidebar_width))
            .p(px(12.0))
            .gap(px(10.0))
            .bg(colors.surface)
            // Header: title + collapse toggle.
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .child(Label::new("MyDevEnv2").size(LabelSize::Title))
                    .child(
                        Button::new("sidebar-collapse")
                            .icon("icons/panel_open.svg")
                            .on_click(cx.listener(|view, _, _, cx| view.toggle_sidebar(cx))),
                    ),
            )
            // Primary action row: New + Refresh.
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(6.0))
                    .child(
                        Button::new("new-session")
                            .icon("icons/add.svg")
                            .label("New")
                            .appearance(ButtonAppearance::Accent)
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.show_new_session = !view.show_new_session;
                                cx.notify();
                            })),
                    )
                    .child(div().flex_grow())
                    .child(
                        Button::new("refresh")
                            .icon("icons/arrow_forward.svg")
                            .on_click(cx.listener(|view, _, _, cx| view.refresh_sessions(cx))),
                    ),
            )
            // Secondary navigation row: Files / Git / Settings.
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(6.0))
                    .child(
                        Button::new("files")
                            .icon("icons/folder.svg")
                            .label("Files")
                            .on_click(cx.listener(|view, _, _, cx| view.open_files_tab(cx))),
                    )
                    .child(
                        Button::new("git")
                            .icon("icons/list_tree.svg")
                            .label("Git")
                            .on_click(cx.listener(|view, _, _, cx| view.open_git_tab(cx))),
                    )
                    .child(div().flex_grow())
                    .child(Button::new("settings").icon("icons/settings.svg").on_click(
                        cx.listener(|view, _, _, cx| {
                            view.show_settings = !view.show_settings;
                            cx.notify();
                        }),
                    )),
            );

        if self.show_new_session {
            sidebar = sidebar.child(self.new_session_panel(cx));
        }

        sidebar.child(list).child(
            div()
                .text_color(colors.on_subtle)
                .text_size(px(12.0))
                .pt(px(4.0))
                .border_t_1()
                .border_color(colors.stroke_neutral_subtle)
                .child(self.status.clone()),
        )
    }

    fn terminal_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors.clone();
        let active_session_name = self
            .active_session()
            .map(|s| s.name.clone())
            .unwrap_or_else(|| "No terminal".into());
        let zoom_label = format!("{}%", (self.font_size / 15.0 * 100.0).round() as i32);

        // A small visual divider between button groups.
        let divider = || {
            div()
                .w(px(1.0))
                .h(px(20.0))
                .mx(px(2.0))
                .bg(colors.stroke_neutral_subtle)
        };

        div()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .px(px(12.0))
            .py(px(8.0))
            .border_b_1()
            .border_color(colors.stroke_neutral_subtle)
            .bg(colors.surface)
            // Row 1: title + grouped action buttons.
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
                            .items_center()
                            .gap(px(4.0))
                            // ── Zoom group (#3) ──
                            .child(Button::new("zoom-out").label("A-").on_click(
                                cx.listener(|view, _, _, cx| view.zoom_active_terminal(-1.0, cx)),
                            ))
                            .child(
                                div()
                                    .w(px(48.0))
                                    .flex()
                                    .justify_center()
                                    .text_size(px(12.0))
                                    .text_color(colors.on_subtle)
                                    .child(zoom_label),
                            )
                            .child(Button::new("zoom-in").label("A+").on_click(
                                cx.listener(|view, _, _, cx| view.zoom_active_terminal(1.0, cx)),
                            ))
                            .child(Button::new("zoom-reset").label("Reset").on_click(
                                cx.listener(|view, _, _, cx| view.zoom_active_terminal(0.0, cx)),
                            ))
                            .child(divider())
                            // ── Connection group (#7) ──
                            .child(
                                Button::new("reconnect")
                                    .label("Reconnect")
                                    .appearance(ButtonAppearance::Accent)
                                    .on_click(cx.listener(|view, _, _, cx| {
                                        view.reattach_active_terminal(cx)
                                    })),
                            )
                            .child(Button::new("clear-term").label("Clear").on_click(
                                cx.listener(|view, _, _, cx| view.clear_active_terminal(cx)),
                            ))
                            .child(divider())
                            // ── Lifecycle group ──
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
            // Row 2: rename + search fields, aligned at their ends.
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_end()
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
                    .child(div().flex_grow())
                    .child(
                        div().w(px(260.0)).child(
                            Field::new(self.search_input.clone()).label("Search scrollback"),
                        ),
                    )
                    .child(
                        Button::new("search-active")
                            .label("Find")
                            .on_click(cx.listener(|view, _, _, cx| view.apply_terminal_search(cx))),
                    ),
            )
    }

    fn tab_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors.clone();
        div()
            .w_full()
            .bg(colors.tab_strip_bg)
            .child(match &self.tab_strip {
                Some(strip) => strip.clone().into_any_element(),
                None => div().h(px(0.0)).into_any_element(),
            })
    }

    fn files_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors.clone();

        // ── Directory listing: clickable rows, dirs first. (#5) ──
        let mut entries = div()
            .id("file-entries")
            .flex()
            .flex_col()
            .gap(px(1.0))
            .min_h_0()
            .flex_grow()
            .overflow_y_scroll()
            .border_1()
            .border_color(colors.stroke_neutral_subtle)
            .rounded(px(4.0))
            .p(px(4.0));

        if self.file_panel.entries.is_empty() {
            entries = entries.child(
                div()
                    .p(px(12.0))
                    .text_color(colors.on_subtle_disabled)
                    .child("Empty directory — use Upload to add files"),
            );
        } else {
            // Directories first, then files; each alphabetical as returned.
            let mut ordered: Vec<&FileEntry> = self.file_panel.entries.iter().collect();
            ordered.sort_by_key(|e| (!e.is_dir, e.path.to_lowercase()));
            for entry in ordered {
                let entry_cloned = entry.clone();
                let name = entry
                    .path
                    .rsplit('/')
                    .next()
                    .unwrap_or(&entry.path)
                    .to_string();
                let icon = if entry.is_dir {
                    "icons/folder.svg"
                } else {
                    "icons/document.svg"
                };
                entries = entries.child(
                    div()
                        .id(SharedString::from(format!("file-row-{}", entry.path)))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(8.0))
                        .w_full()
                        .px(px(8.0))
                        .py(px(5.0))
                        .rounded(px(4.0))
                        .cursor_pointer()
                        .text_color(colors.on_neutral)
                        .hover(|s| s.bg(colors.neutral_hover))
                        .on_click(
                            cx.listener(move |view, _, _, cx| view.open_entry(&entry_cloned, cx)),
                        )
                        .child(svg().path(icon).size(px(15.0)).text_color(colors.on_subtle))
                        .child(div().flex_1().min_w_0().text_ellipsis().child(name)),
                );
            }
        }

        let mut hits = div().flex().flex_col().gap(px(2.0));
        for (path, line, text) in &self.file_panel.search_hits {
            hits = hits.child(
                div()
                    .text_size(px(12.0))
                    .text_color(colors.on_subtle)
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
                            .h(px(220.0))
                            .overflow_hidden()
                            .border_1()
                            .border_color(colors.stroke_neutral_subtle)
                            .rounded(px(4.0))
                            .p(px(8.0))
                            .font_family(".ZedMono")
                            .text_size(px(12.0))
                            .text_color(colors.on_neutral)
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
                .gap(px(10.0))
                .bg(colors.surface)
                // Header: title + primary actions.
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .child(Label::new("Files").size(LabelSize::Title))
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .gap(px(6.0))
                                .child(
                                    Button::new("upload-files")
                                        .icon("icons/upload.svg")
                                        .label("Upload")
                                        .appearance(ButtonAppearance::Accent)
                                        .on_click(
                                            cx.listener(|view, _, _, cx| view.pick_and_upload(cx)),
                                        ),
                                )
                                .child(
                                    Button::new("refresh-dir")
                                        .icon("icons/arrow_forward.svg")
                                        .label("Refresh")
                                        .on_click(cx.listener(|view, _, _, cx| view.load_dir(cx))),
                                ),
                        ),
                )
                // Path bar: Up + editable directory + Go.
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .gap(px(8.0))
                        .items_end()
                        .child(
                            Button::new("dir-up")
                                .icon("icons/chevron_up.svg")
                                .label("Up")
                                .on_click(cx.listener(|view, _, _, cx| view.navigate_parent(cx))),
                        )
                        .child(div().flex_grow().child(
                            Field::new(self.file_panel.path_input.clone()).label("Directory"),
                        ))
                        .child(
                            Button::new("load-dir")
                                .label("Go")
                                .on_click(cx.listener(|view, _, _, cx| view.load_dir(cx))),
                        ),
                )
                .child(entries);

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
                    .child(div().flex_grow().child(
                        Field::new(self.file_panel.search_input.clone()).label("Search in files"),
                    ))
                    .child(
                        Button::new("search-files")
                            .icon("icons/search.svg")
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
            status_entries = status_entries.child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(colors.on_neutral)
                    .child(format!(
                        "{} on {} (+{} / -{})",
                        status.repo, status.branch, status.ahead, status.behind
                    )),
            );
            for entry in &status.entries {
                status_entries = status_entries.child(
                    div()
                        .font_family(".ZedMono")
                        .text_size(px(12.0))
                        .text_color(colors.on_neutral)
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
                    .text_color(colors.on_subtle)
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
        let collapsed = self.sidebar_collapsed;
        let sidebar: gpui::AnyElement = if collapsed {
            self.sidebar_rail(cx).into_any_element()
        } else {
            self.sidebar(cx).into_any_element()
        };

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

        // Drag handle between the (expanded) sidebar and the main content. A
        // 6px hit strip that updates the sidebar width as the pointer moves. (#8)
        let resize_handle: Option<gpui::AnyElement> = if collapsed {
            None
        } else {
            Some(
                div()
                    .id("sidebar-resize")
                    .w(px(6.0))
                    .h_full()
                    .cursor_col_resize()
                    .bg(if self.sidebar_dragging {
                        colors.stroke_neutral
                    } else {
                        colors.stroke_neutral_subtle
                    })
                    .hover(|s| s.bg(colors.stroke_neutral))
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        cx.listener(|view, _ev: &gpui::MouseDownEvent, _, cx| {
                            view.sidebar_dragging = true;
                            cx.notify();
                        }),
                    )
                    .into_any_element(),
            )
        };

        let mut body = div().flex().flex_row().size_full().bg(colors.surface);
        body = body.child(sidebar);
        if let Some(handle) = resize_handle {
            body = body.child(handle);
        }
        body = body.child(div().flex_grow().min_w_0().h_full().child(main));

        let root = div()
            .size_full()
            .flex()
            .flex_col()
            // While dragging the sidebar handle, track the pointer and commit
            // the width on release. Mouse handlers live on the root so the drag
            // continues even when the pointer leaves the thin handle strip.
            .on_mouse_move(cx.listener(|view, ev: &gpui::MouseMoveEvent, _, cx| {
                if view.sidebar_dragging {
                    let x = f32::from(ev.position.x);
                    #[cfg(target_os = "windows")]
                    let x = x; // titlebar is above the body; no horizontal offset
                    view.set_sidebar_width(x, cx);
                }
            }))
            .on_mouse_up(
                gpui::MouseButton::Left,
                cx.listener(|view, _ev: &gpui::MouseUpEvent, _, cx| {
                    if view.sidebar_dragging {
                        view.sidebar_dragging = false;
                        view.persist_layout(cx);
                        cx.notify();
                    }
                }),
            );
        #[cfg(target_os = "windows")]
        let root = root.child(self.windows_title_bar(cx));

        root.child(div().flex_grow().min_h_0().child(body))
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
