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

/// Process start, the Rust stand-in for `PROCESS_STARTED_AT` in
/// `observability.py`: the first time this process is asked, in UTC.
fn process_started_at() -> crate::core::Moment {
    use std::sync::OnceLock;
    static STARTED: OnceLock<crate::core::Moment> = OnceLock::new();
    *STARTED.get_or_init(crate::core::utc_now)
}

/// `instance.diagnostics`, as the registry calls it. Ports
/// `instance_diagnostics` in `services/instance.py`.
pub fn diagnostics_op(
    ctx: &Built,
    params: serde_json::Value,
) -> Result<serde_json::Value, VogtError> {
    crate::with_ctx!(ctx, |ctx| diagnostics(ctx, params))
}

/// Is this instance what it should be, and is it well — in one read.
///
/// Every check reports rather than raises: a diagnostics read that fails when
/// something is wrong would fail exactly when it is needed.
fn diagnostics<C: Clock, I: IdFactory>(
    ctx: &crate::application::context::AppContext<C, I>,
    params: serde_json::Value,
) -> Result<serde_json::Value, VogtError> {
    use crate::application::models::system::{DiagnosticsParams, DiagnosticsResult};
    use crate::application::models::views::{
        DiagnosticCheck, DiagnosticCheckStatus, DiagnosticsResultStatus, RecentLog, StoreMigration,
    };
    use crate::observability::{
        capturing_problems, recent_problems, redact, RECENT_PROBLEMS_CAPACITY,
    };

    let params: DiagnosticsParams = serde_json::from_value(params).map_err(|err| {
        VogtError::InvalidParams(format!("instance.diagnostics parameters: {err}"))
    })?;
    let mut checks: Vec<DiagnosticCheck> = Vec::new();
    let mut migrations = std::collections::BTreeMap::new();
    let mut instance_id = None;

    for (name, applied, expected) in [
        (
            "declared",
            ctx.declared.schema_version(),
            ctx.declared.bundled_schema_version(),
        ),
        (
            "observed",
            ctx.observed.schema_version(),
            ctx.observed.bundled_schema_version(),
        ),
    ] {
        migrations.insert(
            name.to_string(),
            StoreMigration {
                applied,
                expected,
                pending: (expected - applied).max(0),
            },
        );
        checks.push(DiagnosticCheck {
            name: format!("{name}_store"),
            status: if applied == expected {
                DiagnosticCheckStatus::Ok
            } else {
                DiagnosticCheckStatus::Failing
            },
            detail: if applied == expected {
                None
            } else {
                Some(format!(
                    "schema {applied}, this build expects {expected} — run migrate"
                ))
            },
        });
    }

    match ctx.declared.read().and_then(|view| view.instance_id()) {
        Ok(id) => instance_id = Some(id),
        Err(error) => checks.push(DiagnosticCheck {
            name: "instance".to_string(),
            status: DiagnosticCheckStatus::Failing,
            detail: Some(redact(error.message())),
        }),
    }

    match &ctx.engine {
        None => checks.push(DiagnosticCheck {
            name: "engine".to_string(),
            status: DiagnosticCheckStatus::NotConfigured,
            detail: Some("no engine_url; session operations are unavailable".to_string()),
        }),
        Some(engine) => match engine.healthz() {
            Ok(()) => checks.push(DiagnosticCheck {
                name: "engine".to_string(),
                status: DiagnosticCheckStatus::Ok,
                detail: None,
            }),
            Err(error) => checks.push(DiagnosticCheck {
                name: "engine".to_string(),
                status: DiagnosticCheckStatus::Degraded,
                detail: Some(error.message().to_string()),
            }),
        },
    }

    match crate::application::services::freshness::freshness_of(&ctx.observed, &ctx.clock) {
        Ok(freshness) => {
            let status_name = freshness.get("status").and_then(|value| value.as_str());
            let age = freshness
                .get("age_seconds")
                .and_then(|value| value.as_i64());
            let detail_text = freshness
                .get("detail")
                .and_then(|value| value.as_str())
                .filter(|text| !text.is_empty());
            let detail = match age {
                None => detail_text.map(str::to_string),
                Some(seconds) => {
                    let mut text = format!("oldest sweep {seconds}s ago");
                    if let Some(extra) = detail_text {
                        text.push_str("; ");
                        text.push_str(extra);
                    }
                    Some(text)
                }
            };
            checks.push(DiagnosticCheck {
                name: "collection".to_string(),
                status: if status_name == Some("fresh") {
                    DiagnosticCheckStatus::Ok
                } else {
                    DiagnosticCheckStatus::Degraded
                },
                detail,
            });
        }
        Err(error) => checks.push(DiagnosticCheck {
            name: "collection".to_string(),
            status: DiagnosticCheckStatus::Degraded,
            detail: Some(redact(error.message())),
        }),
    }

    let failing = checks
        .iter()
        .any(|check| matches!(check.status, DiagnosticCheckStatus::Failing));
    let degraded = checks
        .iter()
        .any(|check| matches!(check.status, DiagnosticCheckStatus::Degraded));
    let overall = if failing {
        DiagnosticsResultStatus::Failing
    } else if degraded {
        DiagnosticsResultStatus::Degraded
    } else {
        DiagnosticsResultStatus::Ok
    };

    let started = process_started_at();
    let now = crate::application::services::now_of(&ctx.clock);
    let uptime = (now.seconds_since(started) as i64).max(0);
    let result = DiagnosticsResult {
        status: overall,
        vogt_version: crate::VERSION.to_string(),
        image_digest: ctx.config.image_digest.clone(),
        instance_id,
        started_at: started.to_json(),
        uptime_seconds: uptime,
        checks,
        migrations,
        recent_log: RecentLog {
            capturing: capturing_problems(),
            lines: recent_problems(params.log_lines.max(0) as usize),
            capacity: RECENT_PROBLEMS_CAPACITY as i64,
        },
        peer: peer_diagnostics(ctx, params.peer, params.log_lines),
    };
    serde_json::to_value(result).map_err(|err| {
        VogtError::InvalidRequest(format!("instance.diagnostics could not be encoded: {err}"))
    })
}

