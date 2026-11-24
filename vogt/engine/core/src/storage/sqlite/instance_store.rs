//! Instance bootstrap writes against the declared and observed stores. These
//! are the raw statements `application::instance` used to run inline; they keep
//! the same SQL, transactions and error types (WI-4633).

use std::path::Path;

use rusqlite::{params, OptionalExtension};

use super::connection::connect;
use super::migrator::MigrateError;

const WORKFLOW_DEFINITION: &str = concat!(
    r#"{"initial_state": "open", "transitions": {"blocked": ["open", "in_progress", "wont_do"], "#,
    r#""done": ["open"], "in_progress": ["review", "blocked", "open", "wont_do"], "#,
    r#""open": ["in_progress", "blocked", "wont_do"], "#,
    r#""review": ["done", "in_progress", "blocked", "wont_do"], "wont_do": ["open"]}}"#,
);
const WORK_KINDS: [&str; 4] = ["feature", "bug", "chore", "question"];

/// Whether a query finds a row. Only a confirmed "no rows" is `false`; any
/// other failure propagates so a caller never treats a broken lookup as absent.
pub(super) fn row_exists(
    conn: &rusqlite::Connection,
    sql: &str,
    params: impl rusqlite::Params,
) -> rusqlite::Result<bool> {
    use rusqlite::OptionalExtension;

    conn.query_row(sql, params, |_| Ok(()))
        .optional()
        .map(|row| row.is_some())
}

/// Insert the machine for each kind the lookup confirms absent. A failed lookup
/// is an error and inserts nothing further.
pub(super) fn seed_kinds(
    conn: &rusqlite::Connection,
    at: &str,
    exists: impl Fn(&rusqlite::Connection, &str) -> rusqlite::Result<bool>,
) -> rusqlite::Result<()> {
    for kind in WORK_KINDS {
        if exists(conn, kind)? {
            continue;
        }
        conn.execute(
            "INSERT INTO workflow_defs (kind, definition, updated_at) VALUES (?1, ?2, ?3)",
            rusqlite::params![kind, WORKFLOW_DEFINITION, at],
        )?;
    }
    Ok(())
}

/// Seed the workflow machine for each kind. An existing kind is left untouched.
pub fn seed_workflows(path: &Path, at: &str) -> Result<(), MigrateError> {
    let conn = connect(path)?;
    conn.execute("BEGIN IMMEDIATE", [])?;
    let written = seed_kinds(&conn, at, |conn, kind| {
        row_exists(
            conn,
            "SELECT 1 FROM workflow_defs WHERE kind = ?1",
            params![kind],
        )
    });
    match written {
        Ok(()) => conn.execute("COMMIT", []).map(|_| ()).map_err(Into::into),
        Err(err) => {
            let _ = conn.execute("ROLLBACK", []);
            Err(err.into())
        }
    }
}

/// The stored instance id, if any.
pub fn instance_id_opt(path: &Path) -> Result<Option<String>, MigrateError> {
    let conn = connect(path)?;
    Ok(conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'instance_id'",
            [],
            |row| row.get(0),
        )
        .optional()?)
}

/// The instance id a previous init wrote. Empty when the database carries none.
pub fn instance_id_of(path: &Path) -> Result<String, MigrateError> {
    Ok(instance_id_opt(path)?.unwrap_or_default())
}

const EMPTY_DIGEST: &str =
    "sha256:0000000000000000000000000000000000000000000000000000000000000000";

pub struct NewInstance {
    pub user: String,
    pub instance_id: String,
    pub actor_id: String,
    pub audit_id: String,
    pub txn_id: String,
    pub at: String,
    pub operation: &'static str,
    pub reason: &'static str,
}

/// Write the meta rows, the initiating actor and one audit row in one transaction.
pub fn write_instance(path: &Path, n: &NewInstance) -> Result<(), MigrateError> {
    let conn = connect(path)?;
    conn.execute("BEGIN IMMEDIATE", [])?;
    let written = (|| -> rusqlite::Result<()> {
        for (key, value) in [
            ("instance_id", n.instance_id.as_str()),
            ("revision", "0"),
            ("work_ref_seq", "0"),
            ("created_at", n.at.as_str()),
        ] {
            conn.execute(
                "INSERT INTO meta (key, value) VALUES (?1, ?2)",
                params![key, value],
            )?;
        }
        conn.execute(
            "INSERT INTO actors (id, kind, display_name, identity_ref, disabled, created_at)
             VALUES (?1, 'human', ?2, ?3, 0, ?4)",
            params![n.actor_id, &n.user, format!("local:{}", n.user), &n.at],
        )?;
        conn.execute(
            "INSERT INTO audit (id, txn_id, revision, actor_id, operation, entity_kind,
                                entity_id, reason, payload_digest, at)
             VALUES (?1, ?2, 0, ?3, ?4, 'instance', ?5, ?6, ?7, ?8)",
            params![
                n.audit_id,
                n.txn_id,
                n.actor_id,
                n.operation,
                n.instance_id,
                n.reason,
                EMPTY_DIGEST,
                n.at
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
    Ok(())
}

/// Stamp the observed store with its instance id; `stamp` is called only when
/// the key is absent. A failed lookup is an error, not "absent".
pub fn bind_instance(
    path: &Path,
    instance_id: &str,
    stamp: impl FnOnce() -> String,
) -> Result<(), MigrateError> {
    let conn = connect(path)?;
    if row_exists(&conn, "SELECT 1 FROM meta WHERE key = 'instance_id'", [])? {
        return Ok(());
    }
    let at = stamp();
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

#[cfg(test)]
mod seed_tests {
    use super::{row_exists, seed_kinds, WORK_KINDS};

    fn store() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().expect("open");
        conn.execute(
            "CREATE TABLE workflow_defs (kind TEXT PRIMARY KEY, definition TEXT, updated_at TEXT)",
            [],
        )
        .expect("create");
        conn
    }

    fn count(conn: &rusqlite::Connection) -> i64 {
        conn.query_row("SELECT COUNT(*) FROM workflow_defs", [], |r| r.get(0))
            .expect("count")
    }

    #[test]
    fn failed_lookup_is_an_error_and_inserts_nothing() {
        let conn = store();
        let result = seed_kinds(&conn, "t", |_, _| {
            Err(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
                None,
            ))
        });
        assert!(result.is_err());
        assert_eq!(count(&conn), 0);
    }

    #[test]
    fn confirmed_absent_inserts_and_present_is_kept() {
        let conn = store();
        let lookup = |c: &rusqlite::Connection, k: &str| {
            row_exists(
                c,
                "SELECT 1 FROM workflow_defs WHERE kind = ?1",
                rusqlite::params![k],
            )
        };
        seed_kinds(&conn, "t", lookup).expect("seed");
        assert_eq!(count(&conn), WORK_KINDS.len() as i64);
        seed_kinds(&conn, "t", lookup).expect("reseed");
        assert_eq!(count(&conn), WORK_KINDS.len() as i64);
    }

    #[test]
    fn row_exists_propagates_a_broken_query() {
        let conn = store();
        assert!(row_exists(&conn, "SELECT 1 FROM missing_table", []).is_err());
    }
}
