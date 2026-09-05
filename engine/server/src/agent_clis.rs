//! Runtime-pinned agent CLIs, reported and moved from the engine (#590).
//!
//! `engine/deploy/agent-cli-install.sh` is the one writer of a non-image copy
//! of Claude Code, Codex or any other CLI the image's table names: the
//! entrypoint runs it at boot for each `VOGT_<TOOL>_VERSION` in the
//! environment. This module is the same installer reached while the pod is
//! running — `GET /api/agent-clis` says what is active, baked and (on request)
//! newest upstream; `POST /api/agent-clis/{tool}` asks for a version and
//! answers with the new state. New sessions get the new version; a session
//! already running keeps the files it started with, because every prefix is
//! versioned and nothing is edited in place.
//!
//! Three rules, each written against a specific way this could go wrong:
//!
//! **The engine names no tool.** Which tools exist, which npm package each
//! is and which variable pins it come from the table the image writes
//! (`agent-clis.tools`). A tool that is not in it is refused, not guessed.
//!
//! **A version string reaches the installer only in one of three shapes** —
//! an exact version, the word `image`, or a dist-tag the installer will itself
//! refuse unless the deployment opted in. Anything else is a bad request here,
//! before a process is spawned, because the value comes from a tool call.
//!
//! **A failed install changes nothing and says so.** The installer leaves
//! `current` where it was; this module reports the installer's own words and
//! the unchanged state, so the caller learns what happened rather than what
//! was hoped.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use axum::{
    extract::{Path as RoutePath, Query, State},
    Json,
};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::{
    app::AppState,
    error::{ApiError, Result},
};

/// Where the runtime pin lives, as the shell half reads it. The names are the
/// `VOGT_AGENT_CLI_*` variables the scripts read, so the engine and the
/// entrypoint cannot disagree about which root or which table is meant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentCliPaths {
    /// Versioned prefixes, `current` symlinks and the `manifest`.
    pub root: PathBuf,
    /// `vogt-agent-cli-install`, the only writer.
    pub installer: PathBuf,
    /// The image's tool table: tool, package, binary, env var (tab-separated).
    pub tools: PathBuf,
    /// The image's resolved pins (`<tool>=<version>`), the baked baseline.
    pub baked: PathBuf,
    /// Where the image's own binaries are, to say whether an image copy exists.
    pub image_bin: PathBuf,
}

impl Default for AgentCliPaths {
    fn default() -> Self {
        Self {
            root: PathBuf::from("/opt/vogt/agent-clis"),
            installer: PathBuf::from("/usr/local/bin/vogt-agent-cli-install"),
            tools: PathBuf::from("/usr/local/share/vogt/agent-clis.tools"),
            baked: PathBuf::from("/usr/local/share/vogt/agent-versions.resolved"),
            image_bin: PathBuf::from("/usr/local/bin"),
        }
    }
}

impl AgentCliPaths {
    /// The defaults, each overridable by the variable of the same meaning the
    /// shell scripts honour.
    pub fn from_env() -> Self {
        let defaults = Self::default();
        let pick = |name: &str, default: PathBuf| {
            std::env::var(name)
                .ok()
                .filter(|value| !value.trim().is_empty())
                .map(PathBuf::from)
                .unwrap_or(default)
        };
        Self {
            root: pick("VOGT_AGENT_CLI_ROOT", defaults.root),
            installer: pick("VOGT_AGENT_CLI_INSTALLER", defaults.installer),
            tools: pick("VOGT_AGENT_CLI_TOOLS", defaults.tools),
            baked: pick("VOGT_AGENT_CLI_BAKED_MANIFEST", defaults.baked),
            image_bin: pick("VOGT_AGENT_CLI_IMAGE_BIN", defaults.image_bin),
        }
    }
}

/// How long an upstream `npm view` answer is trusted before it is asked again.
const UPSTREAM_TTL: Duration = Duration::from_secs(60 * 60);
/// Ceiling on one `npm view`. Upstream is a hint, not a dependency.
const UPSTREAM_TIMEOUT: Duration = Duration::from_secs(15);
/// Ceiling on one install. A download plus a smoke check, not a build.
const INSTALL_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// `EX_USAGE` from the installer: the request was malformed or refused.
const EX_USAGE: i32 = 64;

/// Per-process state: the paths, the upstream cache and the one-at-a-time
/// install lock.
pub struct AgentCliRuntime {
    pub paths: AgentCliPaths,
    upstream: Mutex<BTreeMap<String, (Instant, Option<String>)>>,
    installing: Mutex<()>,
}

