//! What each session's process tree is holding (WI-916).
//!
//! Visibility, not enforcement — per-session limits are a cgroup's job
//! (WI-895). The aim is that the session eating the node's memory is
//! identifiable from Vogt, before the host's OOM killer picks something at
//! random: a Soot run at 50 GiB, a leaking service at 60.
//!
//! Every [`INTERVAL`] the sampler reads `/proc/*/stat` **once** — parent pid,
//! CPU ticks and resident pages for every process on the host — and then
//! sums the subtree below each live session's PTY child. CPU is the ticks a
//! tree used since the previous sample, per process (a process that is new
//! since then counts its whole life, which is the interval at most), over
//! the wall time between samples. Processes that leave the tree by
//! double-forking to init are not seen, as in `ps --forest`.
//!
//! Linux only; elsewhere sessions simply carry no `resources`.

use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use uuid::Uuid;
use vogt_engine_contract::{SessionResources, SessionResourcesSample};

use crate::{app::AppState, events::ServerEvent};

/// How often every session is sampled.
pub const INTERVAL: Duration = Duration::from_secs(10);

/// One process, as `/proc/<pid>/stat` describes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Proc {
    pub ppid: u32,
    /// utime + stime, in clock ticks.
    pub ticks: u64,
    /// Resident set size, in pages.
    pub rss_pages: u64,
}

/// Parse one `/proc/<pid>/stat` line. The command name (field 2) is in
/// parentheses and may contain spaces and parentheses, so fields are counted
/// from after the last `)`: state is field 3, ppid 4, utime 14, stime 15,
/// rss 24.
pub fn parse_stat(line: &str) -> Option<Proc> {
    let rest = &line[line.rfind(')')? + 1..];
    let fields: Vec<&str> = rest.split_whitespace().collect();
    // fields[0] is field 3 (state), so field N is fields[N - 3].
    let at = |n: usize| fields.get(n - 3).and_then(|v| v.parse::<u64>().ok());
    Some(Proc {
        ppid: at(4)? as u32,
        ticks: at(14)?.saturating_add(at(15)?),
        rss_pages: at(24)?,
    })
}

/// Every process on the host, read once.
pub fn read_table() -> HashMap<u32, Proc> {
    let mut table = HashMap::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return table;
    };
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };
        if let Some(proc) = std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .ok()
            .and_then(|line| parse_stat(&line))
        {
            table.insert(pid, proc);
        }
    }
    table
}

/// `root` and every process below it in `table`.
pub fn subtree(
    table: &HashMap<u32, Proc>,
    children: &HashMap<u32, Vec<u32>>,
    root: u32,
) -> Vec<u32> {
    if !table.contains_key(&root) {
        return Vec::new();
    }
    let mut out = vec![root];
    let mut seen = std::collections::HashSet::from([root]);
    let mut i = 0;
    while i < out.len() {
        if let Some(kids) = children.get(&out[i]) {
            for &kid in kids {
                if seen.insert(kid) {
                    out.push(kid);
                }
            }
        }
        i += 1;
    }
    out
}

pub fn children_of(table: &HashMap<u32, Proc>) -> HashMap<u32, Vec<u32>> {
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for (&pid, proc) in table {
        children.entry(proc.ppid).or_default().push(pid);
    }
    children
}

/// The previous sample of one session, per process, for CPU deltas.
#[derive(Debug, Default)]
pub struct Previous {
    ticks: HashMap<u32, u64>,
    at: Option<Instant>,
}

/// Sum one session's tree, with CPU against `previous` (updated in place).
#[allow(clippy::too_many_arguments)]
pub fn measure(
    table: &HashMap<u32, Proc>,
    children: &HashMap<u32, Vec<u32>>,
    root: u32,
    previous: &mut Previous,
    now: Instant,
    page_size: u64,
    ticks_per_sec: u64,
    warn_bytes: Option<u64>,
    sampled_at: String,
) -> Option<SessionResources> {
    let pids = subtree(table, children, root);
    if pids.is_empty() {
        return None;
    }
    let mut rss_pages = 0u64;
    let mut used_ticks = 0u64;
    let mut ticks = HashMap::with_capacity(pids.len());
    for pid in &pids {
        let proc = table[pid];
        rss_pages = rss_pages.saturating_add(proc.rss_pages);
        let before = previous.ticks.get(pid).copied().unwrap_or(0);
        used_ticks = used_ticks.saturating_add(proc.ticks.saturating_sub(before));
        ticks.insert(*pid, proc.ticks);
    }
    let cpu_pct = match previous.at {
        Some(at) if ticks_per_sec > 0 => {
            let wall = now.duration_since(at).as_secs_f64();
            if wall > 0.0 {
                (used_ticks as f64 / ticks_per_sec as f64 / wall * 100.0) as f32
            } else {
                0.0
            }
        }
        // The first sample has nothing to compare with.
        _ => 0.0,
    };
    previous.ticks = ticks;
    previous.at = Some(now);
    let rss_bytes = rss_pages.saturating_mul(page_size);
    Some(SessionResources {
        rss_bytes,
        cpu_pct: (cpu_pct * 10.0).round() / 10.0,
        processes: pids.len() as u32,
        sampled_at,
        over_threshold: warn_bytes.is_some_and(|w| rss_bytes >= w),
    })
}

