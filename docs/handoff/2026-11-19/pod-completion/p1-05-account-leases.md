---
title: "Account leases: recovered-but-hibernated sessions hold no slot (boot wake refused 'account at capacity')"
labels: [pods, phase-1, bug, "model:sonnet"]
depends_on: []
vogt: new (raise under WI-4426)
lands: graft vogt/rust-2
---

## Why

After the 2026-11-19 22:47Z restart the engine recovered 12 sessions. It woke the 2 pinned-awake ones and refused every other wake:

```
INFO recovered a session as hibernated session=… (×12)
INFO boot wake refused; the session stays hibernated … the core answered 409 Conflict: account at capacity: cap 8; counts {"acct2": 8}
```

## Root cause

- `recover_records` (`engine/server/src/sessions.rs:4731-4757`) calls `account_pool.restore(record.account)` for **every** recovered record that was live, then keeps the lease in `account_leases`.
- `AccountPool::restore` (`engine/server/src/account_pool.rs:80-89`) increments the count with no cap check.
- A wake reuses a restored lease (`sessions.rs:1690-1722`).
- A recovered session that is not woken therefore holds its slot indefinitely. Only `keep_awake` sessions wake at boot.
- A session that was already hibernated before the restart has no restored lease, so its wake needs a fresh `lease()`. That fails while the sleeping recovered sessions hold every slot.
- Dev has one account (`acct2`) with the default cap 8 (`claude_config.rs:320-324`).

## What to change

Files: `engine/server/src/sessions.rs` (recovery and wake lease handling only), `engine/server/src/account_pool.rs`, and tests. Do not run this alongside P1.8, which also edits `sessions.rs`.

1. A recovered record holds **no** lease while it is hibernated. Remember its recorded account on the record so a wake prefers it (sticky) but takes a real lease through `lease()`, subject to the cap.
2. Boot wakes pinned (`keep_awake`) sessions first. If a pinned wake hits the cap, it is logged and retried when a slot frees (or by the next boot wake pass), not dropped silently.
3. `AccountPool::restore` either goes away or is cap-checked. No path increments past the cap without a 429.

## Done when

1. Test: 12 recovered records on account `a` with cap 8, 2 of them pinned. After boot `counts()["a"] == 2`. Six further wakes succeed and the 7th gets 429 with the counts. It fails on the current code.
2. Test: a wake of a recovered session reuses its recorded account when that account has room.
3. The existing `account_pool` tests and the sessions wake tests are green.
4. The hand-off notes the dev cap decision (Q3), since it is config, not code.
