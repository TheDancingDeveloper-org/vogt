//! A cgroup v2 leaf per session, so one runaway cannot OOM the rest (WI-895).
//!
//! Every session's process tree (the agent CLI, its MCP servers, and whatever
//! it spawns — a Soot run at 12 GiB, Gradle and Kotlin daemons at `-Xmx8G`)
//! otherwise shares the pod's cgroup, and a runaway reaches the pod or host
//! OOM killer, which takes the largest process anywhere. Here each session
//! runs in its own leaf with `memory.high` (throttle and reclaim),
//! `memory.max` (the OOM kill stays inside the leaf) and `memory.swap.max`.
//!
//! Layout, under the engine's own cgroup (`/proc/self/cgroup`):
//!
//! ```text
//! <own>/                 subtree_control: +memory
//! <own>/engine/          the engine and everything already in <own>
//! <own>/sessions/        subtree_control: +memory
//! <own>/sessions/<uuid>/ one session: memory.high / memory.max / swap.max
//! ```
//!
//! cgroup v2 forbids processes in a cgroup that hands controllers to its
//! children, so the processes already in `<own>` move to `engine/` first.
//!
//! Membership is inherited across fork and survives reparenting to init, so a
//! daemon a session detaches (Gradle) stays in, and is capped with, the
//! session that started it. A leaf that still holds such a daemon when its
//! session ends is left in place and removed by a later [`sweep`] once empty.
//!
//! The child is moved into its leaf right after the PTY spawn returns (the
//! PTY crate offers no pre-exec hook); the window is the time between fork
//! and that write, long before an agent starts its MCP servers.
//!
//! Degrades to nothing: off Linux, without cgroup v2, without the memory
//! controller, or on a read-only cgroup mount (the default in Docker and
//! Kubernetes without delegation) setup fails once, logs why, and sessions
//! run in the shared cgroup as before.
//!
//! Configuration (environment, read once at startup):
//!
//! - `ENGINE_SESSION_CGROUP` — `auto` (default: on when permitted), `off`.
//! - `ENGINE_SESSION_MEMORY_MAX` — hard cap per session (`16GiB`, default),
//!   `max` for none.
//! - `ENGINE_SESSION_MEMORY_HIGH` — throttle point per session; default 90 %
//!   of the max, `max` for none.
//! - `ENGINE_SESSION_SWAP_MAX` — swap per session, default `0`.

use std::{
    path::{Path, PathBuf},
    sync::OnceLock,
};

use uuid::Uuid;

use crate::hibernate_policy::parse_size;

/// What each session's leaf is given. `None` is cgroup v2's `max` (no limit).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub memory_max: Option<u64>,
    pub memory_high: Option<u64>,
    pub swap_max: Option<u64>,
}

/// The default hard cap: 16 GiB.
pub const DEFAULT_MEMORY_MAX: u64 = 16 << 30;

impl Default for Limits {
    fn default() -> Self {
        Self {
            memory_max: Some(DEFAULT_MEMORY_MAX),
            memory_high: Some(DEFAULT_MEMORY_MAX / 10 * 9),
            swap_max: Some(0),
        }
    }
}

/// `ENGINE_SESSION_CGROUP` and the limits, or why they do not parse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policy {
    pub enabled: bool,
    pub limits: Limits,
}

/// A size, or `max` for no limit.
fn parse_limit(name: &str, raw: &str) -> Result<Option<u64>, String> {
    let raw = raw.trim();
    if raw.eq_ignore_ascii_case("max") {
        return Ok(None);
    }
    if raw == "0" {
        return Ok(Some(0));
    }
    parse_size(raw)
        .map(Some)
        .ok_or_else(|| format!("{name}={raw:?} is not a size like 16GiB, 0 or max"))
}

impl Policy {
    /// Read the policy from `get` (the environment, in production).
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let get = |name: &str| get(name).filter(|v| !v.trim().is_empty());
        let enabled = match get("ENGINE_SESSION_CGROUP").as_deref().map(str::trim) {
            None | Some("auto") | Some("on") | Some("1") | Some("true") => true,
            Some("off") | Some("0") | Some("false") => false,
            Some(other) => {
                return Err(format!(
                    "ENGINE_SESSION_CGROUP={other:?} is not auto or off"
                ));
            }
        };
        let defaults = Limits::default();
        let memory_max = match get("ENGINE_SESSION_MEMORY_MAX") {
            Some(v) => parse_limit("ENGINE_SESSION_MEMORY_MAX", &v)?,
            None => defaults.memory_max,
        };
        let memory_high = match get("ENGINE_SESSION_MEMORY_HIGH") {
            Some(v) => parse_limit("ENGINE_SESSION_MEMORY_HIGH", &v)?,
            None => memory_max.map(|m| m / 10 * 9),
        };
        let swap_max = match get("ENGINE_SESSION_SWAP_MAX") {
            Some(v) => parse_limit("ENGINE_SESSION_SWAP_MAX", &v)?,
            None => defaults.swap_max,
        };
        Ok(Self {
            enabled,
            limits: Limits {
                memory_max,
                memory_high,
                swap_max,
            },
        })
    }

    pub fn from_env() -> Result<Self, String> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }
}

