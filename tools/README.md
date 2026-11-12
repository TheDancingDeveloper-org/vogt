# Operator tools

One-off tools that are not part of the shipped `vogt` package. They are
Python, stdlib-only, and run against an instance's public API.

## `vogt_snapshot_import.py` (WI-4125)

Imports a Vogt tracker **snapshot** from one instance into another:

- projects, initiatives, labels and the assignee actors they need;
- work items with their state, priority, effort, labels, initiative and
  relations;
- comments, preserving `note` / `handoff` / `verdict` / `verification` kind
  and the structured fields of the last three.

It never moves sessions, tokens, forge credentials, agent-auth material,
observations, audit rows, events, decisions, charters, watches, inbox rows,
or engine state. The source export contains none of those.

### Produce the source export

Use the source instance's own `export` operation with a **read** token. That
operation is the sanctioned read path for an instance you must not modify;
this tool never opens the source database. The export is one format-2 JSON
document (the operation caps at 10,000 work items; a larger instance needs a
database-level copy instead).

The WI-4125 dry run wrote one with the source MCP tool:

```text
export(destination="/home/sprooty/Working/.wi4125-export/prod-snapshot.json",
       reason="WI-4125 read-only prod export")
```

### Dry run (the default)

Nothing is written to the target without `--apply` **and** `--confirm`.

```bash
uv run python tools/vogt_snapshot_import.py \
  --source /path/to/export.json \
  --target-url https://vogt-dev.example \
  --token-file /path/to/dev-token \
  --plan-out /tmp/vogt-import-plan.json
```

Reading the target needs only `read`; applying needs `admin`, `work.write`
and `project.write` for the project registrations.

### Apply

```bash
uv run python tools/vogt_snapshot_import.py \
  --source /path/to/export.json \
  --target-url https://vogt-dev.example \
  --token-file /path/to/dev-token \
  --apply --confirm \
  --reason "WI-4125: approved prod snapshot import"
```

Useful switches:

| flag | meaning |
| --- | --- |
| `--prod-host HOST` | add a hostname to the production list (repeatable); `vogt.sprooty.com` and any host containing `vogt-prod` are always on it |
| `--i-know-this-is-not-prod` | override the production-**hostname** refusal only; a target that reports the snapshot's own instance id is still refused |
| `--limit-items N` | resumable pilot: create only the next N work items, and only the relations/comments between them |
| `--skip-comments` / `--skip-relations` | leave that phase out |
| `--no-comment-provenance` | omit the `_prod comment by … at …_` prefix |
| `--superseded-policy wont_do` | default: keep the WI number of a retired item, as `wont_do` with a note |
| `--ref-policy fresh` | do not require or check WI alignment; every item gets a new id and ref |

### The prod guard

Reading the target's `status` also reads its instance id. The importer refuses
before planning, and therefore before any write, when:

- the target reports the same instance id as the snapshot's `instance_id` —
  the run would write the snapshot back into the instance it came from. No
  flag overrides this; and
- the target hostname is `vogt.sprooty.com`, contains `vogt-prod`, or is
  listed with `--prod-host`. Only `--i-know-this-is-not-prod` overrides this
  hostname check, and it does not weaken the instance-id check.

`--apply` also refuses a target whose `status` reports no instance id, because
the importer cannot then prove the target is not the source.

The state file defaults to
`~/.local/state/vogt-snapshot-import/<source-instance>-<target-digest>.json`.
Every item, relation, project and comment is recorded after it is written, so
the same command resumes where it stopped. A second full run is a no-op.

### What is and is not preserved

The target MCP surface has no parameter for identity. `work.create` mints
its own `wrk_` id and takes the next `WI-` number from its counter;
`work.comment` attributes the comment to the calling actor; every write
stamps `created_at` and `updated_at` itself.

| field | result |
| --- | --- |
| `WI-` reference | **preserved** when the source refs are contiguous from WI-1 (all current prod rows are). An existing WI-1 is updated in place and the rest are created in order; the tool aborts if the target hands back any other ref. |
| `wrk_` id | new |
| `created_at` / `updated_at` | import time |
| title, body, kind, state, priority, effort, labels, initiative, project | preserved |
| relations (four declarable kinds) | preserved |
| comment body and kind | preserved; the author and original time go in the provenance prefix |
| comment authorship | the importing actor |
| `superseded_by` | replaced by `wont_do` plus a body note naming the upstream subject |
| `origin` of an adopted item | a native item; its forge subject is not in the export |
| project `root_path` | rewritten to `<target import root>/<slug>` |
| project `write_back` / `link_state` | forced `none` / `unlinked` |
| project `agent_policy`, `mcp_extra`, trust and compliance fields | not imported |
| audit rows and events | not imported; the target audits the import writes it makes |

If exact ids and timestamps matter, the only complete path is a **database
copy** of the declared store (dev's schema is forward-migratable from prod's:
31 vs 30 on 2026-11-12). That needs write access to the target's PVC, which
this tool does not take.

### Forge safety

The critical rule for a dev snapshot is that dev never comments on a GitHub
or Forgejo issue. `project.register` creates an unlinked project whose
write-back is disabled, and before writing any work the importer re-reads
**every** imported project and refuses unless `write_back == "none"` and
`link_state == "unlinked"`. Work is also created with `local_only: true`.

### Tests

```bash
uv run pytest tests/test_vogt_snapshot_import.py \
             tests/test_vogt_snapshot_import_e2e.py --no-cov
```

The second file starts two throwaway cores (`vogt init` + `vogt serve` on
loopback), exports from one and applies into the other over the real MCP HTTP
transport. It touches neither prod nor dev.
