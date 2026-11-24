# WI-2673 read timings

Dev-sized fixture, not a production database: 64 projects, 1800 work items,
200 open drift proposals, 20 000 finished sweeps (about 6 KB of scope each),
8 000 latest observations and 2 000 dependency references. Times are one Rust
call after a warmup, debug profile, on that fixture.

| Operation | Before (ms) | After (ms) | Change |
|---|---:|---:|---|
| coverage | 7400 | 492 | newest finished sweep via `idx_sweeps_finished`; cumulative map flushes one identical-scope run |
| backlog | 1200–7400 | 371 | work items hydrated in two queries; projects loaded once per gather |
| projects/brief | 1200–7400 | 111 | same hydrate and one project load |
| drift | 1200–7400 | 83 | no query change; already indexed |
| deps | 1200–7400 | 110 | project slugs from one project load |
| place/metrics | 1200–7400 | 18 | backlog path uses the same gather cache |

Response JSON sizes matched the pre-change bench (coverage 1710, backlog
18594, brief 5615, drift 44716, deps 16302, place 212). The coverage query
returns the sweep Python keeps (greatest `finished_at`, then `id`, per
collector). `coverage_by_project` still walks finished sweeps in ascending
`(collector, finished_at, id)` order and overwrites the same way; a run of
identical scope text is applied once, at that run's last finished moment.
