//! Forward-only migrations. Ports `src/vogt/storage/sqlite/migrator.py`.
//!
//! The bookkeeping tables are `migrations` and `migration_lock`, not
//! `schema_migrations`: that is what the Python migrator creates, and a
//! Python-written database must upgrade without a second table. Checksums are
//! SHA-256 of the stripped SQL, matching `checksum_of`.
//!
//! SQL is embedded at compile time (`embedded.rs`), because the stack image
//! ships the binary without `src/vogt`. `VOGT_MIGRATIONS_DIR` still overrides
//! that for a checkout ahead of the binary.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension};
use sha2::{Digest, Sha256};

use super::connection::{connect, split_statements};
use super::embedded;

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

/// A lock older than this is stolen. A crashed process must not wedge the
/// instance forever; Python's `DEFAULT_STALE_AFTER` is the same fifteen minutes.
const STALE_AFTER_SECONDS: i64 = 15 * 60;

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

/// Directory holding `declared/` and `observed/`, when `VOGT_MIGRATIONS_DIR`
/// points at a checkout ahead of this binary. Absent, the embedded SQL is used.
pub fn migrations_root() -> Option<PathBuf> {
    std::env::var_os("VOGT_MIGRATIONS_DIR").map(PathBuf::from)
}

fn from_pairs(pairs: &[(&str, &str)]) -> Result<Vec<Migration>, MigrateError> {
    let mut migrations = Vec::new();
    let mut seen = Vec::new();
    for (id, sql) in pairs {
        let migration = Migration {
            checksum: checksum_of(sql),
            id: (*id).to_string(),
            sql: (*sql).to_string(),
        };
        if seen.contains(&migration.number()) {
            return Err(MigrateError::Message(format!(
                "duplicate migration number: {}",
                migration.id
            )));
        }
        seen.push(migration.number());
        migrations.push(migration);
    }
    Ok(migrations)
}

/// The migrations for one store. `directory` wins when given, so a checkout
/// ahead of the binary is tested against its own files; otherwise the SQL
/// embedded in the binary is used.
pub fn load_migrations(
    store: &str,
    directory: Option<&Path>,
) -> Result<Vec<Migration>, MigrateError> {
    if let Some(directory) = directory {
        return load_directory(directory);
    }
    from_pairs(match store {
        "observed" => embedded::OBSERVED,
        _ => embedded::DECLARED,
    })
}

