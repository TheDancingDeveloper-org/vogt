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
}

/// `clock` and `ids` are resolved once by the caller from the hook environment.
/// `None` is the wall clock and a fresh ULID, which is what a plain init does.
pub fn init(
    data_dir: &Path,
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
    adopt_bootstrap_core_token(data_dir, clock, ids)?;
    Ok(InitOutcome {
        created: !declared_existed,
        declared,
        observed,
        instance_id,
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
        // The workspace volume, when a session engine answers. Absent is a
        // different fact from a volume that is fine (WI-1304).
        "disk": workspace_disk(ctx),
    }))
}

/// The engine's report of the workspace volume, or nothing when it cannot be
/// asked. A failure to answer is logged and read as absent, never as an error
/// in status.
fn workspace_disk<C: Clock, I: IdFactory>(
    ctx: &crate::application::context::AppContext<C, I>,
) -> Option<serde_json::Value> {
    let engine = ctx.engine.as_ref()?;
    match engine.operational_status() {
        Ok(payload) => payload.get("disk").filter(|disk| disk.is_object()).cloned(),
        Err(error) => {
            tracing::info!(
                error = %error,
                "workspace disk usage could not be read from the engine"
            );
            None
        }
    }
}

/// The next instant. A step clock walks one second per read, the way Python's
/// does; without one the wall clock is read once.
fn stamp(clock: &mut Option<crate::core::StepClock>) -> String {
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

/// Adopt the operator-supplied core token, the way Python's
/// `adopt_bootstrap_core_token` does at init.
///
/// `"not_configured"` when the file is unset, unreadable or empty — the
/// instance still boots. `"already_present"` when an unrevoked row already
/// holds the hash, so the init that runs on every container start writes
/// nothing the second time. A secret that rotated *back* to a value a
/// rotation revoked reinstates that row instead of inserting a second one
/// (the hash is unique). A secret under 24 characters or an unknown scope is
/// a startup error: a deployment that believes it supplied a credential must
/// not come up without one.
fn adopt_bootstrap_core_token(
    data_dir: &Path,
    clock: &mut Option<crate::core::StepClock>,
    ids: &mut Option<SequentialIds>,
) -> Result<(), migrator::MigrateError> {
    use rusqlite::params;

    // `init` was given this data directory explicitly. Config still names the
    // default, and a token adopted into the wrong database is a token the
    // server never sees.
    let mut overrides = serde_json::Map::new();
    overrides.insert(
        "data_dir".to_string(),
        serde_json::Value::String(data_dir.display().to_string()),
    );
    let config = crate::config::load_config(&overrides).map_err(|error| {
        migrator::MigrateError::Message(format!("bootstrap_core_token: {error}"))
    })?;
    let Some(configured) = config.bootstrap_core_token_file else {
        return Ok(());
    };
    let secret = match std::fs::read_to_string(&configured) {
        Ok(text) => text.trim().to_string(),
        Err(_) => return Ok(()),
    };
    if secret.is_empty() {
        return Ok(());
    }
    let scopes =
        crate::auth::parse_scopes(&config.bootstrap_core_token_scopes).map_err(|error| {
            migrator::MigrateError::Message(format!("bootstrap_core_token: {error}"))
        })?;
    let token_hash = crate::auth::adopt(&secret).map_err(|error| {
        migrator::MigrateError::Message(format!("bootstrap_core_token: {error}"))
    })?;
    let identity_ref = config.bootstrap_core_token_actor;
    let conn = crate::storage::sqlite::connection::connect(&declared_path(data_dir))?;

    let next = |ids: &mut Option<SequentialIds>, prefix: &str| -> String {
        match ids {
            Some(factory) => factory.next(prefix),
            None => crate::core::fresh_id(prefix),
        }
    };

    // Everything that decides the row lives inside the transaction. The
    // revision and the actor are read there too: a read taken before
    // BEGIN IMMEDIATE can lose to the other container of the same stack.
    conn.execute("BEGIN IMMEDIATE", [])?;
    let written = (|| -> Result<bool, rusqlite::Error> {
        let live: Option<String> = conn
            .query_row(
                "SELECT id FROM tokens WHERE token_hash = ?1 AND revoked_at IS NULL",
                params![token_hash],
                |row| row.get(0),
            )
            .ok();
        if live.is_some() {
            return Ok(false);
        }

        // Python refuses `kind != "agent"`. A missing actor is created as an
        // agent below; there is no identity_ref prefix rule.
        let holder: Option<(String, String)> = conn
            .query_row(
                "SELECT id, kind FROM actors WHERE identity_ref = ?1",
                params![identity_ref],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .ok();
        if let Some((_, kind)) = &holder {
            if kind != "agent" {
                return Err(rusqlite::Error::InvalidParameterName(format!(
                    "bootstrap_core_token_actor {identity_ref:?} is a person ({kind}); \
                     the token the engine presents must be bound to an agent actor \
                     (for example agent:vogt-engine)"
                )));
            }
        }

        let at = stamp(clock);
        let (actor_id, actor_is_new) = match &holder {
            Some((id, _)) => (id.clone(), false),
            None => (next(ids, "act"), true),
        };
        if actor_is_new {
            conn.execute(
                "INSERT INTO actors (id, kind, display_name, identity_ref, disabled, created_at)
                 VALUES (?1, 'agent', ?2, ?3, 0, ?4)",
                params![actor_id, identity_ref, identity_ref, at],
            )?;
        }

        // A rotate-back finds the revoked row for this hash. Reinstate it
        // and revoke whatever currently holds the name, the way Python does.
        let revoked_row: Option<String> = conn
            .query_row(
                "SELECT id FROM tokens WHERE token_hash = ?1",
                params![token_hash],
                |row| row.get(0),
            )
            .ok();

        let mut superseded: Vec<String> = Vec::new();
        let mut stmt = conn.prepare(
            "SELECT id FROM tokens
             WHERE actor_id = ?1 AND name = 'bootstrap core token' AND revoked_at IS NULL",
        )?;
        let ids_to_revoke: Vec<String> = stmt
            .query_map(params![actor_id], |row| row.get(0))?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        for id in ids_to_revoke {
            conn.execute(
                "UPDATE tokens SET revoked_at = ?1,
                        revoked_reason = 'superseded by a rotated bootstrap secret'
                 WHERE id = ?2 AND revoked_at IS NULL",
                params![at, id],
            )?;
            superseded.push(id);
        }

        let entity_id = if let Some(ref existing_id) = revoked_row {
            conn.execute(
                "UPDATE tokens SET revoked_at = NULL, revoked_reason = NULL WHERE id = ?1",
                params![existing_id],
            )?;
            existing_id.clone()
        } else {
            let token_id = next(ids, "tok");
            let scopes_json =
                crate::decisions::python_json_dumps(&serde_json::json!(scopes), false);
            conn.execute(
                "INSERT INTO tokens (id, actor_id, name, token_hash, scopes, created_at)
                 VALUES (?1, ?2, 'bootstrap core token', ?3, ?4, ?5)",
                params![token_id, actor_id, token_hash, scopes_json, at],
            )?;
            token_id
        };

        let revision: i64 = conn
            .query_row(
                "SELECT CAST(value AS INTEGER) FROM meta WHERE key = 'revision'",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0)
            + 1;
        // Python's reinstated payload is {adopted, reason, superseded}; a
        // fresh adoption is {actor, scopes, name, source, superseded}. The
        // digest is over that object, so the keys have to match.
        let payload = if revoked_row.is_some() {
            serde_json::json!({
                "adopted": true,
                "reason": "reinstated",
                "superseded": superseded,
            })
        } else {
            serde_json::json!({
                "actor": identity_ref,
                "scopes": scopes,
                "name": "bootstrap core token",
                "source": "operator-supplied",
                "superseded": superseded,
            })
        };
        let digest = crate::decisions::digest_of(&payload);
        let summary = crate::decisions::python_json_dumps(
            &serde_json::json!({"actor": identity_ref, "scopes": scopes}),
            false,
        );
        let audit_id = next(ids, "aud");
        let txn_id = next(ids, "txn");
        conn.execute(
            "UPDATE meta SET value = ?1 WHERE key = 'revision'",
            params![revision.to_string()],
        )?;
        conn.execute(
            "INSERT INTO audit (id, txn_id, revision, actor_id, operation, entity_kind,
                                entity_id, reason, payload_digest, at)
             VALUES (?1, ?2, ?3, ?4, 'token.adopt', 'token', ?5,
                     'adopting the operator-supplied core token at init', ?6, ?7)",
            params![audit_id, txn_id, revision, actor_id, entity_id, digest, at],
        )?;
        conn.execute(
            "INSERT INTO events (kind, entity_kind, entity_id, actor_id, audit_id, summary, at)
             VALUES ('token.adopted', 'token', ?1, ?2, ?3, ?4, ?5)",
            params![entity_id, actor_id, audit_id, summary, at],
        )?;
        Ok(true)
    })();
    match written {
        Ok(_) => {
            conn.execute("COMMIT", []).map(|_| ())?;
            Ok(())
        }
        Err(err) => {
            let _ = conn.execute("ROLLBACK", []);
            Err(migrator::MigrateError::Message(format!("adopt: {err}")))
        }
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn init_adopts_the_stack_secret_the_engine_presents() {
        let dir = std::env::temp_dir().join(format!(
            "vogt-adopt-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let secret = "core-token-abcdef1234567890";
        let secret_path = dir.join("core-token");
        std::fs::write(&secret_path, secret).unwrap();
        let saved = std::env::var("VOGT_BOOTSTRAP_CORE_TOKEN_FILE").ok();
        unsafe { std::env::set_var("VOGT_BOOTSTRAP_CORE_TOKEN_FILE", &secret_path) };
        let mut clock = None;
        let mut ids = None;
        let outcome = super::init(&dir, &mut clock, &mut ids).expect("init adopts");
        assert!(outcome.created);
        let again = super::init(&dir, &mut clock, &mut ids).expect("second init is quiet");
        assert!(!again.created);
        let conn = crate::storage::sqlite::connection::connect(
            &crate::storage::sqlite::declared_path(&dir),
        )
        .unwrap();
        let hash = crate::auth::hash_token(secret);
        let (name, identity): (String, String) = conn
            .query_row(
                "SELECT t.name, a.identity_ref FROM tokens t
                 JOIN actors a ON a.id = t.actor_id
                 WHERE t.token_hash = ?1 AND t.revoked_at IS NULL",
                rusqlite::params![hash],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(name, "bootstrap core token");
        assert_eq!(identity, "agent:vogt-engine");
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM tokens", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1, "a second init must not mint another token");
        // Rotate A -> B -> A. The revoked A row is unique on the hash, so
        // coming back to A has to reinstate it. Inserting a second row is
        // what used to fail boot.
        let secret_b = "core-token-bbbbbbbbbbbbbbbb";
        std::fs::write(&secret_path, secret_b).unwrap();
        super::init(&dir, &mut clock, &mut ids).expect("rotation adopts B");
        std::fs::write(&secret_path, secret).unwrap();
        super::init(&dir, &mut clock, &mut ids).expect("rotate-back reinstates A");
        let live: String = conn
            .query_row(
                "SELECT token_hash FROM tokens WHERE revoked_at IS NULL",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(live, hash, "A is the live secret again");
        let total: i64 = conn
            .query_row("SELECT COUNT(*) FROM tokens", [], |row| row.get(0))
            .unwrap();
        assert_eq!(total, 2, "rotate-back reuses the revoked row");
        let digest: String = conn
            .query_row(
                "SELECT payload_digest FROM audit WHERE operation = 'token.adopt'
                 ORDER BY revision DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let superseded: String = conn
            .query_row(
                "SELECT id FROM tokens WHERE token_hash = ?1",
                rusqlite::params![crate::auth::hash_token(secret_b)],
                |row| row.get(0),
            )
            .unwrap();
        let expected = crate::decisions::digest_of(&serde_json::json!({
            "adopted": true,
            "reason": "reinstated",
            "superseded": [superseded],
        }));
        assert_eq!(digest, expected);
        match saved {
            Some(previous) => unsafe {
                std::env::set_var("VOGT_BOOTSTRAP_CORE_TOKEN_FILE", previous)
            },
            None => unsafe { std::env::remove_var("VOGT_BOOTSTRAP_CORE_TOKEN_FILE") },
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
