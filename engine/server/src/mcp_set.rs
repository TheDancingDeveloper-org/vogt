//! Which MCP servers a session starts with (WI-2740).
//!
//! Every tool definition rides on every request a model makes. The full
//! estate set (vogt, cadastre, komodo, grafana, github, forgejo) is about
//! 85k tokens of schema before the brief is read, which pushed a Codex worker
//! over its auto-compact limit on its first request. A worker or review
//! session needs the work tracker and the inventory, so it starts with the
//! `worker` set: `vogt` under its worker tool profile, `cadastre` and `crw`
//! (web scrape and search, about 3k tokens of schema; WI-2742).
//!
//! The trim is per session and never touches the shared home configs:
//! Claude Code gets `--strict-mcp-config --mcp-config=<file>` with a copy of
//! the kept entries from `~/.claude.json`; Codex gets `-c` overrides that
//! turn every other `[mcp_servers]` entry off and point `vogt` at its worker
//! profile.
//!
//! A project can add servers on top of the worker set (`mcp_extra`,
//! WI-2746). Add-only: the worker servers are always kept, and an extra name
//! the host has not registered is warned about, never a launch failure.

use std::path::{Path, PathBuf};

use crate::error::{ApiError, Result};

/// The servers a worker session keeps.
pub const WORKER_SERVERS: &[&str] = &["vogt", "cadastre", "crw"];
/// The variable the `vogt-mcp-remote` bridge sends as its tool profile.
pub const TOOL_PROFILE_ENV: &str = "VOGT_MCP_TOOL_PROFILE";
/// The vogt tool profile a worker session asks for.
pub const WORKER_PROFILE: &str = "worker";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum McpSet {
    /// Whatever the user's own configs register.
    #[default]
    Full,
    /// [`WORKER_SERVERS`], with vogt's worker tool profile.
    Worker,
}

impl McpSet {
    /// The set a start names. Absent or empty is `full`; anything unknown is
    /// refused rather than started on a set nobody asked for.
    pub fn parse(raw: Option<&str>) -> Result<Self> {
        match raw.map(str::trim).filter(|s| !s.is_empty()) {
            None | Some("full") => Ok(Self::Full),
            Some("worker") => Ok(Self::Worker),
            Some(other) => Err(ApiError::BadRequest(format!(
                "mcp_set {other:?} is not one this engine knows (full, worker)"
            ))),
        }
    }
}

/// What a launch adds for a trimmed set.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct McpTrim {
    /// The `--mcp-config` file a Claude Code launch carries.
    pub claude_config: Option<PathBuf>,
    /// The `-c` overrides (flag and value pairs) a Codex launch carries.
    pub codex_args: Vec<String>,
    /// The servers a Claude Code launch gets, in order.
    pub claude_servers: Vec<String>,
    /// The servers a Codex launch keeps enabled, in order.
    pub codex_servers: Vec<String>,
    /// Extra names neither config registers.
    pub unregistered: Vec<String>,
}

/// The names a worker session keeps: [`WORKER_SERVERS`], then each extra
/// once, in the order given. Blank names are dropped.
pub fn worker_names(extra: &[String]) -> Vec<String> {
    let mut out: Vec<String> = WORKER_SERVERS.iter().map(|s| s.to_string()).collect();
    for name in extra.iter().map(|n| n.trim()).filter(|n| !n.is_empty()) {
        if !out.iter().any(|n| n == name) {
            out.push(name.to_string());
        }
    }
    out
}

/// The `mcpServers` object a worker Claude Code session gets: the kept
/// entries of `~/.claude.json`, vogt told to ask for its worker profile.
/// `extra` adds the project's servers (WI-2746); unregistered ones are skipped.
pub fn claude_worker_config(
    claude_json: &serde_json::Value,
    extra: &[String],
) -> serde_json::Value {
    let mut kept = serde_json::Map::new();
    if let Some(servers) = claude_json.get("mcpServers").and_then(|v| v.as_object()) {
        for name in worker_names(extra) {
            let Some(entry) = servers.get(&name) else {
                continue;
            };
            let mut entry = entry.clone();
            if name == "vogt" {
                set_worker_profile(&mut entry);
            }
            kept.insert(name, entry);
        }
    }
    serde_json::json!({ "mcpServers": kept })
}

