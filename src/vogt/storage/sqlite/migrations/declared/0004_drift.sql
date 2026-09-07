-- 0004_drift — machine-generated proposals, human- or agent-resolved.
--
-- Drift never silently mutates declared data (FR-R2). It produces a
-- proposal: "issue #42 closed upstream → close WI-118?" Somebody accepts,
-- rejects, or leaves it contested, and that resolution is an ordinary
-- audited write.
--
-- `evidence_snapshot` is the self-contained copy taken at raise time
-- (FR-R5): the observation's payload digest, its subject key, its
-- observed-at, and enough of the payload to explain the proposal without the
-- observed store. `evidence_observation_id` still points at the live row,
-- and retention refuses to prune anything a proposal references. A proposal
-- must never outlive its evidence — and because the two stores are pruned
-- and restored independently, "must never" has to mean both.

CREATE TABLE drift_proposals (
    id                      TEXT PRIMARY KEY NOT NULL,
    kind                    TEXT NOT NULL,
    subject_kind            TEXT NOT NULL,
    subject_id              TEXT NOT NULL,
    project_id              TEXT REFERENCES projects (id),
    summary                 TEXT NOT NULL,
    evidence_observation_id TEXT,
    evidence_snapshot       TEXT NOT NULL DEFAULT '{}',
    proposed_change         TEXT NOT NULL DEFAULT '{}',
    status                  TEXT NOT NULL CHECK (
        status IN ('open', 'accepted', 'rejected', 'contested')
    ),
    opened_at               TEXT NOT NULL,
    resolved_by_actor_id    TEXT REFERENCES actors (id),
    resolved_at             TEXT,
    resolution_reason       TEXT
);

CREATE INDEX idx_drift_status ON drift_proposals (status, kind);
CREATE INDEX idx_drift_subject ON drift_proposals (subject_kind, subject_id);
CREATE INDEX idx_drift_evidence ON drift_proposals (evidence_observation_id);

-- One open proposal per (kind, subject). Re-running the drift engine over
-- unchanged state must not stack duplicates: a proposal is a question, and
-- asking it twice is noise. Resolved rows stay, so the same drift can be
-- raised again after somebody has dealt with it once.
CREATE UNIQUE INDEX idx_drift_one_open
    ON drift_proposals (kind, subject_kind, subject_id)
    WHERE status = 'open';
