//! Forward-only migrations. Ports `src/vogt/storage/sqlite/migrator.py`.
//!
//! The bookkeeping tables are `migrations` and `migration_lock`, not
//! `schema_migrations`: that is what the Python migrator creates, and a
//! Python-written database must upgrade without a second table. Checksums are
//! SHA-256 of the stripped SQL, matching `checksum_of`.
//!
//! SQL is read from the checkout at runtime (`VOGT_MIGRATIONS_DIR`, else a
//! path relative to this crate) so a migration added on `main` is picked up
//! without regenerating embedded strings.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension};
use sha2::{Digest, Sha256};

use super::connection::{connect, split_statements};

const FRAMEWORK: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS migrations (
        id         TEXT PRIMARY KEY NOT NULL,
        applied_at TEXT NOT NULL,
        checksum   TEXT NOT NULL
    )",
    "CREATE TABLE IF NOT EXISTS migration_lock (
        id          INTEGER PRIMARY KEY NOT NULL CHECK (id = 1),
        holder      TEXT,
        acquired_at TEXT
    )",
    "INSERT INTO migration_lock (id, holder, acquired_at)
     SELECT 1, NULL, NULL
     WHERE NOT EXISTS (SELECT 1 FROM migration_lock WHERE id = 1)",
];

#[derive(Debug)]
pub struct Migration {
    pub id: String,
    pub sql: String,
    pub checksum: String,
}

impl Migration {
    pub fn number(&self) -> i64 {
        self.id
            .split('_')
            .next()
            .unwrap_or("0")
            .parse()
            .unwrap_or(0)
    }
}

#[derive(Debug)]
pub struct Report {
    pub applied: Vec<String>,
    pub version: i64,
}

#[derive(Debug)]
pub enum MigrateError {
    Sql(rusqlite::Error),
    Io(std::io::Error),
    Message(String),
}

impl std::fmt::Display for MigrateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Sql(err) => write!(f, "{err}"),
            Self::Io(err) => write!(f, "{err}"),
            Self::Message(msg) => write!(f, "{msg}"),
        }
    }
}

impl From<rusqlite::Error> for MigrateError {
    fn from(err: rusqlite::Error) -> Self {
        Self::Sql(err)
    }
}

impl From<std::io::Error> for MigrateError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err)
    }
}

pub fn checksum_of(sql: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(sql.trim().as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Directory holding `declared/` and `observed/`.
pub fn migrations_root() -> PathBuf {
    if let Some(dir) = std::env::var_os("VOGT_MIGRATIONS_DIR") {
        return PathBuf::from(dir);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../src/vogt/storage/sqlite/migrations")
}

pub fn load_migrations(directory: &Path) -> Result<Vec<Migration>, MigrateError> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(directory)?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "sql"))
        .collect();
    paths.sort();
    let mut migrations = Vec::new();
    let mut seen = Vec::new();
    for path in paths {
        let sql = std::fs::read_to_string(&path)?;
        let id = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("")
            .to_string();
        let migration = Migration {
            checksum: checksum_of(&sql),
            id,
            sql,
        };
        if seen.contains(&migration.number()) {
            return Err(MigrateError::Message(format!(
                "duplicate migration number in {}: {}",
                directory.display(),
                migration.id
            )));
        }
        seen.push(migration.number());
        migrations.push(migration);
    }
    Ok(migrations)
}

pub fn bundled_version(directory: &Path) -> Result<i64, MigrateError> {
    Ok(load_migrations(directory)?
        .iter()
        .map(Migration::number)
        .max()
        .unwrap_or(0))
}

pub fn applied_version(conn: &Connection) -> Result<i64, MigrateError> {
    if !table_exists(conn, "migrations")? {
        return Ok(0);
    }
    let id: Option<String> = conn
        .query_row(
            "SELECT id FROM migrations ORDER BY id DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    Ok(id
        .as_deref()
        .and_then(|id| id.split('_').next())
        .and_then(|number| number.parse().ok())
        .unwrap_or(0))
}

pub fn table_exists(conn: &Connection, name: &str) -> Result<bool, MigrateError> {
    let found: Option<String> = conn
        .query_row(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [name],
            |row| row.get(0),
        )
        .optional()?;
    Ok(found.is_some())
}

