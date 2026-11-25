//! The pod-memory warning (WI-4752).
//!
//! The pod agent's heartbeat carries the pod's cgroup memory (WI-4749). A
//! [`MemoryWatch`] turns a session's readings into at most one notice per
//! crossing of 80% of `memory_max` (re-armed below 70%) and one notice per
//! increase of `oom_kill`. It is a pure state machine: the sampler owns one per
//! session and decides how a notice reaches the agent.

use vogt_engine_contract::SessionResources;
use vogt_session_agent::CgroupMemory;

use crate::agent_stream::PodMemory;

/// A reading at or over this share of `memory_max` warns.
pub const WARN_PERCENT: u128 = 80;
/// The warning re-arms once a reading falls below this share.
pub const REARM_PERCENT: u128 = 70;

/// Something the agent should be told.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryNotice {
    NearLimit { current: u64, max: u64 },
    OomKill { killed: u64, total: u64 },
}

impl MemoryNotice {
    pub fn text(&self) -> String {
        match self {
            Self::NearLimit { current, max } => format!(
                "[vogt] Pod memory is at {} MiB of the {} MiB limit ({}%+). Free memory \
                 (stop dev servers, builds and test runs you don't need) before the pod is \
                 OOM-killed.",
                current >> 20,
                max >> 20,
                WARN_PERCENT
            ),
            Self::OomKill { killed, total } => format!(
                "[vogt] The kernel OOM-killed {killed} more process(es) in this pod \
                 ({total} in total). Something you started may be gone; check before \
                 continuing."
            ),
        }
    }
}

/// One session's memory-warning state.
#[derive(Debug, Default)]
pub struct MemoryWatch {
    warned: bool,
    oom_seen: Option<u64>,
}

impl MemoryWatch {
    /// The GUI flag: a warning has fired and the pod has not dropped back
    /// below the re-arm level since.
    pub fn warned(&self) -> bool {
        self.warned
    }

    /// Take one reading. Seeing the same reading again emits nothing, so the
    /// sampler may call this every round. A missing or `max` limit never
    /// warns; the first `oom_kill` reading is a baseline, not an increase.
    pub fn observe(&mut self, memory: &CgroupMemory) -> Vec<MemoryNotice> {
        let mut notices = Vec::new();
        if let (Some(current), Some(max)) = (memory.memory_current, memory.memory_max) {
            if max > 0 {
                self.cross(current, max, &mut notices);
            }
        }
        if let Some(total) = memory.oom_kill {
            if let Some(seen) = self.oom_seen {
                if total > seen {
                    notices.push(MemoryNotice::OomKill {
                        killed: total - seen,
                        total,
                    });
                }
            }
            self.oom_seen = Some(total);
        }
        notices
    }

    fn cross(&mut self, current: u64, max: u64, notices: &mut Vec<MemoryNotice>) {
        let (current_x, max_x) = (u128::from(current) * 100, u128::from(max));
        if current_x >= max_x * WARN_PERCENT {
            if !self.warned {
                self.warned = true;
                notices.push(MemoryNotice::NearLimit { current, max });
            }
        } else if current_x < max_x * REARM_PERCENT {
            self.warned = false;
        }
    }
}

