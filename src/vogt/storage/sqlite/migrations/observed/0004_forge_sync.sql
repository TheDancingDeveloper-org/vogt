-- 0004_forge_sync — incremental forge sync state (D1).
--
-- Two tables, both *observed-store* rather than declared, because neither is
-- a fact a person asserted: they are bookkeeping the sync keeps about itself.
-- A declared write needs an actor and a reason; a watermark has neither.
--
-- `work_links` already anchors forge subjects to declared work, so nothing
-- here re-states identity — these tables only carry "how far has the sync
-- got" and "when was this subject last confirmed to still exist".

-- The incremental watermark: the max upstream `updated_at` a collector has
-- seen for a project. The next sync asks the forge for everything changed
-- since (minus a small overlap), so a closure that happened between sweeps is
-- observed rather than missed. First of its kind — nothing persisted a
-- per-(collector, project) cursor before (D3).
CREATE TABLE sync_state (
    collector  TEXT NOT NULL,
    project_id TEXT NOT NULL,
    -- Max upstream `updated_at` seen, ISO-8601. Never the local clock: a
    -- clock-based cursor drops anything created upstream during the sweep.
    watermark  TEXT,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (collector, project_id)
);

-- When each subject was last *confirmed to still exist* upstream, touched on
-- every sync batch including the batches where nothing about the subject
-- changed. This is what resolves the #50 residual without mutating
-- `observations`: the evidence rows stay immutable and `observed_at` stays
-- first-seen, while "is this still fresh" gets its own mutable home (D1).
CREATE TABLE subject_seen (
    subject_key       TEXT PRIMARY KEY NOT NULL,
    last_confirmed_at TEXT NOT NULL
);

CREATE INDEX idx_subject_seen_confirmed ON subject_seen (last_confirmed_at);
