//! Instance lifecycle. Ports `init_instance` and the bootstrap it performs.
//!
//! Creates the data directory, migrates both databases, then writes the
//! instance: its id, the initiating actor, and one audit row. No event is
//! emitted and the revision stays 0, so a client that connects afterwards sees
//! an empty feed rather than an instance-creation event it cannot act on. The
//! token file is written only when `VOGT_BOOTSTRAP_TOKEN` is set, so a recorded
//! run can place a known secret without this binary minting one.

use std::path::Path;

use crate::application::context::Built;
use crate::application::services::bootstrap::{adopt_bootstrap_token, Bootstrap};
use crate::core::{Clock, IdFactory, SequentialIds};
use crate::errors::VogtError;
use crate::storage::interface::{DeclaredStore, MigrationReport, ObservedStore, ReadView};
use crate::storage::sqlite::migrator;
use crate::storage::sqlite::{declared_path, observed_path};

pub struct InitOutcome {
    pub declared: MigrationReport,
    pub observed: MigrationReport,
    pub created: bool,
    pub instance_id: String,
    /// `not_configured`, `already_present`, or `adopted`. Python reports the
    /// same three, and the CLI prints this rather than a hardcoded value.
    pub bootstrap_core_token: String,
    /// The same three outcomes for the brokered agent token.
    pub bootstrap_agent_token: String,
}

/// `clock` and `ids` are resolved once by the caller from the hook environment.
/// `None` is the wall clock and a fresh ULID, which is what a plain init does.
pub fn init(
    data_dir: &Path,
    clock: &mut Option<crate::core::StepClock>,
    ids: &mut Option<SequentialIds>,
) -> Result<InitOutcome, migrator::MigrateError> {
    init_with(data_dir, &serde_json::Map::new(), clock, ids)
}

/// `init` with config overrides on top of the file and the environment. The
/// bootstrap adoption reads its token files from the merged config, so a test
/// can point it at a file without touching the process environment, which the
/// other tests share.
pub(crate) fn init_with(
    data_dir: &Path,
    overrides: &serde_json::Map<String, serde_json::Value>,
    clock: &mut Option<crate::core::StepClock>,
    ids: &mut Option<SequentialIds>,
) -> Result<InitOutcome, migrator::MigrateError> {
    std::fs::create_dir_all(data_dir)?;
    let root = migrator::migrations_root();
    let declared_existed = declared_path(data_dir).exists();
    // The hook clock is spent only when a migration is actually applied. A
    // database that is already current gets the wall clock, so `serve`
    // re-checking it does not move every later row one tick past Python.
    let declared_due = pending_in(
        "declared",
        &declared_path(data_dir),
        root.as_deref().map(|path| path.join("declared")).as_deref(),
    )?;
    // Python's `migrate` reads the clock whether or not anything is pending, so
    // a no-op declared check still moves the hook clock one tick. Only a database
    // that does not exist yet is free: there is nothing to check.
    let now = stamp_if(clock, declared_existed || declared_due > 0);
    let declared = migrator::open_and_migrate(
        &declared_path(data_dir),
        "declared",
        root.as_deref().map(|path| path.join("declared")).as_deref(),
        "vogt-core",
        &now,
    )?;
    seed_workflows(data_dir, &now)?;
    let observed_due = pending_in(
        "observed",
        &observed_path(data_dir),
        root.as_deref().map(|path| path.join("observed")).as_deref(),
    )?;
    let observed_now = stamp_if(clock, observed_due > 0);
    let observed = migrator::open_and_migrate(
        &observed_path(data_dir),
        "observed",
        root.as_deref().map(|path| path.join("observed")).as_deref(),
        "vogt-core",
        &observed_now,
    )?;
    if let Some(token) = std::env::var_os("VOGT_BOOTSTRAP_TOKEN") {
        let path = data_dir.join("token");
        if !path.exists() {
            std::fs::write(&path, token.as_encoded_bytes())?;
        }
    }
    let instance_id = if !declared_existed {
        let instance_id = bootstrap(data_dir, clock, ids)?;
        bind_instance(data_dir, &instance_id, clock)?;
        instance_id
    } else {
        instance_id_of(data_dir)?
    };
    // The stack secret the engine presents. Python adopts it inside init; a
    // Rust core that skips it answers every engine call with 401. A missing
    // file is not an error; a short secret or an unknown scope refuses to start.
    let config = bootstrap_config(data_dir, overrides)?;
    let bootstrap_core_token =
        adopt_bootstrap_token(&config, data_dir, clock, ids, Bootstrap::Core)
            .map_err(|err| migrator::MigrateError::Message(err.message().to_string()))?;
    let bootstrap_agent_token =
        adopt_bootstrap_token(&config, data_dir, clock, ids, Bootstrap::Agent)
            .map_err(|err| migrator::MigrateError::Message(err.message().to_string()))?;
    Ok(InitOutcome {
        created: !declared_existed,
        declared,
        observed,
        instance_id,
        bootstrap_core_token,
        bootstrap_agent_token,
    })
}

