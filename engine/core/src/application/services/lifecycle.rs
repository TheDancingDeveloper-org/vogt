//! Backup and restore. Ports the matching half of `lifecycle.py`.
//!
//! A backup snapshots both stores with SQLite's own backup API and writes a
//! manifest naming each schema version. Restore verifies that manifest before
//! touching anything: the failure this prevents is discovering a problem half
//! way through, with the live data already gone.
//!
//! `clone`, `export` and `import` are not here. Clone rewrites identity and
//! forge state this build does not carry yet, and export serialises every
//! declared entity.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::application::context::{AppContext, Built};
use crate::core::{Clock, IdFactory, Moment};
use crate::errors::VogtError;
use crate::storage::interface::{DeclaredStore, ObservedStore, ReadView, WorkFilter};
use crate::VERSION;

const MANIFEST_NAME: &str = "manifest.json";
const MANIFEST_VERSION: i64 = 2;
const ENGINE_STATE_DIR: &str = "engine-state";
const BACKUP_EVENT: &str = "instance.backed_up";
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
        "taken_at": crate::core::to_iso(manifest.taken_at),
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
        if !source.join(name).is_file() {
            return Err(VogtError::NotFound(format!(
                "{source} is missing {name}",
                source = source.display()
            )));
        }
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
        "restored_from": crate::core::to_iso(manifest.taken_at),
        "migrations_applied": migrated.applied,
        "declared_schema_version": ctx.declared.schema_version(),
        "engine_state": engine_state,
        "import_root_then": manifest.import_root,
        "import_root_now": ctx.config.resolved_import_root().display().to_string(),
    }))
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
fn expand_user(path: &Path) -> PathBuf {
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
        comments.extend(view.comments_for(&item.id, 100_000)?);
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
    let text = serde_json::to_string_pretty(&payload).expect("export is json") + "\n";
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
}
