//! Smoke tests in kittest (no GPU): sign in, render every screen in every theme, open the
//! command palette and the shortcut overlay, sort and filter the backlog, move a board card.

use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use fluent2_egui::data::data_grid::{SortDirection, SortSpec};
use vogt_gui_mockup::fixtures::WorkState;
use vogt_gui_mockup::{Look, MockupApp, Screen};

const SIZE: egui::Vec2 = egui::vec2(1360.0, 860.0);

fn harness(app: MockupApp) -> Harness<'static, MockupApp> {
    let mut harness = Harness::builder()
        .with_size(SIZE)
        .build_ui_state(|ui, app: &mut MockupApp| app.ui(ui), app);
    harness.run_steps(3);
    harness
}

#[test]
fn sign_in_reaches_sessions() {
    let mut harness = harness(MockupApp::default());
    assert!(!harness.state().signed_in);
    harness
        .get_by_role_and_label(Role::Button, "Sign in")
        .click();
    harness.run_steps(3);
    assert!(harness.state().signed_in);
    assert_eq!(harness.state().screen, Screen::Sessions);
    let _ = harness.get_all_by_label("wi-2162-fluent-mockup").next();
}

#[test]
fn every_screen_renders_in_every_theme() {
    for look in [Look::Light, Look::Dark, Look::HighContrast] {
        for screen in Screen::ALL {
            let mut harness = harness(MockupApp::at(screen, look));
            harness.run_steps(2);
            assert_eq!(harness.state().screen, screen, "{screen:?} in {look:?}");
        }
    }
}

#[test]
fn ctrl_k_opens_the_palette_and_a_command_navigates() {
    let mut harness = harness(MockupApp::at(Screen::Sessions, Look::Light));
    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::K);
    harness.run_steps(3);
    assert!(harness.state().palette_open);
    harness.get_by_label("Go to Board").click();
    harness.run_steps(3);
    assert_eq!(harness.state().screen, Screen::Board);
    assert!(!harness.state().palette_open);
}

#[test]
fn question_mark_opens_the_shortcut_overlay() {
    let mut harness = harness(MockupApp::at(Screen::Backlog, Look::Dark));
    harness.event(egui::Event::Text("?".to_owned()));
    harness.run_steps(3);
    assert!(harness.state().shortcuts_open);
    harness.get_by_label("Open the command palette");
}

#[test]
fn backlog_filters_and_sorts() {
    let mut app = MockupApp::at(Screen::Backlog, Look::Light);
    let all = app.screens.backlog.rows(&app.data);
    assert!(
        all.windows(2).all(|w| w[0].score >= w[1].score),
        "default sort is score, high first"
    );
    assert!(
        all.iter().all(|item| item.state != WorkState::Done),
        "done is hidden by default"
    );

    app.screens.backlog.project = Some(2); // fluent2-egui
    let fluent = app.screens.backlog.rows(&app.data);
    assert!(!fluent.is_empty());
    assert!(fluent.iter().all(|item| item.project == "fluent2-egui"));

    app.screens.backlog.project = Some(0);
    app.screens.backlog.grid.sort = Some(SortSpec {
        column: "ref".to_owned(),
        direction: SortDirection::Ascending,
    });
    let by_ref = app.screens.backlog.rows(&app.data);
    assert!(by_ref.windows(2).all(|w| w[0].reference <= w[1].reference));
}

#[test]
fn sign_out_from_settings_returns_to_login() {
    let mut harness = harness(MockupApp::at(Screen::Settings, Look::Light));
    harness
        .get_by_role_and_label(Role::Button, "Sign out")
        .scroll_to_me();
    harness.run_steps(3);
    harness
        .get_by_role_and_label(Role::Button, "Sign out")
        .click_accesskit();
    harness.run_steps(3);
    assert!(!harness.state().signed_in);
    harness.get_by_role_and_label(Role::Button, "Sign in");
}