/// Bring one database forward. `now` is an ISO-8601 timestamp, the shape
/// Python writes into `applied_at`.
pub fn migrate(
    conn: &mut Connection,
    store: &str,
    directory: &Path,
    holder: &str,
    now: &str,
) -> Result<Report, MigrateError> {
    ensure_framework(conn)?;
    let available = load_migrations(directory)?;
    acquire_lock(conn, store, holder, now)?;
    let result: Result<Report, MigrateError> = (|| {
        verify_forward_only(conn, store, &available)?;
        let applied = applied_ids(conn)?;
        let pending: Vec<&Migration> = available
            .iter()
            .filter(|migration| !applied.contains(&migration.id))
            .collect();
        for migration in &pending {
            apply_one(conn, store, migration, now)?;
        }
        Ok(Report {
            applied: pending
                .iter()
                .map(|migration| migration.id.clone())
                .collect(),
            version: applied_version(conn)?,
        })
    })();
    release_lock(conn, holder)?;
    result
}

fn ensure_framework(conn: &Connection) -> Result<(), MigrateError> {
    conn.execute_batch("BEGIN IMMEDIATE")?;
    for statement in FRAMEWORK {
        if let Err(err) = conn.execute(statement, []) {
            let _ = conn.execute_batch("ROLLBACK");
            return Err(err.into());
        }
    }
    conn.execute_batch("COMMIT")?;
    Ok(())
}

fn applied_ids(conn: &Connection) -> Result<Vec<String>, MigrateError> {
    let mut stmt = conn.prepare("SELECT id FROM migrations")?;
    let rows = stmt.query_map([], |row| row.get(0))?;
    let mut ids = Vec::new();
    for row in rows {
        ids.push(row?);
    }
    Ok(ids)
}

fn verify_forward_only(
    conn: &Connection,
    store: &str,
    available: &[Migration],
) -> Result<(), MigrateError> {
    let mut stmt = conn.prepare("SELECT id, checksum FROM migrations ORDER BY id")?;
    let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
    for row in rows {
        let (id, checksum): (String, String) = row?;
        let known = available.iter().find(|migration| migration.id == id);
        match known {
            None => {
                return Err(MigrateError::Message(format!(
                    "{store}: migration {id} is applied in the database but absent from this build"
                )));
            }
            Some(migration) if migration.checksum != checksum => {
                return Err(MigrateError::Message(format!(
                    "{store}: migration {id} was modified after being applied"
                )));
            }
            Some(_) => {}
        }
    }
    Ok(())
}

fn apply_one(
    conn: &Connection,
    store: &str,
    migration: &Migration,
    now: &str,
) -> Result<(), MigrateError> {
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let result: Result<(), rusqlite::Error> = (|| {
        for statement in split_statements(&migration.sql) {
            conn.execute(&statement, [])?;
        }
        conn.execute(
            "INSERT INTO migrations (id, applied_at, checksum) VALUES (?1, ?2, ?3)",
            (&migration.id, now, &migration.checksum),
        )?;
        Ok(())
    })();
    match result {
        Ok(()) => {
            conn.execute_batch("COMMIT")?;
            Ok(())
        }
        Err(err) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(MigrateError::Message(format!(
                "{store}: migration {} failed: {err}",
                migration.id
            )))
        }
    }
}

fn acquire_lock(
    conn: &Connection,
    store: &str,
    holder: &str,
    now: &str,
) -> Result<(), MigrateError> {
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let row: Option<(Option<String>, Option<String>)> = conn
        .query_row(
            "SELECT holder, acquired_at FROM migration_lock WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    // A held lock is refused rather than stolen. Python steals one older than
    // fifteen minutes; this skeleton migrates its own databases and never holds
    // a lock across a process, so the steal path is not needed yet.
    if let Some((Some(current), Some(acquired_at))) = &row {
        let _ = conn.execute_batch("ROLLBACK");
        return Err(MigrateError::Message(format!(
            "{store}: migration lock held by {current} since {acquired_at}"
        )));
    }
    if let Err(err) = conn.execute(
        "UPDATE migration_lock SET holder = ?1, acquired_at = ?2 WHERE id = 1",
        (holder, now),
    ) {
        let _ = conn.execute_batch("ROLLBACK");
        return Err(err.into());
    }
    conn.execute_batch("COMMIT")?;
    Ok(())
}

fn release_lock(conn: &Connection, holder: &str) -> Result<(), MigrateError> {
    conn.execute_batch("BEGIN IMMEDIATE")?;
    if let Err(err) = conn.execute(
        "UPDATE migration_lock SET holder = NULL, acquired_at = NULL WHERE id = 1 AND holder = ?1",
        [holder],
    ) {
        let _ = conn.execute_batch("ROLLBACK");
        return Err(err.into());
    }
    conn.execute_batch("COMMIT")?;
    Ok(())
}

pub fn open_and_migrate(
    path: &Path,
    store: &str,
    directory: &Path,
    holder: &str,
    now: &str,
) -> Result<Report, MigrateError> {
    let mut conn = connect(path)?;
    migrate(&mut conn, store, directory, holder, now)
}
