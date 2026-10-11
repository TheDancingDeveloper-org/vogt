//! The hand-off file a Klaudia session leaves behind when it stops (C12 of
//! `docs/design/context-management.md`).
//!
//! `<state_dir>/handoffs/<engine-session-uuid>.md` holds the newest
//! `~/.klaudia/sessions/<cwd-key>/<conversation-id>.summary.md` Klaudia wrote
//! for the conversation, then the git facts of the session's own directory.
//! That directory is the checkout the session was opened in — in this
//! deployment the project's main checkout, not a worker's worktree — so the
//! section says so. A conversation with no summary still gets the git facts,
//! and a
//! directory that is not a git checkout gets the summary alone. Either half
//! missing is fine; the file is a convenience for the next session, not a
//! record the engine depends on, so a failure to write it is logged and the
//! hibernation or exit continues.
//!
//! Claude's hand-off is a hook (`PreCompact` / `SessionEnd`), not this path.

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
};

use uuid::Uuid;

use crate::hibernation::Record;

/// How long a summary is allowed to be before it is cut. A hand-off is read
/// into another session's brief; an unbounded transcript summary would be the
/// thing it exists to avoid.
const SUMMARY_CAP: usize = 64 * 1024;

/// Porcelain lines kept. The session directory is the main checkout, whose
/// status can be the whole working tree, not the worker's own change.
const STATUS_LINES: usize = 40;

/// Write `state_dir/handoffs/<id>.md` for a Klaudia session, best-effort.
/// Anything else — no conversation, a different agent — writes nothing.
pub fn write(state_dir: &Path, record: &Record, home: Option<&Path>) {
    let Some(conversation) = record.conversation.as_ref() else {
        return;
    };
    if conversation.agent != "klaudia" {
        return;
    }
    let summary = home
        .and_then(|home| newest_summary(&home.join(".klaudia").join("sessions"), &conversation.id));
    let git = record.cwd.as_deref().and_then(git_facts);
    let body = render(
        &conversation.id,
        record.cwd.as_deref(),
        summary.as_deref(),
        git.as_deref(),
    );
    let dir = state_dir.join("handoffs");
    if let Err(e) = fs::create_dir_all(&dir) {
        tracing::warn!(session = %record.id, error = %e, "could not create the handoff directory");
        return;
    }
    let path = dir.join(format!("{}.md", record.id));
    if let Err(e) = write_atomic(&path, body.as_bytes()) {
        tracing::warn!(session = %record.id, error = %e, "could not write the handoff file");
    }
}

/// The same write, from the pieces the exit hook still has after the record
/// is about to be forgotten.
pub fn write_for(
    state_dir: &Path,
    id: Uuid,
    agent: &str,
    conversation_id: &str,
    cwd: &str,
    home: Option<&Path>,
) {
    write(
        state_dir,
        &Record {
            id,
            cwd: Some(cwd.to_string()),
            conversation: Some(vogt_engine_contract::AgentConversation {
                agent: agent.to_string(),
                id: conversation_id.to_string(),
            }),
            ..Record::new(id, String::new(), String::new())
        },
        home,
    );
}

fn render(
    conversation_id: &str,
    cwd: Option<&str>,
    summary: Option<&str>,
    git: Option<&str>,
) -> String {
    let mut out = format!("# Handoff\n\nconversation: {conversation_id}\n");
    if let Some(cwd) = cwd {
        out.push_str(&format!("cwd: {cwd}\n"));
    }
    out.push_str("\n## Summary\n\n");
    match summary.map(str::trim).filter(|s| !s.is_empty()) {
        Some(summary) => out.push_str(summary),
        None => out.push_str("(no summary)"),
    }
    out.push_str("\n\n## Git (session directory)\n\n");
    match git.map(str::trim).filter(|s| !s.is_empty()) {
        Some(git) => out.push_str(git),
        None => out.push_str("(not a git checkout)"),
    }
    out.push('\n');
    out
}

/// `<root>/<cwd-key>/<id>.summary.md`, the newest when more than one cwd key
/// holds the conversation. Klaudia files Claude Code's layout (WI-950).
fn newest_summary(root: &Path, id: &str) -> Option<String> {
    let name = format!("{id}.summary.md");
    let mut found: Vec<PathBuf> = fs::read_dir(root)
        .ok()?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path().join(&name))
        .filter(|candidate| candidate.is_file())
        .collect();
    found.sort_by_key(|path| fs::metadata(path).and_then(|m| m.modified()).ok());
    let path = found.pop()?;
    let text = fs::read_to_string(&path).ok()?;
    Some(cap(&text))
}

fn cap(text: &str) -> String {
    if text.len() <= SUMMARY_CAP {
        return text.to_string();
    }
    let mut end = SUMMARY_CAP;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n\n[summary truncated]\n", &text[..end])
}

