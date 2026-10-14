//! Backup and restore. Ports the matching half of `lifecycle.py`.
//!
//! A backup snapshots both stores with SQLite's own backup API and writes a
//! manifest naming each schema version. Restore verifies that manifest before
//! touching anything: the failure this prevents is discovering a problem half
//! way through, with the live data already gone.
//!
//! `clone` is `restore` for a backup of another instance: the data comes
//! across, the target's identity and credentials stay, and the copy is made
//! unable to act as its source. `import` is not here.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::application::context::{with_stores_at, AppContext, Built};
use crate::application::services::{context_parts, write_context};
use crate::application::writes::{audited_write, WriteOutcome};
use crate::core::{Clock, IdFactory, Moment};
use crate::errors::VogtError;
use crate::storage::interface::{
    CloneStamp, DeclaredStore, ObservedStore, ProjectUpdate, ReadView, WorkFilter, WriteTxn,
};
use crate::VERSION;

const MANIFEST_NAME: &str = "manifest.json";
const MANIFEST_VERSION: i64 = 2;
const ENGINE_STATE_DIR: &str = "engine-state";
const BACKUP_EVENT: &str = "instance.backed_up";
const CLONE_EVENT: &str = "instance.cloned";
const CLONE_OPERATION: &str = "clone";
/// What `clone --include-engine-state` copies: session history only. `push.json`
/// above all stays behind — the source's phones would receive this copy's
/// notifications otherwise.
const CLONED_ENGINE_STATE: &[&str] = &["history.db", "assistant-log.db", "session-logs"];
const EXPORT_FORMAT_VERSION: i64 = 2;

/// What a backup says about itself. `engine_state` and `import_root` arrived
/// with manifest version 2; a version-1 manifest covered the two stores and
/// said nothing about the rest.
struct Manifest {
    manifest_version: i64,
    instance_id: String,
    vogt_version: String,
    declared_schema_version: i64,
    observed_schema_version: i64,
    taken_at: Moment,
    engine_state: String,
    import_root: Option<String>,
}

impl Manifest {
    fn to_json(&self) -> Value {
        json!({
            "manifest_version": self.manifest_version,
            "instance_id": self.instance_id,
            "vogt_version": self.vogt_version,
            "declared_schema_version": self.declared_schema_version,
            "observed_schema_version": self.observed_schema_version,
            "taken_at": crate::core::to_iso(self.taken_at),
            "engine_state": self.engine_state,
            "import_root": self.import_root,
        })
    }

    /// Read a manifest, accepting the version-1 shape. An absent `engine_state`
    /// reads as "unknown" rather than "nothing": an old backup did not fail to
    /// copy the engine state, it was taken before there was any.
    fn from_json(raw: &Value) -> Result<Self, VogtError> {
        let field = |name: &str| {
            raw.get(name)
                .and_then(Value::as_i64)
                .ok_or_else(|| VogtError::InvalidRequest(format!("manifest has no {name}")))
        };
        let text = |name: &str| raw.get(name).and_then(Value::as_str).unwrap_or("");
        Ok(Self {
            manifest_version: field("manifest_version").unwrap_or(0),
            instance_id: text("instance_id").to_string(),
            vogt_version: text("vogt_version").to_string(),
            declared_schema_version: field("declared_schema_version").unwrap_or(0),
            observed_schema_version: field("observed_schema_version").unwrap_or(0),
            taken_at: crate::core::from_iso(text("taken_at")).map_err(|_| {
                VogtError::InvalidRequest(format!(
                    "backup manifest taken_at {value:?} is not a timestamp",
                    value = text("taken_at")
                ))
            })?,
            engine_state: raw
                .get("engine_state")
                .and_then(Value::as_str)
                .unwrap_or("unknown (manifest v1)")
                .to_string(),
            import_root: match raw.get("import_root") {
                Some(Value::String(value)) => Some(value.clone()),
                _ => None,
            },
        })
    }
}

/// Snapshot both stores plus a manifest. A failure copying the engine state is
/// recorded in the manifest rather than fatal: refusing would trade a partial
/// backup for none.
pub fn backup<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    params: &Value,
) -> Result<Value, VogtError> {
    let instance_id = ctx.declared.read()?.instance_id()?;
    let taken_at = now(ctx);
    let label = params
        .get("label")
        .and_then(Value::as_str)
        .filter(|label| !label.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            // strftime("%Y%m%dT%H%M%SZ"): no separators, no fraction, a Z.
            format!(
                "{}Z",
                crate::core::to_iso(taken_at)[..19].replace(['-', ':'], "")
            )
        });
    let destination = params
        .get("destination")
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
        .map(|path| expand_user(Path::new(path)))
        .unwrap_or_else(|| ctx.config.backups_dir().join(&label));

    if destination.exists()
        && destination
            .read_dir()
            .is_ok_and(|mut it| it.next().is_some())
    {
        return Err(VogtError::Conflict(format!(
            "{destination} already exists and is not empty",
            destination = destination.display()
        )));
    }
    std::fs::create_dir_all(&destination).map_err(|err| {
        VogtError::InvalidRequest(format!(
            "creating {destination} failed: {err}",
            destination = destination.display()
        ))
    })?;

    snapshot(
        &ctx.config.declared_db_path(),
        &destination.join("declared.sqlite3"),
    )?;
    snapshot(
        &ctx.config.observed_db_path(),
        &destination.join("observed.sqlite3"),
    )?;
    let engine_state = copy_engine_state(
        ctx.config.engine_state_dir.as_deref(),
        &destination.join(ENGINE_STATE_DIR),
    );

    let manifest = Manifest {
        manifest_version: MANIFEST_VERSION,
        instance_id: instance_id.clone(),
        vogt_version: VERSION.to_string(),
        declared_schema_version: ctx.declared.schema_version(),
        observed_schema_version: ctx.observed.schema_version(),
        taken_at,
        engine_state: engine_state.clone(),
        import_root: Some(ctx.config.resolved_import_root().display().to_string()),
    };
    let text = crate::decisions::python_json_dumps_indent(&manifest.to_json(), 2) + "\n";
    std::fs::write(destination.join(MANIFEST_NAME), text)
        .map_err(|err| VogtError::InvalidRequest(format!("writing the manifest failed: {err}")))?;

    let reason = params.get("reason").and_then(Value::as_str).unwrap_or("");
    ctx.declared.publish_event(
        BACKUP_EVENT,
        "instance",
        &instance_id,
        &json!({"path": destination.display().to_string(), "reason": reason}),
        // Python reads the clock again here, after the copy. A clock that
        // advances between the two reads records a later moment than taken_at.
        now(ctx),
    )?;

    Ok(json!({
        "path": destination.display().to_string(),
        "instance_id": instance_id,
        "declared_schema_version": manifest.declared_schema_version,
        "observed_schema_version": manifest.observed_schema_version,
        "taken_at": crate::core::to_json_timestamp(manifest.taken_at),
        "engine_state": engine_state,
        "import_root": manifest.import_root,
    }))
}

