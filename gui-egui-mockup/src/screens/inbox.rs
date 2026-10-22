//! Inbox: notifications from sessions, CI, drift and deploys, with archive and snooze.

use fluent2_egui::controls::{Appearance, BadgeColor, Button};
use fluent2_egui::surfaces::{Card, TabList};
use fluent2_egui::{Icon, IconName, IconSize};

use super::{Action, caption, heading, outline, tint};
use crate::fixtures::{Fixtures, InboxEntry, InboxKind};

/// Inbox screen state.
#[derive(Default)]
pub struct InboxScreen {
    /// Active, snoozed or archived.
    pub tab: usize,
    archived: Vec<u32>,
    snoozed: Vec<u32>,
}

impl InboxScreen {
    /// Draw the screen.
    pub fn show(&mut self, ui: &mut egui::Ui, data: &mut Fixtures) -> Option<Action> {
        let mut action = None;
        heading(ui, "Inbox", "What needs you, newest first.");
        let active = data
            .inbox
            .iter()
            .filter(|e| !self.archived.contains(&e.id) && !self.snoozed.contains(&e.id))
            .count();
        ui.horizontal(|ui| {
            TabList::new("inbox_tabs", &mut self.tab)
                .tab(format!("Active ({active})"))
                .tab(format!("Snoozed ({})", self.snoozed.len()))
                .tab(format!("Archived ({})", self.archived.len()))
                .show(ui);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add(Button::new("Mark all read").appearance(Appearance::Subtle))
                    .clicked()
                {
                    for entry in &mut data.inbox {
                        entry.unread = false;
                    }
                }
            });
        });
        ui.add_space(8.0);
        let mut archive = None;
        let mut snooze = None;
        egui::ScrollArea::vertical()
            .id_salt("inbox_scroll")
            .show(ui, |ui| {
                let mut shown = 0;
                for entry in &mut data.inbox {
                    let place = if self.archived.contains(&entry.id) {
                        2
                    } else if self.snoozed.contains(&entry.id) {
                        1
                    } else {
                        0
                    };
                    if place != self.tab {
                        continue;
                    }
                    shown += 1;
                    Card::new().id_salt(("inbox", entry.id)).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            ui.add(Icon::new(icon(entry.kind)).size(IconSize::S20));
                            ui.vertical(|ui| {
                                ui.horizontal(|ui| {
                                    let title = egui::RichText::new(entry.title);
                                    ui.label(if entry.unread { title.strong() } else { title });
                                    kind_badge(ui, entry);
                                    if entry.unread {
                                        tint(ui, "New", BadgeColor::Brand);
                                    }
                                });
                                ui.label(entry.body);
                                caption(ui, format!("{} · {} ago", entry.source, entry.when));
                            });
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if place != 2
                                        && ui
                                            .add(
                                                Button::icon_only(IconName::Archive, "Archive")
                                                    .appearance(Appearance::Subtle),
                                            )
                                            .clicked()
                                    {
                                        archive = Some(entry.id);
                                    }
                                    if place == 0
                                        && ui
                                            .add(
                                                Button::new("Snooze 1h")
                                                    .appearance(Appearance::Subtle),
                                            )
                                            .clicked()
                                    {
                                        snooze = Some(entry.id);
                                    }
                                    if ui.add(Button::new("Open")).clicked() {
                                        entry.unread = false;
                                        action = Some(match entry.kind {
                                            InboxKind::Question => {
                                                Action::OpenSession("ses_01M5H1ZZ2K".to_owned())
                                            }
                                            InboxKind::Handoff => {
                                                Action::OpenWork("WI-2133".to_owned())
                                            }
                                            InboxKind::CiFailed => {
                                                Action::OpenWork("WI-2139".to_owned())
                                            }
                                            InboxKind::Drift | InboxKind::Deploy => {
                                                Action::Go(crate::app::Screen::Projects)
                                            }
                                        });
                                    }
                                },
                            );
                        });
                    });
                    ui.add_space(6.0);
                }
                if shown == 0 {
                    ui.add_space(24.0);
                    ui.vertical_centered(|ui| caption(ui, "Nothing here. Enjoy the quiet."));
                }
            });
        if let Some(id) = archive {
            self.snoozed.retain(|s| *s != id);
            self.archived.push(id);
        }
        if let Some(id) = snooze {
            self.snoozed.push(id);
        }
        action
    }
}

fn icon(kind: InboxKind) -> IconName {
    match kind {
        InboxKind::Question => IconName::QuestionCircle,
        InboxKind::Handoff => IconName::Checkmark,
        InboxKind::CiFailed => IconName::ErrorCircle,
        InboxKind::Drift => IconName::Warning,
        InboxKind::Deploy => IconName::ArrowUp,
    }
}

fn kind_badge(ui: &mut egui::Ui, entry: &InboxEntry) {
    let (label, color) = match entry.kind {
        InboxKind::Question => ("question", BadgeColor::Important),
        InboxKind::Handoff => ("hand-off", BadgeColor::Success),
        InboxKind::CiFailed => ("CI", BadgeColor::Danger),
        InboxKind::Drift => ("drift", BadgeColor::Warning),
        InboxKind::Deploy => ("deploy", BadgeColor::Informative),
    };
    outline(ui, label, color);
}