/// Pending migrations on a database that already exists. Zero means a
/// Python-written database is already at this build's schema.
pub fn pending(data_dir: &Path) -> Result<(usize, usize), migrator::MigrateError> {
    let root = migrator::migrations_root();
    Ok((
        pending_in(
            "declared",
            &declared_path(data_dir),
            root.as_deref().map(|path| path.join("declared")).as_deref(),
        )?,
        pending_in(
            "observed",
            &observed_path(data_dir),
            root.as_deref().map(|path| path.join("observed")).as_deref(),
        )?,
    ))
}

const WORKFLOW_DEFINITION: &str = concat!(
    r#"{"initial_state": "open", "transitions": {"blocked": ["open", "in_progress", "wont_do"], "#,
    r#""done": ["open"], "in_progress": ["review", "blocked", "open", "wont_do"], "#,
    r#""open": ["in_progress", "blocked", "wont_do"], "#,
    r#""review": ["done", "in_progress", "blocked", "wont_do"], "wont_do": ["open"]}}"#,
);
const WORK_KINDS: [&str; 4] = ["feature", "bug", "chore", "question"];

/// The machine every kind starts with. Python seeds it after every migrate, so
/// a fresh instance and an upgraded one both carry the rows. An existing kind
/// is left untouched.
fn seed_workflows(data_dir: &Path, at: &str) -> Result<(), migrator::MigrateError> {
    use rusqlite::params;

    let conn = crate::storage::sqlite::connection::connect(&declared_path(data_dir))?;
    conn.execute("BEGIN IMMEDIATE", [])?;
    let written = (|| -> rusqlite::Result<()> {
        for kind in WORK_KINDS {
            let present: bool = conn
                .query_row(
                    "SELECT 1 FROM workflow_defs WHERE kind = ?1",
                    params![kind],
                    |_| Ok(true),
                )
                .unwrap_or(false);
            if present {
                continue;
            }
            conn.execute(
                "INSERT INTO workflow_defs (kind, definition, updated_at) VALUES (?1, ?2, ?3)",
                params![kind, WORKFLOW_DEFINITION, at],
            )?;
        }
        Ok(())
    })();
    match written {
        Ok(()) => conn.execute("COMMIT", []).map(|_| ()).map_err(Into::into),
        Err(err) => {
            let _ = conn.execute("ROLLBACK", []);
            Err(err.into())
        }
    }
}

fn pending_in(
    store: &str,
    path: &Path,
    directory: Option<&Path>,
) -> Result<usize, migrator::MigrateError> {
    let conn = crate::storage::sqlite::connection::connect(path)?;
    let applied = migrator::applied_version(&conn)?;
    let bundled = migrator::bundled_version(store, directory)?;
    Ok(usize::try_from(bundled.saturating_sub(applied)).unwrap_or(0))
}

const INIT_OPERATION: &str = "instance.init";
const INIT_REASON: &str = "instance bootstrap";
const EMPTY_DIGEST: &str =
    "sha256:0000000000000000000000000000000000000000000000000000000000000000";

/// The instance id a previous init wrote. Empty when the database carries none.
fn instance_id_of(data_dir: &Path) -> Result<String, migrator::MigrateError> {
    let conn = crate::storage::sqlite::connection::connect(&declared_path(data_dir))?;
    Ok(conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'instance_id'",
            [],
            |row| row.get(0),
        )
        .unwrap_or_default())
}

