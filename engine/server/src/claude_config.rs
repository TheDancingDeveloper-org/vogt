//! Answering Claude Code's per-directory startup questions before it asks.
//!
//! A session the engine starts is often driven by another agent, or woken
//! by policy with nobody watching, and a modal at startup reads to both as a
//! hung session (WI-912). Claude Code asks two such questions per working
//! directory, and remembers each answer in its global config
//! (`~/.claude.json`, or `$CLAUDE_CONFIG_DIR/.claude.json`) under
//! `projects["<cwd>"]`:
//!
//! - **folder trust** — "Do you trust the files in this folder?"
//!   (`hasTrustDialogAccepted`);
//! - **external `CLAUDE.md` imports** — a `CLAUDE.md` that imports a file
//!   from outside the directory (`hasClaudeMdExternalIncludesApproved`, and
//!   `hasClaudeMdExternalIncludesWarningShown` so the warning is not shown).
//!
//! The engine only ever starts sessions inside `workspace_root`, which the
//! operator trusts by deploying it, so it records both answers for the
//! session's directory immediately before the spawn. Best effort, and
//! skipped with `ENGINE_AGENT_QUIET_ONBOARDING=0` (the same switch as the
//! entrypoint's "auto mode" dismissal): a config this cannot read or parse is
//! left exactly as it was, and losing a race with a running Claude Code that
//! rewrites the file only brings back the dialog it would have shown anyway.

use std::path::{Path, PathBuf};

const KEYS: &[&str] = &[
    "hasTrustDialogAccepted",
    "hasClaudeMdExternalIncludesApproved",
    "hasClaudeMdExternalIncludesWarningShown",
];

/// Whether, and where, the engine answers the questions. Off by default —
/// which is what a test's hand-built config gets, so a test never writes the
/// real `~/.claude.json` — and read from the environment by `config::load`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Onboarding {
    pub enabled: bool,
    /// The directory Claude Code's `.claude.json` is in when a session does
    /// not set `CLAUDE_CONFIG_DIR` itself: the engine's `CLAUDE_CONFIG_DIR`,
    /// else `$HOME`.
    pub default_dir: Option<PathBuf>,
    /// The driven-session permission policy (WI-926): a Claude Code settings
    /// file, passed to every engine-launched Claude session with
    /// `--settings`. See [`settings_file_from`].
    pub settings_file: Option<PathBuf>,
    /// The opencode half of that policy (WI-932): an opencode config whose
    /// `permission` block every engine-launched opencode session gets in the
    /// default posture, as `OPENCODE_CONFIG_CONTENT`. Read and checked once,
    /// at start. See [`opencode_config_from`].
    pub opencode_config: Option<String>,
}

/// `ENGINE_AGENT_CLAUDE_SETTINGS`, read: a path names a deployment's own
/// policy; unset or empty means the image's, when it is there (Compose
/// passes an unset `.env` value as empty, so empty must not mean off); `off`
/// turns the policy off.
pub fn settings_file_from(value: Option<&str>) -> Option<PathBuf> {
    match value.map(str::trim) {
        Some("off") => None,
        Some(path) if !path.is_empty() => Some(PathBuf::from(path)),
        _ => Some(PathBuf::from(IMAGE_SETTINGS_FILE)).filter(|p| p.is_file()),
    }
}

/// Where the image installs the default driven-session policy.
pub const IMAGE_SETTINGS_FILE: &str = "/usr/local/share/vogt/driven-session-settings.json";

/// Where the image installs the default opencode driven-session policy.
pub const IMAGE_OPENCODE_CONFIG: &str = "/usr/local/share/vogt/driven-session-opencode.json";

/// The permission values opencode accepts. Anything else makes opencode
/// refuse its whole configuration and fail to start, so a policy with one is
/// not handed to any session.
const OPENCODE_ACTIONS: &[&str] = &["allow", "ask", "deny"];

