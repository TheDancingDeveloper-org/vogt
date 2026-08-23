# Handover: vogt forge-integration Phase 4 landing + "vogt as git frontend" pivot

You are picking up work in `/home/sprooty/Working/Active/apps/vogt` (GitHub:
TheDancingDeveloper-org/vogt), branch `feat/forge-provider-integration`.
Read `AGENTS.md` at the repo root first, and `docs/FORGE_INTEGRATION_STATUS.md`
for the delivery state of the forge work. Do not start until you've read both.

## Where things stand

- GitHub issues #171 (design) and #172–#176 (phases 1–5) define the forge-provider
  integration. Phases 1–3 are committed on this branch (`dd5c5f8`, `c7c22ba`,
  `5b0e930`). Phase 4 (#175) is functionally COMPLETE but UNCOMMITTED in the
  working tree — suite green except one pre-existing failure (below).
- Phase 5 (#176, Forgejo) is now DEFERRED past MVP1 — do not implement it.
- The tree also contains UNRELATED dirty files that must NOT go into the Phase 4
  commit: `web/*`, `deploy/vogt-stack.komodo.md`, `docs/OPEN_ISSUES_PLAN.md`,
  `docs/OPEN_SOURCE_TRANSITION.md`. The Phase 4 commit is exactly: all modified
  files under `src/vogt/adapters/forge/`, `src/vogt/adapters/github/writeback.py`,
  `src/vogt/application/services/{collect,drift_service,imports,inbox,notifications,writeback}.py`,
  `tests/{test_checks,test_drift,test_notifications}.py`, plus the three new files
  `src/vogt/adapters/forge/{collectors,kinds,writeback}.py`.
  `docs/FORGE_INTEGRATION_STATUS.md` is a working note — update it (task 3) but
  keep it uncommitted. This handover file itself (`docs/FORGE_PIVOT_HANDOVER.md`)
  also stays uncommitted.

## Known pre-existing failure (not yours to inherit as a regression)

`tests/test_cli.py::test_no_command_prints_help` fails on this machine's
Python 3.14 venv because argparse emits ANSI colour in help output (project
targets 3.11). It fails identically at the Phase 1 commit. Ignore it, or fix it
separately (set `NO_COLOR=1` in the test) — never fold it into the Phase 4 commit.

## ⚠️ Git divergence — resolve BEFORE pushing anything

`origin/main` is `1026d2f` ("ci: stop cancelling main's build … (#129)", a
squash merge) and does NOT contain the local main-line history: local
`feat/forge-provider-integration` has ~130 commits not on origin/main
(including many local "Merge pull request #1xx" merge commits for PRs #106–#166);
origin/main has exactly 1 commit (`1026d2f`) not in local. Merge-base is
`373b742`. In other words: months of locally-merged work was never pushed to
origin/main, and origin/main gained one squash-merged commit meanwhile.

Before opening the Phase 4 PR you must reconcile: verify whether `1026d2f`'s
content already exists in local history (it appears to be PR #129, possibly
merged locally under a different SHA), then get local main pushed to origin/main
(likely `git checkout main && git merge origin/main` — or confirm the CI change
is content-identical and reconcile accordingly — then push). Do NOT force-push
anything without explicitly confirming with the user first. If reconciliation
looks at all ambiguous, stop and ask the user.

## Task 1 — land Phase 4

1. Fix the 4 remaining `ruff` E501 line-length errors (nothing else fails):
   - `src/vogt/adapters/forge/github.py:276`
   - `src/vogt/application/services/writeback.py:308`
   - `tests/test_notifications.py:186` and `:191`
2. Verify: `.venv/bin/ruff check src/vogt tests` clean;
   `.venv/bin/mypy src/vogt` clean; `.venv/bin/python -m pytest -q` →
   expect 953 passed, 1 failed (the pre-existing test_cli failure only).
3. Commit ONLY the Phase 4 file set listed above, message:
   `forge(4/5): the full surface behind the provider — write-back, posture, notifications, labels (#175)`
4. After resolving the divergence above, push the branch and open a PR against
   main with body closing #172, #173, #174, #175 (and noting #169/#170 close via
   Phase 3). Do not merge or deploy without the user's go-ahead — deploying
   vogt-dev kills the terminal session and is explicitly a separate, last step.

## Task 2 — create the new design + implementation issues on GitHub

Background: the user has agreed a pivot beyond #171 — vogt becomes the frontend
for work tracking with the forge as system of record. The agreed decisions
(each confirmed explicitly by the user on 2026-08-21):

1. **Work-item truth**: for a forge-linked project, the GitHub issue IS the work
   item (vogt id = forge subject key, e.g. `gh:{owner}/{repo}#{n}`).
   `work_create` writes through to GitHub immediately. Vogt keeps a thin local
   overlay keyed to the upstream id for vogt-only semantics (ranking, richer
   workflow states, relations, audit). Native standalone work items cease to
   exist on linked projects.
2. **Overlay invisible upstream**: GitHub only ever sees its own vocabulary
   (create, comment, label, open/close). Vogt-only states/ranking/relations stay
   local.
3. **Groupings**: labels are the shared two-way vocabulary (as today);
   initiatives/boards/lenses stay vogt-local views. No milestones, no
   Projects v2.
4. **Account linking**: per-actor PAT paste in settings, stored server-side;
   upstream writes attributed to the linked user. The instance-level file token
   (FR-S7) remains the sweep fallback.
5. **Import**: linking enumerates the PAT's accessible repos → picker with
   select-all; import = clone + full sync (issues/PRs/labels/releases).
6. **Parity gate, no merge logic**: importing an existing filesystem folder is
   refused unless local default-branch HEAD == origin default-branch HEAD AND
   the working tree is clean (other local branches ignored). Refusal receipt
   tells the user to push/pull themselves and retry. Vogt implements zero merge
   logic on import.
7. **Migration**: when a project links/publishes, its open native work items are
   published upstream as GitHub issues, then become upstream-truth. Unlinked
   (forge-less) projects show NO backlog/work surfaces — this withdraws the
   "forge-less layer stays real" guarantee and must be recorded in
   `docs/REQUIREMENTS.md` §7 (read AGENTS.md on how requirements/withdrawals
   are recorded; capabilities designed-but-not-built belong in §7, not DESIGN.md).
8. **Publish**: users can create a repo upstream from a local project and push —
   a NEW write verb beyond the deliberately non-destructive FR-B4 set
   (comment/create_issue/add_labels/set_state); needs explicit requirements
   treatment, as does per-actor PATs vs FR-S7.
9. **Storage stance**: synced mirror with upstream authority + write-through —
   NOT a live API proxy (rate limits, offline honesty per FR-O4, MCP latency).
   Phases 2–3's sync substrate (watermarks, `subject_seen.last_confirmed_at`,
   symmetric drift) is the foundation; the refactor removes the dual work model.
   Write-through failure semantics: fail loud (typed error), no silent queueing.
10. **MVP1 is GitHub-only**; Forgejo/GitLab/Gitea later. `work_create` etc. on an
    unlinked project must return a typed error telling the caller to link/publish.

Create on GitHub (label `enhancement`, matching the house style of #171–#176 —
read those issues first to match voice and structure):

- One **design issue** ("Vogt as the frontend for forge-hosted work — upstream-truth
  work items") recording the decisions above as the agreed design, referencing
  #171 as the predecessor and stating what it supersedes.
- Five **implementation issues** split from it:
  (a) per-actor PAT linking + settings surface (registry operation, CLI/REST/MCP
      parity per AGENTS.md — an operation not in the Python registry does not exist);
  (b) repo picker + parity-gated clean import;
  (c) upstream-truth work items + overlay refactor (the big one — includes the
      typed-error behaviour for unlinked projects and hiding work surfaces);
  (d) the publish verb (create repo + push) with its FR-B4/requirements treatment;
  (e) native-item migration on link + the forge-less guarantee withdrawal in
      REQUIREMENTS §7 + repurposing the forge-less honesty tests into
      link/publish-CTA tests.
- Comment on #171 pointing to the new design issue; comment on #176 that Forgejo
  is deferred past MVP1 (do not close #176).

## Task 3 — update the status doc

Update `docs/FORGE_INTEGRATION_STATUS.md` (keep uncommitted): mark Phase 4
committed (once it is), record that Phase 5 is deferred past MVP1 and why, link
the new design issue set, and note the origin/main divergence finding and how it
was resolved.

## Ground rules from this workspace (non-negotiable)

- Layer order is strict (`core` → `storage` → `application` → `registry` →
  adapters); an adapter that decides anything is a bug. New operations enter the
  Python registry and get CLI/REST/MCP parity for free — never hand-add an
  endpoint.
- `docs/DESIGN.md` describes what EXISTS; designed-not-built goes to
  REQUIREMENTS §7. `docs/CONFIG.md`/`config.example.toml` are generated from
  `src/vogt/config.py` via `scripts/gen_config_docs.py` — edit the schema, run
  the script.
- Verify with: `.venv/bin/python -m pytest -q`, `.venv/bin/ruff check src/vogt
  tests`, `.venv/bin/mypy src/vogt` (or the `uv run` equivalents per AGENTS.md).
- Never deploy; never force-push; ask the user before anything irreversible.
