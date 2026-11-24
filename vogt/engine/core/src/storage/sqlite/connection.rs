//! Connection pragmas. Ports `src/vogt/storage/sqlite/connection.py`.
//!
//! `BUSY_TIMEOUT_MS` is 5000 and `DEFAULT_SYNCHRONOUS` is `normal`. Journal
//! mode is WAL and foreign keys are on. The Python driver runs in autocommit
//! (`isolation_level=None`); rusqlite does the same unless a transaction is
//! opened explicitly.

use std::path::{Path, PathBuf};

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

// ---------------------------------------------------------------------------
// Long-lived connections (WI-4631).
//
// One pool per (database file, synchronous mode) for the whole process. It
// holds one writer connection that one caller at a time may lease, and a few
// read-only connections. Pragmas run once per connection, when it is opened.
// Stores are cheap to clone and are built per request, so the pool lives in a
// process registry, not in a store.
// ---------------------------------------------------------------------------

use std::collections::HashMap;
use std::ops::{Deref, DerefMut};
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak};
use std::thread::ThreadId;

/// How many idle read-only connections a pool keeps. A reader beyond this opens
/// a connection that closes on release, so a burst never waits.
pub const READER_POOL_SIZE: usize = 4;

/// Identity of the file behind the path. A restore or a test that replaces the
/// file must not keep serving the old inode.
fn file_identity(path: &Path) -> Option<(u64, u64)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(path).ok().map(|m| (m.dev(), m.ino()))
    }
    #[cfg(not(unix))]
    {
        std::fs::metadata(path).ok().map(|_| (0, 0))
    }
}

#[derive(Default)]
struct PoolState {
    ident: Option<(u64, u64)>,
    generation: u64,
    writer: Option<Connection>,
    writer_busy: bool,
    writer_owner: Option<ThreadId>,
    readers: Vec<Connection>,
}

pub struct ConnPool {
    path: PathBuf,
    synchronous: String,
    state: Mutex<PoolState>,
    writer_free: Condvar,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LeaseKind {
    Writer,
    Reader,
    /// Not returned to the pool: an extra connection, or one a caller marked
    /// with [`Lease::discard`].
    Loose,
}

/// A connection on loan from the pool. It derefs to `Connection`. On drop an
/// open transaction is rolled back and the connection goes back.
pub struct Lease {
    conn: Option<Connection>,
    pool: Arc<ConnPool>,
    kind: LeaseKind,
    generation: u64,
}

impl Lease {
    /// Close this connection on release instead of pooling it. For a caller
    /// that changed connection state (an `ATTACH`, a scalar function, `query_only`).
    pub fn discard(&mut self) {
        self.kind = LeaseKind::Loose;
    }
}

impl Deref for Lease {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        self.conn.as_ref().expect("a lease holds its connection")
    }
}

impl DerefMut for Lease {
    fn deref_mut(&mut self) -> &mut Connection {
        self.conn.as_mut().expect("a lease holds its connection")
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        let Some(conn) = self.conn.take() else { return };
        let mut clean = true;
        if !conn.is_autocommit() {
            let _ = conn.execute_batch("ROLLBACK");
            clean = conn.is_autocommit();
        }
        let mut state = self.pool.state.lock().unwrap_or_else(|e| e.into_inner());
        let current = state.generation == self.generation;
        match self.kind {
            LeaseKind::Writer => {
                if clean && current {
                    state.writer = Some(conn);
                }
                state.writer_busy = false;
                state.writer_owner = None;
                self.pool.writer_free.notify_one();
            }
            LeaseKind::Reader => {
                if clean && current && state.readers.len() < READER_POOL_SIZE {
                    state.readers.push(conn);
                }
            }
            LeaseKind::Loose => {}
        }
    }
}

impl ConnPool {
    /// The process pool for this file and synchronous mode.
    pub fn for_path(path: &Path, synchronous: &str) -> Arc<ConnPool> {
        let registry = pool_registry();
        let mut map = registry.lock().unwrap_or_else(|e| e.into_inner());
        let key = (path.to_path_buf(), synchronous.to_ascii_lowercase());
        if let Some(pool) = map.get(&key).and_then(Weak::upgrade) {
            return pool;
        }
        map.retain(|_, weak| weak.strong_count() > 0);
        let pool = Arc::new(ConnPool {
            path: path.to_path_buf(),
            synchronous: synchronous.to_string(),
            state: Mutex::new(PoolState::default()),
            writer_free: Condvar::new(),
        });
        map.insert(key, Arc::downgrade(&pool));
        pool
    }

    /// Close every idle connection now and make leased ones close on release.
    /// A replaced database must not meet a connection that still holds the old
    /// one: closing the last connection to a WAL database deletes its `-wal` and
    /// `-shm` by name, and by then those names belong to the new file.
    fn close_all(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.generation += 1;
        state.writer = None;
        state.readers.clear();
    }

    /// Drop pooled connections when the path now names a different file.
    fn refresh(&self, state: &mut PoolState) {
        let ident = file_identity(&self.path);
        if ident != state.ident {
            state.ident = ident;
            state.generation += 1;
            state.writer = None;
            state.readers.clear();
        }
    }

