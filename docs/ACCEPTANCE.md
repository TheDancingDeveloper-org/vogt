# The live acceptance suite

`tests/acceptance/` answers one question: **is this Vogt instance safe and
useful to work in today?** It is a black-box suite. It speaks the published
surfaces — `/healthz`, `/api/status`, the MCP JSON-RPC endpoint, the PWA and a
read-only Kubernetes context — and asserts common functionality, not
implementation details. It is not a replacement for `pytest`, the engine
tests or the Playwright suite; it is the end-to-end check those cannot make.

WI-4135. The requirement list it reports against is
`tests/acceptance/matrix.toml`, taken from the 2026-11-12 vogt-dev functional
coverage note.

## Run it

```console
# Read-only. The token is read from the file only (chmod 600 it first).
python3 tests/acceptance/run.py \
  --base-url https://vogt-dev.sprooty.com \
  --token-file ~/.config/vogt-a5/token \
  --report acceptance.md --json acceptance.json

# Add the throwaway-project work checks (writes, then archives its project):
python3 tests/acceptance/run.py ... --allow-writes

# Add the pod-session checks (starts sessions and always stops them):
python3 tests/acceptance/run.py ... --with-sessions

# No network, no credential: the whole suite against an in-process fake.
python3 tests/acceptance/run.py --fake --with-sessions --allow-writes
```

Exit status is `0` when no check failed (skips are not failures), `1` when a
check failed, and `2` when the invocation was refused (production, an
unlisted host, a missing/insecure token file, an unreachable target).

Useful flags: `--only health,auth` (groups or case-id prefixes), `--list`,
`--expect-digest sha256:…` (enables the image-digest check), and
`--read-token-file …` (proves a read-only credential cannot write).
`--kube-context` defaults to the read-only `estate-ro`; every cluster check
skips when that context is unavailable.

## Safety model

- **Default is read-only.** `--allow-writes` is required for the work group;
  `--with-sessions` for anything that starts a session. Neither flag is
  implied by the other.
- **The token comes from `--token-file` and nowhere else.** It is never read
  from argv or the environment, never printed, and every line of evidence and
  every report field passes through `redact()` with the run's token first.
  A group/readable token file is refused; `chmod 600` it.
- **The host must be on the allowlist** (`tests/acceptance/policy.py`):
  `vogt-dev.sprooty.com` and loopback. `vogt.sprooty.com` is refused by name
  before the allowlist is consulted, and plain `http` is refused off
  loopback. A new dev instance is added in a reviewed change, not a flag.
- **A production instance id is refused too.** The suite reads the target's
  own `instance_id` and compares it with `--prod-instance-id` or, when that
  is absent, with the configured diagnostics peer's id. When neither is
  known the report says the host check was the only guard.
- **A throwaway session has one shape**: project `vogt`, `placement=pod`,
  `worktree=false`, `model=deepseek/deepseek-v4-flash`, `autopilot=false`,
  `permission_mode=default`, and a reason carrying the run's unique tag.
  Durable/expensive models and host placement are not reachable from the
  suite.
- **Cleanup is layered.** A case's own cleanup runs in `finally`; a session a
  case tracks lives until the end of the run, so later cases can assert
  against it, and is stopped once after the last case; an `atexit` sweep
  stops any session whose reason still carries the run tag, so a killed
  process does not leave a pod behind. The full run's cleanup list also
  archives the throwaway project there.
- **The suite never runs `env`/`printenv` to inspect a session.** S12 instead
  proves the engine's environment guard refuses `env | head`, by asking the
  agent to run it and reading the documented refusal.
- **Cleanup leaves what the product cannot delete.** Archiving the throwaway
  project is the available cleanup: its scaffolded directory under the engine
  workspace and its tagged label remain, both named with the run tag. Work
  items are moved to `wont_do`; the project is archived.
- **No Docker, no image builds, no deploys, no restarts.** `--allow-restart`
  only allows the restart-durability check; the suite still cannot restart
  the engine and says so.

