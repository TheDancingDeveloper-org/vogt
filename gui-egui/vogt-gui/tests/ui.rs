//! The app end to end in kittest (no GPU): sign in, watch the session list
//! follow the event stream, lose and regain the stream, read a work item.

use std::cell::RefCell;
use std::rc::Rc;

use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use vogt_gui::VogtApp;
use vogt_gui::api::{ApiError, ServerEvent};
use vogt_gui::backend::FixtureBackend;
use vogt_gui::model::{Screen, StreamState};

fn harness() -> (Harness<'static, VogtApp>, Rc<RefCell<FixtureBackend>>) {
    let fixture = Rc::new(RefCell::new(FixtureBackend::demo()));
    let app = VogtApp::new(Box::new(Rc::clone(&fixture)), "http://dev");
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1000.0, 700.0))
        .build_ui_state(|ui, app: &mut VogtApp| app.frame(ui), app);
    harness.run_steps(2);
    (harness, fixture)
}

/// Focus a field, let a frame run so egui sees the focus, then type.
fn type_into(harness: &mut Harness<'_, VogtApp>, role: Role, label: &str, text: &str) {
    harness.get_by_role_and_label(role, label).focus();
    harness.run_steps(1);
    harness.get_by_role_and_label(role, label).type_text(text);
    harness.run_steps(1);
}

fn sign_in(harness: &mut Harness<'_, VogtApp>) {
    type_into(harness, Role::TextInput, "Username", "demo");
    type_into(harness, Role::PasswordInput, "Password", "demo");
    harness.run_steps(2);
    harness.get_by_label("Sign in").click();
    harness.run_steps(4);
}

#[test]
fn wrong_password_shows_an_error() {
    let (mut harness, _fixture) = harness();
    type_into(&mut harness, Role::TextInput, "Username", "demo");
    type_into(&mut harness, Role::PasswordInput, "Password", "nope");
    harness.run_steps(2);
    harness.get_by_label("Sign in").click();
    harness.run_steps(4);
    assert_eq!(harness.state().model.screen, Screen::Login);
    harness.get_by_label("Wrong username or password.");
}

#[test]
fn sessions_follow_the_event_stream() {
    let (mut harness, fixture) = harness();
    sign_in(&mut harness);
    assert_eq!(harness.state().model.screen, Screen::Sessions);
    harness.get_by_label("Live");
    harness.get_by_label("oversight");
    harness.get_by_label("WI-1806");

    fixture.borrow_mut().emit(ServerEvent::Activity {
        id: "s1".into(),
        state: "awaiting-approval".into(),
        activity_changed_at: None,
    });
    fixture.borrow_mut().emit(ServerEvent::SessionRenamed {
        id: "s1".into(),
        name: "renamed".into(),
    });
    harness.run_steps(2);
    harness.get_by_label("awaiting-approval");
    harness.get_by_label("renamed");
    assert!(harness.query_by_label("oversight").is_none());
}

#[test]
fn lost_stream_shows_reconnecting_then_recovers() {
    let (mut harness, fixture) = harness();
    sign_in(&mut harness);
    fixture.borrow_mut().drop_stream();
    harness.run_steps(2);
    assert!(matches!(harness.state().model.stream, StreamState::Waiting { .. }));
    assert!(harness.query_by_label_contains("Reconnecting in").is_some());

    // The first retry is 500 ms out; let real time pass and frames run.
    std::thread::sleep(std::time::Duration::from_millis(600));
    harness.run_steps(3);
    assert_eq!(harness.state().model.stream, StreamState::Live);
    harness.get_by_label("Live");
}

#[test]
fn expired_token_on_the_stream_returns_to_login() {
    let (mut harness, fixture) = harness();
    sign_in(&mut harness);
    fixture.borrow_mut().refuse_stream = Some(ApiError {
        status: Some(401),
        message: "expired".into(),
    });
    fixture.borrow_mut().drop_stream();
    harness.run_steps(2);
    std::thread::sleep(std::time::Duration::from_millis(600));
    harness.run_steps(3);
    assert_eq!(harness.state().model.screen, Screen::Login);
    harness.get_by_label("Your sign-in expired. Sign in again.");
}

#[test]
fn work_item_detail_renders_markdown_and_comments() {
    let (mut harness, fixture) = harness();
    sign_in(&mut harness);
    harness.get_by_label("Open WI-1806").click();
    harness.run_steps(4);
    assert_eq!(harness.state().model.screen, Screen::WorkItem);
    harness.get_by_label("WI-1806 Vogt-on-fluent2 POC");
    assert!(harness.query_by_label_contains("Umbrella for the").is_some());
    harness.get_by_label("Comments (1)");
    assert!(fixture.borrow().calls.iter().any(|c| c == "work WI-1806"));

    // An unknown reference typed into the box shows the failure, not a crash.
    type_into(&mut harness, Role::TextInput, "Work item", "-404");
    harness.run_steps(1);
    harness.get_by_label("Open").click();
    harness.run_steps(4);
    assert!(harness.query_by_label_contains("Could not read").is_some());
}

#[test]
fn selecting_a_session_shows_its_terminal_read_only() {
    let (mut harness, fixture) = harness();
    sign_in(&mut harness);
    harness.get_by_label("oversight").click();
    harness.run_steps(4);
    assert_eq!(harness.state().model.screen, Screen::Terminal);
    harness.get_by_label("Terminal oversight");
    harness.get_by_label("Attached");
    harness.get_by_label("Read-only");
    harness.get_by_label("s1 title");
    let text = || harness.state().model.terminal.grid.visible_text();
    assert!(text().starts_with("$ cargo test\nok"));

    // Live output paints; an activity event re-reads the screen.
    fixture.borrow_mut().pty_output("s1", b"\x1b[32mgreen\x1b[0m\r\n");
    fixture.borrow_mut().emit(ServerEvent::Activity {
        id: "s1".into(),
        state: "idle".into(),
        activity_changed_at: None,
    });
    harness.run_steps(3);
    assert!(harness.state().model.terminal.grid.visible_text().contains("green"));
    assert_eq!(fixture.borrow().calls.iter().filter(|c| *c == "screen s1").count(), 2);

    // A dropped attach reattaches and resumes where it left off.
    fixture.borrow_mut().drop_attach();
    harness.run_steps(2);
    assert!(harness.query_by_label_contains("Reattaching in").is_some());
    std::thread::sleep(std::time::Duration::from_millis(600));
    harness.run_steps(3);
    harness.get_by_label("Attached");
    assert!(fixture.borrow().calls.iter().any(|c| c == "attach s1 2 from 34"));

    // The view sends nothing: no input or resize call exists, and leaving detaches.
    harness.get_by_label("Back").click();
    harness.run_steps(2);
    assert_eq!(harness.state().model.screen, Screen::Sessions);
    assert!(fixture.borrow().attached.is_none());
}
