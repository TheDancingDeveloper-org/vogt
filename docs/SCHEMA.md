# Vogt — Data Schema & Topology (draft v0.3, revision r3)

Status: **draft, pre-implementation**. Types are indicative; DDL is written
at M0. Companion to `DESIGN.md` §3 (domain model) and §7 (storage).

r2 changes: `contracts` and `packages` tables removed; `events` and
`suppressions` tables added; dependency tables reduced to references;
drift proposals carry an evidence snapshot; retention rules tightened.

r3 changes: no sweep roots and no discovery — collector scope is always a
set of registered project ids; `projects.exclusions` replaces per-root
exclusion patterns; contract results are written by on-demand checks only.

## 1. Storage topology

Two SQLite databases with strictly different write disciplines:

```
data-dir/
  declared.sqlite3    # authoritative, mutable, audited, revisioned
  observed.sqlite3    # append-only evidence + derived "latest" tables
  backups/
```

```mermaid
flowchart LR
    GH[GitHub API] --> COL[collectors]
    GIT[local git checkouts] --> COL
    SRC[source markers] --> COL
    MAN[manifests: path/git refs] --> COL
    COL -->|append| OBS[(observed.sqlite3)]
    OBS --> DRIFT[drift engine]
    DEC[(declared.sqlite3)] --> DRIFT
    DRIFT -->|proposals| DEC
    APP[application layer] -->|audited writes + events| DEC
    APP -->|reads| DEC
    APP -->|reads| OBS
    CLI --> APP
    REST --> APP
    MCP --> APP
    GUI --> REST
```

Rules:

- Nothing writes `declared.sqlite3` except the application layer, and every
  write carries `(actor, reason)` and lands an audit row **and an event
  row** in the same transaction.
- Nothing writes `observed.sqlite3` except collectors; rows are immutable
  once written. Retention pruning is the only delete path, and it is
  constrained by §5.
- The drift engine reads both, writes only `drift_proposals` (declared side).
  Accepting a proposal is an ordinary audited application write.
- Cross-database joins happen in the application layer, not SQL `ATTACH`
  (keeps the stores independently portable/backupable). Because every
  aggregate view is therefore a Python-side join, the NFR-S4 benchmark
  fixture exists from M2 to keep that cost visible.

## 2. declared.sqlite3

### 2.1 Identity & audit spine

| Table | Purpose | Key columns |
|---|---|---|
| `meta` | schema version, instance id, monotonic `revision` counter | |
| `migrations` | forward-only migration ledger | `id, applied_at, checksum` |
| `migration_lock` | single-writer migration guard | |
| `actors` | humans **and** agents | `id, kind(human\|agent), display_name, identity_ref, disabled` |
| `tokens` | API credentials bound to actors | `id, actor_id, scopes, token_hash, expires_at, revoked_at` |
| `audit` | every declared write | `id, txn_id, revision, actor_id, operation, entity_kind, entity_id, reason, payload_digest, at` |

### 2.2 Project

| Table | Purpose | Key columns |
|---|---|---|
| `projects` | unit of the per-repo view; one explicitly registered repo or folder (FR-P5, FR-G15) | `id, slug, name, root_path, repo_url, lifecycle_state, current_version, contract_version, compliance_status(compliant\|non_compliant\|not_checked), compliance_checked_at, exclusions(json), trust_state, created_at, updated_at` |
| `initiatives` | cross-project epics | `id, slug, title, body, state(open\|closed), weight, created_at, updated_at` |
| `project_dependencies` | **declared** edges between projects (FR-D1/D2) | `id, from_project_id, to_project_id, ref_kind(declared), note, created_at` |

*r2 removals*: `contracts` — the contract is configuration carrying a
version string (`DESIGN.md` §5), so a table of versioned contract bodies
bought nothing at one contract per instance; the evaluated result lives in
`latest_contract_checks` (§3.2) and `projects.compliance_status`.
`packages` — with dependency edges resolved by path and repo URL, published
package identity is no longer needed to build the internal graph.

### 2.3 Work

| Table | Purpose | Key columns |
|---|---|---|
| `work_items` | the unit of work | `id, ref, kind(feature\|bug\|chore\|question), title, body, state, priority(p0..p4), effort, project_id, initiative_id, origin(created\|adopted\|observed), trust_state, assignee_actor_id, created_at, updated_at` |
| `work_relations` | typed DAG edges, cross-project | `work_item_id, related_id, kind(depends_on\|relates_to\|duplicate_of\|parent_of)` |
| `labels` | instance-wide tag definitions (GitHub-label aligned) | `id, name, color` |
| `work_item_labels` | tag assignments | `work_item_id, label_id` |
| `work_links` | link to observed forge objects | `work_item_id, forge_kind(issue\|pr), repo, number, relation(completion\|reference), created_at` |
| `comments` | collaboration | `id, work_item_id, actor_id, body, created_at` |
| `workflow_defs` | state machine per work-item kind | `kind, definition(json)` |