/// Read a backup's manifest and refuse anything this build cannot take: an
/// unreadable or future manifest, a missing store, a schema ahead of this build.
fn verified_manifest<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    source: &Path,
) -> Result<Manifest, VogtError> {
    let manifest_path = source.join(MANIFEST_NAME);
    if !manifest_path.is_file() {
        return Err(VogtError::NotFound(format!(
            "{source} has no {MANIFEST_NAME}; it is not a Vogt backup",
            source = source.display()
        )));
    }
    let raw = std::fs::read_to_string(&manifest_path).map_err(|err| {
        VogtError::InvalidRequest(format!(
            "reading {manifest} failed: {err}",
            manifest = manifest_path.display()
        ))
    })?;
    let value = serde_json::from_str(&raw).map_err(|err| {
        VogtError::InvalidRequest(format!(
            "{manifest} is not readable: {err}",
            manifest = manifest_path.display()
        ))
    })?;
    let manifest = Manifest::from_json(&value)?;

    if manifest.manifest_version > MANIFEST_VERSION {
        return Err(VogtError::InvalidRequest(format!(
            "backup manifest version {version} is newer than {MANIFEST_VERSION}; run a newer \
             build rather than guessing what it contains",
            version = manifest.manifest_version
        )));
    }
    if manifest.manifest_version < 1 {
        return Err(VogtError::InvalidRequest(format!(
            "backup manifest version {version} is not readable",
            version = manifest.manifest_version
        )));
    }
    for name in ["declared.sqlite3", "observed.sqlite3"] {
        verify_store(source, name)?;
    }
    let current = ctx.declared.schema_version();
    if manifest.declared_schema_version > current {
        return Err(VogtError::InvalidRequest(format!(
            "the backup was taken at declared schema {backed} and this build is at {current}. \
             Migrations are forward-only: run a newer build rather than restoring backwards.",
            backed = manifest.declared_schema_version
        )));
    }
    Ok(manifest)
}

/// Every Vogt store, declared or observed, carries these two tables from its
/// first migration: `meta` (0001_foundation) and `migrations` (the migrator's
/// own bookkeeping). A database without them is somebody else's.
const STORE_TABLES: [&str; 2] = ["meta", "migrations"];

/// Refuse a store file that must never be copied over the live one.
///
/// WI-1066. Restore and clone used to copy whatever sat at the store's name: a
/// symlink (which a copy follows, so the restore reads a file outside the
/// backup), a torn or corrupt database, or any SQLite file at all. Each is
/// refused here, before anything is touched. The messages match the Python
/// port exactly.
fn verify_store(source: &Path, name: &str) -> Result<(), VogtError> {
    let path = source.join(name);
    if path
        .symlink_metadata()
        .is_ok_and(|metadata| metadata.file_type().is_symlink())
    {
        return Err(VogtError::InvalidRequest(format!(
            "{name} is a symlink; refusing to restore it"
        )));
    }
    if !path.is_file() {
        return Err(VogtError::NotFound(format!(
            "{source} is missing {name}",
            source = source.display()
        )));
    }
    let corrupt = format!("{name} failed its integrity check; refusing to restore it");
    // Read-only and immutable: checking a backup must never write to it, not
    // even the -wal/-shm files a WAL-mode database would otherwise get.
    let absolute = path.canonicalize().unwrap_or_else(|_| path.clone());
    let uri = format!(
        "file:{}?mode=ro&immutable=1",
        absolute.to_string_lossy().replace('?', "%3f")
    );
    let conn = rusqlite::Connection::open_with_flags(
        &uri,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
    .map_err(|_| VogtError::InvalidRequest(corrupt.clone()))?;
    let rows = conn
        .query_row("PRAGMA quick_check", [], |row| row.get::<_, String>(0))
        .map(|value| vec![value])
        .map_err(|_| VogtError::InvalidRequest(corrupt.clone()))?;
    if rows != ["ok"] {
        return Err(VogtError::InvalidRequest(corrupt));
    }
    let mut tables = std::collections::HashSet::new();
    let mut statement = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")
        .map_err(|_| VogtError::InvalidRequest(corrupt.clone()))?;
    let names = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|_| VogtError::InvalidRequest(corrupt.clone()))?;
    for table in names {
        tables.insert(table.map_err(|_| VogtError::InvalidRequest(corrupt.clone()))?);
    }
    if !STORE_TABLES.iter().all(|table| tables.contains(*table)) {
        return Err(VogtError::InvalidRequest(format!(
            "{name} is not a Vogt store; refusing to restore it"
        )));
    }
    Ok(())
}

