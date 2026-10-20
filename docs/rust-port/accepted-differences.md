# Rust port: accepted differences from the Python core

Behaviour where the Rust core (`engine/core`) deliberately differs from the
Python core (`src/vogt`), with the decision that accepted it. HTTP adapter
choices are in [`http-decisions.md`](http-decisions.md). Anything not listed
here or there is meant to match Python, and a mismatch is a bug.

Each entry names what it covers, what each core does, who decided, and what
would reopen it.

## A1. `engine.status`: a malformed nested row is `engine_unavailable`

- **Covers:** `engine.status` on CLI, REST and MCP, when the engine's
  `GET /api/status` answer has a `history`, `agent_tasks` or `event_lag` entry
  that pydantic's lax validation refuses (an unknown field, `"x"` for an int,
  `90.5`, and so on).
- **Python:** the pydantic `ValidationError` is not caught, so the call fails
  as an internal error (a 500 on REST).
- **Rust:** the call fails with `engine_unavailable`, and the message names the
  row: ``the session engine answered GET /api/status with a malformed `history` ``.
  A malformed `disk` row is dropped (`disk: null`) on both cores.
- **Decided:** operator, 2026-10-20 (WI-1081, decision 3.1). The engine
  answered something Vogt cannot read, and a typed error that says so is more
  useful than an unhandled one.
- **Code:** `engine/core/src/application/services/engine_status.rs`
  (`report`, `nested`).
- **Reopen if:** a client depends on the 500.

## A2. `engine.status`: integers past `i64` (resolved, not a difference)

- **Covers:** the same nested rows, plus `disk`, when a count is larger than
  `i64::MAX`.
- **Was:** Rust refused such an int, so `disk` was dropped and the other rows
  became `engine_unavailable`. Python's int is unbounded and keeps the value.
- **Decided:** operator, 2026-10-20 (WI-1081, decision 3.2): fix it, the value
  must round-trip as in Python.
- **Now:** every integer in the `u64` range round-trips exactly, as a JSON int
  or as a string of one (`"18446744073709551615"`), and the row is kept.
  Test: `an_int_past_i64_round_trips_in_a_nested_row`.
- **Residual:** an integer whose magnitude is past `u64` (more than 20 digits),
  or a negative one below `i64::MIN`. `serde_json` holds those only as an `f64`
  unless the crate-wide `arbitrary_precision` feature is on. That feature
  would change number handling across the whole workspace, and it breaks
  `#[serde(untagged)]` enums that carry numbers, such as
  `SessionWaitAnyParamsCursor`. The engine is Rust and reports its counts as
  `u64`, so it cannot send such a value. Rust still treats it as pydantic
  treats a float (an integral value within `i64` is kept, anything else is
  refused).
- **Reopen if:** an engine field could exceed `u64`.

## A3. Error paths where Python raises uncaught (proposed, awaiting operator review)

The WI-1081 drift ports (main @09d3c552) handle a few inputs that Python does
not catch. In each case Python fails with an unhandled exception (a 500 on
REST), and Rust degrades gracefully instead. The happy paths match.

- **GitHub App, non-JSON reply** (main c8d6211): Rust raises
  `github_unavailable`, where Python lets the JSON decode error escape.
- **GitHub App, unreadable private-key file** (main c8d6211/4b4dee6): Rust
  warns once and falls back to the token file, as it already does for a
  missing or broken key. Python lets the `OSError` escape.
- **Claim check, naive `merged_at`/`closed_at`** (main 78a3636): Rust reads a
  timestamp without an offset as UTC. Python raises a `TypeError` when it
  compares it with the session start.
- **Claim check, PR number in non-ASCII digits** (main 78a3636): Rust's
  `mentioned_pulls` drops it. Python's `\d` matches it.
- **GitHub rate budget** (main 1c59243): `reset_at` is kept as whole epoch
  seconds.

- **Reopen if:** the operator wants the uncaught behaviour reproduced.
