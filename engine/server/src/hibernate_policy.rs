//! When the engine hibernates a session by itself (WI-912).
//!
//! Off unless a deployment asks: `ENGINE_HIBERNATE_IDLE_AFTER` (a duration,
//! `2h`) hibernates an agent session that has been quiet that long, and
//! `ENGINE_HIBERNATE_MEMAVAILABLE_BELOW` (a size, `2GiB`) hibernates the
//! quietest eligible session, one per tick, while the memory available to the
//! pod is below it. Both keep every exemption, and each exemption is a reason
//! a person can read — the same sentence the watcher logs.
//!
//! What is never hibernated by policy:
//!
//! - a session that cannot be at all (no conversation to resume, an
//!   agent-task run, exited) — `SessionRegistry::hibernate_refusal`;
//! - one pinned with `keep_awake`;
//! - one whose turn is running, or that shows a permission dialog (Klaudia's
//!   own, which the engine reads from its title, included);
//! - one whose agent reported itself blocked on a person;
//! - by the idle trigger only, one on autopilot (WI-949): it is meant to be
//!   working through a backlog unattended, and the pause at the end of each
//!   of its turns is exactly the quiet the idle trigger looks for. Memory
//!   pressure can still take it — the one valve left when every session is
//!   busy;
//! - one with no agent CLI running in it at all: a shell whose typed-in
//!   agent reported its conversation and then died without unlinking it;
//! - one with a shell running below its agent CLI: a tool call in progress
//!   or a background shell, which a hibernation would kill mid-work. The
//!   agent's MCP servers are not shells, and the wrapper shell that launched
//!   the CLI is above it, not below.
//!
//! At boot, sessions pinned awake that were hibernated (by the shutdown, or
//! recovered) are woken — through vogt-core when there is one, so a linked
//! session gets a fresh token; the engine never stores one. A woken oversight
//! session is then told, once it is at its prompt, that it was resumed after
//! a restart (WI-962): a resumed agent otherwise sits idle, and the overseer
//! is the session that brings the others back.

use std::{sync::Arc, time::Duration};

use uuid::Uuid;
use vogt_engine_contract::{ActivityState, HibernateTrigger, SessionRole, WaitUntil, WakeRequest};

use crate::{app::AppState, hibernation, pty::Session, sessions::SessionRegistry};

/// The deployment's hibernation policy. The default is off.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Policy {
    /// Hibernate an eligible session quiet for at least this long.
    pub idle_after: Option<Duration>,
    /// Hibernate the quietest eligible session while the memory available to
    /// the pod (its cgroup limit minus usage, or else the host's
    /// `MemAvailable`) is below this many bytes.
    pub memavailable_below: Option<u64>,
}

impl Policy {
    pub fn is_off(&self) -> bool {
        self.idle_after.is_none() && self.memavailable_below.is_none()
    }
}

/// How often the watcher looks.
const TICK: Duration = Duration::from_secs(60);

/// Agent CLIs, as `/proc/<pid>/comm` names them.
const AGENTS: &[&str] = &["claude", "codex", "opencode", "klaudia"];

/// A shell below the agent is a tool call or a background job at work.
const SHELLS: &[&str] = &["bash", "sh", "zsh", "dash", "fish", "ksh"];

/// Parse `90s`, `30m`, `2h`, `1d` or a bare number of seconds.
pub fn parse_duration(raw: &str) -> Option<Duration> {
    let raw = raw.trim();
    let (digits, unit) = raw.split_at(raw.find(|c: char| !c.is_ascii_digit()).unwrap_or(raw.len()));
    let n: u64 = digits.parse().ok()?;
    let secs = match unit.trim() {
        "" | "s" => n,
        "m" => n.checked_mul(60)?,
        "h" => n.checked_mul(3600)?,
        "d" => n.checked_mul(86_400)?,
        _ => return None,
    };
    (secs > 0).then(|| Duration::from_secs(secs))
}

/// Parse `512MiB`, `2GiB`, `2G`, `1500M` or a bare number of bytes.
pub fn parse_size(raw: &str) -> Option<u64> {
    let raw = raw.trim();
    let (digits, unit) = raw.split_at(raw.find(|c: char| !c.is_ascii_digit()).unwrap_or(raw.len()));
    let n: u64 = digits.parse().ok()?;
    let factor: u64 = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1,
        "k" | "kb" | "kib" => 1 << 10,
        "m" | "mb" | "mib" => 1 << 20,
        "g" | "gb" | "gib" => 1 << 30,
        _ => return None,
    };
    n.checked_mul(factor).filter(|b| *b > 0)
}

