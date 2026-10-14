//! Workspace guidance for driven Codex sessions (WI-1357).
//!
//! Claude Code loads `CLAUDE.md` from every ancestor of its working directory,
//! `.claude/rules/*.md`, `~/.claude/CLAUDE.md`, `~/.claude/rules` and the
//! skills under `.claude/skills`. Codex reads only `AGENTS.md` from its git
//! root down to its working directory, and skills from `$CODEX_HOME`. The
//! workspace guidance lives above the checkout (`~/Working/AGENTS.md`,
//! `~/Working/.claude/…`), which is not a git repository, so a Codex session
//! started with none of it.
//!
//! The engine gathers that set itself and hands it over as one
//! `-c developer_instructions="…"` override. That layers on top of the user's
//! `~/.codex/config.toml`: `CODEX_HOME` is left alone, so the MCP servers,
//! providers, trust entries, login and transcripts stay where they are. The
//! fork's own knobs do not reach far enough: `[skills] extra_user_roots` is
//! confined under `CODEX_HOME`, and its project-doc walk stops at the `.git`
//! marker. Skills are passed as an index (name, description, `SKILL.md` path)
//! that the model opens when one applies, the way Claude Code presents them.
//!
//! Every file is resolved through symlinks and kept only when the real path
//! is a Markdown file inside an allowed root (the top of the walk, or `$HOME`)
//! and outside the credential directories in [`DENIED_COMPONENTS`]. A
//! repository can therefore not commit `.claude/rules/x.md -> ~/.codex/auth.json`
//! and have the login read into a prompt.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Largest single file read. A larger one is left out and named.
pub const MAX_FILE_BYTES: u64 = 32 * 1024;
/// Upper bound on the whole instructions body. It travels as one argv entry,
/// and Linux caps a single argument at 128 KiB.
pub const MAX_TOTAL_BYTES: usize = 96 * 1024;
/// Longest skill description kept in the index.
const MAX_DESCRIPTION_CHARS: usize = 400;

/// Path components that are never read, wherever a symlink points.
const DENIED_COMPONENTS: &[&str] = &[
    ".codex",
    ".ssh",
    ".gnupg",
    ".aws",
    ".azure",
    ".kube",
    ".docker",
    ".git",
    ".config",
    ".local",
    ".cache",
    ".password-store",
    ".terraform.d",
];

/// The key the override sets. Any argument already setting it means the
/// command carries guidance from an earlier start.
const KEY: &str = "developer_instructions";

/// The `-c` value for a Codex session working in `cwd`, or `None` when there
/// is no guidance to carry. `home` is the engine user's home directory.
pub fn developer_instructions_override(cwd: &Path, home: Option<&Path>) -> Option<String> {
    let body = gather(cwd, home)?;
    Some(format!("{KEY}={}", toml_basic_string(&body)))
}

/// True when `command` already carries a guidance override.
pub fn has_override(command: &[String]) -> bool {
    command
        .iter()
        .any(|arg| arg.starts_with(&format!("{KEY}=")))
}

struct Walk {
    /// Canonical roots a file must resolve inside.
    allowed: Vec<PathBuf>,
    /// Canonical directories, farthest first, ending at `cwd`.
    dirs: Vec<PathBuf>,
    /// Directories whose `AGENTS.md` Codex already loads (git root to cwd).
    native_agents: HashSet<PathBuf>,
    home: Option<PathBuf>,
}

fn plan(cwd: &Path, home: Option<&Path>) -> Option<Walk> {
    let cwd = cwd.canonicalize().ok()?;
    let home = home.and_then(|h| h.canonicalize().ok());
    let git_root = cwd.ancestors().find(|dir| dir.join(".git").exists());
    // Inside $HOME the walk climbs to $HOME, past the git root: that is where
    // the workspace files are. Elsewhere it stops at the checkout, so a host
    // directory's files never ride along.
    let top = match &home {
        Some(home) if cwd.starts_with(home) => home.clone(),
        _ => git_root
            .map(Path::to_path_buf)
            .unwrap_or_else(|| cwd.clone()),
    };
    let mut dirs: Vec<PathBuf> = cwd
        .ancestors()
        .take_while(|dir| dir.starts_with(&top))
        .map(Path::to_path_buf)
        .collect();
    dirs.reverse();
    let native_agents = match git_root {
        Some(root) => dirs
            .iter()
            .filter(|dir| dir.starts_with(root))
            .cloned()
            .collect(),
        None => HashSet::from([cwd.clone()]),
    };
    let mut allowed = vec![top];
    allowed.extend(home.clone());
    Some(Walk {
        allowed,
        dirs,
        native_agents,
        home,
    })
}

