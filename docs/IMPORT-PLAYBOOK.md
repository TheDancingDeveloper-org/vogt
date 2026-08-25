# Importing a project into Vogt

A repeatable procedure an AI agent can run, unattended, to review a repository
and bring it under Vogt as the single source of truth for its work.

It is written as phases. Each phase states what to run, what a good answer
looks like, and what to do when the answer is bad. Phases 0–3 are read-only;
the first write is in phase 4. **Stop and ask a human** wherever a step says
DECISION — those are the points where guessing produces a project record that
is wrong in a way later sweeps will not correct.

The worked example throughout is `rustnzb`, a Rust workspace whose local
checkout lives at `/srv/work/rustnzbd` and whose GitHub repository was
transferred between organisations shortly before import. Substitute your own
project, path and remote.

---

## 0. Establish the instance you are talking to

```
vogt status          # or MCP `status`
vogt connect         # what a client needs to reach this instance
```

Record `instance_id`, `principal`, `data_dir`. If you run more than one Vogt
instance (a development one and a production one, say), importing into the
wrong one is silent and only shows up later as "the project isn't there".

Then confirm your credential can write projects:

- `project.register`, `project.import`, `contract.check`, `sweep`,
  `forge.onboard` and `forge.writeback` all need scope **`project.write`**.
- A typical agent token carries only `read` + `work.write`. It **cannot**
  register a project; `vogt status` shows the scopes of the token in use.
- So the import is run either from the CLI inside the instance
  (`docker exec vogt vogt project register ...` against the container, or
  `uv run vogt ...` in a source checkout), or with a token minted for the job
  (`vogt token issue --scopes read,work.write,project.write ...`, which only
  a local process with access to the data directory can do).

DECISION if no `project.write` path is available: stop; ask for a token or for
the CLI to be run on your behalf. Do not fall back to recording the project as
work items.

## 1. Establish provenance — which remote is authoritative

Do this before anything else, because `repo_url` is what every forge collector
keys off.

```
git -C <root> remote -v
gh api repos/<owner>/<name> -i | head -1      # 200 after a redirect is still a redirect
gh api repos/<owner>/<name> --jq '.full_name, .archived, .default_branch'
gh api orgs/<owner>/repos --jq '.[].full_name' | grep -i <name>
```

`gh api` follows GitHub's transfer redirects and returns `200`, so a URL that
"works" is not evidence the repo still lives there. `full_name` in the body is
the authority; cross-check with the org listing.

- rustnzb: the remembered URL `github.com/<old-org>/rustnzb` redirects.
  `full_name` is `TheDancingDeveloper-org/rustnzb`, and the repo is absent from
  the old organisation's listing. The canonical remote is
  `https://github.com/TheDancingDeveloper-org/rustnzb`.

Also check for other copies of the same code that are not the remote: a
self-hosted Forgejo/Gitea mirror, a second working tree, a sibling directory
with a different name. List them in the import note. A single
source of truth is a claim about *all* the copies, not just the one you found
first.

DECISION when two remotes both look live (e.g. GitHub and Forgejo both take
pushes): which one Vogt tracks, and whether the other becomes a mirror or is
retired. Do not register until this is answered.

## 2. Decide register vs import

Two different operations, and the choice is not cosmetic:

| | `project register` | `project import` |
|---|---|---|
| What it does | records an existing path as a project | clones the named GitHub or configured Forgejo repo into `import_root`, registers *that*, then consolidates |
| Working tree | the one you already have | a new one, server-side |
| Use when | the local tree is the tree people work in | the repo lives on a configured forge and there is no local tree of known ancestry |

`project import` accepts `owner/name` for GitHub, `host/owner/name` for a
configured Forgejo host, or an explicit HTTPS/SSH repository URL. It
deliberately creates its own checkout so that later divergence
is news rather than ambiguity. That is right for a repo you have never had
locally — and wrong for a repo the user actively develops in, because it
manufactures the second copy you were trying to eliminate.

- rustnzb has a live working tree at `/srv/work/rustnzbd`
  on branch `release/v1.4.5`. → **register the local path**, and set `repo_url`
  to the canonical remote from phase 1. Then run `forge onboard` explicitly to
  get the consolidation step `import` would have done.

Confirm the instance can actually *read* `root_path` before registering: the
collectors run inside the Vogt process, so in a container the repository
must be bind-mounted into it (see `docs/CUSTOMISATION.md`) and the registered
path must be the one *inside* the container. If an existing
project under the same parent directory has `git-local` and `source-markers`
sweeps reporting `ok`, that directory is visible to this instance. Check
`coverage` after phase 5 to prove it for yours.

## 3. Pre-flight review (read-only)

Run each of these and write down the answer; this is the review the human
reads before approving the write.

1. **Name and slug.** The slug is derived from the display name. Directory
   name, crate/package name and repo name may all differ — pick the one people
   say out loud, and note the mismatch. rustnzb: directory `rustnzbd`, repo and
   product `rustnzb`. → name `rustnzb`, slug `rustnzb`; the trailing `d`
   is a local directory artefact and should not enter Vogt.
   Check the slug is free: `project list`, or `project get --slug <slug>`.
2. **Contract, dry run.** `vogt contract check --path <root>` (no `--project`,
   so nothing is stored). The default contract v1 wants files
   `AGENTS.md`, `README.md`, `LICENSE` and directories `docs/`, `design/`,
   `src/`. Record every failing criterion by name.
   rustnzb fails three: no root `LICENSE` file (though `Cargo.toml` declares
   `license = "MIT"`), no `design/`, no `src/` (it is a Cargo workspace —
   code lives in `crates/` and `apps/`).
