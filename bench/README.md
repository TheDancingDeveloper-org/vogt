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
