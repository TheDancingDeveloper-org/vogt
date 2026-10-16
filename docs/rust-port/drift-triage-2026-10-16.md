# Rust port drift triage — 2026-10-16

First-pass triage of `scripts/drift_ledger.py` (P0.5 / WI-1047) against
`origin/main` as of `e9955e7454c9`. Triage only: nothing was ported and no
golden was re-recorded.

## Range

| | |
| --- | --- |
| port base | `d60cc3444e1ce024602d83766710c0ee1abecf20` (merge-base of `origin/rust-core` and `origin/main`) |
| main tip | `e9955e7454c9` — *fix(web): a session-list refresh no longer moves focus off the Files tree (WI-1682)* |
| rust-core tip | `f271706a` — *test(core): port trust confirmation to views (WI-1080)* |
| ledger rows | **105** commits that touch `src/vogt/`, `tests/`, `docs/CONFIG.md` or `config.example.toml` |
| all commits on main since the base | 243 |

`config.example.toml` and `docs/CONFIG.md` did not change in the range, so the
config golden is unaffected. No commit in the range touches the chat services:
the `chat.*` operations landed before `d60cc34` and remain owned by WI-1483.

## Classification

| class | rows | what it means |
| --- | --- | --- |
| new operation | 15 | a new registry operation, or a new field/parameter on an existing one that changes the wire shape |
| behaviour change | 21 | an existing operation behaves differently; the Rust port must absorb it |
| config / schema / migration | 4 | a new declared migration (hard item) plus the entity/store change it carries |
| docs / tests-only no-op | 20 | touches only `tests/` or a docstring, or is a version bump; no core behaviour |
| engine / PWA only | 45 | the only Python touch is a test of engine, deploy, CI or pod behaviour; the core is unchanged |

Every actionable row below already has an owning port slice. **No new work
item was created.**

## Actionable drift, by owner

### WI-1073 — S8a sessions I (start/stop/list/screen/input/wait/wake/hibernate/sweep)

Behaviour changes:

- `02980ab` `117e51e` `b3b619f` — `session.start` `cwd`: open in an existing worktree, keep purpose profiles, anchor `Active/`, refuse a subdirectory of the shared checkout (WI-1671)
- `bd83b433` `9d2232cf` — `session.input` submits by default; `draft=true` types without sending; no default Enter after an explicit enter key (WI-1316)
- `09881345` — `session.list` passes the engine's hidden-session count through (WI-1289)
- `c57e408f` `98ce5d23` — page `session.list` after joining live sessions; the declared-link cap must not look complete
- `576d4604` — only a person starts an oversight session (WI-1255)
- `ca2c69dc` — do not wake a session that was handed on (WI-1251)

New wire shape:

- `fe64958f` — `session.list` `started_by` filter, `session.wait_any` `cursor=now`, csv `fields` (WI-1672)

### WI-1074 — S8b sessions II (grants, history, last_reply, blocked, agent activity)

Behaviour changes:

- `0e93f4b` — the session digest carries the full blocked reason
- `4986374` `82fb230` — a resumed Codex gets the swallowed-Enter retry only when its prompt is ready (WI-1669)
- `f2bd699` — an oversight session may answer a worker's permission prompt
- `943d4c7` `f837017` — `session.start` matches `forbidden_text` and refuses a model the project forbids, including a foreign model for claude (WI-1590)
- `7782a7e8` — named Claude Code accounts on `session.start` (WI-1306)
- `3481efc5` `7dec8e43` — transcripts index conversation ids and notice a new Codex rollout in today's directory

New operations:

- `5494af65` `2300de79` — `session.economics`, read from the agent transcript (WI-1242)
- `a637a1fc` — `session.digest`, and blocked reports that survive hibernation (WI-1247)
- `39e6a25` `70bc3e0` `a49bac8` — `session.commit_for`, the oversight commit path, with who-may-commit and `forbidden_text` checks (WI-1248)
- `c57fc5dc` — `session.changes` and `session.wait_any` over a sequenced event bus (WI-1246)
- `0081952` — digest rows and filters on the session tools (WI-1241)

