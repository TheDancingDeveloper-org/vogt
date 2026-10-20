//! Headless check of the real HTTP path, read-only: sign in with a token, read
//! the sessions, hold the event stream open for a while, read one work item.
//!
//! ```sh
//! VOGT_BASE=http://127.0.0.1:8910 VOGT_TOKEN=... cargo run -p vogt-gui --example probe -- WI-1806
//! ```

use std::time::Duration;

use vogt_gui::backend::{Backend, HttpBackend};
use vogt_gui::model::{self, Model, StreamState, WorkView};
use web_time::Instant;

fn main() {
    let base = std::env::var("VOGT_BASE").unwrap_or_else(|_| "http://127.0.0.1:8910".into());
    let token = std::env::var("VOGT_TOKEN").unwrap_or_default();
    let reference = std::env::args().nth(1).unwrap_or_else(|| "WI-1806".into());
    let seconds: u64 = std::env::var("PROBE_SECONDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(8);

    let mut backend = HttpBackend::new(|| {});
    let mut model = Model::new(base);
    model.login.token = token;
    let commands = model.submit_token();
    model::run(commands, &model, &mut backend);

    let end = Instant::now() + Duration::from_secs(seconds);
    let mut events = 0usize;
    let mut asked_for_work = false;
    while Instant::now() < end {
        let now = Instant::now();
        for reply in backend.poll() {
            if matches!(reply, vogt_gui::backend::Reply::StreamEvent(..)) {
                events += 1;
            }
            let commands = model.apply(reply, now);
            model::run(commands, &model, &mut backend);
        }
        let commands = model.tick(now);
        model::run(commands, &model, &mut backend);
        if model.stream == StreamState::Live && !asked_for_work {
            asked_for_work = true;
            let commands = model.open_work_item(&reference);
            model::run(commands, &model, &mut backend);
        }
        if model.auth.is_none() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    println!("signed in: {}", model.auth.is_some());
    if let Some(error) = &model.login.error {
        println!("login error: {error}");
    }
    println!("stream: {:?}", model.stream);
    println!("sessions: {} (error: {:?})", model.sessions.len(), model.sessions_error);
    println!("events in {seconds}s: {events}");
    match &model.work {
        WorkView::Loaded(detail) => println!(
            "work item: {} \"{}\" body {} chars, {} comments",
            detail.item.reference,
            detail.item.title,
            detail.item.body.len(),
            detail.comments.len()
        ),
        other => println!("work item: {other:?}"),
    }
}
