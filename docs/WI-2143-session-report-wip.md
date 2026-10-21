# WI-2143 — session.report port (WIP, stopped at hard stop 20:55Z)

Not started in code. Scope found:
- Python source of truth is on origin/main (not on rust-core):
  src/vogt/application/services/reports.py (SESSION_REPORT, txn.insert_session_report,
  inbox kind `session.report.<kind>`), registry entry src/vogt/registry/operations.py
  ("session.report", POST /sessions/report), models in application/models.py (~l.1025)
  and core/entities.py (~l.888, WI-2039 structured report).
- Rust targets: engine/core/src/registry/operations.rs (beside session.report_blocked ~l.742),
  engine/core/src/application/models/session_ops.rs, a new application/session_report.rs
  modelled on session_blocked.rs, storage table for session_report rows (migration),
  inbox projection, plus parity test.
