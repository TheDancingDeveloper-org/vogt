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
use crate::core::from_iso;

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

impl From<MigrateError> for crate::errors::VogtError {
    fn from(err: MigrateError) -> Self {
        crate::errors::VogtError::MigrationError(err.to_string())
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
) -> Result<crate::storage::interface::MigrationReport, MigrateError> {
    ensure_framework(conn)?;
    let available = load_migrations(store, directory)?;
    acquire_lock(conn, store, holder, now)?;
    let result: Result<crate::storage::interface::MigrationReport, MigrateError> = (|| {
        verify_forward_only(conn, store, &available)?;
        let applied = applied_ids(conn)?;
        let pending: Vec<&Migration> = available
            .iter()
            .filter(|migration| !applied.contains(&migration.id))
            .collect();
        for migration in &pending {
            apply_one(conn, store, migration, now)?;
        }
        Ok(crate::storage::interface::MigrationReport {
            store: store.to_string(),
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
                    "{store}: migration {id} is applied in the database but absent from this build — the database is ahead of the code. Migrations are forward-only; restore a backup or deploy the newer build."
                )));
            }
            Some(migration) if migration.checksum != checksum => {
                return Err(MigrateError::Message(format!(
                    "{store}: migration {id} was modified after being applied. Migrations are forward-only — add a new migration instead of editing an applied one."
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
        match (from_iso(now).ok(), from_iso(acquired_at).ok()) {
            (Some(now_at), Some(acquired))
                if now_at
                    .unix_seconds()
                    .saturating_sub(acquired.unix_seconds())
                    < STALE_AFTER_SECONDS =>
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
) -> Result<crate::storage::interface::MigrationReport, MigrateError> {
    let mut conn = connect(path)?;
    migrate(&mut conn, store, directory, holder, now)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::sqlite::connection::connect_with;

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

        // A non-UTC offset counts: twelve hours later in +12:00 is the same
        // instant, so the lock is exactly fifteen minutes old and is stolen.
        assert_eq!(
            from_iso("2026-10-08T12:00:00.500000+00:00")
                .unwrap()
                .unix_seconds(),
            from_iso("2026-10-09T00:00:00+12:00")
                .unwrap()
                .unix_seconds()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    const NOW: &str = "2026-08-12T05:00:00+00:00";

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "vogt-mig-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_migration(directory: &Path, name: &str, sql: &str) {
        std::fs::create_dir_all(directory).unwrap();
        std::fs::write(directory.join(name), sql).unwrap();
    }

    #[test]
    fn applies_pending_migrations_in_order() {
        let dir = scratch("order");
        let migrations = dir.join("migrations");
        write_migration(&migrations, "0001_a.sql", "CREATE TABLE a (id TEXT);");
        write_migration(&migrations, "0002_b.sql", "CREATE TABLE b (id TEXT);");
        let mut conn = connect(&dir.join("db.sqlite3")).unwrap();

        let report = migrate(&mut conn, "test", Some(&migrations), "test/1", NOW).unwrap();

        assert_eq!(report.applied, ["0001_a", "0002_b"]);
        assert_eq!(report.version, 2);
        assert_eq!(report.store, "test");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn migrating_twice_applies_nothing() {
        let dir = scratch("twice");
        let migrations = dir.join("migrations");
        write_migration(&migrations, "0001_a.sql", "CREATE TABLE a (id TEXT);");
        let mut conn = connect(&dir.join("db.sqlite3")).unwrap();

        migrate(&mut conn, "test", Some(&migrations), "test/1", NOW).unwrap();
        let second = migrate(&mut conn, "test", Some(&migrations), "test/1", NOW).unwrap();

        assert!(second.applied.is_empty());
        assert_eq!(second.version, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn editing_an_applied_migration_fails_loudly() {
        let dir = scratch("edit");
        let migrations = dir.join("migrations");
        let path = migrations.join("0001_a.sql");
        write_migration(&migrations, "0001_a.sql", "CREATE TABLE a (id TEXT);");
        let mut conn = connect(&dir.join("db.sqlite3")).unwrap();
        migrate(&mut conn, "test", Some(&migrations), "test/1", NOW).unwrap();

        std::fs::write(&path, "CREATE TABLE a (id TEXT, extra TEXT);").unwrap();

        let err = migrate(&mut conn, "test", Some(&migrations), "test/1", NOW)
            .unwrap_err()
            .to_string();
        assert!(err.contains("forward-only"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_database_ahead_of_the_code_fails_loudly() {
        let dir = scratch("ahead");
        let migrations = dir.join("migrations");
        write_migration(&migrations, "0001_a.sql", "CREATE TABLE a (id TEXT);");
        write_migration(&migrations, "0002_b.sql", "CREATE TABLE b (id TEXT);");
        let mut conn = connect(&dir.join("db.sqlite3")).unwrap();
        migrate(&mut conn, "test", Some(&migrations), "test/1", NOW).unwrap();

        std::fs::remove_file(migrations.join("0002_b.sql")).unwrap();

        let err = migrate(&mut conn, "test", Some(&migrations), "test/1", NOW)
            .unwrap_err()
            .to_string();
        assert!(err.contains("ahead of the code"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failing_migration_rolls_back_and_is_not_recorded() {
        let dir = scratch("rollback");
        let migrations = dir.join("migrations");
        write_migration(
            &migrations,
            "0001_a.sql",
            "CREATE TABLE a (id TEXT);\nCREATE TABLE a (x TEXT);",
        );
        let mut conn = connect(&dir.join("db.sqlite3")).unwrap();

        let err = migrate(&mut conn, "test", Some(&migrations), "test/1", NOW)
            .unwrap_err()
            .to_string();
        assert!(err.contains("0001_a failed"), "{err}");

        let tables: Vec<String> = {
            let mut stmt = conn
                .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")
                .unwrap();
            stmt.query_map([], |row| row.get(0))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        };
        assert!(!tables.contains(&"a".to_string()));
        let recorded: i64 = conn
            .query_row("SELECT COUNT(*) FROM migrations", [], |row| row.get(0))
            .unwrap();
        assert_eq!(recorded, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn duplicate_migration_numbers_are_rejected() {
        let dir = scratch("dup");
        let migrations = dir.join("migrations");
        write_migration(&migrations, "0001_a.sql", "CREATE TABLE a (id TEXT);");
        write_migration(&migrations, "0001_b.sql", "CREATE TABLE b (id TEXT);");

        let err = load_migrations("test", Some(&migrations))
            .unwrap_err()
            .to_string();
        assert!(err.contains("duplicate migration number"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn shipped_migration_ids_are_an_append_only_prefix() {
        // Identities already delivered to instances. New migrations append
        // after this prefix; renaming one of these is unsafe because a
        // deployed database records the full id.
        let declared = [
            "0001_foundation",
            "0002_work",
            "0003_observed_first",
            "0004_drift",
            "0005_tokens",
            "0006_writeback",
            "0007_sessions",
            "0008_superseded_drift",
            "0009_inbox_triage",
            "0010_session_model",
        ];
        let observed = [
            "0001_foundation",
            "0002_evidence",
            "0003_inherited_dep_refs",
        ];
        for (store, shipped) in [("declared", &declared[..]), ("observed", &observed[..])] {
            let available = load_migrations(store, None).unwrap();
            for (position, shipped_id) in shipped.iter().enumerate() {
                let actual = available
                    .get(position)
                    .map(|migration| migration.id.as_str())
                    .unwrap_or("<missing>");
                assert_eq!(actual, *shipped_id, "{store} position {}", position + 1);
            }
        }
    }

    #[test]
    fn split_statements_ignores_comments_and_strings() {
        let script = "
        -- a comment with a ; semicolon
        INSERT INTO t (v) VALUES ('a;b');
        CREATE TABLE u (id TEXT)
        ";
        assert_eq!(
            split_statements(script),
            [
                "INSERT INTO t (v) VALUES ('a;b')",
                "CREATE TABLE u (id TEXT)",
            ]
        );
    }

    #[test]
    fn shipped_migrations_bring_both_stores_up() {
        let dir = scratch("shipped");
        for store in ["declared", "observed"] {
            let mut conn = connect(&dir.join(format!("{store}.sqlite3"))).unwrap();
            assert_eq!(applied_version(&conn).unwrap(), 0);
            let report = migrate(&mut conn, store, None, "test/1", NOW).unwrap();
            assert_eq!(
                report.applied.first().map(String::as_str),
                Some("0001_foundation")
            );
            let mut sorted = report.applied.clone();
            sorted.sort();
            assert_eq!(report.applied, sorted);
            assert_eq!(applied_version(&conn).unwrap(), report.applied.len() as i64);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_old_database_opens_and_migrates_forward() {
        // A database written by an earlier build (only 0001) upgrades through
        // the embedded SQL and keeps the row it already held.
        let dir = scratch("upgrade");
        let old = dir.join("old");
        let shipped = load_migrations("declared", None).unwrap();
        let first = shipped
            .iter()
            .find(|migration| migration.number() == 1)
            .unwrap();
        write_migration(&old, &format!("{}.sql", first.id), &first.sql);

        let path = dir.join("declared.sqlite3");
        let mut conn = connect(&path).unwrap();
        migrate(&mut conn, "declared", Some(&old), "old/1", NOW).unwrap();
        conn.execute(
            "INSERT INTO meta (key, value) VALUES ('instance_id', 'inst_old')",
            [],
        )
        .unwrap();
        drop(conn);

        let mut conn = connect_with(&path, false, "off").unwrap();
        let report = migrate(&mut conn, "declared", None, "new/2", NOW).unwrap();
        assert!(
            report.applied.contains(&"0002_work".to_string()),
            "{:?}",
            report.applied
        );
        let kept: String = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'instance_id'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(kept, "inst_old");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_database_is_not_created_and_a_bad_synchronous_is_refused() {
        let dir = scratch("connect");
        let missing = dir.join("absent.sqlite3");
        let err = connect_with(&missing, false, "normal")
            .unwrap_err()
            .to_string();
        assert!(err.contains("no database"), "{err}");
        assert!(!missing.exists());

        let present = dir.join("present.sqlite3");
        connect(&present).unwrap();
        let err = connect_with(&present, false, "sometimes")
            .unwrap_err()
            .to_string();
        assert!(err.contains("unknown synchronous setting"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
