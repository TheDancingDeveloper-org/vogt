//! The egui shell: draws the [`Model`] with fluent2-egui and feeds what the
//! user does back into it.

use egui::{Align, Layout, RichText, Ui};
use fluent2_egui::controls::{Appearance, Button};
use fluent2_egui::data::markdown::{Document, LinkTarget, MarkdownView, default_link_policy};
use fluent2_egui::{Badge, BadgeAppearance, BadgeColor, Card, Divider, Field, Input, Theme};
use web_time::Instant;

use crate::backend::Backend;
use crate::model::{self, Command, Model, Screen, StreamState, WorkView};

/// Parsed markdown for the loaded work item, so it is not re-parsed each frame.
struct Docs {
    key: (String, String),
    body: Document,
    comments: Vec<Document>,
}

/// The application.
pub struct VogtApp {
    /// App state.
    pub model: Model,
    backend: Box<dyn Backend>,
    docs: Option<Docs>,
    theme_installed: bool,
}

/// Storage key for the last API base (never the token).
const BASE_KEY: &str = "vogt.base";

impl VogtApp {
    /// An app on `backend`, with the login form starting at `base`.
    pub fn new(backend: Box<dyn Backend>, base: impl Into<String>) -> Self {
        Self {
            model: Model::new(base),
            backend,
            docs: None,
            theme_installed: false,
        }
    }

    /// The app as eframe creates it: the real HTTP backend, the last base from
    /// storage (or `default_base`), and the Fluent theme.
    pub fn from_creation_context(cc: &eframe::CreationContext<'_>, default_base: String) -> Self {
        let ctx = cc.egui_ctx.clone();
        let backend = crate::backend::HttpBackend::new(move || ctx.request_repaint());
        let base = cc.storage.and_then(|s| s.get_string(BASE_KEY)).unwrap_or(default_base);
        Self::new(Box::new(backend), base)
    }

    fn run(&mut self, commands: Vec<Command>) {
        model::run(commands, &self.model, self.backend.as_mut());
    }

    /// One frame: take replies, advance time, draw.
    pub fn frame(&mut self, ui: &mut Ui) {
        if !self.theme_installed {
            fluent2_egui::install(ui.ctx(), &Theme::default());
            self.theme_installed = true;
        }
        let now = Instant::now();
        for reply in self.backend.poll() {
            let commands = self.model.apply(reply, now);
            self.run(commands);
        }
        let commands = self.model.tick(now);
        self.run(commands);
        if let Some(wait) = self.model.next_wake(now) {
            ui.ctx().request_repaint_after(wait);
        }

        match self.model.screen {
            Screen::Login => self.login_screen(ui),
            _ => {
                egui::Panel::top("vogt-top").show(ui, |ui| self.top_bar(ui, now));
                egui::CentralPanel::default().show(ui, |ui| match self.model.screen {
                    Screen::Sessions => self.sessions_screen(ui),
                    _ => self.work_item_screen(ui),
                });
            }
        }
    }

    fn login_screen(&mut self, ui: &mut Ui) {
        let mut commands = Vec::new();
        ui.vertical_centered(|ui| {
            ui.set_max_width(360.0);
            ui.add_space(48.0);
            ui.heading("Sign in to Vogt");
            ui.add_space(16.0);
            let form = &mut self.model.login;
            Field::new("API address").show(ui, |ui| {
                ui.add(
                    Input::new(&mut form.base)
                        .label("API address")
                        .placeholder("https://vogt-dev.example"),
                )
            });
            Field::new("Username").show(ui, |ui| ui.add(Input::new(&mut form.username).label("Username")));
            let password = Field::new("Password").show(ui, |ui| {
                ui.add(Input::new(&mut form.password).label("Password").password())
            });
            let enter = password.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            ui.add_space(8.0);
            let label = if form.busy { "Signing in…" } else { "Sign in" };
            let clicked = ui
                .add_enabled(!form.busy, Button::new(label).appearance(Appearance::Primary))
                .clicked();
            if (clicked || enter) && !form.busy {
                commands = self.model.submit_password();
            }
            ui.add_space(16.0);
            ui.add(Divider::horizontal());
            ui.add_space(8.0);
            let form = &mut self.model.login;
            Field::new("Or paste a token").show(ui, |ui| ui.add(Input::new(&mut form.token).label("Token").password()));
            if ui.add(Button::new("Use token")).clicked() {
                commands = self.model.submit_token();
            }
            if let Some(error) = &self.model.login.error {
                ui.add_space(8.0);
                ui.colored_label(ui.visuals().error_fg_color, error);
            }
        });
        self.run(commands);
    }

