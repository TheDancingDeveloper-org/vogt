//! The managed Codex policy check, shared by the engine and session agent.

use std::path::{Path, PathBuf};

/// The managed requirements path Codex reads, and the only one: the fork's
/// loader has no flag or variable for another (WI-1146).
pub const CODEX_REQUIREMENTS_FILE: &str = "/etc/codex/requirements.toml";

/// Check the managed requirements file. It is validated only when present so
/// malformed policy cannot silently become a successful launch.
pub fn guarded(system: &Path) -> Result<Option<PathBuf>, String> {
    let path = readable_policy(system.to_path_buf())?;
    Ok(path)
}

/// Read the requirements file; `None` when it is absent, an error when it
/// is present but empty-or-malformed TOML.
pub fn readable_policy(path: PathBuf) -> Result<Option<PathBuf>, String> {
    match std::fs::read(&path) {
        Ok(bytes) if !bytes.is_empty() => {
            let text = String::from_utf8_lossy(&bytes);
            check_codex_policy(&text).map_err(|why| {
                format!("Codex driven-session policy {} is invalid ({why})", path.display())
            })?;
            Ok(Some(path))
        }
        Ok(_) => Err(format!(
            "Codex driven-session policy {} is empty; refusing to start a Codex session without a valid managed policy",
            path.display()
        )),
        Err(e) => Err(format!(
            "Codex driven-session policy {} cannot be read ({e}); refusing to start a Codex session without a valid managed policy",
            path.display()
        )),
    }
}

/// Validate that the requirements file is TOML. No external executable or hook
/// is required, so this only rejects malformed policy.
pub fn check_codex_policy(text: &str) -> std::result::Result<(), String> {
    let _: toml::Table = toml::from_str(text).map_err(|e| format!("not TOML: {e}"))?;
    Ok(())
}