### WI-1068 — S3 work items and ranked views

- `7736123` — new operation `work.get_many`, the batched chip read (WI-1224)
- `5ca043b` — behaviour: rank an exact ref ahead of substring hits in work search (WI-1249)

### WI-1069 — S4 initiatives, preferences, inbox

- `a264239b` `6fdb80f6` — new operations `decision.ask` / `decision.answer` / `decision.list` / `decision.withdraw`, the decision entity, and an Inbox row (WI-1321). Carries migration `0023_decisions.sql`.

### WI-1066 — S1 instance, install, auth, lifecycle

- `7fe6156` — behaviour: validate a backup's stores before restoring them (WI-1066)
- `885ccaa0` `2c5d3602` — behaviour: fail-closed worktree cleanup and a disk-usage row on instance status (WI-1304)

### WI-1070 — S5 contracts, compliance, drift

- `0803093` — behaviour: report a criterion's real applicable value (WI-1070)
- `a73d763` — behaviour: mint contract exemptions through the context id factory (WI-1152)

### WI-1071 — S6 collectors and sweep

- `4aa81f3` — behaviour: canonicalise a mirrored-source root before walking it (WI-1071)

### WI-1054 / WI-1058 — P1.6 storage, P1.9 application

- `e6c386a` — behaviour: cache place/metrics badges and split the engine's read cap; new `application/memo.py` and read-pool changes (WI-1224)
- `e9b3abf` — behaviour: give the observed store the context's id factory

### WI-1061 — P2.3 HTTP adapter

- `319bb466` `56001df` — behaviour: opt-in Prometheus `/metrics` with process collectors, every series labelled `component=core` (WI-1380)

### WI-1063 — P2.5 MCP adapter

- `9f5d280` — behaviour: send the whole result as compact text, not a 200-character summary (WI-1241)

### WI-1064 — P2.6 external adapters (engine client, transcripts)

- `fc1766ff` `a389c66f` — behaviour: engine-enforced session budgets, and live session economics, history and histograms (WI-1245)
- `69d744a6` `0b96dfb2` — behaviour: `session.start` `worktree=true` cuts a worktree; the budget stops at a turn boundary and gains a whole-session clock (WI-1298, WI-1281)

### WI-1058 — P1.9 application (brief and policy rendering)

- `6f5526e2` — behaviour: dispatch purposes and start provenance on `session.start` (WI-1323). Carries migration `0024_session_started_by.sql`.
- `8b822484` — behaviour: structured landing, review and verification blocks in `agent_policy` (WI-1318)
- `68385649` `ccf818f5` — behaviour: a review session gets a brief that checks the diff, states the verdict and stays read-only
- `9d0867c7` — behaviour: hand-off comment kind, newest-first threads, engine hand-off files (WI-1244). Carries migration `0022_comment_kind.sql`.
- `6bf47108` `a3d2df95` — behaviour: project `agent_policy` and engine-composed briefs, enforced on agent git and gh writes (WI-1243). Carries migration `0021_agent_policy.sql`.

### WI-1053 — P1.5 pure domain

- `93334c20` — behaviour: exact millisecond gaps in session economics; DEL escaping matches `json.dumps`

### WI-1051 — P1.3 pure domain (entities)

- `d9fe2ef1` — behaviour: accept every agent-CLI install kind the engine reports (WI-1362)

## Migrations (hard items, not optional drift)

Four new declared migrations, none of them on rust-core. The migrator picks up
a migration it finds, but the Rust store code and the upgrade tests do not
exist until the owner ports them.

| migration | arrives in | owner |
| --- | --- | --- |
| `0021_agent_policy.sql` | `6bf47108` | WI-1058 (with WI-1243) |
| `0022_comment_kind.sql` | `9d0867c7` | WI-1058 (with WI-1244) |
| `0023_decisions.sql` | `a264239b` | WI-1069 (WI-1321) |
| `0024_session_started_by.sql` | `6f5526e2` | WI-1058 (WI-1323) |

## New operations on main (10)

