//! The committed disposition of every Python `tests/test_*.py`.
//!
//! WI-1080: a file is ported, retired-with-reason, or unported and deferred
//! to a named work item, and a new Python test with none of these fails
//! this test. The ledger is `docs/PYTHON_TEST_DISPOSITION.md`.

use std::collections::BTreeSet;
use std::path::PathBuf;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn ledger_files() -> BTreeSet<String> {
    let text = std::fs::read_to_string(repo_root().join("docs/PYTHON_TEST_DISPOSITION.md"))
        .expect("disposition ledger");
    text.lines()
        .filter(|line| line.starts_with("| test_"))
        .map(|line| {
            let mut cells = line.split('|').map(str::trim);
            let _ = cells.next();
            let file = cells.next().expect("file cell");
            let disposition = cells.next().expect("disposition cell");
            let why = cells.next().expect("reason cell");
            assert!(
                matches!(disposition, "ported" | "retired" | "unported"),
                "{file} has disposition {disposition:?}, want ported, retired or unported"
            );
            assert!(!why.is_empty(), "{file} has an empty reason");
            assert!(
                disposition != "unported" || why.contains("Deferred to WI-"),
                "{file} is unported without an owner: say \"Deferred to WI-<n>\""
            );
            assert!(file.starts_with("test_") && file.ends_with(".py"));
            file.to_string()
        })
        .collect()
}

fn python_tests() -> BTreeSet<String> {
    std::fs::read_dir(repo_root().join("tests"))
        .expect("tests/")
        .map(|entry| {
            entry
                .expect("dir entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter(|name| name.starts_with("test_") && name.ends_with(".py"))
        .collect()
}

#[test]
fn every_python_test_has_one_disposition() {
    let ledger = ledger_files();
    let files = python_tests();
    let missing: Vec<_> = files.difference(&ledger).cloned().collect();
    let extra: Vec<_> = ledger.difference(&files).cloned().collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "disposition ledger drifted.\nmissing a row: {missing:?}\nrow for a file that is gone: {extra:?}"
    );
}
