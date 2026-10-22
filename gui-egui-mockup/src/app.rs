//! The app shell: sign-in gate, top bar, navigation pane, command palette (Ctrl+K), shortcut
//! overlay (`?`), status bar and theme. Each screen lives in [`crate::screens`].

use fluent2_egui::IconName;
use fluent2_egui::controls::{Appearance, Avatar, Badge, BadgeAppearance, BadgeColor, Button};
use fluent2_egui::data::nav_view::{NavState, NavigationView};
use fluent2_egui::surfaces::{
    CommandPalette, Dialog, Intent, PaletteCommand, Toast, ToastPosition, Toaster, TooltipExt,
};
use fluent2_egui::theme::{Density, Theme};

use crate::fixtures::Fixtures;
use crate::screens::{self, Screens};

/// A top-level place in the navigation pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    /// Session list and the open session.
    Sessions,
    /// Workers and pending decisions.
    Oversight,
    /// The ranked backlog.
    Backlog,
    /// Kanban by state.
    Board,
    /// One work item. Opened from the backlog, the board or the palette.
    WorkItem,
    /// A branch's diff.
    Git,
    /// Notifications.
    Inbox,
    /// Registered projects.
    Projects,
    /// Theme, density and account.
    Settings,
}

impl Screen {
    /// Every screen, in navigation order.
    pub const ALL: [Self; 9] = [
        Self::Sessions,
        Self::Oversight,
        Self::Backlog,
        Self::Board,
        Self::WorkItem,
        Self::Git,
        Self::Inbox,
        Self::Projects,
        Self::Settings,
    ];

    /// Navigation key.
    pub fn key(self) -> &'static str {
        match self {
            Self::Sessions => "sessions",
            Self::Oversight => "oversight",
            Self::Backlog => "backlog",
            Self::Board => "board",
            Self::WorkItem => "work",
            Self::Git => "git",
            Self::Inbox => "inbox",
            Self::Projects => "projects",
            Self::Settings => "settings",
        }
    }

    /// Title, as the navigation pane and the palette show it.
    pub fn title(self) -> &'static str {
        match self {
            Self::Sessions => "Sessions",
            Self::Oversight => "Oversight",
            Self::Backlog => "Backlog",
            Self::Board => "Board",
            Self::WorkItem => "Work item",
            Self::Git => "Git",
            Self::Inbox => "Inbox",
            Self::Projects => "Projects",
            Self::Settings => "Settings",
        }
    }

    fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|screen| screen.key() == key)
    }
}

/// Light or dark, as Settings offers it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Look {
    /// Fluent light.
    Light,
    /// Fluent dark.
    Dark,
    /// High contrast.
    HighContrast,
}

/// The sign-in form.
#[derive(Default)]
pub struct LoginForm {
    /// Engine address.
    pub server: String,
    /// Username.
    pub username: String,
    /// Password. Never stored; this is a mockup.
    pub password: String,
    /// Keep me signed in.
    pub remember: bool,
}

/// The whole mockup. [`MockupApp::ui`] draws one frame into any `Ui`, so the tests and the
/// screenshot run drive it without eframe.
pub struct MockupApp {
    /// Fake deployment.
    pub data: Fixtures,
    /// Signed in past the login screen.
    pub signed_in: bool,
    /// The sign-in form.
    pub login: LoginForm,
    /// The current screen.
    pub screen: Screen,
    /// Light, dark or high contrast.
    pub look: Look,
    /// Compact, medium or touch.
    pub density: Density,
    /// The command palette is open.
    pub palette_open: bool,
    /// The shortcut overlay is open.
    pub shortcuts_open: bool,
    /// Per-screen state.
    pub screens: Screens,
    nav: NavState,
    recents: Vec<String>,
    installed: Option<(Look, Density)>,
}

impl Default for MockupApp {
    fn default() -> Self {
        Self {
            data: Fixtures::default(),
            signed_in: false,
            login: LoginForm {
                server: "https://vogt-dev.sprooty.com".to_owned(),
                username: "sprooty".to_owned(),
                password: String::new(),
                remember: true,
            },
            screen: Screen::Sessions,
            look: Look::Light,
            density: Density::Medium,
            palette_open: false,
            shortcuts_open: false,
            screens: Screens::default(),
            nav: NavState {
                selected: Some(Screen::Sessions.key().to_owned()),
                ..NavState::default()
            },
            recents: Vec::new(),
            installed: None,
        }
    }
}

impl MockupApp {
    /// Signed in, on `screen`, in `look`. For tests and screenshots.
    pub fn at(screen: Screen, look: Look) -> Self {
        let mut app = Self {
            signed_in: true,
            look,
            ..Self::default()
        };
        app.go(screen);
        app
    }

