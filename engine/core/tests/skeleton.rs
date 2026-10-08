//! Shape of this chunk: version, help, init migrates, serve answers health.

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
fn serve_help_names_the_flags() {
    let output = binary()
        .args(["serve", "--help"])
        .output()
        .expect("run help");
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("--data-dir"), "{stdout}");
    assert!(stdout.contains("--port"), "{stdout}");
}

#[test]
fn init_migrates_and_a_second_run_applies_nothing() {
    let dir = std::env::temp_dir().join(format!("vogt-core-init-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let first = binary()
        .args(["--data-dir"])
        .arg(&dir)
        .arg("init")
        .output()
        .expect("init");
    assert!(first.status.success(), "{first:?}");
    let stdout = String::from_utf8(first.stdout).unwrap();
    assert!(stdout.contains("created=true"), "{stdout}");
    assert!(dir.join("declared.sqlite3").is_file());
    assert!(dir.join("observed.sqlite3").is_file());

    let second = binary()
        .args(["--data-dir"])
        .arg(&dir)
        .arg("init")
        .output()
        .expect("init again");
    assert!(second.status.success(), "{second:?}");
    let stdout = String::from_utf8(second.stdout).unwrap();
    assert!(stdout.contains("created=false"), "{stdout}");
    assert!(stdout.contains("migrations_applied="), "{stdout}");
    let _ = std::fs::remove_dir_all(&dir);
}
