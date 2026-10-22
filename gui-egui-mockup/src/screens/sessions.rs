//! Sessions: the session rail (activity, state and model badges) and the open session, with
//! a terminal grid fed a fake transcript, the brief and the status.

use std::collections::HashMap;

use fluent2_egui::IconName;
use fluent2_egui::controls::{Appearance, BadgeColor, Button, ProgressBar, SearchBox, Sparkline};
use fluent2_egui::data::markdown::{Document, MarkdownView};
use fluent2_egui::data::terminal::{TerminalGrid, TerminalState, TerminalView};
use fluent2_egui::surfaces::{Card, TabList};

use super::{Action, caption, facts, outline, session_state, tint};
use crate::fixtures::{self, Fixtures, Role, Session, SessionState};

/// Sessions screen state.
pub struct SessionsScreen {
    /// Id of the open session.
    pub selected: String,
    /// Rail filter text.
    pub filter: String,
    /// Rail tab: all, needs you, running, hibernated.
    pub tab: usize,
    /// Side tab: status or brief.
    pub side: usize,
    terminals: HashMap<String, (TerminalGrid, TerminalState)>,
    brief: Document,
}

impl Default for SessionsScreen {
    fn default() -> Self {
        Self {
            selected: "ses_01M5H2049H".to_owned(),
            filter: String::new(),
            tab: 0,
            side: 0,
            terminals: HashMap::new(),
            brief: Document::parse(fixtures::BRIEF),
        }
    }
}

impl SessionsScreen {
    /// Draw the screen.
    pub fn show(&mut self, ui: &mut egui::Ui, data: &mut Fixtures) -> Option<Action> {
        let mut action = None;
        egui::Panel::left("session_rail")
            .resizable(true)
            .default_size(330.0)
            .show(ui, |ui| self.rail(ui, data));
        let Some(session) = data.session(&self.selected).cloned() else {
            ui.label("No session selected.");
            return None;
        };
        egui::Frame::NONE
            .inner_margin(egui::Margin {
                left: 12,
                ..Default::default()
            })
            .show(ui, |ui| {
                action = self.header(ui, &session);
                ui.add_space(8.0);
                egui::Panel::right("session_side")
                    .resizable(true)
                    .default_size(330.0)
                    .show(ui, |ui| self.side(ui, &session));
                self.terminal(ui, &session);
            });
        action
    }

    fn rail(&mut self, ui: &mut egui::Ui, data: &Fixtures) {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Sessions").size(20.0).strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add(
                    Button::new("New")
                        .icon(IconName::Add)
                        .appearance(Appearance::Primary),
                );
            });
        });
        ui.add_space(6.0);
        ui.add(
            SearchBox::new(&mut self.filter)
                .placeholder("Filter sessions")
                .label("Filter sessions"),
        );
        ui.add_space(4.0);
        TabList::new("session_tabs", &mut self.tab)
            .tab("All")
            .tab("Needs you")
            .tab("Running")
            .tab("Hibernated")
            .show(ui);
        ui.add_space(6.0);
        let filter = self.filter.to_lowercase();
        egui::ScrollArea::vertical()
            .id_salt("rail_scroll")
            .show(ui, |ui| {
                for session in &data.sessions {
                    let wanted = match self.tab {
                        1 => matches!(
                            session.state,
                            SessionState::NeedsYou | SessionState::Blocked
                        ),
                        2 => session.state == SessionState::Running,
                        3 => session.state == SessionState::Hibernated,
                        _ => true,
                    };
                    let hit = filter.is_empty()
                        || session.name.contains(&filter)
                        || session
                            .work
                            .is_some_and(|w| w.to_lowercase().contains(&filter));
                    if wanted && hit && row(ui, session, self.selected == session.id).clicked() {
                        self.selected = session.id.to_owned();
                    }
                }
            });
    }

    fn header(&mut self, ui: &mut egui::Ui, session: &Session) -> Option<Action> {
        let mut action = None;
        ui.horizontal_wrapped(|ui| {
            ui.label(egui::RichText::new(session.name).size(22.0).strong());
            session_state(ui, session.state);
            outline(ui, session.role.label(), BadgeColor::Subtle);
            tint(
                ui,
                &format!("{} · {}", session.agent, session.model),
                BadgeColor::Brand,
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add(
                    Button::new("Stop")
                        .icon(IconName::Dismiss)
                        .appearance(Appearance::Subtle),
                );
                ui.add(
                    Button::new("Hibernate")
                        .icon(IconName::WeatherMoon)
                        .appearance(Appearance::Subtle),
                );
                if let Some(work) = session.work
                    && ui
                        .add(Button::new(format!("Open {work}")).icon(IconName::Document))
                        .clicked()
                {
                    action = Some(Action::OpenWork(work.to_owned()));
                }
            });
        });
        caption(
            ui,
            format!(
                "{} · {} · up {}",
                session.id, session.branch, session.runtime
            ),
        );
        action
    }

    fn side(&mut self, ui: &mut egui::Ui, session: &Session) {
        TabList::new("session_side_tabs", &mut self.side)
            .tab("Status")
            .tab("Brief")
            .show(ui);
        ui.add_space(8.0);
        egui::ScrollArea::vertical()
            .id_salt("side_scroll")
            .show(ui, |ui| {
                if self.side == 1 {
                    let _ = MarkdownView::new("session_brief", &self.brief).show(ui);
                    return;
                }
                Card::new().id_salt("status_card").show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.strong("Status");
                    ui.add_space(4.0);
                    facts(
                        ui,
                        "session_facts",
                        &[
                            ("State", session.state.label().to_owned()),
                            ("Work item", session.work.unwrap_or("—").to_owned()),
                            ("Branch", session.branch.to_owned()),
                            ("Agent", format!("{} ({})", session.agent, session.model)),
                            ("Last active", session.last_active.to_owned()),
                            ("Run time", session.runtime.to_owned()),
                            ("Spend", format!("${:.2}", session.cost)),
                        ],
                    );
                    ui.add_space(8.0);
                    ui.label(format!(
                        "Context window · {:.0}% used",
                        session.context * 100.0
                    ));
                    ui.add(ProgressBar::new(Some(session.context)));
                });
                ui.add_space(8.0);
                Card::new().id_salt("activity_card").show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.strong("Activity, last 30 minutes");
                    ui.add(
                        Sparkline::new(&session.activity)
                            .size(egui::vec2(ui.available_width(), 48.0))
                            .label("Activity"),
                    );
                });
                ui.add_space(8.0);
                Card::new().id_salt("reply_card").show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.strong("Latest reply");
                    ui.label(egui::RichText::new(session.excerpt).italics());
                });
            });
    }

    fn terminal(&mut self, ui: &mut egui::Ui, session: &Session) {
        let (grid, state) = self
            .terminals
            .entry(session.id.to_owned())
            .or_insert_with(|| {
                let mut grid = TerminalGrid::new(100, 30);
                grid.process(transcript_for(session).as_bytes());
                (grid, TerminalState::default())
            });
        let response = TerminalView::new(("terminal", session.id))
            .label(format!("Terminal for {}", session.name))
            .show(ui, grid, state);
        let (cols, rows) = response.fit;
        if (cols, rows) != (grid.cols(), grid.rows()) && cols > 0 && rows > 0 {
            grid.resize(cols, rows);
        }
    }
}