/// The delegated subtree, once set up.
#[derive(Debug)]
pub struct Manager {
    /// `<own>/sessions`.
    sessions_dir: PathBuf,
    limits: Limits,
}

/// One session's memory, as its leaf reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Stats {
    pub current: Option<u64>,
    pub peak: Option<u64>,
    pub max: Option<u64>,
    /// `oom_kill` from `memory.events`: processes the cap has killed.
    pub oom_kills: u64,
}

fn limit_value(limit: Option<u64>) -> String {
    limit.map_or_else(|| "max".to_string(), |v| v.to_string())
}

fn write(path: &Path, value: &str) -> Result<(), String> {
    std::fs::write(path, value).map_err(|e| format!("write {value:?} to {}: {e}", path.display()))
}

fn read_u64(path: &Path) -> Option<u64> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// The engine's own cgroup v2 path (`0::/x/y` in `/proc/self/cgroup`).
pub fn own_cgroup_path(proc_self_cgroup: &str) -> Option<String> {
    proc_self_cgroup
        .lines()
        .find_map(|line| line.strip_prefix("0::"))
        .map(|p| p.trim().to_string())
}

impl Manager {
    /// Set up the subtree under `own` (the engine's cgroup directory).
    pub fn setup(own: &Path, limits: Limits) -> Result<Self, String> {
        let controllers = std::fs::read_to_string(own.join("cgroup.controllers"))
            .map_err(|e| format!("not a cgroup v2 directory ({}): {e}", own.display()))?;
        if !controllers.split_whitespace().any(|c| c == "memory") {
            return Err(format!(
                "the memory controller is not delegated to {}",
                own.display()
            ));
        }
        let engine = own.join("engine");
        let sessions = own.join("sessions");
        for dir in [&engine, &sessions] {
            if let Err(e) = std::fs::create_dir(dir) {
                if e.kind() != std::io::ErrorKind::AlreadyExists {
                    return Err(format!("create {}: {e}", dir.display()));
                }
            }
        }
        // No internal processes: everything in `own` goes to `engine/`. A
        // process can exit between the read and the move; that is fine.
        let procs = std::fs::read_to_string(own.join("cgroup.procs")).unwrap_or_default();
        for pid in procs.split_whitespace() {
            if let Err(e) = write(&engine.join("cgroup.procs"), pid) {
                tracing::debug!(error = %e, "could not move a process into the engine cgroup");
            }
        }
        write(&own.join("cgroup.subtree_control"), "+memory")?;
        write(&sessions.join("cgroup.subtree_control"), "+memory")?;
        Ok(Self {
            sessions_dir: sessions,
            limits,
        })
    }

    fn leaf(&self, id: Uuid) -> PathBuf {
        self.sessions_dir.join(id.to_string())
    }

    /// Create `id`'s leaf with the limits and move `pid` into it.
    pub fn attach(&self, id: Uuid, pid: u32) -> Result<(), String> {
        let leaf = self.leaf(id);
        if let Err(e) = std::fs::create_dir(&leaf) {
            if e.kind() != std::io::ErrorKind::AlreadyExists {
                return Err(format!("create {}: {e}", leaf.display()));
            }
        }
        // Throttle before the cap, so a high above max never applies first.
        write(
            &leaf.join("memory.high"),
            &limit_value(self.limits.memory_high),
        )?;
        write(
            &leaf.join("memory.max"),
            &limit_value(self.limits.memory_max),
        )?;
        // swap accounting can be compiled out; a missing file is not fatal.
        if leaf.join("memory.swap.max").exists() {
            write(
                &leaf.join("memory.swap.max"),
                &limit_value(self.limits.swap_max),
            )?;
        }
        write(&leaf.join("cgroup.procs"), &pid.to_string())
    }