impl AgentCliRuntime {
    pub fn new(paths: AgentCliPaths) -> Self {
        Self {
            paths,
            upstream: Mutex::new(BTreeMap::new()),
            installing: Mutex::new(()),
        }
    }
}

/// Which copy of a tool a new session would run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentCliSource {
    /// The image's own install under `/usr/local`.
    Image,
    /// A versioned prefix under the runtime root, made current by the installer.
    Runtime,
    /// Neither: the image was built without it and nothing has been installed.
    Absent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentCliTool {
    pub tool: String,
    pub package: String,
    pub binary: String,
    /// The variable that pins it at boot (`VOGT_CLAUDE_CODE_VERSION`, …).
    pub env_var: String,
    pub baked_version: Option<String>,
    pub active_version: Option<String>,
    pub source: AgentCliSource,
    /// Versions present under the runtime root, newest first — each one a
    /// switch the installer can make without network access.
    pub installed_versions: Vec<String>,
    /// npm's `latest` for the package, when asked for and answered. Absent
    /// otherwise: an unreachable registry is not this pod's fault.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_latest: Option<String>,
    /// `upstream_latest` differs from `active_version`. Only when both are known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update_available: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentCliReport {
    pub root: String,
    /// Whether this image carries the installer at all; `false` means the
    /// POST route can only refuse.
    pub installer_present: bool,
    pub tools: Vec<AgentCliTool>,
}

#[derive(Debug, Default, Deserialize)]
pub struct ListQuery {
    /// Ask npm for each package's `latest` (cached for an hour).
    #[serde(default)]
    pub upstream: bool,
}

#[derive(Debug, Deserialize)]
pub struct UpdateRequest {
    /// An exact version (`2.1.261`), `image` for the baked copy, or a dist-tag
    /// (`latest`, `stable`) the deployment must have opted into.
    pub version: String,
}

pub async fn list(
    State(state): State<Arc<AppState>>,
    Query(query): Query<ListQuery>,
) -> Result<Json<AgentCliReport>> {
    let runtime = Arc::clone(&state.agent_clis);
    let mut report = read_report(&runtime.paths)?;
    if query.upstream {
        annotate_upstream(&runtime, &mut report).await;
    }
    Ok(Json(report))
}

pub async fn update(
    State(state): State<Arc<AppState>>,
    RoutePath(tool): RoutePath<String>,
    Json(request): Json<UpdateRequest>,
) -> Result<Json<AgentCliReport>> {
    let runtime = Arc::clone(&state.agent_clis);
    let paths = &runtime.paths;
    let table = read_table(&paths.tools)?;
    if !table.iter().any(|row| row.tool == tool) {
        return Err(ApiError::NotFound);
    }
    let version = validate_version(&request.version)?;
    if !paths.installer.is_file() {
        return Err(ApiError::Config(format!(
            "this image carries no agent CLI installer at {}; the runtime pin \
             needs an image built with it (#590)",
            paths.installer.display()
        )));
    }

    // One install at a time. Two concurrent flips of the same `current` are
    // harmless to the filesystem and confusing to the two callers.
    let _guard = runtime.installing.lock().await;
    let output = tokio::time::timeout(
        INSTALL_TIMEOUT,
        tokio::process::Command::new(&paths.installer)
            .arg(&tool)
            .arg(version)
            .env("VOGT_AGENT_CLI_ROOT", &paths.root)
            .env("VOGT_AGENT_CLI_TOOLS", &paths.tools)
            .env("VOGT_AGENT_CLI_BAKED_MANIFEST", &paths.baked)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| {
        ApiError::Conflict(format!(
            "installing {tool} {version} did not finish within {}s; the \
             previous version stays current",
            INSTALL_TIMEOUT.as_secs()
        ))
    })?
    .map_err(|e| ApiError::Internal(format!("could not run the agent CLI installer: {e}")))?;

    if !output.status.success() {
        let said = tail(&String::from_utf8_lossy(&output.stderr), 6);
        return Err(match output.status.code() {
            Some(EX_USAGE) => ApiError::BadRequest(said),
            _ => ApiError::Conflict(format!(
                "{tool} {version} was not made current; the previous version stays. \
                 The installer said: {said}"
            )),
        });
    }
    tracing::info!(tool = %tool, version = %version, "agent CLI runtime pin changed");
    Ok(Json(read_report(paths)?))
}

/// One row of the image's tool table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolRow {
    pub tool: String,
    pub package: String,
    pub binary: String,
    pub env_var: String,
}

pub fn read_table(path: &Path) -> Result<Vec<ToolRow>> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(ApiError::Io(e)),
    };
    Ok(parse_table(&text))
}

