//! Where an agent CLI's own conversation was running, read from its
//! transcript.
//!
//! Claude Code and Codex key a conversation to the directory it ran in, and
//! `claude --resume <id>` only finds a conversation when started in that same
//! directory. A conversation started under `~/Working` or in a worktree is
//! therefore not resumable from the registered project root a session
//! normally opens in (WI-871). The transcript records the directory itself:
//! every Claude Code entry carries `cwd`, and a Codex rollout opens with a
//! `session_meta` entry that does. This module finds the transcript for a
//! conversation id and reads that directory back, so a resumed session can
//! start where its conversation lives.
//!
//! Bounded on purpose: only the head of one file is read, the directory
//! walks are capped, and nothing here is trusted beyond being a path the
//! caller validates against the workspace root before using it.

use std::{
    fs::File,
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
};

/// How much of a transcript's head is read looking for a `cwd`. The first
/// few entries carry it; a resumed conversation's head is the same file.
const HEAD_BYTES: u64 = 256 * 1024;
/// How many head lines are parsed at most.
const HEAD_LINES: usize = 64;
/// A guard against a transcript root pointed at the wrong directory.
const MAX_ENTRIES: usize = 20_000;

/// The directory the agent CLI's conversation `id` ran in, when its
/// transcript can be found under `home` and records one.
///
/// `agent` is the CLI's binary name (`claude`, `codex`); anything else, or a
/// conversation the CLI keeps no readable transcript for, is `None`.
pub fn conversation_cwd(agent: &str, id: &str, home: &Path) -> Option<PathBuf> {
    let transcript = match agent {
        "claude" => claude_transcript(&home.join(".claude").join("projects"), id),
        "codex" => codex_transcript(&home.join(".codex").join("sessions"), id),
        // Klaudia files Claude Code's layout under its own root (WI-950).
        "klaudia" => claude_transcript(&home.join(".klaudia").join("sessions"), id),
        _ => None,
    }?;
    head_cwd(&transcript)
}

/// `<root>/<cwd-key>/<id>.jsonl`, one directory level down.
fn claude_transcript(root: &Path, id: &str) -> Option<PathBuf> {
    let file_name = format!("{id}.jsonl");
    std::fs::read_dir(root)
        .ok()?
        .take(MAX_ENTRIES)
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path().join(&file_name))
        .find(|candidate| candidate.is_file())
}

/// `<root>/YYYY/MM/DD/rollout-<timestamp>-<id>.jsonl`, at most four levels
/// down, newest dates first.
fn codex_transcript(root: &Path, id: &str) -> Option<PathBuf> {
    let suffix = format!("-{id}.jsonl");
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    let mut seen = 0usize;
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut children: Vec<PathBuf> = entries.filter_map(|e| e.ok()).map(|e| e.path()).collect();
        children.sort();
        for path in children {
            seen += 1;
            if seen > MAX_ENTRIES {
                return None;
            }
            if path.is_dir() {
                if depth < 4 {
                    // Sorted ascending and popped from the end, so the
                    // newest date is searched first.
                    stack.push((path, depth + 1));
                }
            } else if path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(&suffix))
            {
                return Some(path);
            }
        }
    }
    None
}

/// The first `cwd` recorded in the head of a transcript, from either agent's
/// shape: a top-level `cwd` (Claude Code) or `payload.cwd` (Codex).
fn head_cwd(path: &Path) -> Option<PathBuf> {
    let file = File::open(path).ok()?;
    let reader = BufReader::new(file.take(HEAD_BYTES));
    for line in reader.lines().take(HEAD_LINES) {
        let Ok(line) = line else {
            break;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        let cwd = value
            .get("cwd")
            .or_else(|| value.get("payload").and_then(|p| p.get("cwd")))
            .and_then(|c| c.as_str())
            .filter(|c| !c.is_empty());
        if let Some(cwd) = cwd {
            return Some(PathBuf::from(cwd));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_a_claude_conversations_directory() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join(".claude/projects/-srv-work");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("abc-123.jsonl"),
            "{\"type\":\"summary\"}\n{\"type\":\"user\",\"cwd\":\"/srv/work\",\"sessionId\":\"abc-123\"}\n",
        )
        .unwrap();
        assert_eq!(
            conversation_cwd("claude", "abc-123", home.path()),
            Some(PathBuf::from("/srv/work"))
        );
        assert_eq!(conversation_cwd("claude", "missing", home.path()), None);
    }

    #[test]
    fn finds_a_codex_rollouts_directory() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join(".codex/sessions/2026/10/04");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("rollout-2026-10-04T10-00-00-0199aaaa-bbbb.jsonl"),
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"0199aaaa-bbbb\",\"cwd\":\"/srv/codex\"}}\n",
        )
        .unwrap();
        assert_eq!(
            conversation_cwd("codex", "0199aaaa-bbbb", home.path()),
            Some(PathBuf::from("/srv/codex"))
        );
    }

    #[test]
    fn finds_a_klaudia_conversations_directory() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join(".klaudia/sessions/-srv-klaudia");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("72d33a6b.jsonl"),
            "{\"type\":\"user\",\"cwd\":\"/srv/klaudia\",\"sessionId\":\"72d33a6b\"}\n",
        )
        .unwrap();
        assert_eq!(
            conversation_cwd("klaudia", "72d33a6b", home.path()),
            Some(PathBuf::from("/srv/klaudia"))
        );
        // Not under Claude Code's root: the agent decides where to look.
        assert_eq!(conversation_cwd("claude", "72d33a6b", home.path()), None);
    }

    #[test]
    fn an_unknown_agent_or_a_transcript_without_cwd_is_none() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join(".claude/projects/x");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("id1.jsonl"), "{\"type\":\"summary\"}\nnot json\n").unwrap();
        assert_eq!(conversation_cwd("claude", "id1", home.path()), None);
        assert_eq!(conversation_cwd("opencode", "id1", home.path()), None);
    }
}
