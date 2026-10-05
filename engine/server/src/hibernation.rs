//! What the engine keeps about a session so it can stop its processes and
//! start it again later: the hibernation record (WI-912).
//!
//! An idle agent session holds a few hundred MiB in its CLI and MCP servers,
//! while everything needed to continue it — the conversation — is already on
//! disk in the agent CLI's own transcript. A record here is the rest: the
//! command, directory, environment and conversation id to resume, plus the
//! last screen to show while nothing is running.
//!
//! Three rules:
//!
//! **Written ahead, at spawn.** A session that can be hibernated has a record
//! from the moment it starts, so an engine that is SIGKILLed — a redeploy
//! whose grace ran out, a crash — still leaves behind the list of sessions it
//! was running. At boot every record without a process is a hibernated
//! session (`trigger: recovered`); nothing has to be reconstructed by hand
//! from transcripts.
//!
//! **No secret is ever written.** Variables `pty::is_secret_env` matches
//! (`VOGT_HTTP_TOKEN`, broker tokens, anything named like a token, secret,
//! password or API key) are dropped before the record is written. A woken
//! session gets a fresh broker grant from the engine and, through
//! `POST /api/sessions/{id}/wake`'s `env`, whatever credential its caller
//! mints for it.
//!
//! **Files, one per session, replaced atomically.** `state_dir/sessions/
//! <uuid>.json` (mode 0600) and, while hibernated, `<uuid>.screen` holding
//! the raw tail of its output. A torn write leaves the previous version.

use std::{
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};

use serde::{Deserialize, Serialize};
use uuid::Uuid;
use vogt_engine_contract::{AgentConversation, Hibernation};

/// How much of a session's output is kept to show while it is hibernated.
/// Enough to render the screen and a few hundred lines above it.
pub const SCREEN_BYTES: usize = 256 * 1024;

/// How long a hibernating agent is given to exit on `SIGTERM` — to flush its
/// transcript — before its process group is killed.
pub const STOP_GRACE: Duration = Duration::from_secs(5);

const RECORD_VERSION: u32 = 1;

/// Everything needed to start a session again, and, while it is hibernated,
/// when and why it stopped.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    pub version: u32,
    pub id: Uuid,
    pub name: String,
    /// RFC 3339; when the session was first created. Kept across wakes.
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
    /// The command as the template expanded it, before the engine added
    /// model, resume, session-id or brief arguments: a wake adds its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// The template's and caller's environment, secrets removed.
    #[serde(default)]
    pub env: Vec<(String, String)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// The permission posture it was started with, kept so a wake does not
    /// quietly change it (WI-926).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<String>,
    /// What a wake resumes. `None` only for a shell hibernated on request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation: Option<AgentConversation>,
    /// The brief the engine wrote at the first start, if any. A wake points
    /// the session at it again (the variable) but never re-sends it as a
    /// first prompt: the resumed conversation has already read it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brief_file: Option<PathBuf>,
    #[serde(default)]
    pub keep_awake: bool,
    /// Set while hibernated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hibernation: Option<Hibernation>,
    /// The terminal size at hibernation, to render the kept screen with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rows: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cols: Option<u16>,
}

impl Record {
    pub fn new(id: Uuid, name: String, created_at: String) -> Self {
        Self {
            version: RECORD_VERSION,
            id,
            name,
            created_at,
            template: None,
            command: None,
            cwd: None,
            env: Vec::new(),
            model: None,
            effort: None,
            permission_mode: None,
            conversation: None,
            brief_file: None,
            keep_awake: false,
            hibernation: None,
            rows: None,
            cols: None,
        }
    }
}

/// The environment worth keeping: everything but credentials.
pub fn without_secrets(env: &[(String, String)]) -> Vec<(String, String)> {
    env.iter()
        .filter(|(k, _)| !crate::pty::is_secret_env(k))
        .cloned()
        .collect()
}

/// Where the records live.
pub fn dir(state_dir: &Path) -> PathBuf {
    state_dir.join("sessions")
}

// Path safety: every file name here is built from a parsed `Uuid`, whose
// textual form is 36 characters of hex digits and dashes — no separator, no
// `..` — joined onto the engine's own configured `state_dir`. CodeQL's
// `rust/path-injection` reads the route's `{id}` as a user-provided path and
// cannot model the `Uuid` parse as the barrier it is (see the same note in
// `workspace_path.rs`).
fn record_path(state_dir: &Path, id: Uuid) -> PathBuf {
    dir(state_dir).join(format!("{id}.json"))
}

fn screen_path(state_dir: &Path, id: Uuid) -> PathBuf {
    dir(state_dir).join(format!("{id}.screen"))
}

