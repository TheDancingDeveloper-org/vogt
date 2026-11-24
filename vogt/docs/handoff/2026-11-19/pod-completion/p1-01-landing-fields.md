---
title: "brief.rs LandingFields accepts the full LandingPolicy (initiative_bases, long_lived_branches, source)"
labels: [pods, phase-1, bug, "model:sonnet"]
depends_on: []
vogt: WI-4417
lands: graft vogt/rust-2
---

## Why

vogt-dev refuses every session start for project `vogt`:

```
invalid_request: project vogt has an unreadable landing rule: unknown field initiative_bases
```

- `engine/core/src/application/brief.rs:193-207`: `struct LandingFields` is `#[serde(deny_unknown_fields)]` and lists only `base_branch`, `branch_pattern`, `worker_lands_by`, `merge_by`, `merge_mode`, `linear_history` and `deploy_by`.
- The core's canonical `LandingPolicy` (`engine/core/src/application/models/entities.rs:442-466`) also serialises `initiative_bases` (default `{}`), `long_lived_branches` (default `[]`) and `source` (default `null`). It has no `skip_serializing_if`, so any `project_update` writes them back.
- `landing_section` turns the parse error into `VogtError::InvalidRequest` (`brief.rs:163-168`). That error propagates through the brief (`brief.rs:532`).
- This also blocks the pod graft token. The mint needs `landing.source` set (P1.2), and setting it would trip the same parse error.

## What to change

Files: `engine/core/src/application/brief.rs` only.

- Accept the three fields: either add them to `LandingFields` with `#[serde(default)]`, or drop `deny_unknown_fields` and keep a test that the documented fields still parse.
- If `source` is present, the landing section may mention the line (e.g. "lands on graft line rust-2"). Rendering it is optional; accepting it is required.

## Done when

1. A unit test in `brief.rs` renders the landing section for a rule carrying `initiative_bases: {}`, `long_lived_branches: []` and `source: null`. A second case has a non-empty `initiative_bases` and a graft `source`. Both fail on the current code.
2. A test that an unknown field which is *not* part of `LandingPolicy` still gets a clear error, or a comment explaining why unknown fields are now accepted.
3. `cargo clippy -p vogt-core --all-targets -- -D warnings` and `cargo nextest run -p vogt-core brief` pass.

## Note

This item's earlier worker (`ses_01M7TY7M…`) is blocked because its oversight-minted token was on the engine's emptyDir and was lost in the 22:47Z restart. Start a fresh worker with a fresh token.
