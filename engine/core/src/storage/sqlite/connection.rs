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
