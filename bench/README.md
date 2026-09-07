# Recorded numbers (#297)

`scripts/load.py` drives the real operation registry and records numbers — it
is not a pass/fail suite. This directory holds the **committed baseline** those
runs are compared against.

## `soak_baseline.json`

The recorded numbers for a sustained soak: seed a base dataset, then drive a
steady operation mix (`work.create` → `work.update` → `work.get` →
`work.list` → `backlog` → `bugs`, with an offline `sweep` interleaved on a
cadence as the in-process stand-in for the live collector schedule) for N
iterations. It records, over the steady phase:

- **throughput** (`throughput_ops_per_s`) — successful calls per second,
- **latency** p50/p95/p99 per operation,
- **error rate** (`error_rate`) — failed calls over attempted, and
- **RSS** start / end / growth — the leak signal a soak exists to watch.

### How these numbers were produced

The committed file is an **in-process** measurement, produced by:

```bash
uv run python scripts/load.py --mode soak --scale 1 \
  --iterations 200 --sweep-every 25 --seed 0 \
  --produced-by "in-process (dev box, scripts/load.py --mode soak)" \
  --out bench/soak_baseline.json
```

It builds one `AppContext` over a temporary SQLite database and drives the
same handlers the CLI, REST and MCP surfaces reach — every mutating call
through `audited_write` with a principal and a reason. No server, no HTTP, no
Rust engine.

`produced_by` records that provenance in the file itself, because **a soak
number is only comparable to another taken the same way.** These are dev-box
numbers, not a production SLA.

### What remains — the authoritative run

The issue's full soak is an **S-hour** run with the live scheduler, and its
numbers should come from the **self-hosted runner** against a stood-up stack,
not a laptop. That run is deferred (it needs the runner, like #295): when it
lands, it re-records this file with `--produced-by "runner …"` and a real
`--iterations` / duration, and *that* becomes the authoritative baseline. The
committed in-process numbers are the starting point a drift check can already
run against today.

The K concurrent WebSocket attach clients from the issue still need the Rust
engine and remain out of scope for this in-process generator.

## `deployed_baseline.json` (#540)

The soak above is blind to the top regressions the holistic review found: they
only exist with **auth on, over HTTP, under concurrency**, none of which an
in-process soak of one caller has. The deployed-shape mode fills that gap. It
seeds a dataset, stands the **real server** up on a loopback port with
`require_auth` on, mints a bearer token, and drives the hot read surfaces
(`work.list`, `backlog`, `board.list`, `inbox.list`, `bugs`) and the write path
(`work.create`, `work.update`) with a pool of concurrent HTTP clients — so the
per-request auth write floor (#526) and event-loop serialization (#525) are in
the numbers. It records the same throughput / p50 / p95 / p99 / RSS shape as the
soak, and the same `compare_to_baseline` gate reads it the same way.

```bash
uv run python scripts/load.py --mode deployed --scale 5 \
  --requests 2000 --concurrency 16 --seed 0 \
  --produced-by "runner (self-hosted, uvicorn, require_auth)" \
  --out bench/deployed_baseline.json
```

`.github/workflows/bench-deployed.yml` runs this nightly on the self-hosted
runner and via `workflow_dispatch`, and fails on drift past the 2× rule.

**The committed file is a dev-box starting point.** A deployed number is only
comparable to another with the same `produced_by`, and the nightly gate runs on
the self-hosted runner — so before the gate is authoritative, re-record the
baseline **on the runner**: dispatch the workflow with `record: true`, download
the `deployed-report` artifact, and commit it as `bench/deployed_baseline.json`.
This is the same runner-authoritative handoff the soak's "authoritative run"
note describes, now with a pipeline to do it.

## Drift check

A later run compares itself to a baseline and fails on regression past the
issue's 2× rule (a p95 that grew past 2× baseline, or throughput that fell
past ½× baseline):

```bash
uv run python scripts/load.py --mode soak --scale 1 \
  --iterations 200 --sweep-every 25 \
  --check-baseline bench/soak_baseline.json
```

It exits non-zero and prints the drifted metrics when any regressed. It is a
tool for the nightly job, deliberately **not** wired into `pytest`: a
wall-clock latency gate on a shared runner would be flaky, exactly the mistake
`tests/test_benchmark.py` documents avoiding. The pure comparison logic
(`compare_to_baseline`) is unit-tested instead.

## Browser rendering profile (#422)

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
