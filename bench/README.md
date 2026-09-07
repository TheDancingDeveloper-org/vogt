# Recorded numbers

`scripts/load.py` drives the real operation registry and records numbers — it
is not a pass/fail suite. This directory holds the **committed baselines**
those runs are compared against. Every number here is a dev-box measurement,
not a production SLA, and a number is only comparable to another taken the
same way: each report carries a `produced_by` label saying how it was
measured.

## `soak_baseline.json`

The recorded numbers for a sustained in-process soak: seed a base dataset,
then drive a steady operation mix (`work.create` → `work.update` →
`work.get` → `work.list` → `backlog` → `bugs`, with an offline `sweep`
interleaved every N iterations as the in-process stand-in for the live
collector schedule). Over the steady phase it records:

- **throughput** (`throughput_ops_per_s`) — successful calls per second,
- **latency** p50/p95/p99 per operation,
- **error rate** (`error_rate`) — failed calls over attempted, and
- **RSS** start / end / growth — the leak signal a soak exists to watch.

The run builds one `AppContext` over a temporary SQLite database and drives
the same handlers the CLI, REST and MCP surfaces reach — every mutating call
through `audited_write` with a principal and a reason. No server, no HTTP, no
Rust engine. Produced by:

```bash
uv run python scripts/load.py --mode soak --scale 1 \
  --iterations 200 --sweep-every 25 --seed 0 \
  --produced-by "in-process (dev box, scripts/load.py --mode soak)" \
  --out bench/soak_baseline.json
```

Concurrent WebSocket attach clients need the Rust engine and are out of scope
for this in-process generator.

## `deployed_baseline.json`

The soak is blind to anything that only shows **with auth on, over HTTP,
under concurrency** — the per-request auth write floor and event-loop
serialisation. The deployed-shape mode fills that gap: it seeds a dataset,
stands the **real server** up on a loopback port with `require_auth` on,
mints a bearer token, and drives the hot read surfaces (`work.list`,
`backlog`, `board.list`, `inbox.list`, `bugs`) and the write path
(`work.create`, `work.update`) with a pool of concurrent HTTP clients. It
records the same throughput / p50 / p95 / p99 / RSS shape as the soak, and
the same `compare_to_baseline` gate reads it the same way. Produced by:

```bash
uv run python scripts/load.py --mode deployed --scale 5 \
  --requests 2000 --concurrency 16 --seed 0 \
  --produced-by "dev box (uvicorn, require_auth)" \
  --out bench/deployed_baseline.json
```

To re-record either baseline on a different machine, run the same command
there with a `--produced-by` that says so and commit the result.

## Drift check

A later run compares itself to a baseline and fails on regression past the
2× rule — a per-operation p95 that grew past 2× baseline, or throughput that
fell past ½× baseline:

```bash
uv run python scripts/load.py --mode soak --scale 1 \
  --iterations 200 --sweep-every 25 \
  --check-baseline bench/soak_baseline.json
```

`--check-baseline` works the same way with `--mode deployed` against
`bench/deployed_baseline.json`. The run exits non-zero and prints the drifted
metrics when any regressed. It is deliberately **not** wired into `pytest`: a
wall-clock latency gate on a shared runner would be flaky, exactly the mistake
`tests/test_benchmark.py` documents avoiding. The pure comparison logic
(`compare_to_baseline`) is unit-tested instead.

## Browser rendering profile

`web/tests/browser/perf.spec.ts` is a documented release check using the
production PWA and demo transport. `?demoScale=2000` creates 2,000 work items,
5,000 files at depth 8, and 32 sessions, then measures board DOM size, cold
first interaction, long tasks, retained nodes, heap usage when Chromium
exposes it, and a visibility/focus wake cycle.

Run it on a named release machine:

```bash
cd web
VOGT_RUN_PERF=1 pnpm exec playwright test tests/browser/perf.spec.ts --project=desktop
```

`bench/web_perf_baseline.json` records the fixture sizes and actionable
budgets. Timing values are deliberately not a CI assertion: the release
check prints JSON for recording a comparable baseline without making shared
runner tests flaky.
