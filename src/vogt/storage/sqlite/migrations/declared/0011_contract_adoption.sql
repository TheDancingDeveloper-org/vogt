-- 0011_contract_adoption — the contract is something a project opts into,
-- and a criterion it cannot meet is not a criterion it failed
-- (FR-G16, FR-G19, r14).
--
-- Two columns' worth of change, and both are about the same asymmetry. A
-- project Vogt created got the scaffold; a project Vogt was handed got a
-- verdict — `non_compliant`, the only word available, and therefore a word
-- carrying no information once every project in the estate had it.
--
-- `contract_adopted_at` is null for every existing row, which is the correct
-- reading of history: nobody was ever asked. A project that has not adopted
-- reports `not_applicable` rather than a fault, and the reporting layer is
-- where that translation happens — the column below records the *fact* of
-- adoption and nothing else. `compliance_status` keeps its CHECK constraint
-- and its three values, because it stores what a check found, not what a
-- reader should be told about a project that never asked for one.
ALTER TABLE projects ADD COLUMN contract_adopted_at TEXT;

-- A criterion a project cannot meet by construction: a Cargo workspace has
-- no root `src/`, and calling that a failure is a statement about the
-- contract rather than about the project.
--
-- Rows here are declarations, not inferences. Nothing writes to this table
-- from a heuristic about the ecosystem: somebody — a person or an agent —
-- says a criterion does not apply and says why, and that is an audited write
-- like every other. The reason is NOT NULL and non-blank for the reason the
-- audit table's own reason is: an exemption with no explanation is the
-- silent one this exists to prevent.
CREATE TABLE contract_exemptions (
    id          TEXT PRIMARY KEY NOT NULL,
    project_id  TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    rule        TEXT NOT NULL,
    target      TEXT NOT NULL,
    reason      TEXT NOT NULL CHECK (length(trim(reason)) > 0),
    declared_by TEXT NOT NULL,
    declared_at TEXT NOT NULL,
    UNIQUE (project_id, rule, target)
);

CREATE INDEX contract_exemptions_project
    ON contract_exemptions (project_id);