/// Resident memory of `root` and everything under it, in bytes: one read of
/// `/proc`, for a caller that watches a single process tree (quick chats).
pub fn tree_rss_bytes(root: u32) -> u64 {
    let table = read_table();
    let children = children_of(&table);
    subtree(&table, &children, root)
        .iter()
        .filter_map(|pid| table.get(pid))
        .map(|proc| proc.rss_pages)
        .sum::<u64>()
        .saturating_mul(sysconf(libc::_SC_PAGESIZE, 4096))
}

fn sysconf(name: libc::c_int, fallback: u64) -> u64 {
    // SAFETY: sysconf has no preconditions.
    let value = unsafe { libc::sysconf(name) };
    if value > 0 {
        value as u64
    } else {
        fallback
    }
}

/// The sampler's state between rounds: each session's previous per-process
/// ticks, for CPU.
pub struct Sampler {
    previous: HashMap<Uuid, Previous>,
    /// Each live session's `project` label, worked out once from its cwd.
    projects: HashMap<Uuid, Option<String>>,
    page_size: u64,
    ticks_per_sec: u64,
    warn_bytes: Option<u64>,
}

impl Sampler {
    pub fn new(warn_bytes: Option<u64>) -> Self {
        Self {
            previous: HashMap::new(),
            projects: HashMap::new(),
            page_size: sysconf(libc::_SC_PAGESIZE, 4096),
            ticks_per_sec: sysconf(libc::_SC_CLK_TCK, 100),
            warn_bytes,
        }
    }

    /// One round: measure every live session, keep the result on the session
    /// (it rides on its summary) and publish one `session-resources` event.
    pub async fn run_once(&mut self, state: &AppState) {
        let sessions: Vec<_> = state
            .sessions
            .live_sessions()
            .into_iter()
            .filter(|s| s.is_alive())
            .filter_map(|s| s.pid().map(|pid| (s, pid)))
            .collect();
        self.previous
            .retain(|id, _| sessions.iter().any(|(s, _)| s.id == *id));
        self.projects
            .retain(|id, _| sessions.iter().any(|(s, _)| s.id == *id));
        let Ok(table) = tokio::task::spawn_blocking(read_table).await else {
            return;
        };
        let pod_rss = table
            .values()
            .map(|proc| proc.rss_pages)
            .sum::<u64>()
            .saturating_mul(self.page_size);
        crate::metrics::metrics()
            .pod_rss
            .replace([(Vec::new(), pod_rss as f64)]);
        if sessions.is_empty() {
            publish_session_gauges(&[]);
            return;
        }
        let children = children_of(&table);
        let now = Instant::now();
        let sampled_at = time::OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_default();
        let mut samples = Vec::with_capacity(sessions.len());
        let mut gauges = Vec::with_capacity(sessions.len());
        for (session, pid) in sessions {
            let measured = measure(
                &table,
                &children,
                pid,
                self.previous.entry(session.id).or_default(),
                now,
                self.page_size,
                self.ticks_per_sec,
                self.warn_bytes,
                sampled_at.clone(),
            );
            session.set_resources(measured.clone());
            if let Some(resources) = measured {
                let project = self
                    .projects
                    .entry(session.id)
                    .or_insert_with(|| project_of(std::path::Path::new(&session.cwd)))
                    .clone();
                gauges.push((
                    session_labels(
                        session.id,
                        session.template(),
                        session.agent(),
                        crate::pty::launched_model(&session),
                        project,
                    ),
                    resources.clone(),
                ));
                samples.push(SessionResourcesSample {
                    id: session.id,
                    resources,
                });
            }
        }
        publish_session_gauges(&gauges);
        if !samples.is_empty() {
            state.bus.publish(ServerEvent::SessionResources { samples });
        }
    }
}

