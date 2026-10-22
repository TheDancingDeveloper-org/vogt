//! The Vogt GUI mockup. Native: `cargo run -p vogt-gui-mockup`. Wasm: `trunk serve`.

use vogt_gui_mockup::MockupApp;

#[cfg(not(target_arch = "wasm32"))]
fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Vogt (mockup)")
            .with_inner_size([1360.0, 860.0]),
        ..Default::default()
    };
    eframe::run_native(
        "vogt-gui-mockup",
        options,
        Box::new(|_cc| Ok(Box::new(MockupApp::default()))),
    )
}

#[cfg(target_arch = "wasm32")]
fn main() {
    wasm_bindgen_futures::spawn_local(async {
        if let Err(error) = start_web().await {
            log::error!("mockup failed to start: {error}");
        }
    });
}

#[cfg(target_arch = "wasm32")]
async fn start_web() -> Result<(), String> {
    use eframe::wasm_bindgen::JsCast as _;

    let canvas = web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.get_element_by_id("the_canvas_id"))
        .ok_or("no #the_canvas_id element")?
        .dyn_into::<web_sys::HtmlCanvasElement>()
        .map_err(|_| "#the_canvas_id is not a canvas")?;
    eframe::WebRunner::new()
        .start(
            canvas,
            eframe::WebOptions::default(),
            Box::new(|_cc| Ok(Box::new(MockupApp::default()))),
        )
        .await
        .map_err(|error| format!("{error:?}"))
}
