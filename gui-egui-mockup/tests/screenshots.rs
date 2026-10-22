//! Screenshots of every screen in the light and dark themes, plus the login screen, the command
//! palette and the shortcut overlay, rendered headlessly to `docs/screenshots/`.
//!
//! Needs a wgpu adapter. Run it through `scripts/screenshots.sh`, which uses a pinned container
//! with Mesa lavapipe (a software Vulkan driver), as fluent2-egui's gallery does.

#![cfg(feature = "snapshots")]

use egui_kittest::{Harness, SnapshotOptions, SnapshotResults};
use vogt_gui_mockup::{Look, MockupApp, Screen};

const OUT: &str = "docs/screenshots";
const SIZE: egui::Vec2 = egui::vec2(1360.0, 860.0);
/// Fixed frame count: spinners and the caret animate, so `run()` would never settle.
const FRAMES: usize = 10;

fn slug(look: Look) -> &'static str {
    match look {
        Look::Light => "light",
        Look::Dark => "dark",
        Look::HighContrast => "hc",
    }
}

fn shoot(
    results: &mut SnapshotResults,
    name: &str,
    app: MockupApp,
    setup: impl FnOnce(&mut Harness<'_, MockupApp>),
) {
    let mut harness = Harness::builder()
        .with_size(SIZE)
        .with_pixels_per_point(1.0)
        .build_ui_state(|ui, app: &mut MockupApp| app.ui(ui), app);
    harness.run_steps(3);
    setup(&mut harness);
    harness.run_steps(FRAMES);
    let options = SnapshotOptions::new().output_path(OUT);
    results.add(harness.try_snapshot_options(name, &options));
}

#[test]
fn screenshots() {
    let mut results = SnapshotResults::new();
    for look in [Look::Light, Look::Dark] {
        let theme = slug(look);
        for screen in Screen::ALL {
            let name = format!("{}-{theme}", screen.key());
            shoot(&mut results, &name, MockupApp::at(screen, look), |_| {});
        }
        let mut login = MockupApp::default();
        login.look = look;
        shoot(&mut results, &format!("login-{theme}"), login, |_| {});
        shoot(
            &mut results,
            &format!("palette-{theme}"),
            MockupApp::at(Screen::Sessions, look),
            |h| {
                h.state_mut().palette_open = true;
            },
        );
        shoot(
            &mut results,
            &format!("shortcuts-{theme}"),
            MockupApp::at(Screen::Backlog, look),
            |h| {
                h.state_mut().shortcuts_open = true;
            },
        );
    }
    results.unwrap();
}