/// Why policy must leave this live session alone, or `None` when it may
/// hibernate it.
pub fn exemption(registry: &SessionRegistry, session: &Session) -> Option<String> {
    if let Some(refusal) = registry.hibernate_refusal(session, false) {
        return Some(refusal);
    }
    if session.keep_awake() {
        return Some("pinned awake".into());
    }
    match session.activity() {
        ActivityState::Running => return Some("a turn is running".into()),
        ActivityState::AwaitingApproval => return Some("a permission dialog is open".into()),
        _ => {}
    }
    // Klaudia's permission asks, questions and plan approvals are not dialogs
    // the engine recognises, and a quiet Klaudia waiting on one reads `idle`.
    // Its title says so (WI-1090).
    if crate::screen::klaudia_title(session.title().as_deref())
        == Some(crate::screen::KlaudiaTitle::AwaitingApproval)
        && session.agent().as_deref() == Some("klaudia")
    {
        return Some("a permission dialog is open".into());
    }
    if session.blocked().is_some() {
        return Some("blocked on a person".into());
    }
    if let Some(pid) = session.pid() {
        if !agent_running(pid) {
            // A shell whose typed-in agent reported its conversation and then
            // died without saying so (WI-962): a hibernation would wake it into
            // that agent, which is not what the person left running.
            return Some("no agent CLI is running in it".into());
        }
        if let Some(shell) = shell_below_agent(pid) {
            return Some(format!("a shell is running below the agent (pid {shell})"));
        }
    }
    None
}

/// Whether a process named like an agent CLI is in the tree rooted at `root`.
fn agent_running(root: u32) -> bool {
    std::iter::once(root)
        .chain(hibernation::descendants(root))
        .any(|pid| {
            hibernation::process_name(pid).is_some_and(|name| AGENTS.contains(&name.as_str()))
        })
}

/// The pid of a shell running below the agent CLI in the tree rooted at
/// `root`, if any. The agent is the first process named like one, the root
/// included (a bare `claude`) or below it (`vogt-agent-auth run -- claude`,
/// whose wrapper shell is above the agent and does not count).
fn shell_below_agent(root: u32) -> Option<u32> {
    let mut tree = vec![root];
    tree.extend(hibernation::descendants(root));
    let agent = tree.iter().copied().find(|&pid| {
        hibernation::process_name(pid).is_some_and(|name| AGENTS.contains(&name.as_str()))
    })?;
    hibernation::descendants(agent).into_iter().find(|&pid| {
        hibernation::process_name(pid).is_some_and(|name| SHELLS.contains(&name.as_str()))
    })
}

/// Bytes available to this pod: its cgroup v2 limit minus usage when it has
/// a limit, else the host's `MemAvailable`. `None` when neither can be read.
pub fn memory_available() -> Option<u64> {
    let read = |path: &str| std::fs::read_to_string(path).ok();
    if let (Some(max), Some(current)) = (
        read("/sys/fs/cgroup/memory.max"),
        read("/sys/fs/cgroup/memory.current"),
    ) {
        if let (Ok(max), Ok(current)) = (max.trim().parse::<u64>(), current.trim().parse::<u64>()) {
            let cgroup = max.saturating_sub(current);
            let host = host_mem_available();
            return Some(host.map_or(cgroup, |h| h.min(cgroup)));
        }
    }
    host_mem_available()
}

fn host_mem_available() -> Option<u64> {
    let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
    let line = meminfo.lines().find(|l| l.starts_with("MemAvailable:"))?;
    let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    kib.checked_mul(1024)
}

/// A session policy may hibernate: its id, how long it has been quiet, and
/// whether the idle trigger must leave it alone (on autopilot).
pub type Candidate = (Uuid, Duration, bool);

/// One pass of the policy: which sessions to hibernate, and why each one.
/// Pure over its inputs, so the rules are testable without a clock or a
/// cgroup.
pub fn choose(
    policy: &Policy,
    candidates: &[Candidate],
    available: Option<u64>,
) -> Vec<(Uuid, HibernateTrigger, String)> {
    let mut chosen: Vec<(Uuid, HibernateTrigger, String)> = Vec::new();
    if let Some(after) = policy.idle_after {
        for &(id, quiet, idle_exempt) in candidates {
            if quiet >= after && !idle_exempt {
                chosen.push((
                    id,
                    HibernateTrigger::Idle,
                    format!(
                        "quiet for {} min (threshold {} min)",
                        quiet.as_secs() / 60,
                        after.as_secs() / 60
                    ),
                ));
            }
        }
    }
    if let (Some(floor), Some(available)) = (policy.memavailable_below, available) {
        if available < floor {
            let quietest = candidates
                .iter()
                .filter(|(id, _, _)| !chosen.iter().any(|(c, _, _)| c == id))
                .max_by_key(|(_, quiet, _)| *quiet);
            if let Some(&(id, quiet, _)) = quietest {
                chosen.push((
                    id,
                    HibernateTrigger::Memory,
                    format!(
                        "memory available {} MiB is below {} MiB; quietest eligible session ({} min)",
                        available >> 20,
                        floor >> 20,
                        quiet.as_secs() / 60
                    ),
                ));
            }
        }
    }
    chosen
}