/// Copy a pod's memory reading, and the watch's flag, onto a resources sample.
pub fn apply(resources: &mut SessionResources, memory: Option<&PodMemory>, warned: bool) {
    let Some(pod) = memory else { return };
    resources.memory_current = pod.memory.memory_current;
    resources.memory_max = pod.memory.memory_max;
    resources.memory_peak = pod.memory.memory_peak;
    resources.oom_kill = pod.memory.oom_kill;
    resources.observed_at = Some(pod.observed_at.clone());
    resources.memory_warning = warned;
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1 << 30;

    fn reading(current: u64, max: Option<u64>, oom: Option<u64>) -> CgroupMemory {
        CgroupMemory {
            memory_current: Some(current),
            memory_max: max,
            memory_peak: None,
            oom_kill: oom,
        }
    }

    /// A scripted heartbeat sequence against a 10 GiB limit.
    #[test]
    fn one_notice_per_crossing_with_hysteresis() {
        let max = Some(10 * GIB);
        let mut w = MemoryWatch::default();
        assert!(w.observe(&reading(7 * GIB, max, None)).is_empty());
        assert!(!w.warned());
        // Crossing 80%: exactly one notice and the flag.
        let n = w.observe(&reading(8 * GIB, max, None));
        assert_eq!(
            n,
            vec![MemoryNotice::NearLimit {
                current: 8 * GIB,
                max: 10 * GIB
            }]
        );
        assert!(w.warned());
        // Still above: nothing.
        assert!(w.observe(&reading(9 * GIB, max, None)).is_empty());
        assert!(w.observe(&reading(9 * GIB, max, None)).is_empty());
        // Between 70% and 80%: not re-armed yet, and a new crossing is silent.
        assert!(w.observe(&reading(75 * GIB / 10, max, None)).is_empty());
        assert!(w.warned());
        assert!(w.observe(&reading(8 * GIB, max, None)).is_empty());
        // Below 70%: re-armed, flag clear; the next crossing warns again.
        assert!(w.observe(&reading(6 * GIB, max, None)).is_empty());
        assert!(!w.warned());
        assert_eq!(w.observe(&reading(9 * GIB, max, None)).len(), 1);
    }

    #[test]
    fn an_unknown_limit_never_warns() {
        let mut w = MemoryWatch::default();
        assert!(w.observe(&reading(900 * GIB, None, None)).is_empty());
        assert!(!w.warned());
        // A zero limit is not a limit either.
        assert!(w.observe(&reading(GIB, Some(0), None)).is_empty());
        assert!(!w.warned());
    }

    #[test]
    fn an_oom_kill_increase_notices_once() {
        let max = Some(10 * GIB);
        let mut w = MemoryWatch::default();
        // The first reading is a baseline, even when it is already above 0.
        assert!(w.observe(&reading(GIB, max, Some(2))).is_empty());
        assert!(w.observe(&reading(GIB, max, Some(2))).is_empty());
        assert_eq!(
            w.observe(&reading(GIB, max, Some(3))),
            vec![MemoryNotice::OomKill {
                killed: 1,
                total: 3
            }]
        );
        assert!(w.observe(&reading(GIB, max, Some(3))).is_empty());
        // A heartbeat without the counter changes nothing.
        assert!(w.observe(&reading(GIB, max, None)).is_empty());
        assert_eq!(w.observe(&reading(GIB, max, Some(5))).len(), 1);
    }

    #[test]
    fn apply_exposes_the_reading_with_its_freshness_and_the_flag() {
        let mut resources = SessionResources {
            rss_bytes: 1,
            cpu_pct: 0.0,
            processes: 1,
            sampled_at: "s".into(),
            over_threshold: false,
            memory_current: None,
            memory_max: None,
            memory_peak: None,
            oom_kill: None,
            observed_at: None,
            memory_warning: false,
        };
        apply(&mut resources, None, true);
        assert_eq!(resources.observed_at, None);
        assert!(!resources.memory_warning);
        let pod = PodMemory {
            memory: reading(9 * GIB, Some(10 * GIB), Some(1)),
            observed_at: "2026-01-01T00:00:00Z".into(),
        };
        apply(&mut resources, Some(&pod), true);
        assert_eq!(resources.memory_current, Some(9 * GIB));
        assert_eq!(resources.memory_max, Some(10 * GIB));
        assert_eq!(resources.oom_kill, Some(1));
        assert_eq!(
            resources.observed_at.as_deref(),
            Some("2026-01-01T00:00:00Z")
        );
        assert!(resources.memory_warning);
    }

    #[test]
    fn notice_text_names_the_figures() {
        let text = MemoryNotice::NearLimit {
            current: 8 * GIB,
            max: 10 * GIB,
        }
        .text();
        assert!(
            text.contains("8192 MiB") && text.contains("10240 MiB"),
            "{text}"
        );
    }
}