pub fn parse_table(text: &str) -> Vec<ToolRow> {
    text.lines()
        .filter(|line| !line.trim().is_empty() && !line.starts_with('#'))
        .filter_map(|line| {
            let mut cols = line.split('\t');
            Some(ToolRow {
                tool: cols.next()?.to_string(),
                package: cols.next()?.to_string(),
                binary: cols.next()?.to_string(),
                env_var: cols.next()?.to_string(),
            })
        })
        .collect()
}

fn read_manifest(path: &Path) -> BTreeMap<String, String> {
    std::fs::read_to_string(path)
        .map(|text| parse_manifest(&text))
        .unwrap_or_default()
}

pub fn parse_manifest(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .filter_map(|line| {
            let (tool, version) = line.split_once('=')?;
            let version = version.trim();
            (!version.is_empty()).then(|| (tool.trim().to_string(), version.to_string()))
        })
        .collect()
}

pub fn read_report(paths: &AgentCliPaths) -> Result<AgentCliReport> {
    let table = read_table(&paths.tools)?;
    let baked = read_manifest(&paths.baked);
    let tools = table
        .into_iter()
        .map(|row| {
            let current = std::fs::read_link(paths.root.join(&row.tool).join("current"))
                .ok()
                .and_then(|target| {
                    target
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                });
            let runtime_binary = paths
                .root
                .join(&row.tool)
                .join("current")
                .join("bin")
                .join(&row.binary);
            let baked_version = baked.get(&row.tool).cloned();
            let image_binary = paths.image_bin.join(&row.binary);
            let (source, active_version) = match current {
                Some(version) if runtime_binary.exists() => {
                    (AgentCliSource::Runtime, Some(version))
                }
                _ if image_binary.exists() && baked_version.is_some() => {
                    (AgentCliSource::Image, baked_version.clone())
                }
                _ => (AgentCliSource::Absent, None),
            };
            AgentCliTool {
                installed_versions: installed_versions(&paths.root.join(&row.tool)),
                tool: row.tool,
                package: row.package,
                binary: row.binary,
                env_var: row.env_var,
                baked_version,
                active_version,
                source,
                upstream_latest: None,
                update_available: None,
            }
        })
        .collect();
    Ok(AgentCliReport {
        root: paths.root.display().to_string(),
        installer_present: paths.installer.is_file(),
        tools,
    })
}

/// Version directories under one tool's root, newest first by mtime.
fn installed_versions(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut versions: Vec<(std::time::SystemTime, String)> = entries
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            !name.starts_with('.')
                && name != "current"
                && entry
                    .file_type()
                    .map(|kind| kind.is_dir() && !kind.is_symlink())
                    .unwrap_or(false)
        })
        .map(|entry| {
            let modified = entry
                .metadata()
                .and_then(|meta| meta.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            (modified, entry.file_name().to_string_lossy().into_owned())
        })
        .collect();
    versions.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
    versions.into_iter().map(|(_, name)| name).collect()
}

/// The three shapes a version may take on the way to the installer. Anything
/// else is refused here rather than escaped, because the value arrives from
/// a tool call and ends up in argv.
pub fn validate_version(raw: &str) -> Result<&str> {
    let value = raw.trim();
    if matches!(value, "image" | "latest" | "stable") {
        return Ok(value);
    }
    if is_exact_version(value) {
        return Ok(value);
    }
    Err(ApiError::BadRequest(format!(
        "version {raw:?} is not an exact version (like 2.1.261), `image`, or a \
         dist-tag (`latest`, `stable`)"
    )))
}

fn is_exact_version(value: &str) -> bool {
    if value.len() > 64 {
        return false;
    }
    let (core, rest) = match value.find(['-', '+']) {
        Some(at) => (&value[..at], &value[at + 1..]),
        None => (value, ""),
    };
    let mut parts = core.split('.');
    let numeric = |part: Option<&str>| {
        part.is_some_and(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
    };
    if !(numeric(parts.next()) && numeric(parts.next()) && numeric(parts.next()))
        || parts.next().is_some()
    {
        return false;
    }
    if value.find(['-', '+']).is_some() && rest.is_empty() {
        return false;
    }
    rest.bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'+'))
}

async fn annotate_upstream(runtime: &AgentCliRuntime, report: &mut AgentCliReport) {
    let lookups = report
        .tools
        .iter()
        .map(|tool| upstream_latest(runtime, &tool.package));
    let answers = futures_util::future::join_all(lookups).await;
    for (tool, latest) in report.tools.iter_mut().zip(answers) {
        tool.update_available = match (&latest, &tool.active_version) {
            (Some(latest), Some(active)) => Some(latest != active),
            _ => None,
        };
        tool.upstream_latest = latest;
    }
}

