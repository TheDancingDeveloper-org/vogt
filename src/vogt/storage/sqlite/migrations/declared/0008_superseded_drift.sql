-- 0008_superseded_drift — a proposal raised under evidence that no longer
-- says what it said.
--
-- WI-2's fix (#43) stopped `dep-refs` misclassifying Cargo dependency
-- inheritance as `unresolved_dependency`. The thirty-six proposals it had
-- already raised under the old logic stayed open through the fix, its
-- deploy, and a later regression and re-fix — and closed only because
-- somebody reconciled them against timestamps by hand, one `drift resolve
-- --reject` per proposal (#48). `drift detect` only ever adds: nothing
-- re-validates what is already on the board against current evidence.
--
-- This is not auto-close. FR-R2 says drift never silently mutates declared
-- data and FR-U18 says a person reviews evidence before resolving; a
-- proposal marked here is still open, still needs a human, and still carries
-- its original evidence snapshot. What changes is that the inbox can now
-- tell "still true" from "raised under evidence a later sweep no longer
-- reproduces", which is the distinction a reader was reconstructing from
-- timestamps by hand.
--
-- Both columns are nullable and clear again: a condition that reappears
-- un-marks the proposal, because a stale flag is worse than none.

ALTER TABLE drift_proposals ADD COLUMN superseded_at TEXT;
ALTER TABLE drift_proposals ADD COLUMN superseded_detail TEXT;

CREATE INDEX idx_drift_superseded ON drift_proposals (superseded_at)
    WHERE superseded_at IS NOT NULL;