/// Verify the manifest, then replace the data directory's stores. Verification
/// happens before anything is touched.
pub fn restore<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    params: &Value,
) -> Result<Value, VogtError> {
    let source = expand_user(Path::new(
        params.get("source").and_then(Value::as_str).unwrap_or(""),
    ));
    let manifest = verified_manifest(ctx, &source)?;
    if params.get("confirm").and_then(Value::as_bool) != Some(true) {
        return Err(VogtError::InvalidRequest(format!(
            "this replaces the stores in {data_dir} with the backup taken at {taken}. \
             Pass --confirm.",
            data_dir = ctx.config.resolved_data_dir().display(),
            taken = crate::core::to_iso(manifest.taken_at)
        )));
    }

    let data_dir = ctx.config.resolved_data_dir();
    std::fs::create_dir_all(&data_dir).map_err(|err| {
        VogtError::InvalidRequest(format!(
            "creating {data_dir} failed: {err}",
            data_dir = data_dir.display()
        ))
    })?;
    for name in ["declared.sqlite3", "observed.sqlite3"] {
        std::fs::copy(source.join(name), data_dir.join(name))
            .map_err(|err| VogtError::InvalidRequest(format!("copying {name} failed: {err}")))?;
        // WAL and shm files belong to the replaced database, not the new one.
        for suffix in ["-wal", "-shm"] {
            let stale = data_dir.join(format!("{name}{suffix}"));
            if stale.exists() {
                std::fs::remove_file(&stale).map_err(|err| {
                    VogtError::InvalidRequest(format!(
                        "removing {stale} failed: {err}",
                        stale = stale.display()
                    ))
                })?;
            }
        }
    }

    let engine_state = restore_engine_state(
        &source.join(ENGINE_STATE_DIR),
        ctx.config.engine_state_dir.as_deref(),
    );
    let migrated = ctx.declared.migrate()?;
    ctx.observed.migrate()?;

    Ok(json!({
        "source": source.display().to_string(),
        "instance_id": manifest.instance_id,
        "restored_from": crate::core::to_json_timestamp(manifest.taken_at),
        "migrations_applied": migrated.applied,
        "declared_schema_version": ctx.declared.schema_version(),
        "engine_state": engine_state,
        "import_root_then": manifest.import_root,
        "import_root_now": ctx.config.resolved_import_root().display().to_string(),
    }))
}

/// Restore another instance's backup here as a copy, not as that instance.
///
/// A plain restore makes this instance *be* that one. A clone keeps this
/// instance's id and credentials, revokes the source's live tokens, drops its
/// password logins and forge accounts, disarms every project's write-back,
/// marks the source's running sessions stopped, and stamps the copy. The
/// source's push subscriptions and agent tasks are never copied.
///
/// Verification and the sanitising write happen on a staged copy. A failure
/// before the staged stores replace the live ones leaves the live stores
/// untouched.
pub fn clone_instance<C: Clock + 'static, I: IdFactory + 'static>(
    ctx: &AppContext<C, I>,
    params: &Value,
) -> Result<Value, VogtError> {
    let source = expand_user(Path::new(
        params.get("source").and_then(Value::as_str).unwrap_or(""),
    ));
    let manifest = verified_manifest(ctx, &source)?;
    let current_observed = ctx.observed.schema_version();
    if manifest.observed_schema_version > current_observed {
        return Err(VogtError::InvalidRequest(format!(
            "the backup was taken at observed schema {backed} and this build is at \
             {current_observed}. Run a newer build rather than cloning backwards.",
            backed = manifest.observed_schema_version
        )));
    }
    let reason = params.get("reason").and_then(Value::as_str).unwrap_or("");

    let instance_id = ctx.declared.read()?.instance_id()?;
    if manifest.instance_id == instance_id {
        return Err(VogtError::InvalidRequest(format!(
            "{source} is a backup of this instance ({instance_id}); a clone copies another \
             instance. Use `restore` to put this one back.",
            source = source.display()
        )));
    }
    if params.get("confirm").and_then(Value::as_bool) != Some(true) {
        return Err(VogtError::InvalidRequest(format!(
            "this replaces the stores in {data_dir} with a copy of instance {source_id} as it \
             was at {taken}, keeping this instance's id and credentials. Pass --confirm.",
            data_dir = ctx.config.resolved_data_dir().display(),
            source_id = manifest.instance_id,
            taken = crate::core::to_iso(manifest.taken_at)
        )));
    }

    let carried = ctx.declared.credentials()?;
    let cloned_at = now(ctx);
    let stamp = CloneStamp {
        source_instance_id: manifest.instance_id.clone(),
        cloned_at,
        backup_taken_at: manifest.taken_at,
    };
    let revoke_reason = format!(
        "cloned from instance {source_id}: the source's credentials are not valid on a copy",
        source_id = manifest.instance_id
    );

    let data_dir = ctx.config.resolved_data_dir();
    std::fs::create_dir_all(&data_dir).map_err(|err| {
        VogtError::InvalidRequest(format!(
            "creating {data_dir} failed: {err}",
            data_dir = data_dir.display()
        ))
    })?;
    // Beside the live stores, so the final swap is a same-filesystem rename.
    let stamp_label = format!(
        "{}Z",
        crate::core::to_iso(cloned_at)[..19].replace(['-', ':'], "")
    );
    let staging = data_dir.join(format!(".clone-staging-{stamp_label}"));
    std::fs::create_dir(&staging).map_err(|err| {
        VogtError::InvalidRequest(format!(
            "creating {staging} failed: {err}",
            staging = staging.display()
        ))
    })?;

    let outcome = (|| {
        for name in ["declared.sqlite3", "observed.sqlite3"] {
            std::fs::copy(source.join(name), staging.join(name)).map_err(|err| {
                VogtError::InvalidRequest(format!("copying {name} failed: {err}"))
            })?;
        }
        let staged = with_stores_at(ctx, &staging);
        let migrated = staged.declared.migrate()?;
        staged.observed.migrate()?;

        let (declared, _observed, principal, clock, ids, _config) = context_parts(&staged);
        let mut write = write_context(declared, principal, clock, ids);
        let sanitised = audited_write(&mut write, CLONE_OPERATION, reason, |txn, _actor| {
            let carry = txn.carry_credentials(&carried, &revoke_reason, cloned_at)?;
            let mut reset = Vec::new();
            for project in txn.list_projects(10_000, 0)? {
                if project.write_back != crate::core::WriteBack::Disabled {
                    txn.update_project(
                        &project.id,
                        &ProjectUpdate {
                            write_back: Some("none".to_string()),
                            ..ProjectUpdate::default()
                        },
                        cloned_at,
                    )?;
                    reset.push(project.slug);
                }
            }
            let running = txn.list_sessions(None, None, false, 10_000, 0)?;
            let sessions_closed = running.len() as i64;
            for session in &running {
                txn.mark_session_stopped(&session.id, cloned_at)?;
            }
            txn.set_instance_identity(&instance_id, &stamp)?;
            let summary = json!({
                "source_instance_id": manifest.instance_id,
                "backup_taken_at": crate::core::to_iso(manifest.taken_at),
                "source": source.display().to_string(),
                "tokens_kept": carry.tokens_kept,
                "source_tokens_revoked": carry.source_tokens_revoked,
                "write_back_reset": reset,
                "sessions_closed": sessions_closed,
            });
            Ok(WriteOutcome::new(
                (carry, reset, sessions_closed),
                "instance",
                &instance_id,
                summary.clone(),
                CLONE_EVENT,
                summary,
            ))
        })?;
        staged.observed.rebind_instance(&instance_id)?;

        for name in ["declared.sqlite3", "observed.sqlite3"] {
            for suffix in ["-wal", "-shm"] {
                let stale = data_dir.join(format!("{name}{suffix}"));
                if stale.exists() {
                    std::fs::remove_file(&stale).map_err(|err| {
                        VogtError::InvalidRequest(format!(
                            "removing {stale} failed: {err}",
                            stale = stale.display()
                        ))
                    })?;
                }
            }
            std::fs::rename(staging.join(name), data_dir.join(name)).map_err(|err| {
                VogtError::InvalidRequest(format!("replacing {name} failed: {err}"))
            })?;
            let leftover = staging.join(format!("{name}-wal"));
            if leftover.exists() {
                std::fs::rename(&leftover, data_dir.join(format!("{name}-wal"))).map_err(
                    |err| {
                        VogtError::InvalidRequest(format!(
                            "replacing {leftover} failed: {err}",
                            leftover = leftover.display()
                        ))
                    },
                )?;
            }
        }
        Ok::<_, VogtError>((sanitised, migrated.applied))
    })();
    let _ = std::fs::remove_dir_all(&staging);
    let (sanitised, migrations_applied) = outcome?;

    let engine_state = clone_engine_state(
        &source.join(ENGINE_STATE_DIR),
        ctx.config.engine_state_dir.as_deref(),
        params
            .get("include_engine_state")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    );
    let (carry, write_back_reset, sessions_closed) = sanitised;
    Ok(json!({
        "source": source.display().to_string(),
        "instance_id": instance_id,
        "source_instance_id": manifest.instance_id,
        "restored_from": crate::core::to_json_timestamp(manifest.taken_at),
        "cloned_at": crate::core::to_json_timestamp(cloned_at),
        "migrations_applied": migrations_applied,
        "declared_schema_version": ctx.declared.schema_version(),
        "tokens_kept": carry.tokens_kept,
        "source_tokens_revoked": carry.source_tokens_revoked,
        "password_logins_kept": carry.password_logins_kept,
        "source_password_logins_dropped": carry.source_password_logins_dropped,
        "forge_accounts_kept": carry.forge_accounts_kept,
        "source_forge_accounts_dropped": carry.source_forge_accounts_dropped,
        "write_back_reset": write_back_reset,
        "sessions_closed": sessions_closed,
        "engine_state": engine_state,
        "import_root_then": manifest.import_root,
        "import_root_now": ctx.config.resolved_import_root().display().to_string(),
    }))
}