    fn top_bar(&mut self, ui: &mut Ui, now: Instant) {
        let mut commands = Vec::new();
        ui.horizontal(|ui| {
            ui.strong("Vogt");
            ui.add_space(12.0);
            let sessions = Button::new("Sessions").appearance(if self.model.screen == Screen::Sessions {
                Appearance::Primary
            } else {
                Appearance::Subtle
            });
            if ui.add(sessions).clicked() {
                self.model.show_sessions();
            }
            let work = Button::new("Work item").appearance(if self.model.screen == Screen::WorkItem {
                Appearance::Primary
            } else {
                Appearance::Subtle
            });
            if ui.add(work).clicked() {
                self.model.screen = Screen::WorkItem;
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.add(Button::new("Sign out").appearance(Appearance::Subtle)).clicked() {
                    commands = self.model.sign_out(None);
                }
                ui.label(&self.model.actor);
                let (text, color) = stream_badge(self.model.stream, now);
                ui.add(Badge::new(text).appearance(BadgeAppearance::Tint).color(color));
            });
        });
        self.run(commands);
    }

    fn sessions_screen(&mut self, ui: &mut Ui) {
        let mut commands = Vec::new();
        ui.horizontal(|ui| {
            ui.heading("Sessions");
            ui.label(format!("{}", self.model.sessions.len()));
            if ui.add(Button::new("Refresh").appearance(Appearance::Subtle)).clicked() {
                commands = self.model.refresh_sessions();
            }
            if self.model.sessions_loading {
                ui.spinner();
            }
        });
        if let Some(error) = &self.model.sessions_error {
            ui.colored_label(ui.visuals().error_fg_color, format!("Could not read sessions: {error}"));
        }
        if self.model.sessions.is_empty() && !self.model.sessions_loading {
            ui.label("No sessions.");
        }
        let mut open_ref = None;
        egui::ScrollArea::vertical().show(ui, |ui| {
            for session in &self.model.sessions {
                ui.horizontal(|ui| {
                    ui.add(
                        Badge::new(session.activity.as_str())
                            .appearance(BadgeAppearance::Tint)
                            .color(activity_color(&session.activity)),
                    );
                    ui.label(RichText::new(&session.name).strong());
                    if let Some(code) = session.exit_code {
                        ui.weak(format!("exit {code}"));
                    }
                    ui.weak(&session.cwd);
                    if let Some(reference) = work_ref_in(&session.name)
                        && ui
                            .add(Button::new(format!("Open {reference}")).appearance(Appearance::Subtle))
                            .clicked()
                    {
                        open_ref = Some(reference);
                    }
                });
                ui.separator();
            }
        });
        if let Some(reference) = open_ref {
            commands = self.model.open_work_item(&reference);
        }
        self.run(commands);
    }

    fn work_item_screen(&mut self, ui: &mut Ui) {
        let mut commands = Vec::new();
        ui.horizontal(|ui| {
            let input = ui.add(
                Input::new(&mut self.model.work_ref)
                    .label("Work item")
                    .placeholder("WI-123")
                    .desired_width(160.0),
            );
            let enter = input.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if ui.add(Button::new("Open").appearance(Appearance::Primary)).clicked() || enter {
                let reference = self.model.work_ref.clone();
                commands = self.model.open_work_item(&reference);
            }
        });
        ui.add_space(8.0);
        match &self.model.work {
            WorkView::Empty => {
                ui.label("Type a work item reference to read it.");
            }
            WorkView::Loading(reference) => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(format!("Loading {reference}…"));
                });
            }
            WorkView::Failed(reference, error) => {
                ui.colored_label(
                    ui.visuals().error_fg_color,
                    format!("Could not read {reference}: {error}"),
                );
            }
            WorkView::Loaded(detail) => {
                let key = (detail.item.reference.clone(), detail.item.updated_at.clone());
                if self
                    .docs
                    .as_ref()
                    .is_none_or(|d| d.key != key || d.comments.len() != detail.comments.len())
                {
                    self.docs = Some(Docs {
                        key,
                        body: Document::parse(&detail.item.body),
                        comments: detail.comments.iter().map(|c| Document::parse(&c.body)).collect(),
                    });
                }
                let Some(docs) = &self.docs else { return };
                let item = &detail.item;
                let mut open = None;
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.heading(format!("{} {}", item.reference, item.title));
                    ui.horizontal_wrapped(|ui| {
                        for (text, color) in [
                            (item.state.as_str(), BadgeColor::Brand),
                            (item.priority.as_str(), BadgeColor::Informative),
                            (item.kind.as_str(), BadgeColor::Subtle),
                        ] {
                            ui.add(Badge::new(text).appearance(BadgeAppearance::Tint).color(color));
                        }
                        if let Some(project) = &item.project_slug {
                            ui.weak(project);
                        }
                        for label in &item.labels {
                            ui.add(Badge::new(label.as_str()).appearance(BadgeAppearance::Outline));
                        }
                    });
                    ui.add_space(8.0);
                    let out = MarkdownView::new("work-body", &docs.body)
                        .link_policy(&default_link_policy)
                        .show(ui);
                    open = open.take().or(out.clicked);
                    if !detail.comments.is_empty() {
                        ui.add_space(12.0);
                        ui.strong(format!("Comments ({})", detail.comments.len()));
                        for (i, (comment, doc)) in detail.comments.iter().zip(&docs.comments).enumerate() {
                            Card::new().show(ui, |ui| {
                                ui.weak(&comment.created_at);
                                let out = MarkdownView::new(("comment", i), doc)
                                    .link_policy(&default_link_policy)
                                    .show(ui);
                                open = open.take().or(out.clicked);
                            });
                        }
                    }
                });
                if let Some(LinkTarget::Web(url)) = open.map(|c| c.target) {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(url));
                }
            }
        }
        self.run(commands);
    }
}

