//! Login: engine address, username and password, in a card in the middle of the window.

use fluent2_egui::controls::{Appearance, Button, Field, Input, Link, Switch};
use fluent2_egui::surfaces::{Intent, MessageBar};

use crate::app::LoginForm;

/// Draw the login screen. `true` when the user signs in. Any credentials work: it is a mockup.
pub fn show(ui: &mut egui::Ui, form: &mut LoginForm) -> bool {
    let mut signed_in = false;
    ui.vertical_centered(|ui| {
        ui.add_space((ui.available_height() * 0.12).max(16.0));
        ui.label(egui::RichText::new("Vogt").size(36.0).strong());
        ui.label(egui::RichText::new("Work items, agent sessions and oversight").weak());
        ui.add_space(20.0);
        ui.scope(|ui| {
            ui.set_max_width(380.0);
            super::card(ui, |ui| {
                ui.label(egui::RichText::new("Sign in").size(20.0).strong());
                ui.add_space(8.0);
                Field::new("Server")
                    .hint("The engine this GUI talks to")
                    .show(ui, |ui| {
                        ui.add(Input::new(&mut form.server).label("Server"));
                    });
                Field::new("Username").required(true).show(ui, |ui| {
                    ui.add(Input::new(&mut form.username).label("Username"));
                });
                Field::new("Password").required(true).show(ui, |ui| {
                    let response = ui.add(
                        Input::new(&mut form.password)
                            .label("Password")
                            .password()
                            .placeholder("Any password works in the mockup"),
                    );
                    if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        signed_in = true;
                    }
                });
                ui.add(Switch::new(
                    &mut form.remember,
                    "Keep me signed in on this device",
                ));
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui
                        .add(Button::new("Sign in").appearance(Appearance::Primary))
                        .clicked()
                    {
                        signed_in = true;
                    }
                    ui.add(Link::new("Use a bearer token instead"));
                });
            });
        });
        ui.add_space(12.0);
        ui.scope(|ui| {
            ui.set_max_width(380.0);
            let _ = MessageBar::new(
                Intent::Info,
                "This is a static mockup. Nothing is sent anywhere; every screen shows fake data.",
            )
            .show(ui);
        });
    });
    signed_in
}
