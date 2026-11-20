//! When the engine hibernates a session by itself (WI-912).
//!
//! Off unless a deployment asks: `ENGINE_HIBERNATE_IDLE_AFTER` (a duration,
//! `2h`) hibernates an agent session that has been quiet that long, and
//! `ENGINE_HIBERNATE_MEMAVAILABLE_BELOW` (a size, `2GiB`) hibernates the
//! quietest eligible session, one per tick, while the memory available to the
//! pod is below it. Both keep every exemption, and each exemption is a reason
//! a person can read — the same sentence the watcher logs.
//!
//! `ENGINE_HIBERNATE_DONE_AFTER` (a duration, `10m`) hibernates a session
//! whose agent declared itself done (WI-1518 `AUTOPILOT: DONE`; a `result`
//! report, WI-1520, is not ported to this line yet) at least that long ago
//! and has had no input since (WI-1523), with trigger `done`. An oversight session is never
//! hibernated this way: it declares a sweep done and is then woken by the
//! next one. Autopilot does not exempt it — the declaration is the end of
//! the backlog the exemption protects.
//!
//! `ENGINE_HIBERNATE_LIVE_CAP` is not a watcher trigger. It is the number of
//! live sessions at which the assistant refuses to start another one (WI-1269).
//! Set on its own it leaves the watcher off: idle and memory are still what
//! hibernate a session.
//!
//! What is never hibernated by policy:
//!
//! - a session that cannot be at all (no conversation to resume, an
//!   agent-task run, exited) — `SessionRegistry::hibernate_refusal`;
//! - one pinned with `keep_awake`;
//! - one whose turn is running, or that shows a permission dialog (read from
//!   an agent's title included), or that drew
//!   anything within the activity quiet window. Quiet time here is raw
//!   output time, not the screen-change clock activity reads (WI-1090): an
//!   agent animating only colours mid-turn must never look quiet to it;
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

use crate::{app::AppState, error::ApiError, hibernation, pty::Session, sessions::SessionRegistry};
use axum::{extract::State, Json};

/// The deployment's hibernation policy. The default is off.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Policy {
    /// Hibernate an eligible session quiet for at least this long.
    pub idle_after: Option<Duration>,
    /// Hibernate the quietest eligible session while the memory available to
    /// the pod (its cgroup limit minus usage, or else the host's
    /// `MemAvailable`) is below this many bytes.
    pub memavailable_below: Option<u64>,
    /// How many live sessions the assistant will start up to. Not a watcher
    /// trigger: `is_off` ignores it, and the watcher never reads it. Unset
    /// means the assistant does not count live sessions before a start.
    pub live_cap: Option<usize>,
    /// Hibernate an eligible session whose agent declared itself done at
    /// least this long ago, with no input since (WI-1523).
    pub done_after: Option<Duration>,
}

impl Policy {
    /// Whether the watcher has nothing to do. A live cap alone does not turn
    /// it on: the cap is enforced at the assistant's start gate, not here.
    pub fn is_off(&self) -> bool {
        self.idle_after.is_none() && self.memavailable_below.is_none() && self.done_after.is_none()
    }
}

/// How often the watcher looks.
const TICK: Duration = Duration::from_secs(60);

