//! `scripts/gen_config_docs.py` in Rust.
//!
//! Regenerates the artefacts derived from the config schema. The committed
//! files must match, so a schema change that forgets to regenerate is caught
//! by `git diff --exit-code` in CI.

use std::path::Path;

pub fn run(root: &Path) -> Result<(), String> {
    for (path, content) in crate::config::config_artifacts(root) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|err| format!("{parent:?}: {err}"))?;
        }
        let existing = std::fs::read_to_string(&path).ok();
        let relative = path.strip_prefix(root).unwrap_or(&path).display();
        if existing.as_deref() == Some(content.as_str()) {
            println!("unchanged  {relative}");
            continue;
        }
        std::fs::write(&path, &content).map_err(|err| format!("{relative}: {err}"))?;
        println!("written    {relative}");
    }
    Ok(())
}
