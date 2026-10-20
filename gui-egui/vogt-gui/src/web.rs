//! Browser entry point. The page (see `index.html`) calls `start` with the id
//! of its canvas. The API base defaults to the page's own origin, so the build
//! is served from the engine it talks to (no CORS).

use wasm_bindgen::JsCast as _;
use wasm_bindgen::prelude::*;

/// Start the app on the canvas with this element id.
#[wasm_bindgen]
pub async fn start(canvas_id: String) -> Result<(), JsValue> {
    let window = web_sys::window().ok_or("no window")?;
    let origin = window.location().origin()?;
    let canvas = window
        .document()
        .ok_or("no document")?
        .get_element_by_id(&canvas_id)
        .ok_or("no canvas")?
        .dyn_into::<web_sys::HtmlCanvasElement>()?;
    eframe::WebRunner::new()
        .start(
            canvas,
            eframe::WebOptions::default(),
            Box::new(move |cc| Ok(Box::new(crate::VogtApp::from_creation_context(cc, origin)))),
        )
        .await
}
