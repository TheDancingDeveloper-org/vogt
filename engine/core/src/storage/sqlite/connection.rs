//! Connection pragmas. Ports `src/vogt/storage/sqlite/connection.py`.
//!
//! `BUSY_TIMEOUT_MS` is 5000 and `DEFAULT_SYNCHRONOUS` is `normal`. Journal
//! mode is WAL and foreign keys are on. The Python driver runs in autocommit
//! (`isolation_level=None`); rusqlite does the same unless a transaction is
//! opened explicitly.

use std::path::Path;

use rusqlite::Connection;

pub const BUSY_TIMEOUT_MS: u32 = 5_000;
pub const DEFAULT_SYNCHRONOUS: &str = "normal";

const SYNCHRONOUS_SETTINGS: &[&str] = &["off", "normal", "full", "extra"];

/// Open a connection with Vogt's pragmas applied.
///
/// `create` false and a missing file is `FileNotFoundError` in Python; here it
/// is `Err`. `synchronous` is checked against a fixed set because a PRAGMA
/// takes no bound parameters.
pub fn connect(path: &Path) -> rusqlite::Result<Connection> {
    connect_with(path, true, DEFAULT_SYNCHRONOUS)
}

pub fn connect_with(path: &Path, create: bool, synchronous: &str) -> rusqlite::Result<Connection> {
    if !SYNCHRONOUS_SETTINGS.contains(&synchronous.to_ascii_lowercase().as_str()) {
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error {
                code: rusqlite::ffi::ErrorCode::Unknown,
                extended_code: 0,
            },
            Some(format!("unknown synchronous setting: {synchronous:?}")),
        ));
    }
    if !create && !path.exists() {
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error {
                code: rusqlite::ffi::ErrorCode::CannotOpen,
                extended_code: 0,
            },
            Some(format!("no database at {}", path.display())),
        ));
    }
    if create {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|err| {
                rusqlite::Error::SqliteFailure(
                    rusqlite::ffi::Error {
                        code: rusqlite::ffi::ErrorCode::CannotOpen,
                        extended_code: 0,
                    },
                    Some(err.to_string()),
                )
            })?;
        }
    }
    let conn = Connection::open(path)?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "busy_timeout", BUSY_TIMEOUT_MS)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", synchronous.to_ascii_uppercase())?;
    Ok(conn)
}

/// Ports `split_statements`. Migrations are plain DDL: no triggers and no
/// semicolons inside identifiers. A `--` comment runs to the next newline.
pub fn split_statements(script: &str) -> Vec<String> {
    let mut statements = Vec::new();
    let mut buffer = String::new();
    let mut in_string = false;
    let bytes = script.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        let char = bytes[index] as char;
        if in_string {
            buffer.push(char);
            if char == '\'' {
                in_string = false;
            }
            index += 1;
        } else if char == '\'' {
            in_string = true;
            buffer.push(char);
            index += 1;
        } else if char == '-' && bytes.get(index + 1) == Some(&b'-') {
            index = script[index..]
                .find('\n')
                .map(|end| index + end)
                .unwrap_or(bytes.len());
        } else if char == ';' {
            let statement = buffer.trim().to_string();
            if !statement.is_empty() {
                statements.push(statement);
            }
            buffer.clear();
            index += 1;
        } else {
            buffer.push(char);
            index += 1;
        }
    }
    let tail = buffer.trim().to_string();
    if !tail.is_empty() {
        statements.push(tail);
    }
    statements
}

/// A read-only connection that never takes the write lock or creates the file.
/// Any failure (missing file, locked database) is an `Err` the caller may treat
/// as "no data".
pub fn open_read_only(path: &Path) -> rusqlite::Result<Connection> {
    Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
}

/// Open a backup file by `file:` URI, read-only. The caller builds the URI
/// (`mode=ro&immutable=1`) so that checking a backup never writes -wal/-shm.
pub fn open_read_only_uri(uri: &str) -> rusqlite::Result<Connection> {
    Connection::open_with_flags(
        uri,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
}

/// Newest session report per engine session: `(kind, summary, data,
/// engine_session_id)`. A missing table or a locked database is no rows.
pub fn latest_report_rows(
    path: &Path,
    wanted: &[&str],
) -> Vec<(String, String, String, Option<String>)> {
    let Ok(conn) = open_read_only(path) else {
        return Vec::new();
    };
    let placeholders = std::iter::repeat_n("?", wanted.len())
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT kind, summary, data, engine_session_id FROM (
            SELECT r.kind, r.summary, r.data, r.engine_session_id,
                   ROW_NUMBER() OVER (
                       PARTITION BY engine_session_id
                       ORDER BY created_at DESC, id DESC
                   ) AS rn
            FROM session_reports r
            WHERE engine_session_id IN ({placeholders})
        ) WHERE rn = 1"
    );
    let Ok(mut stmt) = conn.prepare(&sql) else {
        return Vec::new();
    };
    let rows = stmt.query_map(rusqlite::params_from_iter(wanted.iter()), |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Option<String>>(3)?,
        ))
    });
    let Ok(rows) = rows else {
        return Vec::new();
    };
    rows.flatten().collect()
}
