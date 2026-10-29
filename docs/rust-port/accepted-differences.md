# Accepted differences between the Python core and the Rust core

These are the halves of the port that the weekly rebase (see
`docs/rust-port/weekly-rebase.md`) left as follow-ups because the Rust core
does not yet have the infrastructure they need. They are accepted until the
named work item lands. A parity failure that is one of these is not a
regression; any other divergence is.

## A1 — watch schedule (WI-2021)

The collector schedule is ported: `vogt-core serve` sweeps every
`sweep_interval_seconds` and then settles budget stops, unless `--no-schedule`
or an interval of zero. The watch schedule is not. `evaluate_watches` is not
ported, so `watch_interval_seconds` is unread and no watch loop runs. A caller
still runs `sweep` itself on both cores.

## A2 — digest rows (WI-2022)

Session digest rows (turn counts, context tokens, compaction) are computed
and stored by the Python path. The Rust core does not yet write those rows,
so a read that joins them can be empty on Rust where Python has a number.
The field is absent or null, not a different shape.

## A3 — Codex resubmit loop (WI-2023)

Python resubmits a Codex turn when the CLI reports a retryable stop. The
Rust core does not yet run that loop, so a Codex session that would have
been resubmitted stops instead. The session record itself is the same shape.

## A4 — model policy on `project.update` (WI-2024)

`project.update`'s `agent_policy` does not yet enforce the WI-1590 model
policy (allowed models, never-models) on the Rust core. The field is stored
and returned; the refusal Python raises for a disallowed model is not.

## A5 — config keys parsed and not read (WI-2671)

`UNREAD_KEYS` in `engine/core/src/config.rs` is the closed list. Each key is
parsed and stored; nothing outside that module reads it. `sweep_interval_seconds`
is not on the list: the collector schedule reads it (A1).

- `watch_interval_seconds` (WI-2021). See A1. A loop here would expire watches
  `evaluate_watches` cannot deliver.
- `forge_posture_interval_seconds` (WI-2021). Python runs forge-posture on its
  own cadence. Rust runs it inside `sweep` when a forge is configured.
- `github_rate_limit_floor` and `github_app_installation_id` (WI-2021). The
  Rust forge client reports a 403 and does not keep a rate budget or GitHub
  App installation auth. Calls use the configured token.
- `usage_accounts`, `usage_litellm_url`, `usage_litellm_keys`,
  `usage_daily_spend_alert_usd` (WI-2666). `usage.board` is registered and
  not ported. These keys are its only inputs.
- `cold_wake_compaction`, `cold_wake_threshold_tokens`, `cold_wake_ttl_seconds`
  (WI-2588). Cold-wake compaction is decided when a session wakes. This core
  stores the per-session opt-out and does not decide compaction.
- `session_scratch_project` (WI-2666). Python resolves a scratch project when
  `session.start` names no project. Rust `session.start` requires one.
- `autopilot_done_requires_result_report` (WI-2666). Python refuses to treat
  an autopilot session as finished until a result report. `session.report` is
  not ported.
- `image_digest` (WI-2666). Python's `instance.diagnostics` returns the digest
  the deployment stated. That operation is registered and not ported.

## A6 — read-API residue on a core-only process (WI-2672)

A live comparison of this core against vogt-dev still differs on these rows.
They are the engine's data, not a missing core field. A normaliser that drops
them is correct for a core-only preview.

- `status.disk` is null. The core does not stat the workspace; the engine does.
- `project.list` `writable_reason` adds "no forge credential resolves" when no
  token file resolves. Dev has a credential; the preview does not.
- `engine.status` is a short unavailable string. Python returns a typed object
  once an engine is configured. This build has no engine client for that read.
- `session.history_list` is empty. Those rows live in the engine's history.
- `inbox.list` `coverage.agent` reads unconfigured, and session reports are not
  joined, because there is no engine transcript store.

Already the same shape on both cores, so not repeated here: `project.brief`
checks and `dependencies.detail`, `place.metrics` `work_total`, the
deploy-lanes collector when a lane is configured, `initiative.list`
`mode=summary`, `preference.set` `dispatch.budget_restarts_per_item`, and
`clone` with no HTTP route (Python declares none either).