/// `branch`, `HEAD` and `status --porcelain` of `cwd`, or `None` when it is
/// not a checkout. Blocking, and only the three fast read-only calls.
///
/// Every call passes `--no-optional-locks`: `status` otherwise takes
/// `index.lock`, and the session directory is one a worker may be committing
/// in at the same moment.
fn git_facts(cwd: &str) -> Option<String> {
    let capture = |args: &[&str]| -> Option<String> {
        let out = Command::new("git")
            .arg("--no-optional-locks")
            .arg("-C")
            .arg(cwd)
            .args(args)
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (!text.is_empty()).then_some(text)
    };
    if capture(&["rev-parse", "--is-inside-work-tree"]).as_deref() != Some("true") {
        return None;
    }
    let mut lines = Vec::new();
    if let Some(branch) = capture(&["rev-parse", "--abbrev-ref", "HEAD"]) {
        lines.push(format!("branch: {branch}"));
    }
    if let Some(sha) = capture(&["rev-parse", "HEAD"]) {
        lines.push(format!("head: {sha}"));
    }
    let status = capture(&["status", "--porcelain"]).unwrap_or_default();
    if status.is_empty() {
        lines.push("status: clean".to_string());
    } else {
        let status_lines: Vec<&str> = status.lines().collect();
        lines.push("status:".to_string());
        let shown = status_lines.len().min(STATUS_LINES);
        lines.push(status_lines[..shown].join("\n"));
        if status_lines.len() > shown {
            lines.push(format!("… and {} more", status_lines.len() - shown));
        }
    }
    Some(lines.join("\n"))
}

/// Replace `path` by writing a sibling and renaming it, so a reader never
/// sees a half-written hand-off.
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("md.tmp");
    let mut file = fs::File::create(&tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(agent: &str, cwd: Option<&str>) -> Record {
        let id = Uuid::nil();
        Record {
            cwd: cwd.map(str::to_string),
            conversation: Some(vogt_engine_contract::AgentConversation {
                agent: agent.to_string(),
                id: "conv-1".to_string(),
            }),
            ..Record::new(id, "s".to_string(), "t".to_string())
        }
    }

    #[test]
    fn a_klaudia_session_gets_its_newest_summary_and_git_facts() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let older = home.join(".klaudia").join("sessions").join("old-key");
        let newer = home.join(".klaudia").join("sessions").join("new-key");
        fs::create_dir_all(&older).unwrap();
        fs::create_dir_all(&newer).unwrap();
        fs::write(older.join("conv-1.summary.md"), "the old summary").unwrap();
        let newest = newer.join("conv-1.summary.md");
        fs::write(&newest, "the new summary").unwrap();
        // The newer key must sort last by mtime, whatever the filesystem's
        // timestamp resolution is.
        let later =
            fs::metadata(&newest).unwrap().modified().unwrap() + std::time::Duration::from_secs(2);
        fs::File::options()
            .write(true)
            .open(&newest)
            .unwrap()
            .set_modified(later)
            .unwrap();

        let repo = root.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        assert!(Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["init", "-q"])
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["config", "user.email", "t@example.com"])
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["config", "user.name", "t"])
            .status()
            .unwrap()
            .success());
        fs::write(repo.join("f"), "x").unwrap();
        assert!(Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["add", "f"])
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["commit", "-q", "-m", "one"])
            .status()
            .unwrap()
            .success());

        let state = root.path().join("state");
        write(
            &state,
            &record("klaudia", Some(repo.to_str().unwrap())),
            Some(&home),
        );

        let written =
            fs::read_to_string(state.join("handoffs").join(format!("{}.md", Uuid::nil()))).unwrap();
        assert!(written.contains("the new summary"), "{written}");
        assert!(!written.contains("the old summary"), "{written}");
        assert!(written.contains("branch:"), "{written}");
        assert!(written.contains("head:"), "{written}");
        assert!(written.contains("## Git (session directory)"), "{written}");
        assert!(written.contains("status: clean"), "{written}");
    }

    #[test]
    fn a_long_status_keeps_forty_lines() {
        let text = (0..45)
            .map(|n| format!("?? f{n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let lines: Vec<&str> = text.lines().collect();
        let shown = lines.len().min(STATUS_LINES);
        let mut out = lines[..shown].join("\n");
        if lines.len() > shown {
            out.push_str(&format!("\n… and {} more", lines.len() - shown));
        }
        assert!(out.contains("?? f39"));
        assert!(!out.contains("?? f40"));
        assert!(out.ends_with("… and 5 more"), "{out}");
    }

    #[test]
    fn a_missing_summary_still_records_the_git_facts() {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("state");
        write(
            &state,
            &record("klaudia", Some(root.path().to_str().unwrap())),
            None,
        );
        let written =
            fs::read_to_string(state.join("handoffs").join(format!("{}.md", Uuid::nil()))).unwrap();
        assert!(written.contains("(no summary)"), "{written}");
        assert!(written.contains("(not a git checkout)"), "{written}");
    }

    #[test]
    fn another_agent_writes_nothing() {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("state");
        write(&state, &record("claude", Some("/tmp")), None);
        assert!(!state.join("handoffs").exists());
    }
}
