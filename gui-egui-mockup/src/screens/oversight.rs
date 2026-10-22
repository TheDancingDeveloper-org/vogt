//! Oversight: the pending decisions first, then a card per worker session.

use fluent2_egui::IconName;
use fluent2_egui::controls::{Appearance, BadgeColor, Button, ProgressBar, Sparkline};
use fluent2_egui::surfaces::{Intent, MessageBar, Toast, Toaster};

use super::{Action, caption, heading, outline, session_state, tint};
use crate::fixtures::{Fixtures, Role, Session, SessionState};

/// Oversight screen state.
#[derive(Default)]
pub struct OversightScreen {
    answered: Vec<(&'static str, usize)>,
}

impl OversightScreen {
    /// Draw the screen.
    pub fn show(&mut self, ui: &mut egui::Ui, data: &mut Fixtures) -> Option<Action> {
        let mut action = None;
        heading(
            ui,
            "Oversight",
            "What the workers are doing, and what they are waiting on you for.",
        );
        egui::ScrollArea::vertical()
            .id_salt("oversight_scroll")
            .show(ui, |ui| {
                summary(ui, data);
                ui.add_space(16.0);
                ui.label(egui::RichText::new("Pending decisions").size(18.0).strong());
                ui.add_space(6.0);
                for decision in &data.decisions {
                    let answered = self
                        .answered
                        .iter()
                        .find(|(id, _)| *id == decision.id)
                        .map(|(_, i)| *i);
                    super::card(ui, |ui| {
                        ui.horizontal(|ui| {
                            tint(ui, "Decision", BadgeColor::Important);
                            ui.strong(decision.session);
                            caption(
                                ui,
                                format!("{} · waiting {}", decision.work, decision.waiting),
                            );
                        });
                        ui.add_space(4.0);
                        ui.label(decision.question);
                        ui.add_space(6.0);
                        if let Some(index) = answered {
                            let _ = MessageBar::new(
                                Intent::Success,
                                format!("Answered: {}", decision.options[index]),
                            )
                            .show(ui);
                            return;
                        }
                        ui.horizontal_wrapped(|ui| {
                            for (index, option) in decision.options.iter().enumerate() {
                                let (label, appearance) = if index == 0 {
                                    (format!("{option} (recommended)"), Appearance::Primary)
                                } else {
                                    ((*option).to_owned(), Appearance::Secondary)
                                };
                                if ui.add(Button::new(label).appearance(appearance)).clicked() {
                                    self.answered.push((decision.id, index));
                                    Toaster::push(
                                        ui.ctx(),
                                        Toast {
                                            intent: Intent::Success,
                                            title: "Answer recorded".to_owned(),
                                            body: Some(format!(
                                                "{} will see: {option}",
                                                decision.session
                                            )),
                                            timeout: None,
                                            action: None,
                                        },
                                    );
                                }
                            }
                            if ui
                                .add(Button::new("Open session").appearance(Appearance::Subtle))
                                .clicked()
                                && let Some(session) =
                                    data.sessions.iter().find(|s| s.name == decision.session)
                            {
                                action = Some(Action::OpenSession(session.id.to_owned()));
                            }
                        });
                    });
                    ui.add_space(8.0);
                }
                ui.add_space(8.0);
                ui.label(egui::RichText::new("Workers").size(18.0).strong());
                ui.add_space(6.0);
                let workers: Vec<&Session> = data
                    .sessions
                    .iter()
                    .filter(|s| s.role != Role::Shell && s.role != Role::Oversight)
                    .collect();
                let columns = ((ui.available_width() / 340.0).floor() as usize).clamp(1, 3);
                for chunk in workers.chunks(columns) {
                    ui.columns(columns, |cols| {
                        for (col, session) in cols.iter_mut().zip(chunk) {
                            if let Some(a) = worker_card(col, session) {
                                action = Some(a);
                            }
                        }
                    });
                    ui.add_space(8.0);
                }
            });
        action
    }
}

fn summary(ui: &mut egui::Ui, data: &Fixtures) {
    let count = |state: SessionState| data.sessions.iter().filter(|s| s.state == state).count();
    let spend: f32 = data.sessions.iter().map(|s| s.cost).sum();
    let tiles = [
        (
            "Running",
            count(SessionState::Running).to_string(),
            BadgeColor::Success,
        ),
        (
            "Needs you",
            count(SessionState::NeedsYou).to_string(),
            BadgeColor::Important,
        ),
        (
            "Blocked",
            count(SessionState::Blocked).to_string(),
            BadgeColor::Danger,
        ),
        (
            "Hibernated",
            count(SessionState::Hibernated).to_string(),
            BadgeColor::Subtle,
        ),
        ("Spend today", format!("${spend:.2}"), BadgeColor::Brand),
    ];
    ui.columns(tiles.len(), |cols| {
        for (col, (label, value, color)) in cols.iter_mut().zip(tiles) {
            super::card(col, |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(value).size(26.0).strong());
                    outline(ui, "●", color);
                });
                caption(ui, label);
            });
        }
    });
}

fn worker_card(ui: &mut egui::Ui, session: &Session) -> Option<Action> {
    let mut action = None;
    super::card(ui, |ui| {
        ui.horizontal(|ui| {
            ui.strong(session.name);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                session_state(ui, session.state);
            });
        });
        ui.horizontal_wrapped(|ui| {
            if let Some(work) = session.work {
                tint(ui, work, BadgeColor::Informative);
            }
            outline(ui, session.role.label(), BadgeColor::Subtle);
            outline(
                ui,
                &format!("{} · {}", session.agent, session.model),
                BadgeColor::Brand,
            );
        });
        ui.add_space(4.0);
        ui.label(egui::RichText::new(session.excerpt).italics());
        ui.add_space(4.0);
        ui.add(
            Sparkline::new(&session.activity)
                .size(egui::vec2(ui.available_width(), 22.0))
                .label(format!("{} activity", session.name)),
        );
        ui.horizontal(|ui| {
            caption(
                ui,
                format!(
                    "up {} · {} · ${:.2}",
                    session.runtime, session.last_active, session.cost
                ),
            );
        });
        ui.add(ProgressBar::new(Some(session.context)));
        caption(ui, format!("{:.0}% of context", session.context * 100.0));
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if ui.add(Button::new("Open").icon(IconName::Open)).clicked() {
                action = Some(Action::OpenSession(session.id.to_owned()));
            }
            ui.add(Button::new("Nudge").appearance(Appearance::Subtle));
            ui.add(Button::new("Hibernate").appearance(Appearance::Subtle));
        });
    });
    action
}
