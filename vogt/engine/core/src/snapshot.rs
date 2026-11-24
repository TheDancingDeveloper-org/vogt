//! Rust-native snapshot files for the core's published surfaces (WI-4467).
//!
//! The registry dump, the MCP `tools/list` reply and the OpenAPI document are
//! contracts: a change to any of them must be deliberate. Each test renders the
//! surface and compares it, as pretty-printed JSON text (so field order is
//! compared too), with `engine/core/tests/snapshots/<name>.json`.
//!
//! The files are generated from the Rust implementation. After an intended
//! change, regenerate them from `engine/` and review the diff:
//!
//! ```sh
//! UPDATE_SNAPSHOTS=1 cargo test -p vogt-core snapshot
//! ```
//!
//! These replace the Python-recorded parity goldens; the Python core is frozen.

use serde_json::Value;
use std::path::PathBuf;

fn path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/snapshots")
        .join(format!("{name}.json"))
}

/// Compare `actual` with the snapshot `name`, or rewrite the snapshot when
/// `UPDATE_SNAPSHOTS` is set. On a mismatch, `describe` names what moved.
pub fn assert_json_snapshot(name: &str, actual: &Value, describe: impl Fn(&Value) -> String) {
    let file = path(name);
    let rendered = format!("{}\n", serde_json::to_string_pretty(actual).unwrap());
    if std::env::var_os("UPDATE_SNAPSHOTS").is_some() {
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, &rendered).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&file).unwrap_or_else(|err| {
        panic!(
            "no snapshot {} ({err}); generate it with UPDATE_SNAPSHOTS=1 cargo test -p vogt-core snapshot",
            file.display()
        )
    });
    if rendered != expected {
        let expected: Value = serde_json::from_str(&expected).unwrap();
        let detail = if &expected == actual {
            "field order only".to_string()
        } else {
            describe(&expected)
        };
        panic!(
            "{name} drifted from {}: {detail}\nIf the change is intended, run \
             UPDATE_SNAPSHOTS=1 cargo test -p vogt-core snapshot and commit the diff.",
            file.display()
        );
    }
}

/// The entries of the array at `key` whose `id_field` differs, is added or is
/// removed between `expected` and `actual`.
pub fn changed_entries(expected: &Value, actual: &Value, key: &str, id_field: &str) -> String {
    let index = |doc: &Value| -> Vec<(String, Value)> {
        doc[key]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .map(|item| {
                        (
                            item[id_field].as_str().unwrap_or("?").to_string(),
                            item.clone(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    let (old, new) = (index(expected), index(actual));
    let mut out = Vec::new();
    for (name, value) in &new {
        match old.iter().find(|(n, _)| n == name) {
            None => out.push(format!("+{name}")),
            Some((_, prev)) if prev != value => out.push(format!("~{name}")),
            _ => {}
        }
    }
    for (name, _) in &old {
        if !new.iter().any(|(n, _)| n == name) {
            out.push(format!("-{name}"));
        }
    }
    let order = |v: &[(String, Value)]| v.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>();
    if out.is_empty() && order(&old) != order(&new) {
        out.push("order changed".into());
    }
    if out.is_empty() {
        out.push("fields outside the entries changed".into());
    }
    out.join(", ")
}