/// One rail row.
fn row(ui: &mut egui::Ui, session: &Session, selected: bool) -> egui::Response {
    let visuals = ui.visuals().clone();
    let fill = if selected {
        visuals.selection.bg_fill.gamma_multiply(0.35)
    } else {
        egui::Color32::TRANSPARENT
    };
    let frame = egui::Frame::NONE
        .fill(fill)
        .corner_radius(6.0)
        .inner_margin(egui::Margin::symmetric(8, 6));
    let inner = frame.show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            let name = egui::RichText::new(session.name).strong();
            ui.label(name);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                caption(ui, session.last_active);
            });
        });
        ui.horizontal_wrapped(|ui| {
            session_state(ui, session.state);
            if session.role != Role::Worker {
                outline(ui, session.role.label(), BadgeColor::Subtle);
            }
            outline(ui, session.model, BadgeColor::Brand);
            if let Some(work) = session.work {
                caption(ui, work);
            }
        });
        ui.add(
            Sparkline::new(&session.activity)
                .size(egui::vec2(ui.available_width().min(260.0), 16.0))
                .label(format!("{} activity", session.name)),
        );
    });
    let response = ui.interact(
        inner.response.rect,
        ui.id().with(session.id),
        egui::Sense::click(),
    );
    if response.hovered() && !selected {
        ui.painter().rect_filled(
            inner.response.rect,
            6.0,
            visuals.widgets.hovered.weak_bg_fill.gamma_multiply(0.3),
        );
    }
    response
}

/// The fake terminal output for `session`.
fn transcript_for(session: &Session) -> String {
    if session.id == "ses_01M5H2049H" {
        return fixtures::transcript();
    }
    let mut out = String::new();
    if session.role == Role::Shell {
        out.push_str(
            "\x1b[32msprooty@winrarhost\x1b[0m:\x1b[34m~/stacks\x1b[0m$ docker compose ps\r\n",
        );
        out.push_str("NAME            IMAGE                       STATUS\r\n");
        out.push_str("vogt-dev        ghcr.io/vogt/vogt:0.7.22    Up 3 hours (healthy)\r\n");
        out.push_str("vogt-prod       ghcr.io/vogt/vogt:0.7.21    Up 2 days (healthy)\r\n");
        out.push_str("caddy           caddy:2.10                  Up 9 days\r\n");
        out.push_str("loki            grafana/loki:3.5            Up 9 days\r\n");
        out.push_str("\x1b[32msprooty@winrarhost\x1b[0m:\x1b[34m~/stacks\x1b[0m$ ");
        return out;
    }
    out.push_str(&format!(
        "\x1b[1m✻ {}\x1b[0m  {} · {}\r\n\r\n",
        session.agent, session.model, session.branch
    ));
    out.push_str("\x1b[32m●\x1b[0m \x1b[1mBash\x1b[0m(git log --oneline -3)\r\n");
    out.push_str("  ⎿  \x1b[2m4be19c0 feat: first pass\x1b[0m\r\n\r\n");
    let colour = match session.state {
        SessionState::Blocked => "31",
        SessionState::NeedsYou => "33",
        _ => "37",
    };
    out.push_str(&format!(
        "\x1b[{colour}m●\x1b[0m {}\r\n\r\n",
        session.excerpt
    ));
    if session.state == SessionState::NeedsYou {
        out.push_str("\x1b[33m? \x1b[0m\x1b[1mChoose one\x1b[0m\r\n");
        out.push_str("  \x1b[36m❯ 1. At the top, marked unread (recommended)\x1b[0m\r\n");
        out.push_str("    2. Back at its original position\r\n");
        out.push_str("    3. In a 'Woke up' group\r\n");
    } else {
        out.push_str("\x1b[38;5;240m> \x1b[0m");
    }
    out
}