/// A stdio entry passes the profile to the bridge in its env; an HTTP entry
/// takes it on the URL.
fn set_worker_profile(entry: &mut serde_json::Value) {
    let Some(obj) = entry.as_object_mut() else {
        return;
    };
    if let Some(url) = obj.get("url").and_then(|v| v.as_str()) {
        let url = with_profile(url);
        obj.insert("url".into(), serde_json::Value::String(url));
        return;
    }
    let env = obj
        .entry("env")
        .or_insert_with(|| serde_json::Value::Object(Default::default()));
    if let Some(env) = env.as_object_mut() {
        env.insert(
            TOOL_PROFILE_ENV.into(),
            serde_json::Value::String(WORKER_PROFILE.into()),
        );
    }
}

fn with_profile(url: &str) -> String {
    let sep = if url.contains('?') { '&' } else { '?' };
    format!("{url}{sep}profile={WORKER_PROFILE}")
}

/// The `-c` overrides that give a Codex session the worker set: every
/// server outside [`WORKER_SERVERS`] and `extra` disabled, vogt on its
/// worker profile.
pub fn codex_worker_args(config: &toml::Table, extra: &[String]) -> Vec<String> {
    let keep = worker_names(extra);
    let mut out = Vec::new();
    let Some(servers) = config.get("mcp_servers").and_then(|v| v.as_table()) else {
        return out;
    };
    for (name, entry) in servers {
        if !is_bare_key(name) {
            // A name that would need quoting cannot be addressed safely in a
            // dotted `-c` key; leave it as the user registered it.
            continue;
        }
        if !keep.contains(name) {
            out.extend([
                "-c".to_string(),
                format!("mcp_servers.{name}.enabled=false"),
            ]);
            continue;
        }
        if name != "vogt" {
            continue;
        }
        if let Some(url) = entry.get("url").and_then(|v| v.as_str()) {
            let value = toml::Value::String(with_profile(url));
            out.extend(["-c".to_string(), format!("mcp_servers.vogt.url={value}")]);
        } else if entry.get("command").is_some() {
            out.extend([
                "-c".to_string(),
                format!("mcp_servers.vogt.env.{TOOL_PROFILE_ENV}=\"{WORKER_PROFILE}\""),
            ]);
        }
    }
    out
}

