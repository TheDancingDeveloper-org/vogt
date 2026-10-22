//! Git: a branch's changed files beside a read-only diff, inline or side by side.

use fluent2_egui::controls::{BadgeColor, Dropdown, ToggleButton};
use fluent2_egui::data::diff::{DiffMode, DiffModel, DiffView};

use super::{Action, caption, heading, outline, tint};
use crate::fixtures::Fixtures;

const BRANCHES: [&str; 3] = [
    "wi-2150-inbox-snooze",
    "wi-2141-terminal-reflow",
    "wi-2133-voice-vad",
];

/// Git screen state.
pub struct GitScreen {
    /// Branch dropdown.
    pub branch: Option<usize>,
    /// Selected file index.
    pub file: usize,
    /// Side-by-side instead of inline.
    pub side_by_side: bool,
    models: Vec<DiffModel>,
}

impl Default for GitScreen {
    fn default() -> Self {
        Self {
            branch: Some(0),
            file: 0,
            side_by_side: false,
            models: Vec::new(),
        }
    }
}

impl GitScreen {
    /// Draw the screen.
    pub fn show(&mut self, ui: &mut egui::Ui, data: &mut Fixtures) -> Option<Action> {
        if self.models.len() != data.changes.len() {
            self.models = data
                .changes
                .iter()
                .map(|f| DiffModel::new(&f.old, &f.new))
                .collect();
        }
        heading(ui, "Git", "Read-only diff of a branch against its base.");
        ui.horizontal_wrapped(|ui| {
            ui.add(Dropdown::new("git_branch", &mut self.branch, &BRANCHES));
            ui.label("into");
            outline(ui, "main", BadgeColor::Subtle);
            tint(ui, "3 commits ahead", BadgeColor::Brand);
            tint(ui, "CI green", BadgeColor::Success);
            ui.add(ToggleButton::new(&mut self.side_by_side, "Side by side"));
        });
        caption(
            ui,
            "4be19c0 feat(inbox): snooze entries until a time (WI-2150) · codex · 22 min ago",
        );
        ui.add_space(8.0);
        egui::Panel::left("git_files")
            .resizable(true)
            .default_size(280.0)
            .show(ui, |ui| {
                ui.strong(format!("Changed files ({})", data.changes.len()));
                ui.add_space(4.0);
                for (index, (file, model)) in data.changes.iter().zip(&self.models).enumerate() {
                    ui.horizontal(|ui| {
                        if ui
                            .selectable_label(
                                self.file == index,
                                file.path.rsplit('/').next().unwrap_or(file.path),
                            )
                            .clicked()
                        {
                            self.file = index;
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(
                                egui::RichText::new(format!("−{}", model.removals()))
                                    .color(ui.visuals().error_fg_color),
                            );
                            ui.label(
                                egui::RichText::new(format!("+{}", model.additions()))
                                    .color(egui::Color32::from_rgb(0x10, 0x7c, 0x10)),
                            );
                        });
                    });
                    caption(ui, file.path);
                    ui.add_space(4.0);
                }
            });
        let (file, model) = data
            .changes
            .get(self.file)
            .zip(self.models.get(self.file))?;
        egui::Frame::NONE
            .inner_margin(egui::Margin {
                left: 12,
                ..Default::default()
            })
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(file.path).monospace().strong());
                    caption(
                        ui,
                        format!(
                            "{} hunks · +{} −{}",
                            model.hunks().len(),
                            model.additions(),
                            model.removals()
                        ),
                    );
                });
                ui.add_space(4.0);
                let mode = if self.side_by_side {
                    DiffMode::SideBySide
                } else {
                    DiffMode::Inline
                };
                DiffView::new(("git_diff", self.file))
                    .label(file.path)
                    .mode(mode)
                    .show(ui, model);
            });
        None
    }
}
