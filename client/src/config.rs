//! Client configuration: the server base URL and API bearer token.
//!
//! Persisted as JSON under the platform config dir
//! (`%APPDATA%\mydevenv2-client\config.json` on Windows,
//! `~/.config/mydevenv2-client/config.json` on Linux). The token gates every
//! server route except `/healthz` and `/api/config`, so it is stored locally
//! and sent as `Authorization: Bearer` (HTTP) or the first `auth` frame (WS).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

const DEFAULT_SERVER_URL: &str = "https://mydevenv2.sprooty.com";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientConfig {
    /// Base URL, e.g. `https://mydevenv2.sprooty.com` (no trailing slash).
    pub server_url: String,
    /// API bearer token (`MYDEVENV2_TOKEN` on the server).
    #[serde(default)]
    pub token: String,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            server_url: DEFAULT_SERVER_URL.to_string(),
            token: String::new(),
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
        std::fs::write(&path, json).map_err(|e| e.to_string())
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
