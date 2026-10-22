//! Board: a kanban column per work-item state. Cards drag between columns; the move only
//! changes the mockup's in-memory data.

use fluent2_egui::controls::{Avatar, BadgeColor, Dropdown};
use fluent2_egui::surfaces::{Intent, Toast, Toaster};

use super::{Action, caption, heading, outline, priority, work_color};
use crate::fixtures::{Fixtures, WorkItem, WorkState};

const PROJECTS: [&str; 6] = [
    "All projects",
    "vogt",
    "fluent2-egui",
    "cadastre",
    "farmeggs",
    "ops",
];

/// Board screen state.
pub struct BoardScreen {
    /// Project filter; 0 is all.
    pub project: Option<usize>,
}

impl Default for BoardScreen {
    fn default() -> Self {
        Self { project: Some(0) }
    }
}

/// The payload a dragged card carries: its reference.
struct Dragged(&'static str);

impl BoardScreen {
    /// Draw the screen.
    pub fn show(&mut self, ui: &mut egui::Ui, data: &mut Fixtures) -> Option<Action> {
        let mut action = None;
        ui.horizontal(|ui| {
            heading(
                ui,
                "Board",
                "Drag a card to another column to change its state (in the mockup only).",
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                ui.add(Dropdown::new("board_project", &mut self.project, &PROJECTS));
            });
        });
        let project = self
            .project
            .filter(|i| *i > 0)
            .and_then(|i| PROJECTS.get(i).copied());
        let mut moved: Option<(&'static str, WorkState)> = None;
        egui::ScrollArea::horizontal()
            .id_salt("board_scroll")
            .show(ui, |ui| {
                ui.horizontal_top(|ui| {
                    let width = ((ui.available_width() - 4.0 * 12.0) / 5.0).max(220.0);
                    for state in WorkState::ALL {
                        let items: Vec<&WorkItem> = data
                            .work
                            .iter()
                            .filter(|item| item.state == state)
                            .filter(|item| project.is_none_or(|p| item.project == p))
                            .collect();
                        let frame = egui::Frame::group(ui.style())
                            .fill(ui.visuals().faint_bg_color)
                            .corner_radius(8.0)
                            .inner_margin(8.0);
                        let (_, dropped) = ui.dnd_drop_zone::<Dragged, ()>(frame, |ui| {
                            ui.set_width(width);
                            ui.set_min_height(ui.available_height().max(420.0));
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new(state.label()).strong());
                                outline(ui, &items.len().to_string(), work_color(state));
                            });
                            ui.add_space(6.0);
                            for item in &items {
                                let id = egui::Id::new(("board_card", item.reference));
                                let response = ui
                                    .dnd_drag_source(id, Dragged(item.reference), |ui| {
                                        card(ui, item, width)
                                    })
                                    .response;
                                if response.double_clicked() {
                                    action = Some(Action::OpenWork(item.reference.to_owned()));
                                }
                                ui.add_space(6.0);
                            }
                            if items.is_empty() {
                                caption(ui, "Nothing here.");
                            }
                        });
                        if let Some(dragged) = dropped {
                            moved = Some((dragged.0, state));
                        }
                    }
                });
            });
        if let Some((reference, state)) = moved
            && let Some(item) = data
                .work
                .iter_mut()
                .find(|item| item.reference == reference)
            && item.state != state
        {
            item.state = state;
            Toaster::push(
                ui.ctx(),
                Toast {
                    intent: Intent::Info,
                    title: format!("{reference} moved to {}", state.label()),
                    body: Some("Mockup: nothing was sent.".to_owned()),
                    timeout: None,
                    action: None,
                },
            );
        }
        action
    }
}

fn card(ui: &mut egui::Ui, item: &WorkItem, width: f32) {
    egui::Frame::NONE
        .fill(ui.visuals().window_fill)
        .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
        .corner_radius(6.0)
        .inner_margin(10.0)
        .show(ui, |ui| {
            ui.set_width(width - 20.0);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(item.reference).monospace().weak());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    priority(ui, item.priority);
                });
            });
            ui.add(egui::Label::new(egui::RichText::new(item.title).strong()).wrap());
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                outline(ui, item.project, BadgeColor::Subtle);
                outline(ui, item.kind, BadgeColor::Informative);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if let Some(assignee) = item.assignee {
                        ui.add(Avatar::new(assignee).size(20.0));
                    }
                    caption(ui, item.updated);
                });
            });
        });
}
