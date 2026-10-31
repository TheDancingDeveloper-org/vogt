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

/// The bridge command that speaks to the vogt endpoint.
const VOGT_BRIDGE: &str = "vogt-mcp-remote";
/// The most extra names a project may add; matches the Python model.
pub const MCP_EXTRA_MAX: usize = 16;

/// A server name a project may add: 1-64 of `[A-Za-z0-9_-]`, as the Python
/// model validates it.
pub fn valid_extra_name(name: &str) -> bool {
    (1..=64).contains(&name.len()) && is_bare_key(name)
}

/// Where a URL points: its lowercased host without a trailing dot, and its
/// port (the scheme's default when absent). Scheme and path are ignored, so
/// any door onto the same host and port compares equal. `None` when the URL
/// does not parse or has no host.
fn host_port(raw: &str) -> Option<(String, u16)> {
    let url = url::Url::parse(raw.trim()).ok()?;
    let host = match url.host()? {
        url::Host::Domain(d) => d.trim_end_matches('.').to_ascii_lowercase(),
        url::Host::Ipv4(ip) => ip.to_string(),
        // `[::ffff:127.0.0.1]` is 127.0.0.1: compare and classify the
        // canonical form so a mapped address is not a second spelling.
        url::Host::Ipv6(ip) => ip.to_canonical().to_string(),
    };
    if host.is_empty() {
        return None;
    }
    Some((host, url.port_or_known_default().unwrap_or(0)))
}

/// A host on this deployment's own network: loopback, `localhost`, a private
/// or tailnet (100.64.0.0/10) address, or link-local. IPv4-mapped IPv6 is
/// judged as the IPv4 address it maps.
fn is_local_host(host: &str) -> bool {
    if host == "localhost" || host.ends_with(".localhost") {
        return true;
    }
    let host = host.trim_start_matches('[').trim_end_matches(']');
    match host.parse::<std::net::IpAddr>().map(|ip| ip.to_canonical()) {
        Ok(std::net::IpAddr::V4(ip)) => {
            let [a, b, ..] = ip.octets();
            ip.is_loopback()
                || ip.is_private()
                || ip.is_unspecified()
                || ip.is_link_local()
                || (a == 100 && (64..128).contains(&b))
        }
        Ok(std::net::IpAddr::V6(ip)) => {
            let s0 = ip.segments()[0];
            ip.is_loopback()
                || ip.is_unspecified()
                || (s0 & 0xfe00) == 0xfc00
                || (s0 & 0xffc0) == 0xfe80
        }
        Err(_) => false,
    }
}

/// The strings an entry carries: command, args and env values.
fn entry_strings(entry: &serde_json::Value) -> Vec<&str> {
    let mut out: Vec<&str> = Vec::new();
    if let Some(c) = entry.get("command").and_then(|v| v.as_str()) {
        out.push(c);
    }
    if let Some(args) = entry.get("args").and_then(|v| v.as_array()) {
        out.extend(args.iter().filter_map(|v| v.as_str()));
    }
    if let Some(env) = entry.get("env").and_then(|v| v.as_object()) {
        out.extend(env.values().filter_map(|v| v.as_str()));
    }
    out
}

/// The hosts vogt's own entry points at: its URL, or any URL among a stdio
/// entry's args and env (the bridge's target).
fn vogt_hosts(entry: &serde_json::Value) -> Vec<(String, u16)> {
    if let Some(url) = entry.get("url").and_then(|v| v.as_str()) {
        return host_port(url).into_iter().collect();
    }
    entry_strings(entry)
        .into_iter()
        .flat_map(|s| s.split_whitespace())
        .filter_map(host_port)
        .collect()
}

/// A stdio entry that runs the vogt bridge: `vogt-mcp-remote` anywhere in
/// the command or its args (`npx vogt-mcp-remote`, `sh -c "vogt-mcp-remote
/// ..."`), or the same command and args as vogt's own stdio entry.
fn runs_vogt_bridge(entry: &serde_json::Value, vogt: Option<&serde_json::Value>) -> bool {
    if entry.get("url").is_some() {
        return false;
    }
    let mine = entry_strings(entry);
    if mine.iter().any(|s| s.contains(VOGT_BRIDGE)) {
        return true;
    }
    vogt.is_some_and(|v| v.get("command").is_some() && cmd_args(v) == cmd_args(entry))
}

fn cmd_args(e: &serde_json::Value) -> Vec<&str> {
    let mut v = vec![e.get("command").and_then(|c| c.as_str()).unwrap_or("")];
    if let Some(args) = e.get("args").and_then(|a| a.as_array()) {
        v.extend(args.iter().filter_map(|a| a.as_str()));
    }
    v
}

/// The credential an entry carries: env var names that hold a token
/// (`bearer_token_env_var`, env keys other than the tool profile) and the
/// literal values of its headers and env (8 characters or more).
struct Creds {
    names: Vec<String>,
    values: Vec<String>,
}