fn load_directory(directory: &Path) -> Result<Vec<Migration>, MigrateError> {
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

pub fn bundled_version(store: &str, directory: Option<&Path>) -> Result<i64, MigrateError> {
    Ok(load_migrations(store, directory)?
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
    directory: Option<&Path>,
    holder: &str,
    now: &str,
) -> Result<Report, MigrateError> {
    ensure_framework(conn)?;
    let available = load_migrations(store, directory)?;
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
    // A lock younger than fifteen minutes is refused. An older one is stolen:
    // the holder crashed, and leaving it would wedge the instance forever.
    // Python compares `now - acquired_at < stale_after` the same way.
    if let Some((Some(current), Some(acquired_at))) = &row {
        match (parse_iso(now), parse_iso(acquired_at)) {
            (Some(now_secs), Some(acquired_secs))
                if now_secs.saturating_sub(acquired_secs) < STALE_AFTER_SECONDS =>
            {
                let _ = conn.execute_batch("ROLLBACK");
                return Err(MigrateError::Message(format!(
                    "{store}: migration lock held by {current} since {acquired_at}"
                )));
            }
            _ => {}
        }
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
    directory: Option<&Path>,
    holder: &str,
    now: &str,
) -> Result<Report, MigrateError> {
    let mut conn = connect(path)?;
    migrate(&mut conn, store, directory, holder, now)
}

/// Seconds since the epoch for a UTC timestamp written as
/// `YYYY-MM-DDTHH:MM:SS`, optionally with a fraction and a `Z` or `+00:00`
/// suffix — the shapes `to_iso` and Python's `isoformat` produce. Anything else
/// is unreadable, and the caller treats that as stale rather than wedging.
fn parse_iso(text: &str) -> Option<i64> {
    let body = text.trim().trim_end_matches('Z');
    let (date, time) = body.split_once('T')?;
    let mut date_parts = date.split('-');
    let year: i64 = date_parts.next()?.parse().ok()?;
    let month: u32 = date_parts.next()?.parse().ok()?;
    let day: u32 = date_parts.next()?.parse().ok()?;
    let time = time.split(['+', '-']).next()?;
    let (clock, _fraction) = time.split_once('.').unwrap_or((time, ""));
    let mut clock_parts = clock.split(':');
    let hour: u32 = clock_parts.next()?.parse().ok()?;
    let minute: u32 = clock_parts.next()?.parse().ok()?;
    let second: u32 = clock_parts.next()?.parse().ok()?;
    let days = days_from_civil(year, month, day)?;
    Some(days * 86_400 + i64::from(hour) * 3600 + i64::from(minute) * 60 + i64::from(second))
}

fn days_from_civil(year: i64, month: u32, day: u32) -> Option<i64> {
    if !(1..=12).contains(&month) || day == 0 || day > 31 {
        return None;
    }
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year.rem_euclid(400) as u64;
    let mp = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * mp as u64 + 2) / 5 + day as u64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe as i64 - 719_468)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_embedded_sql_matches_the_checkout() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../src/vogt/storage/sqlite/migrations");
        for (store, pairs) in [
            ("declared", embedded::DECLARED),
            ("observed", embedded::OBSERVED),
        ] {
            let directory = root.join(store);
            let from_disk = load_directory(&directory).expect("checkout migrations");
            let embedded = from_pairs(pairs).expect("embedded migrations");
            let disk_ids: Vec<&str> = from_disk.iter().map(|m| m.id.as_str()).collect();
            let embedded_ids: Vec<&str> = embedded.iter().map(|m| m.id.as_str()).collect();
            assert_eq!(disk_ids, embedded_ids, "{store} ids");
            for (disk, embedded) in from_disk.iter().zip(&embedded) {
                assert_eq!(disk.checksum, embedded.checksum, "{}", disk.id);
            }
        }
    }

    fn hold(conn: &Connection, holder: &str, acquired_at: &str) {
        ensure_framework(conn).unwrap();
        conn.execute(
            "UPDATE migration_lock SET holder = ?1, acquired_at = ?2 WHERE id = 1",
            (holder, acquired_at),
        )
        .unwrap();
    }

    fn holder_of(conn: &Connection) -> Option<String> {
        conn.query_row(
            "SELECT holder FROM migration_lock WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .unwrap()
    }

    #[test]
    fn a_fresh_lock_is_refused_and_a_fifteen_minute_old_one_is_stolen() {
        let dir = std::env::temp_dir().join(format!("vogt-lock-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("declared.sqlite3");
        let now = "2026-10-08T12:15:00+00:00";

        let conn = connect(&path).unwrap();
        hold(&conn, "crashed", "2026-10-08T12:00:01+00:00");
        drop(conn);
        let mut conn = connect(&path).unwrap();
        let refused = migrate(&mut conn, "declared", None, "vogt-core", now);
        assert!(refused.unwrap_err().to_string().contains("held by crashed"));
        assert_eq!(holder_of(&conn).as_deref(), Some("crashed"));

        // Exactly fifteen minutes is not younger, so it is stolen.
        hold(&conn, "crashed", "2026-10-08T12:00:00+00:00");
        let stolen = migrate(&mut conn, "declared", None, "vogt-core", now);
        assert!(stolen.is_ok(), "{stolen:?}");
        assert_eq!(holder_of(&conn), None);

        // A fractional timestamp and a Z suffix parse the same instant.
        assert_eq!(
            parse_iso("2026-10-08T12:00:00.500000+00:00"),
            parse_iso("2026-10-08T12:00:00Z")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