/// Replace `path` with `bytes`: written to a sibling temp file (0600) and
/// renamed over it, so a reader sees the old or the new, never a torn one.
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let tmp = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        Uuid::new_v4()
    ));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| {
        let mut file = options.open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

pub fn write(state_dir: &Path, record: &Record) -> std::io::Result<()> {
    let json = serde_json::to_vec_pretty(record).map_err(std::io::Error::other)?;
    write_atomic(&record_path(state_dir, record.id), &json)
}

pub fn write_screen(state_dir: &Path, id: Uuid, bytes: &[u8]) -> std::io::Result<()> {
    write_atomic(&screen_path(state_dir, id), bytes)
}

/// The kept output of a hibernated session; empty when there is none.
pub fn read_screen(state_dir: &Path, id: Uuid) -> Vec<u8> {
    std::fs::read(screen_path(state_dir, id)).unwrap_or_default()
}

pub fn remove_screen(state_dir: &Path, id: Uuid) {
    let _ = std::fs::remove_file(screen_path(state_dir, id));
}

/// Forget a session: its record and its kept screen.
pub fn remove(state_dir: &Path, id: Uuid) {
    let _ = std::fs::remove_file(record_path(state_dir, id));
    remove_screen(state_dir, id);
}

/// Every readable record. One that cannot be parsed is logged and left in
/// place for a person to look at, never deleted.
pub fn load_all(state_dir: &Path) -> Vec<Record> {
    let Ok(entries) = std::fs::read_dir(dir(state_dir)) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let parsed = std::fs::read(&path)
            .map_err(|e| e.to_string())
            .and_then(|bytes| serde_json::from_slice::<Record>(&bytes).map_err(|e| e.to_string()));
        match parsed {
            Ok(record)
                if path.file_stem().and_then(|s| s.to_str())
                    == Some(record.id.to_string().as_str()) =>
            {
                out.push(record)
            }
            Ok(_) => {
                tracing::warn!(path = %path.display(), "session record names another id; ignored")
            }
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "unreadable session record; ignored")
            }
        }
    }
    out
}

/// Every process below `root` (not `root` itself), found by walking
/// `/proc/*/stat` parent links. Taken *before* anything is signalled: once a
/// parent dies its children are re-parented and the tree can no longer be
/// read. Empty off Linux.
pub fn descendants(root: u32) -> Vec<u32> {
    #[cfg(target_os = "linux")]
    {
        let mut parent_of: Vec<(u32, u32)> = Vec::new();
        if let Ok(entries) = std::fs::read_dir("/proc") {
            for entry in entries.flatten() {
                let Some(pid) = entry
                    .file_name()
                    .to_str()
                    .and_then(|s| s.parse::<u32>().ok())
                else {
                    continue;
                };
                if let Some(ppid) = parent_pid(pid) {
                    parent_of.push((pid, ppid));
                }
            }
        }
        let mut found = Vec::new();
        let mut frontier = vec![root];
        while let Some(parent) = frontier.pop() {
            for &(pid, ppid) in &parent_of {
                if ppid == parent && pid != root && !found.contains(&pid) {
                    found.push(pid);
                    frontier.push(pid);
                }
            }
        }
        found
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = root;
        Vec::new()
    }
}

/// The parent of `pid` from `/proc/<pid>/stat`, whose second field (the
/// command name, in parentheses) may itself contain spaces and parentheses —
/// so the fields are read after the *last* `)`.
#[cfg(target_os = "linux")]
fn parent_pid(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = &stat[stat.rfind(')')? + 1..];
    let mut fields = rest.split_whitespace();
    let _state = fields.next()?;
    fields.next()?.parse().ok()
}

/// The command name of `pid` (`/proc/<pid>/comm`), if it still exists.
pub fn process_name(pid: u32) -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string(format!("/proc/{pid}/comm"))
            .ok()
            .map(|s| s.trim().to_string())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        None
    }
}

fn signal(pid: i32, sig: i32) {
    #[cfg(unix)]
    unsafe {
        // ESRCH (already gone) is the expected outcome for most of these.
        let _ = libc::kill(pid, sig);
    }
}