/// Run the policy every minute, when a deployment configured one.
pub fn spawn_watcher(state: Arc<AppState>) {
    let policy = state.config.hibernation.clone();
    if policy.is_off() {
        return;
    }
    tracing::info!(?policy, "session hibernation policy is on");
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(TICK);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            run_once(&state.sessions, &policy).await;
        }
    });
}

/// One tick: find the eligible sessions, choose, hibernate.
pub async fn run_once(registry: &SessionRegistry, policy: &Policy) {
    // A few small `/proc` reads per session, once a minute: cheap enough to
    // do inline rather than shipping the registry to a blocking thread.
    let candidates: Vec<Candidate> = registry
        .live_sessions()
        .iter()
        .filter_map(|s| match exemption(registry, s) {
            None => Some((s.id, s.quiet_for(), s.autopilot())),
            Some(why) => {
                tracing::debug!(session = %s.id, reason = %why, "not hibernating");
                None
            }
        })
        .collect();
    let available = if policy.memavailable_below.is_some() {
        memory_available()
    } else {
        None
    };
    for (id, trigger, why) in choose(policy, &candidates, available) {
        match registry
            .hibernate(id, Some(why.clone()), trigger, false)
            .await
        {
            Ok(_) => tracing::info!(session = %id, ?trigger, reason = %why, "hibernated by policy"),
            Err(e) => {
                tracing::warn!(session = %id, error = %e, "policy could not hibernate a session")
            }
        }
    }
}

/// Wake the sessions pinned awake that the engine found hibernated at boot.
///
/// A linked session is woken through vogt-core's `session.wake`, with the
/// stack secret, so it gets a newly minted token — the engine never stores
/// one. The core may still be starting beside the engine, so this retries
/// for a few minutes before giving up on a session (which then stays
/// hibernated, listed, and wakeable by hand). With no core configured there
/// is no token to mint and the engine wakes it directly.
pub fn spawn_boot_wake(state: Arc<AppState>) {
    let pinned = state.sessions.hibernated_keep_awake();
    if pinned.is_empty() {
        return;
    }
    tokio::spawn(async move {
        for id in pinned {
            let woken = match state.vogt_core.as_ref() {
                None => state
                    .sessions
                    .wake(id, WakeRequest::default())
                    .await
                    .map(|_| ())
                    .map_err(|e| e.to_string()),
                Some(core) => wake_through_core(core, id).await,
            };
            match woken {
                Ok(()) => {
                    tracing::info!(session = %id, "woke a session pinned awake at boot");
                    prompt_resumed_overseer(&state, id);
                }
                Err(e) => {
                    tracing::warn!(session = %id, error = %e, "could not wake a session pinned awake; it stays hibernated")
                }
            }
        }
    });
}

/// How long a woken overseer is given to reach its prompt before the resume
/// line is dropped. A resumed Claude Code with MCP servers to start takes
/// tens of seconds.
const RESUME_PROMPT_WAIT: Duration = Duration::from_secs(300);

/// The line typed into an oversight session woken at boot, at `at`.
pub fn resume_prompt(at: &str) -> String {
    format!(
        "[vogt] This oversight session was resumed after the engine restarted at {at}. \
         Check on the sessions you oversee (session_sweep), wake the ones you need, \
         and carry on where you left off."
    )
}

/// Tell a woken oversight session it was resumed after a restart, once it is
/// at its prompt (WI-962). Workers are not told: they wake on demand, and the
/// overseer decides which to wake. Nothing is typed into a session that is
/// not ready within [`RESUME_PROMPT_WAIT`], that needs a person, or that is
/// not running an agent.
fn prompt_resumed_overseer(state: &Arc<AppState>, id: Uuid) {
    let Ok(session) = state.sessions.get(id) else {
        return;
    };
    if session.role() != SessionRole::Oversight || session.conversation().is_none() {
        return;
    }
    let bus = state.bus.clone();
    tokio::spawn(async move {
        let at = time::OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_default();
        match crate::wait::wait(
            &bus,
            Arc::clone(&session),
            WaitUntil::Ready,
            RESUME_PROMPT_WAIT,
        )
        .await
        {
            Ok(waited) if waited.matched => {
                // Enter on its own, as the autopilot nudge types it: a TUI
                // reads a burst ending in a carriage return as a paste.
                let typed = match session.write_input(resume_prompt(&at).as_bytes()) {
                    Ok(()) => {
                        tokio::time::sleep(Duration::from_millis(300)).await;
                        session.write_input(b"\r")
                    }
                    Err(e) => Err(e),
                };
                match typed {
                    Ok(()) => tracing::info!(session = %id, "told a woken overseer it was resumed"),
                    Err(e) => {
                        tracing::warn!(session = %id, error = %e, "could not prompt a woken overseer")
                    }
                }
            }
            Ok(waited) => tracing::info!(
                session = %id,
                outcome = %waited.outcome,
                "a woken overseer did not reach its prompt; not prompting it"
            ),
            Err(e) => {
                tracing::warn!(session = %id, error = %e, "could not wait for a woken overseer")
            }
        }
    });
}

