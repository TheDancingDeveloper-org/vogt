//! The skeleton's only contract: it builds, prints a version, and its `serve`
//! subcommand has help. Behaviour arrives with later chunks.

use std::process::Command;

fn binary() -> Command {
    Command::new(env!("CARGO_BIN_EXE_vogt-core"))
}

#[test]
fn version_names_the_binary() {
    let output = binary().arg("--version").output().expect("run --version");
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.starts_with("vogt-core "), "{stdout}");
}

#[test]
fn serve_help_succeeds_and_names_the_command() {
    let output = binary()
        .args(["serve", "--help"])
        .output()
        .expect("run help");
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("vogt-core serve"), "{stdout}");
}

#[test]
fn serve_is_not_implemented_yet() {
    let output = binary().arg("serve").output().expect("run serve");
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("not implemented"), "{stderr}");
}