3. **Lifecycle state.** `incubating` / `active` / `maintenance` / `archived`,
   judged from commit recency and releases, not from vibes.
   rustnzb: released `v1.4.5` four days before import → `active`.
4. **Existing work to consolidate.** Open issues, PRs, labels, releases:
   `gh api repos/<full_name> --jq '.open_issues_count'`, `gh pr list`,
   `gh release list -L 5`. This sets expectations for phase 5 — an empty
   consolidation is only good news if upstream really is empty.
5. **Exclusions.** Defaults are `.venv/ node_modules/ target/ dist/ build/ .git/`.
   Add anything large or generated that is specific to this repo.
   rustnzb also carries `.ci-output/`, `.ci-artifacts/`, `TestData/`, `demo/`,
   `target/` under `benchnzb/` and `desktop/`.
6. **Working tree state.** `git status --porcelain`, current branch. A dirty
   tree or a non-default branch is not a blocker, but it is context for the
   first sweep and belongs in the reason string.
   rustnzb: on `release/v1.4.5`, three untracked `.claude/` directories.

Failing the contract does **not** block anything: "the contract is a value you
read, not a barrier you pass". Record the failures, do not fix the repo to
please it, and do not skip the import over them.

DECISION on contract failures that are structural rather than sloppy — a Rust
workspace has no `src/` and never will. Either accept a permanent
`non_compliant` for those criteria, or raise the question of a per-language
contract. Do not invent an empty `src/` to score a pass.

## 4. Register (first write)

Every write takes a `reason`, and it is audited. Write a reason a stranger
could act on: what, why, and a reference.

```
vogt project register \
  --name rustnzb \
  --root-path /srv/work/rustnzbd \
  --repo-url https://github.com/TheDancingDeveloper-org/rustnzb \
  --lifecycle-state active \
  --reason "Onboard rustnzb; canonical remote is TheDancingDeveloper-org after the org transfer."
```

Verify: `project get --slug rustnzb` returns the record, `repo_url` matches
phase 1, `root_path` matches phase 2, `trust_state` is `unverified` (expected —
nothing has corroborated the declaration yet).

## 5. Collect, consolidate, check

In this order, each with its own reason:

```
vogt sweep --project rustnzb --reason "First sweep after onboarding rustnzb."
vogt forge onboard --project rustnzb --reason "Consolidate existing forge issues, PRs, labels and releases for rustnzb."
vogt contract check --project rustnzb --reason "Record rustnzb's contract status at onboarding."
vogt drift detect --reason "First drift pass after onboarding rustnzb."
```

- `sweep` runs the collectors; narrow it to the new project so you are not
  paying for every registered project.
- `forge onboard` is read-only upstream. It is what `project import` would have
  run for you; registering a local path skips it, so run it explicitly.
- `contract check --project` (not `--path`) is what records the status —
  note `contract-checker` shows `never_run` in `coverage` until some project
  has been checked this way.
- `drift detect` compares what the repo declares against what was observed.

Then read it back and confirm the numbers are plausible:

```
vogt coverage                      # every collector ok/partial, and how stale
vogt project brief --slug rustnzb  # version, CI status, bugs, backlog, freshness
vogt compliance --project rustnzb  # the recorded contract result and its age
vogt drift list --project rustnzb
```

A brief that shows `observed_version` matching the repo's actual release, and a
`ci_status` matching what the configured forge says, is the signal the import landed.
`gh-posture` reporting `partial` for one project is a known collector wart, not
an import failure — read its `detail` before treating it as one. If no forge
token is configured for the project's host, `forge onboard` and the forge
collectors report "not collected" rather than an empty upstream; that is the
optional integration being absent, not a finding. Public repositories can
still be cloned by `project import` without a token, but consolidation waits
until the corresponding host credential is configured.

## 6. Write-back policy — the last decision, made explicitly

New projects default to `write_back: none`. Vogt only says things upstream once
someone chooses that:

```
vogt forge writeback --project <slug> --mode none|comment_only|full --reason "..."
```

Write-back is additive and forward-only (create, comment, label, close/reopen)
but it is still visible to everyone watching that repo.

DECISION, always human: leave at `none` for a first import. Move to
`comment_only` or `full` only after a human has watched a full sweep and agrees
with what Vogt believes. `forge actions` is the ledger of what it has said and
what landed.

## 7. Write the import note

Append to the project's own `AGENTS.md` (or `docs/`) a short section recording:
the Vogt slug and instance, the canonical remote and any redirect you resolved,
which other copies exist and what their status now is, the contract criteria
that fail and whether that is accepted or open, and the write-back mode. This
is the part that makes "single source of truth" true for the *next* agent,
who will otherwise re-derive all of phase 1 from a stale memory of the URL.

---

## Failure modes this ordering is designed to prevent

- **Registering a redirected remote.** Phase 1 before phase 4 — collectors key
  off `repo_url`, and a stale owner makes every forge sweep quietly wrong.
- **Manufacturing a second working tree.** Phase 2's table. `project import` is
  the right verb only when there is no local tree worth keeping.
- **A project registered at a path the instance cannot read.** Phase 2's
  visibility check and phase 5's `coverage` read-back.
- **Fixing the repo to satisfy the contract.** Phase 3 records failures; it
  does not act on them. A `src/` created to score a pass is a lie in the tree.
- **Silent write-back.** Phase 6 is opt-in, human, and last.
- **An audit trail nobody can use.** Every write carries a reason naming the
  driver, not "onboarding".
