//! Desktop entry point. `VOGT_BASE` presets the API address on first run.

#[cfg(not(target_arch = "wasm32"))]
fn main() -> eframe::Result {
    env_logger::init();
    let base = std::env::var("VOGT_BASE").unwrap_or_default();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Vogt")
            .with_inner_size([1100.0, 720.0]),
        ..Default::default()
    };
    eframe::run_native(
        "vogt-gui",
        options,
        Box::new(move |cc| Ok(Box::new(vogt_gui::VogtApp::from_creation_context(cc, base)))),
    )
}

#[cfg(target_arch = "wasm32")]
fn main() {}