async fn wake_through_core(core: &crate::vogt_core::VogtCore, id: Uuid) -> Result<(), String> {
    let mut delay = Duration::from_secs(2);
    let mut last = String::new();
    for _ in 0..8 {
        match core
            .post_json(
                "/sessions/wake",
                &serde_json::json!({
                    "id": id.to_string(),
                    "reason": "pinned awake; woken as the engine started",
                }),
            )
            .await
        {
            Ok(()) => return Ok(()),
            Err(e) => last = e,
        }
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(Duration::from_secs(60));
    }
    Err(last)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_and_sizes_parse_the_way_an_operator_writes_them() {
        assert_eq!(parse_duration("2h"), Some(Duration::from_secs(7200)));
        assert_eq!(parse_duration("30m"), Some(Duration::from_secs(1800)));
        assert_eq!(parse_duration("90"), Some(Duration::from_secs(90)));
        assert_eq!(parse_duration("1d"), Some(Duration::from_secs(86_400)));
        assert_eq!(parse_duration("0"), None);
        assert_eq!(parse_duration("2 hours"), None);
        assert_eq!(parse_size("2GiB"), Some(2 << 30));
        assert_eq!(parse_size("512M"), Some(512 << 20));
        assert_eq!(parse_size("1024"), Some(1024));
        assert_eq!(parse_size("lots"), None);
    }

    #[test]
    fn idle_takes_everything_past_the_threshold_and_memory_the_quietest_left() {
        let (a, b, c) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let mins = |m: u64| Duration::from_secs(m * 60);
        let candidates = [
            (a, mins(200), false),
            (b, mins(30), false),
            (c, mins(90), false),
        ];
        let idle = Policy {
            idle_after: Some(mins(120)),
            memavailable_below: None,
        };
        let chosen: Vec<_> = choose(&idle, &candidates, None)
            .into_iter()
            .map(|(id, t, _)| (id, t))
            .collect();
        assert_eq!(chosen, vec![(a, HibernateTrigger::Idle)]);

        let both = Policy {
            idle_after: Some(mins(120)),
            memavailable_below: Some(2 << 30),
        };
        let chosen: Vec<_> = choose(&both, &candidates, Some(1 << 30))
            .into_iter()
            .map(|(id, t, _)| (id, t))
            .collect();
        assert_eq!(
            chosen,
            vec![(a, HibernateTrigger::Idle), (c, HibernateTrigger::Memory)],
            "memory pressure takes one more: the quietest not already chosen"
        );
        assert_eq!(
            choose(&both, &candidates, Some(4 << 30)).len(),
            1,
            "no pressure"
        );
        assert!(choose(&Policy::default(), &candidates, Some(0)).is_empty());
    }

    #[test]
    fn autopilot_is_spared_by_idle_but_not_by_memory_pressure() {
        let (looping, idle) = (Uuid::new_v4(), Uuid::new_v4());
        let mins = |m: u64| Duration::from_secs(m * 60);
        // The looping session is the quietest: it paused between items.
        let candidates = [(looping, mins(300), true), (idle, mins(150), false)];
        let idle_only = Policy {
            idle_after: Some(mins(120)),
            memavailable_below: None,
        };
        let chosen: Vec<_> = choose(&idle_only, &candidates, None)
            .into_iter()
            .map(|(id, _, _)| id)
            .collect();
        assert_eq!(
            chosen,
            vec![idle],
            "the idle trigger leaves autopilot alone"
        );
        let pressure = Policy {
            idle_after: None,
            memavailable_below: Some(2 << 30),
        };
        let chosen: Vec<_> = choose(&pressure, &candidates, Some(1 << 30))
            .into_iter()
            .map(|(id, t, _)| (id, t))
            .collect();
        assert_eq!(chosen, vec![(looping, HibernateTrigger::Memory)]);
    }
}