    /// `id`'s memory, or `None` when it has no leaf.
    pub fn stats(&self, id: Uuid) -> Option<Stats> {
        let leaf = self.leaf(id);
        let current = read_u64(&leaf.join("memory.current"));
        current?;
        let oom_kills = std::fs::read_to_string(leaf.join("memory.events"))
            .ok()
            .and_then(|events| {
                events.lines().find_map(|l| {
                    l.strip_prefix("oom_kill ")
                        .and_then(|v| v.trim().parse().ok())
                })
            })
            .unwrap_or(0);
        Some(Stats {
            current,
            peak: read_u64(&leaf.join("memory.peak")),
            max: read_u64(&leaf.join("memory.max")),
            oom_kills,
        })
    }

    /// Remove `id`'s leaf. Fails (and is kept) while a detached daemon from
    /// the session is still in it; [`Self::sweep`] retries.
    pub fn detach(&self, id: Uuid) -> bool {
        std::fs::remove_dir(self.leaf(id)).is_ok()
    }

    /// Remove every leaf that is empty and not a live session's.
    pub fn sweep(&self, live: &[Uuid]) {
        let Ok(entries) = std::fs::read_dir(&self.sessions_dir) else {
            return;
        };
        for entry in entries.flatten() {
            let Some(id) = entry
                .file_name()
                .to_str()
                .and_then(|n| Uuid::parse_str(n).ok())
            else {
                continue;
            };
            if !live.contains(&id) {
                let _ = std::fs::remove_dir(entry.path());
            }
        }
    }
}

static MANAGER: OnceLock<Option<Manager>> = OnceLock::new();

/// Set up per-session cgroups from the environment, once, at startup.
pub fn init() {
    MANAGER.get_or_init(|| {
        let policy = match Policy::from_env() {
            Ok(p) => p,
            Err(e) => {
                tracing::error!(error = %e, "per-session cgroups disabled");
                return None;
            }
        };
        if !policy.enabled {
            tracing::info!("per-session cgroups off (ENGINE_SESSION_CGROUP=off)");
            return None;
        }
        let own = match std::fs::read_to_string("/proc/self/cgroup")
            .ok()
            .as_deref()
            .and_then(own_cgroup_path)
        {
            Some(p) => p,
            None => {
                tracing::info!("per-session cgroups unavailable: no cgroup v2 hierarchy");
                return None;
            }
        };
        let dir = Path::new("/sys/fs/cgroup").join(own.trim_start_matches('/'));
        match Manager::setup(&dir, policy.limits) {
            Ok(m) => {
                tracing::info!(
                    cgroup = %dir.display(),
                    memory_max = %limit_value(policy.limits.memory_max),
                    memory_high = %limit_value(policy.limits.memory_high),
                    "per-session cgroups on"
                );
                Some(m)
            }
            Err(e) => {
                tracing::warn!(error = %e, "per-session cgroups unavailable; sessions share the pod cgroup");
                None
            }
        }
    });
}

/// The manager, when per-session cgroups are on.
pub fn manager() -> Option<&'static Manager> {
    MANAGER.get().and_then(Option::as_ref)
}

/// Put a freshly spawned session into its leaf. A failure is logged and the
/// session runs uncapped.
pub fn attach(id: Uuid, pid: Option<u32>) {
    let (Some(m), Some(pid)) = (manager(), pid) else {
        return;
    };
    if let Err(e) = m.attach(id, pid) {
        tracing::warn!(session = %id, error = %e, "session runs without its own cgroup");
    }
}