/// `ENGINE_AGENT_OPENCODE_CONFIG`, read like `ENGINE_AGENT_CLAUDE_SETTINGS`
/// (a path, empty for the image's, `off`), and loaded: the file's text when it
/// is a JSON object whose `permission` values opencode would accept, else
/// `None` with a warning. A broken policy must not stop every opencode session
/// from starting.
pub fn opencode_config_from(value: Option<&str>) -> Option<String> {
    let path = match value.map(str::trim) {
        Some("off") => return None,
        Some(path) if !path.is_empty() => PathBuf::from(path),
        _ => PathBuf::from(IMAGE_OPENCODE_CONFIG),
    };
    let text = std::fs::read_to_string(&path).ok()?;
    match check_opencode_config(&text) {
        Ok(()) => Some(text),
        Err(why) => {
            tracing::warn!(path = %path.display(), reason = %why, "opencode driven-session policy ignored");
            None
        }
    }
}

/// Why opencode would refuse this config, if it would.
pub fn check_opencode_config(text: &str) -> std::result::Result<(), String> {
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|e| format!("not JSON: {e}"))?;
    let permission = value
        .as_object()
        .ok_or("not a JSON object")?
        .get("permission");
    let Some(permission) = permission else {
        return Ok(());
    };
    let ok = |v: &serde_json::Value| v.as_str().is_some_and(|s| OPENCODE_ACTIONS.contains(&s));
    let rules = permission
        .as_object()
        .ok_or("`permission` is not an object")?;
    for (tool, rule) in rules {
        let valid = match rule {
            serde_json::Value::Object(patterns) => patterns.values().all(ok),
            other => ok(other),
        };
        if !valid {
            return Err(format!(
                "permission.{tool}: every value must be one of allow, ask, deny"
            ));
        }
    }
    Ok(())
}

impl Onboarding {
    /// On unless `ENGINE_AGENT_QUIET_ONBOARDING=0`.
    pub fn from_env() -> Self {
        Self {
            enabled: std::env::var("ENGINE_AGENT_QUIET_ONBOARDING")
                .map_or(true, |v| v.trim() != "0"),
            default_dir: std::env::var_os("CLAUDE_CONFIG_DIR")
                .or_else(|| std::env::var_os("HOME"))
                .map(PathBuf::from),
            settings_file: settings_file_from(
                std::env::var("ENGINE_AGENT_CLAUDE_SETTINGS")
                    .ok()
                    .as_deref(),
            ),
            opencode_config: opencode_config_from(
                std::env::var("ENGINE_AGENT_OPENCODE_CONFIG")
                    .ok()
                    .as_deref(),
            ),
        }
    }

    /// Claude Code's global config file for a session with `env`.
    pub fn config_path(&self, env: &[(String, String)]) -> Option<PathBuf> {
        env.iter()
            .rev()
            .find(|(k, _)| k == "CLAUDE_CONFIG_DIR")
            .map(|(_, v)| PathBuf::from(v))
            .or_else(|| self.default_dir.clone())
            .map(|dir| dir.join(".claude.json"))
    }

    /// [`accept_project`] for a Claude session about to start in `cwd`,
    /// logging rather than failing: the session starts either way.
    pub fn prepare(&self, env: &[(String, String)], cwd: &str) {
        if !self.enabled {
            return;
        }
        let Some(path) = self.config_path(env) else {
            return;
        };
        if let Err(e) = accept_project(&path, cwd) {
            tracing::info!(path = %path.display(), error = %e, "could not pre-accept Claude Code's folder trust; it may ask");
        }
    }
}

