# Known flakes and quarantined tests

Quarantined tests are skipped, never deleted. Each one has a Vogt item to
restore it. The fix PR removes the skip and this entry, and cites the same
item. See the workspace `flaky-quarantine` rule.

| Test | Symptom | Vogt item | Quarantined |
|---|---|---|---|
| `web/tests/browser/gui.spec.ts` › terminal swipe owns moves saved rows once (`phone`, `phone-android`) | First visible row moves a different number of rows than `floor(120 / row height)` (e.g. 118 expected, 122 actual); fails every run since WI-2694. `test.fixme`. | WI-2710 | 2026-10-30 |
| `web/tests/browser/terminalSize.spec.ts` › a phone draws a wider PTY's repaints without ghost frames (`phone`) | Fails on the phone project with or without the batch; not in any required step. `test.fixme`. | WI-2714 | 2026-10-30 |