/// The real path of `path`, when it is a regular Markdown file inside an
/// allowed root, outside every denied directory, and small enough.
fn confined(path: &Path, allowed: &[PathBuf]) -> Option<PathBuf> {
    let real = path.canonicalize().ok()?;
    if real.extension().and_then(|e| e.to_str()) != Some("md") {
        return None;
    }
    if !allowed.iter().any(|root| real.starts_with(root)) {
        return None;
    }
    let denied = real.components().any(|c| {
        c.as_os_str()
            .to_str()
            .is_some_and(|name| DENIED_COMPONENTS.contains(&name))
    });
    if denied {
        return None;
    }
    let meta = std::fs::metadata(&real).ok()?;
    meta.is_file().then_some(real)
}

enum Read {
    Text(String),
    TooLarge,
    Refused,
}

fn read_confined(path: &Path, allowed: &[PathBuf]) -> Read {
    let Some(real) = confined(path, allowed) else {
        return Read::Refused;
    };
    match std::fs::metadata(&real) {
        Ok(meta) if meta.len() > MAX_FILE_BYTES => Read::TooLarge,
        Ok(_) => std::fs::read_to_string(&real)
            .map(Read::Text)
            .unwrap_or(Read::Refused),
        Err(_) => Read::Refused,
    }
}

fn sorted_rules(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir.join(".claude").join("rules")) else {
        return Vec::new();
    };
    let mut rules: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|e| e.to_str()) == Some("md"))
        .collect();
    rules.sort();
    rules
}

/// The guidance files in load order: user-level first, then each walked
/// directory from the top down, `CLAUDE.md`, `AGENTS.md` (unless Codex reads
/// it natively) and its rules.
fn candidates(walk: &Walk) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(home) = &walk.home {
        let user = home.join(".claude");
        out.push(user.join("CLAUDE.md"));
        out.extend(sorted_rules(home));
    }
    for dir in &walk.dirs {
        out.push(dir.join("CLAUDE.md"));
        if !walk.native_agents.contains(dir) {
            out.push(dir.join("AGENTS.md"));
        }
        if walk.home.as_deref() != Some(dir.as_path()) {
            out.extend(sorted_rules(dir));
        }
    }
    out
}

struct Skill {
    name: String,
    description: String,
    path: PathBuf,
}

