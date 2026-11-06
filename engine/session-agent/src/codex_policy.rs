//! The Codex gitguard policy check (WI-1143, WI-1146, WI-1356), shared by the
//! engine (a host session) and the session agent (a pod, WI-3335). Codex
//! loads [`CODEX_REQUIREMENTS_FILE`] itself, so whoever starts Codex checks
//! that file and the hook program on the filesystem Codex will run on: the
//! engine's own for a host session, the pod's for a pod session.

use std::path::{Path, PathBuf};

/// The managed requirements path Codex reads, and the only one: the fork's
/// loader has no flag or variable for another (WI-1146).
pub const CODEX_REQUIREMENTS_FILE: &str = "/etc/codex/requirements.toml";

/// The requirements file at `system`, checked to wire gitguard, and (with
/// `root_owned`) checked, with the hook program at `hook_bin`, to be out of
/// the session user's reach. `Err` refuses the Codex launch.
pub fn guarded(
    system: &Path,
    hook_bin: &Path,
    root_owned: bool,
) -> Result<Option<PathBuf>, String> {
    let path = readable_policy(system.to_path_buf())?;
    if root_owned {
        protected_from_sessions(system)?;
        protected_hook_binary(hook_bin)?;
    }
    Ok(path)
}

/// Why a session could change the requirements file Codex loads: the file or
/// its directory is not owned by root, or is writable by group or others. A
/// session runs as the engine's user, so root ownership with no group/other
/// write is what keeps the hook out of its reach.
pub fn protected_from_sessions(path: &Path) -> Result<(), String> {
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
pub fn protected_hook_binary(path: &Path) -> Result<(), String> {
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

pub fn readable_policy(path: PathBuf) -> Result<Option<PathBuf>, String> {
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
pub const GITGUARD_BIN: &str = "/usr/local/bin/klaudia";

/// The hook command the Codex requirements file must run (WI-1136): exactly
/// this string, so a bare `klaudia` (resolved through the session's PATH) is
/// refused.
pub const GITGUARD_HOOK: &str = "/usr/local/bin/klaudia hook gitguard";

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
