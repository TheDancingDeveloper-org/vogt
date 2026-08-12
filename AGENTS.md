# Vogt — Agent Guidance

Design-phase project: a standalone, self-hosted, open-source product
development environment (Jira-like scope) built AI-native. Python is the
implementation language.

## Where things live

- `docs/DESIGN.md` — the design outline; source of truth for architecture,
  domain model, and roadmap. Update it when decisions change; don't fork
  competing design docs.
- `design/` — diagrams, mockups, exploratory notes (may be messy).
- `src/` — implementation (empty until M0 starts).

## Ground rules

- **Cadastre is prior art, not a dependency.** It lives at
  `~/Working/Active/cadastre` and is the same author's work — read it freely
  for patterns (declared/observed split, trust ledger, transport parity,
  audit design), but do not import from it or couple to its API in v1.
- Key inversions vs cadastre — do not regress these (`docs/DESIGN.md` §2):
  observed-first visibility, explicit collector coverage, a real write
  plane, actors/people as core entities.
- Scope decisions that are easy to reintroduce by accident — don't
  (`docs/REQUIREMENTS.md` §3 lists all of them with reasons):
  - **Nothing enforces.** Compliance, trust, and drift are values to be
    read; no operation may take them as a precondition (FR-G13).
  - **Nothing discovers.** Collection scope is the registered project
    list. No crawling roots, no candidate listings, no re-checking on a
    timer (FR-G15).
  - **No lockfiles, no resolved versions.** Dependency edges are path/git
    references between projects, nothing more (FR-D1).
  - **Observed-first is gated by promotion + suppression + exclusions**
    (`docs/DESIGN.md` §3.6) — never wire raw markers into ranked views.
  - AI-assisted drift detection is a **non-committed stretch goal**; no
    requirement or interface may assume it exists.
- Every feature must be reachable via CLI, REST, and MCP with tested parity;
  the GUI consumes the same HTTP adapter.
- Every write requires a principal and a reason (audit table).
- mypy strict + ruff from the first commit; no unmigrated schema changes.