/// Skills from `.claude/skills` in each walked directory and in
/// `~/.claude/skills`. The nearest directory wins a name clash, as in
/// Claude Code where project skills shadow user ones.
fn skills(walk: &Walk) -> Vec<Skill> {
    let mut roots: Vec<PathBuf> = walk
        .dirs
        .iter()
        .rev()
        .map(|dir| dir.join(".claude").join("skills"))
        .collect();
    if let Some(home) = &walk.home {
        roots.push(home.join(".claude").join("skills"));
    }
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for root in roots {
        let Ok(entries) = std::fs::read_dir(&root) else {
            continue;
        };
        let mut names: Vec<_> = entries.flatten().map(|e| e.path()).collect();
        names.sort();
        for dir in names {
            let Some(name) = dir.file_name().and_then(|n| n.to_str()).map(str::to_string) else {
                continue;
            };
            if seen.contains(&name) {
                continue;
            }
            let Read::Text(text) = read_confined(&dir.join("SKILL.md"), &walk.allowed) else {
                continue;
            };
            let Some(path) = confined(&dir.join("SKILL.md"), &walk.allowed) else {
                continue;
            };
            seen.insert(name.clone());
            out.push(Skill {
                name,
                description: frontmatter_description(&text),
                path,
            });
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// The `description:` value of a `SKILL.md` front matter block.
fn frontmatter_description(text: &str) -> String {
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some("---") {
        return String::new();
    }
    for line in lines {
        if line.trim() == "---" {
            break;
        }
        if let Some(value) = line.strip_prefix("description:") {
            let value = value.trim().trim_matches('"').trim_matches('\'');
            return value.chars().take(MAX_DESCRIPTION_CHARS).collect();
        }
    }
    String::new()
}

fn gather(cwd: &Path, home: Option<&Path>) -> Option<String> {
    let walk = plan(cwd, home)?;
    let mut body = String::from(
        "Workspace guidance gathered by the Vogt engine from the files Claude Code \
         would load for this working directory. Codex does not read these itself. \
         Each block names its source file.\n\n",
    );
    let preamble = body.len();
    let mut left_out = Vec::new();
    let mut seen = HashSet::new();
    for path in candidates(&walk) {
        let text = match read_confined(&path, &walk.allowed) {
            Read::Text(text) => text,
            Read::TooLarge => {
                left_out.push(path);
                continue;
            }
            Read::Refused => continue,
        };
        // A CLAUDE.md that is a symlink to the AGENTS.md beside it is one file.
        if let Some(real) = confined(&path, &walk.allowed) {
            if !seen.insert(real) {
                continue;
            }
        }
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        let block = format!(
            "<guidance source=\"{}\">\n{text}\n</guidance>\n\n",
            path.display()
        );
        // A block that does not fit is left out and named; later, smaller
        // blocks may still fit.
        if body.len() + block.len() > MAX_TOTAL_BYTES {
            left_out.push(path);
            continue;
        }
        body.push_str(&block);
    }
    let skills = skills(&walk);
    if !skills.is_empty() {
        let mut index = String::from(
            "<skills>\nSkills available in this workspace. When a task matches one, \
             read its SKILL.md first and follow it.\n",
        );
        for skill in &skills {
            index.push_str(&format!(
                "- {}: {} ({})\n",
                skill.name,
                skill.description,
                skill.path.display()
            ));
        }
        index.push_str("</skills>\n\n");
        if body.len() + index.len() <= MAX_TOTAL_BYTES {
            body.push_str(&index);
        }
    }
    if body.len() == preamble {
        return None;
    }
    if !left_out.is_empty() {
        body.push_str("Left out for size; read them directly if needed:\n");
        for path in left_out {
            body.push_str(&format!("- {}\n", path.display()));
        }
    }
    Some(body.trim_end().to_string())
}

/// `text` as a TOML basic string: quoted, with backslash, quote and every
/// control character escaped, so the fork's `-c key=value` parser reads back
/// exactly `text`.
pub fn toml_basic_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if c.is_control() => out.push_str(&format!("\\u{:04X}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn decode(arg: &str) -> String {
        let value = arg.strip_prefix("developer_instructions=").unwrap();
        let parsed: toml::Table = toml::from_str(&format!("v = {value}")).unwrap();
        parsed["v"].as_str().unwrap().to_string()
    }

    /// home/Working (no git) holds the workspace guidance; home/Working/Active/repo
    /// is the checkout the session runs in.
    fn layout() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let ws = home.join("Working");
        let repo = ws.join("Active").join("repo");
        fs::create_dir_all(repo.join(".git")).unwrap();
        write(
            &ws.join("AGENTS.md"),
            "WORKSPACE_AGENTS it's \"quoted\" \\ ok",
        );
        write(&ws.join("CLAUDE.md"), "WORKSPACE_CLAUDE");
        write(&ws.join(".claude/rules/operator.md"), "WORKSPACE_RULE");
        write(
            &ws.join(".claude/skills/komodo/SKILL.md"),
            "---\nname: komodo\ndescription: Drive Komodo deploys\n---\nbody",
        );
        write(&home.join(".claude/CLAUDE.md"), "USER_CLAUDE");
        write(&home.join(".claude/rules/user.md"), "USER_RULE");
        write(&repo.join("CLAUDE.md"), "REPO_CLAUDE");
        write(&repo.join("AGENTS.md"), "REPO_AGENTS");
        (tmp, home, repo)
    }

    #[test]
    fn guidance_above_the_repo_root_is_included() {
        let (_tmp, home, repo) = layout();
        let arg = developer_instructions_override(&repo, Some(&home)).unwrap();
        let body = decode(&arg);
        for marker in [
            "WORKSPACE_AGENTS it's \"quoted\" \\ ok",
            "WORKSPACE_CLAUDE",
            "WORKSPACE_RULE",
            "USER_CLAUDE",
            "USER_RULE",
            "REPO_CLAUDE",
            "- komodo: Drive Komodo deploys",
        ] {
            assert!(body.contains(marker), "{marker} missing from:\n{body}");
        }
        // Codex reads the checkout's own AGENTS.md natively.
        assert!(!body.contains("REPO_AGENTS"));
        // The workspace comes before the checkout, user-level before both.
        let at = |m: &str| body.find(m).unwrap();
        assert!(at("USER_CLAUDE") < at("WORKSPACE_CLAUDE"));
        assert!(at("WORKSPACE_CLAUDE") < at("REPO_CLAUDE"));
    }

    #[test]
    fn a_symlink_out_of_the_allowed_roots_is_refused() {
        let (tmp, home, repo) = layout();
        let outside = tmp.path().join("elsewhere");
        write(&outside.join("secret.md"), "OUTSIDE_SECRET");
        write(&home.join(".codex/auth.json"), "AUTH_TOKEN");
        write(&home.join(".codex/notes.md"), "CODEX_NOTES");
        let rules = repo.join(".claude/rules");
        fs::create_dir_all(&rules).unwrap();
        std::os::unix::fs::symlink(outside.join("secret.md"), rules.join("a.md")).unwrap();
        std::os::unix::fs::symlink(home.join(".codex/auth.json"), rules.join("b.md")).unwrap();
        std::os::unix::fs::symlink(home.join(".codex/notes.md"), rules.join("c.md")).unwrap();
        let skill = repo.join(".claude/skills/evil");
        fs::create_dir_all(&skill).unwrap();
        std::os::unix::fs::symlink(outside.join("secret.md"), skill.join("SKILL.md")).unwrap();
        let body = decode(&developer_instructions_override(&repo, Some(&home)).unwrap());
        assert!(!body.contains("OUTSIDE_SECRET"));
        assert!(!body.contains("AUTH_TOKEN"));
        assert!(!body.contains("CODEX_NOTES"));
        assert!(!body.contains("evil"));
        assert!(body.contains("WORKSPACE_RULE"));
    }

    #[test]
    fn outside_home_the_walk_stops_at_the_git_root() {
        let tmp = tempfile::tempdir().unwrap();
        let host = tmp.path().join("srv");
        let repo = host.join("repo");
        fs::create_dir_all(repo.join(".git")).unwrap();
        write(&host.join("CLAUDE.md"), "HOST_FILE");
        write(&repo.join("CLAUDE.md"), "REPO_CLAUDE");
        let home = tmp.path().join("home");
        fs::create_dir_all(&home).unwrap();
        let body = decode(&developer_instructions_override(&repo, Some(&home)).unwrap());
        assert!(body.contains("REPO_CLAUDE"));
        assert!(!body.contains("HOST_FILE"));
    }

    #[test]
    fn oversized_files_are_left_out_and_named() {
        let (_tmp, home, repo) = layout();
        let big = "x".repeat(MAX_FILE_BYTES as usize + 1);
        write(&repo.join(".claude/rules/big.md"), &big);
        let body = decode(&developer_instructions_override(&repo, Some(&home)).unwrap());
        assert!(!body.contains(&big));
        assert!(body.contains("Left out for size"));
        assert!(body.contains("big.md"));
        assert!(body.len() <= MAX_TOTAL_BYTES + 4096);
    }

    #[test]
    fn nothing_to_carry_gives_no_override() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join(".git")).unwrap();
        assert!(developer_instructions_override(&repo, None).is_none());
    }

    #[test]
    fn toml_escaping_round_trips_awkward_text() {
        let text = "it's ''' \"\"\" \\n \u{1}\ttab\r\nend";
        let parsed: toml::Table =
            toml::from_str(&format!("v = {}", toml_basic_string(text))).unwrap();
        assert_eq!(parsed["v"].as_str().unwrap(), text);
    }
}