async fn upstream_latest(runtime: &AgentCliRuntime, package: &str) -> Option<String> {
    {
        let cache = runtime.upstream.lock().await;
        if let Some((asked, answer)) = cache.get(package) {
            if asked.elapsed() < UPSTREAM_TTL {
                return answer.clone();
            }
        }
    }
    let answer = tokio::time::timeout(
        UPSTREAM_TIMEOUT,
        tokio::process::Command::new("npm")
            .args(["view", package, "version"])
            .kill_on_drop(true)
            .output(),
    )
    .await
    .ok()
    .and_then(|result| result.ok())
    .filter(|output| output.status.success())
    .and_then(|output| {
        String::from_utf8(output.stdout)
            .ok()
            .and_then(|text| text.lines().last().map(|line| line.trim().to_string()))
            .filter(|version| is_exact_version(version))
    });
    runtime
        .upstream
        .lock()
        .await
        .insert(package.to_string(), (Instant::now(), answer.clone()));
    answer
}

fn tail(text: &str, lines: usize) -> String {
    let all: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let start = all.len().saturating_sub(lines);
    all[start..].join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_versions_and_the_three_words_pass_everything_else_is_refused() {
        for good in [
            "2.1.261",
            "0.149.1",
            "1.0.0-beta.2",
            "1.2.3+build.5",
            "image",
            "latest",
            "stable",
        ] {
            assert!(validate_version(good).is_ok(), "{good}");
        }
        for bad in [
            "",
            "2.1",
            "v2.1.261",
            "2.1.261; rm -rf /",
            "../x",
            "--flag",
            "2.1.261-",
            "1.2.3.4",
            "next",
        ] {
            assert!(validate_version(bad).is_err(), "{bad:?} reached argv");
        }
    }

    #[test]
    fn the_table_and_manifest_parse_as_the_scripts_write_them() {
        let rows = parse_table(
            "codex\t@openai/codex\tcodex\tVOGT_CODEX_VERSION\n\
             claude-code\t@anthropic-ai/claude-code\tclaude\tVOGT_CLAUDE_CODE_VERSION\n\
             # a comment\nshort\tline\n",
        );
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].binary, "claude");
        assert_eq!(rows[1].env_var, "VOGT_CLAUDE_CODE_VERSION");
        let manifest = parse_manifest("codex=0.149.1\nclaude-code=2.1.258\nempty=\n");
        assert_eq!(
            manifest.get("claude-code").map(String::as_str),
            Some("2.1.258")
        );
        assert!(!manifest.contains_key("empty"));
    }

    #[test]
    fn a_report_reads_the_image_copy_then_the_runtime_copy() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = AgentCliPaths {
            root: tmp.path().join("root"),
            installer: tmp.path().join("nope"),
            tools: tmp.path().join("tools"),
            baked: tmp.path().join("baked"),
            image_bin: tmp.path().join("image-bin"),
        };
        std::fs::write(
            &paths.tools,
            "claude-code\t@anthropic-ai/claude-code\tclaude\tVOGT_CLAUDE_CODE_VERSION\n",
        )
        .unwrap();
        std::fs::write(&paths.baked, "claude-code=2.1.258\n").unwrap();
        std::fs::create_dir_all(&paths.image_bin).unwrap();
        std::fs::write(paths.image_bin.join("claude"), "").unwrap();

        let report = read_report(&paths).unwrap();
        assert!(!report.installer_present);
        let tool = &report.tools[0];
        assert_eq!(tool.source, AgentCliSource::Image);
        assert_eq!(tool.active_version.as_deref(), Some("2.1.258"));
        assert!(tool.installed_versions.is_empty());

        // The installer's layout: a versioned prefix and `current` onto it.
        let prefix = paths.root.join("claude-code").join("2.1.261").join("bin");
        std::fs::create_dir_all(&prefix).unwrap();
        std::fs::write(prefix.join("claude"), "").unwrap();
        std::os::unix::fs::symlink("2.1.261", paths.root.join("claude-code").join("current"))
            .unwrap();
        let tool = &read_report(&paths).unwrap().tools[0];
        assert_eq!(tool.source, AgentCliSource::Runtime);
        assert_eq!(tool.active_version.as_deref(), Some("2.1.261"));
        assert_eq!(tool.baked_version.as_deref(), Some("2.1.258"));
        assert_eq!(tool.installed_versions, vec!["2.1.261".to_string()]);
    }
}
