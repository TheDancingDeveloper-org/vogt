//! Instance lifecycle. Ports `init_instance` and the bootstrap it performs.
//!
//! Creates the data directory, migrates both databases, then writes the
//! instance: its id, the initiating actor, and one audit row. No event is
//! emitted and the revision stays 0, so a client that connects afterwards sees
//! an empty feed rather than an instance-creation event it cannot act on. The
//! token file is written only when `VOGT_BOOTSTRAP_TOKEN` is set, so a recorded
//! run can place a known secret without this binary minting one.

use std::path::Path;

use crate::storage::interface::MigrationReport;
use crate::storage::sqlite::migrator;
use crate::storage::sqlite::{declared_path, observed_path};

pub struct InitOutcome {
    pub declared: MigrationReport,
    pub observed: MigrationReport,
    pub created: bool,
    pub instance_id: String,
}

pub fn init(
    data_dir: &Path,
    now: &str,
    observed_now: &str,
) -> Result<InitOutcome, migrator::MigrateError> {
    std::fs::create_dir_all(data_dir)?;
    let root = migrator::migrations_root();
    let declared_existed = declared_path(data_dir).exists();
    let declared = migrator::open_and_migrate(
        &declared_path(data_dir),
        "declared",
        root.as_deref().map(|path| path.join("declared")).as_deref(),
        "vogt-core",
        now,
    )?;
    seed_workflows(data_dir, now)?;
    let observed = migrator::open_and_migrate(
        &observed_path(data_dir),
        "observed",
        root.as_deref().map(|path| path.join("observed")).as_deref(),
        "vogt-core",
        observed_now,
    )?;
    if let Some(token) = std::env::var_os("VOGT_BOOTSTRAP_TOKEN") {
        let path = data_dir.join("token");
        if !path.exists() {
            std::fs::write(&path, token.as_encoded_bytes())?;
        }
    }
    let instance_id = if !declared_existed {
        let instance_id = bootstrap(data_dir, now)?;
        bind_instance(data_dir, &instance_id, now)?;
        instance_id
    } else {
        instance_id_of(data_dir)?
    };
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
fn seed_workflows(data_dir: &Path, now: &str) -> Result<(), migrator::MigrateError> {
    use rusqlite::params;

    let conn = crate::storage::sqlite::connection::connect(&declared_path(data_dir))?;
    let at = clock_stamp(now, 0);
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
fn bootstrap(data_dir: &Path, now: &str) -> Result<String, migrator::MigrateError> {
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
    let mut ids = SequentialIds::load(&data_dir.join("test-ids.json"));
    let user = os_user();
    let instance_id = ids.next("ins");
    let actor_id = ids.next("act");
    let audit_id = ids.next("aud");
    let txn_id = ids.next("txn");
    let at = clock_stamp(now, 2);
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
    ids.save()?;
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
    now: &str,
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
    let at = clock_stamp(now, 3);
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

/// `LOGNAME`, then `USER`, then a fallback. `getpass.getuser` reads `LOGNAME`.
fn os_user() -> String {
    std::env::var("LOGNAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_else(|_| "unknown".to_string())
}

/// The instant bootstrap stamps, two seconds after the clock start. `init`
/// reads the clock twice before bootstrap does — once is the harness, once is
/// the migration `now` — so the rows land where Python's step clock puts them.
fn clock_stamp(fallback: &str, steps: i64) -> String {
    let start = std::env::var("VOGT_TEST_CLOCK_START")
        .ok()
        .and_then(|text| crate::core::from_iso(&text).ok())
        .map(|moment| moment.unix_seconds());
    match start {
        Some(seconds) => crate::core::to_iso(crate::core::Moment::from_unix(seconds + steps, 0)),
        None => fallback.to_string(),
    }
}

/// Sequential ids persisted across processes when `VOGT_TEST_IDS=sequential`,
/// so `init` and the `serve` that follows agree. Ports `SequentialIds`.
struct SequentialIds {
    path: std::path::PathBuf,
    counts: std::collections::BTreeMap<String, u32>,
    persist: bool,
}

impl SequentialIds {
    fn load(path: &Path) -> Self {
        let persist = std::env::var("VOGT_TEST_IDS").ok().as_deref() == Some("sequential");
        let counts = if persist {
            std::fs::read_to_string(path)
                .ok()
                .map(|text| {
                    text.trim_matches(|c| c == '{' || c == '}')
                        .split(',')
                        .filter_map(|pair| {
                            let (key, value) = pair.split_once(':')?;
                            let key = key.trim().trim_matches('"').to_string();
                            let value = value.trim().parse().ok()?;
                            Some((key, value))
                        })
                        .collect()
                })
                .unwrap_or_default()
        } else {
            std::collections::BTreeMap::new()
        };
        Self {
            path: path.to_path_buf(),
            counts,
            persist,
        }
    }

    fn next(&mut self, prefix: &str) -> String {
        let count = self.counts.entry(prefix.to_string()).or_insert(0);
        *count += 1;
        format!("{prefix}_{count:04}")
    }

    fn save(&self) -> Result<(), migrator::MigrateError> {
        if !self.persist {
            return Ok(());
        }
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let body = self
            .counts
            .iter()
            .map(|(key, value)| format!("\"{key}\":{value}"))
            .collect::<Vec<_>>()
            .join(",");
        std::fs::write(&self.path, format!("{{{body}}}"))?;
        Ok(())
    }
}