/// The instance id, the initiating actor and one audit row. A database that
/// already carries an instance id is left alone, so a second init is safe.
fn bootstrap(
    data_dir: &Path,
    clock: &mut Option<crate::core::StepClock>,
    ids: &mut Option<SequentialIds>,
) -> Result<String, migrator::MigrateError> {
    use rusqlite::params;

    let conn = crate::storage::sqlite::connection::connect(&declared_path(data_dir))?;
    let existing: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'instance_id'",
            [],
            |row| row.get(0),
        )
        .ok();
    if let Some(instance_id) = existing {
        return Ok(instance_id);
    }
    let user = crate::core::os_user();
    let next = |ids: &mut Option<SequentialIds>, prefix: &str| -> String {
        match ids {
            Some(factory) => factory.next(prefix),
            None => crate::core::fresh_id(prefix),
        }
    };
    let instance_id = next(ids, "ins");
    let actor_id = next(ids, "act");
    let audit_id = next(ids, "aud");
    let txn_id = next(ids, "txn");
    let at = stamp(clock);
    conn.execute("BEGIN IMMEDIATE", [])?;
    let written = (|| -> rusqlite::Result<()> {
        for (key, value) in [
            ("instance_id", instance_id.as_str()),
            ("revision", "0"),
            ("work_ref_seq", "0"),
            ("created_at", at.as_str()),
        ] {
            conn.execute(
                "INSERT INTO meta (key, value) VALUES (?1, ?2)",
                params![key, value],
            )?;
        }
        conn.execute(
            "INSERT INTO actors (id, kind, display_name, identity_ref, disabled, created_at)
             VALUES (?1, 'human', ?2, ?3, 0, ?4)",
            params![actor_id, &user, format!("local:{user}"), &at],
        )?;
        conn.execute(
            "INSERT INTO audit (id, txn_id, revision, actor_id, operation, entity_kind,
                                entity_id, reason, payload_digest, at)
             VALUES (?1, ?2, 0, ?3, ?4, 'instance', ?5, ?6, ?7, ?8)",
            params![
                audit_id,
                txn_id,
                actor_id,
                INIT_OPERATION,
                instance_id,
                INIT_REASON,
                EMPTY_DIGEST,
                at
            ],
        )?;
        Ok(())
    })();
    match written {
        Ok(()) => conn.execute("COMMIT", []).map(|_| ())?,
        Err(err) => {
            let _ = conn.execute("ROLLBACK", []);
            return Err(err.into());
        }
    }
    Ok(instance_id)
}

/// Stamp the observed store with the instance it belongs to. The two stores are
/// backed up and restored independently, so a restore that pairs mismatched
/// files is then a detectable error. Python inserts once and never retries, so
/// a store that already carries the key is left alone. The stamp is the third
/// clock read, one after the declared bootstrap.
fn bind_instance(
    data_dir: &Path,
    instance_id: &str,
    clock: &mut Option<crate::core::StepClock>,
) -> Result<(), migrator::MigrateError> {
    use rusqlite::params;

    let conn = crate::storage::sqlite::connection::connect(&observed_path(data_dir))?;
    let exists: bool = conn
        .query_row("SELECT 1 FROM meta WHERE key = 'instance_id'", [], |_| {
            Ok(true)
        })
        .unwrap_or(false);
    if exists {
        return Ok(());
    }
    let at = stamp(clock);
    conn.execute("BEGIN IMMEDIATE", [])?;
    let written = (|| -> rusqlite::Result<()> {
        conn.execute(
            "INSERT INTO meta (key, value) VALUES ('instance_id', ?1)",
            params![instance_id],
        )?;
        conn.execute(
            "INSERT INTO meta (key, value) VALUES ('created_at', ?1)",
            params![at],
        )?;
        Ok(())
    })();
    match written {
        Ok(()) => conn.execute("COMMIT", []).map(|_| ()).map_err(Into::into),
        Err(err) => {
            let _ = conn.execute("ROLLBACK", []);
            Err(err.into())
        }
    }
}

/// `mcp.stdio`, as the registry calls it. Ports `serve_mcp_stdio`.
///
/// Local-only: it reads this process's stdin until it closes and writes each
/// JSON-RPC response to stdout before reading the next line, then returns the
/// session report. A remote client uses `/mcp` instead, which is why the
/// operation is not an MCP tool.
pub fn mcp_stdio_op(
    ctx: &Built,
    _params: serde_json::Value,
) -> Result<serde_json::Value, VogtError> {
    // The registry the process already built, not a second one. The grant is
    // everything MCP exposes, which is what a local session may do; the
    // principal stays the caller's, because the context the tools run in is
    // this one rather than one rebuilt for the transport.
    let registry = crate::registry::default_registry();
    let grant = crate::adapters::mcp::framing::FullGrant;
    let report =
        crate::adapters::mcp::framing::serve_stdio_streams_with(&registry, &grant, Some(ctx))
            .map_err(|err| VogtError::InvalidRequest(format!("mcp stdio failed: {err}")))?;
    Ok(serde_json::json!({
        "protocol_version": report.protocol_version,
        "messages_handled": report.messages_handled,
        "supported_protocol_versions": crate::adapters::mcp::framing::SUPPORTED_PROTOCOL_VERSIONS,
    }))
}

/// `migrate`, as the registry calls it. Ports `migrate_instance`.
pub fn migrate_op(ctx: &Built, _params: serde_json::Value) -> Result<serde_json::Value, VogtError> {
    crate::with_ctx!(ctx, |ctx| migrate(ctx))
}