`--fake` is the one exception to "no token": it generates an ephemeral token
for the in-process fake and prints it nowhere. A fake run is labelled as such
in stdout and in the report.

## What each group asserts

| Group | Cases | Asserts |
| --- | --- | --- |
| `health` | H1–H6 | `/healthz`, `/readyz`, real `source_ref`/`source_sha`/`version`, diagnostics engine check, optional image digest, `/api/config` identity |
| `auth` | A1–A4 | no bearer is 401 on every protected route and `/mcp`; a bogus bearer is 401; a read-only token cannot write; the credential resolves to a named identity |
| `mcp` | M1–M7 | `tools/list` equals `tests/parity/golden/mcp-tools.json`; initialize; unknown tool; `registry_dump`; every checked-in minimal read call avoids 5xx; bad arguments are data, not a crash; `auth_whoami` |
| `work` | W1–W11 | create/update/comment/transition/walk/relate/search on a throwaway local-only project, ref preservation, priority and labels, a label edit on an unlinked project is refused, an unlinked project refuses `upstream=true`, archive cleanup |
| `sessions` | S1–S18 | Codex pod start, default listing, first task, PONG reply, screen, second input, hibernate/wake, placement off Node B, workspace content and secret absence, graft clone, cadastre and vogt MCP registration, environment guard, secret redaction, stop with no orphan pod, Claude Code/opencode pod probes, restart durability (operator-gated) |
| `providers` | P1–P4 | deepseek flash registered; unknown model refused by name; a broken provider refuses launch naming only the secret name; provider-failure visibility (WI-4131, skips) |
| `tracker` | T1–T4 | every project has `write_back=none`; forge accounts unlinked unless stated; `forge_repos` refuses cleanly; prod-import refusal (skips until WI-3172/3173) |
| `security` | X1–X3 | `/metrics` and `/api/status` exposure reported (not changed), CORS does not allow an arbitrary origin, every answer has `X-Request-Id` |
| `pwa` | U1–U4 | index/manifest/service worker/bundle load, no plain-http asset references, authenticated WebSocket attach |
| `persistence` | K1–K3 | engine state on a Bound PVC and not `emptyDir`, the latest `k8s-backup` Job succeeded, no engine restart in 24h |
| `budget` | E1–E2 | `usage_board` and `session_economics` read |
| `local` | L1–L4 | prod/unknown-host refusal, pinned session arguments, cleanup after a failed case, redaction of a bearer and a secret-shaped string |

A check that cannot observe its subject — no cluster context, a feature not
deployed, no broken provider, no `--with-sessions` — returns `SKIP` with the
reason. It is never a silent pass.

## Reports and coverage

`--report x.md` writes a Markdown report; `--json x.json` (or `--report
x.json`) writes the machine-readable one. Both contain:

- the verdict for every case and its evidence (redacted);
- the known-issue exemptions that applied;
- a requirement-coverage table built from `matrix.toml`, mapping each matrix
  row to the case ids that cover it and marking `untested` / `not testable`
  rows explicitly.

A failure is exempted only when `known_issues.toml` has an entry for that
case id and, when it states a `match`, that text appears in the failure
evidence. Every entry names a work item. A passing case with a known-issue
entry is not special-cased: the entry simply never applies.

## Adding a check

1. Pick or add a `[[requirement]]` in `matrix.toml` (or name a `WI-…`).
2. Add a function in the right `tests/acceptance/checks/*.py` that takes a
   `Context` and returns `passed(...)`, `failed(...)` or `skipped(...)`.
   Raise `SkipCase` for a capability that is absent; never return a pass for
   something you did not observe.
3. Register a `Case(id, group, requirement, severity, title, fn)` in that
   module's `CASES`. Ids are stable and prefix-addressable by `--only`.
4. If it reads a tool, add the tool (and minimal arguments) to
   `minimal_read_calls.toml` — `tests/test_acceptance_suite.py` checks that
   every name there still exists in the golden registry.