/// Agent CLIs, as `/proc/<pid>/comm` names them.
const AGENTS: &[&str] = &["claude", "codex", "opencode"];

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
    // Activity reads the screen-change clock, which an agent animating
    // only colours mid-turn can leave reading `idle` (WI-1090). Hibernation
    // trusts the raw bytes instead: anything drawn within the quiet window
    // may be a live turn.
    if session.output_quiet_for() < session.idle_after() {
        return Some("output is still arriving".into());
    }
    // The WI-1127 Codex fork (WI-1145) says in the window title when a turn is
    // running, and when it waits on a permission ask, question or plan
    // approval — none of which are dialogs the engine recognises from the
    // screen.
    if matches!(session.agent().as_deref(), Some("codex")) {
        use crate::screen::{agent_title, AgentTitle};
        match agent_title(session.title().as_deref()) {
            Some(AgentTitle::Working) => return Some("a turn is running".into()),
            Some(AgentTitle::AwaitingApproval) => {
                return Some("a permission dialog is open".into())
            }
            _ => {}
        }
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

/// A session policy may hibernate: its id, how long it has been quiet,
/// whether the idle trigger must leave it alone (on autopilot, or an
/// oversight session, WI-4457), and how long
/// ago it declared itself done when the done trigger may take it (no input
/// since, not an overseer).
pub type Candidate = (Uuid, Duration, bool, Option<Duration>);

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
        for &(id, quiet, idle_exempt, _) in candidates {
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
    if let Some(after) = policy.done_after {
        for &(id, _, _, done_for) in candidates {
            if chosen.iter().any(|(c, _, _)| *c == id) {
                continue;
            }
            if let Some(done_for) = done_for.filter(|d| *d >= after) {
                chosen.push((
                    id,
                    HibernateTrigger::Done,
                    format!(
                        "done {} min ago with no input since (threshold {} min)",
                        done_for.as_secs() / 60,
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
                .filter(|(id, _, _, _)| !chosen.iter().any(|(c, _, _)| c == id))
                .max_by_key(|(_, quiet, _, _)| *quiet);
            if let Some(&(id, quiet, _, _)) = quietest {
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

/// The policy the watcher reads on every tick, changeable while the engine
/// runs (WI-4457). It starts from the boot configuration, with any overrides
/// saved by an earlier `PUT` laid over it, so a change made live survives a
/// restart. The watcher re-reads it every minute, so a change takes effect on
/// the next sweep with no restart.
#[derive(Debug)]
pub struct LivePolicy {
    current: std::sync::RwLock<Policy>,
    /// Where an accepted change is saved; `None` keeps it in memory only.
    path: Option<std::path::PathBuf>,
}

impl LivePolicy {
    /// Start from `boot`, with the overrides saved at `path` laid over it.
    /// A saved file that cannot be read or parsed is logged and ignored: the
    /// boot policy still applies rather than the engine refusing to start.
    pub fn load(boot: Policy, path: Option<std::path::PathBuf>) -> Self {
        let mut policy = boot;
        if let Some(path) = path.as_deref() {
            match std::fs::read_to_string(path) {
                Ok(raw) => match serde_json::from_str::<serde_json::Value>(&raw)
                    .map_err(|e| e.to_string())
                    .and_then(|v| apply_patch(&policy, &v))
                {
                    Ok(saved) => policy = saved,
                    Err(e) => tracing::warn!(
                        path = %path.display(),
                        error = %e,
                        "ignoring a saved hibernation policy that does not parse"
                    ),
                },
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "could not read the saved hibernation policy"
                ),
            }
        }
        Self {
            current: std::sync::RwLock::new(policy),
            path,
        }
    }

    /// The policy as the watcher will read it on its next tick.
    pub fn get(&self) -> Policy {
        self.current
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Apply a partial change (see [`apply_patch`]), save it, and make it
    /// the policy the next tick reads. Returns the (before, after) pair. A
    /// change that cannot be saved is refused, so it never applies in memory
    /// only.
    pub fn update(&self, patch: &serde_json::Value) -> Result<(Policy, Policy), ApiError> {
        let mut current = self
            .current
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let before = current.clone();
        let after = apply_patch(&before, patch).map_err(ApiError::BadRequest)?;
        if let Some(path) = self.path.as_deref() {
            save(path, &after)?;
        }
        *current = after.clone();
        Ok((before, after))
    }
}

/// The three live keys of a policy as JSON. Durations are seconds, sizes are
/// bytes, and an unset trigger is `null`. `live_cap` is boot-only and is not
/// written here.
pub fn to_json(policy: &Policy) -> serde_json::Value {
    serde_json::json!({
        "idle_after": policy.idle_after.map(|d| d.as_secs()),
        "done_after": policy.done_after.map(|d| d.as_secs()),
        "memavailable_below": policy.memavailable_below,
    })
}

/// Lay a partial change over `base`. Only the keys present change; `null`
/// unsets a trigger. A duration takes `10m` or a number of seconds, a size
/// takes `2GiB` or a number of bytes. `live_cap` is boot-only, and an unknown
/// key is refused, so a typo cannot pass silently.
pub fn apply_patch(base: &Policy, patch: &serde_json::Value) -> Result<Policy, String> {
    let object = patch
        .as_object()
        .ok_or_else(|| "the body must be a JSON object".to_string())?;
    let mut next = base.clone();
    for (key, value) in object {
        match key.as_str() {
            "idle_after" => next.idle_after = duration_field(key, value)?,
            "done_after" => next.done_after = duration_field(key, value)?,
            "memavailable_below" => {
                next.memavailable_below =
                    match value {
                        serde_json::Value::Null => None,
                        serde_json::Value::Number(n) => Some(
                            n.as_u64()
                                .filter(|b| *b > 0)
                                .ok_or_else(|| format!("{key} must be a positive size"))?,
                        ),
                        serde_json::Value::String(s) => Some(parse_size(s).ok_or_else(|| {
                            format!("{key}={s:?} is not a size like 2GiB or 512M")
                        })?),
                        _ => return Err(format!("{key} must be a size, a number or null")),
                    }
            }
            "live_cap" => return Err(
                "live_cap is set at boot (ENGINE_HIBERNATE_LIVE_CAP) and cannot be changed live"
                    .into(),
            ),
            other => return Err(format!("unknown hibernation policy key {other:?}")),
        }
    }
    Ok(next)
}

fn duration_field(key: &str, value: &serde_json::Value) -> Result<Option<Duration>, String> {
    match value {
        serde_json::Value::Null => Ok(None),
        serde_json::Value::Number(n) => n
            .as_u64()
            .filter(|s| *s > 0)
            .map(|s| Some(Duration::from_secs(s)))
            .ok_or_else(|| format!("{key} must be a positive number of seconds")),
        serde_json::Value::String(s) => parse_duration(s)
            .map(Some)
            .ok_or_else(|| format!("{key}={s:?} is not a duration like 10m, 1h or 600")),
        _ => Err(format!(
            "{key} must be a duration, a number of seconds or null"
        )),
    }
}

/// Save the policy atomically: a temporary file, then a rename over the old.
fn save(path: &std::path::Path, policy: &Policy) -> Result<(), ApiError> {
    let body = serde_json::to_vec_pretty(&to_json(policy))
        .map_err(|e| ApiError::Internal(format!("encoding the hibernation policy: {e}")))?;
    let tmp = path.with_extension("json.tmp");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&tmp, body)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// `GET /api/hibernation/policy`: the policy the watcher reads on its next tick.
pub async fn get_policy(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    Json(to_json(&state.hibernation.get()))
}

/// Who may change the caps (WI-4457): a person, the break-glass operator, or
/// vogt-core (decided by the gate's flags, never by `name`), or a session the
/// engine has marked oversight. Any other token, a worker's included, is
/// refused with a 403 and an audit line, however many scopes it holds: the
/// `sessions` capability that gates the route is granted to every writer.
fn require_policy_authority(
    state: &AppState,
    identity: Option<&crate::auth::AuthorizedIdentity>,
) -> Result<(), ApiError> {
    let Some(identity) = identity else {
        return Err(ApiError::Unauthorized);
    };
    if identity.person || identity.break_glass || identity.stack_secret {
        return Ok(());
    }
    let role = crate::git::caller_session(state, &identity.name)
        .and_then(|id| state.sessions.summary_of(id))
        .map(|s| s.role);
    if role == Some(SessionRole::Oversight) {
        return Ok(());
    }
    tracing::warn!(
        target: "vogt::audit",
        event = "hibernation.policy",
        outcome = "refused",
        caller = %identity.name,
        "hibernation policy change refused"
    );
    Err(ApiError::Forbidden(format!(
        "only a person or an oversight session changes the hibernation caps \
         ({} is neither)",
        identity.name
    )))
}

/// `PUT /api/hibernation/policy`: change one or more live keys. The change is
/// audited, saved, and read by the next tick. Answers with the policy after the
/// change. Only a person or an oversight session may change it
/// (`require_policy_authority`).
pub async fn put_policy(
    State(state): State<Arc<AppState>>,
    identity: Option<axum::Extension<crate::auth::AuthorizedIdentity>>,
    Json(patch): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_policy_authority(&state, identity.as_deref())?;
    let (before, after) = state.hibernation.update(&patch)?;
    tracing::info!(
        target: "vogt::audit",
        event = "hibernation.policy",
        before = %to_json(&before),
        after = %to_json(&after),
        "hibernation policy changed"
    );
    Ok(Json(to_json(&after)))
}

/// Run the policy every minute. It is always running; a policy with no
/// trigger on is a tick that does nothing, so a cap set later takes effect
/// with no restart (WI-4457).
pub fn spawn_watcher(state: Arc<AppState>) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(TICK);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            let policy = state.hibernation.get();
            if policy.is_off() {
                continue;
            }
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
            // Oversight is spared the idle trigger as autopilot is: it is the
            // session that brings the others back, and it sits quiet between
            // sweeps by design (WI-4457). Memory pressure may still take it.
            None => Some((
                s.id,
                s.output_quiet_for(),
                s.autopilot() || s.role() == SessionRole::Oversight,
                done_for(s),
            )),
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
            .hibernate_internal(id, Some(why.clone()), trigger, false, None)
            .await
        {
            Ok(_) => tracing::info!(session = %id, ?trigger, reason = %why, "hibernated by policy"),
            Err(e) => {
                tracing::warn!(session = %id, error = %e, "policy could not hibernate a session")
            }
        }
    }
}

/// How long a pinned session refused for want of an account slot waits before
/// the boot wake asks again, and how many passes it makes (WI-4423). A full
/// account frees a slot as a session stops, so a retry can succeed later.
const BOOT_WAKE_RETRY: Duration = Duration::from_secs(60);
const BOOT_WAKE_PASSES: u32 = 30;

/// Why a boot wake did not happen (WI-4423). A capacity refusal is retried;
/// a handoff refusal is final; anything else is logged and not retried.
#[derive(Debug, PartialEq, Eq)]
enum BootRefusal {
    Capacity,
    Handoff,
    Failed,
}

fn direct_refusal(error: &crate::error::ApiError) -> BootRefusal {
    match error {
        crate::error::ApiError::AtCapacity(_) => BootRefusal::Capacity,
        other if other.to_string().starts_with("conflict:") => BootRefusal::Handoff,
        _ => BootRefusal::Failed,
    }
}

fn core_refusal(error: &str) -> BootRefusal {
    if error.contains("account at capacity") {
        BootRefusal::Capacity
    } else if error.contains("409") {
        BootRefusal::Handoff
    } else {
        BootRefusal::Failed
    }
}

/// How long ago the session's agent declared itself done, when the done
/// trigger may take it: not an overseer, and no input written to it since
/// the declaration (WI-1523).
fn done_for(session: &Session) -> Option<Duration> {
    if session.role() == SessionRole::Oversight {
        return None;
    }
    let at = session.done_at()?;
    let at =
        time::OffsetDateTime::parse(&at, &time::format_description::well_known::Rfc3339).ok()?;
    let age = time::OffsetDateTime::now_utc() - at;
    let age = Duration::try_from(age).ok()?;
    done_quiet(age, session.input_age())
}

/// The done age, when no input arrived after the declaration: input `input_age`
/// ago is after a declaration `done_age` ago when it is the younger of the two.
fn done_quiet(done_age: Duration, input_age: Option<Duration>) -> Option<Duration> {
    match input_age {
        Some(input) if input < done_age => None,
        _ => Some(done_age),
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
///
/// A session refused because its account is at capacity (WI-4423) is tried
/// again every minute for a while, so a slot that frees up still wakes it.
pub fn spawn_boot_wake(state: Arc<AppState>) {
    let pinned = state.sessions.hibernated_keep_awake();
    if pinned.is_empty() {
        return;
    }
    tokio::spawn(async move {
        let mut waiting = pinned;
        for pass in 1..=BOOT_WAKE_PASSES {
            let mut full = Vec::new();
            for id in waiting {
                let woken = match state.vogt_core.as_ref() {
                    None => state
                        .sessions
                        .wake(id, WakeRequest::default())
                        .await
                        .map(|_| ())
                        .map_err(|e| (direct_refusal(&e), e.to_string())),
                    Some(core) => wake_through_core(core, id)
                        .await
                        .map_err(|e| (core_refusal(&e), e)),
                };
                match woken {
                    Ok(()) => {
                        tracing::info!(session = %id, "woke a session pinned awake at boot");
                        prompt_resumed_overseer(&state, id);
                    }
                    // The handoff guard refused. Said as a refusal, not a failure,
                    // and not retried: the direct wake has no retry, and the core
                    // path stops inside wake_through_core.
                    Err((BootRefusal::Handoff, e)) => {
                        tracing::info!(
                            session = %id,
                            error = %e,
                            "boot wake refused; the session stays hibernated"
                        );
                    }
                    Err((BootRefusal::Capacity, e)) => {
                        tracing::warn!(
                            session = %id,
                            pass,
                            error = %e,
                            "boot wake waits for an account slot; it stays hibernated and is tried again"
                        );
                        full.push(id);
                    }
                    Err((BootRefusal::Failed, e)) => {
                        tracing::warn!(session = %id, error = %e, "could not wake a session pinned awake; it stays hibernated")
                    }
                }
            }
            if full.is_empty() {
                return;
            }
            waiting = full;
            if pass < BOOT_WAKE_PASSES {
                tokio::time::sleep(BOOT_WAKE_RETRY).await;
            }
        }
        tracing::warn!(
            sessions = ?waiting,
            "boot wake gave up on sessions refused for an account slot; they stay hibernated"
        );
    });
}

/// How long a woken overseer is given to reach its prompt before the resume
/// line is dropped. A resumed Claude Code with MCP servers to start takes
/// tens of seconds.
const RESUME_PROMPT_WAIT: Duration = Duration::from_secs(300);
/// A resume prompt held back by a dialog is typed again, this many times at
/// most, [`RESUME_RETRY_GAP`] apart (WI-4459).
const RESUME_ATTEMPTS: usize = 6;
const RESUME_RETRY_GAP: Duration = Duration::from_secs(10);

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
                // Enter on its own, as the autopilot nudge types it, through
                // the shared guard (WI-4459). A dialog holds the prompt back
                // and the line is erased, so it is typed again a little later;
                // a stranded line is left alone rather than typed on top.
                let line = resume_prompt(&at);
                for attempt in 0..RESUME_ATTEMPTS {
                    if attempt > 0 {
                        tokio::time::sleep(RESUME_RETRY_GAP).await;
                        if !session.is_alive() {
                            break;
                        }
                    }
                    match crate::enter_guard::type_then_enter(&session, &line, "overseer resume")
                        .await
                    {
                        Ok(crate::enter_guard::EnterOutcome::Sent) => {
                            tracing::info!(session = %id, "told a woken overseer it was resumed");
                            break;
                        }
                        Ok(crate::enter_guard::EnterOutcome::Skipped(_)) => continue,
                        Ok(crate::enter_guard::EnterOutcome::Stranded(_)) => break,
                        Err(e) => {
                            tracing::warn!(session = %id, error = %e, "could not prompt a woken overseer");
                            break;
                        }
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
            // A 409 is the handoff guard refusing, and it will refuse the same
            // way on every retry. Stop at once and say it was refused.
            Err(e) if e.contains("409") => {
                tracing::info!(session = %id, error = %e, "boot wake refused; not retrying");
                return Err(e);
            }
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
    fn a_full_account_is_retried_and_a_handoff_refusal_is_not() {
        let full = crate::error::ApiError::AtCapacity("cap 8; counts {}".into());
        assert_eq!(direct_refusal(&full), BootRefusal::Capacity);
        assert_eq!(
            core_refusal("409 Conflict: account at capacity: cap 8; counts {\"acct2\": 8}"),
            BootRefusal::Capacity
        );
        let handoff = crate::error::ApiError::Conflict("conflict: handed off".into());
        assert_eq!(direct_refusal(&handoff), BootRefusal::Handoff);
        assert_eq!(
            core_refusal("409 Conflict: handed off"),
            BootRefusal::Handoff
        );
        assert_eq!(core_refusal("connection refused"), BootRefusal::Failed);
    }

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
            (a, mins(200), false, None),
            (b, mins(30), false, None),
            (c, mins(90), false, None),
        ];
        let idle = Policy {
            idle_after: Some(mins(120)),
            memavailable_below: None,
            live_cap: None,
            done_after: None,
        };
        let chosen: Vec<_> = choose(&idle, &candidates, None)
            .into_iter()
            .map(|(id, t, _)| (id, t))
            .collect();
        assert_eq!(chosen, vec![(a, HibernateTrigger::Idle)]);

        let both = Policy {
            idle_after: Some(mins(120)),
            memavailable_below: Some(2 << 30),
            live_cap: None,
            done_after: None,
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
        let candidates = [
            (looping, mins(300), true, None),
            (idle, mins(150), false, None),
        ];
        let idle_only = Policy {
            idle_after: Some(mins(120)),
            memavailable_below: None,
            live_cap: None,
            done_after: None,
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
            live_cap: None,
            done_after: None,
        };
        let chosen: Vec<_> = choose(&pressure, &candidates, Some(1 << 30))
            .into_iter()
            .map(|(id, t, _)| (id, t))
            .collect();
        assert_eq!(chosen, vec![(looping, HibernateTrigger::Memory)]);
    }

    #[test]
    fn a_live_cap_alone_does_not_turn_the_watcher_on() {
        let capped = Policy {
            live_cap: Some(3),
            ..Policy::default()
        };
        assert!(capped.is_off());
        assert!(Policy::default().is_off());
        assert!(!Policy {
            idle_after: Some(Duration::from_secs(60)),
            live_cap: Some(3),
            ..Policy::default()
        }
        .is_off());
    }

    #[test]
    fn done_takes_a_quiet_done_session_and_spares_one_given_input_since() {
        let (done, fresh, typed, undone) = (
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
        );
        let mins = |m: u64| Duration::from_secs(m * 60);
        let candidates = [
            (done, mins(11), true, done_quiet(mins(11), Some(mins(20)))),
            (fresh, mins(3), true, done_quiet(mins(3), None)),
            (typed, mins(11), false, done_quiet(mins(15), Some(mins(2)))),
            (undone, mins(60), false, None),
        ];
        let policy = Policy {
            done_after: Some(mins(10)),
            ..Policy::default()
        };
        assert!(!policy.is_off());
        let chosen: Vec<_> = choose(&policy, &candidates, None)
            .into_iter()
            .map(|(id, t, _)| (id, t))
            .collect();
        assert_eq!(
            chosen,
            vec![(done, HibernateTrigger::Done)],
            "autopilot does not exempt a done session; input since the declaration does"
        );
        let both = Policy {
            idle_after: Some(mins(10)),
            done_after: Some(mins(10)),
            ..Policy::default()
        };
        let chosen: Vec<_> = choose(&both, &candidates, None)
            .into_iter()
            .map(|(id, t, _)| (id, t))
            .collect();
        assert_eq!(
            chosen,
            vec![
                (typed, HibernateTrigger::Idle),
                (undone, HibernateTrigger::Idle),
                (done, HibernateTrigger::Done),
            ],
            "a session is chosen once"
        );
    }
}

#[cfg(test)]
mod live_policy_tests {
    use super::*;
    use serde_json::json;

    fn mins(m: u64) -> Duration {
        Duration::from_secs(m * 60)
    }

    #[test]
    fn a_patch_changes_only_the_keys_it_names() {
        let base = Policy {
            idle_after: Some(mins(60)),
            done_after: Some(mins(10)),
            ..Policy::default()
        };
        let next = apply_patch(&base, &json!({ "done_after": "5m" })).unwrap();
        assert_eq!(next.idle_after, Some(mins(60)), "untouched");
        assert_eq!(next.done_after, Some(mins(5)));
        let unset = apply_patch(&next, &json!({ "idle_after": null })).unwrap();
        assert_eq!(unset.idle_after, None, "null unsets a trigger");
        let sized = apply_patch(&base, &json!({ "memavailable_below": "2GiB" })).unwrap();
        assert_eq!(sized.memavailable_below, Some(2 << 30));
        let seconds = apply_patch(&base, &json!({ "idle_after": 90 })).unwrap();
        assert_eq!(seconds.idle_after, Some(Duration::from_secs(90)));
    }

    #[test]
    fn a_patch_refuses_what_the_watcher_cannot_take_live() {
        let base = Policy::default();
        let live_cap = apply_patch(&base, &json!({ "live_cap": 3 })).unwrap_err();
        assert!(live_cap.contains("boot"), "{live_cap}");
        let typo = apply_patch(&base, &json!({ "idle_aftr": "1h" })).unwrap_err();
        assert!(typo.contains("unknown"), "{typo}");
        assert!(apply_patch(&base, &json!({ "idle_after": "2 hours" })).is_err());
        assert!(apply_patch(&base, &json!({ "idle_after": 0 })).is_err());
        assert!(apply_patch(&base, &json!({ "memavailable_below": true })).is_err());
        assert!(apply_patch(&base, &json!(["idle_after"])).is_err());
    }

    #[test]
    fn a_saved_change_is_what_the_next_boot_starts_from() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("hibernation-policy.json");
        let boot = Policy {
            idle_after: Some(mins(120)),
            ..Policy::default()
        };
        let live = LivePolicy::load(boot.clone(), Some(path.clone()));
        assert_eq!(live.get(), boot, "no saved file: the boot policy");
        let (before, after) = live.update(&json!({ "idle_after": "15m" })).unwrap();
        assert_eq!(before.idle_after, Some(mins(120)));
        assert_eq!(after.idle_after, Some(mins(15)));
        assert_eq!(live.get(), after);

        let reboot = LivePolicy::load(boot, Some(path));
        assert_eq!(
            reboot.get().idle_after,
            Some(mins(15)),
            "survives a restart"
        );
    }

    #[test]
    fn an_unreadable_saved_policy_falls_back_to_the_boot_policy() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("hibernation-policy.json");
        std::fs::write(&path, "{ not json").unwrap();
        let boot = Policy {
            done_after: Some(mins(10)),
            ..Policy::default()
        };
        assert_eq!(LivePolicy::load(boot.clone(), Some(path)).get(), boot);
    }
}
