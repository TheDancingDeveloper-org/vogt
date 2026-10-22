//! Settings: appearance (theme and density), terminal, notifications and account.

use fluent2_egui::controls::{
    Appearance, Avatar, BadgeColor, Button, Field, RadioGroup, Slider, Switch,
};
use fluent2_egui::surfaces::Card;
use fluent2_egui::theme::{Density, Theme, ThemeKind};

use super::{Action, caption, heading, outline};
use crate::app::Look;
use crate::fixtures::Fixtures;

/// Settings screen state.
pub struct SettingsScreen {
    /// Terminal font size.
    pub font_size: f64,
    /// Push notifications.
    pub push: bool,
    /// Notify on hand-offs.
    pub handoffs: bool,
    /// Notify on CI failures.
    pub ci: bool,
    /// Sounds.
    pub sounds: bool,
}

impl Default for SettingsScreen {
    fn default() -> Self {
        Self {
            font_size: 13.0,
            push: true,
            handoffs: true,
            ci: true,
            sounds: false,
        }
    }
}

impl SettingsScreen {
    /// Draw the screen.
    pub fn show(&mut self, ui: &mut egui::Ui, _data: &mut Fixtures) -> Option<Action> {
        let mut action = None;
        heading(
            ui,
            "Settings",
            "Saved on this device. The mockup forgets them on restart.",
        );
        let theme = Theme::get(ui.ctx());
        egui::ScrollArea::vertical()
            .id_salt("settings_scroll")
            .show(ui, |ui| {
                ui.set_max_width(640.0);
                Card::new().id_salt("appearance").show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(egui::RichText::new("Appearance").size(18.0).strong());
                    ui.add_space(6.0);
                    let mut look = match theme.kind {
                        ThemeKind::Light => 0,
                        ThemeKind::Dark => 1,
                        ThemeKind::HighContrast => 2,
                    };
                    Field::new("Theme").show(ui, |ui| {
                        let before = look;
                        RadioGroup::new(&mut look)
                            .option("Light")
                            .option("Dark")
                            .option("High contrast")
                            .horizontal()
                            .show(ui);
                        if look != before {
                            action = Some(Action::SetLook(match look {
                                0 => Look::Light,
                                1 => Look::Dark,
                                _ => Look::HighContrast,
                            }));
                        }
                    });
                    let mut density = match theme.density {
                        Density::Compact => 0,
                        Density::Medium => 1,
                        Density::Touch => 2,
                    };
                    Field::new("Density")
                        .hint("Compact fits more rows; Touch makes every target at least 44 pt.")
                        .show(ui, |ui| {
                            let before = density;
                            RadioGroup::new(&mut density)
                                .option("Compact")
                                .option("Medium")
                                .option("Touch")
                                .horizontal()
                                .show(ui);
                            if density != before {
                                action = Some(Action::SetDensity(match density {
                                    0 => Density::Compact,
                                    1 => Density::Medium,
                                    _ => Density::Touch,
                                }));
                            }
                        });
                });
                ui.add_space(10.0);
                Card::new().id_salt("terminal_settings").show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(egui::RichText::new("Terminal").size(18.0).strong());
                    ui.add_space(6.0);
                    Field::new("Font size").show(ui, |ui| {
                        ui.add(Slider::new(&mut self.font_size, 9.0..=20.0).step(Some(1.0)));
                    });
                });
                ui.add_space(10.0);
                Card::new().id_salt("notifications").show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(egui::RichText::new("Notifications").size(18.0).strong());
                    ui.add_space(6.0);
                    ui.add(Switch::new(
                        &mut self.push,
                        "Push notifications on this device",
                    ));
                    ui.add(Switch::new(&mut self.handoffs, "When a worker hands off"));
                    ui.add(Switch::new(&mut self.ci, "When CI fails on a branch I own"));
                    ui.add(Switch::new(&mut self.sounds, "Play a sound"));
                });
                ui.add_space(10.0);
                Card::new().id_salt("account").show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(egui::RichText::new("Account").size(18.0).strong());
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.add(Avatar::new("Sprooty").size(40.0));
                        ui.vertical(|ui| {
                            ui.horizontal(|ui| {
                                ui.strong("sprooty");
                                outline(ui, "operator", BadgeColor::Brand);
                            });
                            caption(ui, "Signed in to vogt-dev.sprooty.com · this device");
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .add(Button::new("Sign out").appearance(Appearance::Outline))
                                .clicked()
                            {
                                action = Some(Action::SignOut);
                            }
                        });
                    });
                });
                ui.add_space(10.0);
                caption(
                    ui,
                    "vogt-gui-mockup 0.1.0 · fluent2-egui fddc829 · static mockup, fake data",
                );
            });
        action
    }
}