fn is_bare_key(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// The per-session file a worker Claude Code launch reads, beside its brief.
pub fn claude_config_path(state_dir: &Path, session_id: uuid::Uuid) -> PathBuf {
    crate::prompt_files::session_prompt_dir(state_dir).join(format!("{session_id}.mcp.json"))
}

/// Build the trim for `set` from the user's configs under `home`.
/// `claude_json` is the Claude Code config the launch reads (the engine's
/// `$CLAUDE_CONFIG_DIR/.claude.json`, else `$HOME/.claude.json`; see
/// [`claude_json_path`]). A config that is missing, does not parse, or has no
/// `vogt` entry leaves that CLI untrimmed (warned), so a fault here never
/// stops a launch and never starts a worker without the work tracker.
///
/// `extra` is the project's `mcp_extra`: added to the worker set where the
/// configs register it; a name neither registers is warned and listed in
/// [`McpTrim::unregistered`].
pub fn prepare(
    set: McpSet,
    extra: &[String],
    home: Option<&Path>,
    claude_json: Option<&Path>,
    state_dir: &Path,
    session_id: uuid::Uuid,
) -> McpTrim {
    if set == McpSet::Full {
        return McpTrim::default();
    }
    let mut trim = McpTrim::default();
    match claude_json.map(|p| (p, read_json(p))) {
        Some((_, Some(claude))) => {
            let config = claude_worker_config(&claude, extra);
            if config["mcpServers"].get("vogt").is_none() {
                tracing::warn!(
                    "no vogt entry in the Claude config; worker MCP set not applied, full set"
                );
            } else {
                let path = claude_config_path(state_dir, session_id);
                match write_private(&path, &config) {
                    Ok(()) => {
                        trim.claude_config = Some(path);
                        trim.claude_servers = worker_names(extra)
                            .into_iter()
                            .filter(|n| config["mcpServers"].get(n).is_some())
                            .collect();
                    }
                    Err(e) => tracing::warn!(error = %e, "worker MCP config not written; full set"),
                }
            }
        }
        Some((path, None)) => tracing::warn!(
            path = %path.display(),
            "no readable Claude config; Claude MCP set left as is"
        ),
        None => tracing::warn!("no Claude config path; Claude MCP set left as is"),
    }
    let codex = home.and_then(|home| {
        std::fs::read_to_string(home.join(".codex").join("config.toml"))
            .ok()
            .and_then(|text| text.parse::<toml::Table>().ok())
    });
    let codex_registered: Vec<String> = codex
        .as_ref()
        .and_then(|c| c.get("mcp_servers").and_then(|v| v.as_table()))
        .map(|t| t.keys().cloned().collect())
        .unwrap_or_default();
    if let Some(codex) = codex.as_ref() {
        trim.codex_args = codex_worker_args(codex, extra);
        trim.codex_servers = worker_names(extra)
            .into_iter()
            .filter(|n| codex_registered.contains(n))
            .collect();
    }
    let claude_registered = claude_json
        .and_then(read_json)
        .and_then(|c| c.get("mcpServers").and_then(|v| v.as_object()).cloned())
        .unwrap_or_default();
    let worker: Vec<String> = worker_names(&[]);
    trim.unregistered = worker_names(extra)
        .into_iter()
        .filter(|n| !worker.contains(n))
        .filter(|n| !claude_registered.contains_key(n) && !codex_registered.contains(n))
        .collect();
    if !trim.unregistered.is_empty() {
        tracing::warn!(
            names = ?trim.unregistered,
            "project mcp_extra names no MCP server this host registers; skipped"
        );
    }
    trim
}

/// The brief section that tells a worker which MCP servers it got
/// (WI-2746).
pub fn brief_section(servers: &[String], unregistered: &[String]) -> String {
    let mut out =
        String::from("\n\n## MCP servers\n\nThis session started with these MCP servers: ");
    if servers.is_empty() {
        out.push_str("(none recorded)");
    } else {
        out.push_str(&servers.join(", "));
    }
    out.push_str(". Ask for another with `session_grant_request`.");
    if !unregistered.is_empty() {
        out.push_str(&format!(
            " The project also names {}, which this host does not register.",
            unregistered.join(", ")
        ));
    }
    out.push('\n');
    out
}

/// The Claude Code config a launch reads: `$CLAUDE_CONFIG_DIR/.claude.json`
/// when set, else `$HOME/.claude.json`.
pub fn claude_json_path(config_dir: Option<&Path>, home: Option<&Path>) -> Option<PathBuf> {
    config_dir
        .filter(|d| !d.as_os_str().is_empty())
        .or(home)
        .map(|d| d.join(".claude.json"))
}

fn read_json(path: &Path) -> Option<serde_json::Value> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// 0600: a kept entry's env can name a credential.
fn write_private(path: &Path, value: &serde_json::Value) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(serde_json::to_string_pretty(value)?.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_set_parses_and_refuses_unknown_names() {
        assert_eq!(McpSet::parse(None).unwrap(), McpSet::Full);
        assert_eq!(McpSet::parse(Some(" ")).unwrap(), McpSet::Full);
        assert_eq!(McpSet::parse(Some("full")).unwrap(), McpSet::Full);
        assert_eq!(McpSet::parse(Some("worker")).unwrap(), McpSet::Worker);
        assert!(McpSet::parse(Some("everything")).is_err());
    }

    #[test]
    fn a_worker_claude_config_keeps_vogt_and_cadastre_only() {
        let home = serde_json::json!({
            "mcpServers": {
                "vogt": {"type": "stdio", "command": "/usr/local/bin/vogt-mcp", "args": []},
                "cadastre": {"type": "stdio", "command": "/usr/local/bin/vogt-cadastre-mcp"},
                "komodo": {"type": "stdio", "command": "/usr/local/bin/komodo-mcp",
                           "env": {"KOMODO_URL": "http://x"}},
                "grafana-ro": {"type": "stdio", "command": "/usr/local/bin/vogt-readonly-mcp"},
            }
        });
        let out = claude_worker_config(&home, &[]);
        let servers = out["mcpServers"].as_object().unwrap();
        let mut names: Vec<_> = servers.keys().cloned().collect();
        names.sort();
        assert_eq!(names, ["cadastre", "vogt"]);
        assert_eq!(servers["vogt"]["env"][TOOL_PROFILE_ENV], "worker");
        assert!(servers["cadastre"].get("env").is_none());
    }

    #[test]
    fn an_http_vogt_entry_takes_the_profile_on_its_url() {
        let home = serde_json::json!({
            "mcpServers": {"vogt": {"type": "http", "url": "https://v.example/mcp"}}
        });
        let out = claude_worker_config(&home, &[]);
        assert_eq!(
            out["mcpServers"]["vogt"]["url"],
            "https://v.example/mcp?profile=worker"
        );
    }

    #[test]
    fn codex_overrides_disable_the_rest_and_profile_vogt() {
        let config: toml::Table = r#"
            [mcp_servers.komodo]
            command = "/usr/local/bin/komodo-mcp"
            [mcp_servers.cadastre]
            url = "https://c.example/mcp"
            [mcp_servers.vogt]
            url = "https://v.example/mcp"
            bearer_token_env_var = "VOGT_HTTP_TOKEN"
            [mcp_servers."odd name"]
            command = "x"
        "#
        .parse()
        .unwrap();
        let args = codex_worker_args(&config, &[]);
        assert_eq!(
            args,
            [
                "-c",
                "mcp_servers.komodo.enabled=false",
                "-c",
                "mcp_servers.vogt.url=\"https://v.example/mcp?profile=worker\"",
            ]
        );
    }

    #[test]
    fn prepare_writes_a_private_file_and_full_does_nothing() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        std::fs::write(
            home.path().join(".claude.json"),
            r#"{"mcpServers":{"vogt":{"command":"v"},"komodo":{"command":"k"}}}"#,
        )
        .unwrap();
        let id = uuid::Uuid::nil();
        let cj = home.path().join(".claude.json");
        assert_eq!(
            prepare(
                McpSet::Full,
                &[],
                Some(home.path()),
                Some(&cj),
                state.path(),
                id
            ),
            McpTrim::default()
        );
        let trim = prepare(
            McpSet::Worker,
            &[],
            Some(home.path()),
            Some(&cj),
            state.path(),
            id,
        );
        let path = trim.claude_config.unwrap();
        assert_eq!(path, claude_config_path(state.path(), id));
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let written: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(written["mcpServers"].get("komodo").is_none());
        assert!(trim.codex_args.is_empty(), "no codex config, no overrides");
    }

    #[test]
    fn a_claude_config_without_vogt_leaves_the_full_set() {
        let home = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let cj = home.path().join(".claude.json");
        std::fs::write(&cj, r#"{"mcpServers":{"cadastre":{"command":"c"}}}"#).unwrap();
        let id = uuid::Uuid::nil();
        let trim = prepare(
            McpSet::Worker,
            &[],
            Some(home.path()),
            Some(&cj),
            state.path(),
            id,
        );
        assert!(trim.claude_config.is_none());
        assert!(!claude_config_path(state.path(), id).exists());
    }

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn extras_add_to_the_worker_set_once_and_never_remove() {
        assert_eq!(
            worker_names(&names(&["komodo", " ", "vogt", "komodo"])),
            ["vogt", "cadastre", "crw", "komodo"]
        );
    }

    #[test]
    fn a_claude_extra_is_kept_only_when_registered() {
        let home = serde_json::json!({
            "mcpServers": {
                "vogt": {"command": "v"},
                "komodo": {"command": "k"},
                "grafana-ro": {"command": "g"},
            }
        });
        let out = claude_worker_config(&home, &names(&["komodo", "nope"]));
        let mut kept: Vec<_> = out["mcpServers"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        kept.sort();
        assert_eq!(kept, ["komodo", "vogt"]);
    }

    #[test]
    fn a_codex_extra_stays_enabled() {
        let config: toml::Table = r#"
            [mcp_servers.komodo]
            command = "k"
            [mcp_servers.grafana]
            command = "g"
        "#
        .parse()
        .unwrap();
        assert_eq!(
            codex_worker_args(&config, &names(&["komodo"])),
            ["-c", "mcp_servers.grafana.enabled=false"]
        );
    }

    #[test]
    fn prepare_lists_what_was_kept_and_warns_unknown_extras() {
        let home = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let cj = home.path().join(".claude.json");
        std::fs::write(
            &cj,
            r#"{"mcpServers":{"vogt":{"command":"v"},"cadastre":{"command":"c"},"komodo":{"command":"k"}}}"#,
        )
        .unwrap();
        let trim = prepare(
            McpSet::Worker,
            &names(&["komodo", "ghost"]),
            Some(home.path()),
            Some(&cj),
            state.path(),
            uuid::Uuid::nil(),
        );
        assert_eq!(trim.claude_servers, ["vogt", "cadastre", "komodo"]);
        assert_eq!(trim.unregistered, ["ghost"]);
        assert!(
            trim.claude_config.is_some(),
            "an unknown extra never stops the trim"
        );
        let section = brief_section(&trim.claude_servers, &trim.unregistered);
        assert!(section.contains("vogt, cadastre, komodo"));
        assert!(section.contains("session_grant_request"));
        assert!(section.contains("ghost"));
    }

    #[test]
    fn the_claude_config_follows_claude_config_dir() {
        let dir = Path::new("/acct");
        let home = Path::new("/home/u");
        assert_eq!(
            claude_json_path(Some(dir), Some(home)).unwrap(),
            dir.join(".claude.json")
        );
        assert_eq!(
            claude_json_path(Some(Path::new("")), Some(home)).unwrap(),
            home.join(".claude.json")
        );
        assert_eq!(claude_json_path(None, None), None);
    }
}