*Added at M1*: `work_items.ref` is the short handle (`WI-7`) allocated from a
counter in `meta`, inside the creating transaction so a rolled-back creation
leaves no gap. Ids stay ULID-shaped and stable; refs are what a human or an
agent actually types, and every parameter that names a work item takes one.
`initiatives` likewise gained a slug for the same reason.

There is deliberately **no `rank_order` column** (decided 2026-08-12).
Ordering is computed from documented weights and is fully explainable by
`why`; manual influence is expressed through `priority` and initiative
weight, which are themselves scored inputs. No hand-set position competes
with the score.

### 2.4 Drift

| Table | Purpose | Key columns |
|---|---|---|
| `drift_proposals` | machine-generated, human/agent-resolved | `id, kind, subject_kind, subject_id, evidence_observation_id, evidence_snapshot(json), proposed_change(json), status(open\|accepted\|rejected\|contested), opened_at, resolved_by_actor_id, resolved_at, resolution_reason` |

`evidence_snapshot` (r2, FR-R5) is the self-contained copy of the evidence
as it stood at raise time: the observation payload digest, its
`subject_key`, its `observed_at`, and enough of the payload to explain the
proposal without the observed store. `evidence_observation_id` remains the
pointer to the live row, and observations so referenced are exempt from
retention pruning (§5). A proposal must never outlive its evidence.

Drift kinds (v1):

| Kind | Stage | Notes |
|---|---|---|
| `version_mismatch` | M3 | declared version vs observed tag/release |
| `unresolved_dependency` | M3 | internal-looking reference with no registered target (FR-D5) |
| `forge_state_mismatch` | M5 | linked issue/PR state disagrees |
| `ci_red_vs_healthy` | M5 | CI red on default branch, project claims healthy |
| `vanished_upstream` | M5 | linked forge object absent within provably swept scope |
| `update_automation_gap` | M5 | a required automation toggle is off (FR-D6) |

*r2 removals*: `contract_violation` and `unregistered_project` are no
longer drift — the first is `projects.compliance_status` (`DESIGN.md` §5),
the second is the candidates listing (FR-G6). `dep_version_skew`,
`lock_manifest_mismatch` and `vendored_divergence` required resolved
versions and are withdrawn with them; the situation the last one described
is reported as the `mirrored_source` observation instead (FR-D8).

### 2.5 Events & suppression (r2)

| Table | Purpose | Key columns |
|---|---|---|
| `events` | the single ordered notification feed (FR-N1) | `seq INTEGER PRIMARY KEY AUTOINCREMENT, kind, entity_kind, entity_id, actor_id, audit_id, summary(json), at` |
| `suppressions` | observed subjects excluded from ranked views (FR-W10) | `id, match_kind(exact\|pattern), subject_key_or_pattern, scope_project_id, actor_id, reason, created_at, revoked_at` |

`events.seq` **is** the `/events` cursor. Two producers, one table:

*Implementation note (M0)*: instance creation is the one audited write that
emits **no** event. `vogt init` creates the instance rather than changing
anything inside one, so it writes `meta`, the initiating actor, and an audit
row at revision 0 — and the feed therefore starts at the first change a
client could act on. Everything else, including the auto-registration of a
principal seen for the first time, lands its event in the same transaction as
its entity change and audit row.

1. Declared writes insert their event row inside the same transaction as
   the entity change and audit row (`audit_id` set), so a write is never
   visible without its event and never emits an event it did not commit.
2. Observed-side happenings — sweep completion, CI state transition — are
   published by the *application layer* at sweep completion (`audit_id`
   null, `summary` naming the sweep). Collectors still never write the
   declared store; the application publishes on their behalf.

This is why no client merges orderings across the two databases: the
observed store has no cursor of its own and does not need one.

`suppressions` lives in the declared store because it is an audited human
or agent decision, not an observation — which is also why it survives
re-observation of the same `subject_key`.

## 3. observed.sqlite3

### 3.1 Evidence (append-only)

| Table | Purpose | Key columns |
|---|---|---|
| `sweeps` | collector coverage records — makes "absent" ≠ "not collected" | `id, collector, scope(json), started_at, finished_at, outcome(ok\|partial\|failed), stats(json)` |
| `observations` | one immutable evidence row | `id, sweep_id, collector, kind, subject_key, payload(json), content_digest, source_url, observed_at` |

