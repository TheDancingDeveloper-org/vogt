use std::{net::SocketAddr, path::Path};

use serde::Deserialize;

use crate::error::{ApiError, Result};

#[derive(Debug, Clone)]
pub struct Config {
    pub bind: SocketAddr,
    pub token: String,
    pub scrollback_bytes: usize,
    pub default_shell: String,
    pub default_cwd: std::path::PathBuf,
    pub activity_idle_after_ms: u64,
    /// Root the file API operates inside. Any request path is resolved
    /// against this and rejected if it escapes the root.
    pub workspace_root: std::path::PathBuf,
}

#[derive(Debug, Default, Deserialize)]
struct FileConfig {
    bind: Option<String>,
    token: Option<String>,
    scrollback_bytes: Option<usize>,
    default_shell: Option<String>,
    default_cwd: Option<String>,
    activity_idle_after_ms: Option<u64>,
    workspace_root: Option<String>,
}

pub fn load(
    file: Option<&Path>,
    cli_bind: Option<String>,
    cli_token: Option<String>,
) -> Result<Config> {
    let from_file: FileConfig = match file {
        Some(path) => {
            let raw = std::fs::read_to_string(path)
                .map_err(|e| ApiError::Config(format!("reading {}: {e}", path.display())))?;
            toml::from_str(&raw)
                .map_err(|e| ApiError::Config(format!("parsing {}: {e}", path.display())))?
        }
        None => FileConfig::default(),
    };

    let bind_str = cli_bind
        .or(from_file.bind)
        .unwrap_or_else(|| "127.0.0.1:8910".to_string());
    let bind: SocketAddr = bind_str
        .parse()
        .map_err(|e| ApiError::Config(format!("invalid bind {bind_str:?}: {e}")))?;

    let token = cli_token.or(from_file.token).ok_or_else(|| {
        ApiError::Config("token required (MYDEVENV2_TOKEN env or config.token)".into())
    })?;
    if token.len() < 16 {
        return Err(ApiError::Config(
            "token must be at least 16 characters".into(),
        ));
    }

    let workspace_root_raw = from_file.workspace_root.map(std::path::PathBuf::from);
    let workspace_root = workspace_root_raw
        .or_else(|| dirs_home().map(|h| h.join("Working")))
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| std::path::PathBuf::from("/"));
    let workspace_root = workspace_root
        .canonicalize()
        .map_err(|e| ApiError::Config(format!("workspace_root {workspace_root:?}: {e}")))?;

    Ok(Config {
        bind,
        token,
        scrollback_bytes: from_file.scrollback_bytes.unwrap_or(256 * 1024),
        default_shell: from_file
            .default_shell
            .or_else(|| std::env::var("SHELL").ok())
            .unwrap_or_else(|| "/bin/bash".to_string()),
        default_cwd: from_file
            .default_cwd
            .map(std::path::PathBuf::from)
            .or_else(dirs_home)
            .unwrap_or_else(|| std::path::PathBuf::from("/tmp")),
        activity_idle_after_ms: from_file.activity_idle_after_ms.unwrap_or(1_500),
        workspace_root,
    })
}

fn dirs_home() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME").map(std::path::PathBuf::from)
}
