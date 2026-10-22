//! Projects: the registered projects on the left, the selected one's detail on the right.

use fluent2_egui::controls::{BadgeColor, MiniBars, Sparkline};
use fluent2_egui::surfaces::{Card, Intent, MessageBar};

use super::{Action, caption, facts, heading, outline, tint};
use crate::fixtures::{Fixtures, Project, WorkState};

/// Projects screen state.
#[derive(Default)]
pub struct ProjectsScreen {
    /// Selected project index.
    pub selected: usize,
}

impl ProjectsScreen {
    /// Draw the screen.
    pub fn show(&mut self, ui: &mut egui::Ui, data: &mut Fixtures) -> Option<Action> {
        let mut action = None;
        heading(
            ui,
            "Projects",
            "Repositories Vogt tracks work for, and how their changes land.",
        );
        egui::Panel::left("project_list")
            .resizable(true)
            .default_size(300.0)
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("project_scroll")
                    .show(ui, |ui| {
                        for (index, project) in data.projects.iter().enumerate() {
                            let selected = self.selected == index;
                            let response =
                                Card::new()
                                    .id_salt(("project", project.slug))
                                    .show(ui, |ui| {
                                        ui.set_width(ui.available_width());
                                        ui.horizontal(|ui| {
                                            ui.strong(project.name);
                                            if selected {
                                                tint(ui, "selected", BadgeColor::Brand);
                                            }
                                            ui.with_layout(
                                                egui::Layout::right_to_left(egui::Align::Center),
                                                |ui| {
                                                    if project.drift > 0 {
                                                        tint(
                                                            ui,
                                                            &format!("{} drift", project.drift),
                                                            BadgeColor::Warning,
                                                        );
                                                    }
                                                },
                                            );
                                        });
                                        caption(ui, project.repo);
                                        let open: f32 = project.by_state[..4].iter().sum();
                                        ui.horizontal(|ui| {
                                            caption(
                                                ui,
                                                format!("{open:.0} open · {}", project.lifecycle),
                                            );
                                            ui.add(
                                                Sparkline::new(&project.commits)
                                                    .size(egui::vec2(90.0, 16.0))
                                                    .label(format!("{} commits", project.name)),
                                            );
                                        });
                                    });
                            if ui
                                .interact(
                                    response.rect,
                                    ui.id().with(("pick", index)),
                                    egui::Sense::click(),
                                )
                                .clicked()
                            {
                                self.selected = index;
                            }
                            ui.add_space(6.0);
                        }
                    });
            });
        let project = data.projects.get(self.selected)?;
        egui::Frame::NONE
            .inner_margin(egui::Margin {
                left: 16,
                ..Default::default()
            })
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("project_detail")
                    .show(ui, |ui| {
                        if let Some(a) = detail(ui, project) {
                            action = Some(a);
                        }
                    });
            });
        action
    }
}

fn detail(ui: &mut egui::Ui, project: &Project) -> Option<Action> {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(project.name).size(22.0).strong());
        outline(ui, project.lifecycle, BadgeColor::Subtle);
    });
    ui.label(project.summary);
    caption(ui, project.repo);
    ui.add_space(8.0);
    if project.drift > 0 {
        let _ = MessageBar::new(
            Intent::Warning,
            format!(
                "{} drift finding(s): the deployed config differs from the repository.",
                project.drift
            ),
        )
        .title("Drift")
        .show(ui);
        ui.add_space(8.0);
    }
    ui.columns(2, |cols| {
        Card::new()
            .id_salt("project_work")
            .show(&mut cols[0], |ui| {
                ui.set_width(ui.available_width());
                ui.strong("Work by state");
                ui.add(
                    MiniBars::new(&project.by_state)
                        .size(egui::vec2(ui.available_width(), 56.0))
                        .label("Work items by state"),
                );
                ui.horizontal_wrapped(|ui| {
                    for (state, count) in WorkState::ALL.iter().zip(project.by_state) {
                        caption(ui, format!("{} {count:.0}", state.label()));
                    }
                });
            });
        Card::new()
            .id_salt("project_commits")
            .show(&mut cols[1], |ui| {
                ui.set_width(ui.available_width());
                ui.strong("Commits, last 14 days");
                ui.add(
                    Sparkline::new(&project.commits)
                        .size(egui::vec2(ui.available_width(), 56.0))
                        .label("Commits per day"),
                );
                let total: f32 = project.commits.iter().sum();
                caption(ui, format!("{total:.0} commits"));
            });
    });
    ui.add_space(8.0);
    Card::new().id_salt("project_lanes").show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.strong("Deploy lanes");
        ui.add_space(4.0);
        egui::Grid::new("lanes")
            .num_columns(3)
            .spacing([24.0, 6.0])
            .show(ui, |ui| {
                for (lane, version, when) in &project.lanes {
                    tint(ui, lane, BadgeColor::Brand);
                    ui.label(egui::RichText::new(*version).monospace());
                    caption(ui, *when);
                    ui.end_row();
                }
            });
    });
    ui.add_space(8.0);
    Card::new().id_salt("project_policy").show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.strong("Policy");
        ui.add_space(4.0);
        facts(
            ui,
            "project_policy_facts",
            &[
                ("Lands by", project.lands_by.to_owned()),
                (
                    "Commit identity",
                    "sprooty <sprooty@sprooty.com>, DCO".to_owned(),
                ),
                (
                    "Branch pattern",
                    format!("^wi-NNNN-[a-z0-9-]+$ ({})", project.slug),
                ),
                (
                    "Required checks",
                    "build · unit tests · lint · policy".to_owned(),
                ),
            ],
        );
    });
    None
}
