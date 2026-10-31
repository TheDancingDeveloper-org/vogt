//! Prompt artifacts: the files the engine writes so a spawned agent can read
//! its brief from disk.
//!
//! Two callers write them — a scheduled agent task's run (`agent_tasks`) and a
//! session created with a brief (`sessions`, on behalf of vogt-core's
//! work-item sessions). They share this module rather than each picking their
//! own directory, because the cleanup pass in `agent_tasks` sweeps one root
//! and only sees what is written under it.

use std::path::{Path, PathBuf};

use uuid::Uuid;

use crate::error::Result;

/// Root for every prompt artifact, relative to `state_dir`. Named for agent
/// tasks because they got here first; the name is on disk in deployed state
/// dirs, so it stays as it is rather than being renamed for tidiness.
pub const PROMPT_DIR: &str = "agent-task-prompts";

/// Session briefs live in one directory beside the per-task directories, one
/// file per session id. Not a uuid, so `agent_tasks` cleanup recognizes it and
/// does not mistake it for the directory of a task that no longer exists.
pub const SESSION_SUBDIR: &str = "sessions";

/// The variable naming the prompt file in the spawned child's environment.
///
/// Deliberately the same variable a scheduled task run already sets: an agent
/// started for a work item and an agent started by a schedule are configured
/// identically, so one startup wrapper reads one name and never has to ask
/// which half of the engine launched it.
pub const PROMPT_FILE_ENV: &str = "VOGT_ENGINE_AGENT_TASK_PROMPT_FILE";

pub fn prompt_root(state_dir: &Path) -> PathBuf {
    state_dir.join(PROMPT_DIR)
}

pub fn session_prompt_dir(state_dir: &Path) -> PathBuf {
    prompt_root(state_dir).join(SESSION_SUBDIR)
}

pub fn session_prompt_path(state_dir: &Path, session_id: Uuid) -> PathBuf {
    session_prompt_dir(state_dir).join(prompt_file_name(session_id))
}

/// `<uuid>.guide.md`, beside the brief. Both names map back to the session
/// (`session_id_from_artifact`), so the prompt sweep keeps the guide while the
/// session is known and removes it with the brief once the session is gone.
pub fn session_guide_name(session_id: Uuid) -> String {
    format!("{session_id}.guide.md")
}

pub fn session_guide_path(state_dir: &Path, session_id: Uuid) -> PathBuf {
    session_prompt_dir(state_dir).join(session_guide_name(session_id))
}

/// `<owner-id>.md` — the naming every prompt artifact uses, whether the owner
/// is a task run or a session.
pub fn prompt_file_name(owner_id: Uuid) -> String {
    format!("{owner_id}.md")
}

/// Write `body` as the prompt file for `owner_id` inside `dir`, creating `dir`
/// on the way. Returns the path to hand to the child.
pub fn write_prompt(dir: &Path, owner_id: Uuid, body: &str) -> Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(prompt_file_name(owner_id));
    std::fs::write(&path, body)?;
    Ok(path)
}

/// Write a session's brief. The file is created before the PTY is spawned so
/// a child that reads it on its first line cannot lose the race.
pub fn write_session_prompt(state_dir: &Path, session_id: Uuid, body: &str) -> Result<PathBuf> {
    write_prompt(&session_prompt_dir(state_dir), session_id, body)
}

/// Write the standing guide beside the brief. Same directory, so the
/// `--add-dir` the launch already passes covers it.
pub fn write_session_guide(state_dir: &Path, session_id: Uuid, body: &str) -> Result<PathBuf> {
    let dir = session_prompt_dir(state_dir);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(session_guide_name(session_id));
    std::fs::write(&path, body)?;
    Ok(path)
}

/// Forget a session's brief. Best effort: a prompt file that outlives its
/// session is collected by the agent-task artifact cleanup, so failing to
/// delete it here is a warning, never a failed request.
pub fn remove_session_prompt(state_dir: &Path, session_id: Uuid) {
    for path in [
        session_prompt_path(state_dir, session_id),
        session_guide_path(state_dir, session_id),
        crate::mcp_set::claude_config_path(state_dir, session_id),
    ] {
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                tracing::warn!(
                    session = %session_id,
                    path = %path.display(),
                    error = %e,
                    "failed to remove session prompt file"
                );
            }
        }
    }
}

/// The session id a prompt file belongs to, if the name is one this module
/// wrote. Anything else under the sessions directory is litter.
pub fn session_id_from_prompt_file(file_name: &str) -> Option<Uuid> {
    file_name
        .strip_suffix(".md")
        .and_then(|stem| Uuid::parse_str(stem).ok())
}

/// The session a sessions-directory entry belongs to: a brief (`<uuid>.md`) or
/// its companion guide (`<uuid>.guide.md`). The sweep keeps either while the
/// session is known and removes both once it is forgotten.
pub fn session_id_from_artifact(file_name: &str) -> Option<Uuid> {
    let stem = file_name
        .strip_suffix(".guide.md")
        .or_else(|| file_name.strip_suffix(".mcp.json"))
        .or_else(|| file_name.strip_suffix(".md"))?;
    Uuid::parse_str(stem).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_prompts_sit_under_the_shared_prompt_root() {
        let id = Uuid::nil();
        let path = session_prompt_path(Path::new("/state"), id);
        assert_eq!(
            path,
            Path::new("/state/agent-task-prompts/sessions/00000000-0000-0000-0000-000000000000.md")
        );
    }

    #[test]
    fn only_prompt_file_names_map_back_to_a_session() {
        let id = Uuid::new_v4();
        assert_eq!(session_id_from_prompt_file(&prompt_file_name(id)), Some(id));
        assert!(session_id_from_prompt_file("context.md").is_none());
        assert!(session_id_from_prompt_file(&id.to_string()).is_none());
        // The guide is not a brief, so the brief-only mapping leaves it alone.
        assert!(session_id_from_prompt_file(&session_guide_name(id)).is_none());
    }

    #[test]
    fn a_guide_maps_back_to_its_session_like_its_brief() {
        let id = Uuid::new_v4();
        assert_eq!(session_id_from_artifact(&prompt_file_name(id)), Some(id));
        assert_eq!(session_id_from_artifact(&session_guide_name(id)), Some(id));
        assert!(session_id_from_artifact("context.md").is_none());
        assert!(session_id_from_artifact(&format!("{id}.hooks")).is_none());
    }
}
