//! GPUI application shell: server connection, session list sidebar, and the
//! active terminal pane. Built on FluentGUI (Fluent 2 design over a GPUI fork),
//! the same stack as the in-house `rdpapp`.

mod terminal_view;

use fluent_app::FluentApp;
use fluent_core::ThemeProvider as _;
use fluent_primitives::{Button, ButtonAppearance, Field, Label, LabelSize, TextInput};
use gpui::{
    div, prelude::*, px, ClickEvent, Context, Entity, FontWeight, IntoElement, Render,
    SharedString, Window,
};
use uuid::Uuid;

use mydevenv2_client::{
    bridge,
    client::ApiClient,
    config::ClientConfig,
    protocol::{SessionSpec, SessionSummary},
};

use terminal_view::TerminalView;

/// Launch the desktop client. Blocks on the GPUI event loop until the window
/// closes.
pub fn run(cfg: ClientConfig) {
    FluentApp::new("MyDevEnv2")
        .window_size(1200.0, 800.0)
        .window_min_size(720.0, 480.0)
        .app_id("com.sprooty.mydevenv2-client")
        .dark_theme()
        .run(move |cx| cx.new(|cx| RootView::new(cfg, cx)));
}

struct RootView {
    api: ApiClient,
    configured: bool,
    sessions: Vec<SessionSummary>,
    status: SharedString,
    active: Option<(Uuid, Entity<TerminalView>)>,
    url_input: Entity<TextInput>,
    token_input: Entity<TextInput>,
    show_settings: bool,
}

impl RootView {
    fn new(cfg: ClientConfig, cx: &mut Context<Self>) -> Self {
        let api = ApiClient::new(cfg.base(), cfg.token.clone());
        let configured = cfg.is_configured();
        let status: SharedString = if configured {
            "Connecting…".into()
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
        let view = Self {
            api,
            configured,
            sessions: Vec::new(),
            status,
            active: None,
            url_input,
            token_input,
            // Open straight to settings until the server is configured.
            show_settings: !configured,
        };
        if configured {
            view.refresh_sessions(cx);
        }
        view
    }

    /// Read the settings inputs, persist config, and reconnect.
    fn save_settings(&mut self, cx: &mut Context<Self>) {
        let server_url = self.url_input.read(cx).text().trim().to_string();
        let token = self.token_input.read(cx).text().trim().to_string();
        let cfg = ClientConfig { server_url, token };
        if let Err(e) = cfg.save() {
            self.status = SharedString::from(format!("save failed: {e}"));
            cx.notify();
            return;
        }
        self.api = ApiClient::new(cfg.base(), cfg.token.clone());
        self.configured = cfg.is_configured();
        self.active = None;
        self.show_settings = false;
        if self.configured {
            self.status = "Connecting…".into();
            self.refresh_sessions(cx);
        } else {
            self.status = "Enter the server URL and API token".into();
            self.show_settings = true;
        }
        cx.notify();
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
                    .w(px(440.0))
                    .flex()
                    .flex_col()
                    .gap(px(12.0))
                    .child(Field::new(self.url_input.clone()).label("Server URL"))
                    .child(Field::new(self.token_input.clone()).label("API token"))
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

    /// Fetch the session list on the tokio runtime; update on completion.
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

    /// Create a fresh shell session, then attach to it.
    fn create_session(&mut self, cx: &mut Context<Self>) {
        let api = self.api.clone();
        let handle = bridge::handle();
        let spec = SessionSpec {
            name: "terminal".into(),
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

    /// Open (or switch to) a terminal attached to `id`.
    fn attach(&mut self, id: Uuid, cx: &mut Context<Self>) {
        if self.active.as_ref().map(|(a, _)| *a) == Some(id) {
            return;
        }
        let ws_url = self.api.attach_ws_url(id);
        let token = self.api.token().to_string();
        let term = cx.new(|cx| TerminalView::new(ws_url, token, id, cx));
        self.active = Some((id, term));
        self.status = SharedString::from(format!("attached {id}"));
        cx.notify();
    }

    fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors.clone();
        let active_id = self.active.as_ref().map(|(a, _)| *a);

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

        div()
            .flex()
            .flex_col()
            .h_full()
            .w(px(260.0))
            .p(px(12.0))
            .gap(px(10.0))
            .bg(colors.surface)
            .child(Label::new("MyDevEnv2").size(LabelSize::Title))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(6.0))
                    .child(
                        Button::new("new-session")
                            .label("New")
                            .appearance(ButtonAppearance::Accent)
                            .on_click(cx.listener(|view, _, _, cx| view.create_session(cx))),
                    )
                    .child(
                        Button::new("refresh")
                            .label("Refresh")
                            .on_click(cx.listener(|view, _, _, cx| view.refresh_sessions(cx))),
                    )
                    .child(
                        Button::new("settings")
                            .label("Settings")
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.show_settings = !view.show_settings;
                                cx.notify();
                            })),
                    ),
            )
            .child(list)
            .child(div().flex_grow())
            .child(
                div()
                    .text_color(colors.on_subtle)
                    .text_size(px(12.0))
                    .child(self.status.clone()),
            )
    }
}

impl Render for RootView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors.clone();
        let sidebar = self.sidebar(cx);

        let main: gpui::AnyElement = if self.show_settings {
            self.settings_panel(cx).into_any_element()
        } else {
            match &self.active {
                Some((_, term)) => div().size_full().child(term.clone()).into_any_element(),
                None => div()
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
                    .into_any_element(),
            }
        };

        div()
            .flex()
            .flex_row()
            .size_full()
            .bg(colors.surface)
            .child(sidebar)
            .child(div().flex_grow().h_full().child(main))
    }
}
