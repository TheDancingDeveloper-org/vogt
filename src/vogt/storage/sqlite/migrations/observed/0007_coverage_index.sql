-- Coverage reads every finished sweep. `coverage` keeps only the newest
-- completed row per collector; `coverage_by_project` walks every finished
-- scope in (collector, finished_at, id) order. Neither query was covered by
-- idx_sweeps_collector, which is (collector, started_at).

CREATE INDEX idx_sweeps_finished ON sweeps (collector, finished_at, id);
