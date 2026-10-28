# Accepted differences between the Python core and the Rust core

These are the halves of the port that the weekly rebase (see
`docs/rust-port/weekly-rebase.md`) left as follow-ups because the Rust core
does not yet have the infrastructure they need. They are accepted until the
named work item lands. A parity failure that is one of these is not a
regression; any other divergence is.

## A1 — scheduler (WI-2021)

The Python core runs a collector scheduler. The Rust core has no scheduler
process, so sweep timing, the recheck interval and anything that exists only
as a scheduled tick are Python-only. A caller still runs `sweep` itself on
both cores.

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