/// Drop an ended session's leaf (kept while a detached daemon remains).
pub fn detach(id: Uuid) {
    if let Some(m) = manager() {
        m.detach(id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            pairs
                .iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| v.to_string())
        }
    }

    #[test]
    fn the_default_policy_is_on_with_a_16_gib_cap_and_no_swap() {
        let p = Policy::from_lookup(env(&[])).unwrap();
        assert!(p.enabled);
        assert_eq!(p.limits, Limits::default());
        assert_eq!(p.limits.memory_max, Some(16 << 30));
        assert_eq!(p.limits.swap_max, Some(0));
    }

    #[test]
    fn limits_parse_sizes_max_and_off() {
        let p = Policy::from_lookup(env(&[
            ("ENGINE_SESSION_MEMORY_MAX", "8GiB"),
            ("ENGINE_SESSION_SWAP_MAX", "max"),
        ]))
        .unwrap();
        assert_eq!(p.limits.memory_max, Some(8 << 30));
        // high follows max unless set.
        assert_eq!(p.limits.memory_high, Some((8u64 << 30) / 10 * 9));
        assert_eq!(p.limits.swap_max, None);

        let p = Policy::from_lookup(env(&[
            ("ENGINE_SESSION_MEMORY_MAX", "max"),
            ("ENGINE_SESSION_CGROUP", "off"),
        ]))
        .unwrap();
        assert!(!p.enabled);
        assert_eq!(p.limits.memory_high, None);

        assert!(Policy::from_lookup(env(&[("ENGINE_SESSION_MEMORY_MAX", "lots")])).is_err());
        assert!(Policy::from_lookup(env(&[("ENGINE_SESSION_CGROUP", "maybe")])).is_err());
    }

    #[test]
    fn own_cgroup_is_the_v2_line() {
        assert_eq!(
            own_cgroup_path("12:memory:/v1\n0::/kubepods/pod1/abc\n").as_deref(),
            Some("/kubepods/pod1/abc")
        );
        assert_eq!(own_cgroup_path("0::/\n").as_deref(), Some("/"));
        assert_eq!(own_cgroup_path("4:cpu:/x\n"), None);
    }

    /// A plain directory standing in for a delegated cgroup: setup and attach
    /// write the files a kernel would act on.
    fn fake_cgroup(controllers: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("cgroup.controllers"), controllers).unwrap();
        std::fs::write(dir.path().join("cgroup.procs"), "1\n42\n").unwrap();
        dir
    }

    #[test]
    fn setup_moves_the_engine_and_delegates_memory() {
        let own = fake_cgroup("cpu memory pids");
        let m = Manager::setup(own.path(), Limits::default()).unwrap();
        let read = |p: &str| std::fs::read_to_string(own.path().join(p)).unwrap();
        assert_eq!(read("cgroup.subtree_control"), "+memory");
        assert_eq!(read("sessions/cgroup.subtree_control"), "+memory");
        // A plain file keeps only the last write; the kernel takes each pid.
        assert_eq!(read("engine/cgroup.procs"), "42");

        let id = Uuid::new_v4();
        std::fs::create_dir(own.path().join(format!("sessions/{id}"))).unwrap();
        std::fs::write(
            own.path().join(format!("sessions/{id}/memory.swap.max")),
            "",
        )
        .unwrap();
        m.attach(id, 4242).unwrap();
        let leaf = |f: &str| read(&format!("sessions/{id}/{f}"));
        assert_eq!(leaf("cgroup.procs"), "4242");
        assert_eq!(leaf("memory.max"), (16u64 << 30).to_string());
        assert_eq!(leaf("memory.high"), ((16u64 << 30) / 10 * 9).to_string());
        assert_eq!(leaf("memory.swap.max"), "0");
    }

    #[test]
    fn setup_refuses_without_the_memory_controller() {
        let own = fake_cgroup("cpu pids");
        let err = Manager::setup(own.path(), Limits::default()).unwrap_err();
        assert!(err.contains("memory controller"), "{err}");
    }

    #[test]
    fn setup_refuses_outside_a_cgroup() {
        let dir = tempfile::tempdir().unwrap();
        assert!(Manager::setup(dir.path(), Limits::default()).is_err());
    }

    #[test]
    fn stats_read_usage_peak_cap_and_oom_kills() {
        let own = fake_cgroup("memory");
        let m = Manager::setup(own.path(), Limits::default()).unwrap();
        let id = Uuid::new_v4();
        assert_eq!(m.stats(id), None);
        let leaf = own.path().join(format!("sessions/{id}"));
        std::fs::create_dir(&leaf).unwrap();
        std::fs::write(leaf.join("memory.current"), "1048576\n").unwrap();
        std::fs::write(leaf.join("memory.peak"), "2097152\n").unwrap();
        std::fs::write(leaf.join("memory.max"), "max\n").unwrap();
        std::fs::write(
            leaf.join("memory.events"),
            "low 0\nhigh 3\nmax 1\noom 1\noom_kill 2\noom_group_kill 0\n",
        )
        .unwrap();
        assert_eq!(
            m.stats(id),
            Some(Stats {
                current: Some(1 << 20),
                peak: Some(2 << 20),
                max: None,
                oom_kills: 2,
            })
        );
    }

    #[test]
    fn sweep_removes_empty_leaves_of_ended_sessions_only() {
        let own = fake_cgroup("memory");
        let m = Manager::setup(own.path(), Limits::default()).unwrap();
        let (live, ended) = (Uuid::new_v4(), Uuid::new_v4());
        for id in [live, ended] {
            std::fs::create_dir(own.path().join(format!("sessions/{id}"))).unwrap();
        }
        m.sweep(&[live]);
        assert!(own.path().join(format!("sessions/{live}")).exists());
        assert!(!own.path().join(format!("sessions/{ended}")).exists());
    }
}