/// Bring both stores forward. It refuses an empty data directory rather than
/// quietly creating an instance, because `init` is the operation that does that.
fn migrate<C: Clock, I: IdFactory>(
    ctx: &crate::application::context::AppContext<C, I>,
) -> Result<serde_json::Value, VogtError> {
    if !ctx.declared.is_initialized() {
        return Err(VogtError::InvalidRequest(
            "no instance in this data directory to migrate — `vogt init` \
             creates one, and is idempotent against an existing instance"
                .to_string(),
        ));
    }
    let declared_report = ctx.declared.migrate()?;
    let observed_report = ctx.observed.migrate()?;
    let mut applied: Vec<String> = declared_report
        .applied
        .iter()
        .map(|name| format!("declared:{name}"))
        .collect();
    applied.extend(
        observed_report
            .applied
            .iter()
            .map(|name| format!("observed:{name}")),
    );
    Ok(serde_json::json!({
        "data_dir": ctx.config.resolved_data_dir().display().to_string(),
        "declared_schema_version": declared_report.version,
        "observed_schema_version": observed_report.version,
        "declared_schema_expected": ctx.declared.bundled_schema_version(),
        "observed_schema_expected": ctx.observed.bundled_schema_version(),
        "migrations_applied": applied,
    }))
}

/// `status`, as the registry calls it. Ports `status` in `services/instance.py`.
pub fn status_op(ctx: &Built, _params: serde_json::Value) -> Result<serde_json::Value, VogtError> {
    crate::with_ctx!(ctx, |ctx| status(ctx))
}

/// What this instance is and how much is in it.
fn status<C: Clock, I: IdFactory>(
    ctx: &crate::application::context::AppContext<C, I>,
) -> Result<serde_json::Value, VogtError> {
    let view = ctx.declared.read()?;
    let counts = view.counts()?;
    let stamp = view.clone_stamp()?;
    Ok(serde_json::json!({
        "vogt_version": crate::VERSION,
        "instance_id": view.instance_id()?,
        "data_dir": ctx.config.resolved_data_dir().display().to_string(),
        "principal": ctx.principal.identity_ref,
        "revision": view.current_revision()?,
        "declared_schema_version": ctx.declared.schema_version(),
        "observed_schema_version": ctx.observed.schema_version(),
        "counts": {
            "projects": counts.projects,
            "actors": counts.actors,
            "events": counts.events,
            "audit": counts.audit,
            "work_items": counts.work_items,
            "initiatives": counts.initiatives,
        },
        "clone": stamp.map(|stamp| serde_json::json!({
            "source_instance_id": stamp.source_instance_id,
            "cloned_at": stamp.cloned_at.to_json(),
            "backup_taken_at": stamp.backup_taken_at.to_json(),
        })),
        // The workspace volume, from the engine. Absent when no engine is
        // configured or it cannot answer (WI-1304): that is "not collected",
        // not a locally invented row.
        "disk": workspace_disk(ctx),
    }))
}

/// The workspace volume, from the engine. Absent when no engine is configured,
/// when it cannot answer, or when the payload is malformed — a different fact
/// from a volume that is fine (WI-1304). Python returns null with no engine;
/// the core does not invent a local stat.
pub(crate) fn workspace_disk<C: Clock, I: IdFactory>(
    ctx: &crate::application::context::AppContext<C, I>,
) -> Option<serde_json::Value> {
    let engine = ctx.engine.as_ref()?;
    match engine.operational_status() {
        // Read as Python's `EngineDiskStatus` reads it: a row with a field of
        // the wrong type, or one it does not know, is no row.
        Ok(payload) => crate::application::services::engine_status::disk(payload.get("disk")),
        Err(error) => {
            tracing::info!(
                error = %error,
                "workspace disk usage could not be read from the engine"
            );
            None
        }
    }
}

/// The config `init` adopts bootstrap tokens with. `init` was given this data
/// directory explicitly, and config still names the default, so the override is
/// what keeps a token from landing in the wrong database.
fn bootstrap_config(
    data_dir: &Path,
    overrides: &serde_json::Map<String, serde_json::Value>,
) -> Result<crate::config::VogtConfig, migrator::MigrateError> {
    let mut merged = overrides.clone();
    merged.insert(
        "data_dir".to_string(),
        serde_json::Value::String(data_dir.display().to_string()),
    );
    crate::config::load_config(&merged)
        .map_err(|error| migrator::MigrateError::Message(format!("bootstrap token: {error}")))
}

/// The next instant. A step clock walks one second per read, the way Python's
/// does; without one the wall clock is read once.
pub(crate) fn stamp(clock: &mut Option<crate::core::StepClock>) -> String {
    stamp_if(clock, true)
}

/// `stamp` when work is actually being written, the wall clock otherwise. A
/// step clock ticks on every read, so checking a database that needs nothing
/// must not spend one.
fn stamp_if(clock: &mut Option<crate::core::StepClock>, due: bool) -> String {
    if !due {
        return crate::core::to_iso(crate::core::utc_now());
    }
    match clock {
        Some(clock) => crate::core::to_iso(Clock::now(clock)),
        None => crate::core::to_iso(crate::core::utc_now()),
    }
}
