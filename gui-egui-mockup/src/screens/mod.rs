//! One module per screen, plus the small pieces they share.

pub mod backlog;
pub mod board;
pub mod git;
pub mod inbox;
pub mod login;
pub mod oversight;
pub mod projects;
pub mod sessions;
pub mod settings;
pub mod work_item;

use fluent2_egui::controls::{Badge, BadgeAppearance, BadgeColor};
use fluent2_egui::theme::{Density, Theme, c};

use crate::app::{Look, Screen};
use crate::fixtures::{Fixtures, SessionState, WorkState};

/// Something a screen asks the shell to do.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// Open the work item with this reference.
    OpenWork(String),
    /// Open the session with this id.
    OpenSession(String),
    /// Go to a screen.
    Go(Screen),
    /// Switch the theme.
    SetLook(Look),
    /// Switch the density.
    SetDensity(Density),
    /// Back to the login screen.
    SignOut,
}

/// The state every screen keeps between frames.
#[derive(Default)]
pub struct Screens {
    /// Sessions.
    pub sessions: sessions::SessionsScreen,
    /// Oversight.
    pub oversight: oversight::OversightScreen,
    /// Backlog.
    pub backlog: backlog::BacklogScreen,
    /// Board.
    pub board: board::BoardScreen,
    /// Work item.
    pub work: work_item::WorkItemScreen,
    /// Git.
    pub git: git::GitScreen,
    /// Inbox.
    pub inbox: inbox::InboxScreen,
    /// Projects.
    pub projects: projects::ProjectsScreen,
    /// Settings.
    pub settings: settings::SettingsScreen,
}

/// Draw `screen`.
pub fn show(
    ui: &mut egui::Ui,
    screen: Screen,
    screens: &mut Screens,
    data: &mut Fixtures,
) -> Option<Action> {
    match screen {
        Screen::Sessions => screens.sessions.show(ui, data),
        Screen::Oversight => screens.oversight.show(ui, data),
        Screen::Backlog => screens.backlog.show(ui, data),
        Screen::Board => screens.board.show(ui, data),
        Screen::WorkItem => screens.work.show(ui, data),
        Screen::Git => screens.git.show(ui, data),
        Screen::Inbox => screens.inbox.show(ui, data),
        Screen::Projects => screens.projects.show(ui, data),
        Screen::Settings => screens.settings.show(ui, data),
    }
}

/// A screen title with an optional caption under it.
pub fn heading(ui: &mut egui::Ui, title: &str, caption: &str) {
    ui.label(egui::RichText::new(title).size(24.0).strong());
    if !caption.is_empty() {
        ui.label(egui::RichText::new(caption).weak());
    }
    ui.add_space(8.0);
}

/// A tinted badge.
pub fn tint(ui: &mut egui::Ui, text: &str, color: BadgeColor) -> egui::Response {
    ui.add(
        Badge::new(text)
            .appearance(BadgeAppearance::Tint)
            .color(color),
    )
}

/// An outline badge.
pub fn outline(ui: &mut egui::Ui, text: &str, color: BadgeColor) -> egui::Response {
    ui.add(
        Badge::new(text)
            .appearance(BadgeAppearance::Outline)
            .color(color),
    )
}

/// The colour a session state badge uses.
pub fn session_color(state: SessionState) -> BadgeColor {
    match state {
        SessionState::Running => BadgeColor::Success,
        SessionState::NeedsYou => BadgeColor::Important,
        SessionState::Idle => BadgeColor::Informative,
        SessionState::Blocked => BadgeColor::Danger,
        SessionState::Hibernated => BadgeColor::Subtle,
    }
}

/// A session state badge.
pub fn session_state(ui: &mut egui::Ui, state: SessionState) -> egui::Response {
    let appearance = if state == SessionState::NeedsYou {
        BadgeAppearance::Filled
    } else {
        BadgeAppearance::Tint
    };
    ui.add(
        Badge::new(state.label())
            .appearance(appearance)
            .color(session_color(state)),
    )
}

/// The colour a work-item state badge uses.
pub fn work_color(state: WorkState) -> BadgeColor {
    match state {
        WorkState::Open => BadgeColor::Informative,
        WorkState::InProgress => BadgeColor::Brand,
        WorkState::Review => BadgeColor::Warning,
        WorkState::Blocked => BadgeColor::Danger,
        WorkState::Done => BadgeColor::Success,
    }
}

/// A work-item state badge.
pub fn work_state(ui: &mut egui::Ui, state: WorkState) -> egui::Response {
    tint(ui, state.label(), work_color(state))
}

/// A priority badge: p0 is filled red, p1 orange, the rest quieter.
pub fn priority(ui: &mut egui::Ui, priority: &str) -> egui::Response {
    let (appearance, color) = match priority {
        "p0" => (BadgeAppearance::Tint, BadgeColor::Danger),
        "p1" => (BadgeAppearance::Tint, BadgeColor::Severe),
        "p2" => (BadgeAppearance::Tint, BadgeColor::Informative),
        _ => (BadgeAppearance::Outline, BadgeColor::Subtle),
    };
    ui.add(Badge::new(priority).appearance(appearance).color(color))
}

/// A muted caption.
pub fn caption(ui: &mut egui::Ui, text: impl Into<String>) -> egui::Response {
    ui.label(egui::RichText::new(text.into()).small().weak())
}

/// A grid of `label: value` rows.
pub fn facts(ui: &mut egui::Ui, id: &str, rows: &[(&str, String)]) {
    egui::Grid::new(id)
        .num_columns(2)
        .spacing([16.0, 6.0])
        .show(ui, |ui| {
            for (label, value) in rows {
                ui.label(egui::RichText::new(*label).weak());
                ui.label(value);
                ui.end_row();
            }
        });
}

/// A white (or dark) surface with a hairline stroke and a soft shadow, painted behind its
/// content. fluent2-egui's `Card` paints its rest shadow over its own fill at fddc829, which
/// turns resting cards grey, so the mockup draws its cards with a plain `Frame` from the same
/// tokens.
pub fn card<R>(
    ui: &mut egui::Ui,
    content: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::InnerResponse<R> {
    let theme = Theme::get(ui.ctx());
    egui::Frame::NONE
        .fill(c(theme.color.neutral_background_1))
        .stroke(egui::Stroke::new(1.0, c(theme.color.neutral_stroke_2)))
        .corner_radius(8.0)
        .inner_margin(12.0)
        .shadow(egui::Shadow {
            offset: [0, 1],
            blur: 4,
            spread: 0,
            color: egui::Color32::from_black_alpha(24),
        })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.vertical(content).inner
        })
}