/// Stop a session's whole process tree: `SIGTERM` to the child's process
/// group (the PTY child is a session leader, so its group is its pid) and to
/// every descendant found beforehand — a CLI's MCP servers and tool shells
/// may sit in groups of their own — then, after `grace` or as soon as
/// `exited()` says the child is gone and nothing else is left, `SIGKILL` to
/// whatever remains.
///
/// The plain kill route signals the child pid alone, which leaves MCP
/// servers running orphaned; hibernation exists to free that memory, so it
/// takes the tree.
pub async fn stop_tree(pid: u32, grace: Duration, exited: impl Fn() -> bool) {
    let tree = descendants(pid);
    let group = -(pid as i32);
    #[cfg(unix)]
    {
        signal(group, libc::SIGTERM);
        for &p in &tree {
            signal(p as i32, libc::SIGTERM);
        }
    }
    let deadline = tokio::time::Instant::now() + grace;
    while tokio::time::Instant::now() < deadline {
        if exited() && tree.iter().all(|&p| !alive(p)) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    #[cfg(unix)]
    {
        signal(group, libc::SIGKILL);
        signal(pid as i32, libc::SIGKILL);
        for &p in &tree {
            signal(p as i32, libc::SIGKILL);
        }
    }
}

fn alive(pid: u32) -> bool {
    #[cfg(unix)]
    unsafe {
        libc::kill(pid as i32, 0) == 0
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_record_round_trips_and_never_holds_a_secret() {
        let dir = tempfile::tempdir().unwrap();
        let id = Uuid::new_v4();
        let mut record = Record::new(id, "agent".into(), "2026-10-05T00:00:00Z".into());
        record.env = without_secrets(&[
            ("VOGT_HTTP_TOKEN".into(), "sekrit".into()),
            ("VOGT_SESSION_ID".into(), "ses_1".into()),
            ("OPENAI_API_KEY".into(), "sk".into()),
        ]);
        record.conversation = Some(AgentConversation {
            agent: "claude".into(),
            id: id.to_string(),
        });
        write(dir.path(), &record).unwrap();
        let text = std::fs::read_to_string(dir.path().join("sessions").join(format!("{id}.json")))
            .unwrap();
        assert!(
            !text.contains("sekrit") && !text.contains("\"sk\""),
            "{text}"
        );
        let loaded = load_all(dir.path());
        assert_eq!(loaded.len(), 1);
        assert_eq!(
            loaded[0].env,
            vec![("VOGT_SESSION_ID".to_string(), "ses_1".to_string())]
        );
        assert_eq!(loaded[0].conversation, record.conversation);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.path().join("sessions").join(format!("{id}.json")))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }

        write_screen(dir.path(), id, b"last screen").unwrap();
        assert_eq!(read_screen(dir.path(), id), b"last screen");
        remove(dir.path(), id);
        assert!(load_all(dir.path()).is_empty());
        assert!(read_screen(dir.path(), id).is_empty());
    }

    #[test]
    fn a_record_filed_under_another_id_or_unparseable_is_ignored_not_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let sessions = dir.path().join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        let bad = sessions.join(format!("{}.json", Uuid::new_v4()));
        std::fs::write(&bad, b"{not json").unwrap();
        let other = Record::new(Uuid::new_v4(), "x".into(), String::new());
        let misfiled = sessions.join(format!("{}.json", Uuid::new_v4()));
        std::fs::write(&misfiled, serde_json::to_vec(&other).unwrap()).unwrap();
        assert!(load_all(dir.path()).is_empty());
        assert!(bad.exists() && misfiled.exists());
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn stop_tree_takes_children_in_their_own_groups() {
        use std::os::unix::process::CommandExt;
        // A parent in its own session that starts a child in yet another
        // process group, standing in for an agent CLI and an MCP server.
        let mut parent = std::process::Command::new("/bin/sh");
        parent
            .arg("-c")
            .arg("setsid sleep 300 & echo $! ; wait")
            .stdout(std::process::Stdio::piped());
        unsafe {
            parent.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        let mut parent = parent.spawn().unwrap();
        let pid = parent.id();
        let mut line = String::new();
        {
            use std::io::BufRead;
            let stdout = parent.stdout.take().unwrap();
            std::io::BufReader::new(stdout)
                .read_line(&mut line)
                .unwrap();
        }
        let grandchild: u32 = line.trim().parse().unwrap();
        // setsid(1) may fork once more; take whichever `sleep` is below.
        tokio::time::sleep(Duration::from_millis(100)).await;
        let below = descendants(pid);
        assert!(
            !below.is_empty(),
            "the child should be found below the parent"
        );

        let reaper = std::thread::spawn(move || parent.wait());
        stop_tree(pid, Duration::from_millis(500), || false).await;
        reaper.join().unwrap().unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        for p in below.iter().chain(std::iter::once(&grandchild)) {
            // A zombie still answers kill(0); the name is gone with the
            // process, or it is a zombie awaiting init.
            let state = std::fs::read_to_string(format!("/proc/{p}/stat")).unwrap_or_default();
            let gone = state.is_empty() || state[state.rfind(')').unwrap() + 2..].starts_with('Z');
            assert!(gone, "process {p} survived: {state}");
        }
    }
}
