//! Migration SQL embedded at compile time.
//!
//! The stack image ships the binary without `src/vogt`, so the SQL cannot
//! be read from the checkout at runtime. A migration added on `main` is
//! picked up by the next build; `VOGT_MIGRATIONS_DIR` still overrides this
//! for a checkout that is ahead of the binary.
//!
//! Generated from the files on disk. Regenerate rather than editing by hand.

pub const DECLARED: &[(&str, &str)] = &[
    ("0001_foundation", include_str!("../../../../../src/vogt/storage/sqlite/migrations/declared/0001_foundation.sql")),
    ("0002_work", include_str!("../../../../../src/vogt/storage/sqlite/migrations/declared/0002_work.sql")),
    ("0003_observed_first", include_str!("../../../../../src/vogt/storage/sqlite/migrations/declared/0003_observed_first.sql")),
    ("0004_drift", include_str!("../../../../../src/vogt/storage/sqlite/migrations/declared/0004_drift.sql")),
    ("0005_tokens", include_str!("../../../../../src/vogt/storage/sqlite/migrations/declared/0005_tokens.sql")),
    ("0006_writeback", include_str!("../../../../../src/vogt/storage/sqlite/migrations/declared/0006_writeback.sql")),
    ("0007_sessions", include_str!("../../../../../src/vogt/storage/sqlite/migrations/declared/0007_sessions.sql")),
    ("0008_superseded_drift", include_str!("../../../../../src/vogt/storage/sqlite/migrations/declared/0008_superseded_drift.sql")),
    ("0009_inbox_triage", include_str!("../../../../../src/vogt/storage/sqlite/migrations/declared/0009_inbox_triage.sql")),
    ("0010_session_model", include_str!("../../../../../src/vogt/storage/sqlite/migrations/declared/0010_session_model.sql")),
    ("0011_contract_adoption", include_str!("../../../../../src/vogt/storage/sqlite/migrations/declared/0011_contract_adoption.sql")),
    ("0012_forge_accounts", include_str!("../../../../../src/vogt/storage/sqlite/migrations/declared/0012_forge_accounts.sql")),
    ("0013_upstream_truth", include_str!("../../../../../src/vogt/storage/sqlite/migrations/declared/0013_upstream_truth.sql")),
    ("0014_native_migration", include_str!("../../../../../src/vogt/storage/sqlite/migrations/declared/0014_native_migration.sql")),
    ("0015_work_overlay_branches", include_str!("../../../../../src/vogt/storage/sqlite/migrations/declared/0015_work_overlay_branches.sql")),
    ("0016_perf_indexes", include_str!("../../../../../src/vogt/storage/sqlite/migrations/declared/0016_perf_indexes.sql")),
    ("0017_password_credentials", include_str!("../../../../../src/vogt/storage/sqlite/migrations/declared/0017_password_credentials.sql")),
    ("0018_actor_preferences", include_str!("../../../../../src/vogt/storage/sqlite/migrations/declared/0018_actor_preferences.sql")),
    ("0019_session_grants", include_str!("../../../../../src/vogt/storage/sqlite/migrations/declared/0019_session_grants.sql")),
];

pub const OBSERVED: &[(&str, &str)] = &[
    ("0001_foundation", include_str!("../../../../../src/vogt/storage/sqlite/migrations/observed/0001_foundation.sql")),
    ("0002_evidence", include_str!("../../../../../src/vogt/storage/sqlite/migrations/observed/0002_evidence.sql")),
    ("0003_inherited_dep_refs", include_str!("../../../../../src/vogt/storage/sqlite/migrations/observed/0003_inherited_dep_refs.sql")),
    ("0004_forge_sync", include_str!("../../../../../src/vogt/storage/sqlite/migrations/observed/0004_forge_sync.sql")),
    ("0005_perf_indexes", include_str!("../../../../../src/vogt/storage/sqlite/migrations/observed/0005_perf_indexes.sql")),
    ("0006_agent_activity", include_str!("../../../../../src/vogt/storage/sqlite/migrations/observed/0006_agent_activity.sql")),
];