fn vogt_creds(vogt: Option<&serde_json::Value>) -> Creds {
    let mut c = Creds {
        names: Vec::new(),
        values: Vec::new(),
    };
    let Some(v) = vogt else { return c };
    if let Some(n) = v.get("bearer_token_env_var").and_then(|n| n.as_str()) {
        c.names.push(n.to_string());
    }
    for key in ["env", "headers", "http_headers"] {
        let Some(map) = v.get(key).and_then(|m| m.as_object()) else {
            continue;
        };
        for (k, val) in map {
            if k == TOOL_PROFILE_ENV {
                continue;
            }
            if key == "env" {
                c.names.push(k.clone());
            }
            if let Some(s) = val.as_str().filter(|s| s.len() >= 8) {
                c.values.push(s.to_string());
            }
        }
    }
    c
}

/// A token variable named after vogt (`VOGT_HTTP_TOKEN`, `VOGT_MCP_TOKEN`).
fn is_vogt_token_name(s: &str) -> bool {
    let s = s.to_ascii_uppercase();
    s.contains("VOGT") && s.contains("TOKEN")
}

/// Whether an extra carries vogt's credential: its token variable, a vogt
/// token name anywhere in it (`Bearer ${VOGT_HTTP_TOKEN}`), or a header or
/// env value of vogt's own entry. This catches an alias under another DNS
/// name, which the host checks cannot see.
fn uses_vogt_creds(entry: &serde_json::Value, creds: &Creds) -> bool {
    let mut strings: Vec<&str> = entry_strings(entry);
    if let Some(n) = entry.get("bearer_token_env_var").and_then(|n| n.as_str()) {
        strings.push(n);
    }
    if let Some(u) = entry.get("url").and_then(|u| u.as_str()) {
        strings.push(u);
    }
    for key in ["env", "headers", "http_headers", "env_http_headers"] {
        if let Some(map) = entry.get(key).and_then(|m| m.as_object()) {
            strings.extend(map.keys().map(String::as_str));
            strings.extend(map.values().filter_map(|v| v.as_str()));
        }
    }
    if let Some(vars) = entry.get("env_vars").and_then(|v| v.as_array()) {
        strings.extend(vars.iter().filter_map(|v| v.as_str()));
    }
    strings.iter().any(|s| {
        is_vogt_token_name(s)
            || creds.names.iter().any(|n| s.contains(n.as_str()))
            || creds.values.iter().any(|v| s.contains(v.as_str()))
    })
}

/// Whether an extra is a second door onto vogt. Fails closed: an extra URL
/// that does not parse counts as one, and with no `vogt` entry to compare
/// against every local host counts as vogt's.
fn aliases_vogt(
    entry: &serde_json::Value,
    vogt: Option<&serde_json::Value>,
    hosts: &[(String, u16)],
) -> bool {
    if runs_vogt_bridge(entry, vogt) || uses_vogt_creds(entry, &vogt_creds(vogt)) {
        return true;
    }
    let vogt_local = vogt.is_none() || hosts.iter().any(|(h, _)| is_local_host(h));
    let hits = |target: &(String, u16)| {
        hosts.iter().any(|(h, _)| h == &target.0) || (vogt_local && is_local_host(&target.0))
    };
    if let Some(url) = entry.get("url") {
        let Some(target) = url.as_str().and_then(host_port) else {
            return true;
        };
        return hits(&target);
    }
    // A stdio extra that names vogt's host in its args or env.
    entry_strings(entry).into_iter().any(|s| {
        s.split_whitespace().any(|w| {
            host_port(w).is_some_and(|t| hits(&t))
                || hosts
                    .iter()
                    .any(|(h, _)| w.to_ascii_lowercase().contains(h.as_str()))
        })
    })
}