fn peer_diagnostics<C: Clock, I: IdFactory>(
    ctx: &crate::application::context::AppContext<C, I>,
    asked: bool,
    log_lines: i64,
) -> crate::application::models::views::PeerDiagnostics {
    use crate::application::models::views::{PeerDiagnostics, PeerDiagnosticsStatus};
    if !asked {
        return PeerDiagnostics {
            status: PeerDiagnosticsStatus::NotRequested,
            url: None,
            detail: None,
            diagnostics: None,
        };
    }
    let Some(peer) = &ctx.peer else {
        return PeerDiagnostics {
            status: PeerDiagnosticsStatus::NotConfigured,
            url: None,
            detail: Some("set diagnostics_peer_url (and diagnostics_peer_token_file)".to_string()),
            diagnostics: None,
        };
    };
    match peer.diagnostics(log_lines) {
        Ok(answer) => PeerDiagnostics {
            status: PeerDiagnosticsStatus::Ok,
            url: Some(peer.base_url().to_string()),
            detail: None,
            diagnostics: Some(answer),
        },
        Err(VogtError::PeerUnavailable { status, message }) => PeerDiagnostics {
            status: match status.as_str() {
                "refused" => PeerDiagnosticsStatus::Refused,
                "invalid_response" => PeerDiagnosticsStatus::InvalidResponse,
                _ => PeerDiagnosticsStatus::Unreachable,
            },
            url: Some(peer.base_url().to_string()),
            detail: Some(message),
            diagnostics: None,
        },
        Err(error) => PeerDiagnostics {
            status: PeerDiagnosticsStatus::Unreachable,
            url: Some(peer.base_url().to_string()),
            detail: Some(crate::observability::redact(error.message())),
            diagnostics: None,
        },
    }
}

