//! Client configuration: the server base URL and API bearer token.
//!
//! Persisted as JSON under the platform config dir
//! (`%APPDATA%\mydevenv2-client\config.json` on Windows,
//! `~/.config/mydevenv2-client/config.json` on Linux). The token gates every
//! server route except `/healthz` and `/api/config`, so it is stored locally
//! and sent as `Authorization: Bearer` (HTTP) or the first `auth` frame (WS).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const DEFAULT_SERVER_URL: &str = "https://mydevenv2.sprooty.com";

/// Default terminal font size persisted when the user has not changed zoom.
pub const DEFAULT_FONT_SIZE: f32 = 15.0;
/// Default sidebar width in logical pixels.
pub const DEFAULT_SIDEBAR_WIDTH: f32 = 288.0;

fn default_font_size() -> f32 {
    DEFAULT_FONT_SIZE
}

fn default_sidebar_width() -> f32 {
    DEFAULT_SIDEBAR_WIDTH
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientConfig {
    /// Base URL, e.g. `https://mydevenv2.sprooty.com` (no trailing slash).
    pub server_url: String,
    /// API bearer token (`MYDEVENV2_TOKEN` on the server).
    #[serde(default)]
    pub token: String,
    /// Persisted terminal zoom level (font size in px). (#3)
    #[serde(default = "default_font_size")]
    pub font_size: f32,
    /// Persisted left-sidebar width in logical px. (#8)
    #[serde(default = "default_sidebar_width")]
    pub sidebar_width: f32,
    /// Whether the sidebar is collapsed to the icon rail. (#8)
    #[serde(default)]
    pub sidebar_collapsed: bool,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            server_url: DEFAULT_SERVER_URL.to_string(),
            token: String::new(),
            font_size: DEFAULT_FONT_SIZE,
            sidebar_width: DEFAULT_SIDEBAR_WIDTH,
            sidebar_collapsed: false,
        }
    }
}

impl ClientConfig {
    /// Path to the on-disk config file, creating the parent dir if needed.
    pub fn path() -> PathBuf {
        let dir = dirs_next::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("mydevenv2-client");
        let _ = std::fs::create_dir_all(&dir);
        dir.join("config.json")
    }

    /// Load config from disk, falling back to defaults on any error.
    pub fn load() -> Self {
        let path = Self::path();
        match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                log::warn!("config parse failed ({e}); using defaults");
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    /// Persist config to disk. Returns an error string on failure.
    pub fn save(&self) -> Result<(), String> {
        let path = Self::path();
        let json = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(&path, json).map_err(|e| e.to_string())?;
        secure_config_file(&path).map_err(|e| e.to_string())
    }

    /// Base URL with any trailing slash trimmed.
    pub fn base(&self) -> &str {
        self.server_url.trim_end_matches('/')
    }

    /// True if enough is configured to attempt a connection.
    pub fn is_configured(&self) -> bool {
        !self.base().is_empty() && !self.token.is_empty()
    }
}

#[cfg(unix)]
fn secure_config_file(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_mode(0o600);
    std::fs::set_permissions(path, perms)
}

#[cfg(not(unix))]
fn secure_config_file(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_trims_trailing_slashes() {
        let cfg = ClientConfig {
            server_url: "https://mydevenv2.sprooty.com///".into(),
            token: "tok".into(),
            ..Default::default()
        };
        assert_eq!(cfg.base(), "https://mydevenv2.sprooty.com");
    }

    #[test]
    fn empty_token_is_not_configured() {
        let cfg = ClientConfig {
            server_url: "https://mydevenv2.sprooty.com".into(),
            token: String::new(),
            ..Default::default()
        };
        assert!(!cfg.is_configured());
    }

    #[test]
    fn config_defaults_are_sane() {
        let cfg = ClientConfig::default();
        assert_eq!(cfg.font_size, DEFAULT_FONT_SIZE);
        assert_eq!(cfg.sidebar_width, DEFAULT_SIDEBAR_WIDTH);
        assert!(!cfg.sidebar_collapsed);
    }

    #[test]
    fn config_round_trips_new_fields() {
        let cfg = ClientConfig {
            font_size: 20.0,
            sidebar_width: 320.0,
            sidebar_collapsed: true,
            ..Default::default()
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let back: ClientConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.font_size, 20.0);
        assert_eq!(back.sidebar_width, 320.0);
        assert!(back.sidebar_collapsed);
    }

    #[test]
    fn config_tolerates_missing_new_fields() {
        // An older config file without the new keys must still load.
        let json = r#"{"server_url":"https://x","token":"t"}"#;
        let cfg: ClientConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.font_size, DEFAULT_FONT_SIZE);
        assert_eq!(cfg.sidebar_width, DEFAULT_SIDEBAR_WIDTH);
    }
}