    /// Go to `screen`.
    pub fn go(&mut self, screen: Screen) {
        self.screen = screen;
        // The work-item page has no entry of its own; keep the place it was opened from lit.
        if screen != Screen::WorkItem {
            self.nav.selected = Some(screen.key().to_owned());
        }
    }

    /// Install the theme for `look` and `density` if either changed since the last frame.
    fn apply_theme(&mut self, ctx: &egui::Context) {
        if self.installed == Some((self.look, self.density)) {
            return;
        }
        let theme = match self.look {
            Look::Light => Theme::default(),
            Look::Dark => Theme::dark(fluent2_egui::tokens::FLUENT),
            Look::HighContrast => Theme::high_contrast(),
        }
        .with_density(self.density);
        fluent2_egui::install(ctx, &theme);
        self.installed = Some((self.look, self.density));
        ctx.request_repaint();
    }

    /// Draw one frame.
    pub fn ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let first = self.installed.is_none();
        self.apply_theme(&ctx);
        if first {
            // The Fluent text styles take effect from the next frame.
            return;
        }
        if !self.signed_in {
            egui::CentralPanel::default().show(ui, |ui| {
                if screens::login::show(ui, &mut self.login) {
                    self.signed_in = true;
                    self.go(Screen::Sessions);
                }
            });
            Toaster::show(&ctx, ToastPosition::BottomEnd);
            return;
        }
        self.keys(&ctx);
        self.top_bar(ui);
        self.status_bar(ui);
        egui::CentralPanel::default()
            .frame(egui::Frame::central_panel(ui.style()).inner_margin(0.0))
            .show(ui, |ui| self.body(ui));
        self.palette(&ctx);
        self.shortcuts(&ctx);
        Toaster::show(&ctx, ToastPosition::BottomEnd);
    }

    fn keys(&mut self, ctx: &egui::Context) {
        if ctx.input_mut(|input| input.consume_key(egui::Modifiers::COMMAND, egui::Key::K)) {
            self.palette_open = !self.palette_open;
        }
        let typing = ctx.egui_wants_keyboard_input();
        let question = ctx.input(|input| {
            input
                .events
                .iter()
                .any(|event| matches!(event, egui::Event::Text(text) if text == "?"))
        });
        if question && !typing && !self.palette_open {
            self.shortcuts_open = true;
        }
    }

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top("top_bar").show(ui, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Vogt").strong().size(18.0));
                ui.add(
                    Badge::new("vogt-dev")
                        .appearance(BadgeAppearance::Tint)
                        .color(BadgeColor::Brand),
                );
                ui.add(
                    Badge::new("Mockup · fake data")
                        .appearance(BadgeAppearance::Outline)
                        .color(BadgeColor::Warning),
                );
                ui.add_space(16.0);
                let search = ui
                    .add(
                        Button::new("Search or run a command…   Ctrl+K")
                            .icon(IconName::Search)
                            .appearance(Appearance::Outline),
                    )
                    .fluent_tooltip("Command palette");
                if search.clicked() {
                    self.palette_open = true;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add(Avatar::new("Sprooty").size(28.0));
                    let (icon, tip, next) = match self.look {
                        Look::Light => (IconName::WeatherMoon, "Dark theme", Look::Dark),
                        _ => (IconName::WeatherSunny, "Light theme", Look::Light),
                    };
                    if ui
                        .add(Button::icon_only(icon, tip).appearance(Appearance::Subtle))
                        .fluent_tooltip(tip)
                        .clicked()
                    {
                        self.look = next;
                    }
                    if ui
                        .add(
                            Button::icon_only(IconName::QuestionCircle, "Keyboard shortcuts")
                                .appearance(Appearance::Subtle),
                        )
                        .fluent_tooltip("Keyboard shortcuts (?)")
                        .clicked()
                    {
                        self.shortcuts_open = true;
                    }
                    let unread = self.data.inbox.iter().filter(|entry| entry.unread).count();
                    if unread > 0 {
                        ui.add(Badge::new(unread.to_string()).color(BadgeColor::Danger));
                    }
                    if ui
                        .add(
                            Button::icon_only(IconName::Mail, "Inbox")
                                .appearance(Appearance::Subtle),
                        )
                        .fluent_tooltip("Inbox")
                        .clicked()
                    {
                        self.go(Screen::Inbox);
                    }
                });
            });
            ui.add_space(4.0);
        });
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::bottom("status_bar").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.add(
                    Badge::new("●")
                        .appearance(BadgeAppearance::Ghost)
                        .color(BadgeColor::Success),
                );
                ui.small("Engine connected · vogt-dev v0.7.22");
                ui.separator();
                let running = self
                    .data
                    .sessions
                    .iter()
                    .filter(|s| s.state == crate::fixtures::SessionState::Running)
                    .count();
                ui.small(format!(
                    "{} sessions · {running} running · {} decisions waiting",
                    self.data.sessions.len(),
                    self.data.decisions.len()
                ));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.small("Press ? for shortcuts");
                });
            });
        });
    }

    fn body(&mut self, ui: &mut egui::Ui) {
        let inbox = self.data.inbox.iter().filter(|entry| entry.unread).count();
        let needs = self
            .data
            .sessions
            .iter()
            .filter(|s| s.state == crate::fixtures::SessionState::NeedsYou)
            .count();
        let response = NavigationView::new("main_nav")
            .title("Places")
            .item(
                Screen::Sessions.key(),
                IconName::Window,
                format!("Sessions ({})", self.data.sessions.len()),
            )
            .item(
                Screen::Oversight.key(),
                IconName::Navigation,
                format!("Oversight ({needs} need you)"),
            )
            .section("Work")
            .item(Screen::Backlog.key(), IconName::Table, "Backlog")
            .item(Screen::Board.key(), IconName::Grid, "Board")
            .item(
                Screen::Inbox.key(),
                IconName::Mail,
                format!("Inbox ({inbox})"),
            )
            .item(Screen::Projects.key(), IconName::Folder, "Projects")
            .section("Code")
            .item(Screen::Git.key(), IconName::SquareMultiple, "Git")
            .footer_item(Screen::Settings.key(), IconName::Settings, "Settings")
            .show(ui, &mut self.nav, |ui| {
                egui::Frame::NONE
                    .inner_margin(16.0)
                    .show(ui, |ui| {
                        ui.set_min_size(ui.available_size());
                        screens::show(ui, self.screen, &mut self.screens, &mut self.data)
                    })
                    .inner
            });
        if let Some(key) = response.selected
            && let Some(screen) = Screen::from_key(key)
        {
            self.go(screen);
        }
        if let Some(action) = response.inner {
            self.act(action);
        }
    }

    /// Run something a screen asked for.
    fn act(&mut self, action: screens::Action) {
        match action {
            screens::Action::OpenWork(reference) => {
                self.screens.work.reference = reference;
                self.go(Screen::WorkItem);
            }
            screens::Action::OpenSession(id) => {
                self.screens.sessions.selected = id;
                self.go(Screen::Sessions);
            }
            screens::Action::Go(screen) => self.go(screen),
            screens::Action::SetLook(look) => self.look = look,
            screens::Action::SetDensity(density) => self.density = density,
            screens::Action::SignOut => {
                self.signed_in = false;
                self.login.password.clear();
            }
        }
    }

    fn commands(&self) -> Vec<PaletteCommand> {
        let mut commands: Vec<PaletteCommand> = [
            (Screen::Sessions, IconName::Window, "G S"),
            (Screen::Oversight, IconName::Navigation, "G O"),
            (Screen::Backlog, IconName::Table, "G B"),
            (Screen::Board, IconName::Grid, "G K"),
            (Screen::Git, IconName::SquareMultiple, "G D"),
            (Screen::Inbox, IconName::Mail, "G I"),
            (Screen::Projects, IconName::Folder, "G P"),
            (Screen::Settings, IconName::Settings, "Ctrl+,"),
        ]
        .into_iter()
        .map(|(screen, icon, keys)| {
            PaletteCommand::new(
                format!("go.{}", screen.key()),
                format!("Go to {}", screen.title()),
            )
            .group("Places")
            .icon(icon)
            .shortcut(keys)
        })
        .collect();
        for item in self.data.work.iter().take(8) {
            commands.push(
                PaletteCommand::new(
                    format!("work.{}", item.reference),
                    format!("{} {}", item.reference, item.title),
                )
                .group("Work items")
                .icon(IconName::Document)
                .subtitle(format!(
                    "{} · {} · {}",
                    item.project,
                    item.priority,
                    item.state.label()
                )),
            );
        }
        for session in &self.data.sessions {
            commands.push(
                PaletteCommand::new(
                    format!("session.{}", session.id),
                    format!("Open session {}", session.name),
                )
                .group("Sessions")
                .icon(IconName::Window)
                .subtitle(format!(
                    "{} · {} · {}",
                    session.agent,
                    session.model,
                    session.state.label()
                )),
            );
        }
        commands.extend([
            PaletteCommand::new("session.new", "New terminal session")
                .group("Actions")
                .icon(IconName::Add)
                .shortcut("Ctrl+Shift+T"),
            PaletteCommand::new("theme.light", "Use the light theme")
                .group("Appearance")
                .icon(IconName::WeatherSunny)
                .keyword("appearance"),
            PaletteCommand::new("theme.dark", "Use the dark theme")
                .group("Appearance")
                .icon(IconName::WeatherMoon)
                .keyword("appearance"),
            PaletteCommand::new("density.compact", "Compact density").group("Appearance"),
            PaletteCommand::new("density.medium", "Medium density").group("Appearance"),
            PaletteCommand::new("help.shortcuts", "Keyboard shortcuts")
                .group("Help")
                .icon(IconName::QuestionCircle)
                .shortcut("?"),
            PaletteCommand::new("account.signout", "Sign out")
                .group("Account")
                .icon(IconName::LockClosed),
        ]);
        commands
    }

    fn palette(&mut self, ctx: &egui::Context) {
        if !self.palette_open {
            return;
        }
        let commands = self.commands();
        let Some(id) = CommandPalette::new("vogt_palette", &commands)
            .recents(&self.recents)
            .show(ctx, &mut self.palette_open)
        else {
            return;
        };
        self.recents.retain(|recent| recent != &id);
        self.recents.insert(0, id.clone());
        self.recents.truncate(5);
        if let Some(key) = id.strip_prefix("go.") {
            if let Some(screen) = Screen::from_key(key) {
                self.go(screen);
            }
        } else if let Some(reference) = id.strip_prefix("work.") {
            self.act(screens::Action::OpenWork(reference.to_owned()));
        } else if let Some(session) = id.strip_prefix("session.")
            && session != "new"
        {
            self.act(screens::Action::OpenSession(session.to_owned()));
        } else {
            match id.as_str() {
                "theme.light" => self.look = Look::Light,
                "theme.dark" => self.look = Look::Dark,
                "density.compact" => self.density = Density::Compact,
                "density.medium" => self.density = Density::Medium,
                "help.shortcuts" => self.shortcuts_open = true,
                "account.signout" => self.act(screens::Action::SignOut),
                _ => {
                    Toaster::push(
                        ctx,
                        Toast {
                            intent: Intent::Info,
                            title: "Mockup".to_owned(),
                            body: Some("Nothing runs here: there is no backend.".to_owned()),
                            timeout: None,
                            action: None,
                        },
                    );
                }
            }
        }
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        if !self.shortcuts_open {
            return;
        }
        let _ = Dialog::new("shortcut_overlay")
            .title("Keyboard shortcuts")
            .actions(|actions| actions.primary("Close"))
            .show(ctx, &mut self.shortcuts_open, |ui| {
                for (group, rows) in SHORTCUTS {
                    ui.add_space(6.0);
                    ui.strong(*group);
                    egui::Grid::new(("shortcuts", *group))
                        .num_columns(2)
                        .spacing([24.0, 6.0])
                        .show(ui, |ui| {
                            for (keys, what) in *rows {
                                ui.horizontal(|ui| {
                                    for key in keys.split('+') {
                                        ui.add(
                                            Badge::new(key)
                                                .appearance(BadgeAppearance::Outline)
                                                .color(BadgeColor::Informative),
                                        );
                                    }
                                });
                                ui.label(*what);
                                ui.end_row();
                            }
                        });
                }
            });
    }
}

/// The shortcut overlay's content, after the web GUI's `keyboardShortcuts.ts`.
const SHORTCUTS: &[(&str, &[(&str, &str)])] = &[
    (
        "Anywhere",
        &[
            ("Ctrl+K", "Open the command palette"),
            ("?", "Show this overlay"),
            ("Ctrl+Shift+T", "New terminal session"),
            ("Ctrl+Shift+W", "Close the active tab"),
            ("Ctrl+Alt+←", "Previous tab"),
            ("Ctrl+Alt+→", "Next tab"),
        ],
    ),
    (
        "Terminal",
        &[
            ("Ctrl+Shift+C", "Copy the selection"),
            ("Ctrl+Shift+V", "Paste"),
            ("Ctrl+Shift+F", "Find in the terminal buffer"),
            ("Ctrl+C", "Copy with a selection, otherwise interrupt"),
        ],
    ),
    (
        "Editor",
        &[
            ("Ctrl+S", "Save file"),
            ("Ctrl+F", "Find"),
            ("Ctrl+G", "Go to line"),
        ],
    ),
];

impl eframe::App for MockupApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        MockupApp::ui(self, ui);
    }
}