/// The labels one session's resource gauges carry (WI-1373). A missing
/// value reads `unknown`, so every series has the same label names.
pub fn session_labels(
    id: Uuid,
    template: Option<String>,
    agent: Option<String>,
    model: Option<String>,
    project: Option<String>,
) -> crate::metrics::Labels {
    let or_unknown = |v: Option<String>| v.unwrap_or_else(|| "unknown".to_string());
    vec![
        ("session", id.to_string()),
        ("template", or_unknown(template)),
        ("agent", or_unknown(agent)),
        ("model", or_unknown(model)),
        ("project", or_unknown(project)),
    ]
}

/// Replace the per-session gauges with this round's samples, and the pod
/// total of them. A session not sampled this round — stopped, hibernated,
/// exited — has no series after it.
pub fn publish_session_gauges(gauges: &[(crate::metrics::Labels, SessionResources)]) {
    let metrics = crate::metrics::metrics();
    metrics.session_rss.replace(
        gauges
            .iter()
            .map(|(labels, r)| (labels.clone(), r.rss_bytes as f64)),
    );
    metrics.session_cpu.replace(
        gauges
            .iter()
            .map(|(labels, r)| (labels.clone(), f64::from(r.cpu_pct))),
    );
    metrics.session_processes.replace(
        gauges
            .iter()
            .map(|(labels, r)| (labels.clone(), f64::from(r.processes))),
    );
    let total: u64 = gauges.iter().map(|(_, r)| r.rss_bytes).sum();
    metrics
        .sessions_rss_total
        .replace([(Vec::new(), total as f64)]);
}

/// The repository a session works in, named after its main checkout: a
/// linked worktree (`WI-1373-…`) is named after the repository whose
/// `.git/worktrees` it is registered in, so every worker on one repository
/// shares one `project` label. The core's project slug is not known to the
/// engine; for the estate's checkouts the directory name is that slug.
pub fn project_of(cwd: &std::path::Path) -> Option<String> {
    for dir in cwd.ancestors().take(4) {
        let dot_git = dir.join(".git");
        if dot_git.is_dir() {
            return dir.file_name()?.to_str().map(str::to_string);
        }
        if let Ok(text) = std::fs::read_to_string(&dot_git) {
            let gitdir = text.trim().strip_prefix("gitdir:")?.trim();
            let gitdir = std::path::Path::new(gitdir);
            // `<repo>/.git/worktrees/<name>`, or a bare `<repo>.git/worktrees/<name>`.
            let common = gitdir.parent()?.parent()?;
            let repo = if common.file_name()? == ".git" {
                common.parent()?.file_name()?
            } else {
                common.file_name()?
            };
            return repo
                .to_str()
                .map(|r| r.strip_suffix(".git").unwrap_or(r).to_string());
        }
    }
    None
}