/// The extras that are safe to add (WI-2750): an extra registered as a
/// second door to the vogt endpoint (a `vogt-admin` on vogt's host, the vogt
/// bridge under another name, or anything local when vogt itself is served
/// locally) would hand a worker the full vogt tool profile, so it is dropped
/// with a warning. So is a name that is not valid, and anything past
/// [`MCP_EXTRA_MAX`]. `servers` is the config's server map, as JSON; with no
/// map nothing is added.
///
/// Limits: a stdio extra is judged only by its command, args and env. A
/// wrapper script that reaches vogt on its own cannot be seen from here;
/// the host config that defines these entries is written by the operator,
/// and that is the trust boundary, not this check.
pub fn safe_extra(
    servers: Option<&serde_json::Map<String, serde_json::Value>>,
    extra: &[String],
) -> Vec<String> {
    let Some(servers) = servers else {
        return Vec::new();
    };
    let vogt = servers.get("vogt");
    let hosts = vogt.map(vogt_hosts).unwrap_or_default();
    extra
        .iter()
        .filter(|name| !name.trim().is_empty())
        .take(MCP_EXTRA_MAX)
        .filter(|name| {
            let name = name.trim();
            if !valid_extra_name(name) {
                tracing::warn!(name, "project mcp_extra name is not valid; dropped");
                return false;
            }
            if WORKER_SERVERS.contains(&name) {
                return true;
            }
            let Some(entry) = servers.get(name) else {
                return true;
            };
            let bad = aliases_vogt(entry, vogt, &hosts);
            if bad {
                tracing::warn!(
                    name,
                    "project mcp_extra names a server on the vogt endpoint; dropped"
                );
            }
            !bad
        })
        .cloned()
        .collect()
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
        for name in worker_names(&safe_extra(Some(servers), extra)) {
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
    let mut out = Vec::new();
    let Some(servers) = config.get("mcp_servers").and_then(|v| v.as_table()) else {
        return out;
    };
    let keep = worker_names(&safe_extra(codex_servers_json(servers).as_ref(), extra));
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

fn codex_servers_json(servers: &toml::Table) -> Option<serde_json::Map<String, serde_json::Value>> {
    match serde_json::to_value(servers).ok()? {
        serde_json::Value::Object(map) => Some(map),
        _ => None,
    }
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
                        trim.claude_servers = worker_names(&safe_extra(
                            claude.get("mcpServers").and_then(|v| v.as_object()),
                            extra,
                        ))
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
        let servers = codex
            .get("mcp_servers")
            .and_then(|v| v.as_table())
            .and_then(codex_servers_json);
        trim.codex_servers = worker_names(&safe_extra(servers.as_ref(), extra))
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

    #[test]
    fn a_claude_extra_on_the_vogt_endpoint_is_dropped() {
        let home = serde_json::json!({
            "mcpServers": {
                "vogt": {"type": "http", "url": "https://v.example/mcp"},
                "vogt-admin": {"type": "http", "url": "https://v.example/mcp/?profile=full"},
                "bridge": {"command": "/usr/local/bin/vogt-mcp-remote"},
                "komodo": {"command": "k"},
            }
        });
        let out = claude_worker_config(&home, &names(&["vogt-admin", "bridge", "komodo"]));
        let mut kept: Vec<_> = out["mcpServers"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        kept.sort();
        assert_eq!(kept, ["komodo", "vogt"]);
        assert_eq!(
            out["mcpServers"]["vogt"]["url"],
            "https://v.example/mcp?profile=worker"
        );
    }

    #[test]
    fn a_stdio_extra_with_vogts_command_is_dropped() {
        let servers = serde_json::json!({
            "vogt": {"command": "vogt-mcp", "args": ["--remote"]},
            "again": {"command": "vogt-mcp", "args": ["--remote"], "env": {"X": "1"}},
            "other": {"command": "vogt-mcp", "args": ["--other"]},
        });
        assert_eq!(
            safe_extra(servers.as_object(), &names(&["again", "other", "ghost"])),
            ["other", "ghost"]
        );
    }

    #[test]
    fn a_codex_extra_on_the_vogt_endpoint_stays_disabled() {
        let config: toml::Table = r#"
            [mcp_servers.vogt]
            url = "https://v.example/mcp"
            [mcp_servers.vogt-admin]
            url = "https://v.example/mcp?profile=full"
        "#
        .parse()
        .unwrap();
        let args = codex_worker_args(&config, &names(&["vogt-admin"]));
        assert!(args.contains(&"mcp_servers.vogt-admin.enabled=false".to_string()));
    }

    fn dropped(vogt: serde_json::Value, extra: serde_json::Value) -> bool {
        let servers = serde_json::json!({"vogt": vogt, "x": extra});
        safe_extra(servers.as_object(), &names(&["x"])).is_empty()
    }

    #[test]
    fn url_variants_of_the_vogt_host_are_dropped() {
        let vogt = serde_json::json!({"url": "https://v.example/mcp"});
        for url in [
            "HTTPS://V.EXAMPLE/mcp",
            "http://v.example/mcp",
            "https://v.example:443/mcp",
            "https://v.example./mcp",
            "https://v.example//mcp",
            "https://v.example/mcp/./",
            "https://v.example/other",
            "not a url",
        ] {
            assert!(
                dropped(vogt.clone(), serde_json::json!({"url": url})),
                "{url}"
            );
        }
        assert!(!dropped(
            vogt,
            serde_json::json!({"url": "https://k.example/mcp"})
        ));
    }

    #[test]
    fn local_extras_are_dropped_when_vogt_is_local() {
        let vogt = serde_json::json!({"url": "http://100.92.54.45:8910/mcp"});
        for url in [
            "http://127.0.0.1:8910/mcp",
            "http://localhost:8910/mcp",
            "http://100.100.1.1/mcp",
            "http://[::1]:8910/mcp",
            "http://[::ffff:127.0.0.1]:8910/mcp",
            "http://[::ffff:7f00:1]:8910/mcp",
            "http://[::ffff:10.0.0.5]/mcp",
            "http://[::ffff:100.64.1.1]/mcp",
            "http://169.254.169.254/mcp",
            "http://[fe80::1]/mcp",
        ] {
            assert!(
                dropped(vogt.clone(), serde_json::json!({"url": url})),
                "{url}"
            );
        }
        let public = serde_json::json!({"url": "https://v.example/mcp"});
        assert!(!dropped(
            public,
            serde_json::json!({"url": "http://127.0.0.1:9000/mcp"})
        ));
    }

    #[test]
    fn a_mapped_spelling_of_vogts_address_is_dropped() {
        let vogt = serde_json::json!({"url": "https://203.0.113.7/mcp"});
        assert!(dropped(
            vogt,
            serde_json::json!({"url": "https://[::ffff:203.0.113.7]/mcp"})
        ));
    }

    #[test]
    fn extras_carrying_vogts_credential_are_dropped() {
        let vogt = serde_json::json!({
            "url": "https://v.example/mcp",
            "bearer_token_env_var": "MY_TOK",
            "headers": {"Authorization": "Bearer abcdefgh1234"}
        });
        for extra in [
            serde_json::json!({"url": "https://alias.example/mcp", "bearer_token_env_var": "MY_TOK"}),
            serde_json::json!({"url": "https://alias.example/mcp", "headers": {"Authorization": "Bearer abcdefgh1234"}}),
            serde_json::json!({"url": "https://alias.example/mcp", "headers": {"Authorization": "Bearer ${VOGT_HTTP_TOKEN}"}}),
            serde_json::json!({"command": "p", "env": {"T": "$MY_TOK"}}),
            serde_json::json!({"command": "p", "env": {"VOGT_MCP_TOKEN": "x"}}),
        ] {
            assert!(dropped(vogt.clone(), extra.clone()), "{extra}");
        }
        assert!(!dropped(
            vogt,
            serde_json::json!({"url": "https://komodo.example/mcp", "bearer_token_env_var": "KOMODO_TOKEN"})
        ));
    }

    #[test]
    fn with_no_vogt_entry_local_and_credentialed_extras_are_dropped() {
        let keep = |extra: serde_json::Value| {
            let mut m = serde_json::Map::new();
            m.insert("x".into(), extra);
            safe_extra(Some(&m), &["x".to_string()]) == vec!["x".to_string()]
        };
        assert!(!keep(
            serde_json::json!({"url": "http://127.0.0.1:8910/mcp"})
        ));
        assert!(!keep(
            serde_json::json!({"url": "http://[::ffff:10.1.1.1]/mcp"})
        ));
        assert!(!keep(
            serde_json::json!({"url": "https://a.example/mcp", "bearer_token_env_var": "VOGT_HTTP_TOKEN"})
        ));
        assert!(keep(serde_json::json!({"url": "https://a.example/mcp"})));
    }

    #[test]
    fn bridge_launchers_and_vogt_host_args_are_dropped() {
        let vogt = serde_json::json!({
            "command": "vogt-mcp-remote", "args": ["https://v.example/mcp"]
        });
        for extra in [
            serde_json::json!({"command": "npx", "args": ["-y", "vogt-mcp-remote"]}),
            serde_json::json!({"command": "uvx", "args": ["vogt-mcp-remote"]}),
            serde_json::json!({"command": "sh", "args": ["-c", "vogt-mcp-remote --x"]}),
            serde_json::json!({"command": "/opt/renamed", "args": ["--url", "https://V.example/mcp"]}),
            serde_json::json!({"command": "mcp-proxy", "args": ["v.example"]}),
            serde_json::json!({"command": "p", "env": {"URL": "http://v.example:443/mcp"}}),
        ] {
            assert!(dropped(vogt.clone(), extra.clone()), "{extra}");
        }
        assert!(!dropped(
            vogt,
            serde_json::json!({"command": "komodo-mcp", "args": ["--x"]})
        ));
    }

    #[test]
    fn no_server_map_and_bad_names_add_nothing() {
        assert!(safe_extra(None, &names(&["komodo"])).is_empty());
        let servers = serde_json::json!({"vogt": {"url": "https://v.example/mcp"}});
        let long = "a".repeat(65);
        assert_eq!(
            safe_extra(
                servers.as_object(),
                &names(&["ok_1", "bad name", &long, "a.b"])
            ),
            ["ok_1"]
        );
        let many: Vec<String> = (0..20).map(|i| format!("s{i}")).collect();
        assert_eq!(safe_extra(servers.as_object(), &many).len(), MCP_EXTRA_MAX);
    }
}