/// Copy the allowlisted session history, or say why nothing was copied. Never
/// fatal: by the time this runs the stores are already in place.
fn clone_engine_state(source: &Path, target: Option<&Path>, include: bool) -> String {
    let left_behind =
        "push subscriptions and agent tasks are never copied by a clone; this engine keeps its own";
    if !include {
        return format!("not copied (pass include_engine_state to copy history); {left_behind}");
    }
    if !source.is_dir() {
        return format!("not in this backup; {left_behind}");
    }
    let Some(target) = target else {
        return format!(
            "in the backup at {source}, but no engine_state_dir is configured here, so it was not \
             copied; {left_behind}",
            source = source.display()
        );
    };
    let resolved = expand_user(target);
    let copied = (|| -> std::io::Result<Vec<String>> {
        std::fs::create_dir_all(&resolved)?;
        let mut names = Vec::new();
        let mut entries: Vec<_> = std::fs::read_dir(source)?.collect::<Result<_, _>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !CLONED_ENGINE_STATE
                .iter()
                .any(|allowed| name == *allowed || name.starts_with(&format!("{allowed}-")))
            {
                continue;
            }
            let destination = resolved.join(entry.file_name());
            if entry.file_type()?.is_dir() {
                copy_tree(&entry.path(), &destination)?;
            } else {
                std::fs::copy(entry.path(), &destination)?;
            }
            names.push(name.to_string());
        }
        Ok(names)
    })();
    match copied {
        Err(err) => format!(
            "copying session history into {resolved} failed: {err}",
            resolved = resolved.display()
        ),
        Ok(names) if names.is_empty() => {
            format!("no session history in the backup; {left_behind}")
        }
        Ok(names) => format!(
            "copied {names} into {resolved}; {left_behind}",
            names = names.join(", "),
            resolved = resolved.display()
        ),
    }
}

/// Copy one SQLite database consistently, using its own backup API. A file copy
/// taken while a write is in flight is a torn database, and WAL mode makes that
/// more likely rather than less.
fn snapshot(source: &Path, target: &Path) -> Result<(), VogtError> {
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|err| {
            VogtError::InvalidRequest(format!(
                "creating {parent} failed: {err}",
                parent = parent.display()
            ))
        })?;
    }
    let origin = rusqlite::Connection::open(source).map_err(|err| {
        VogtError::InvalidRequest(format!(
            "opening {source} failed: {err}",
            source = source.display()
        ))
    })?;
    let mut destination = rusqlite::Connection::open(target).map_err(|err| {
        VogtError::InvalidRequest(format!(
            "opening {target} failed: {err}",
            target = target.display()
        ))
    })?;
    let backup = rusqlite::backup::Backup::new(&origin, &mut destination).map_err(|err| {
        VogtError::InvalidRequest(format!(
            "snapshotting {source} failed: {err}",
            source = source.display()
        ))
    })?;
    backup.step(-1).map(|_| ()).map_err(|err| {
        VogtError::InvalidRequest(format!(
            "snapshotting {source} failed: {err}",
            source = source.display()
        ))
    })
}

