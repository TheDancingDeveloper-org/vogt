---
title: "Live acceptance on vogt-dev: 5 Rust worker pods land, then survive an engine restart (graceful and forced)"
labels: [pods, phase-1, acceptance, "model:opus"]
depends_on: [P0.1, P1.1, P1.2, P1.3, P1.4, P1.5, P1.6, P1.7, P1.8, P1.10 — all deployed to vogt-dev]
vogt: WI-3169 (closes), WI-3158, WI-3160, WI-3161, WI-3244 (close on evidence)
lands: no code. An oversight session drives it; the operator approves the window and both engine restarts.
---

## Setup

- An announced window with no other dev sessions (Q9).
- Pre-check the following, and record the output:
  - `kubectl -n vogt-dev get deploy vogt-dev-engine -o yaml | redact` shows the expected image and env;
  - `kubectl -n vogt-sessions describe quota` shows enough headroom;
  - `project_get vogt` shows a graft `landing.source`.
- Five tiny real briefs on vogt `rust-2`. Each is: add a failing unit test (red), make it pass (green), then run the gates and land. Use small, disjoint, genuinely useful changes, e.g. doc-test or unit-test gaps from the backlog, one crate each.

## Steps and done-when

1. **Five workers.** Start five worker sessions concurrently (worker template, no `placement`). Each must:
   - boot as a pod on the rust image with a minted graft token (no `graft token unavailable` in the engine log);
   - run `cargo fmt --check`, `cargo clippy -p <crate> --all-targets -- -D warnings` and `cargo nextest run -p <crate>`;
   - `g land` on `rust-2` and post a hand-off.

   Record per pod: boot time, build times, peak memory and ephemeral (`kubectl top`, `du`), node, and the MintToken/RevokeToken lines in graft-dev's audit.
2. **Graceful engine restart.** While at least 3 of the 5 are inside a cargo build, run `kubectl -n vogt-dev delete pod <engine-pod>` (graceful). When the new engine is Ready, all 5 must show:
   - the same session ids;
   - unchanged pods (same pod UID, restartCount 0);
   - screens that continue from where they were;
   - a finished brief that lands.
3. **Forced engine restart.** Repeat step 2 on a second set of 5 sessions (or the same 5 before they land) with `--grace-period=0 --force`. All resume.
4. **Credentials and in-pod tools.** During the run, the engine log shows at least one `/api/agent-auth/store/bundle/bundle-file` 2xx per pod, and one refreshed-credential write reaches the account dir (closes WI-3244 and the write-back part of WI-3160). In one pod, all succeed:
   - `vogt-agent-auth fetch` of an allowed name;
   - a cadastre `lookup`;
   - `echo $AGENT_AUTH_MODE` printing `brokered` (closes WI-3161).
5. **Hibernate and wake.** One session is hibernated and woken, and continues its conversation (it answers a question about its earlier turn). This closes WI-3158's live proof. WI-3169 gets the numbers table and is closed.

## Fail handling

Any failed step is a new bug item with the session id, timestamps and redacted logs. The acceptance is re-run in full after the fix lands.
