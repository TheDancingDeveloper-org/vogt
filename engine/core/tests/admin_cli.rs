//! The admin verbs an operator runs inside the container: `status`,
//! `user create`, `token issue`, `backup` and `restore`. Each one runs as the
//! binary runs it, against a fresh instance, and the exit codes and output
//! are the ones the Python CLI gives (`scripts/parity.py` pins the same steps
//! against the recorded golden).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

const PASSWORD: &str = "correct horse battery";

fn binary() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_vogt-core"));
    // The same deterministic hooks the parity harness sets, so ids and clock
    // stamps do not depend on the machine the test runs on.
    command
        .env("VOGT_TEST_CLOCK_START", "2026-01-02T03:04:05+00:00")
        .env("VOGT_TEST_IDS", "sequential")
        .env("USER", "parity")
        .env("LOGNAME", "parity");
    command
}

/// A fresh scratch root with an initialised instance under `instance/`.
fn fresh(name: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("vogt-admin-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let data = root.join("instance");
    let init = binary()
        .arg("--json")
        .arg("--data-dir")
        .arg(&data)
        .arg("init")
        .output()
        .expect("run init");
    assert!(init.status.success(), "init: {}", text(&init.stderr));
    (root, data)
}

fn password_file(root: &Path) -> PathBuf {
    let path = root.join("password");
    std::fs::write(&path, PASSWORD).unwrap();
    path
}

/// Runs a verb in the global-flag position every caller uses.
fn run(data: &Path, args: &[&str]) -> Output {
    binary()
        .arg("--json")
        .arg("--data-dir")
        .arg(data)
        .args(args)
        .output()
        .expect("run vogt-core")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn json_of(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("stdout is not JSON ({error}): {}", text(&output.stdout)))
}

#[test]
fn status_reports_the_instance_after_init() {
    let (_root, data) = fresh("status");
    let output = run(&data, &["status"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    let body = json_of(&output);
    assert_eq!(body["principal"], "local:parity");
    assert_eq!(body["counts"]["work_items"], 0);
    assert_eq!(body["clone"], Value::Null);
}

#[test]
fn user_create_then_token_issue_returns_a_secret_shown_once() {
    let (root, data) = fresh("token");
    let password = password_file(&root);
    let created = run(
        &data,
        &[
            "user",
            "create",
            "--username",
            "ada",
            "--display-name",
            "Ada Lovelace",
            "--password-file",
            password.to_str().unwrap(),
            "--reason",
            "admin test",
        ],
    );
    assert_eq!(created.status.code(), Some(0), "{}", text(&created.stderr));
    let user = json_of(&created);
    assert_eq!(user["user"]["actor_identity_ref"], "human:ada");
    assert_eq!(
        user["user"]["scopes"],
        serde_json::json!(["read", "work.write", "project.write"])
    );

    let issued = run(
        &data,
        &[
            "token",
            "issue",
            "--actor",
            "human:ada",
            "--name",
            "claude-code",
            "--scopes",
            "read,work.write",
            "--reason",
            "admin test",
        ],
    );
    assert_eq!(issued.status.code(), Some(0), "{}", text(&issued.stderr));
    let body = json_of(&issued);
    assert!(
        body["secret"]
            .as_str()
            .is_some_and(|secret| secret.len() > 20),
        "{body}"
    );
    assert_eq!(body["token"]["name"], "claude-code");
    assert!(body["warning"].as_str().is_some_and(|w| !w.is_empty()));
}

#[test]
fn token_issue_refuses_an_unknown_scope_and_an_unknown_actor() {
    let (root, data) = fresh("token-refusals");
    let password = password_file(&root);
    let _ = run(
        &data,
        &[
            "user",
            "create",
            "--username",
            "ada",
            "--password-file",
            password.to_str().unwrap(),
            "--reason",
            "admin test",
        ],
    );
    let bad_scope = run(
        &data,
        &[
            "token",
            "issue",
            "--actor",
            "human:ada",
            "--name",
            "x",
            "--scopes",
            "read,bogus",
            "--reason",
            "admin test",
        ],
    );
    assert_eq!(bad_scope.status.code(), Some(1));
    assert!(
        text(&bad_scope.stderr).contains("invalid_request: unknown scope 'bogus'"),
        "{}",
        text(&bad_scope.stderr)
    );

    let no_actor = run(
        &data,
        &[
            "token",
            "issue",
            "--actor",
            "human:nobody",
            "--name",
            "x",
            "--reason",
            "admin test",
        ],
    );
    assert_eq!(no_actor.status.code(), Some(1));
    assert!(
        text(&no_actor.stderr).contains("not_found"),
        "{}",
        text(&no_actor.stderr)
    );
}

#[test]
fn a_non_integer_expiry_is_an_argparse_usage_error_not_the_help_page() {
    let (_root, data) = fresh("expiry");
    let output = run(
        &data,
        &[
            "token",
            "issue",
            "--actor",
            "human:ada",
            "--name",
            "x",
            "--expires-in-days",
            "x",
            "--reason",
            "admin test",
        ],
    );
    assert_eq!(output.status.code(), Some(2));
    let stderr = text(&output.stderr);
    // The usage synopsis, then the one error line argparse prints. The full
    // option table belongs to `--help`, not to a usage error.
    assert!(stderr.starts_with("usage: vogt token issue ["), "{stderr}");
    assert!(
        stderr.ends_with(
            "vogt token issue: error: argument --expires-in-days: invalid int value: 'x'\n"
        ),
        "{stderr}"
    );
    assert!(!stderr.contains("options:"), "{stderr}");
}

#[test]
fn user_create_refuses_an_unreadable_password_file_on_stderr() {
    let (root, data) = fresh("user-missing-password");
    let output = run(
        &data,
        &[
            "user",
            "create",
            "--username",
            "bob",
            "--password-file",
            root.join("missing").to_str().unwrap(),
            "--reason",
            "admin test",
        ],
    );
    // Python's uncaught OSError exits 1; the message goes to stderr, nothing to stdout.
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(
        text(&output.stderr).contains("cannot read secret file"),
        "{}",
        text(&output.stderr)
    );
}

#[test]
fn backup_then_restore_needs_confirm_and_then_restores() {
    let (root, data) = fresh("backup");
    let destination = root.join("backup");
    let backup = run(
        &data,
        &[
            "backup",
            "--destination",
            destination.to_str().unwrap(),
            "--label",
            "admin-test",
            "--reason",
            "admin test",
        ],
    );
    assert_eq!(backup.status.code(), Some(0), "{}", text(&backup.stderr));
    assert!(destination.join("manifest.json").is_file());

    // Backing up over a non-empty directory is refused, and writes nothing.
    let again = run(
        &data,
        &[
            "backup",
            "--destination",
            destination.to_str().unwrap(),
            "--label",
            "admin-test",
            "--reason",
            "admin test",
        ],
    );
    assert_eq!(again.status.code(), Some(1));
    assert!(
        text(&again.stderr).contains("conflict"),
        "{}",
        text(&again.stderr)
    );

    // Restore without --confirm is refused before anything is replaced.
    let unconfirmed = run(
        &data,
        &[
            "restore",
            "--source",
            destination.to_str().unwrap(),
            "--no-confirm",
            "--reason",
            "admin test",
        ],
    );
    assert_eq!(unconfirmed.status.code(), Some(1));
    assert!(
        text(&unconfirmed.stderr).contains("Pass --confirm."),
        "{}",
        text(&unconfirmed.stderr)
    );

    let restored = run(
        &data,
        &[
            "restore",
            "--source",
            destination.to_str().unwrap(),
            "--confirm",
            "--reason",
            "admin test",
        ],
    );
    assert_eq!(
        restored.status.code(),
        Some(0),
        "{}",
        text(&restored.stderr)
    );
}

#[test]
fn restore_of_a_directory_that_is_not_a_backup_says_so() {
    let (root, data) = fresh("restore-missing");
    let output = run(
        &data,
        &[
            "restore",
            "--source",
            root.join("nope").to_str().unwrap(),
            "--confirm",
            "--reason",
            "admin test",
        ],
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(
        text(&output.stderr).contains("not_found")
            && text(&output.stderr).contains("manifest.json"),
        "{}",
        text(&output.stderr)
    );
}

#[test]
fn a_missing_required_argument_is_usage_before_a_secret_file_is_read() {
    let (root, data) = fresh("required-before-secret");
    // `--username` is absent. The password file is also missing, so reading it
    // first would report that file instead of the usage error.
    let output = run(
        &data,
        &[
            "user",
            "create",
            "--password-file",
            root.join("missing").to_str().unwrap(),
            "--reason",
            "admin test",
        ],
    );
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty(), "{}", text(&output.stdout));
    let stderr = text(&output.stderr);
    assert!(stderr.starts_with("usage: vogt user create ["), "{stderr}");
    assert!(
        stderr.ends_with(
            "vogt user create: error: the following arguments are required: --username\n"
        ),
        "{stderr}"
    );
    assert!(!stderr.contains("cannot read secret file"), "{stderr}");
}