/// Copy the engine's state directory, and say what happened either way. Not
/// fatal: a backup of the stores is worth having even when the engine's
/// directory is unreadable.
fn copy_engine_state(source: Option<&Path>, target: &Path) -> String {
    let Some(source) = source else {
        return "not configured".to_string();
    };
    let resolved = expand_user(source);
    if !resolved.is_dir() {
        return format!(
            "configured at {resolved}, which does not exist",
            resolved = resolved.display()
        );
    }
    match copy_tree(&resolved, target) {
        Ok(()) => format!("copied from {resolved}", resolved = resolved.display()),
        Err(err) => format!(
            "copy of {resolved} failed: {err}",
            resolved = resolved.display()
        ),
    }
}

/// Put the engine's state back, and say what happened either way. Never fatal
/// and never silent: the stores are already in place by the time this runs.
fn restore_engine_state(source: &Path, target: Option<&Path>) -> String {
    if !source.is_dir() {
        return "not in this backup".to_string();
    }
    let Some(target) = target else {
        return format!(
            "in the backup at {source}, but no engine_state_dir is configured here, so it was \
             not restored",
            source = source.display()
        );
    };
    let resolved = expand_user(target);
    if let Some(parent) = resolved.parent() {
        if let Err(err) = std::fs::create_dir_all(parent) {
            return format!(
                "restoring into {resolved} failed: {err}",
                resolved = resolved.display()
            );
        }
    }
    match copy_tree(source, &resolved) {
        Ok(()) => format!("restored into {resolved}", resolved = resolved.display()),
        Err(err) => format!(
            "restoring into {resolved} failed: {err}",
            resolved = resolved.display()
        ),
    }
}

/// `shutil.copytree` with symlinks preserved and an existing destination merged.
fn copy_tree(source: &Path, target: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(target)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let dest = target.join(entry.file_name());
        if entry.file_type()?.is_symlink() {
            let link = std::fs::read_link(entry.path())?;
            if dest.exists() || dest.symlink_metadata().is_ok() {
                let _ = std::fs::remove_file(&dest);
            }
            std::os::unix::fs::symlink(link, dest)?;
        } else if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &dest)?;
        } else {
            std::fs::copy(entry.path(), dest)?;
        }
    }
    Ok(())
}

fn now<C: Clock, I: IdFactory>(ctx: &AppContext<C, I>) -> Moment {
    ctx.clock.lock().expect("clock").now()
}

/// `Path.expanduser`: a leading `~` or `~/` becomes the home directory.
pub(crate) fn expand_user(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    let Some(rest) = text.strip_prefix('~') else {
        return path.to_path_buf();
    };
    if rest.is_empty() || rest.starts_with('/') {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest.trim_start_matches('/'));
        }
    }
    path.to_path_buf()
}

pub fn backup_op(ctx: &Built, params: Value) -> Result<Value, VogtError> {
    crate::application::services::dispatch!(ctx, backup, &params)
}

pub fn restore_op(ctx: &Built, params: Value) -> Result<Value, VogtError> {
    crate::application::services::dispatch!(ctx, restore, &params)
}

pub fn clone_op(ctx: &Built, params: Value) -> Result<Value, VogtError> {
    crate::application::services::dispatch!(ctx, clone_instance, &params)
}

/// Write the declared entities as JSON. Deliberately declared-only:
/// observations are evidence a collector can reproduce. Never exported: tokens,
/// password logins, forge accounts, auth decisions, sessions — nothing that
/// would let the file act as the instance it came from.
///
/// With `project`, the export is one project: its work items and only the
/// initiatives, labels and actors those reference.
pub fn export_instance<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    params: &Value,
) -> Result<Value, VogtError> {
    let project_slug = params
        .get("project")
        .and_then(Value::as_str)
        .filter(|slug| !slug.is_empty())
        .map(str::to_string);
    let view = ctx.declared.read()?;
    let stamp = view.clone_stamp()?;
    let mut projects = view.list_projects(10_000, 0)?;
    let mut items = view.list_work_items(&WorkFilter {
        limit: 10_000,
        exclude_terminal: false,
        include_superseded: true,
        ..WorkFilter::default()
    })?;
    let mut initiatives = view.list_initiatives(10_000, 0)?;
    let mut labels = view.list_labels(10_000, 0)?;
    let mut actors = view.list_actors(10_000, 0)?;
    if let Some(slug) = &project_slug {
        let scoped = view
            .project_by_slug(slug)?
            .ok_or_else(|| VogtError::NotFound(format!("no project with slug {slug:?}")))?;
        items.retain(|item| item.project_id.as_deref() == Some(scoped.id.as_str()));
        projects = vec![scoped];
    }
    let mut comments = Vec::new();
    for item in &items {
        comments.extend(view.comments_for(&item.id, 100_000, false)?);
    }
    if project_slug.is_some() {
        let initiative_ids: std::collections::BTreeSet<&str> = items
            .iter()
            .filter_map(|item| item.initiative_id.as_deref())
            .collect();
        initiatives.retain(|initiative| initiative_ids.contains(initiative.id.as_str()));
        let label_names: std::collections::BTreeSet<&str> = items
            .iter()
            .flat_map(|item| item.labels.iter().map(String::as_str))
            .collect();
        labels.retain(|label| label_names.contains(label.name.as_str()));
        let mut actor_ids: std::collections::BTreeSet<&str> = items
            .iter()
            .filter_map(|item| item.assignee_actor_id.as_deref())
            .collect();
        actor_ids.extend(comments.iter().map(|comment| comment.actor_id.as_str()));
        actors.retain(|actor| actor_ids.contains(actor.id.as_str()));
    }
    let payload = json!({
        "export_format_version": EXPORT_FORMAT_VERSION,
        "instance_id": view.instance_id()?,
        "exported_at": now(ctx).to_iso(),
        "revision": view.current_revision()?,
        "scope": {"project": project_slug},
        "clone_stamp": stamp.map(|stamp| json!({
            "source_instance_id": stamp.source_instance_id,
            "cloned_at": stamp.cloned_at.to_iso(),
            "backup_taken_at": stamp.backup_taken_at.to_iso(),
        })),
        "projects": projects,
        "work_items": items,
        "initiatives": initiatives,
        "labels": labels,
        "actors": actors,
        "comments": comments,
    });
    drop(view);

    let destination = expand_user(Path::new(
        params
            .get("destination")
            .and_then(Value::as_str)
            .unwrap_or(""),
    ));
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent).map_err(|err| {
            VogtError::InvalidRequest(format!(
                "creating {parent} failed: {err}",
                parent = parent.display()
            ))
        })?;
    }
    let text = crate::decisions::python_json_dumps_indent(&payload, 2) + "\n";
    std::fs::write(&destination, text).map_err(|err| {
        VogtError::InvalidRequest(format!(
            "writing {destination} failed: {err}",
            destination = destination.display()
        ))
    })?;
    Ok(json!({
        "path": destination.display().to_string(),
        "export_format_version": EXPORT_FORMAT_VERSION,
        "project": project_slug,
        "projects": projects.len(),
        "work_items": items.len(),
        "comments": comments.len(),
    }))
}

