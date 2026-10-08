//! The conflict policy an import follows. Ports `src/vogt/core/merge.py`.
//!
//! Without a baseline nothing can tell "changed here" from "changed there", so
//! every difference is a conflict rather than a guess. The policy never
//! resolves a difference it cannot explain.

#![allow(dead_code)]

use crate::core::Moment;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeDecision {
    Unchanged,
    TakeIncoming,
    KeepTarget,
    Conflict,
}

pub fn decide(
    equal: bool,
    base: Option<Moment>,
    target_updated_at: Moment,
    incoming_updated_at: Moment,
) -> MergeDecision {
    if equal {
        return MergeDecision::Unchanged;
    }
    let Some(base) = base else {
        return MergeDecision::Conflict;
    };
    let target_changed = target_updated_at > base;
    let incoming_changed = incoming_updated_at > base;
    match (incoming_changed, target_changed) {
        (true, false) => MergeDecision::TakeIncoming,
        (false, true) => MergeDecision::KeepTarget,
        _ => MergeDecision::Conflict,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(seconds: i64) -> Moment {
        Moment::from_unix(seconds, 0)
    }

    #[test]
    fn the_table_in_the_module_doc() {
        let base = at(100);
        assert_eq!(
            decide(true, None, at(200), at(300)),
            MergeDecision::Unchanged
        );
        assert_eq!(
            decide(false, None, at(200), at(300)),
            MergeDecision::Conflict
        );
        assert_eq!(
            decide(false, Some(base), at(100), at(200)),
            MergeDecision::TakeIncoming
        );
        assert_eq!(
            decide(false, Some(base), at(200), at(100)),
            MergeDecision::KeepTarget
        );
        // Both changed, and neither changed: each is a difference the baseline
        // cannot explain.
        assert_eq!(
            decide(false, Some(base), at(200), at(300)),
            MergeDecision::Conflict
        );
        assert_eq!(
            decide(false, Some(base), at(100), at(100)),
            MergeDecision::Conflict
        );
    }
}
