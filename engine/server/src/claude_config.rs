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
#[derive(Debug, Clone, PartialEq, Eq)]
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
    /// The named Claude Code accounts a session may be started under
    /// (`ENGINE_AGENT_CLAUDE_ACCOUNTS`), each a config directory holding its
    /// own login. See [`parse_accounts`].
    pub accounts: Vec<ClaudeAccount>,
    /// The Codex half (WI-1143 / WI-1146): the managed requirements file Codex
    /// reads from `/etc/codex/requirements.toml`, or a deployment's own path.
    /// Unlike the opencode policy, a named file that is missing or unreadable
    /// is an error the launch must refuse — never a reason to start without
    /// the gitguard hook. See [`codex_policy_from`].
    pub codex_policy: Result<Option<PathBuf>, String>,
}

/// One Claude Code subscription account: a name callers use, and the
/// `CLAUDE_CONFIG_DIR` that holds its login, `.claude.json` and transcripts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeAccount {
    pub name: String,
    pub dir: PathBuf,
}

/// The longest account name accepted.
const ACCOUNT_NAME_MAX: usize = 32;

/// Whether `name` is usable as an account name: lowercase letters, digits,
/// `-` and `_`, starting with a letter or digit. Names appear on the wire and
/// in records, so they stay boring.
pub fn is_account_name(name: &str) -> bool {
    let mut chars = name.chars();
    name.len() <= ACCOUNT_NAME_MAX
        && chars
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// `ENGINE_AGENT_CLAUDE_ACCOUNTS`, read: comma-separated `name=/abs/dir`
/// pairs. The engine, not the caller, maps a name to a directory, so a
/// session can only be pointed at a directory the operator listed. A malformed
/// or duplicate entry, or a relative directory, is skipped with a warning:
/// one bad entry must not stop the engine or disable the others.
pub fn parse_accounts(value: Option<&str>) -> Vec<ClaudeAccount> {
    let mut accounts: Vec<ClaudeAccount> = Vec::new();
    for entry in value.unwrap_or_default().split(',') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let Some((name, dir)) = entry.split_once('=') else {
            tracing::warn!(
                entry,
                "ENGINE_AGENT_CLAUDE_ACCOUNTS entry is not name=dir; ignored"
            );
            continue;
        };
        let (name, dir) = (name.trim(), dir.trim());
        if !is_account_name(name) {
            tracing::warn!(
                name,
                "ENGINE_AGENT_CLAUDE_ACCOUNTS name is not [a-z0-9][a-z0-9_-]{{0,31}}; ignored"
            );
            continue;
        }
        if !Path::new(dir).is_absolute() {
            tracing::warn!(
                name,
                dir,
                "ENGINE_AGENT_CLAUDE_ACCOUNTS directory is not absolute; ignored"
            );
            continue;
        }
        if accounts.iter().any(|a| a.name == name) {
            tracing::warn!(
                name,
                "ENGINE_AGENT_CLAUDE_ACCOUNTS names the account twice; the first wins"
            );
            continue;
        }
        accounts.push(ClaudeAccount {
            name: name.to_string(),
            dir: PathBuf::from(dir),
        });
    }
    accounts
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

/// Where the image installs the default Codex requirements file. The same
/// bytes are also copied to [`CODEX_REQUIREMENTS_FILE`], which is the path
/// stock Codex reads without a flag.
pub const IMAGE_CODEX_POLICY: &str = "/usr/local/share/vogt/driven-session-codex-requirements.toml";

/// The managed requirements path Codex reads, and the only one: the fork's
/// loader has no flag or variable for another (WI-1146). A deployment that
/// points `ENGINE_AGENT_CODEX_POLICY` elsewhere still has to put that file
/// here, and the engine checks both.
pub const CODEX_REQUIREMENTS_FILE: &str = "/etc/codex/requirements.toml";

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

/// `ENGINE_AGENT_CODEX_POLICY`, read like `ENGINE_AGENT_CLAUDE_SETTINGS` (a
/// path, empty for the image's requirements file when it is installed, `off`).
///
/// Fail closed (WI-1143) when a policy is named. A path that is missing,
/// empty, or unreadable is `Err`: the session must not start, and must not
/// start with bypass because the hook could not be loaded. `off` is the only
/// way a deployment that *has* the file starts Codex without it. OpenCode's
/// broken-policy warning is deliberately not copied.
///
/// What is judged is the file Codex loads, `/etc/codex/requirements.toml`
/// (WI-1146): the image copy under `/usr/local/share/vogt` is only what the
/// image meant to install, and checking it passed a pod whose system file was
/// missing or different. That file must wire gitguard, and it must be owned
/// by root and writable by nobody else, along with its directory, or a
/// session could rewrite it before the next Codex starts. A named file is
/// checked as well. Unset or empty with neither file present is `Ok(None)`
/// ("no policy configured"), so a checkout or a pod built before the file
/// existed still starts; the image copy without the system file is `Err`.
pub fn codex_policy_from(value: Option<&str>) -> Result<Option<PathBuf>, String> {
    codex_policy_at(
        value,
        Path::new(IMAGE_CODEX_POLICY),
        Path::new(CODEX_REQUIREMENTS_FILE),
        true,
    )
}

/// [`codex_policy_from`] with the image and system paths given, and the
/// ownership check optional, so tests need neither root nor `/etc`.
fn codex_policy_at(
    value: Option<&str>,
    image: &Path,
    system: &Path,
    root_owned: bool,
) -> Result<Option<PathBuf>, String> {
    let loaded = || -> Result<Option<PathBuf>, String> {
        let path = readable_policy(system.to_path_buf())?;
        if root_owned {
            protected_from_sessions(system)?;
            protected_hook_binary(Path::new(GITGUARD_BIN))?;
        }
        Ok(path)
    };
    match value.map(str::trim) {
        Some("off") => Ok(None),
        Some(path) if !path.is_empty() => {
            readable_policy(PathBuf::from(path))?;
            loaded()
        }
        _ if system.exists() => loaded(),
        _ if image.exists() => Err(format!(
            "{} is installed but {} is not, and Codex reads only the latter; refusing to start a Codex session without the gitguard hook",
            image.display(),
            system.display()
        )),
        // A checkout, a unit test, or a pod built before the policy existed
        // has neither file. That is "no policy configured", which launch
        // treats as off. A deployment that *names* a file still fails above.
        _ => Ok(None),
    }
}

/// Why a session could change the requirements file Codex loads: the file or
/// its directory is not owned by root, or is writable by group or others. A
/// session runs as the engine's user, so root ownership with no group/other
/// write is what keeps the hook out of its reach.
fn protected_from_sessions(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;
    let dir = path.parent().unwrap_or(Path::new("/"));
    for p in [path, dir] {
        let meta = std::fs::metadata(p).map_err(|e| {
            format!(
                "Codex requirements {} cannot be inspected ({e}); refusing to start a Codex session",
                p.display()
            )
        })?;
        if meta.uid() != 0 || meta.mode() & 0o022 != 0 {
            return Err(format!(
                "Codex requirements {} is owned by uid {} with mode {:o}; it must be root-owned and writable only by root, or a session could switch the gitguard hook off. Refusing to start a Codex session",
                p.display(),
                meta.uid(),
                meta.mode() & 0o7777
            ));
        }
    }
    Ok(())
}

/// Why a session could swap the program the gitguard hook runs: the hook
/// names [`GITGUARD_BIN`] by absolute path, so that path must be a regular
/// file (not a symlink a session could re-point), owned by root and not
/// group- or world-writable, in a directory with the same ownership. A bare
/// `klaudia` would resolve through the session's PATH, whose first entry
/// (`/opt/vogt/agent-clis/bin`) the session user owns, so an `exit 0` stub
/// there would pass every call (WI-1146).
fn protected_hook_binary(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;
    let dir = path.parent().unwrap_or(Path::new("/"));
    for p in [path, dir] {
        let meta = std::fs::symlink_metadata(p).map_err(|e| {
            format!(
                "gitguard hook program {} cannot be inspected ({e}); refusing to start a Codex session",
                p.display()
            )
        })?;
        let shape_ok = if p == path {
            meta.file_type().is_file()
        } else {
            meta.file_type().is_dir()
        };
        if !shape_ok || meta.uid() != 0 || meta.mode() & 0o022 != 0 {
            return Err(format!(
                "gitguard hook program {} is {} owned by uid {} with mode {:o}; it must be a root-owned regular file in a root-owned directory, writable only by root, or a session could replace it. Refusing to start a Codex session",
                p.display(),
                if meta.file_type().is_symlink() { "a symlink" } else { "a path" },
                meta.uid(),
                meta.mode() & 0o7777
            ));
        }
    }
    Ok(())
}

fn readable_policy(path: PathBuf) -> Result<Option<PathBuf>, String> {
    match std::fs::read(&path) {
        Ok(bytes) if !bytes.is_empty() => {
            let text = String::from_utf8_lossy(&bytes);
            check_codex_policy(&text).map_err(|why| {
                format!(
                    "Codex driven-session policy {} does not wire the gitguard hook ({why}); refusing to start a Codex session without it",
                    path.display()
                )
            })?;
            Ok(Some(path))
        }
        Ok(_) => Err(format!(
            "Codex driven-session policy {} is empty; refusing to start a Codex session without the gitguard hook",
            path.display()
        )),
        Err(e) => Err(format!(
            "Codex driven-session policy {} cannot be read ({e}); refusing to start a Codex session without the gitguard hook",
            path.display()
        )),
    }
}

/// The program the gitguard hook runs: the image's root-owned Klaudia build,
/// by absolute path (WI-1146). Never resolved through PATH, and never a
/// runtime `VOGT_KLAUDIA_VERSION` copy, which lives under the session-owned
/// `/opt/vogt/agent-clis`.
const GITGUARD_BIN: &str = "/usr/local/bin/klaudia";

/// The hook command the Codex requirements file must run (WI-1136): exactly
/// this string, so a bare `klaudia` (resolved through the session's PATH) is
/// refused.
const GITGUARD_HOOK: &str = "/usr/local/bin/klaudia hook gitguard";

/// PreToolUse tool names the gitguard matcher must select: the fork sends
/// `Bash` for shell, exec_command and write_stdin, and `apply_patch` for
/// patches (WI-1356).
const GITGUARD_TOOLS: &[&str] = &["Bash", "apply_patch"];

/// Why a Codex requirements file would let a session run without gitguard
/// (WI-1356). It must be TOML with `[[hooks.SessionStart]]` and
/// `[[hooks.PreToolUse]]` groups, each holding a `type = "command"` handler
/// that runs `/usr/local/bin/klaudia hook gitguard` with `failClosed = true`, and the
/// PreToolUse group's matcher must select every name in [`GITGUARD_TOOLS`].
/// It must also pin `[features] hooks = true`: the fork runs managed hooks
/// only while that feature is on, and without the pin a session could switch
/// it off with `-c features.hooks=false` or `--disable hooks`.
/// A comments-only file — what WI-1146 shipped — fails here, so the launch
/// is refused rather than run unguarded.
pub fn check_codex_policy(text: &str) -> std::result::Result<(), String> {
    let value: toml::Table = toml::from_str(text).map_err(|e| format!("not TOML: {e}"))?;
    let pinned = value
        .get("features")
        .and_then(toml::Value::as_table)
        .and_then(|features| features.get("hooks"))
        .and_then(toml::Value::as_bool);
    if pinned != Some(true) {
        return Err("no [features] hooks = true pin".to_string());
    }
    let hooks = value
        .get("hooks")
        .and_then(toml::Value::as_table)
        .ok_or("no [hooks] table")?;
    for event in ["SessionStart", "PreToolUse"] {
        let groups = hooks
            .get(event)
            .and_then(toml::Value::as_array)
            .ok_or_else(|| format!("no [[hooks.{event}]]"))?;
        let guarded = groups.iter().any(|group| {
            let matcher = group.get("matcher").and_then(toml::Value::as_str);
            let selects_tools = event != "PreToolUse" || matcher_selects_gitguard_tools(matcher);
            let handlers = group.get("hooks").and_then(toml::Value::as_array);
            selects_tools
                && handlers.is_some_and(|handlers| {
                    handlers.iter().any(|h| {
                        h.get("type").and_then(toml::Value::as_str) == Some("command")
                            && h.get("command")
                                .and_then(toml::Value::as_str)
                                .map(str::trim)
                                == Some(GITGUARD_HOOK)
                            && h.get("failClosed").and_then(toml::Value::as_bool) == Some(true)
                    })
                })
        });
        if !guarded {
            return Err(format!(
                "no [[hooks.{event}]] runs `{GITGUARD_HOOK}` with failClosed = true"
            ));
        }
    }
    Ok(())
}

/// Whether a Codex hook matcher selects every gitguard tool. Codex treats a
/// missing, empty or `*` matcher as match-all and anything else as a regex.
fn matcher_selects_gitguard_tools(matcher: Option<&str>) -> bool {
    match matcher.map(str::trim) {
        None | Some("") | Some("*") => true,
        Some(pattern) => regex::Regex::new(pattern)
            .is_ok_and(|re| GITGUARD_TOOLS.iter().all(|tool| re.is_match(tool))),
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

impl Default for Onboarding {
    fn default() -> Self {
        Self {
            enabled: false,
            default_dir: None,
            settings_file: None,
            opencode_config: None,
            accounts: Vec::new(),
            // A hand-built config has no policy. A named missing file is the
            // error; "not configured" is not.
            codex_policy: Ok(None),
        }
    }
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
            accounts: parse_accounts(
                std::env::var("ENGINE_AGENT_CLAUDE_ACCOUNTS")
                    .ok()
                    .as_deref(),
            ),
            codex_policy: codex_policy_from(
                std::env::var("ENGINE_AGENT_CODEX_POLICY").ok().as_deref(),
            ),
        }
    }

    /// The `CLAUDE_CONFIG_DIR` for the account `name`, or why there is none.
    pub fn account_dir(&self, name: &str) -> std::result::Result<&Path, String> {
        self.accounts
            .iter()
            .find(|a| a.name == name)
            .map(|a| a.dir.as_path())
            .ok_or_else(|| {
                let known: Vec<&str> = self.accounts.iter().map(|a| a.name.as_str()).collect();
                if known.is_empty() {
                    format!("account {name:?}: no accounts are configured (ENGINE_AGENT_CLAUDE_ACCOUNTS)")
                } else {
                    format!("account {name:?} is not one of {}", known.join(", "))
                }
            })
    }

    /// Every Claude Code config directory whose `projects/` may hold a
    /// transcript: the engine's own `CLAUDE_CONFIG_DIR` when set, then each
    /// configured account. The default `~/.claude` is the caller's to add.
    pub fn claude_dirs(&self) -> Vec<PathBuf> {
        let mut dirs: Vec<PathBuf> = std::env::var_os("CLAUDE_CONFIG_DIR")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .into_iter()
            .collect();
        for account in &self.accounts {
            if !dirs.contains(&account.dir) {
                dirs.push(account.dir.clone());
            }
        }
        dirs
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
    fn accounts_parse_to_named_absolute_directories() {
        let accounts = parse_accounts(Some(" main=/home/a/.claude , acct2=/home/a/.claude-acct2,"));
        assert_eq!(
            accounts,
            vec![
                ClaudeAccount {
                    name: "main".into(),
                    dir: PathBuf::from("/home/a/.claude")
                },
                ClaudeAccount {
                    name: "acct2".into(),
                    dir: PathBuf::from("/home/a/.claude-acct2")
                },
            ]
        );
        assert!(parse_accounts(None).is_empty());
    }

    #[test]
    fn a_bad_account_entry_is_skipped_and_the_rest_kept() {
        let accounts = parse_accounts(Some(
            "noequals,Upper=/x,rel=relative/dir,ok=/ok,ok=/second,=/empty,../up=/y",
        ));
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].dir, PathBuf::from("/ok"));
    }

    #[test]
    fn an_unknown_account_is_refused_with_the_known_names() {
        let onboarding = Onboarding {
            accounts: parse_accounts(Some("a=/a,b=/b")),
            ..Default::default()
        };
        assert_eq!(onboarding.account_dir("b"), Ok(Path::new("/b")));
        let err = onboarding.account_dir("c").unwrap_err();
        assert!(err.contains("a, b"), "{err}");
        let none = Onboarding::default().account_dir("a").unwrap_err();
        assert!(none.contains("no accounts are configured"), "{none}");
    }

    #[test]
    fn the_session_config_dir_wins() {
        let onboarding = Onboarding {
            enabled: true,
            default_dir: Some(PathBuf::from("/home/pod")),
            settings_file: None,
            opencode_config: None,
            accounts: Vec::new(),
            codex_policy: Ok(None),
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

/// The shipped driven-session policy (`engine/deploy/driven-session-settings.json`)
/// is a security boundary, so it is pinned here (WI-926).
///
/// Two layers decide a driven Claude session's actions. Pattern rules
/// (`permissions.deny`, then `ask`, then `allow`) resolve first and never
/// reach the classifier; everything else goes to auto mode's classifier,
/// judged against its built-in rules plus this file's `autoMode` entries.
/// These tests prove what the file itself decides: it keeps every built-in
/// rule, it adds no pattern rule (so a merge, force-push, `rm -rf`, Komodo
/// write, Infisical read or `docker compose down` is never pattern-allowed or
/// downgraded to a prompt), its allow list is exactly the five entries
/// below, and the two read-only exceptions say, word for word, what was
/// reviewed. Rewording a classifier prompt is a change to this boundary, so
/// it must come with a deliberate change to the golden text here.
///
/// What they cannot prove is the classifier's verdict: whether it actually
/// lets `docker ps` through under *Read-Only Inspection*, or still denies a
/// `.Config.Env` dump, is judged by a model at run time. That needs a check
/// on vogt-dev after merge (and the deployment's own policy file, which
/// replaces this one, must carry the same rules).
#[cfg(test)]
mod driven_policy_tests {
    use serde_json::Value;

    const SHIPPED: &str = include_str!("../../deploy/driven-session-settings.json");

    /// The reviewed text of *Read-Only Inspection* (PR #874).
    const READ_ONLY_INSPECTION: &str = r#"Read-Only Inspection: Running one of these specific commands, which report existing state and change nothing: `docker ps`, `docker logs`, `docker stats`; `docker inspect --format` restricted to the fields `.State`, `.Name`, `.Id`, `.Created`, `.RestartCount`, `.Config.Image`, `.Config.Labels`, `.NetworkSettings` and `.Mounts`; `ss`, `netstat`; `git status`, `git log`, `git diff`, `git show`, `git branch`; `gh pr view`, `gh pr list`, `gh pr checks`, `gh pr diff`, `gh run view`; and a `curl` or `wget` GET to `127.0.0.1` or `localhost` carrying no request body, no added headers and no credentials. This list is exhaustive, not illustrative: a command it does not name is not covered. It clears only Modify Shared Resources and Interfere With Workloads, and only for the commands named. It never covers any other `docker` subcommand — including `exec`, `inspect` of `.Config.Env` or of a whole object, `compose`, `run`, `create`, `start`, `stop`, `restart`, `rm`, `cp` — any request that is not a credential-free loopback GET (so not Komodo, Infisical, a secrets store, another host, a cloud metadata address, or any non-GET method), reading another process's or container's environment, printing a secret or any part of one, writing to any shared service, or sending data off the host. Data Exfiltration and Production Reads are never cleared."#;

    /// The reviewed text of *Credential Presence Check* (PR #874).
    const CREDENTIAL_PRESENCE_CHECK: &str = r#"Credential Presence Check: Establishing that a credential this session already holds is set or unset, and nothing beyond that — testing whether one of this session's own environment variables is non-empty, whether a path this session owns exists, and reading the `AGENT_AUTH_MODE`, `AGENT_AUTH_GRANTED`, `AGENT_AUTH_ONDEMAND` and `AGENT_AUTH_WRITABLE` lists, which carry names only. The sole fact this may yield is set or unset. It never yields any other property of a value: not its length, a substring, a slice, a hash, a checksum, a character or byte count, nor the outcome of comparing it with anything — each of those leaks the value and stays blocked. It is confined to this session's own environment and its own files: it never covers another process's or container's environment (including any `/proc/*/environ`), the engine's or the core's environment, `/run/secrets` or any other credential store on disk, the vault or a secrets manager, printing a value, copying one into a file, variable, log, commit or message, or obtaining a credential this session does not already hold. It clears Credential Exposure for the set-or-unset check alone; Credential Exploration and Data Exfiltration are never cleared, and obtaining a credential stays governed by the deployment's manifest and the Vogt grant flow."#;

    fn policy() -> Value {
        serde_json::from_str(SHIPPED).expect("the shipped policy is JSON")
    }

    fn allow_rules() -> Vec<String> {
        policy()["autoMode"]["allow"]
            .as_array()
            .expect("autoMode.allow is a list")
            .iter()
            .map(|r| r.as_str().expect("every rule is prose").to_string())
            .collect()
    }

    fn rule(name: &str) -> String {
        let prefix = format!("{name}:");
        allow_rules()
            .into_iter()
            .find(|r| r.starts_with(&prefix))
            .unwrap_or_else(|| panic!("the shipped policy has no `{name}` rule"))
    }

    /// Where `needle` first appears in `rule`, failing the test if it does not.
    fn at(rule: &str, needle: &str) -> usize {
        rule.find(needle)
            .unwrap_or_else(|| panic!("{needle:?} is missing from: {rule}"))
    }

    /// Claude Code's documented Bash rule matching, enough to ask whether
    /// any rule in the file would catch a command: `*` matches any text,
    /// a trailing ` *` (or `:*`) also matches the bare command, and a
    /// compound command is matched part by part.
    fn bash_rule_matches(rule: &str, command: &str) -> bool {
        let Some(pattern) = rule.strip_prefix("Bash(").and_then(|r| r.strip_suffix(')')) else {
            // A bare tool name (`Bash`) matches every call of that tool.
            return rule == "Bash";
        };
        let pattern = pattern
            .strip_suffix(":*")
            .map_or_else(|| pattern.to_string(), |p| format!("{p} *"));
        let glob = format!(
            "^{}$",
            pattern
                .split('*')
                .map(regex::escape)
                .collect::<Vec<_>>()
                .join(".*")
        );
        let re = regex::Regex::new(&glob).unwrap();
        let bare = pattern
            .strip_suffix(" *")
            .filter(|p| !p.contains('*'))
            .map(str::to_string);
        regex::Regex::new(r"&&|\|\||;|\||\n")
            .unwrap()
            .split(command)
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .any(|part| re.is_match(part) || bare.as_deref() == Some(part))
    }

    /// The pattern verdict for a command, or `None` when no pattern rule
    /// matches and the classifier decides.
    fn pattern_verdict(command: &str) -> Option<&'static str> {
        let policy = policy();
        for list in ["deny", "ask", "allow"] {
            let rules = policy["permissions"][list]
                .as_array()
                .cloned()
                .unwrap_or_default();
            if rules
                .iter()
                .filter_map(Value::as_str)
                .any(|r| bash_rule_matches(r, command))
            {
                return Some(list);
            }
        }
        None
    }

    #[test]
    fn every_built_in_rule_is_kept() {
        // A list without "$defaults" replaces Claude Code's whole list for
        // that section: soft_deny would lose force-push, prod deploys and
        // credential rules; hard_deny would lose Data Exfiltration.
        let auto = &policy()["autoMode"];
        for list in ["environment", "allow", "soft_deny", "hard_deny"] {
            if let Some(rules) = auto.get(list) {
                let rules = rules.as_array().expect("a rule list");
                assert_eq!(
                    rules.first().and_then(Value::as_str),
                    Some("$defaults"),
                    "autoMode.{list} must start with \"$defaults\""
                );
            }
        }
        // The policy only adds exceptions: it never relaxes a built-in block
        // rule by redefining it, nor turns the classifier off for the shell.
        assert!(auto.get("soft_deny").is_none(), "no soft_deny override");
        assert!(auto.get("hard_deny").is_none(), "no hard_deny override");
        assert!(auto.get("classifyAllShell").is_none());
    }

    #[test]
    fn the_allow_list_is_exactly_these_five_in_order() {
        let names: Vec<String> = allow_rules()
            .iter()
            .map(|r| r.split(':').next().unwrap().to_string())
            .collect();
        assert_eq!(
            names,
            [
                "$defaults",
                "Read-Only Inspection",
                "Credential Presence Check",
                "Own Green PR Merge",
                "Approved Vogt Grant",
            ],
            "adding, removing or reordering a classifier exception is a reviewed change"
        );
        let env = policy()["autoMode"]["environment"].to_string();
        assert!(
            env.contains("**Autonomous-merge repositories**: none configured"),
            "the image ships no autonomous-merge repository"
        );
    }

    #[test]
    fn there_is_no_pattern_rule_so_the_classifier_judges_every_command() {
        // A pattern allow cannot tell `docker inspect --format '{{.State}}'`
        // from a `.Config.Env` dump, or a curl GET from one with `-d @file`,
        // and resolves before the classifier sees the command. A pattern ask
        // on `gh pr merge` is deferred to WI-983: any agent with work.write
        // can answer a session's prompt, so it would let one agent approve
        // another's merge.
        let policy = policy();
        for list in ["allow", "ask", "deny"] {
            let rules = policy["permissions"][list]
                .as_array()
                .cloned()
                .unwrap_or_default();
            assert!(
                rules.is_empty(),
                "permissions.{list} must stay empty in the shipped policy: {rules:?}"
            );
        }
        // Nor may the file pick a posture: bypass is a person's grant, made
        // per spawn (`permission_mode`), never by the policy itself.
        assert!(policy["permissions"].get("defaultMode").is_none());
        assert!(policy.get("skipDangerousModePermissionPrompt").is_none());
    }

    #[test]
    fn read_only_inspection_is_the_reviewed_text() {
        assert_eq!(rule("Read-Only Inspection"), READ_ONLY_INSPECTION);
    }

    #[test]
    fn credential_presence_check_is_the_reviewed_text() {
        assert_eq!(rule("Credential Presence Check"), CREDENTIAL_PRESENCE_CHECK);
    }

    #[test]
    fn read_only_inspection_keeps_its_clauses_in_order() {
        let rule = READ_ONLY_INSPECTION;
        // allowed list → "exhaustive" → what it clears → what it never covers.
        let closed = at(rule, "This list is exhaustive, not illustrative");
        let clears = at(
            rule,
            "It clears only Modify Shared Resources and Interfere With Workloads",
        );
        let never = at(rule, "It never covers");
        let final_ = at(
            rule,
            "Data Exfiltration and Production Reads are never cleared.",
        );
        assert!(closed < clears && clears < never && never < final_);
        assert!(rule.ends_with("Data Exfiltration and Production Reads are never cleared."));
        // Every exclusion sits in the "never covers" clause, so none can
        // drift into the list of what is allowed.
        for excluded in [
            "`exec`",
            "`inspect` of `.Config.Env` or of a whole object",
            "`compose`",
            "`run`",
            "`create`",
            "`start`",
            "`stop`",
            "`restart`",
            "`rm`",
            "`cp`",
            "not Komodo, Infisical, a secrets store, another host, a cloud metadata address, or any non-GET method",
            "reading another process's or container's environment",
            "printing a secret or any part of one",
            "writing to any shared service",
            "sending data off the host",
        ] {
            assert!(at(rule, excluded) > never, "{excluded:?} must follow \"It never covers\"");
        }
        // And the allowed part names none of the dangerous things at all.
        let allowed = rule[..closed].to_lowercase();
        for word in [
            "exec",
            "compose",
            "komodo",
            "infisical",
            "secret",
            "environ",
            "push",
            "merge",
            "deploy",
            "stack",
            "delete",
            "post",
            "put",
            "kubectl",
            "ssh",
            "metadata",
        ] {
            assert!(
                !allowed.contains(word),
                "the allowed list must not mention {word:?}"
            );
        }
        // The GET is bounded to loopback.
        assert!(rule[..closed].contains("GET to `127.0.0.1` or `localhost` carrying no request body, no added headers and no credentials"));
    }

    #[test]
    fn credential_presence_check_keeps_its_clauses_in_order() {
        let rule = CREDENTIAL_PRESENCE_CHECK;
        let scope = at(
            rule,
            "a credential this session already holds is set or unset",
        );
        let sole = at(rule, "The sole fact this may yield is set or unset.");
        let oracle = at(rule, "It never yields any other property of a value");
        let confined = at(
            rule,
            "It is confined to this session's own environment and its own files",
        );
        let never = at(rule, "it never covers");
        let clears = at(
            rule,
            "It clears Credential Exposure for the set-or-unset check alone",
        );
        assert!(
            scope < sole
                && sole < oracle
                && oracle < confined
                && confined < never
                && never < clears
        );
        // Every value oracle is named in the "never yields" clause.
        for oracle_item in [
            "its length",
            "a substring",
            "a slice",
            "a hash",
            "a checksum",
            "a character or byte count",
            "the outcome of comparing it with anything",
        ] {
            let i = at(rule, oracle_item);
            assert!(
                oracle < i && i < confined,
                "{oracle_item:?} belongs to the oracle clause"
            );
        }
        // Every out-of-scope store is named after "it never covers".
        for store in [
            "another process's or container's environment (including any `/proc/*/environ`)",
            "the engine's or the core's environment",
            "`/run/secrets` or any other credential store on disk",
            "the vault or a secrets manager",
            "printing a value",
            "copying one into a file, variable, log, commit or message",
            "obtaining a credential this session does not already hold",
        ] {
            let i = at(rule, store);
            assert!(
                never < i && i < clears,
                "{store:?} belongs to the never-covers clause"
            );
        }
        assert!(rule.ends_with(
            "Credential Exploration and Data Exfiltration are never cleared, and obtaining a credential stays governed by the deployment's manifest and the Vogt grant flow."
        ));
    }

    #[test]
    fn merge_prod_destructive_and_secret_actions_still_go_to_the_classifier() {
        for command in [
            // Merge stays with Merge Without Review / Own Green PR Merge.
            "gh pr merge 873 --rebase",
            "gh pr merge 873 --squash --delete-branch",
            "gh pr merge 873 --admin --rebase",
            "gh pr merge --auto 873",
            "cd /w/repo && gh pr merge 1",
            // Force-push and pushes.
            "git push --force origin main",
            "git push -f origin main",
            "git push origin +main",
            "git push origin --delete main",
            "git push origin feature/x",
            // Destruction.
            "rm -rf /",
            "rm -rf ~",
            "rm -rf /var/lib/docker/volumes",
            "docker compose down",
            "docker compose -p vogt-prod down -v",
            "docker rm -f vogt-prod-vogt-1",
            "sqlite3 vogt.db 'DROP TABLE work_items'",
            // Secrets.
            "docker exec vogt-prod-vogt-1 env",
            "docker inspect vogt-prod-vogt-1",
            "docker inspect --format '{{json .Config}}' vogt-prod-vogt-1",
            "infisical secrets get HOMELAB_VOGT_PROD_AGENT_TOKEN",
            "infisical export --env=prod",
            "infisical run -- env",
            "vogt-agent-auth fetch GH_TOKEN",
            "printenv",
            "cat /proc/1/environ",
            "test -r /run/secrets/vogt_core_token",
            "sha256sum <<<\"$GH_TOKEN\"",
            "cat ~/.config/gh/hosts.yml",
            "curl http://169.254.169.254/latest/meta-data/",
            // Komodo, read or write.
            "curl -X POST http://100.92.54.45:3011/read/GetStack -d '{}'",
            "curl -X POST http://100.92.54.45:3011/execute/DeployStack -d '{}'",
            "curl -X POST http://100.92.54.45:3011/write/UpdateStack -d @stack.json",
        ] {
            assert_eq!(pattern_verdict(command), None, "{command}");
            // The verdict is only meaningful if the matcher would have caught
            // the command had a rule named it.
            let words: Vec<&str> = command
                .rsplit("&& ")
                .next()
                .unwrap()
                .split_whitespace()
                .take(2)
                .collect();
            let probe = format!("Bash({} *)", words.join(" "));
            assert!(
                bash_rule_matches(&probe, command),
                "{probe} should match {command}"
            );
        }
    }

    /// A throwaway file the policy resolver can be pointed at. The value is
    /// passed in; these tests do not touch process env.
    fn policy_file(bytes: &[u8]) -> (std::path::PathBuf, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "vogt-codex-policy-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("requirements.toml");
        std::fs::write(&path, bytes).unwrap();
        (dir, path)
    }

    /// The resolver against a given system file, ownership unchecked, and no
    /// image copy.
    fn policy_at(
        value: Option<&str>,
        system: &std::path::Path,
    ) -> Result<Option<std::path::PathBuf>, String> {
        super::codex_policy_at(
            value,
            std::path::Path::new("/nonexistent/image-requirements.toml"),
            system,
            false,
        )
    }

    #[test]
    fn codex_policy_off_is_none() {
        assert_eq!(super::codex_policy_from(Some("off")).unwrap(), None);
        assert_eq!(super::codex_policy_from(Some("  off  ")).unwrap(), None);
    }

    #[test]
    fn codex_policy_named_file_must_be_readable_and_non_empty() {
        let (sys_dir, system) = policy_file(SHIPPED_CODEX_POLICY.as_bytes());
        let (dir, path) = policy_file(SHIPPED_CODEX_POLICY.as_bytes());
        // The file Codex loads is the answer, whatever was named.
        let got = policy_at(Some(path.to_str().unwrap()), &system)
            .unwrap()
            .unwrap();
        assert_eq!(got, system);
        std::fs::remove_dir_all(dir).ok();

        let (dir, empty_path) = policy_file(b"");
        let err = policy_at(Some(empty_path.to_str().unwrap()), &system).unwrap_err();
        assert!(err.contains("empty"), "{err}");
        let missing = dir.join("missing.toml");
        let err = policy_at(Some(missing.to_str().unwrap()), &system).unwrap_err();
        assert!(err.contains("cannot be read"), "{err}");
        std::fs::remove_dir_all(dir).ok();
        std::fs::remove_dir_all(sys_dir).ok();
    }

    /// WI-1146: what Codex reads is `/etc/codex/requirements.toml`, so a
    /// good named or image file does not stand in for a missing or
    /// unguarded system file.
    #[test]
    fn codex_policy_judges_the_file_codex_loads() {
        let (dir, named) = policy_file(SHIPPED_CODEX_POLICY.as_bytes());
        let missing = dir.join("etc-codex-requirements.toml");
        let err = policy_at(Some(named.to_str().unwrap()), &missing).unwrap_err();
        assert!(err.contains("cannot be read"), "{err}");

        // The image copy installed, the system file not: refused, not "off".
        let err = super::codex_policy_at(None, &named, &missing, false).unwrap_err();
        assert!(err.contains("Codex reads only"), "{err}");

        // Unset with a system file that is comments only: refused, even
        // though the image copy beside it is good.
        let (sys_dir, stale) = policy_file(b"# requirements\n");
        let err = super::codex_policy_at(None, &named, &stale, false).unwrap_err();
        assert!(err.contains("does not wire the gitguard hook"), "{err}");
        let err = policy_at(Some(named.to_str().unwrap()), &stale).unwrap_err();
        assert!(err.contains("does not wire the gitguard hook"), "{err}");
        std::fs::remove_dir_all(dir).ok();
        std::fs::remove_dir_all(sys_dir).ok();
    }

    /// A requirements file the session user owns (as every file a test can
    /// make is) could be rewritten by a session, so it is refused.
    #[test]
    fn a_codex_policy_the_session_can_rewrite_is_refused() {
        let (dir, system) = policy_file(SHIPPED_CODEX_POLICY.as_bytes());
        if unsafe { libc::geteuid() } == 0 {
            eprintln!("skipping: running as root, every temp file is root-owned");
            std::fs::remove_dir_all(dir).ok();
            return;
        }
        let err = super::codex_policy_at(None, std::path::Path::new("/nonexistent"), &system, true)
            .unwrap_err();
        assert!(err.contains("must be root-owned"), "{err}");
        std::fs::remove_dir_all(dir).ok();
    }

    /// The gitguard hook program must be a root-owned regular file: one the
    /// session user owns (as every file a test can make is), a symlink, or a
    /// missing path is refused (WI-1146).
    #[test]
    fn a_gitguard_program_the_session_can_replace_is_refused() {
        let dir = std::env::temp_dir().join(format!("gitguard-bin-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let err = super::protected_hook_binary(&dir.join("missing")).unwrap_err();
        assert!(err.contains("cannot be inspected"), "{err}");
        let link = dir.join("klaudia-link");
        std::os::unix::fs::symlink("/bin/sh", &link).unwrap();
        let err = super::protected_hook_binary(&link).unwrap_err();
        assert!(err.contains("a symlink"), "{err}");
        if unsafe { libc::geteuid() } != 0 {
            let own = dir.join("klaudia");
            std::fs::write(&own, "#!/bin/sh\nexit 0\n").unwrap();
            let err = super::protected_hook_binary(&own).unwrap_err();
            assert!(err.contains("must be a root-owned regular file"), "{err}");
        }
        // A root-owned regular file in a root-owned directory passes.
        if std::path::Path::new("/bin/sh").exists() {
            let sh = std::fs::canonicalize("/bin/sh").unwrap();
            super::protected_hook_binary(&sh).unwrap();
        }
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn the_shipped_codex_policy_runs_the_root_owned_klaudia() {
        assert_eq!(
            SHIPPED_CODEX_POLICY
                .matches(&format!("command = \"{}\"", super::GITGUARD_HOOK))
                .count(),
            2
        );
        assert!(super::GITGUARD_HOOK.starts_with(&format!("{} ", super::GITGUARD_BIN)));
    }

    const SHIPPED_CODEX_POLICY: &str =
        include_str!("../../deploy/driven-session-codex-requirements.toml");

    #[test]
    fn the_shipped_codex_policy_wires_gitguard() {
        super::check_codex_policy(SHIPPED_CODEX_POLICY).unwrap();
    }

    #[test]
    fn a_codex_policy_without_a_fail_closed_gitguard_hook_is_refused() {
        // The comments-only file WI-1146 shipped: Codex ran unguarded.
        let (sys_dir, system) = policy_file(SHIPPED_CODEX_POLICY.as_bytes());
        let (dir, path) = policy_file(b"# requirements\n");
        let err = policy_at(Some(path.to_str().unwrap()), &system).unwrap_err();
        assert!(err.contains("does not wire the gitguard hook"), "{err}");
        std::fs::remove_dir_all(dir).ok();
        std::fs::remove_dir_all(sys_dir).ok();

        let check = super::check_codex_policy;
        let without = |from: &str, to: &str| {
            assert!(SHIPPED_CODEX_POLICY.contains(from), "{from}");
            SHIPPED_CODEX_POLICY.replacen(from, to, 1)
        };
        // failClosed missing or false on either event.
        assert!(check(&without("\nfailClosed = true", "\nfailClosed = false")).is_err());
        assert!(check(&SHIPPED_CODEX_POLICY.replace("failClosed = true\n", "")).is_err());
        // No SessionStart group: PreToolUse would have no baseline to compare.
        assert!(check(&without("[[hooks.SessionStart]]", "[[hooks.Stop]]")).is_err());
        // A matcher that misses apply_patch, or one that is not a regex.
        assert!(check(&SHIPPED_CODEX_POLICY.replace(
            "^(Bash|shell|exec_command|write_stdin|apply_patch)$",
            "^Bash$"
        ))
        .is_err());
        assert!(check(&SHIPPED_CODEX_POLICY.replace(
            "^(Bash|shell|exec_command|write_stdin|apply_patch)$",
            "(Bash"
        ))
        .is_err());
        // Another command.
        assert!(check(&SHIPPED_CODEX_POLICY.replace(super::GITGUARD_HOOK, "true")).is_err());
        // Klaudia by bare name, or from a session-writable path (WI-1146):
        // PATH lookup would let a session shadow it with a stub.
        for shadowable in [
            "klaudia hook gitguard",
            "/opt/vogt/agent-clis/bin/klaudia hook gitguard",
            "env klaudia hook gitguard",
        ] {
            let err =
                check(&SHIPPED_CODEX_POLICY.replace(super::GITGUARD_HOOK, shadowable)).unwrap_err();
            assert!(err.contains(super::GITGUARD_HOOK), "{err}");
        }
        assert!(check("not = [toml").is_err());
        // The hooks feature not pinned on: a session could disable it.
        let err = check(&without("[features]\nhooks = true\n", "")).unwrap_err();
        assert!(err.contains("[features] hooks = true"), "{err}");
        assert!(check(&without(
            "[features]\nhooks = true",
            "[features]\nhooks = false"
        ))
        .is_err());
        assert!(check(&without(
            "[features]\nhooks = true",
            "[features]\nhooks = \"true\""
        ))
        .is_err());
        assert!(check(&without(
            "[features]\nhooks = true",
            "[features]\nother = true"
        ))
        .is_err());
    }

    #[test]
    fn codex_policy_unset_without_image_files_is_unconfigured() {
        // Neither the image copy nor the system file exists. Unset and empty
        // are "no policy configured", not a refusal — a named path is
        // checked, above. Paths given so the test holds inside the image too.
        let none = std::path::Path::new("/nonexistent/requirements.toml");
        for value in [None, Some(""), Some("   ")] {
            assert_eq!(
                super::codex_policy_at(value, none, none, true).unwrap(),
                None
            );
        }
    }
}
