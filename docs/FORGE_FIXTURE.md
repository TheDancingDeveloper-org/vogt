# The forge test fixture

Vogt's forge adapter is normally tested against a fake HTTP transport
([`tests/test_forge_provider.py`](../tests/test_forge_provider.py)), which is
fast and deterministic but never meets real GitHub pagination, PAT scopes,
check runs, or the write-back verbs. This fixture closes that gap: a small,
real repository, driven to a **known state**, that an opt-in test suite reads
and writes.

Three pieces make it up:

| Piece | Path |
|---|---|
| The known state, as data | [`tests/fixtures/forge_fixture_manifest.json`](../tests/fixtures/forge_fixture_manifest.json) |
| The script that builds it | [`scripts/fixture_reset.py`](../scripts/fixture_reset.py) |
| The opt-in test suite | [`tests/test_forge_live.py`](../tests/test_forge_live.py) |

## What the fixture contains

The manifest is the single source of truth; the script builds to it and the
tests grade against it. Its `expected` block records the counts the live reads
must produce:

- **Labels** — `bug`, `enhancement`, `documentation`, `good first issue`,
  `wontfix`.
- **Milestones** — two open (`v0.1 - first cut`, `v0.2 - hardening`).
- **Issues** — five, of each kind: open bug, open enhancement, closed bug,
  open documentation, closed `wontfix`. Three open, two closed, each labelled.
- **Pull requests** — four: one open, one draft, one merged whose body says
  `Closes #3`, and one open whose CI workflow is expected to fail.
- **Branches** — four, in the #283 naming pattern (below), which double as the
  pull requests' head branches.
- **Files** — a module carrying `TODO`/`FIXME` markers, a `requirements.txt`
  dependency manifest, and a `.github/dependabot.yml` so posture reads have
  something to find.

### The #283 branch-naming pattern

Issue #283 states the convention: a branch is *that work item's* branch when
its name contains the item's reference. The fixture exercises three shapes of
it, keyed on the forge issue number (the shape used for linked projects):

- `gh-<n>-<slug>` — e.g. `gh-1-open-bug`, `gh-2-pagination`
- `feature/gh-<n>-<slug>` — e.g. `feature/gh-4-cache`
- `wi-<n>/<slug>` — e.g. `wi-3/readme-typo`

> The pattern above is taken from issue #283's proposal (any branch containing
> the work item ref or the linked forge number). If #283 later settles on a
> narrower default pattern in `config.py`, update the manifest's `branches`
> and `branch_patterns` to match.

## Building the fixture (a human, once)

**This is a deliberate operator action, not part of any test run or CI job.**
The repository is created **once, by a person**, and then rebuilt by the
script as needed. Nothing in the automated suite ever creates it.

1. Create the empty repository once:

   ```sh
   gh repo create TheDancingDeveloper-org/vogt-fixture --private \
     --description "Vogt forge test fixture" --add-readme
   ```

   The script needs the default branch to already have a commit; `--add-readme`
   provides one.

2. Provide a PAT with `repo` scope, plus `workflow` scope for the
   failing-checks pull request's CI file. Then rebuild:

   ```sh
   # token from a file (preferred), or --token, or $VOGT_FIXTURE_TOKEN / $GH_TOKEN
   uv run python scripts/fixture_reset.py --token-file ./pat

   # review the plan first, touching no network:
   uv run python scripts/fixture_reset.py --dry-run
   ```

The script is **idempotent** and **never destroys history**: it finds each
object by a stable key (label name, issue title, pull-request head branch) and
creates it only when absent, then relabels and closes/reopens to match. It
issues no DELETE and never force-pushes. Re-running it reconciles drift back to
the manifest without renumbering anything.

> GitHub shares one counter between issues and pull requests. On a *fresh*
> repository built in manifest order the five issues take `#1`–`#5` and the
> four pull requests `#6`–`#9`, which is why the manifest's `number` and
> `Closes #n` fields hold. After the first build the script never renumbers.

## Running the live suite

The live tests carry the `live_forge` marker and are **skipped by default** —
they never run in `uv run pytest` or CI. Two guards enforce this:

- `tests/conftest.py` skips every `live_forge` item unless the run selected
  them with `-m live_forge`.
- The tests skip themselves when no `VOGT_FIXTURE_TOKEN` / `GH_TOKEN` is set,
  so `-m live_forge` on a machine without credentials skips cleanly.

```sh
# default: the live tests are collected and SKIPPED, never run or failed
uv run pytest

# opt in, with credentials and the fixture repo present:
VOGT_FIXTURE_TOKEN=ghp_xxx uv run pytest -m live_forge
```

Override the repository with `--repo owner/name` on the script or
`$VOGT_FIXTURE_REPO` for the tests; both default to the manifest's `repo`.

The manifest-consistency tests in `tests/test_forge_live.py` carry no marker,
so they *do* run in the ordinary suite and keep the manifest internally
coherent without a network.
