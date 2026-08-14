//! Background tokio runtime shared by the whole client.
//!
//! GPUI owns the main thread and runs its own event loop, so all server I/O
//! (REST, SSE, WS attach) runs on a separate multi-threaded tokio runtime whose
//! `Handle` is stashed here. UI code calls [`spawn`] to launch async work and
//! receives results back over channels it drains on the GPUI thread.

use std::sync::OnceLock;

use tokio::runtime::{Handle, Runtime};

static TOKIO_HANDLE: OnceLock<Handle> = OnceLock::new();
// Keep the runtime alive for the process lifetime; dropping it would stop I/O.
static TOKIO_RUNTIME: OnceLock<Runtime> = OnceLock::new();

/// Build the background runtime and stash its handle. Call once, early in
/// `main`, before starting the GPUI app. Idempotent.
pub fn init() {
    if TOKIO_HANDLE.get().is_some() {
        return;
    }
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("mydevenv2-io")
        .build()
        .expect("build tokio runtime");
    let _ = TOKIO_HANDLE.set(rt.handle().clone());
    let _ = TOKIO_RUNTIME.set(rt);
}

/// The background runtime handle. Panics if [`init`] has not run.
pub fn handle() -> Handle {
    TOKIO_HANDLE
        .get()
        .cloned()
        .expect("bridge::init() must be called before bridge::handle()")
}

/// Spawn a future on the background runtime.
pub fn spawn<F>(fut: F) -> tokio::task::JoinHandle<F::Output>
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    handle().spawn(fut)
}