`subject_key` is a deterministic natural key, e.g.
`gh:owner/repo#123`, `ci:owner/repo@sha:workflow`, `mark:repo/path#L42`,
`release:owner/repo@tag`, `depref:repo/Cargo.toml→../nzb-core`,
`contract:project-slug`. Same `subject_key` + same `content_digest` in a
new sweep ⇒ no new row (sweep stats count it as unchanged), keeping growth
proportional to change, not to polling frequency.

Marker observations carry a `promoted` flag derived from the FR-W11
promotion pattern, so unpromoted markers stay queryable without entering
backlog views.

### 3.2 Derived (rebuilt, not source of truth)

| Table | Purpose |
|---|---|
| `latest_forge_items` | newest observation per issue/PR subject — what queries join against |
| `latest_ci_runs` | newest CI conclusion per repo/branch/workflow |
| `latest_contract_checks` | newest contract result per project: status + failing criteria + contract version + checked-at (written by on-demand checks only) |
| `latest_releases` | newest tag/release per repo |
| `latest_dep_refs` | one row per (project, target): `ref_kind(path\|git\|declared)`, raw target, manifest file it was read from, resolved `to_project_id` or null (FR-D1–D4) |
| `latest_markers` | newest marker per source location, with `promoted` |
| `latest_autoupdate_posture` | *(M5)* per project: version-updates config present / vulnerability alerts / security fixes — three columns, never one boolean |

Derived tables are rebuilt transactionally at sweep completion from
`observations`; they can always be dropped and regenerated, bounded by the
retention horizon (§5).

*Implementation note (M2)*: two of these shipped, not seven. The evidence
store holds `latest_observations` — generic, keyed by `subject_key`, carrying
kind, project, payload and the `promoted` flag — plus `latest_dep_refs`,
which is the only projection that adds anything the observation does not
already state (resolution to a registered project, FR-D3). The typed reads
the other five described are queries over `kind`. Five rebuild paths would
have been five places for a collector and its projection to drift apart;
`latest_autoupdate_posture` still arrives with the forge module at M5.

*r2*: `latest_dependencies` (requested spec, locked version,
direct/transitive) is replaced by `latest_dep_refs`. No lockfile is parsed
and no version is resolved.

## 4. Cross-store semantics

- **Trust computation**: `verified` = declared row's linked subjects were
  seen in a sweep newer than `verify_horizon` and agree; `stale` = last
  agreeing observation older than horizon; `unverified` = no linked
  observation ever; `disputed` = open drift proposal. Computed, never
  hand-set. The trust state is `disputed`, not `contested` (decided
  2026-08-12): `contested` is reserved for the drift *resolution* status,
  where it names something a human deliberately chose.
- **Coverage-gated absence**: "issue vanished upstream" may only be asserted
  when a completed sweep's `scope` provably included that subject and the
  observation is absent. Otherwise the answer is "not collected", surfaced
  as such in API responses.
- **Observed-first work**: `latest_forge_items` and *promoted* markers
  appear in backlog/bug views immediately (trust `unverified→verified` via
  sweeps). `adopt` promotes one into `work_items` with `origin=adopted` and
  a `work_links` row; drift then keeps the pair honest.
- **Suppression filter**: every ranked or aggregated read applies
  `suppressions` (exact and pattern, unrevoked) and the `promoted` flag
  before scoring. Suppressed subjects remain returnable by explicit
  observation queries — the decision hides them from views, it does not
  delete evidence.
- **Compliance**: `projects.compliance_status` is a projection of
  `latest_contract_checks`, written when an on-demand contract check runs
  against a registered project (r3 — nothing refreshes it on a timer). It
  is always read together with `compliance_checked_at`, and no code path
  treats it as a precondition (`DESIGN.md` §2.1, FR-G13/FR-G14).
- **Scope**: every collector's `scope` is a set of registered project ids.
  The system has no notion of an unregistered path (FR-G15), so coverage
  questions are always answerable against a known finite list.

## 5. Scale envelope & retention (v1 targets)

Single node, SQLite WAL mode: ≤ ~500 projects, ≤ ~100k work items. A seeded
fixture at this envelope exists from M2 and asserts the sub-second
interactive-query target in CI (NFR-S4).

Retention (NFR-I5), in precedence order — a row is pruned only if no rule
protects it:

1. The **latest** observation per `subject_key` is kept indefinitely.
   Digest dedup (FR-O7) means a stable subject's latest row can be older
   than the history window, so age alone must never prune it.
2. Any observation referenced by a `drift_proposals` row — of any status,
   including resolved — is kept (FR-R5).
3. All other history is pruned by configurable policy (default 180 days).

Postgres is a later backend behind the storage interface if the envelope is
outgrown; the schema avoids SQLite-only features to keep that door open.