`decision.answer`, `decision.ask`, `decision.list`, `decision.withdraw`,
`session.changes`, `session.commit_for`, `session.digest`,
`session.economics`, `session.wait_any`, `work.get_many`. None removed. The
registry grew from 134 operations to 144.

## Docs / tests-only no-ops (20)

Version bumps: `25453c0` (v0.7.16), `2805808` (v0.7.15), `ca9767b` (v0.7.14),
`febda8d` (v0.7.13), `5374ed2` (v0.7.12), `115a170` (v0.7.11), `6de701d`
(v0.7.10). These change `src/vogt/__init__.py` and `tests/test_product_version.py`
only.

Test-only or docstring-only: `b01494c` `e89573f` `a15d48c` `d817d3a8`
`2c6e83d1` `19562699` `704ad1f4` `fbd92b44` `601b6404` `9e913e4a` `f7a4aa2a`
`aff10f1` `69726c8`. `69726c8` is the web search feature (WI-1249) and its only
Python change is a test; the core behaviour is the separate row `5ca043b`.

## Engine / PWA only (45)

The Python core did not change. Listed so they are visibly excluded rather
than missed.

- Codex / LiteLLM / MCP bootstrap tests (`tests/test_readonly_mcp_wrappers.py`,
  `tests/test_agent_cli_boot_contract.py`): `026302c` `088ea43` (WI-1640),
  `d413009` (WI-1639), `b3962fe` (WI-1634), `3db6d55` `038d909` (WI-1592,
  WI-1147), `d7f7086` `6969369` (WI-1149), `e69b86c` `06eb769` `c382cd3`
  `573d874` (WI-1146), `5394b21` (WI-1358), `a3adc14` `2716c4a` (WI-1360),
  `ede654f` `3b63614` `62b0894` (WI-1147), `19f21a3` `db9fbd5` (WI-1142),
  `3076e69` `57c8585` `5b86e88`
- CI and release workflow tests: `19606fe` `e8217c7` (WI-1649, WI-1650)
- pod cargo cache tests: `3199b57` `e1201c7` (WI-1256)
- one model-only docstring: `af77273` (WI-1255)

## WI-1443 estimate — merge main forward and refresh goldens

WI-1443 is the mechanical sync, not the port of the rows above. Merging
`origin/main` into `rust-core` and re-recording the goldens at the new base is
small; absorbing the rows is the large part and belongs to the owners named
above, not to WI-1443.

**Conflict hotspots: 6 files, none of them Python core.** Files changed on both
sides of `d60cc34`:

| file | why it conflicts |
| --- | --- |
| `engine/Cargo.lock` | both sides add dependencies |
| `engine/Dockerfile` | rust-core adds the core build; main changes the pod |
| `.github/workflows/ci.yml` | rust-core adds the parity job; main changes CI |
| `.github/workflows/e2e.yml` | same |
| `.github/workflows/release.yml` | same |
| `pyproject.toml` | both sides touch tool configuration |

No file under `src/vogt/` or `tests/` was edited on both sides, so the merge of
the Python core is clean. The Rust port lives in `engine/core/`, which main
does not touch.

**Golden files that change: 4 of 4, all of them small.**

| file | size now | expected change |
| --- | --- | --- |
| `tests/parity/golden/d60cc34/cli.json` | 41 KB, 58 steps, 50 operations | renamed to the new base SHA; new steps for the 10 new operations and changed answers where an existing operation's shape moved |
| `tests/parity/golden/d60cc34/http.json` | 27 KB | same, for the HTTP transport |
| `tests/parity/golden/d60cc34/meta.json` | 3 lines | the recorded SHA and timestamp |
| `tests/parity/golden/registry.json` | registry snapshot | 134 → 144 operations |

`tests/parity/script.json` (11 KB) is the script corpus, not a recorded golden,
and does not change unless the corpus itself is extended. The config golden is
unchanged because `config.example.toml` did not move.

The refresh cannot go green until the 10 new operations and the behaviour
changes above are ported. Expect the re-recorded golden to fail `parity.py
check --impl rust` on those operations; that failure is the residue WI-1081
closes, and it is what the per-owner comments below track.