5. Prove it offline: add a path to `fake_server.py` if the fake cannot answer
   it yet, then run `uv run pytest tests/test_acceptance_suite.py --no-cov`
   and `python3 tests/acceptance/run.py --fake --with-sessions
   --allow-writes`.
6. Run it live once a dev token exists, and say in the hand-off which checks
   were only fake-verified.

## What has been verified, and what has not

The dev token at `~/.config/vogt-a5/token` works as of 2026-11-12. The suite
has been run live against `https://vogt-dev.sprooty.com`: read-only, then
`--allow-writes --only work`, then `--with-sessions --only sessions`, and
finally full `--with-sessions --allow-writes` runs. The latest, with
`--expect-digest f09829df`, reported **70 checks: 60 PASS, 0 FAIL, 1
FAIL-known, 9 SKIP** in 85 seconds.

Everything is still executed against the in-process fake (`--fake`) first, and
the suite's plumbing has pytest coverage in `tests/test_acceptance_suite.py`.
The fake proves MCP framing, auth handling, redaction, cleanup, reporting and
every branch that does not need a real cluster or a deployed feature. It is
not live evidence: a fake pass is not a live pass, and every report says which
target it ran against.

**Corrections since the 40/22 run.** Only `H2` was a product failure; the
other reds were suite defects, fixed without weakening the assertions:

- `K1` used the first ancestor mount, so the emptyDir at `/home/sprooty`
  hid the state PVC nested at `/home/sprooty/.local/share/vogt-engine`. It
  now selects the longest covering mount and fails any non-PVC winner.
- `H5` compared a short rollout prefix (`f09829df`) with the full
  `sha256:…` digest. It now compares the hex bodies, accepts an exact match
  or a prefix of at least seven characters, and still rejects a mismatch.
- `P1` accepts the live `active` state and now requires an explicit usable
  state; `P3` treats only explicit missing/broken-secret states as broken,
  so a healthy `active` provider is no longer mistaken for a broken one.
- Session cleanup is run-scoped: S1's pod survives S2–S13 and is stopped by
  S14 and the run-level/atexit cleanup.
- `W2`–`W8` read a write's compact `{ref, state, title, updated_at}`
  summary back through `work_get`, and relations through `related_ref`.

The unknown-model launch (`P2`) now refuses with a 4xx naming the model, so
it is no longer an open finding.

**Exempted, with a work item.** `H2` (`version=local/dev`, WI-3866). The live
instance answered `source_ref=graft/batch-134`,
`source_sha=c81eb71d…` — a real graft commit — so only the human-facing
version string is wrong.

**Skipped live, and why.** `A3` (no read-only token on this host), `S17` (no
`--allow-restart`), `P3` (no broken provider to exercise), `P4` (WI-4131 has
not landed), `T4` (no prod-to-dev import tool yet), `E1` (usage board not
configured), `S8` (no workspace volume; ENGINE_POD_WORKSPACE off or WI-4120
not deployed), `S9` (no per-session graft credential; WI-3229/WI-3986), and
`S10` (cadastre MCP not registered; WI-4121 in flight). A session probe the
agent refuses to run is a SKIP with the refusal quoted; it is never a pass.

Run the groups in this order once a token exists:

```console
python3 tests/acceptance/run.py \
  --base-url https://vogt-dev.sprooty.com \
  --token-file ~/.config/vogt-a5/token \
  --report acceptance.md --json acceptance.json
```

Then add `--allow-writes`, and finally `--with-sessions` on a quiet instance.
The run tag is printed for a with-sessions run; any session still carrying it
is a leak and is stopped by the next run's sweep. A tracked session now lives
for the whole run: S1 starts the pod the later session cases assert against,
and the run-level cleanup (and the atexit sweep) stops it.

## CI

`.github/workflows/acceptance.yml` is a manual-dispatch stub only: it is not a
required check, it needs a `VOGT_DEV_TOKEN` repository secret, and it defaults
to the read-only suite. The offline fake run is what
`python3 tests/acceptance/run.py --fake` exists for; it needs nothing.