/// Record, in the config at `path`, that `cwd` is trusted and its external
/// `CLAUDE.md` imports approved. `Ok(false)` when nothing needed writing.
pub fn accept_project(path: &Path, cwd: &str) -> std::io::Result<bool> {
    let mut config: serde_json::Value = match std::fs::read(path) {
        Ok(bytes) => match serde_json::from_slice(&bytes) {
            Ok(value) => value,
            // Not ours to repair.
            Err(_) => return Ok(false),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => serde_json::json!({}),
        Err(e) => return Err(e),
    };
    let Some(root) = config.as_object_mut() else {
        return Ok(false);
    };
    let projects = root
        .entry("projects")
        .or_insert_with(|| serde_json::json!({}));
    let Some(projects) = projects.as_object_mut() else {
        return Ok(false);
    };
    let project = projects
        .entry(cwd.to_string())
        .or_insert_with(|| serde_json::json!({}));
    let Some(project) = project.as_object_mut() else {
        return Ok(false);
    };
    if KEYS
        .iter()
        .all(|k| project.get(*k) == Some(&serde_json::Value::Bool(true)))
    {
        return Ok(false);
    }
    for key in KEYS {
        project.insert((*key).to_string(), serde_json::Value::Bool(true));
    }
    let body = serde_json::to_vec_pretty(&config).map_err(std::io::Error::other)?;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let tmp = parent.join(format!(".claude.json.vogt-{}", uuid::Uuid::new_v4()));
    let result = (|| {
        std::fs::write(&tmp, &body)?;
        if let Ok(meta) = std::fs::metadata(path) {
            std::fs::set_permissions(&tmp, meta.permissions())?;
        }
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result.map(|()| true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trust_is_recorded_once_and_everything_else_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".claude.json");
        std::fs::write(
            &path,
            r#"{"autoModeEnvSetup":{"dismissed":true},"projects":{"/w/other":{"x":1},"/w/repo":{"allowedTools":["Bash"]}}}"#,
        )
        .unwrap();
        assert!(accept_project(&path, "/w/repo").unwrap());
        let config: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let repo = &config["projects"]["/w/repo"];
        for key in KEYS {
            assert_eq!(repo[*key], true, "{key}");
        }
        assert_eq!(repo["allowedTools"][0], "Bash");
        assert_eq!(config["projects"]["/w/other"]["x"], 1);
        assert_eq!(config["autoModeEnvSetup"]["dismissed"], true);
        assert!(
            !accept_project(&path, "/w/repo").unwrap(),
            "nothing left to write"
        );
    }

    #[test]
    fn a_missing_config_is_created_and_a_broken_one_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join(".claude.json");
        assert!(accept_project(&path, "/w/repo").unwrap());
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .contains("hasTrustDialogAccepted"));
        let broken = dir.path().join("broken.json");
        std::fs::write(&broken, b"{nope").unwrap();
        assert!(!accept_project(&broken, "/w/repo").unwrap());
        assert_eq!(std::fs::read(&broken).unwrap(), b"{nope");
    }

    #[test]
    fn the_policy_setting_reads_a_path_off_or_the_image_default() {
        assert_eq!(
            settings_file_from(Some("/srv/policy.json")),
            Some(PathBuf::from("/srv/policy.json"))
        );
        assert_eq!(settings_file_from(Some("off")), None);
        // Unset and empty mean the same: the image's file, when it exists.
        assert_eq!(settings_file_from(Some("")), settings_file_from(None));
    }

    #[test]
    fn the_session_config_dir_wins() {
        let onboarding = Onboarding {
            enabled: true,
            default_dir: Some(PathBuf::from("/home/pod")),
            settings_file: None,
            opencode_config: None,
        };
        let env = vec![("CLAUDE_CONFIG_DIR".to_string(), "/cfg".to_string())];
        assert_eq!(
            onboarding.config_path(&env),
            Some(PathBuf::from("/cfg/.claude.json"))
        );
        assert_eq!(
            onboarding.config_path(&[]),
            Some(PathBuf::from("/home/pod/.claude.json"))
        );
    }

    #[test]
    fn the_shipped_opencode_policy_is_one_opencode_accepts() {
        let shipped = include_str!("../../deploy/driven-session-opencode.json");
        assert_eq!(check_opencode_config(shipped), Ok(()));
        assert_eq!(
            check_opencode_config(crate::agent_cli::OPENCODE_BYPASS),
            Ok(())
        );
        assert_eq!(
            check_opencode_config(crate::agent_cli::OPENCODE_ACCEPT_EDITS),
            Ok(())
        );
    }

    #[test]
    fn an_opencode_policy_opencode_would_refuse_is_never_handed_out() {
        assert!(check_opencode_config(r#"{"permission":{"bash":"maybe"}}"#).is_err());
        assert!(check_opencode_config(r#"{"permission":{"bash":{"*":"yes"}}}"#).is_err());
        assert!(check_opencode_config("not json").is_err());
        assert!(check_opencode_config("[]").is_err());
        let tmp = tempfile::tempdir().unwrap();
        let bad = tmp.path().join("bad.json");
        std::fs::write(&bad, r#"{"permission":{"edit":"sometimes"}}"#).unwrap();
        assert_eq!(opencode_config_from(bad.to_str()), None);
        let good = tmp.path().join("good.json");
        std::fs::write(&good, r#"{"permission":{"edit":"allow"}}"#).unwrap();
        assert!(opencode_config_from(good.to_str()).is_some());
        assert_eq!(opencode_config_from(Some("off")), None);
    }
}