pub fn export_op(ctx: &Built, params: Value) -> Result<Value, VogtError> {
    crate::application::services::dispatch!(ctx, export_instance, &params)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::context::build_context;
    use crate::core::{ActorKind, Moment, Principal, SequentialIds, StepClock};
    use crate::storage::interface::{DeclaredStore, ReadView};

    fn opened() -> Built {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "vogt-lifecycle-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let built = build_context(
            crate::config::VogtConfig {
                data_dir: dir,
                ..crate::config::VogtConfig::default()
            },
            Some(Principal::new("local:test-user", ActorKind::Human, "Test").unwrap()),
            Some(StepClock::new(Moment::from_unix(1_700_000_000, 0))),
            Some(SequentialIds::new(None).unwrap()),
            None,
            None,
            None,
            None,
        )
        .unwrap();
        match &built {
            Built::StepSequential(ctx) => {
                ctx.declared.migrate().unwrap();
                ctx.declared
                    .bootstrap(
                        &Principal::new("local:test-user", ActorKind::Human, "Test").unwrap(),
                    )
                    .unwrap();
                ctx.observed.migrate().unwrap();
            }
            _other => panic!("expected a step clock"),
        }
        built
    }

    fn ctx(built: &Built) -> &AppContext<StepClock, SequentialIds> {
        match built {
            Built::StepSequential(ctx) => ctx,
            _other => panic!("expected a step clock"),
        }
    }

    #[test]
    fn a_backup_carries_a_manifest_and_publishes_an_event() {
        let built = opened();
        let destination = ctx(&built).config.resolved_data_dir().join("snap");
        let result = backup(
            ctx(&built),
            &json!({"destination": destination.display().to_string(), "reason": "before an upgrade"}),
        )
        .unwrap();
        let snapshot = PathBuf::from(result["path"].as_str().unwrap());
        assert!(snapshot.join("declared.sqlite3").is_file());
        assert!(snapshot.join("observed.sqlite3").is_file());

        let manifest: Value =
            serde_json::from_str(&std::fs::read_to_string(snapshot.join(MANIFEST_NAME)).unwrap())
                .unwrap();
        assert_eq!(manifest["instance_id"], result["instance_id"]);
        assert!(manifest["declared_schema_version"].as_i64().unwrap() > 0);
        assert!(!manifest["vogt_version"].as_str().unwrap().is_empty());
        assert_eq!(manifest["engine_state"], "not configured");

        let kinds: Vec<String> = ctx(&built)
            .declared
            .read()
            .unwrap()
            .list_events(0, 50, None)
            .unwrap()
            .iter()
            .map(|event| event.kind.clone())
            .collect();
        assert!(kinds.contains(&"instance.backed_up".to_string()));
    }

    #[test]
    fn backing_up_over_something_is_refused() {
        let built = opened();
        let destination = ctx(&built).config.resolved_data_dir().join("occupied");
        std::fs::create_dir_all(&destination).unwrap();
        std::fs::write(destination.join("something.txt"), "mine").unwrap();
        let error = backup(
            ctx(&built),
            &json!({"destination": destination.display().to_string(), "reason": "why"}),
        )
        .unwrap_err();
        assert!(matches!(error, VogtError::Conflict(message) if message.contains("not empty")));
    }

    #[test]
    fn the_default_destination_is_inside_the_data_dir() {
        let built = opened();
        let result = backup(ctx(&built), &json!({"reason": "why"})).unwrap();
        let path = PathBuf::from(result["path"].as_str().unwrap());
        assert_eq!(
            path.parent(),
            Some(ctx(&built).config.backups_dir().as_path())
        );
    }

    #[test]
    fn a_backup_restores_into_a_different_instance() {
        let source = opened();
        let taken = backup(
            ctx(&source),
            &json!({"destination": ctx(&source).config.resolved_data_dir().join("snap").display().to_string(), "reason": "why"}),
        )
        .unwrap();
        let target = opened();
        let restored = restore(
            ctx(&target),
            &json!({"source": taken["path"], "confirm": true, "reason": "disaster"}),
        )
        .unwrap();
        assert_eq!(restored["instance_id"], taken["instance_id"]);
        let after = ctx(&target).declared.read().unwrap().instance_id().unwrap();
        assert_eq!(after, taken["instance_id"].as_str().unwrap());
    }

    #[test]
    fn restore_refuses_without_confirmation_and_touches_nothing() {
        let source = opened();
        let taken = backup(
            ctx(&source),
            &json!({"destination": ctx(&source).config.resolved_data_dir().join("snap").display().to_string(), "reason": "why"}),
        )
        .unwrap();
        let target = opened();
        let before = ctx(&target).declared.read().unwrap().instance_id().unwrap();
        let error = restore(
            ctx(&target),
            &json!({"source": taken["path"], "reason": "why"}),
        )
        .unwrap_err();
        assert!(
            matches!(error, VogtError::InvalidRequest(message) if message.contains("--confirm"))
        );
        let after = ctx(&target).declared.read().unwrap().instance_id().unwrap();
        assert_eq!(after, before, "nothing was touched");
    }

    #[test]
    fn restoring_a_directory_that_is_not_a_backup_says_so() {
        let built = opened();
        let error = restore(
            ctx(&built),
            &json!({"source": ctx(&built).config.resolved_data_dir().display().to_string(), "confirm": true, "reason": "why"}),
        )
        .unwrap_err();
        assert!(
            matches!(error, VogtError::NotFound(message) if message.contains("not a Vogt backup"))
        );
    }

    #[test]
    fn a_manifest_from_the_future_is_refused_before_anything_is_touched() {
        let source = opened();
        let taken = backup(
            ctx(&source),
            &json!({"destination": ctx(&source).config.resolved_data_dir().join("snap").display().to_string(), "reason": "why"}),
        )
        .unwrap();
        let manifest_path = PathBuf::from(taken["path"].as_str().unwrap()).join(MANIFEST_NAME);
        let mut manifest: Value =
            serde_json::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();
        manifest["declared_schema_version"] = json!(999);
        std::fs::write(&manifest_path, serde_json::to_string(&manifest).unwrap()).unwrap();

        let target = opened();
        let before = ctx(&target).declared.read().unwrap().instance_id().unwrap();
        let error = restore(
            ctx(&target),
            &json!({"source": taken["path"], "confirm": true, "reason": "why"}),
        )
        .unwrap_err();
        assert!(
            matches!(error, VogtError::InvalidRequest(message) if message.contains("forward-only"))
        );
        let after = ctx(&target).declared.read().unwrap().instance_id().unwrap();
        assert_eq!(after, before, "the live store was not replaced");
    }

    /// A backup whose stores pass, with one of them replaced by `plant`.
    fn planted(name: &str, plant: impl FnOnce(&std::path::Path)) -> (Built, Value) {
        let source = opened();
        let taken = backup(
            ctx(&source),
            &json!({"destination": ctx(&source).config.resolved_data_dir().join("snap").display().to_string(), "reason": "why"}),
        )
        .unwrap();
        plant(&PathBuf::from(taken["path"].as_str().unwrap()).join(name));
        (source, taken)
    }

    /// Restore must refuse with exactly this message, and the target's live data
    /// must survive.
    fn refused(source: Built, taken: &Value, message: &str) {
        let target = opened();
        let before = ctx(&target).declared.read().unwrap().instance_id().unwrap();
        let error = restore(
            ctx(&target),
            &json!({"source": taken["path"], "confirm": true, "reason": "why"}),
        )
        .unwrap_err();
        assert!(
            matches!(error, VogtError::InvalidRequest(ref got) if got == message),
            "got {error:?}, wanted {message}"
        );
        let after = ctx(&target).declared.read().unwrap().instance_id().unwrap();
        assert_eq!(after, before, "the live store was not replaced");
        drop(source);
    }

    #[test]
    fn restore_refuses_a_symlinked_store_and_touches_nothing() {
        let (source, taken) = planted("declared.sqlite3", |store| {
            let real = store.with_extension("real");
            std::fs::rename(store, &real).unwrap();
            std::os::unix::fs::symlink(&real, store).unwrap();
        });
        refused(
            source,
            &taken,
            "declared.sqlite3 is a symlink; refusing to restore it",
        );
    }

    #[test]
    fn restore_refuses_a_corrupt_store_and_touches_nothing() {
        let (source, taken) = planted("observed.sqlite3", |store| {
            std::fs::write(store, "not a database".repeat(512)).unwrap();
        });
        refused(
            source,
            &taken,
            "observed.sqlite3 failed its integrity check; refusing to restore it",
        );
    }

    #[test]
    fn restore_refuses_a_damaged_store_and_touches_nothing() {
        // A valid header over damaged pages fails quick_check too.
        let (source, taken) = planted("declared.sqlite3", |store| {
            let mut data = std::fs::read(store).unwrap();
            let page_size = u16::from_be_bytes([data[16], data[17]]) as usize;
            for byte in data.iter_mut().skip(page_size) {
                *byte = 0xA5;
            }
            std::fs::write(store, data).unwrap();
        });
        refused(
            source,
            &taken,
            "declared.sqlite3 failed its integrity check; refusing to restore it",
        );
    }

    #[test]
    fn restore_refuses_a_database_that_is_not_a_vogt_store() {
        let (source, taken) = planted("observed.sqlite3", |store| {
            std::fs::remove_file(store).unwrap();
            let conn = rusqlite::Connection::open(store).unwrap();
            conn.execute_batch("CREATE TABLE something_else (x INTEGER)")
                .unwrap();
        });
        refused(
            source,
            &taken,
            "observed.sqlite3 is not a Vogt store; refusing to restore it",
        );
    }

    #[test]
    fn a_good_store_restores_and_leaves_no_wal_behind() {
        let source = opened();
        let taken = backup(
            ctx(&source),
            &json!({"destination": ctx(&source).config.resolved_data_dir().join("snap").display().to_string(), "reason": "why"}),
        )
        .unwrap();
        let target = opened();
        restore(
            ctx(&target),
            &json!({"source": taken["path"], "confirm": true, "reason": "why"}),
        )
        .unwrap();
        let after = ctx(&target).declared.read().unwrap().instance_id().unwrap();
        assert_eq!(after, taken["instance_id"].as_str().unwrap());
        let backup_dir = PathBuf::from(taken["path"].as_str().unwrap());
        let leftovers: Vec<_> = std::fs::read_dir(&backup_dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with("-wal") || name.ends_with("-shm"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "validating a backup leaves nothing behind in it: {leftovers:?}"
        );
    }

    #[test]
    fn an_export_writes_the_declared_entities_and_nothing_else() {
        let built = opened();
        let destination = ctx(&built).config.resolved_data_dir().join("export.json");
        let result = export_instance(
            ctx(&built),
            &json!({"destination": destination.display().to_string(), "reason": "why"}),
        )
        .unwrap();
        let payload: Value =
            serde_json::from_str(&std::fs::read_to_string(&destination).unwrap()).unwrap();
        assert_eq!(result["export_format_version"], 2);
        assert!(result["projects"].as_i64().unwrap() >= 0);
        assert_eq!(payload["export_format_version"], 2);
        assert!(payload["scope"]["project"].is_null());
        assert!(payload["clone_stamp"].is_null());
        for absent in ["observations", "tokens", "sessions", "auth_decisions"] {
            assert!(
                payload.get(absent).is_none(),
                "{absent} is not part of an export"
            );
        }
        assert!(payload["work_items"].is_array());
        assert!(payload["comments"].is_array());
    }

    #[test]
    fn exporting_one_project_that_does_not_exist_says_so() {
        let built = opened();
        let destination = ctx(&built).config.resolved_data_dir().join("export.json");
        let error = export_instance(
            ctx(&built),
            &json!({"destination": destination.display().to_string(), "project": "missing", "reason": "why"}),
        )
        .unwrap_err();
        assert!(
            matches!(error, VogtError::NotFound(message) if message.contains("no project with slug"))
        );
        assert!(!destination.exists(), "nothing was written");
    }

    #[test]
    fn a_garbled_taken_at_is_refused_before_anything_is_touched() {
        let source = opened();
        let taken = backup(
            ctx(&source),
            &json!({"destination": ctx(&source).config.resolved_data_dir().join("snap").display().to_string(), "reason": "why"}),
        )
        .unwrap();
        let manifest_path = PathBuf::from(taken["path"].as_str().unwrap()).join(MANIFEST_NAME);
        let mut manifest: Value =
            serde_json::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();
        manifest["taken_at"] = json!("not a time");
        std::fs::write(&manifest_path, serde_json::to_string(&manifest).unwrap()).unwrap();

        let target = opened();
        let before = ctx(&target).declared.read().unwrap().instance_id().unwrap();
        let error = restore(
            ctx(&target),
            &json!({"source": taken["path"], "confirm": true, "reason": "why"}),
        )
        .unwrap_err();
        assert!(
            matches!(error, VogtError::InvalidRequest(message) if message.contains("not a timestamp"))
        );
        let after = ctx(&target).declared.read().unwrap().instance_id().unwrap();
        assert_eq!(after, before, "the live store was not replaced");
    }

    #[test]
    fn the_default_label_is_the_compact_timestamp() {
        let built = opened();
        let result = backup(ctx(&built), &json!({"reason": "why"})).unwrap();
        let name = PathBuf::from(result["path"].as_str().unwrap())
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_string();
        // The clock advances once per read, and backup reads it for the
        // instance id, the label, and the event.
        assert_eq!(name, "20231114T221323Z");
        // The answer uses pydantic's JSON spelling, with Z rather than +00:00.
        assert!(result["taken_at"].as_str().unwrap().ends_with('Z'));
    }

    #[test]
    fn an_incomplete_backup_is_refused() {
        let source = opened();
        let taken = backup(
            ctx(&source),
            &json!({"destination": ctx(&source).config.resolved_data_dir().join("snap").display().to_string(), "reason": "why"}),
        )
        .unwrap();
        std::fs::remove_file(
            PathBuf::from(taken["path"].as_str().unwrap()).join("observed.sqlite3"),
        )
        .unwrap();
        let target = opened();
        let error = restore(
            ctx(&target),
            &json!({"source": taken["path"], "confirm": true, "reason": "why"}),
        )
        .unwrap_err();
        assert!(
            matches!(error, VogtError::NotFound(message) if message.contains("missing observed.sqlite3"))
        );
    }

    /// A backup of one instance and a second instance to clone it into. Both
    /// fresh databases take `ins_0001` from the shared test id factory, so the
    /// target's id is restamped — two real instances never share one.
    fn pair() -> (Built, Built, Value) {
        let source = opened();
        let taken = backup(
            ctx(&source),
            &json!({
                "destination": ctx(&source).config.resolved_data_dir().join("snap").display().to_string(),
                "reason": "before clone",
            }),
        )
        .unwrap();
        let target = opened();
        let path = ctx(&target)
            .config
            .resolved_data_dir()
            .join("declared.sqlite3");
        let conn = crate::storage::sqlite::connection::connect(&path).unwrap();
        conn.execute(
            "UPDATE meta SET value = 'ins_target' WHERE key = 'instance_id'",
            [],
        )
        .unwrap();
        drop(conn);
        (source, target, taken)
    }

    #[test]
    fn a_clone_keeps_the_targets_identity_and_records_the_stamp() {
        let (source, target, taken) = pair();
        let source_id = ctx(&source).declared.read().unwrap().instance_id().unwrap();
        let target_id = ctx(&target).declared.read().unwrap().instance_id().unwrap();
        let result = clone_instance(
            ctx(&target),
            &json!({"source": taken["path"], "confirm": true, "reason": "stand up a copy"}),
        )
        .unwrap();

        assert_eq!(result["instance_id"], target_id);
        assert_eq!(result["source_instance_id"], source_id);
        assert_ne!(source_id, target_id);
        let after = ctx(&target).declared.read().unwrap();
        assert_eq!(after.instance_id().unwrap(), target_id);
        let stamp = after.clone_stamp().unwrap().unwrap();
        assert_eq!(stamp.source_instance_id, source_id);

        let kinds: Vec<String> = after
            .list_events(0, 50, None)
            .unwrap()
            .iter()
            .map(|event| event.kind.clone())
            .collect();
        assert!(kinds.contains(&"instance.cloned".to_string()));
    }

    #[test]
    fn cloning_a_backup_of_this_instance_is_refused() {
        let built = opened();
        let taken = backup(
            ctx(&built),
            &json!({
                "destination": ctx(&built).config.resolved_data_dir().join("snap").display().to_string(),
                "reason": "why",
            }),
        )
        .unwrap();
        let error = clone_instance(
            ctx(&built),
            &json!({"source": taken["path"], "confirm": true, "reason": "why"}),
        )
        .unwrap_err();
        assert!(
            matches!(error, VogtError::InvalidRequest(message) if message.contains("Use `restore`"))
        );
    }

    #[test]
    fn clone_refuses_without_confirmation_and_touches_nothing() {
        let (_source, target, taken) = pair();
        let before = ctx(&target).declared.read().unwrap().instance_id().unwrap();
        let error = clone_instance(
            ctx(&target),
            &json!({"source": taken["path"], "reason": "why"}),
        )
        .unwrap_err();
        assert!(matches!(
            error,
            VogtError::InvalidRequest(message) if message.contains("--confirm")
        ));
        let after = ctx(&target).declared.read().unwrap().instance_id().unwrap();
        assert_eq!(after, before, "nothing was touched");
    }

    #[test]
    fn a_clone_does_not_copy_engine_state_unless_asked() {
        let (_source, target, taken) = pair();
        let result = clone_instance(
            ctx(&target),
            &json!({"source": taken["path"], "confirm": true, "reason": "why"}),
        )
        .unwrap();
        assert!(result["engine_state"]
            .as_str()
            .unwrap()
            .contains("not copied"));
        assert!(result["engine_state"]
            .as_str()
            .unwrap()
            .contains("push subscriptions"));
    }
}