/// Sample every live session every [`INTERVAL`].
pub fn spawn_sampler(state: Arc<AppState>) {
    if !cfg!(target_os = "linux") {
        return;
    }
    tokio::spawn(async move {
        let mut sampler = Sampler::new(state.config.session_rss_warn_bytes);
        let mut ticker = tokio::time::interval(INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            sampler.run_once(&state).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proc(ppid: u32, ticks: u64, rss_pages: u64) -> Proc {
        Proc {
            ppid,
            ticks,
            rss_pages,
        }
    }

    #[test]
    fn a_stat_line_with_a_hostile_name_still_parses() {
        // `(evil) R 1 (x)` as a command name; the fields are read after the
        // last `)`.
        let line = "4242 (evil) R 1 (x)) S 77 4242 4242 0 -1 4194560 100 0 0 0 \
                    150 50 0 0 20 0 3 0 12345 104857600 2560 18446744073709551615";
        assert_eq!(parse_stat(line), Some(proc(77, 200, 2560)));
        assert_eq!(parse_stat("garbage"), None);
    }

    #[test]
    fn a_tree_sums_its_processes_and_cpu_is_the_delta_over_wall_time() {
        let mut table = HashMap::from([
            (10, proc(1, 1000, 100)),  // the session's PTY child
            (11, proc(10, 500, 1000)), // the agent
            (12, proc(11, 0, 50)),     // an MCP server
            (20, proc(1, 9999, 9999)), // somebody else
        ]);
        let mut previous = Previous::default();
        let t0 = Instant::now();
        let first = measure(
            &table,
            &children_of(&table),
            10,
            &mut previous,
            t0,
            4096,
            100,
            Some(4096 * 1000),
            "t0".into(),
        )
        .unwrap();
        assert_eq!(first.rss_bytes, 1150 * 4096);
        assert_eq!(first.processes, 3);
        assert_eq!(first.cpu_pct, 0.0, "no previous sample to compare with");
        assert!(first.over_threshold);

        // 10 s later the agent used 1500 ticks (15 CPU-seconds) and a new
        // tool process appeared with 100.
        table.get_mut(&11).unwrap().ticks += 1500;
        table.insert(13, proc(11, 100, 10));
        let second = measure(
            &table,
            &children_of(&table),
            10,
            &mut previous,
            t0 + Duration::from_secs(10),
            4096,
            100,
            None,
            "t1".into(),
        )
        .unwrap();
        assert_eq!(second.processes, 4);
        assert_eq!(second.cpu_pct, 160.0, "16 CPU-seconds over 10 s");
        assert!(!second.over_threshold);
    }

    #[test]
    fn a_gone_root_measures_nothing() {
        let table = HashMap::from([(10, proc(1, 0, 1))]);
        assert!(measure(
            &table,
            &children_of(&table),
            99,
            &mut Previous::default(),
            Instant::now(),
            4096,
            100,
            None,
            String::new()
        )
        .is_none());
    }

    #[test]
    fn the_running_test_process_measures_itself() {
        let table = read_table();
        let me = std::process::id();
        assert!(table.contains_key(&me));
        let measured = measure(
            &table,
            &children_of(&table),
            me,
            &mut Previous::default(),
            Instant::now(),
            4096,
            100,
            None,
            String::new(),
        )
        .unwrap();
        assert!(measured.rss_bytes > 0);
    }

    #[test]
    fn a_project_is_named_after_its_main_checkout() {
        let root = tempfile::tempdir().unwrap();
        let main = root.path().join("vogt");
        std::fs::create_dir_all(main.join(".git/worktrees/WI-1-x")).unwrap();
        std::fs::create_dir_all(main.join("engine/server")).unwrap();
        assert_eq!(project_of(&main).as_deref(), Some("vogt"));
        assert_eq!(
            project_of(&main.join("engine/server")).as_deref(),
            Some("vogt")
        );

        let worktree = root.path().join("WI-1-x");
        std::fs::create_dir_all(worktree.join("engine")).unwrap();
        std::fs::write(
            worktree.join(".git"),
            format!("gitdir: {}\n", main.join(".git/worktrees/WI-1-x").display()),
        )
        .unwrap();
        assert_eq!(project_of(&worktree).as_deref(), Some("vogt"));
        assert_eq!(
            project_of(&worktree.join("engine")).as_deref(),
            Some("vogt")
        );

        assert_eq!(project_of(&root.path().join("nowhere")), None);
    }

    #[test]
    fn session_gauges_carry_their_labels_and_drop_ended_sessions() {
        let resources = |rss_bytes| SessionResources {
            rss_bytes,
            cpu_pct: 12.5,
            processes: 3,
            sampled_at: String::new(),
            over_threshold: false,
        };
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let labels_a = session_labels(
            a,
            Some("klaudia".into()),
            Some("klaudia".into()),
            Some("opus".into()),
            Some("vogt".into()),
        );
        let labels_b = session_labels(b, None, None, None, None);
        publish_session_gauges(&[
            (labels_a, resources(1 << 30)),
            (labels_b, resources(1 << 20)),
        ]);
        let out = crate::metrics::metrics().render();
        assert!(
            out.contains(&format!(
                "vogt_session_rss_bytes{{session=\"{a}\",template=\"klaudia\",agent=\"klaudia\",model=\"opus\",project=\"vogt\"}} 1073741824"
            )),
            "{out}"
        );
        assert!(
            out.contains(&format!(
                "vogt_session_cpu_percent{{session=\"{b}\",template=\"unknown\",agent=\"unknown\",model=\"unknown\",project=\"unknown\"}} 12.5"
            )),
            "{out}"
        );
        assert!(out.contains("vogt_session_processes{"), "{out}");

        // `a` stopped: the next round has only `b`.
        publish_session_gauges(&[(session_labels(b, None, None, None, None), resources(5))]);
        let out = crate::metrics::metrics().render();
        assert!(!out.contains(&a.to_string()), "{out}");
        assert!(out.contains(&b.to_string()), "{out}");
    }
}