impl eframe::App for VogtApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.frame(ui);
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        storage.set_string(BASE_KEY, self.model.login.base.clone());
    }
}

/// The text and colour of the live-stream badge.
pub fn stream_badge(state: StreamState, now: Instant) -> (String, BadgeColor) {
    match state {
        StreamState::Off => ("Offline".into(), BadgeColor::Subtle),
        StreamState::Connecting => ("Connecting".into(), BadgeColor::Informative),
        StreamState::Live => ("Live".into(), BadgeColor::Success),
        StreamState::Waiting { retry_at, .. } => {
            let secs = retry_at.saturating_duration_since(now).as_secs_f32().ceil() as u64;
            (format!("Reconnecting in {secs}s"), BadgeColor::Warning)
        }
    }
}

fn activity_color(activity: &str) -> BadgeColor {
    match activity {
        "running" | "starting" => BadgeColor::Brand,
        "awaiting-approval" | "blocked" => BadgeColor::Warning,
        "exited" => BadgeColor::Subtle,
        _ => BadgeColor::Informative,
    }
}

/// A work-item reference at the start of a session name (`WI-1806 fix` → `WI-1806`).
pub fn work_ref_in(name: &str) -> Option<String> {
    let head = name.split(|c: char| c.is_whitespace() || c == ':').next()?;
    let digits = head.strip_prefix("WI-")?;
    (!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())).then(|| head.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_work_refs_in_session_names() {
        assert_eq!(work_ref_in("WI-1806").as_deref(), Some("WI-1806"));
        assert_eq!(work_ref_in("WI-12: fix").as_deref(), Some("WI-12"));
        assert_eq!(work_ref_in("oversight"), None);
        assert_eq!(work_ref_in("WI-"), None);
        assert_eq!(work_ref_in("WI-1a"), None);
    }

    #[test]
    fn waiting_badge_counts_down() {
        let now = Instant::now();
        let state = StreamState::Waiting {
            retry_at: now + std::time::Duration::from_millis(1500),
            attempt: 1,
        };
        assert_eq!(stream_badge(state, now).0, "Reconnecting in 2s");
    }
}
