# Forge-frontend implementation design (#181, #182, #183)

_Execution design for the second half of the #178 pivot. #179 (per-actor PAT
linking) and #180 (repo picker + parity-gated import) land as the additive
foundation; the three issues designed here change the **work model itself** and
carry a schema migration of the live declared store, so they are sequenced as a
deliberate, separately-reviewed step rather than folded into the foundation
deploy. This document is the plan of record for that step; delete it once #183
closes and the decisions have moved into the requirements register and
`DESIGN.md`. The requirements register (`FR-*`/`NFR-*` IDs) is maintained by
the maintainer outside the repository, in the git-ignored `docs/local/`; IDs
below are stable identifiers, and the rule each one carries is stated in
words beside it._

**Predecessors already delivered:** the provider seam (#172), incremental
all-state sync + watermarks + `last_confirmed` (#173), symmetric drift (#174),
the provider write surface — `create/comment/label/close/reopen` (#175), and the
foundation (#179 linking, #180 import).

**Design source:** issue #178, decisions 1, 2, 7, 8, 9, 10. Read it first; this
document translates those decisions into the concrete code changes, grounded in
the current tree.

---

## 1. What exists today (the starting point)

Vogt today runs a **dual work model** — this is exactly the thing #181 removes.

- **Declared work items** live in the declared SQLite store as `WorkItem`
  rows (`src/vogt/core/entities.py`), `wrk_*` ids, a human `ref` (`WI-N`), an
  `origin` of `"created"` (native) or `"adopted"` (promoted from an
  observation). `work.create` (`src/vogt/application/services/work.py:46`)
  inserts a native row and writes nothing upstream.
- **Observed subjects** live in the *observed* store (`observed.py`): forge
  issues/PRs keyed by subject key `gh:{owner}/{repo}#{n}`, maintained by the
  sync collectors with watermarks and `last_confirmed_at`.
- **Adoption** (`observed_first.py:132`) is the one bridge: it materialises a
  declared `WorkItem` (`origin="adopted"`) plus a `WorkLink`
  (`entities.py:203`) back to the observed `subject_key`.
- **Write-back** (`work.py` → `writeback.attempt`) speaks upstream **only for a
  linked subject** (`_linked_subject`, `work.py:377`): a `transition` into a
  terminal state closes, out of one reopens, a `comment` posts. Everything else
  is local. After #179 the identity is the acting actor's PAT, else the FR-S7
  file token.
- **The Backlog** (`views.py` `_gather`) merges declared + observed → the ~159.
  **The Board** (`board.py`) reads declared only → the ~2. #187's interim fix
  makes that gap explicit; **#181 is the real convergence.**

So the machinery the pivot needs already exists in pieces — adoption already
turns an observation into a work item with a subject-key link, and write-back
already speaks to linked subjects. #181 makes that path the *only* path on a
linked project and adds **create-through** (today `work.create` never creates
upstream).

---

## 2. #181 — upstream-truth work items + local overlay (the big one)

Decisions 1, 2, 9, 10.

### 2.1 Identity

On a **forge-linked** project, `vogt id = forge subject key`
(`gh:{owner}/{repo}#{n}`). Native standalone work items cease to exist there.
A project is "linked" when it has a `repo_url` a registered provider matches
**and** a usable credential (per-actor PAT from #179, or the FR-S7 file token) —
reuse `adapters/forge/registry.provider_for` / `has_configured_forge`; do not
invent a second notion of "linked".

Introduce a **project link state** rather than inferring it every call:
`unlinked | linked`, persisted on the project (new column, migration below).
`publish` (#182) and `import` (#180) set it to `linked`.

### 2.2 The overlay table (decision 2 — invisible upstream)

A new declared table `work_overlay`, keyed to the **upstream subject key**, not
to a `wrk_*` id. It carries only vogt-only semantics that must never cross the
boundary:

```
work_overlay(
  subject_key TEXT NOT NULL,          -- gh:{owner}/{repo}#{n}
  project_id  TEXT NOT NULL REFERENCES projects(id),
  rank        REAL,                   -- vogt-local ordering
  workflow_state TEXT,               -- richer than open/closed
  -- relations live in the existing relations table, re-keyed to subject
  -- audit stays in the existing audit trail
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  PRIMARY KEY (subject_key)
)
```

The upstream issue (already mirrored in the observed store) is the source of
truth for title/body/labels/open-closed. The overlay is *additive* local state.
A read of a linked project's work item **joins** the observed mirror (truth) to
the overlay (local semantics). This is the storage stance of decision 9: a
**synced mirror with upstream authority + write-through**, not a live proxy —
the observed store already is that mirror.

> **Migration discipline.** This is a schema change to the declared store; pin
> the migration id before landing (the migration-identity CI guard). It is the
> reason this work is deferred out of the foundation deploy — a redeploy runs
> declared migrations against the live DB, and an instance's existing native
> work items (few on the maintainer's own instance, but real) must be handled
> by the migration, not stranded. The migration must decide those 2 native
> items' fate the same way #183's link-migration does (publish upstream, then
> re-key) — so **#181's migration and #183 are the same event** for already-
> linked projects and must be designed together.

### 2.3 Write-through (decision 9)

`work.create` / `work.comment` / label / open-close on a **linked** project
write through to the forge via the provider (`adapters/forge` write surface
from #175), then reconcile the local overlay + observed mirror:

- `work.create` → `provider.create_issue(...)` → returns the new issue number →
  the vogt id is the subject key → insert the overlay row. The returned
  `WorkResult.item.ref` becomes the subject key (or a subject-key-derived ref).
- `work.comment` → `provider.comment(...)` (already wired in `work.py:348`).
- label add/remove → `provider.add_labels` / provider label set.
- `transition` into/out of a terminal state → `close`/`reopen` (already wired,
  `work.py:230`).
- **Overlay-only** changes — rank, vogt-only workflow states beyond
  open/closed, relations — produce **no** upstream write (decision 2). This is
  the invariant the tests must pin: re-ranking or a vogt-only state change must
  assert zero provider calls.

**Failure semantics (decision 9): fail loud.** Write-through failure is a typed
error, no silent queueing. Reuse `WriteBackResult`'s `failed` outcome but for
`create` (the new verb) surface it as a raised typed error so the caller learns
the issue was *not* created — a queued/eventually-consistent success is
forbidden.

### 2.4 Unlinked projects (decision 10)

`work.create` and the other write verbs on an **unlinked** project return a
**typed error** telling the caller to link (#180) or publish (#182). Add a
`NotLinked`-style error to `src/vogt/errors.py`. Unlinked projects show **no**
backlog/work surfaces — that surface withdrawal is #183.

### 2.5 Registry / parity

Every changed or new operation stays registry-defined
(`registry/operations.py`); CLI/REST/MCP parity is generated. No hand-added
endpoints. The parity test that already exists must stay green with the changed
`work.*` semantics.

### 2.6 Acceptance (from #181)

- Creating a work item on a linked project produces a GitHub issue **and** a
  keyed overlay row; the returned vogt id is the subject key.
- A vogt-only state change or re-rank produces **no** upstream write.
- A write-through failure surfaces a typed error, not a queued success.
- `work.create` on an unlinked project returns the typed link-or-publish error.

---

## 3. #182 — the publish verb (create repo + push)

Decision 8. **After** #181's model exists.

New registry op `forge.publish`: create a remote repo under the linked actor's
PAT (#179) and push the local default branch.

- **This is the first destructive-capable verb** beyond the deliberately
  non-destructive write-back set (FR-B4: `comment/create_issue/add_labels/set_state`,
  never force, never delete). It
  *creates upstream state and pushes commits*. It therefore needs its own
  requirements revision with an FR-ID and a bounded rationale:
  - refuses if the remote already exists (typed refusal, never a clobber);
  - **never force-pushes, ever** (mirror FR-B4's "no force" invariant into the
    push path — `git push` without `--force`, and refuse a non-fast-forward);
  - requires a clean/explicit local state (reuse #180's parity-gate helper).
- Provider surface: add `create_repo(name, private, ...)` to the `ForgeProvider`
  write protocol and the GitHub implementation; keep it provider-agnostic like
  the rest of `adapters/forge/writeback.py`.
- After publish, the project becomes `linked` and upstream-truth (hand to
  #181's model); its open native items migrate via #183.
- Parity: CLI/REST/MCP from the registry.

### Acceptance (from #182)

- Publish creates the named repo under the linked actor and pushes the default
  branch; a naming/existing-repo conflict is a typed refusal, not a clobber.
- No force-push, ever.
- Documented in the requirements register with its FR-ID + bounded
  rationale; DESIGN describes it only once built.
- A published project is thereafter linked/upstream-truth.

---

## 4. #183 — native-item migration on link + forge-less guarantee withdrawal

Decision 7. Shares its migration event with #181 §2.2 for already-linked
projects.

- **Migration on link/publish.** Enumerate a project's **open** native work
  items; publish each as a GitHub issue (`provider.create_issue` + labels to
  carry what maps), then re-key to the subject key and fold the vogt-only fields
  into the overlay. **No open native item is silently dropped.** Closed/archived
  native items: default **leave historical, migrate only open** — record the
  choice in the requirements register.
- **Guarantee withdrawal (requirements).** A new revision withdraws the
  "forge-less layer stays real" guarantee. The designed-but-now-withdrawn
  capability is noted in the requirements register's gap section (§7),
  **not** DESIGN.md (which describes only what exists). Cross-reference #178.
- **Surface change.** Unlinked projects show **no** backlog/work surfaces —
  instead a **link/publish CTA**. This is the web counterpart to #181 §2.4's
  typed error.
- **Test repurposing.** The existing forge-less honesty tests (which assert a
  real forge-less work layer) are **repurposed** into link/publish-CTA tests: an
  unlinked project asserts the typed link-or-publish error (#181) and the CTA
  surface, not a working native backlog. Grep the suite for those honesty tests
  before writing new ones — they are the tests to change, not delete.

### Acceptance (from #183)

- Linking/publishing a project with N open native items creates N upstream
  issues, re-keyed and overlaid; no native open item silently dropped.
- The requirements register carries the withdrawal revision + §7 gap entry; the
  former honesty tests now assert the CTA/typed-error behaviour and pass.
- An unlinked project renders the link/publish CTA and no backlog.

---

## 5. Sequencing and the deploy boundary

```
#179 linking ─┐
#180 import  ─┼─► (foundation: additive, no work-model migration) ─► deploy A (foundation)
#187 honesty ─┘

#181 model + migration ─┐
#183 native migration   ─┼─► (one migration event; live declared DB) ─► deploy B (deliberate)
#182 publish            ─┘
```

- **Deploy A** carries the foundation only. `#187`
  keeps the Board honest until convergence.
- **Deploy B** is the work-model migration. It must: pin its migration id; back
  up the declared store first (`vogt backup` covers it); migrate every live
  native item via the #183 path; and be walked against a restore
  rehearsal before firing, because it rewrites the live work store. This is why
  it is not bundled with A.

## 6. Risks / watch-items

- **The migration is the whole risk.** Everything else is additive. Rehearse the
  declared-store migration on a copy of the live DB before Deploy B.
- **Write-through latency** on the interactive path (`work.create` now does a
  network round-trip). Keep it synchronous and fail-loud (decision 9), but the
  PWA must show the round-trip state.
- **Ranking cost.** The overlay's `rank` must not require re-gathering the whole
  observed set on every write; key the overlay by subject and update in place.
- **Subject-key as `ref`.** Callers and tests assume `WI-N`-style refs; decide
  whether the subject key becomes the ref or a subject-keyed ref is derived, and
  do it once, centrally, so the CLI/REST/MCP surfaces agree.

## 7. Definition of done for the deferred step

- #181, #182, #183 acceptance boxes all green.
- One declared migration, id-pinned, rehearsed on a live-DB copy.
- Requirements revisions: upstream-truth model; the publish verb + FR-ID; the
  forge-less guarantee withdrawal + §7 gap entry.
- DESIGN.md updated to describe only what now exists.
- The Board and Backlog converge (closing #187 for real); its interim banner is
  removed.