    /// The single writer. Callers queue on it, so two writers never meet
    /// `SQLITE_BUSY`. A caller that already holds the writer on this thread
    /// gets a loose connection rather than a deadlock.
    pub fn writer(self: &Arc<Self>, create: bool) -> rusqlite::Result<Lease> {
        let me = std::thread::current().id();
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            self.refresh(&mut state);
            if !state.writer_busy {
                break;
            }
            if state.writer_owner == Some(me) {
                drop(state);
                let conn = connect_with(&self.path, create, &self.synchronous)?;
                return Ok(Lease {
                    conn: Some(conn),
                    pool: Arc::clone(self),
                    kind: LeaseKind::Loose,
                    generation: 0,
                });
            }
            state = self
                .writer_free
                .wait(state)
                .unwrap_or_else(|e| e.into_inner());
        }
        state.writer_busy = true;
        state.writer_owner = Some(me);
        let cached = state.writer.take();
        let generation = state.generation;
        drop(state);
        let conn = match cached {
            Some(conn) => conn,
            None => match connect_with(&self.path, create, &self.synchronous) {
                Ok(conn) => conn,
                Err(err) => {
                    let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    state.writer_busy = false;
                    state.writer_owner = None;
                    self.writer_free.notify_one();
                    return Err(err);
                }
            },
        };
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.ident.is_none() {
            state.ident = file_identity(&self.path);
        }
        drop(state);
        Ok(Lease {
            conn: Some(conn),
            pool: Arc::clone(self),
            kind: LeaseKind::Writer,
            generation,
        })
    }

    /// A read-only connection. It never creates the file and never takes the
    /// write lock; a missing file is an error.
    pub fn reader(self: &Arc<Self>) -> rusqlite::Result<Lease> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        self.refresh(&mut state);
        let generation = state.generation;
        let cached = state.readers.pop();
        drop(state);
        let conn = match cached {
            Some(conn) => conn,
            None => {
                let conn = Connection::open_with_flags(
                    &self.path,
                    rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
                        | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
                )?;
                // Opened read-write so WAL recovery and the -shm file work after a
                // restore (a SQLITE_OPEN_READONLY handle hit an I/O error there),
                // then made read-only: no statement on it can write.
                conn.pragma_update(None, "query_only", true)?;
                conn.pragma_update(None, "foreign_keys", "ON")?;
                conn.pragma_update(None, "busy_timeout", BUSY_TIMEOUT_MS)?;
                conn
            }
        };
        Ok(Lease {
            conn: Some(conn),
            pool: Arc::clone(self),
            kind: LeaseKind::Reader,
            generation,
        })
    }
}

/// Close the pooled connections of every database file in `data_dir`. Call it
/// before the files are replaced (restore, clone).
pub fn close_pooled(data_dir: &Path) {
    let pools: Vec<Arc<ConnPool>> = {
        let registry = pool_registry().lock().unwrap_or_else(|e| e.into_inner());
        registry
            .iter()
            .filter(|((path, _), _)| path.parent() == Some(data_dir))
            .filter_map(|(_, weak)| weak.upgrade())
            .collect()
    };
    for pool in pools {
        pool.close_all();
    }
}

/// Fold the write-ahead log of every pooled database back into its file and
/// truncate it (WI-4638). Each pass takes the pool's single writer in turn, so
/// it queues behind a running write instead of racing it; readers are never
/// blocked (SQLite waits only for readers still on old frames, bounded by the
/// busy timeout). Returns the files it checkpointed and the ones that failed.
pub fn checkpoint_all() -> (usize, Vec<(PathBuf, String)>) {
    let pools: Vec<Arc<ConnPool>> = {
        let registry = pool_registry().lock().unwrap_or_else(|e| e.into_inner());
        registry.values().filter_map(Weak::upgrade).collect()
    };
    let mut done = 0;
    let mut failed = Vec::new();
    for pool in pools {
        if !pool.path.exists() {
            continue;
        }
        let result = pool
            .writer(false)
            .and_then(|conn| conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(())));
        match result {
            Ok(()) => done += 1,
            Err(err) => failed.push((pool.path.clone(), err.to_string())),
        }
    }
    (done, failed)
}

type Registry = Mutex<HashMap<(PathBuf, String), Weak<ConnPool>>>;

fn pool_registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

#[cfg(test)]
mod checkpoint_tests {
    use super::*;

    #[test]
    fn checkpoint_all_truncates_the_wal_and_keeps_the_data() {
        let dir = std::env::temp_dir().join(format!("vogt-ckpt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("ckpt.sqlite3");
        let pool = ConnPool::for_path(&path, "normal");
        {
            let conn = pool.writer(true).unwrap();
            conn.execute_batch("CREATE TABLE t (v TEXT); INSERT INTO t VALUES ('a'), ('b');")
                .unwrap();
        }
        let wal = dir.join("ckpt.sqlite3-wal");
        assert!(
            std::fs::metadata(&wal).unwrap().len() > 0,
            "writes sit in the WAL"
        );
        let (done, failed) = checkpoint_all();
        assert!(done >= 1);
        assert!(failed.is_empty(), "{failed:?}");
        assert_eq!(std::fs::metadata(&wal).map(|m| m.len()).unwrap_or(0), 0);
        let reader = pool.reader().unwrap();
        let n: i64 = reader
            .query_row("SELECT count(*) FROM t", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 2);
    }
}