#[cfg(test)]
mod diagnostics_tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use serde_json::json;

    use crate::adapters::engine::EngineClient;
    use crate::adapters::peer::PeerClient;
    use crate::application::context::{build_context, Built};
    use crate::application::instance::{diagnostics_op, migrate_op, status_op};
    use crate::core::{ActorKind, Moment, Principal, SequentialIds, StepClock};
    use crate::errors::VogtError;
    use crate::observability::{configure_logging, recent_problems};
    use crate::storage::interface::{DeclaredStore, ObservedStore};

    fn principal() -> Principal {
        Principal::new("local:test-user", ActorKind::Human, "Test").unwrap()
    }

    fn opened(
        engine: Option<EngineClient>,
        peer: Option<PeerClient>,
        digest: Option<&str>,
    ) -> Built {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("vogt-diag-{}-{}", std::process::id(), n));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let config = crate::config::VogtConfig {
            data_dir: dir,
            image_digest: digest.map(str::to_string),
            ..crate::config::VogtConfig::default()
        };
        let built = build_context(
            config,
            Some(principal()),
            Some(StepClock::new(Moment::from_unix(1_700_000_000, 0))),
            Some(SequentialIds::new(None).unwrap()),
            None,
            engine,
            peer,
            None,
        )
        .unwrap();
        crate::with_ctx!(&built, |ctx| {
            ctx.declared.migrate().unwrap();
            ctx.declared.bootstrap(&principal()).unwrap();
            ctx.observed.migrate().unwrap();
        });
        built
    }

    fn checks(answer: &serde_json::Value) -> BTreeMap<String, serde_json::Value> {
        answer["checks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|check| (check["name"].as_str().unwrap().to_string(), check.clone()))
            .collect()
    }

    #[test]
    fn a_healthy_instance_reports_identity_and_a_degraded_collection() {
        let built = opened(None, None, None);
        let answer = diagnostics_op(&built, json!({})).unwrap();
        assert_eq!(answer["vogt_version"], crate::VERSION);
        assert!(answer["image_digest"].is_null());
        assert!(answer["instance_id"].as_str().unwrap().starts_with("ins_"));
        assert!(answer["uptime_seconds"].as_i64().unwrap() >= 0);
        assert_eq!(answer["migrations"]["declared"]["pending"], 0);
        assert_eq!(answer["migrations"]["observed"]["pending"], 0);
        let named = checks(&answer);
        assert_eq!(named["declared_store"]["status"], "ok");
        assert_eq!(named["observed_store"]["status"], "ok");
        assert_eq!(named["engine"]["status"], "not_configured");
        assert!(named["engine"]["detail"]
            .as_str()
            .unwrap()
            .contains("no engine_url"));
        assert_eq!(named["collection"]["status"], "degraded");
        assert_eq!(answer["status"], "degraded");
        assert_eq!(answer["peer"]["status"], "not_requested");
        assert!(answer["peer"]["diagnostics"].is_null());
    }

    #[test]
    fn the_image_digest_is_the_one_the_deployment_stated() {
        let built = opened(None, None, Some("sha256:abc"));
        let answer = diagnostics_op(&built, json!({})).unwrap();
        assert_eq!(answer["image_digest"], "sha256:abc");
    }

    #[test]
    fn an_unreachable_engine_degrades_the_check_instead_of_failing_the_read() {
        let transport: crate::adapters::engine::Transport =
            Arc::new(|_url, _headers, _body, _method| (503, b"no".to_vec()));
        let engine = EngineClient::new("http://e", None, Some(transport));
        let built = opened(Some(engine), None, None);
        let answer = diagnostics_op(&built, json!({})).unwrap();
        let named = checks(&answer);
        assert_eq!(named["engine"]["status"], "degraded");
        assert!(named["engine"]["detail"].as_str().unwrap().contains("503"));
    }

    #[test]
    fn a_healthy_engine_is_ok() {
        let transport: crate::adapters::engine::Transport =
            Arc::new(|url, _headers, _body, _method| {
                assert!(url.ends_with("/healthz"), "{url}");
                (200, br#"{"ok": true}"#.to_vec())
            });
        let engine = EngineClient::new("http://e", None, Some(transport));
        let built = opened(Some(engine), None, None);
        let answer = diagnostics_op(&built, json!({})).unwrap();
        assert_eq!(checks(&answer)["engine"]["status"], "ok");
    }

    #[test]
    fn recent_problems_are_redacted_and_bounded() {
        configure_logging("info", "text");
        tracing::warn!(
            target: "vogt.diagnostics_test",
            "push failed for https://bot:s3cret@github.com/x token=vogt_abcdefgh123"
        );
        tracing::error!(
            target: "vogt.diagnostics_test",
            "Authorization: Bearer abc.def.ghi rejected"
        );
        tracing::info!(target: "vogt.diagnostics_test", "an info line is not a problem");
        let built = opened(None, None, None);
        let answer = diagnostics_op(&built, json!({"log_lines": 2})).unwrap();
        assert_eq!(answer["recent_log"]["capturing"], true);
        let lines = answer["recent_log"]["lines"].as_array().unwrap();
        assert_eq!(lines.len(), 2);
        let joined = lines
            .iter()
            .map(|line| line.as_str().unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!joined.contains("s3cret"));
        assert!(!joined.contains("vogt_abcdefgh123"));
        assert!(!joined.contains("abc.def.ghi"));
        assert!(joined.contains("push failed"));
        assert!(!joined.contains("an info line"));
        let none = diagnostics_op(&built, json!({"log_lines": 0})).unwrap();
        assert_eq!(none["recent_log"]["lines"].as_array().unwrap().len(), 0);
        assert!(recent_problems(0).is_empty());
    }

    #[test]
    fn a_peer_that_was_not_configured_is_said() {
        let built = opened(None, None, None);
        let answer = diagnostics_op(&built, json!({"peer": true})).unwrap();
        assert_eq!(answer["peer"]["status"], "not_configured");
        assert!(answer["peer"]["detail"]
            .as_str()
            .unwrap()
            .contains("diagnostics_peer_url"));
    }

    #[test]
    fn a_peer_answer_is_fetched_with_the_token_and_without_recursion() {
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded = Arc::clone(&seen);
        let transport: crate::adapters::peer::Transport =
            Box::new(move |url, headers, _body, method| {
                assert_eq!(method, "GET");
                recorded
                    .lock()
                    .unwrap()
                    .push((url.to_string(), headers.get("Authorization").cloned()));
                (200, br#"{"vogt_version":"9.9.9","status":"ok"}"#.to_vec())
            });
        // from_config reads the token from a file.
        let dir = std::env::temp_dir().join(format!("vogt-peer-token-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let token_path = dir.join("peer.token");
        std::fs::write(&token_path, "t0k\n").unwrap();
        let peer = PeerClient::from_config(
            Some("https://prod/api/vogt"),
            Some(&token_path),
            Some(transport),
        )
        .unwrap();
        let built = opened(None, Some(peer), None);
        let answer = diagnostics_op(&built, json!({"peer": true, "log_lines": 5})).unwrap();
        assert_eq!(answer["peer"]["status"], "ok");
        assert_eq!(answer["peer"]["diagnostics"]["vogt_version"], "9.9.9");
        let seen = seen.lock().unwrap();
        assert_eq!(
            seen[0].0,
            "https://prod/api/vogt/instance/diagnostics?peer=false&log_lines=5"
        );
        assert_eq!(seen[0].1.as_deref(), Some("Bearer t0k"));
    }

    #[test]
    fn peer_failures_are_reported_not_raised() {
        for (status, body, expected) in [
            (401u16, b"".as_slice(), "refused"),
            (502, b"", "unreachable"),
            (200, b"<html>", "invalid_response"),
            (200, b"[1]", "invalid_response"),
        ] {
            let body = body.to_vec();
            let transport: crate::adapters::peer::Transport =
                Box::new(move |_url, _headers, _body, _method| (status, body.clone()));
            let peer = PeerClient::from_config(Some("https://p"), None, Some(transport)).unwrap();
            let built = opened(None, Some(peer), None);
            let answer = diagnostics_op(&built, json!({"peer": true})).unwrap();
            assert_eq!(answer["peer"]["status"], expected, "{status}");
            assert!(answer["peer"]["diagnostics"].is_null());
        }
    }

    /// `instance.status` names the instance and counts an empty register as zero.
    #[test]
    fn status_names_the_instance_and_counts_nothing() {
        let built = opened(None, None, None);
        let answer = status_op(&built, json!({})).unwrap();
        assert_eq!(answer["vogt_version"], crate::VERSION);
        assert!(answer["instance_id"].as_str().unwrap().starts_with("ins_"));
        assert_eq!(answer["counts"]["projects"], 0);
        assert_eq!(answer["counts"]["work_items"], 0);
        assert!(answer["principal"].as_str().unwrap().starts_with("local:"));
    }

    /// Migrating an instance that is already at the head is a no-op that
    /// reports nothing pending.
    #[test]
    fn migrating_an_up_to_date_instance_reports_nothing_pending() {
        let built = opened(None, None, None);
        let answer = migrate_op(&built, json!({})).unwrap();
        assert_eq!(answer["migrations_applied"].as_array().unwrap().len(), 0);
        assert_eq!(
            answer["declared_schema_version"],
            answer["declared_schema_expected"]
        );
        assert_eq!(
            answer["observed_schema_version"],
            answer["observed_schema_expected"]
        );
        let status = status_op(&built, json!({})).unwrap();
        assert!(status["instance_id"].as_str().unwrap().starts_with("ins_"));
    }

    /// `migrate` refuses an empty data directory rather than quietly creating
    /// an instance. `init` is the operation that does that.
    #[test]
    fn migrating_an_uninitialized_data_dir_is_refused() {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("vogt-migrate-empty-{}-{}", std::process::id(), n));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let config = crate::config::VogtConfig {
            data_dir: dir,
            ..crate::config::VogtConfig::default()
        };
        let built = build_context(
            config,
            Some(principal()),
            Some(StepClock::new(Moment::from_unix(1_700_000_000, 0))),
            Some(SequentialIds::new(None).unwrap()),
            None,
            None,
            None,
            None,
        )
        .unwrap();
        let err = migrate_op(&built, json!({})).unwrap_err();
        let VogtError::InvalidRequest(message) = err else {
            panic!("expected InvalidRequest, got {err:?}");
        };
        assert!(
            message.contains("no instance in this data directory"),
            "{message}"
        );
    }
}
