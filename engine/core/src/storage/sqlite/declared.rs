//! Declared SQLite store. Ports `src/vogt/storage/sqlite/declared.py`.
//!
//! SQL stays next to the Python so ordering and the `ON CONFLICT` sites match.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rusqlite::{params, Connection, OptionalExtension, Row};

use crate::core::{
    default_workflow, from_iso, to_iso, Actor, ActorKind, ActorPreference, AuditRecord,
    AuthDecision, Clock, CodingSession, Comment, ContractExemption, DriftProposal, Event,
    ForgeAccount, IdFactory, InboxTriage, Initiative, InitiativeState, Label, Moment,
    PasswordCredential, Principal, Project, RelationKind, SessionGrant, Suppression, Token,
    WorkItem, WorkLink, WorkOverlay, Workflow, WriteBackAction, WriteBackOutcome, WriteBackRecord,
};
use crate::errors::VogtError;
use crate::storage::interface::{
    AuditQuery, Blocker, BoardCellQuery, BootstrapResult, CarriedCredentials, CarryReport,
    CloneStamp, Counts, DeclaredStore, MigrationReport, ProjectUpdate, ReadView, WorkFilter,
    WorkItemUpdate, WriteTxn,
};
use crate::storage::sqlite::connection::connect_with;
use crate::storage::sqlite::migrator::{self, migrations_root};

const META_INSTANCE_ID: &str = "instance_id";
const META_REVISION: &str = "revision";
const META_CREATED_AT: &str = "created_at";
const META_WORK_REF_SEQ: &str = "work_ref_seq";
const META_CLONED_FROM: &str = "cloned_from_instance_id";
const META_CLONED_AT: &str = "cloned_at";
const META_CLONED_BACKUP_TAKEN_AT: &str = "cloned_backup_taken_at";
const INIT_OPERATION: &str = "instance.init";
const INIT_REASON: &str = "instance bootstrap";
const WORK_REF_PREFIX: &str = "WI-";

fn sql_err(err: rusqlite::Error) -> VogtError {
    // A constraint, lock or I/O failure is the store breaking, not the caller
    // sending a bad request. Python surfaces sqlite3.Error as a 500.
    VogtError::MigrationError(err.to_string())
}

/// A meta counter that cannot be read as an integer is corrupt, not zero.
/// Defaulting it would silently restart the work-ref or revision sequence.
fn meta_counter(conn: &Connection, key: &str) -> Result<i64, VogtError> {
    match meta_get(conn, key)? {
        Some(text) => text.parse::<i64>().map_err(|_| {
            VogtError::MigrationError(format!("meta {key} is not an integer: {text}"))
        }),
        None => Err(VogtError::MigrationError(format!("meta {key} is missing"))),
    }
}

pub struct SqliteDeclaredStore<C, I> {
    path: PathBuf,
    /// Shared with the context and the observed store. Python's `build_context`
    /// hands one clock and one id factory to both stores, and each draw or tick
    /// must be visible to the others — a clone rewrites `test-ids.json` from its
    /// own counts and loses theirs.
    clock: Arc<std::sync::Mutex<C>>,
    ids: Arc<std::sync::Mutex<I>>,
    synchronous: String,
}

impl<C, I> SqliteDeclaredStore<C, I>
where
    C: Clock,
    I: IdFactory,
{
    pub fn mint(&self, prefix: &str) -> (Moment, String) {
        let now = self
            .clock
            .lock()
            .expect("the clock lock is not poisoned")
            .now();
        let id = self
            .ids
            .lock()
            .expect("the id lock is not poisoned")
            .next(prefix);
        (now, id)
    }

    pub fn new(path: PathBuf, clock: C, ids: I) -> Self {
        Self::shared(
            path,
            Arc::new(std::sync::Mutex::new(clock)),
            Arc::new(std::sync::Mutex::new(ids)),
            crate::storage::sqlite::connection::DEFAULT_SYNCHRONOUS,
        )
    }

    /// A store over the same clock and id factory as this one.
    pub fn joined(&self, path: PathBuf) -> Self {
        Self::shared(
            path,
            Arc::clone(&self.clock),
            Arc::clone(&self.ids),
            &self.synchronous,
        )
    }

    /// A store over a clock and an id factory something else also holds.
    pub fn shared(
        path: PathBuf,
        clock: Arc<std::sync::Mutex<C>>,
        ids: Arc<std::sync::Mutex<I>>,
        synchronous: &str,
    ) -> Self {
        Self {
            path,
            clock,
            ids,
            synchronous: synchronous.to_string(),
        }
    }

    /// The clock this store ticks. The context holds the same one.
    pub fn clock(&self) -> &Arc<std::sync::Mutex<C>> {
        &self.clock
    }

    /// The database file. A request builds its own store over a restarted clock
    /// and needs the same file.
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// The synchronous mode this store was opened with, so a request's store
    /// writes the way the process store does.
    pub fn synchronous(&self) -> &str {
        &self.synchronous
    }

    /// The id factory this store counts with. The context holds the same one,
    /// because a second factory starts again at one and the two collide.
    // Used by the write tests, which live outside this crate's binary target.
    #[allow(dead_code)]
    pub fn id_factory(&self) -> &Arc<std::sync::Mutex<I>> {
        &self.ids
    }

    fn open(&self, create: bool) -> Result<Connection, VogtError> {
        connect_with(&self.path, create, &self.synchronous).map_err(sql_err)
    }

    /// Open a connection that belongs to an initialised instance, or the
    /// exact refusal Python's store raises when it does not.
    fn open_initialized(&self) -> Result<Connection, VogtError> {
        if !self.path.exists() {
            return Err(VogtError::NotInitialized(self.not_initialized_message()));
        }
        let conn = self
            .open(false)
            .map_err(|_| VogtError::NotInitialized(self.not_initialized_message()))?;
        match meta_get(&conn, META_INSTANCE_ID) {
            Ok(Some(_)) => Ok(conn),
            _ => Err(VogtError::NotInitialized(self.not_initialized_message())),
        }
    }

    fn not_initialized_message(&self) -> String {
        let parent = self.path.parent().unwrap_or(Path::new("."));
        format!(
            "no Vogt instance in {} — run `vogt init` first",
            parent.display()
        )
    }
}

impl<C, I> DeclaredStore for SqliteDeclaredStore<C, I>
where
    C: Clock,
    I: IdFactory,
{
    fn stamp_and_id(&self, prefix: &str) -> (Moment, String) {
        self.mint(prefix)
    }
    type Read<'a>
        = SqliteReadView
    where
        Self: 'a;
    type Write<'a>
        = SqliteWrite<I>
    where
        Self: 'a;

    fn migrate(&self) -> Result<MigrationReport, VogtError> {
        // One clock tick, taken before anything is written: Python's migrate
        // calls the clock once and stamps both the migration rows and the
        // seeded workflows with it. A wall clock here puts every later
        // bootstrap timestamp one second early under a stepped test clock.
        let now = self
            .clock
            .lock()
            .expect("the shared clock and ids are not poisoned")
            .now();
        let mut conn = self.open(true)?;
        let holder = format!(
            "{}/{}",
            std::env::var("HOSTNAME").unwrap_or_else(|_| "localhost".into()),
            std::process::id()
        );
        let report = migrator::migrate(
            &mut conn,
            "declared",
            migrations_root().as_deref(),
            &holder,
            &to_iso(now),
        )
        .map_err(VogtError::from)?;
        ensure_default_workflows(&conn, now)?;
        Ok(report)
    }

    fn is_initialized(&self) -> bool {
        self.open(false)
            .ok()
            .and_then(|conn| meta_get(&conn, META_INSTANCE_ID).ok().flatten())
            .is_some()
    }

    fn schema_version(&self) -> i64 {
        self.open(false)
            .ok()
            .and_then(|conn| migrator::applied_version(&conn).ok())
            .unwrap_or(0)
    }

    fn bundled_schema_version(&self) -> i64 {
        migrator::bundled_version("declared", migrations_root().as_deref()).unwrap_or(0)
    }

    fn bootstrap(&self, principal: &Principal) -> Result<BootstrapResult, VogtError> {
        let conn = self.open(true)?;
        conn.execute_batch("BEGIN IMMEDIATE").map_err(sql_err)?;
        let result = (|| -> Result<BootstrapResult, VogtError> {
            if meta_get(&conn, META_INSTANCE_ID)?.is_some() {
                let parent = self.path.parent().unwrap_or(Path::new("."));
                return Err(VogtError::AlreadyInitialized(format!(
                    "an instance already exists at {}",
                    parent.display()
                )));
            }
            // Allocated after the check: a refused re-init must not burn ids,
            // and the clock tick belongs to the instance that was created.
            let now = self
                .clock
                .lock()
                .expect("the shared clock and ids are not poisoned")
                .now();
            let instance_id = self
                .ids
                .lock()
                .expect("the shared clock and ids are not poisoned")
                .next("ins");
            let actor_id = self
                .ids
                .lock()
                .expect("the shared clock and ids are not poisoned")
                .next("act");
            let audit_id = self
                .ids
                .lock()
                .expect("the shared clock and ids are not poisoned")
                .next("aud");
            let txn_id = self
                .ids
                .lock()
                .expect("the shared clock and ids are not poisoned")
                .next("txn");
            meta_set(&conn, META_INSTANCE_ID, &instance_id)?;
            meta_set(&conn, META_REVISION, "0")?;
            meta_set(&conn, META_WORK_REF_SEQ, "0")?;
            meta_set(&conn, META_CREATED_AT, &to_iso(now))?;
            let actor = Actor {
                id: actor_id,
                kind: principal.kind,
                display_name: principal.display_name.clone(),
                identity_ref: principal.identity_ref.clone(),
                disabled: false,
                created_at: now,
            };
            insert_actor(&conn, &actor)?;
            insert_audit(
                &conn,
                &AuditRecord {
                    id: audit_id,
                    txn_id,
                    revision: 0,
                    actor_id: actor.id.clone(),
                    actor_identity_ref: actor.identity_ref.clone(),
                    operation: INIT_OPERATION.to_string(),
                    entity_kind: "instance".to_string(),
                    entity_id: instance_id.clone(),
                    reason: INIT_REASON.to_string(),
                    payload_digest: format!("sha256:{}", "0".repeat(64)),
                    at: now,
                },
            )?;
            Ok(BootstrapResult { instance_id, actor })
        })();
        if result.is_ok() {
            conn.execute_batch("COMMIT").map_err(sql_err)?;
        } else {
            let _ = conn.execute_batch("ROLLBACK");
        }
        result
    }

    fn credentials(&self) -> Result<CarriedCredentials, VogtError> {
        let conn = self.open_initialized()?;
        conn.execute("BEGIN", []).map_err(sql_err)?;
        let outcome = (|| {
            let tokens = select_columns(&conn, "tokens", TOKEN_CARRY_COLUMNS)?;
            let passwords = select_columns(&conn, "password_credentials", PASSWORD_CARRY_COLUMNS)?;
            let forge = select_columns(&conn, "forge_accounts", FORGE_ACCOUNT_CARRY_COLUMNS)?;
            let mut actor_ids: BTreeSet<String> = BTreeSet::new();
            for row in tokens.iter().chain(passwords.iter()).chain(forge.iter()) {
                if let Some(actor_id) = row.get("actor_id").and_then(|v| v.as_str()) {
                    actor_ids.insert(actor_id.to_string());
                }
            }
            let mut actors = Vec::new();
            for actor_id in actor_ids {
                if let Some(actor) = one(
                    &conn,
                    "SELECT * FROM actors WHERE id = ?",
                    [&actor_id],
                    row_actor,
                )? {
                    actors.push(actor);
                }
            }
            Ok(CarriedCredentials {
                actors,
                tokens,
                password_credentials: passwords,
                forge_accounts: forge,
            })
        })();
        let _ = conn.execute("ROLLBACK", []);
        outcome
    }
    fn record_auth_decision(&self, decision: &AuthDecision) -> Result<(), VogtError> {
        // Not a declared write: nothing changed and nobody supplied a reason,
        // and it happens on reads too. An audit row would make "every audit
        // row is a change" false.
        let conn = self.open_initialized()?;
        conn.execute("BEGIN IMMEDIATE", []).map_err(sql_err)?;
        let outcome = conn.execute(
            "INSERT INTO auth_decisions (id, at, decision, reason_code, operation, scope, actor_id, token_id, identity_ref, transport, detail) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                decision.id, to_iso(decision.at), vocab_text(decision.decision), decision.reason_code,
                decision.operation, decision.scope, decision.actor_id, decision.token_id,
                decision.identity_ref, decision.transport, decision.detail,
            ],
        );
        finish_immediate(&conn, outcome)
    }
    fn touch_token(
        &self,
        token_id: &str,
        at: Moment,
        expires_at: Option<Moment>,
    ) -> Result<(), VogtError> {
        let conn = self.open_initialized()?;
        conn.execute("BEGIN IMMEDIATE", []).map_err(sql_err)?;
        let outcome = match expires_at {
            None => conn.execute(
                "UPDATE tokens SET last_used_at = ? WHERE id = ?",
                params![to_iso(at), token_id],
            ),
            Some(expires) => conn.execute(
                "UPDATE tokens SET last_used_at = ?, expires_at = CASE WHEN revoked_at IS NULL AND expires_at IS NOT NULL AND expires_at > ? AND expires_at < ? THEN ? ELSE expires_at END WHERE id = ?",
                params![to_iso(at), to_iso(at), to_iso(expires), to_iso(expires), token_id],
            ),
        };
        finish_immediate(&conn, outcome)
    }
    fn prune_auth_decisions(
        &self,
        allow_before: Moment,
        deny_before: Moment,
    ) -> Result<i64, VogtError> {
        let conn = self.open_initialized()?;
        conn.execute("BEGIN IMMEDIATE", []).map_err(sql_err)?;
        let outcome = conn.execute(
            "DELETE FROM auth_decisions WHERE (decision = 'allow' AND at < ?) OR (decision = 'deny' AND at < ?)",
            params![to_iso(allow_before), to_iso(deny_before)],
        );
        match outcome {
            Ok(removed) => {
                conn.execute("COMMIT", []).map_err(sql_err)?;
                Ok(removed as i64)
            }
            Err(err) => {
                let _ = conn.execute("ROLLBACK", []);
                Err(sql_err(err))
            }
        }
    }
    fn publish_event(
        &self,
        kind: &str,
        entity_kind: &str,
        entity_id: &str,
        summary: &serde_json::Value,
        at: Moment,
    ) -> Result<Event, VogtError> {
        // Not a declared write: no audit row and the revision stays put,
        // which is what lets a collector keep its promise never to write
        // the declared store while the application publishes for it. The
        // transaction id is still drawn, though: Python builds a
        // SqliteWriteTxn for the append, and under a sequential id factory
        // every later audit's txn id counts the publishes that came first.
        let conn = self.open_initialized()?;
        let _txn_id = self
            .ids
            .lock()
            .expect("the shared clock and ids are not poisoned")
            .next("txn");
        conn.execute("BEGIN IMMEDIATE", []).map_err(sql_err)?;
        let outcome = (|| -> Result<Event, rusqlite::Error> {
            let rendered = crate::decisions::python_json_dumps(summary, false);
            conn.execute(
                "INSERT INTO events (kind, entity_kind, entity_id, actor_id, audit_id, summary, at) VALUES (?, ?, ?, NULL, NULL, ?, ?)",
                params![kind, entity_kind, entity_id, rendered, to_iso(at)],
            )?;
            Ok(Event {
                seq: conn.last_insert_rowid(),
                kind: kind.to_string(),
                entity_kind: entity_kind.to_string(),
                entity_id: entity_id.to_string(),
                actor_id: None,
                audit_id: None,
                summary: summary.clone(),
                at,
            })
        })();
        match outcome {
            Ok(event) => {
                conn.execute("COMMIT", []).map_err(sql_err)?;
                Ok(event)
            }
            Err(err) => {
                let _ = conn.execute("ROLLBACK", []);
                Err(sql_err(err))
            }
        }
    }

    fn read(&self) -> Result<Self::Read<'_>, VogtError> {
        let conn = self.open_initialized()?;
        // BEGIN, not autocommit: a count and the page beside it must see one
        // revision, and SQLite only promises that inside a transaction.
        conn.execute_batch("BEGIN").map_err(sql_err)?;
        Ok(SqliteReadView {
            conn,
            workflow_cache: std::cell::RefCell::new(BTreeMap::new()),
        })
    }

    fn write(&self) -> Result<Self::Write<'_>, VogtError> {
        let conn = self.open_initialized()?;
        conn.execute_batch("BEGIN IMMEDIATE").map_err(sql_err)?;
        let revision = meta_counter(&conn, META_REVISION)? + 1;
        meta_set(&conn, META_REVISION, &revision.to_string())?;
        // The id comes after the instance check, so a refused write burns none.
        let txn_id = self
            .ids
            .lock()
            .expect("the shared clock and ids are not poisoned")
            .next("txn");
        Ok(SqliteWrite {
            view: SqliteReadView {
                conn,
                workflow_cache: std::cell::RefCell::new(BTreeMap::new()),
            },
            ids: Arc::clone(&self.ids),
            txn_id,
            revision,
            open: true,
        })
    }
}

pub struct SqliteReadView {
    conn: Connection,
    workflow_cache: std::cell::RefCell<BTreeMap<String, Workflow>>,
}
pub struct SqliteWrite<I: IdFactory> {
    view: SqliteReadView,
    ids: Arc<std::sync::Mutex<I>>,
    txn_id: String,
    revision: i64,
    open: bool,
}

impl<I: IdFactory> SqliteWrite<I> {
    pub fn commit(mut self) -> Result<(), VogtError> {
        self.view.conn.execute_batch("COMMIT").map_err(sql_err)?;
        self.open = false;
        Ok(())
    }
}
impl<I: IdFactory> Drop for SqliteWrite<I> {
    fn drop(&mut self) {
        if self.open {
            let _ = self.view.conn.execute_batch("ROLLBACK");
        }
    }
}

impl ReadView for SqliteReadView {
    fn instance_id(&self) -> Result<String, VogtError> {
        meta_get(&self.conn, META_INSTANCE_ID)?
            .ok_or_else(|| VogtError::NotInitialized("instance id missing".into()))
    }
    fn clone_stamp(&self) -> Result<Option<CloneStamp>, VogtError> {
        let (Some(source), Some(cloned_at), Some(taken_at)) = (
            meta_get(&self.conn, META_CLONED_FROM)?,
            meta_get(&self.conn, META_CLONED_AT)?,
            meta_get(&self.conn, META_CLONED_BACKUP_TAKEN_AT)?,
        ) else {
            return Ok(None);
        };
        Ok(Some(CloneStamp {
            source_instance_id: source,
            cloned_at: from_iso(&cloned_at).map_err(VogtError::InvalidRequest)?,
            backup_taken_at: from_iso(&taken_at).map_err(VogtError::InvalidRequest)?,
        }))
    }
    fn current_revision(&self) -> Result<i64, VogtError> {
        meta_counter(&self.conn, META_REVISION)
    }
    fn latest_event_seq(&self) -> Result<i64, VogtError> {
        self.conn
            .query_row("SELECT COALESCE(MAX(seq), 0) FROM events", [], |row| {
                row.get(0)
            })
            .map_err(sql_err)
    }
    fn counts(&self) -> Result<Counts, VogtError> {
        Ok(Counts {
            projects: count(&self.conn, "projects")?,
            actors: count(&self.conn, "actors")?,
            events: count(&self.conn, "events")?,
            audit: count(&self.conn, "audit")?,
            work_items: count(&self.conn, "work_items")?,
            initiatives: count(&self.conn, "initiatives")?,
        })
    }
    fn actor_by_identity(&self, identity_ref: &str) -> Result<Option<Actor>, VogtError> {
        one(
            &self.conn,
            "SELECT * FROM actors WHERE identity_ref = ?",
            [identity_ref],
            row_actor,
        )
    }
    fn actor_by_id(&self, actor_id: &str) -> Result<Option<Actor>, VogtError> {
        one(
            &self.conn,
            "SELECT * FROM actors WHERE id = ?",
            [actor_id],
            row_actor,
        )
    }
    fn list_actors(&self, limit: i64, offset: i64) -> Result<Vec<Actor>, VogtError> {
        many(
            &self.conn,
            "SELECT * FROM actors ORDER BY identity_ref LIMIT ? OFFSET ?",
            params![limit, offset],
            row_actor,
        )
    }
    fn project_by_slug(&self, slug: &str) -> Result<Option<Project>, VogtError> {
        one(
            &self.conn,
            "SELECT * FROM projects WHERE slug = ?",
            [slug],
            row_project,
        )
    }
    fn project_by_id(&self, project_id: &str) -> Result<Option<Project>, VogtError> {
        one(
            &self.conn,
            "SELECT * FROM projects WHERE id = ?",
            [project_id],
            row_project,
        )
    }
    fn list_projects(&self, limit: i64, offset: i64) -> Result<Vec<Project>, VogtError> {
        many(
            &self.conn,
            "SELECT * FROM projects ORDER BY slug LIMIT ? OFFSET ?",
            params![limit, offset],
            row_project,
        )
    }
    fn work_item_by_id(&self, id: &str) -> Result<Option<WorkItem>, VogtError> {
        load_work_item(&self.conn, "w.id = ?", id)
    }
    fn work_item_by_ref(&self, reference: &str) -> Result<Option<WorkItem>, VogtError> {
        load_work_item(&self.conn, "w.ref = ?", reference)
    }
    fn list_work_items(&self, filter: &WorkFilter) -> Result<Vec<WorkItem>, VogtError> {
        let (where_sql, params) = work_where(filter);
        let mut sql_params = params;
        sql_params.push(Box::new(filter.limit));
        sql_params.push(Box::new(filter.offset));
        let refs: Vec<&dyn rusqlite::ToSql> = sql_params.iter().map(|v| v.as_ref()).collect();
        let bases = many(
            &self.conn,
            &format!("{WORK_SELECT} {where_sql} ORDER BY w.created_at, w.ref LIMIT ? OFFSET ?"),
            refs.as_slice(),
            row_work_base,
        )?;
        bases
            .into_iter()
            .map(|base| finish_work_item(&self.conn, base))
            .collect()
    }
    fn count_work_items(&self, filter: &WorkFilter) -> Result<i64, VogtError> {
        let (where_sql, params) = work_where(filter);
        let refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|v| v.as_ref()).collect();
        self.conn
            .query_row(
                &format!("SELECT COUNT(*) FROM work_items w {where_sql}"),
                refs.as_slice(),
                |row| row.get(0),
            )
            .map_err(sql_err)
    }
    fn board_high_water(&self, filter: &WorkFilter) -> Result<Option<(Moment, String)>, VogtError> {
        let (where_sql, params) = work_where(filter);
        let refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|v| v.as_ref()).collect();
        self.conn
            .query_row(
                &format!(
                    "SELECT w.created_at, w.ref FROM work_items w LEFT JOIN projects p ON p.id = w.project_id {where_sql} ORDER BY w.created_at DESC, w.ref DESC LIMIT 1"
                ),
                refs.as_slice(),
                |row| Ok((moment(row, "created_at")?, row.get("ref")?)),
            )
            .optional()
            .map_err(sql_err)
    }
    fn board_counts(
        &self,
        filter: &WorkFilter,
        lane_mode: &str,
        high_water: Option<&(Moment, String)>,
    ) -> Result<BTreeMap<(String, String), i64>, VogtError> {
        let Some(high_water) = high_water else {
            return Ok(BTreeMap::new());
        };
        let (where_sql, params) = with_board_high_water(filter, high_water)?;
        let lane = board_lane(lane_mode)?;
        let refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|v| v.as_ref()).collect();
        let rows = many(
            &self.conn,
            &format!(
                "SELECT {lane} AS board_lane, w.state AS state, COUNT(*) AS n FROM work_items w LEFT JOIN projects p ON p.id = w.project_id {where_sql} GROUP BY {lane}, w.state"
            ),
            refs.as_slice(),
            |row| {
                Ok((
                    row.get::<_, String>("board_lane")?,
                    row.get::<_, String>("state")?,
                    row.get::<_, i64>("n")?,
                ))
            },
        )?;
        Ok(rows
            .into_iter()
            .map(|(lane, state, n)| ((lane, state), n))
            .collect())
    }
    fn board_work_items(
        &self,
        filter: &WorkFilter,
        lane_mode: &str,
        cells: &[BoardCellQuery],
        high_water: Option<&(Moment, String)>,
        limit: i64,
    ) -> Result<BTreeMap<(String, String), Vec<WorkItem>>, VogtError> {
        let mut result: BTreeMap<(String, String), Vec<WorkItem>> = cells
            .iter()
            .map(|cell| ((cell.lane_key.clone(), cell.state.clone()), Vec::new()))
            .collect();
        let Some(high_water) = high_water else {
            return Ok(result);
        };
        if cells.is_empty() {
            return Ok(result);
        }
        let (mut where_sql, mut params) = with_board_high_water(filter, high_water)?;
        let lane = board_lane(lane_mode)?;
        let mut requested: Vec<String> = Vec::new();
        for cell in cells {
            let mut clause = vec![format!("{lane} = ?"), "w.state = ?".to_string()];
            params.push(Box::new(cell.lane_key.clone()));
            params.push(Box::new(cell.state.clone()));
            if let (Some(after), Some(after_ref)) = (&cell.after_created_at, &cell.after_ref) {
                clause.push("(w.created_at > ? OR (w.created_at = ? AND w.ref > ?))".to_string());
                let moment = to_iso(*after);
                params.push(Box::new(moment.clone()));
                params.push(Box::new(moment));
                params.push(Box::new(after_ref.clone()));
            }
            requested.push(format!("({})", clause.join(" AND ")));
        }
        where_sql = append_where(&where_sql, &format!("({})", requested.join(" OR ")));
        params.push(Box::new(limit));
        let refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|v| v.as_ref()).collect();
        let rows = many(
            &self.conn,
            &format!(
                "WITH requested AS (SELECT w.*, p.slug AS project_slug, ac.identity_ref AS assignee_identity_ref, {lane} AS board_lane, ROW_NUMBER() OVER (PARTITION BY {lane}, w.state ORDER BY w.created_at, w.ref) AS board_row FROM work_items w LEFT JOIN projects p ON p.id = w.project_id LEFT JOIN actors ac ON ac.id = w.assignee_actor_id {where_sql}) SELECT * FROM requested WHERE board_row <= ? ORDER BY board_lane, state, created_at, ref"
            ),
            refs.as_slice(),
            |row| Ok((row.get::<_, String>("board_lane")?, row_work_base(row)?)),
        )?;
        for (lane_key, base) in rows {
            let state = base.item.state.clone();
            let item = finish_work_item(&self.conn, base)?;
            result.entry((lane_key, state)).or_default().push(item);
        }
        Ok(result)
    }
    fn blocking_fan_out(
        &self,
        work_item_ids: &[String],
    ) -> Result<BTreeMap<String, i64>, VogtError> {
        if work_item_ids.is_empty() {
            return Ok(BTreeMap::new());
        }
        let placeholders = vec!["?"; work_item_ids.len()].join(", ");
        let params: Vec<Box<dyn rusqlite::ToSql>> = work_item_ids
            .iter()
            .map(|id| Box::new(id.clone()) as Box<dyn rusqlite::ToSql>)
            .collect();
        let refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|v| v.as_ref()).collect();
        let rows = many(
            &self.conn,
            &format!(
                "SELECT related_id, COUNT(*) AS n FROM work_relations WHERE kind = 'depends_on' AND related_id IN ({placeholders}) GROUP BY related_id"
            ),
            refs.as_slice(),
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
        )?;
        Ok(rows.into_iter().collect())
    }
    fn unfinished_blockers(
        &self,
        work_item_id: &str,
        terminal_states: &[&str],
    ) -> Result<Vec<Blocker>, VogtError> {
        let placeholders = if terminal_states.is_empty() {
            "''".to_string()
        } else {
            vec!["?"; terminal_states.len()].join(", ")
        };
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(work_item_id.to_string())];
        for state in terminal_states {
            params.push(Box::new((*state).to_string()));
        }
        let refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|v| v.as_ref()).collect();
        many(
            &self.conn,
            &format!(
                "SELECT t.ref AS ref, t.state AS state FROM work_relations r JOIN work_items t ON t.id = r.related_id WHERE r.work_item_id = ? AND r.kind = 'depends_on' AND t.state NOT IN ({placeholders}) ORDER BY t.ref"
            ),
            refs.as_slice(),
            |row| {
                Ok(Blocker {
                    r#ref: row.get("ref")?,
                    state: row.get("state")?,
                })
            },
        )
    }
    fn comments_for(&self, id: &str, limit: i64) -> Result<Vec<Comment>, VogtError> {
        many(&self.conn, "SELECT c.*, a.display_name AS actor_display_name FROM comments c JOIN actors a ON a.id = c.actor_id WHERE c.work_item_id = ? ORDER BY c.created_at, c.id LIMIT ?", params![id, limit], row_comment)
    }
    fn label_by_name(&self, name: &str) -> Result<Option<Label>, VogtError> {
        one(
            &self.conn,
            "SELECT * FROM labels WHERE name = ?",
            [name],
            row_label,
        )
    }
    fn list_labels(&self, limit: i64, offset: i64) -> Result<Vec<Label>, VogtError> {
        many(
            &self.conn,
            "SELECT * FROM labels ORDER BY name LIMIT ? OFFSET ?",
            params![limit, offset],
            row_label,
        )
    }
    fn initiative_by_id(&self, id: &str) -> Result<Option<Initiative>, VogtError> {
        one(
            &self.conn,
            "SELECT * FROM initiatives WHERE id = ?",
            [id],
            row_initiative,
        )
    }
    fn initiative_by_slug(&self, slug: &str) -> Result<Option<Initiative>, VogtError> {
        one(
            &self.conn,
            "SELECT * FROM initiatives WHERE slug = ?",
            [slug],
            row_initiative,
        )
    }
    fn list_initiatives(&self, limit: i64, offset: i64) -> Result<Vec<Initiative>, VogtError> {
        many(
            &self.conn,
            "SELECT * FROM initiatives ORDER BY slug LIMIT ? OFFSET ?",
            params![limit, offset],
            row_initiative,
        )
    }
    fn workflow_for(&self, kind: &str) -> Result<Workflow, VogtError> {
        if let Some(cached) = self.workflow_cache.borrow().get(kind) {
            return Ok(cached.clone());
        }
        let stored = self
            .conn
            .query_row(
                "SELECT definition FROM workflow_defs WHERE kind = ?",
                [kind],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(sql_err)?;
        let workflow = match stored {
            Some(text) => {
                Workflow::from_definition_json(kind, &text).map_err(VogtError::InvalidRequest)?
            }
            None => default_workflow(kind),
        };
        self.workflow_cache
            .borrow_mut()
            .insert(kind.to_string(), workflow.clone());
        Ok(workflow)
    }
    fn list_suppressions(
        &self,
        include_revoked: bool,
        limit: i64,
    ) -> Result<Vec<Suppression>, VogtError> {
        let clause = if include_revoked {
            ""
        } else {
            "WHERE s.revoked_at IS NULL"
        };
        many(
            &self.conn,
            &format!("{SUPPRESSION_SELECT} {clause} ORDER BY s.created_at DESC, s.id DESC LIMIT ?"),
            params![limit],
            row_suppression,
        )
    }
    fn suppression_by_id(&self, suppression_id: &str) -> Result<Option<Suppression>, VogtError> {
        one(
            &self.conn,
            &format!("{SUPPRESSION_SELECT} WHERE s.id = ?"),
            [suppression_id],
            row_suppression,
        )
    }
    fn contract_exemptions(&self, project_id: &str) -> Result<Vec<ContractExemption>, VogtError> {
        many(&self.conn, "SELECT e.*, p.slug AS project_slug FROM contract_exemptions e JOIN projects p ON p.id = e.project_id WHERE e.project_id = ? ORDER BY e.rule, e.target", params![project_id], row_exemption)
    }
    fn work_links_for_subjects(
        &self,
        subject_keys: &[String],
    ) -> Result<BTreeMap<String, String>, VogtError> {
        let mut found = BTreeMap::new();
        for slice in subject_keys.chunks(500) {
            let placeholders = vec!["?"; slice.len()].join(", ");
            let rows = many(
                &self.conn,
                &format!(
                    "SELECT l.subject_key AS subject_key, w.ref AS ref FROM work_links l JOIN work_items w ON w.id = l.work_item_id WHERE l.subject_key IN ({placeholders})"
                ),
                rusqlite::params_from_iter(slice),
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )?;
            found.extend(rows);
        }
        Ok(found)
    }
    fn work_links_for_subjects_by_item(
        &self,
        work_item_id: &str,
    ) -> Result<BTreeMap<String, String>, VogtError> {
        let rows = many(
            &self.conn,
            "SELECT subject_key, origin_kind FROM work_links WHERE work_item_id = ?",
            [work_item_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )?;
        Ok(rows.into_iter().collect())
    }
    fn work_item_by_subject(&self, subject_key: &str) -> Result<Option<WorkItem>, VogtError> {
        let row = one(
            &self.conn,
            &format!("{WORK_SELECT} JOIN work_links l ON l.work_item_id = w.id WHERE l.subject_key = ? LIMIT 1"),
            [subject_key],
            row_work_base,
        )?;
        row.map(|base| finish_work_item(&self.conn, base))
            .transpose()
    }
    fn work_overlay(&self, subject_key: &str) -> Result<Option<WorkOverlay>, VogtError> {
        one(
            &self.conn,
            "SELECT * FROM work_overlay WHERE subject_key = ?",
            [subject_key],
            row_overlay,
        )
    }
    fn work_overlays(
        &self,
        subject_keys: &[String],
    ) -> Result<BTreeMap<String, WorkOverlay>, VogtError> {
        let mut found = BTreeMap::new();
        for slice in subject_keys.chunks(500) {
            let placeholders = vec!["?"; slice.len()].join(", ");
            let rows = many(
                &self.conn,
                &format!("SELECT * FROM work_overlay WHERE subject_key IN ({placeholders})"),
                rusqlite::params_from_iter(slice),
                row_overlay,
            )?;
            found.extend(
                rows.into_iter()
                    .map(|overlay| (overlay.subject_key.clone(), overlay)),
            );
        }
        Ok(found)
    }
    fn bound_branch_overlays(&self, limit: i64) -> Result<Vec<WorkOverlay>, VogtError> {
        many(
            &self.conn,
            "SELECT * FROM work_overlay WHERE branches != '[]' ORDER BY updated_at DESC, subject_key LIMIT ?",
            [limit],
            row_overlay,
        )
    }
    fn token_by_hash(&self, token_hash: &str) -> Result<Option<Token>, VogtError> {
        one(
            &self.conn,
            &format!("{TOKEN_SELECT} WHERE t.token_hash = ?"),
            [token_hash],
            row_token,
        )
    }
    fn token_by_id(&self, token_id: &str) -> Result<Option<Token>, VogtError> {
        one(
            &self.conn,
            &format!("{TOKEN_SELECT} WHERE t.id = ?"),
            [token_id],
            row_token,
        )
    }
    fn list_tokens(&self, include_revoked: bool, limit: i64) -> Result<Vec<Token>, VogtError> {
        let clause = if include_revoked {
            ""
        } else {
            "WHERE t.revoked_at IS NULL"
        };
        many(
            &self.conn,
            &format!("{TOKEN_SELECT} {clause} ORDER BY t.created_at DESC, t.id DESC LIMIT ?"),
            params![limit],
            row_token,
        )
    }
    fn tokens_for_actor(
        &self,
        actor_id: &str,
        include_revoked: bool,
    ) -> Result<Vec<Token>, VogtError> {
        let clause = if include_revoked {
            ""
        } else {
            "AND t.revoked_at IS NULL"
        };
        many(
            &self.conn,
            &format!("{TOKEN_SELECT} WHERE t.actor_id = ? {clause} ORDER BY t.created_at DESC, t.id DESC"),
            [actor_id],
            row_token,
        )
    }
    fn install_closed(&self) -> Result<bool, VogtError> {
        self.conn
            .query_row(
                "SELECT EXISTS (SELECT 1 FROM install_latch) OR EXISTS (SELECT 1 FROM tokens t JOIN actors a ON a.id = t.actor_id WHERE a.kind <> 'agent') OR EXISTS (SELECT 1 FROM password_credentials)",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map(|found| found != 0)
            .map_err(sql_err)
    }
    fn list_auth_decisions(
        &self,
        decision: Option<&str>,
        limit: i64,
    ) -> Result<Vec<AuthDecision>, VogtError> {
        // An empty string is no filter, matching Python's `if decision`.
        match decision.filter(|value| !value.is_empty()) {
            Some(decision) => many(
                &self.conn,
                "SELECT * FROM auth_decisions WHERE decision = ? ORDER BY at DESC, id DESC LIMIT ?",
                params![decision, limit],
                row_auth_decision,
            ),
            None => many(
                &self.conn,
                "SELECT * FROM auth_decisions ORDER BY at DESC, id DESC LIMIT ?",
                params![limit],
                row_auth_decision,
            ),
        }
    }
    fn password_credential_by_username(
        &self,
        username: &str,
    ) -> Result<Option<PasswordCredential>, VogtError> {
        one(
            &self.conn,
            &format!("{PASSWORD_SELECT} WHERE p.username = ?"),
            [username],
            row_password,
        )
    }
    fn password_credential_for_actor(
        &self,
        actor_id: &str,
    ) -> Result<Option<PasswordCredential>, VogtError> {
        one(
            &self.conn,
            &format!("{PASSWORD_SELECT} WHERE p.actor_id = ?"),
            [actor_id],
            row_password,
        )
    }
    fn password_hash(&self, actor_id: &str) -> Result<Option<String>, VogtError> {
        self.conn
            .query_row(
                "SELECT password_hash FROM password_credentials WHERE actor_id = ?",
                [actor_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(sql_err)
    }
    fn list_password_credentials(&self) -> Result<Vec<PasswordCredential>, VogtError> {
        many(
            &self.conn,
            &format!("{PASSWORD_SELECT} ORDER BY p.username"),
            [],
            row_password,
        )
    }
    fn forge_account(&self, actor_id: &str, host: &str) -> Result<Option<ForgeAccount>, VogtError> {
        one(
            &self.conn,
            "SELECT actor_id, host, login, scopes, created_at, updated_at FROM forge_accounts WHERE actor_id = ? AND host = ?",
            params![actor_id, host],
            row_forge_account,
        )
    }
    fn forge_accounts_for_actor(&self, actor_id: &str) -> Result<Vec<ForgeAccount>, VogtError> {
        many(
            &self.conn,
            "SELECT actor_id, host, login, scopes, created_at, updated_at FROM forge_accounts WHERE actor_id = ? ORDER BY created_at DESC, host",
            [actor_id],
            row_forge_account,
        )
    }
    fn forge_account_secret(
        &self,
        actor_id: &str,
        host: &str,
    ) -> Result<Option<String>, VogtError> {
        self.conn
            .query_row(
                "SELECT encrypted_token FROM forge_accounts WHERE actor_id = ? AND host = ?",
                params![actor_id, host],
                |row| row.get(0),
            )
            .optional()
            .map_err(sql_err)
    }
    fn list_drift(
        &self,
        status: Option<&str>,
        kind: Option<&str>,
        project_id: Option<&str>,
        limit: i64,
    ) -> Result<Vec<DriftProposal>, VogtError> {
        let mut clauses: Vec<String> = Vec::new();
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        for (column, value) in [
            ("d.status", status),
            ("d.kind", kind),
            ("d.project_id", project_id),
        ] {
            if let Some(value) = value {
                clauses.push(format!("{column} = ?"));
                params.push(Box::new(value.to_string()));
            }
        }
        let where_sql = if clauses.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", clauses.join(" AND "))
        };
        params.push(Box::new(limit));
        let refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|v| v.as_ref()).collect();
        many(
            &self.conn,
            &format!("{DRIFT_SELECT} {where_sql} ORDER BY d.opened_at DESC, d.id DESC LIMIT ?"),
            refs.as_slice(),
            row_drift,
        )
    }
    fn drift_by_id(&self, proposal_id: &str) -> Result<Option<DriftProposal>, VogtError> {
        one(
            &self.conn,
            &format!("{DRIFT_SELECT} WHERE d.id = ?"),
            [proposal_id],
            row_drift,
        )
    }
    fn open_drift_subjects(&self) -> Result<BTreeSet<(String, String, String)>, VogtError> {
        let rows = many(
            &self.conn,
            "SELECT kind, subject_kind, subject_id FROM drift_proposals WHERE status = 'open'",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )?;
        Ok(rows.into_iter().collect())
    }
    fn list_writeback_actions(
        &self,
        outcome: Option<&str>,
        limit: i64,
    ) -> Result<Vec<WriteBackRecord>, VogtError> {
        // Python's `if outcome` treats "" as absent, so an empty filter
        // returns every row rather than matching the empty outcome.
        match outcome.filter(|value| !value.is_empty()) {
            Some(value) => many(
                &self.conn,
                "SELECT * FROM writeback_actions WHERE outcome = ? ORDER BY at DESC, id DESC LIMIT ?",
                params![value, limit],
                row_writeback,
            ),
            None => many(
                &self.conn,
                "SELECT * FROM writeback_actions ORDER BY at DESC, id DESC LIMIT ?",
                [limit],
                row_writeback,
            ),
        }
    }
    fn drift_evidence_ids(&self) -> Result<BTreeSet<String>, VogtError> {
        let rows = many(
            &self.conn,
            "SELECT DISTINCT evidence_observation_id FROM drift_proposals WHERE evidence_observation_id IS NOT NULL",
            [],
            |row| row.get::<_, String>(0),
        )?;
        Ok(rows.into_iter().collect())
    }
    fn inbox_triage_by_key(&self, entry_key: &str) -> Result<Option<InboxTriage>, VogtError> {
        one(
            &self.conn,
            &format!("{INBOX_SELECT} WHERE t.entry_key = ?"),
            [entry_key],
            row_inbox_triage,
        )
    }
    fn inbox_triage_by_keys(
        &self,
        entry_keys: &[String],
    ) -> Result<BTreeMap<String, InboxTriage>, VogtError> {
        // SQLite caps bound parameters per statement, so a large page goes in
        // slices rather than one statement that fails only once it matters.
        let mut found = BTreeMap::new();
        for slice in entry_keys.chunks(500) {
            let placeholders = vec!["?"; slice.len()].join(", ");
            let params: Vec<Box<dyn rusqlite::ToSql>> = slice
                .iter()
                .map(|key| Box::new(key.clone()) as Box<dyn rusqlite::ToSql>)
                .collect();
            let refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|v| v.as_ref()).collect();
            for triage in many(
                &self.conn,
                &format!("{INBOX_SELECT} WHERE t.entry_key IN ({placeholders})"),
                refs.as_slice(),
                row_inbox_triage,
            )? {
                found.insert(triage.entry_key.clone(), triage);
            }
        }
        Ok(found)
    }
    fn list_inbox_triage(&self, limit: i64) -> Result<Vec<InboxTriage>, VogtError> {
        many(
            &self.conn,
            &format!("{INBOX_SELECT} ORDER BY t.decided_at DESC, t.entry_key DESC LIMIT ?"),
            params![limit],
            row_inbox_triage,
        )
    }
    fn actor_preference(
        &self,
        actor_id: &str,
        key: &str,
    ) -> Result<Option<ActorPreference>, VogtError> {
        one(
            &self.conn,
            "SELECT actor_id, key, value, version, updated_at FROM actor_preferences WHERE actor_id = ? AND key = ?",
            params![actor_id, key],
            row_actor_preference,
        )
    }
    fn actor_preferences(&self, actor_id: &str) -> Result<Vec<ActorPreference>, VogtError> {
        many(
            &self.conn,
            "SELECT actor_id, key, value, version, updated_at FROM actor_preferences WHERE actor_id = ? ORDER BY key",
            [actor_id],
            row_actor_preference,
        )
    }
    fn session_by_id(&self, session_id: &str) -> Result<Option<CodingSession>, VogtError> {
        one(
            &self.conn,
            "SELECT * FROM coding_sessions WHERE id = ?",
            [session_id],
            row_session,
        )
    }
    fn session_grant(&self, grant_id: &str) -> Result<Option<SessionGrant>, VogtError> {
        one(
            &self.conn,
            "SELECT * FROM session_grants WHERE id = ?",
            [grant_id],
            row_session_grant,
        )
    }
    fn session_by_engine_id(
        &self,
        engine_session_id: &str,
    ) -> Result<Option<CodingSession>, VogtError> {
        one(
            &self.conn,
            "SELECT * FROM coding_sessions WHERE engine_session_id = ?",
            [engine_session_id],
            row_session,
        )
    }
    fn list_session_grants(
        &self,
        state: Option<&str>,
        target_engine_session_id: Option<&str>,
        limit: i64,
    ) -> Result<Vec<SessionGrant>, VogtError> {
        let mut clauses: Vec<String> = Vec::new();
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        if let Some(state) = state {
            clauses.push("state = ?".into());
            params.push(Box::new(state.to_string()));
        }
        if let Some(target) = target_engine_session_id {
            clauses.push("target_engine_session_id = ?".into());
            params.push(Box::new(target.to_string()));
        }
        let where_sql = if clauses.is_empty() {
            String::new()
        } else {
            format!("WHERE {} ", clauses.join(" AND "))
        };
        params.push(Box::new(limit));
        let refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|v| v.as_ref()).collect();
        many(
            &self.conn,
            &format!("SELECT * FROM session_grants {where_sql}ORDER BY requested_at DESC, id DESC LIMIT ?"),
            refs.as_slice(),
            row_session_grant,
        )
    }
    fn list_sessions(
        &self,
        project_id: Option<&str>,
        work_item_id: Option<&str>,
        include_stopped: bool,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<CodingSession>, VogtError> {
        let mut clauses: Vec<String> = Vec::new();
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        for (column, value) in [("project_id", project_id), ("work_item_id", work_item_id)] {
            if let Some(value) = value {
                clauses.push(format!("{column} = ?"));
                params.push(Box::new(value.to_string()));
            }
        }
        if !include_stopped {
            clauses.push("stopped_at IS NULL".into());
        }
        let where_sql = if clauses.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", clauses.join(" AND "))
        };
        params.push(Box::new(limit));
        params.push(Box::new(offset));
        let refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|v| v.as_ref()).collect();
        many(
            &self.conn,
            &format!("SELECT * FROM coding_sessions {where_sql} ORDER BY started_at DESC, id DESC LIMIT ? OFFSET ?"),
            refs.as_slice(),
            row_session,
        )
    }
    fn list_events(
        &self,
        after: i64,
        limit: i64,
        entity_id: Option<&str>,
    ) -> Result<Vec<Event>, VogtError> {
        match entity_id {
            Some(entity) => many(
                &self.conn,
                "SELECT * FROM events WHERE seq > ? AND entity_id = ? ORDER BY seq LIMIT ?",
                params![after, entity, limit],
                row_event,
            ),
            None => many(
                &self.conn,
                "SELECT * FROM events WHERE seq > ? ORDER BY seq LIMIT ?",
                params![after, limit],
                row_event,
            ),
        }
    }
    fn list_audit(&self, query: &AuditQuery) -> Result<Vec<AuditRecord>, VogtError> {
        let (where_sql, mut params_box) = audit_where(query);
        params_box.push(Box::new(query.limit));
        params_box.push(Box::new(query.offset));
        let refs: Vec<&dyn rusqlite::ToSql> = params_box.iter().map(|v| v.as_ref()).collect();
        many(&self.conn, &format!("SELECT a.*, ac.identity_ref AS actor_identity_ref FROM audit a JOIN actors ac ON ac.id = a.actor_id {where_sql} ORDER BY a.revision DESC, a.at DESC, a.id DESC LIMIT ? OFFSET ?"), refs.as_slice(), row_audit)
    }
    fn count_audit(&self, query: &AuditQuery) -> Result<i64, VogtError> {
        let (where_sql, params_box) = audit_where(query);
        let refs: Vec<&dyn rusqlite::ToSql> = params_box.iter().map(|v| v.as_ref()).collect();
        self.conn
            .query_row(
                &format!("SELECT COUNT(*) FROM audit a {where_sql}"),
                refs.as_slice(),
                |row| row.get(0),
            )
            .map_err(sql_err)
    }
}

impl<I: IdFactory> ReadView for SqliteWrite<I> {
    fn instance_id(&self) -> Result<String, VogtError> {
        self.view.instance_id()
    }
    fn clone_stamp(&self) -> Result<Option<CloneStamp>, VogtError> {
        self.view.clone_stamp()
    }
    fn current_revision(&self) -> Result<i64, VogtError> {
        self.view.current_revision()
    }
    fn latest_event_seq(&self) -> Result<i64, VogtError> {
        self.view.latest_event_seq()
    }
    fn counts(&self) -> Result<Counts, VogtError> {
        self.view.counts()
    }
    fn actor_by_identity(&self, a: &str) -> Result<Option<Actor>, VogtError> {
        self.view.actor_by_identity(a)
    }
    fn actor_by_id(&self, a: &str) -> Result<Option<Actor>, VogtError> {
        self.view.actor_by_id(a)
    }
    fn list_actors(&self, l: i64, o: i64) -> Result<Vec<Actor>, VogtError> {
        self.view.list_actors(l, o)
    }
    fn project_by_slug(&self, a: &str) -> Result<Option<Project>, VogtError> {
        self.view.project_by_slug(a)
    }
    fn project_by_id(&self, a: &str) -> Result<Option<Project>, VogtError> {
        self.view.project_by_id(a)
    }
    fn list_projects(&self, l: i64, o: i64) -> Result<Vec<Project>, VogtError> {
        self.view.list_projects(l, o)
    }
    fn work_item_by_id(&self, a: &str) -> Result<Option<WorkItem>, VogtError> {
        self.view.work_item_by_id(a)
    }
    fn work_item_by_ref(&self, a: &str) -> Result<Option<WorkItem>, VogtError> {
        self.view.work_item_by_ref(a)
    }
    fn list_work_items(&self, a: &WorkFilter) -> Result<Vec<WorkItem>, VogtError> {
        self.view.list_work_items(a)
    }
    fn count_work_items(&self, a: &WorkFilter) -> Result<i64, VogtError> {
        self.view.count_work_items(a)
    }
    fn board_high_water(&self, a: &WorkFilter) -> Result<Option<(Moment, String)>, VogtError> {
        self.view.board_high_water(a)
    }
    fn board_counts(
        &self,
        a: &WorkFilter,
        b: &str,
        c: Option<&(Moment, String)>,
    ) -> Result<BTreeMap<(String, String), i64>, VogtError> {
        self.view.board_counts(a, b, c)
    }
    fn board_work_items(
        &self,
        a: &WorkFilter,
        b: &str,
        c: &[BoardCellQuery],
        d: Option<&(Moment, String)>,
        e: i64,
    ) -> Result<BTreeMap<(String, String), Vec<WorkItem>>, VogtError> {
        self.view.board_work_items(a, b, c, d, e)
    }
    fn blocking_fan_out(&self, a: &[String]) -> Result<BTreeMap<String, i64>, VogtError> {
        self.view.blocking_fan_out(a)
    }
    fn unfinished_blockers(&self, a: &str, b: &[&str]) -> Result<Vec<Blocker>, VogtError> {
        self.view.unfinished_blockers(a, b)
    }
    fn comments_for(&self, a: &str, b: i64) -> Result<Vec<Comment>, VogtError> {
        self.view.comments_for(a, b)
    }
    fn label_by_name(&self, a: &str) -> Result<Option<Label>, VogtError> {
        self.view.label_by_name(a)
    }
    fn list_labels(&self, l: i64, o: i64) -> Result<Vec<Label>, VogtError> {
        self.view.list_labels(l, o)
    }
    fn initiative_by_id(&self, a: &str) -> Result<Option<Initiative>, VogtError> {
        self.view.initiative_by_id(a)
    }
    fn initiative_by_slug(&self, a: &str) -> Result<Option<Initiative>, VogtError> {
        self.view.initiative_by_slug(a)
    }
    fn list_initiatives(&self, l: i64, o: i64) -> Result<Vec<Initiative>, VogtError> {
        self.view.list_initiatives(l, o)
    }
    fn workflow_for(&self, a: &str) -> Result<Workflow, VogtError> {
        self.view.workflow_for(a)
    }
    fn list_suppressions(&self, a: bool, b: i64) -> Result<Vec<Suppression>, VogtError> {
        self.view.list_suppressions(a, b)
    }
    fn suppression_by_id(&self, a: &str) -> Result<Option<Suppression>, VogtError> {
        self.view.suppression_by_id(a)
    }
    fn contract_exemptions(&self, a: &str) -> Result<Vec<ContractExemption>, VogtError> {
        self.view.contract_exemptions(a)
    }
    fn work_links_for_subjects(&self, a: &[String]) -> Result<BTreeMap<String, String>, VogtError> {
        self.view.work_links_for_subjects(a)
    }
    fn work_links_for_subjects_by_item(
        &self,
        a: &str,
    ) -> Result<BTreeMap<String, String>, VogtError> {
        self.view.work_links_for_subjects_by_item(a)
    }
    fn work_item_by_subject(&self, a: &str) -> Result<Option<WorkItem>, VogtError> {
        self.view.work_item_by_subject(a)
    }
    fn work_overlay(&self, a: &str) -> Result<Option<WorkOverlay>, VogtError> {
        self.view.work_overlay(a)
    }
    fn work_overlays(&self, a: &[String]) -> Result<BTreeMap<String, WorkOverlay>, VogtError> {
        self.view.work_overlays(a)
    }
    fn bound_branch_overlays(&self, a: i64) -> Result<Vec<WorkOverlay>, VogtError> {
        self.view.bound_branch_overlays(a)
    }
    fn token_by_hash(&self, a: &str) -> Result<Option<Token>, VogtError> {
        self.view.token_by_hash(a)
    }
    fn token_by_id(&self, a: &str) -> Result<Option<Token>, VogtError> {
        self.view.token_by_id(a)
    }
    fn list_tokens(&self, a: bool, b: i64) -> Result<Vec<Token>, VogtError> {
        self.view.list_tokens(a, b)
    }
    fn tokens_for_actor(&self, a: &str, b: bool) -> Result<Vec<Token>, VogtError> {
        self.view.tokens_for_actor(a, b)
    }
    fn install_closed(&self) -> Result<bool, VogtError> {
        self.view.install_closed()
    }
    fn list_auth_decisions(&self, a: Option<&str>, b: i64) -> Result<Vec<AuthDecision>, VogtError> {
        self.view.list_auth_decisions(a, b)
    }
    fn password_credential_by_username(
        &self,
        a: &str,
    ) -> Result<Option<PasswordCredential>, VogtError> {
        self.view.password_credential_by_username(a)
    }
    fn password_credential_for_actor(
        &self,
        a: &str,
    ) -> Result<Option<PasswordCredential>, VogtError> {
        self.view.password_credential_for_actor(a)
    }
    fn password_hash(&self, a: &str) -> Result<Option<String>, VogtError> {
        self.view.password_hash(a)
    }
    fn list_password_credentials(&self) -> Result<Vec<PasswordCredential>, VogtError> {
        self.view.list_password_credentials()
    }
    fn forge_account(&self, a: &str, b: &str) -> Result<Option<ForgeAccount>, VogtError> {
        self.view.forge_account(a, b)
    }
    fn forge_accounts_for_actor(&self, a: &str) -> Result<Vec<ForgeAccount>, VogtError> {
        self.view.forge_accounts_for_actor(a)
    }
    fn forge_account_secret(&self, a: &str, b: &str) -> Result<Option<String>, VogtError> {
        self.view.forge_account_secret(a, b)
    }
    fn list_drift(
        &self,
        a: Option<&str>,
        b: Option<&str>,
        c: Option<&str>,
        d: i64,
    ) -> Result<Vec<DriftProposal>, VogtError> {
        self.view.list_drift(a, b, c, d)
    }
    fn drift_by_id(&self, a: &str) -> Result<Option<DriftProposal>, VogtError> {
        self.view.drift_by_id(a)
    }
    fn open_drift_subjects(&self) -> Result<BTreeSet<(String, String, String)>, VogtError> {
        self.view.open_drift_subjects()
    }
    fn list_writeback_actions(
        &self,
        a: Option<&str>,
        b: i64,
    ) -> Result<Vec<WriteBackRecord>, VogtError> {
        self.view.list_writeback_actions(a, b)
    }
    fn drift_evidence_ids(&self) -> Result<BTreeSet<String>, VogtError> {
        self.view.drift_evidence_ids()
    }
    fn inbox_triage_by_key(&self, a: &str) -> Result<Option<InboxTriage>, VogtError> {
        self.view.inbox_triage_by_key(a)
    }
    fn inbox_triage_by_keys(
        &self,
        a: &[String],
    ) -> Result<BTreeMap<String, InboxTriage>, VogtError> {
        self.view.inbox_triage_by_keys(a)
    }
    fn list_inbox_triage(&self, a: i64) -> Result<Vec<InboxTriage>, VogtError> {
        self.view.list_inbox_triage(a)
    }
    fn actor_preference(&self, a: &str, b: &str) -> Result<Option<ActorPreference>, VogtError> {
        self.view.actor_preference(a, b)
    }
    fn actor_preferences(&self, a: &str) -> Result<Vec<ActorPreference>, VogtError> {
        self.view.actor_preferences(a)
    }
    fn session_by_id(&self, a: &str) -> Result<Option<CodingSession>, VogtError> {
        self.view.session_by_id(a)
    }
    fn session_grant(&self, a: &str) -> Result<Option<SessionGrant>, VogtError> {
        self.view.session_grant(a)
    }
    fn session_by_engine_id(&self, a: &str) -> Result<Option<CodingSession>, VogtError> {
        self.view.session_by_engine_id(a)
    }
    fn list_session_grants(
        &self,
        a: Option<&str>,
        b: Option<&str>,
        c: i64,
    ) -> Result<Vec<SessionGrant>, VogtError> {
        self.view.list_session_grants(a, b, c)
    }
    fn list_sessions(
        &self,
        a: Option<&str>,
        b: Option<&str>,
        c: bool,
        d: i64,
        e: i64,
    ) -> Result<Vec<CodingSession>, VogtError> {
        self.view.list_sessions(a, b, c, d, e)
    }
    fn list_events(&self, a: i64, b: i64, c: Option<&str>) -> Result<Vec<Event>, VogtError> {
        self.view.list_events(a, b, c)
    }
    fn list_audit(&self, a: &AuditQuery) -> Result<Vec<AuditRecord>, VogtError> {
        self.view.list_audit(a)
    }
    fn count_audit(&self, a: &AuditQuery) -> Result<i64, VogtError> {
        self.view.count_audit(a)
    }
}

impl<I: IdFactory> WriteTxn for SqliteWrite<I> {
    fn commit(self) -> Result<(), VogtError> {
        SqliteWrite::commit(self)
    }
    fn txn_id(&self) -> &str {
        &self.txn_id
    }
    fn revision(&self) -> i64 {
        self.revision
    }
    fn insert_actor(&mut self, actor: &Actor) -> Result<(), VogtError> {
        insert_actor(&self.view.conn, actor)
    }
    fn insert_project(&mut self, project: &Project) -> Result<(), VogtError> {
        self.view.conn.execute(
            "INSERT INTO projects (id, slug, name, root_path, repo_url, lifecycle_state, current_version, contract_version, compliance_status, compliance_checked_at, contract_adopted_at, write_back, link_state, exclusions, trust_state, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![project.id, project.slug, project.name, project.root_path, project.repo_url, project.lifecycle_state, project.current_version, project.contract_version, project.compliance_status, project.compliance_checked_at.map(to_iso), project.contract_adopted_at.map(to_iso), project.write_back, project.link_state, crate::decisions::python_json_dumps(&serde_json::json!(project.exclusions), false), project.trust_state, to_iso(project.created_at), to_iso(project.updated_at)],
        ).map_err(sql_err)?;
        Ok(())
    }
    fn update_project(
        &mut self,
        project_id: &str,
        update: &ProjectUpdate,
        at: Moment,
    ) -> Result<(), VogtError> {
        let mut set: Vec<String> = Vec::new();
        let mut values: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        let mut push = |column: &str, value: &Option<String>| {
            if let Some(value) = value {
                set.push(format!("{column} = ?"));
                values.push(Box::new(value.clone()));
            }
        };
        push("lifecycle_state", &update.lifecycle_state);
        push("repo_url", &update.repo_url);
        push("current_version", &update.current_version);
        push("compliance_status", &update.compliance_status);
        push("write_back", &update.write_back);
        push("link_state", &update.link_state);
        if let Some(moment) = update.compliance_checked_at {
            set.push("compliance_checked_at = ?".into());
            values.push(Box::new(to_iso(moment)));
        }
        if let Some(moment) = update.contract_adopted_at {
            set.push("contract_adopted_at = ?".into());
            values.push(Box::new(to_iso(moment)));
        } else if update.clear_contract_adopted_at {
            set.push("contract_adopted_at = NULL".into());
        }
        if let Some(exclusions) = &update.exclusions {
            set.push("exclusions = ?".into());
            values.push(Box::new(crate::decisions::python_json_dumps(
                &serde_json::json!(exclusions),
                false,
            )));
        }
        if set.is_empty() {
            return Ok(());
        }
        set.push("updated_at = ?".into());
        values.push(Box::new(to_iso(at)));
        values.push(Box::new(project_id.to_string()));
        let refs: Vec<&dyn rusqlite::ToSql> = values.iter().map(|v| v.as_ref()).collect();
        self.view
            .conn
            .execute(
                &format!("UPDATE projects SET {} WHERE id = ?", set.join(", ")),
                refs.as_slice(),
            )
            .map_err(sql_err)?;
        Ok(())
    }
    fn next_work_ref(&mut self) -> Result<String, VogtError> {
        let next = meta_counter(&self.view.conn, META_WORK_REF_SEQ)? + 1;
        meta_set(&self.view.conn, META_WORK_REF_SEQ, &next.to_string())?;
        Ok(format!("{WORK_REF_PREFIX}{next}"))
    }
    fn insert_work_item(&mut self, item: &WorkItem) -> Result<(), VogtError> {
        self.view.conn.execute(
            "INSERT INTO work_items (id, ref, kind, title, body, state, priority, effort, project_id, initiative_id, origin, trust_state, assignee_actor_id, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![item.id, item.reference, item.kind, item.title, item.body, item.state, item.priority, item.effort, item.project_id, item.initiative_id, item.origin, item.trust_state, item.assignee_actor_id, to_iso(item.created_at), to_iso(item.updated_at)],
        ).map_err(sql_err)?;
        for name in &item.labels {
            attach_label(&self.view.conn, &item.id, name)?;
        }
        Ok(())
    }
    fn update_work_item(
        &mut self,
        work_item_id: &str,
        update: &WorkItemUpdate,
        at: Moment,
    ) -> Result<(), VogtError> {
        let mut set: Vec<String> = Vec::new();
        let mut values: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        let mut push = |column: &str, value: &Option<String>| {
            if let Some(value) = value {
                set.push(format!("{column} = ?"));
                values.push(Box::new(value.clone()));
            }
        };
        push("title", &update.title);
        push("body", &update.body);
        push("state", &update.state);
        push("priority", &update.priority);
        push("effort", &update.effort);
        push("assignee_actor_id", &update.assignee_actor_id);
        push("initiative_id", &update.initiative_id);
        push("project_id", &update.project_id);
        push("superseded_by", &update.superseded_by);
        for (column, clear) in [
            ("effort", update.clear_effort),
            ("assignee_actor_id", update.clear_assignee),
            ("initiative_id", update.clear_initiative),
        ] {
            if clear {
                set.push(format!("{column} = NULL"));
            }
        }
        if !set.is_empty() {
            set.push("updated_at = ?".into());
            values.push(Box::new(to_iso(at)));
            values.push(Box::new(work_item_id.to_string()));
            let refs: Vec<&dyn rusqlite::ToSql> = values.iter().map(|v| v.as_ref()).collect();
            self.view
                .conn
                .execute(
                    &format!("UPDATE work_items SET {} WHERE id = ?", set.join(", ")),
                    refs.as_slice(),
                )
                .map_err(sql_err)?;
        }
        for name in &update.add_labels {
            attach_label(&self.view.conn, work_item_id, name)?;
        }
        for name in &update.remove_labels {
            self.view.conn.execute("DELETE FROM work_item_labels WHERE work_item_id = ? AND label_id IN (SELECT id FROM labels WHERE name = ?)", params![work_item_id, name]).map_err(sql_err)?;
        }
        if !update.add_labels.is_empty() || !update.remove_labels.is_empty() {
            self.view
                .conn
                .execute(
                    "UPDATE work_items SET updated_at = ? WHERE id = ?",
                    params![to_iso(at), work_item_id],
                )
                .map_err(sql_err)?;
        }
        Ok(())
    }
    fn insert_relation(
        &mut self,
        work_item_id: &str,
        related_id: &str,
        kind: RelationKind,
        at: Moment,
    ) -> Result<(), VogtError> {
        self.view.conn.execute("INSERT INTO work_relations (work_item_id, related_id, kind, created_at) VALUES (?, ?, ?, ?)", params![work_item_id, related_id, vocab_of(&kind), to_iso(at)]).map_err(sql_err)?;
        Ok(())
    }
    fn delete_relation(
        &mut self,
        work_item_id: &str,
        related_id: &str,
        kind: RelationKind,
    ) -> Result<bool, VogtError> {
        Ok(self
            .view
            .conn
            .execute(
                "DELETE FROM work_relations WHERE work_item_id = ? AND related_id = ? AND kind = ?",
                params![work_item_id, related_id, vocab_of(&kind)],
            )
            .map_err(sql_err)?
            > 0)
    }
    fn insert_label(&mut self, label: &Label) -> Result<(), VogtError> {
        self.view
            .conn
            .execute(
                "INSERT INTO labels (id, name, color, created_at) VALUES (?, ?, ?, ?)",
                params![label.id, label.name, label.color, to_iso(label.created_at)],
            )
            .map_err(sql_err)?;
        Ok(())
    }
    fn insert_initiative(&mut self, initiative: &Initiative) -> Result<(), VogtError> {
        self.view.conn.execute("INSERT INTO initiatives (id, slug, title, body, state, weight, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)", params![initiative.id, initiative.slug, initiative.title, initiative.body, vocab(&initiative.state), initiative.weight, to_iso(initiative.created_at), to_iso(initiative.updated_at)]).map_err(sql_err)?;
        Ok(())
    }
    fn update_initiative(&mut self, initiative: &Initiative) -> Result<(), VogtError> {
        self.view.conn.execute("UPDATE initiatives SET title = ?, body = ?, state = ?, weight = ?, updated_at = ? WHERE id = ?", params![initiative.title, initiative.body, vocab(&initiative.state), initiative.weight, to_iso(initiative.updated_at), initiative.id]).map_err(sql_err)?;
        Ok(())
    }
    fn insert_comment(&mut self, comment: &Comment) -> Result<(), VogtError> {
        self.view.conn.execute("INSERT INTO comments (id, work_item_id, actor_id, body, created_at) VALUES (?, ?, ?, ?, ?)", params![comment.id, comment.work_item_id, comment.actor_id, comment.body, to_iso(comment.created_at)]).map_err(sql_err)?;
        Ok(())
    }
    fn insert_suppression(&mut self, suppression: &Suppression) -> Result<(), VogtError> {
        self.view.conn.execute("INSERT INTO suppressions (id, match_kind, subject_key_or_pattern, scope_project_id, actor_id, reason, created_at) VALUES (?, ?, ?, ?, ?, ?, ?)", params![suppression.id, vocab_of(&suppression.match_kind), suppression.subject_key_or_pattern, suppression.scope_project_id, suppression.actor_id, suppression.reason, to_iso(suppression.created_at)]).map_err(sql_err)?;
        Ok(())
    }
    fn revoke_suppression(
        &mut self,
        suppression_id: &str,
        actor_id: &str,
        reason: &str,
        at: Moment,
    ) -> Result<bool, VogtError> {
        Ok(self.view.conn.execute("UPDATE suppressions SET revoked_at = ?, revoked_by_actor_id = ?, revoked_reason = ? WHERE id = ? AND revoked_at IS NULL", params![to_iso(at), actor_id, reason, suppression_id]).map_err(sql_err)? > 0)
    }
    fn insert_contract_exemption(
        &mut self,
        exemption: &ContractExemption,
    ) -> Result<(), VogtError> {
        self.view.conn.execute("INSERT INTO contract_exemptions (id, project_id, rule, target, reason, declared_by, declared_at) VALUES (?, ?, ?, ?, ?, ?, ?) ON CONFLICT (project_id, rule, target) DO UPDATE SET reason = excluded.reason, declared_by = excluded.declared_by, declared_at = excluded.declared_at", params![exemption.id, exemption.project_id, exemption.rule, exemption.target, exemption.reason, exemption.declared_by, to_iso(exemption.declared_at)]).map_err(sql_err)?;
        Ok(())
    }
    fn delete_contract_exemption(
        &mut self,
        project_id: &str,
        rule: &str,
        target: &str,
    ) -> Result<bool, VogtError> {
        Ok(self
            .view
            .conn
            .execute(
                "DELETE FROM contract_exemptions WHERE project_id = ? AND rule = ? AND target = ?",
                params![project_id, rule, target],
            )
            .map_err(sql_err)?
            > 0)
    }
    fn insert_work_link(&mut self, link: &WorkLink) -> Result<(), VogtError> {
        self.view.conn.execute(
            "INSERT INTO work_links (work_item_id, subject_key, origin_kind, source_url, relation, created_at) VALUES (?, ?, ?, ?, ?, ?)",
            params![
                link.work_item_id, link.subject_key, link.origin_kind, link.source_url,
                vocab_text(link.relation), to_iso(link.created_at),
            ],
        ).map(|_| ()).map_err(sql_err)
    }
    fn upsert_work_overlay(&mut self, overlay: &WorkOverlay) -> Result<(), VogtError> {
        // created_at keeps the existing row's value on conflict: the overlay
        // records when local semantics first attached, and updated_at carries
        // when they last moved.
        let branches =
            crate::decisions::python_json_dumps(&serde_json::json!(overlay.branches), false);
        self.view.conn.execute(
            "INSERT INTO work_overlay (subject_key, project_id, rank, workflow_state, priority, effort, assignee_actor_id, initiative_id, branches, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT(subject_key) DO UPDATE SET project_id = excluded.project_id, rank = excluded.rank, workflow_state = excluded.workflow_state, priority = excluded.priority, effort = excluded.effort, assignee_actor_id = excluded.assignee_actor_id, initiative_id = excluded.initiative_id, branches = excluded.branches, updated_at = excluded.updated_at",
            params![
                overlay.subject_key, overlay.project_id, overlay.rank, overlay.workflow_state,
                overlay.priority.map(vocab_text), overlay.effort.map(vocab_text),
                overlay.assignee_actor_id, overlay.initiative_id, branches,
                to_iso(overlay.created_at), to_iso(overlay.updated_at),
            ],
        ).map(|_| ()).map_err(sql_err)
    }
    fn insert_token(&mut self, token: &Token, token_hash: &str) -> Result<(), VogtError> {
        let scopes = crate::decisions::python_json_dumps(&serde_json::json!(token.scopes), false);
        let kind = vocab_text(token.kind);
        self.view.conn.execute(
            "INSERT INTO tokens (id, actor_id, name, token_hash, scopes, kind, created_at, expires_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            params![token.id, token.actor_id, token.name, token_hash, scopes, kind, to_iso(token.created_at), token.expires_at.map(to_iso)],
        ).map_err(sql_err)?;
        // First-run install mode closes once a person holds a credential, and
        // it closes in the same transaction that gives them one.
        latch_install_if_operator(&self.view.conn, token.created_at)
    }
    fn carry_credentials(
        &mut self,
        carried: &CarriedCredentials,
        reason: &str,
        at: Moment,
    ) -> Result<CarryReport, VogtError> {
        // Actors match by identity rather than by id: the two instances minted
        // their actor ids independently, and the same person is the same
        // identity_ref on both.
        let mut actor_map: BTreeMap<String, String> = BTreeMap::new();
        let mut added = 0i64;
        for actor in &carried.actors {
            if let Some(existing) = self.view.actor_by_identity(&actor.identity_ref)? {
                actor_map.insert(actor.id.clone(), existing.id.clone());
                // The carried credentials must keep working, so their actor
                // keeps the enabled state it had where they were issued.
                self.view
                    .conn
                    .execute(
                        "UPDATE actors SET disabled = ? WHERE id = ?",
                        params![i64::from(actor.disabled), existing.id],
                    )
                    .map_err(sql_err)?;
                continue;
            }
            let mut inserted = actor.clone();
            if self.view.actor_by_id(&actor.id)?.is_some() {
                // The id belongs to a different person here. The carried actor
                // takes a fresh id rather than merging the two.
                inserted.id = self
                    .ids
                    .lock()
                    .expect("the shared clock and ids are not poisoned")
                    .next("act");
            }
            insert_actor(&self.view.conn, &inserted)?;
            actor_map.insert(actor.id.clone(), inserted.id);
            added += 1;
        }
        let mapped = |row: &serde_json::Value| -> Result<serde_json::Value, VogtError> {
            let actor_id = row.get("actor_id").and_then(|v| v.as_str()).unwrap_or("");
            let Some(mapped_id) = actor_map.get(actor_id) else {
                return Err(VogtError::Conflict(format!(
                    "carried credential names actor {actor_id}, not carried"
                )));
            };
            let mut copy = row.clone();
            copy["actor_id"] = serde_json::Value::String(mapped_id.clone());
            Ok(copy)
        };

        // Every live token the copy already holds is revoked unless it is one
        // of the carried secrets.
        let carried_hashes: BTreeSet<String> = carried
            .tokens
            .iter()
            .filter_map(|row| {
                row.get("token_hash")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            })
            .collect();
        let live = many(
            &self.view.conn,
            "SELECT id, token_hash FROM tokens WHERE revoked_at IS NULL",
            [],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )?;
        let mut revoked = 0i64;
        for (id, hash) in &live {
            if carried_hashes.contains(hash) {
                continue;
            }
            self.view
                .conn
                .execute(
                    "UPDATE tokens SET revoked_at = ?, revoked_reason = ? WHERE id = ?",
                    params![to_iso(at), reason, id],
                )
                .map_err(sql_err)?;
            revoked += 1;
        }
        for raw in &carried.tokens {
            let mut row = mapped(raw)?;
            let hash = json_str(&row, "token_hash");
            let same_secret: Option<String> = self
                .view
                .conn
                .query_row(
                    "SELECT id FROM tokens WHERE token_hash = ?",
                    [&hash],
                    |found| found.get(0),
                )
                .optional()
                .map_err(sql_err)?;
            if let Some(existing_id) = same_secret {
                // Both instances hold this secret. Keep the existing row's id,
                // which the history refers to, and make it say what the
                // carried row said.
                let sets = TOKEN_CARRY_COLUMNS
                    .iter()
                    .filter(|column| **column != "id")
                    .map(|column| format!("{column} = ?"))
                    .collect::<Vec<_>>()
                    .join(", ");
                let mut params: Vec<Box<dyn rusqlite::ToSql>> = TOKEN_CARRY_COLUMNS
                    .iter()
                    .filter(|column| **column != "id")
                    .map(|column| json_param(&row, column))
                    .collect();
                params.push(Box::new(existing_id));
                let refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|v| v.as_ref()).collect();
                self.view
                    .conn
                    .execute(
                        &format!("UPDATE tokens SET {sets} WHERE id = ?"),
                        refs.as_slice(),
                    )
                    .map_err(sql_err)?;
                continue;
            }
            let carried_id = json_str(&row, "id");
            let clash: Option<String> = self
                .view
                .conn
                .query_row(
                    "SELECT id FROM tokens WHERE id = ?",
                    [&carried_id],
                    |found| found.get(0),
                )
                .optional()
                .map_err(sql_err)?;
            if clash.is_some() {
                // Another secret already holds this id. The carried token keeps
                // its secret and takes a fresh id.
                row["id"] = serde_json::Value::String(
                    self.ids
                        .lock()
                        .expect("the shared clock and ids are not poisoned")
                        .next("tok"),
                );
            }
            insert_carry_row(&self.view.conn, "tokens", TOKEN_CARRY_COLUMNS, &row)?;
        }

        // Password logins and forge accounts are replaced wholesale: nothing
        // refers to either by id, so there is no history to keep.
        let dropped_passwords = self
            .view
            .conn
            .execute("DELETE FROM password_credentials", [])
            .map_err(sql_err)? as i64;
        for raw in &carried.password_credentials {
            insert_carry_row(
                &self.view.conn,
                "password_credentials",
                PASSWORD_CARRY_COLUMNS,
                &mapped(raw)?,
            )?;
        }
        let dropped_forge = self
            .view
            .conn
            .execute("DELETE FROM forge_accounts", [])
            .map_err(sql_err)? as i64;
        for raw in &carried.forge_accounts {
            insert_carry_row(
                &self.view.conn,
                "forge_accounts",
                FORGE_ACCOUNT_CARRY_COLUMNS,
                &mapped(raw)?,
            )?;
        }
        let report = CarryReport {
            tokens_kept: carried.tokens.len() as i64,
            source_tokens_revoked: revoked,
            password_logins_kept: carried.password_credentials.len() as i64,
            source_password_logins_dropped: dropped_passwords,
            forge_accounts_kept: carried.forge_accounts.len() as i64,
            source_forge_accounts_dropped: dropped_forge,
            actors_added: added,
        };
        // A carried password or a person's token closes install mode too.
        latch_install_if_operator(&self.view.conn, at)?;
        Ok(report)
    }
    fn set_instance_identity(
        &mut self,
        instance_id: &str,
        stamp: &CloneStamp,
    ) -> Result<(), VogtError> {
        meta_set(&self.view.conn, META_INSTANCE_ID, instance_id)?;
        meta_set(&self.view.conn, META_CLONED_FROM, &stamp.source_instance_id)?;
        meta_set(&self.view.conn, META_CLONED_AT, &to_iso(stamp.cloned_at))?;
        meta_set(
            &self.view.conn,
            META_CLONED_BACKUP_TAKEN_AT,
            &to_iso(stamp.backup_taken_at),
        )
    }
    fn revoke_token(
        &mut self,
        token_id: &str,
        reason: &str,
        at: Moment,
    ) -> Result<bool, VogtError> {
        Ok(self.view.conn.execute(
            "UPDATE tokens SET revoked_at = ?, revoked_reason = ? WHERE id = ? AND revoked_at IS NULL",
            params![to_iso(at), reason, token_id],
        ).map_err(sql_err)? > 0)
    }
    fn reinstate_token(&mut self, token_id: &str) -> Result<bool, VogtError> {
        Ok(self.view.conn.execute(
            "UPDATE tokens SET revoked_at = NULL, revoked_reason = NULL WHERE id = ? AND revoked_at IS NOT NULL",
            [token_id],
        ).map_err(sql_err)? > 0)
    }
    fn upsert_password_credential(
        &mut self,
        actor_id: &str,
        username: &str,
        password_hash: &str,
        scopes: &[String],
        at: Moment,
    ) -> Result<(), VogtError> {
        let stamp = to_iso(at);
        let scopes = crate::decisions::python_json_dumps(&serde_json::json!(scopes), false);
        self.view.conn.execute(
            "INSERT INTO password_credentials (actor_id, username, password_hash, scopes, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?) ON CONFLICT (actor_id) DO UPDATE SET username = excluded.username, password_hash = excluded.password_hash, scopes = excluded.scopes, updated_at = excluded.updated_at",
            params![actor_id, username, password_hash, scopes, stamp, stamp],
        ).map_err(sql_err)?;
        latch_install_if_operator(&self.view.conn, at)
    }
    fn delete_password_credential(&mut self, actor_id: &str) -> Result<bool, VogtError> {
        Ok(self
            .view
            .conn
            .execute(
                "DELETE FROM password_credentials WHERE actor_id = ?",
                [actor_id],
            )
            .map_err(sql_err)?
            > 0)
    }
    fn upsert_forge_account(
        &mut self,
        actor_id: &str,
        host: &str,
        login: &str,
        scopes: &str,
        encrypted_token: &str,
        at: Moment,
    ) -> Result<(), VogtError> {
        let stamp = to_iso(at);
        // Re-linking keeps the original created_at and rotates everything
        // else, including the ciphertext.
        let updated = self.view.conn.execute(
            "UPDATE forge_accounts SET login = ?, scopes = ?, encrypted_token = ?, updated_at = ? WHERE actor_id = ? AND host = ?",
            params![login, scopes, encrypted_token, stamp, actor_id, host],
        ).map_err(sql_err)?;
        if updated == 0 {
            self.view.conn.execute(
                "INSERT INTO forge_accounts (actor_id, host, login, scopes, encrypted_token, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
                params![actor_id, host, login, scopes, encrypted_token, stamp, stamp],
            ).map_err(sql_err)?;
        }
        Ok(())
    }
    fn delete_forge_account(&mut self, actor_id: &str, host: &str) -> Result<bool, VogtError> {
        Ok(self
            .view
            .conn
            .execute(
                "DELETE FROM forge_accounts WHERE actor_id = ? AND host = ?",
                params![actor_id, host],
            )
            .map_err(sql_err)?
            > 0)
    }
    fn insert_writeback(&mut self, record: &WriteBackRecord) -> Result<(), VogtError> {
        self.view.conn.execute(
            "INSERT INTO writeback_actions (id, at, project_id, work_item_id, actor_id, action, subject_key, policy, outcome, reason, detail, source_url) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                record.id, to_iso(record.at), record.project_id, record.work_item_id,
                record.actor_id, vocab_text(record.action), record.subject_key, record.policy,
                vocab_text(record.outcome), record.reason, record.detail, record.source_url,
            ],
        ).map(|_| ()).map_err(sql_err)
    }
    fn insert_session(&mut self, session: &CodingSession) -> Result<(), VogtError> {
        self.view.conn.execute(
            "INSERT INTO coding_sessions (id, engine_session_id, project_id, work_item_id, actor_id, cwd, template, model, effort, reason, started_at, stopped_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                session.id, session.engine_session_id, session.project_id, session.work_item_id,
                session.actor_id, session.cwd, session.template, session.model, session.effort,
                session.reason, to_iso(session.started_at), session.stopped_at.map(to_iso),
            ],
        ).map(|_| ()).map_err(sql_err)
    }
    fn insert_session_grant(&mut self, grant: &SessionGrant) -> Result<(), VogtError> {
        self.view.conn.execute(
            "INSERT INTO session_grants (id, target_engine_session_id, kind, var, project_id, secret_name, capability, uses, ttl_seconds, reason, requested_by, requested_at, state, decided_by, decided_at, decision_reason, expires_at, revoked_by, revoked_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                grant.id, grant.target_engine_session_id, vocab_text(grant.kind), grant.var,
                grant.project_id, grant.secret_name, grant.capability, vocab_text(grant.uses),
                grant.ttl_seconds, grant.reason, grant.requested_by, to_iso(grant.requested_at),
                vocab_text(grant.state), grant.decided_by, grant.decided_at.map(to_iso),
                grant.decision_reason, grant.expires_at.map(to_iso), grant.revoked_by,
                grant.revoked_at.map(to_iso),
            ],
        ).map(|_| ()).map_err(sql_err)
    }
    fn update_session_grant(&mut self, grant: &SessionGrant) -> Result<(), VogtError> {
        self.view.conn.execute(
            "UPDATE session_grants SET state = ?, decided_by = ?, decided_at = ?, decision_reason = ?, expires_at = ?, revoked_by = ?, revoked_at = ? WHERE id = ?",
            params![
                vocab_text(grant.state), grant.decided_by, grant.decided_at.map(to_iso),
                grant.decision_reason, grant.expires_at.map(to_iso), grant.revoked_by,
                grant.revoked_at.map(to_iso), grant.id,
            ],
        ).map(|_| ()).map_err(sql_err)
    }
    fn set_session_work_item(
        &mut self,
        session_id: &str,
        work_item_id: Option<&str>,
    ) -> Result<(), VogtError> {
        self.view
            .conn
            .execute(
                "UPDATE coding_sessions SET work_item_id = ? WHERE id = ?",
                params![work_item_id, session_id],
            )
            .map(|_| ())
            .map_err(sql_err)
    }
    fn mark_session_stopped(
        &mut self,
        session_id: &str,
        stopped_at: Moment,
    ) -> Result<(), VogtError> {
        // A second stop changes nothing: the first one is the time it ended.
        self.view
            .conn
            .execute(
                "UPDATE coding_sessions SET stopped_at = ? WHERE id = ? AND stopped_at IS NULL",
                params![to_iso(stopped_at), session_id],
            )
            .map(|_| ())
            .map_err(sql_err)
    }
    fn insert_drift(&mut self, proposal: &DriftProposal) -> Result<(), VogtError> {
        self.view.conn.execute(
            "INSERT INTO drift_proposals (id, kind, subject_kind, subject_id, project_id, summary, evidence_observation_id, evidence_snapshot, proposed_change, status, opened_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                proposal.id, proposal.kind, proposal.subject_kind, proposal.subject_id,
                proposal.project_id, proposal.summary, proposal.evidence_observation_id,
                crate::decisions::python_json_dumps(&proposal.evidence_snapshot, false),
                crate::decisions::python_json_dumps(&proposal.proposed_change, false),
                vocab_text(proposal.status), to_iso(proposal.opened_at),
            ],
        ).map(|_| ()).map_err(sql_err)
    }
    fn upsert_inbox_triage(&mut self, triage: &InboxTriage) -> Result<(), VogtError> {
        self.view.conn.execute(
            "INSERT INTO inbox_triage (entry_key, state, snooze_until, actor_id, decided_at, occurrence_snapshot) VALUES (?, ?, ?, ?, ?, ?) ON CONFLICT(entry_key) DO UPDATE SET state = excluded.state, snooze_until = excluded.snooze_until, actor_id = excluded.actor_id, decided_at = excluded.decided_at, occurrence_snapshot = excluded.occurrence_snapshot",
            params![
                triage.entry_key, vocab_text(triage.state), triage.snooze_until.map(to_iso),
                triage.actor_id, to_iso(triage.decided_at),
                crate::decisions::python_json_dumps(&triage.occurrence_snapshot, true),
            ],
        ).map(|_| ()).map_err(sql_err)
    }
    fn upsert_actor_preference(&mut self, preference: &ActorPreference) -> Result<(), VogtError> {
        let values = params![
            crate::decisions::python_json_dumps(&preference.value, true),
            preference.version,
            to_iso(preference.updated_at),
            preference.actor_id,
            preference.key,
        ];
        let updated = self.view.conn.execute(
            "UPDATE actor_preferences SET value = ?, version = ?, updated_at = ? WHERE actor_id = ? AND key = ?",
            values,
        ).map_err(sql_err)?;
        if updated == 0 {
            self.view.conn.execute(
                "INSERT INTO actor_preferences (value, version, updated_at, actor_id, key) VALUES (?, ?, ?, ?, ?)",
                values,
            ).map_err(sql_err)?;
        }
        Ok(())
    }
    fn mark_drift_superseded(
        &mut self,
        proposal_id: &str,
        detail: Option<&str>,
        at: Option<Moment>,
    ) -> Result<bool, VogtError> {
        // Only open proposals: a resolved one is history.
        Ok(self.view.conn.execute(
            "UPDATE drift_proposals SET superseded_at = ?, superseded_detail = ? WHERE id = ? AND status = 'open'",
            params![at.map(to_iso), detail, proposal_id],
        ).map_err(sql_err)? > 0)
    }
    fn resolve_drift(
        &mut self,
        proposal_id: &str,
        status: &str,
        actor_id: &str,
        reason: &str,
        at: Moment,
    ) -> Result<bool, VogtError> {
        Ok(self.view.conn.execute(
            "UPDATE drift_proposals SET status = ?, resolved_by_actor_id = ?, resolution_reason = ?, resolved_at = ? WHERE id = ? AND status = 'open'",
            params![status, actor_id, reason, to_iso(at), proposal_id],
        ).map_err(sql_err)? > 0)
    }
    fn upsert_workflow(&mut self, workflow: &Workflow, at: Moment) -> Result<(), VogtError> {
        let definition = workflow.to_definition_json();
        let updated = self
            .view
            .conn
            .execute(
                "UPDATE workflow_defs SET definition = ?, updated_at = ? WHERE kind = ?",
                params![definition, to_iso(at), workflow.kind],
            )
            .map_err(sql_err)?;
        if updated == 0 {
            self.view
                .conn
                .execute(
                    "INSERT INTO workflow_defs (kind, definition, updated_at) VALUES (?, ?, ?)",
                    params![workflow.kind, definition, to_iso(at)],
                )
                .map_err(sql_err)?;
        }
        self.view.workflow_cache.borrow_mut().remove(&workflow.kind);
        Ok(())
    }
    fn append_audit(
        &mut self,
        actor: &Actor,
        operation: &str,
        entity_kind: &str,
        entity_id: &str,
        reason: &str,
        payload_digest: &str,
        at: Moment,
    ) -> Result<AuditRecord, VogtError> {
        // The store owns the record's identity: the caller supplies who did
        // what and why, and the transaction supplies the id, txn and revision.
        let stored = AuditRecord {
            id: self
                .ids
                .lock()
                .expect("the shared clock and ids are not poisoned")
                .next("aud"),
            txn_id: self.txn_id.clone(),
            revision: self.revision,
            actor_id: actor.id.clone(),
            actor_identity_ref: actor.identity_ref.clone(),
            operation: operation.to_string(),
            entity_kind: entity_kind.to_string(),
            entity_id: entity_id.to_string(),
            reason: reason.to_string(),
            payload_digest: payload_digest.to_string(),
            at,
        };
        insert_audit(&self.view.conn, &stored)?;
        Ok(stored)
    }
    fn append_event(
        &mut self,
        kind: &str,
        entity_kind: &str,
        entity_id: &str,
        actor_id: Option<&str>,
        audit_id: Option<&str>,
        summary: &serde_json::Value,
        at: Moment,
    ) -> Result<Event, VogtError> {
        let rendered = crate::decisions::python_json_dumps(summary, false);
        self.view.conn.execute(
            "INSERT INTO events (kind, entity_kind, entity_id, actor_id, audit_id, summary, at) VALUES (?, ?, ?, ?, ?, ?, ?)",
            params![kind, entity_kind, entity_id, actor_id, audit_id, rendered, to_iso(at)],
        ).map_err(sql_err)?;
        Ok(Event {
            seq: self.view.conn.last_insert_rowid(),
            kind: kind.to_string(),
            entity_kind: entity_kind.to_string(),
            entity_id: entity_id.to_string(),
            actor_id: actor_id.map(str::to_string),
            audit_id: audit_id.map(str::to_string),
            summary: summary.clone(),
            at,
        })
    }
}

fn ensure_default_workflows(conn: &Connection, now: Moment) -> Result<(), VogtError> {
    // Seeding lives in migrate, not bootstrap: an instance created before the
    // workflow table existed never runs bootstrap again, and spelling the
    // defaults in the migration SQL would write them twice.
    if !migrator::table_exists(conn, "workflow_defs").unwrap_or(false) {
        return Ok(());
    }
    conn.execute_batch("BEGIN IMMEDIATE").map_err(sql_err)?;
    let seeded = (|| -> Result<(), VogtError> {
        for kind in ["feature", "bug", "chore", "question"] {
            let present: bool = conn
                .query_row("SELECT 1 FROM workflow_defs WHERE kind = ?", [kind], |_| {
                    Ok(true)
                })
                .unwrap_or(false);
            if !present {
                let workflow = default_workflow(kind);
                conn.execute(
                    "INSERT INTO workflow_defs (kind, definition, updated_at) VALUES (?, ?, ?)",
                    params![kind, workflow.to_definition_json(), to_iso(now)],
                )
                .map_err(sql_err)?;
            }
        }
        Ok(())
    })();
    if seeded.is_ok() {
        conn.execute_batch("COMMIT").map_err(sql_err)?;
    } else {
        let _ = conn.execute_batch("ROLLBACK");
    }
    seeded
}

fn meta_get(conn: &Connection, key: &str) -> Result<Option<String>, VogtError> {
    conn.query_row("SELECT value FROM meta WHERE key = ?", [key], |row| {
        row.get(0)
    })
    .optional()
    .map_err(sql_err)
}
fn meta_set(conn: &Connection, key: &str, value: &str) -> Result<(), VogtError> {
    conn.execute("INSERT INTO meta (key, value) VALUES (?, ?) ON CONFLICT (key) DO UPDATE SET value = excluded.value", params![key, value]).map_err(sql_err)?;
    Ok(())
}
fn insert_actor(conn: &Connection, actor: &Actor) -> Result<(), VogtError> {
    conn.execute("INSERT INTO actors (id, kind, display_name, identity_ref, disabled, created_at) VALUES (?, ?, ?, ?, ?, ?)", params![actor.id, actor.kind.as_str(), actor.display_name, actor.identity_ref, i64::from(actor.disabled), to_iso(actor.created_at)]).map_err(sql_err)?;
    Ok(())
}
fn insert_audit(conn: &Connection, record: &AuditRecord) -> Result<(), VogtError> {
    conn.execute("INSERT INTO audit (id, txn_id, revision, actor_id, operation, entity_kind, entity_id, reason, payload_digest, at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)", params![record.id, record.txn_id, record.revision, record.actor_id, record.operation, record.entity_kind, record.entity_id, record.reason, record.payload_digest, to_iso(record.at)]).map_err(sql_err)?;
    Ok(())
}
fn count(conn: &Connection, table: &str) -> Result<i64, VogtError> {
    conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
        row.get(0)
    })
    .map_err(sql_err)
}
fn one<T>(
    conn: &Connection,
    sql: &str,
    params: impl rusqlite::Params,
    map: impl Fn(&Row<'_>) -> rusqlite::Result<T>,
) -> Result<Option<T>, VogtError> {
    conn.query_row(sql, params, map).optional().map_err(sql_err)
}
fn many<T>(
    conn: &Connection,
    sql: &str,
    params: impl rusqlite::Params,
    map: impl Fn(&Row<'_>) -> rusqlite::Result<T>,
) -> Result<Vec<T>, VogtError> {
    let mut statement = conn.prepare(sql).map_err(sql_err)?;
    let rows = statement.query_map(params, map).map_err(sql_err)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(sql_err)
}
fn moment(row: &Row<'_>, column: &str) -> rusqlite::Result<Moment> {
    from_iso(&row.get::<_, String>(column)?).map_err(rusqlite::Error::InvalidColumnName)
}
fn opt_moment(row: &Row<'_>, column: &str) -> rusqlite::Result<Option<Moment>> {
    row.get::<_, Option<String>>(column)?
        .map(|text| from_iso(&text).map_err(rusqlite::Error::InvalidColumnName))
        .transpose()
}
fn row_actor(row: &Row<'_>) -> rusqlite::Result<Actor> {
    Ok(Actor {
        id: row.get("id")?,
        kind: if row.get::<_, String>("kind")? == "agent" {
            ActorKind::Agent
        } else {
            ActorKind::Human
        },
        display_name: row.get("display_name")?,
        identity_ref: row.get("identity_ref")?,
        disabled: row.get::<_, i64>("disabled")? != 0,
        created_at: moment(row, "created_at")?,
    })
}
fn row_project(row: &Row<'_>) -> rusqlite::Result<Project> {
    Ok(Project {
        id: row.get("id")?,
        slug: row.get("slug")?,
        name: row.get("name")?,
        root_path: row.get("root_path")?,
        repo_url: row.get("repo_url")?,
        lifecycle_state: row.get("lifecycle_state")?,
        current_version: row.get("current_version")?,
        contract_version: row.get("contract_version")?,
        compliance_status: row.get("compliance_status")?,
        compliance_checked_at: opt_moment(row, "compliance_checked_at")?,
        contract_adopted_at: opt_moment(row, "contract_adopted_at")?,
        write_back: row.get("write_back")?,
        link_state: row.get("link_state")?,
        exclusions: serde_json::from_str(&row.get::<_, String>("exclusions")?)
            .map_err(|err| rusqlite::Error::InvalidColumnName(err.to_string()))?,
        trust_state: row.get("trust_state")?,
        created_at: moment(row, "created_at")?,
        updated_at: moment(row, "updated_at")?,
    })
}
fn row_label(row: &Row<'_>) -> rusqlite::Result<Label> {
    Ok(Label {
        id: row.get("id")?,
        name: row.get("name")?,
        color: row.get("color")?,
        created_at: moment(row, "created_at")?,
    })
}
fn row_initiative(row: &Row<'_>) -> rusqlite::Result<Initiative> {
    Ok(Initiative {
        id: row.get("id")?,
        slug: row.get("slug")?,
        title: row.get("title")?,
        body: row.get("body")?,
        state: serde_json::from_value(serde_json::Value::String(row.get("state")?))
            .map_err(|e| rusqlite::Error::InvalidColumnName(e.to_string()))?,
        weight: row.get("weight")?,
        created_at: moment(row, "created_at")?,
        updated_at: moment(row, "updated_at")?,
    })
}
fn row_exemption(row: &Row<'_>) -> rusqlite::Result<ContractExemption> {
    Ok(ContractExemption {
        id: row.get("id")?,
        project_id: row.get("project_id")?,
        project_slug: row.get("project_slug")?,
        rule: row.get("rule")?,
        target: row.get("target")?,
        reason: row.get("reason")?,
        declared_by: row.get("declared_by")?,
        declared_at: moment(row, "declared_at")?,
    })
}
fn vocab(state: &InitiativeState) -> String {
    serde_json::to_value(state)
        .unwrap()
        .as_str()
        .unwrap()
        .to_string()
}

/// Close first-run install mode once a person holds a credential. The latch
/// only ever gets set, so removing the credential later does not reopen the
/// door. An agent-only token leaves it open.
fn attach_label(conn: &Connection, work_item_id: &str, name: &str) -> Result<(), VogtError> {
    let id: String = conn
        .query_row("SELECT id FROM labels WHERE name = ?", [name], |row| {
            row.get(0)
        })
        .optional()
        .map_err(sql_err)?
        .ok_or_else(|| VogtError::NotFound(format!("no label named {name:?}")))?;
    conn.execute("INSERT INTO work_item_labels (work_item_id, label_id) SELECT ?, ? WHERE NOT EXISTS (SELECT 1 FROM work_item_labels WHERE work_item_id = ? AND label_id = ?)", params![work_item_id, id, work_item_id, id]).map_err(sql_err)?;
    Ok(())
}
fn vocab_of<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .unwrap()
        .as_str()
        .unwrap()
        .to_string()
}

fn latch_install_if_operator(conn: &Connection, at: Moment) -> Result<(), VogtError> {
    conn.execute(
        "INSERT OR IGNORE INTO install_latch (id, closed_at, reason) SELECT 1, ?, 'a person holds a credential' WHERE EXISTS (SELECT 1 FROM tokens t JOIN actors a ON a.id = t.actor_id WHERE a.kind <> 'agent') OR EXISTS (SELECT 1 FROM password_credentials)",
        [to_iso(at)],
    ).map(|_| ()).map_err(sql_err)
}

fn vocab_text<T: serde::Serialize>(value: T) -> String {
    serde_json::to_value(value)
        .expect("a vocab value is a string")
        .as_str()
        .expect("snake_case")
        .to_string()
}

/// Commit an immediate transaction, or roll it back and surface the error.
fn finish_immediate(conn: &Connection, outcome: rusqlite::Result<usize>) -> Result<(), VogtError> {
    match outcome {
        Ok(_) => conn.execute("COMMIT", []).map(|_| ()).map_err(sql_err),
        Err(err) => {
            let _ = conn.execute("ROLLBACK", []);
            Err(sql_err(err))
        }
    }
}

fn row_auth_decision(row: &Row<'_>) -> rusqlite::Result<AuthDecision> {
    let decision: String = row.get("decision")?;
    Ok(AuthDecision {
        id: row.get("id")?,
        at: moment(row, "at")?,
        decision: serde_json::from_value(serde_json::Value::String(decision)).map_err(|err| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(err))
        })?,
        reason_code: row.get("reason_code")?,
        operation: row.get("operation")?,
        scope: row.get("scope")?,
        actor_id: row.get("actor_id")?,
        token_id: row.get("token_id")?,
        identity_ref: row.get("identity_ref")?,
        transport: row.get("transport")?,
        detail: row.get("detail")?,
    })
}

fn row_session(row: &Row<'_>) -> rusqlite::Result<CodingSession> {
    Ok(CodingSession {
        id: row.get("id")?,
        engine_session_id: row.get("engine_session_id")?,
        project_id: row.get("project_id")?,
        work_item_id: row.get("work_item_id")?,
        actor_id: row.get("actor_id")?,
        cwd: row.get("cwd")?,
        template: row.get("template")?,
        model: row.get("model")?,
        effort: row.get("effort")?,
        reason: row.get("reason")?,
        started_at: moment(row, "started_at")?,
        stopped_at: opt_moment(row, "stopped_at")?,
    })
}

fn row_session_grant(row: &Row<'_>) -> rusqlite::Result<SessionGrant> {
    Ok(SessionGrant {
        id: row.get("id")?,
        target_engine_session_id: row.get("target_engine_session_id")?,
        kind: vocab_cell(row, "kind")?,
        var: row.get("var")?,
        project_id: row.get("project_id")?,
        secret_name: row.get("secret_name")?,
        capability: row.get("capability")?,
        uses: vocab_cell(row, "uses")?,
        ttl_seconds: row.get("ttl_seconds")?,
        reason: row.get("reason")?,
        requested_by: row.get("requested_by")?,
        requested_at: moment(row, "requested_at")?,
        state: vocab_cell(row, "state")?,
        decided_by: row.get("decided_by")?,
        decided_at: opt_moment(row, "decided_at")?,
        decision_reason: row.get("decision_reason")?,
        expires_at: opt_moment(row, "expires_at")?,
        revoked_by: row.get("revoked_by")?,
        revoked_at: opt_moment(row, "revoked_at")?,
    })
}

const DRIFT_SELECT: &str = "SELECT d.*, p.slug AS project_slug, a.identity_ref AS resolved_by FROM drift_proposals d LEFT JOIN projects p ON p.id = d.project_id LEFT JOIN actors a ON a.id = d.resolved_by_actor_id";

fn row_drift(row: &Row<'_>) -> rusqlite::Result<DriftProposal> {
    let status: String = row.get("status")?;
    Ok(DriftProposal {
        id: row.get("id")?,
        kind: row.get("kind")?,
        subject_kind: row.get("subject_kind")?,
        subject_id: row.get("subject_id")?,
        project_id: row.get("project_id")?,
        project_slug: row.get("project_slug")?,
        summary: row.get("summary")?,
        evidence_observation_id: row.get("evidence_observation_id")?,
        evidence_snapshot: json_cell(row, "evidence_snapshot")?,
        proposed_change: json_cell(row, "proposed_change")?,
        status: serde_json::from_value(serde_json::Value::String(status)).map_err(|err| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(err))
        })?,
        opened_at: moment(row, "opened_at")?,
        superseded_at: opt_moment(row, "superseded_at")?,
        superseded_detail: row.get("superseded_detail")?,
        resolved_by_actor_id: row.get("resolved_by_actor_id")?,
        resolved_by_identity_ref: row.get("resolved_by")?,
        resolved_at: opt_moment(row, "resolved_at")?,
        resolution_reason: row.get("resolution_reason")?,
    })
}

const INBOX_SELECT: &str = "SELECT t.*, a.identity_ref AS actor_identity_ref FROM inbox_triage t JOIN actors a ON a.id = t.actor_id";

fn row_inbox_triage(row: &Row<'_>) -> rusqlite::Result<InboxTriage> {
    let state: String = row.get("state")?;
    Ok(InboxTriage {
        entry_key: row.get("entry_key")?,
        state: serde_json::from_value(serde_json::Value::String(state)).map_err(|err| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(err))
        })?,
        snooze_until: opt_moment(row, "snooze_until")?,
        actor_id: row.get("actor_id")?,
        actor_identity_ref: row.get("actor_identity_ref")?,
        decided_at: moment(row, "decided_at")?,
        occurrence_snapshot: json_cell(row, "occurrence_snapshot")?,
    })
}

fn row_actor_preference(row: &Row<'_>) -> rusqlite::Result<ActorPreference> {
    // A preference value is an object. Anything else stored there reads back
    // as an empty one, which is what Python's row mapper does.
    let value = json_cell(row, "value")?;
    Ok(ActorPreference {
        actor_id: row.get("actor_id")?,
        key: row.get("key")?,
        value: if value.is_object() {
            value
        } else {
            serde_json::json!({})
        },
        version: row.get("version")?,
        updated_at: moment(row, "updated_at")?,
    })
}

/// One JSON column, parsed. A corrupt value is a conversion failure, not null.
fn json_cell(row: &Row<'_>, column: &str) -> rusqlite::Result<serde_json::Value> {
    let text: String = row.get(column)?;
    serde_json::from_str(&text).map_err(|err| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(err))
    })
}

const TOKEN_CARRY_COLUMNS: &[&str] = &[
    "id",
    "actor_id",
    "name",
    "token_hash",
    "scopes",
    "kind",
    "created_at",
    "expires_at",
    "last_used_at",
    "revoked_at",
    "revoked_reason",
];
const PASSWORD_CARRY_COLUMNS: &[&str] = &[
    "actor_id",
    "username",
    "password_hash",
    "scopes",
    "created_at",
    "updated_at",
];
const FORGE_ACCOUNT_CARRY_COLUMNS: &[&str] = &[
    "actor_id",
    "host",
    "login",
    "scopes",
    "encrypted_token",
    "created_at",
    "updated_at",
];

fn json_str(row: &serde_json::Value, column: &str) -> String {
    row.get(column)
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string()
}

/// One carried cell, bound as text. JSON null and a missing column both become
/// SQL NULL, which is what the source row held.
fn json_param(row: &serde_json::Value, column: &str) -> Box<dyn rusqlite::ToSql> {
    match row.get(column) {
        Some(serde_json::Value::Null) | None => Box::new(None::<String>),
        Some(serde_json::Value::String(text)) => Box::new(text.clone()),
        Some(other) => Box::new(other.to_string()),
    }
}

fn insert_carry_row(
    conn: &Connection,
    table: &str,
    columns: &[&str],
    row: &serde_json::Value,
) -> Result<(), VogtError> {
    let names = columns.join(", ");
    let placeholders = vec!["?"; columns.len()].join(", ");
    let params: Vec<Box<dyn rusqlite::ToSql>> = columns
        .iter()
        .map(|column| json_param(row, column))
        .collect();
    let refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|value| value.as_ref()).collect();
    conn.execute(
        &format!("INSERT INTO {table} ({names}) VALUES ({placeholders})"),
        refs.as_slice(),
    )
    .map(|_| ())
    .map_err(sql_err)
}

fn select_columns(
    conn: &Connection,
    table: &str,
    columns: &[&str],
) -> Result<Vec<serde_json::Value>, VogtError> {
    let names = columns.join(", ");
    many(conn, &format!("SELECT {names} FROM {table}"), [], |row| {
        let mut object = serde_json::Map::new();
        for (index, column) in columns.iter().enumerate() {
            let value: Option<String> = row.get(index)?;
            object.insert(
                (*column).to_string(),
                match value {
                    Some(text) => serde_json::Value::String(text),
                    None => serde_json::Value::Null,
                },
            );
        }
        Ok(serde_json::Value::Object(object))
    })
}

const TOKEN_SELECT: &str = "SELECT t.*, a.identity_ref AS actor_identity_ref FROM tokens t JOIN actors a ON a.id = t.actor_id";

fn row_token(row: &Row<'_>) -> rusqlite::Result<Token> {
    let scopes: String = row.get("scopes")?;
    let kind: String = row.get("kind")?;
    Ok(Token {
        id: row.get("id")?,
        actor_id: row.get("actor_id")?,
        actor_identity_ref: row.get("actor_identity_ref")?,
        name: row.get("name")?,
        scopes: serde_json::from_str(&scopes).map_err(|err| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(err))
        })?,
        kind: serde_json::from_value(serde_json::Value::String(kind))
            .unwrap_or(crate::core::TokenKind::Api),
        created_at: moment(row, "created_at")?,
        expires_at: opt_moment(row, "expires_at")?,
        last_used_at: opt_moment(row, "last_used_at")?,
        revoked_at: opt_moment(row, "revoked_at")?,
        revoked_reason: row.get("revoked_reason")?,
    })
}

const PASSWORD_SELECT: &str = "SELECT p.actor_id, p.username, p.scopes, p.created_at, p.updated_at, a.identity_ref AS actor_identity_ref FROM password_credentials p JOIN actors a ON a.id = p.actor_id";

fn row_password(row: &Row<'_>) -> rusqlite::Result<PasswordCredential> {
    let scopes: String = row.get("scopes")?;
    Ok(PasswordCredential {
        actor_id: row.get("actor_id")?,
        actor_identity_ref: row.get("actor_identity_ref")?,
        username: row.get("username")?,
        scopes: serde_json::from_str(&scopes).map_err(|err| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(err))
        })?,
        created_at: moment(row, "created_at")?,
        updated_at: moment(row, "updated_at")?,
    })
}

fn row_forge_account(row: &Row<'_>) -> rusqlite::Result<ForgeAccount> {
    Ok(ForgeAccount {
        actor_id: row.get("actor_id")?,
        host: row.get("host")?,
        login: row.get("login")?,
        scopes: row.get("scopes")?,
        created_at: moment(row, "created_at")?,
        updated_at: moment(row, "updated_at")?,
    })
}

const SUPPRESSION_SELECT: &str = "SELECT s.*, a.identity_ref AS actor_identity_ref, p.slug AS scope_project_slug FROM suppressions s JOIN actors a ON a.id = s.actor_id LEFT JOIN projects p ON p.id = s.scope_project_id";

fn row_suppression(row: &Row<'_>) -> rusqlite::Result<Suppression> {
    let match_kind: String = row.get("match_kind")?;
    Ok(Suppression {
        id: row.get("id")?,
        match_kind: serde_json::from_value(serde_json::Value::String(match_kind)).map_err(
            |err| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(err),
                )
            },
        )?,
        subject_key_or_pattern: row.get("subject_key_or_pattern")?,
        scope_project_id: row.get("scope_project_id")?,
        scope_project_slug: row.get("scope_project_slug")?,
        actor_id: row.get("actor_id")?,
        actor_identity_ref: row.get("actor_identity_ref")?,
        reason: row.get("reason")?,
        created_at: moment(row, "created_at")?,
        revoked_at: opt_moment(row, "revoked_at")?,
        revoked_reason: row.get("revoked_reason")?,
    })
}

const WORK_SELECT: &str = "SELECT w.*, p.slug AS project_slug, ac.identity_ref AS assignee_identity_ref FROM work_items w LEFT JOIN projects p ON p.id = w.project_id LEFT JOIN actors ac ON ac.id = w.assignee_actor_id";

const TERMINAL_STATES: [&str; 2] = ["done", "wont_do"];

/// The WHERE clause every work view filters through, so a count and the page
/// beside it describe the same set.
fn work_where(filter: &WorkFilter) -> (String, Vec<Box<dyn rusqlite::ToSql>>) {
    let mut clauses: Vec<String> = Vec::new();
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    if !filter.include_superseded {
        // A native row that migrated upstream is retired. The row stays
        // reachable by ref or id; only the lists leave it out.
        clauses.push("w.superseded_by IS NULL".into());
    }
    if filter.exclude_unlinked_native {
        // A native row on an unlinked project leaves the curated surfaces. A
        // project-less item stays: there is no project to be linked.
        clauses.push(
            "(w.project_id IS NULL OR EXISTS (SELECT 1 FROM projects lp WHERE lp.id = w.project_id AND lp.link_state = 'linked'))".into(),
        );
    }
    if let Some(project) = &filter.project_id {
        clauses.push("w.project_id = ?".into());
        params.push(Box::new(project.clone()));
    }
    if let Some(assignee) = &filter.assignee_actor_id {
        clauses.push("w.assignee_actor_id = ?".into());
        params.push(Box::new(assignee.clone()));
    }
    if let Some(initiative) = &filter.initiative_id {
        clauses.push("w.initiative_id = ?".into());
        params.push(Box::new(initiative.clone()));
    }
    for (column, values) in [
        ("w.kind", &filter.kinds),
        ("w.state", &filter.states),
        ("w.priority", &filter.priorities),
        ("w.trust_state", &filter.trust_states),
    ] {
        if !values.is_empty() {
            let placeholders = vec!["?"; values.len()].join(", ");
            clauses.push(format!("{column} IN ({placeholders})"));
            for value in values {
                params.push(Box::new(value.clone()));
            }
        }
    }
    if filter.exclude_terminal {
        let placeholders = vec!["?"; TERMINAL_STATES.len()].join(", ");
        clauses.push(format!("w.state NOT IN ({placeholders})"));
        for state in TERMINAL_STATES {
            params.push(Box::new(state.to_string()));
        }
    }
    if let Some(text) = &filter.text {
        // instr over lower rather than LIKE: the needle is caller text, and
        // LIKE would read its % and _ as wildcards.
        if !text.is_empty() {
            let needle = text.to_lowercase();
            clauses.push(
                "(instr(lower(w.title), ?) > 0 OR instr(lower(w.body), ?) > 0 OR instr(lower(w.ref), ?) > 0)".into(),
            );
            params.push(Box::new(needle.clone()));
            params.push(Box::new(needle.clone()));
            params.push(Box::new(needle));
        }
    }
    if let Some(label) = &filter.label {
        clauses.push(
            "EXISTS (SELECT 1 FROM work_item_labels wl JOIN labels l ON l.id = wl.label_id WHERE wl.work_item_id = w.id AND l.name = ?)".into(),
        );
        params.push(Box::new(label.clone()));
    }
    let where_sql = if clauses.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", clauses.join(" AND "))
    };
    (where_sql, params)
}

fn append_where(where_sql: &str, clause: &str) -> String {
    if where_sql.is_empty() {
        format!("WHERE {clause}")
    } else {
        format!("{where_sql} AND {clause}")
    }
}

fn with_board_high_water(
    filter: &WorkFilter,
    high_water: &(Moment, String),
) -> Result<(String, Vec<Box<dyn rusqlite::ToSql>>), VogtError> {
    let (where_sql, mut params) = work_where(filter);
    let moment = to_iso(high_water.0);
    params.push(Box::new(moment.clone()));
    params.push(Box::new(moment));
    params.push(Box::new(high_water.1.clone()));
    Ok((
        append_where(
            &where_sql,
            "(w.created_at < ? OR (w.created_at = ? AND w.ref <= ?))",
        ),
        params,
    ))
}

/// The lane expression, drawn only from the closed set the application names.
fn board_lane(lane_mode: &str) -> Result<&'static str, VogtError> {
    match lane_mode {
        "none" => Ok("''"),
        "project" => Ok("COALESCE(p.slug, '')"),
        "initiative" => Ok("COALESCE(w.initiative_id, '')"),
        other => Err(VogtError::InvalidRequest(format!(
            "unknown Board lane mode: {other}"
        ))),
    }
}

fn load_work_item(
    conn: &Connection,
    predicate: &str,
    key: &str,
) -> Result<Option<WorkItem>, VogtError> {
    let row = conn
        .query_row(
            &format!("{WORK_SELECT} WHERE {predicate}"),
            [key],
            row_work_base,
        )
        .optional()
        .map_err(sql_err)?;
    row.map(|base| finish_work_item(conn, base)).transpose()
}

struct WorkBase {
    item: WorkItem,
}

fn row_work_base(row: &Row<'_>) -> rusqlite::Result<WorkBase> {
    Ok(WorkBase {
        item: WorkItem {
            id: row.get("id")?,
            reference: row.get("ref")?,
            kind: row.get("kind")?,
            title: row.get("title")?,
            body: row.get("body")?,
            state: row.get("state")?,
            priority: row.get("priority")?,
            effort: row.get("effort")?,
            project_id: row.get("project_id")?,
            project_slug: row.get("project_slug")?,
            initiative_id: row.get("initiative_id")?,
            origin: row.get("origin")?,
            trust_state: row.get("trust_state")?,
            assignee_actor_id: row.get("assignee_actor_id")?,
            assignee_identity_ref: row.get("assignee_identity_ref")?,
            labels: Vec::new(),
            relations: Vec::new(),
            superseded_by: row.get("superseded_by")?,
            created_at: moment(row, "created_at")?,
            updated_at: moment(row, "updated_at")?,
        },
    })
}

fn finish_work_item(conn: &Connection, base: WorkBase) -> Result<WorkItem, VogtError> {
    let mut item = base.item;
    item.labels = many(conn, "SELECT l.name FROM labels l JOIN work_item_labels wl ON wl.label_id = l.id WHERE wl.work_item_id = ? ORDER BY l.name", params![item.id], |row| row.get(0))?;
    item.relations = many(conn, "SELECT r.kind, o.id AS related_id, o.ref AS related_ref, o.title AS related_title, o.state AS related_state FROM work_relations r JOIN work_items o ON o.id = r.related_id WHERE r.work_item_id = ? ORDER BY r.kind, o.ref", params![item.id], row_relation)?;
    Ok(item)
}

fn row_relation(row: &Row<'_>) -> rusqlite::Result<crate::core::Relation> {
    let kind: String = row.get("kind")?;
    Ok(crate::core::Relation {
        kind: serde_json::from_value(serde_json::Value::String(kind))
            .map_err(|e| rusqlite::Error::InvalidColumnName(e.to_string()))?,
        related_id: row.get("related_id")?,
        related_ref: row.get("related_ref")?,
        related_title: row.get("related_title")?,
        related_state: row.get("related_state")?,
    })
}

fn row_comment(row: &Row<'_>) -> rusqlite::Result<Comment> {
    Ok(Comment {
        id: row.get("id")?,
        work_item_id: row.get("work_item_id")?,
        actor_id: row.get("actor_id")?,
        actor_display_name: row.get("actor_display_name")?,
        body: row.get("body")?,
        created_at: moment(row, "created_at")?,
    })
}

fn row_event(row: &Row<'_>) -> rusqlite::Result<Event> {
    let summary: String = row.get("summary")?;
    Ok(Event {
        seq: row.get("seq")?,
        kind: row.get("kind")?,
        entity_kind: row.get("entity_kind")?,
        entity_id: row.get("entity_id")?,
        actor_id: row.get("actor_id")?,
        audit_id: row.get("audit_id")?,
        summary: serde_json::from_str(&summary).map_err(|err| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(err))
        })?,
        at: moment(row, "at")?,
    })
}

fn vocab_cell<T: serde::de::DeserializeOwned>(row: &Row<'_>, column: &str) -> rusqlite::Result<T> {
    let text: String = row.get(column)?;
    serde_json::from_value(serde_json::Value::String(text)).map_err(|err| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(err))
    })
}

fn opt_vocab_cell<T: serde::de::DeserializeOwned>(
    row: &Row<'_>,
    column: &str,
) -> rusqlite::Result<Option<T>> {
    let text: Option<String> = row.get(column)?;
    text.map(|value| {
        serde_json::from_value(serde_json::Value::String(value)).map_err(|err| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(err))
        })
    })
    .transpose()
}

fn row_overlay(row: &Row<'_>) -> rusqlite::Result<WorkOverlay> {
    let branches: Option<String> = row.get("branches")?;
    Ok(WorkOverlay {
        subject_key: row.get("subject_key")?,
        project_id: row.get("project_id")?,
        rank: row.get("rank")?,
        workflow_state: row.get("workflow_state")?,
        priority: opt_vocab_cell(row, "priority")?,
        effort: opt_vocab_cell(row, "effort")?,
        assignee_actor_id: row.get("assignee_actor_id")?,
        initiative_id: row.get("initiative_id")?,
        branches: branches
            .filter(|text| !text.is_empty())
            .map(|text| serde_json::from_str(&text))
            .transpose()
            .map_err(|err| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(err),
                )
            })?
            .unwrap_or_default(),
        created_at: moment(row, "created_at")?,
        updated_at: moment(row, "updated_at")?,
    })
}

fn row_writeback(row: &Row<'_>) -> rusqlite::Result<WriteBackRecord> {
    Ok(WriteBackRecord {
        id: row.get("id")?,
        at: moment(row, "at")?,
        project_id: row.get("project_id")?,
        work_item_id: row.get("work_item_id")?,
        actor_id: row.get("actor_id")?,
        action: vocab_cell::<WriteBackAction>(row, "action")?,
        subject_key: row.get("subject_key")?,
        policy: row.get("policy")?,
        outcome: vocab_cell::<WriteBackOutcome>(row, "outcome")?,
        reason: row.get("reason")?,
        detail: row.get("detail")?,
        source_url: row.get("source_url")?,
    })
}

fn row_audit(row: &Row<'_>) -> rusqlite::Result<AuditRecord> {
    Ok(AuditRecord {
        id: row.get("id")?,
        txn_id: row.get("txn_id")?,
        revision: row.get("revision")?,
        actor_id: row.get("actor_id")?,
        actor_identity_ref: row.get("actor_identity_ref")?,
        operation: row.get("operation")?,
        entity_kind: row.get("entity_kind")?,
        entity_id: row.get("entity_id")?,
        reason: row.get("reason")?,
        payload_digest: row.get("payload_digest")?,
        at: moment(row, "at")?,
    })
}

fn audit_where(query: &AuditQuery) -> (String, Vec<Box<dyn rusqlite::ToSql>>) {
    let mut clauses: Vec<String> = Vec::new();
    let mut params_box: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    if let Some(actor) = &query.actor_id {
        clauses.push("a.actor_id = ?".into());
        params_box.push(Box::new(actor.clone()));
    }
    if let Some(operation) = &query.operation {
        clauses.push("a.operation = ?".into());
        params_box.push(Box::new(operation.clone()));
    }
    if let Some(entity) = &query.entity_id {
        // A comment is audited against the comment, so an exact match on a
        // work item's id would omit everything said about it. The comment
        // table carries the link, so the trail is a semi-join. The id is
        // bound twice, once for the row and once for the semi-join.
        clauses.push(
            "(a.entity_id = ? OR (a.entity_kind = 'comment' AND a.entity_id IN (SELECT id FROM comments WHERE work_item_id = ?)))".into(),
        );
        params_box.push(Box::new(entity.clone()));
        params_box.push(Box::new(entity.clone()));
    }
    if let Some(project) = &query.project_id {
        // Nothing about a project is copied onto an audit row. Each kind that
        // belongs to a project is resolved through its own table, and a kind
        // that belongs to the instance (actor, label, token) is absent on
        // purpose. The project id is bound once per kind.
        let scoped = [
            ("project", "SELECT id FROM projects WHERE id = ?"),
            ("work_item", "SELECT id FROM work_items WHERE project_id = ?"),
            (
                "comment",
                "SELECT c.id FROM comments c JOIN work_items w ON w.id = c.work_item_id WHERE w.project_id = ?",
            ),
            ("session", "SELECT id FROM coding_sessions WHERE project_id = ?"),
            (
                "drift_proposal",
                "SELECT id FROM drift_proposals WHERE project_id = ?",
            ),
            (
                "suppression",
                "SELECT id FROM suppressions WHERE scope_project_id = ?",
            ),
        ];
        let joined = scoped
            .iter()
            .map(|(kind, resolver)| {
                format!("(a.entity_kind = '{kind}' AND a.entity_id IN ({resolver}))")
            })
            .collect::<Vec<_>>()
            .join(" OR ");
        clauses.push(format!("({joined})"));
        for _ in scoped {
            params_box.push(Box::new(project.clone()));
        }
    }
    if let Some(since) = query.since {
        clauses.push("a.at >= ?".into());
        params_box.push(Box::new(to_iso(since)));
    }
    if let Some(until) = query.until {
        clauses.push("a.at < ?".into());
        params_box.push(Box::new(to_iso(until)));
    }
    (
        if clauses.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", clauses.join(" AND "))
        },
        params_box,
    )
}

#[cfg(test)]
mod overlay_tests {
    use super::tests::{moment, project, store};
    use super::*;

    #[test]
    fn an_overlay_keeps_its_first_created_at_and_a_link_resolves_the_item() {
        let dir = std::env::temp_dir().join(format!("vogt-overlay-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = store(&dir);
        let now = moment();
        let later_at = Moment::from_unix(1_700_000_900, 0);
        let project = project(&store, "linked");
        let actor = store.read().unwrap().list_actors(1, 0).unwrap().remove(0);
        let mut txn = store.write().unwrap();
        let reference = txn.next_work_ref().unwrap();
        txn.insert_project(&project).unwrap();
        txn.insert_work_item(&WorkItem {
            id: "wrk_1".into(),
            reference: reference.clone(),
            kind: "bug".parse().unwrap(),
            title: "leaks".into(),
            body: String::new(),
            state: "open".into(),
            priority: "p2".parse().unwrap(),
            effort: None,
            project_id: Some(project.id.clone()),
            project_slug: None,
            initiative_id: None,
            origin: "created".parse().unwrap(),
            trust_state: "unverified".parse().unwrap(),
            assignee_actor_id: None,
            assignee_identity_ref: None,
            labels: Vec::new(),
            relations: Vec::new(),
            superseded_by: None,
            created_at: now,
            updated_at: now,
        })
        .unwrap();
        txn.insert_work_link(&WorkLink {
            work_item_id: "wrk_1".into(),
            subject_key: "gh:1".into(),
            origin_kind: "issue".into(),
            source_url: None,
            relation: "completion".parse().unwrap(),
            created_at: now,
        })
        .unwrap();
        let overlay = WorkOverlay {
            subject_key: "gh:1".into(),
            project_id: project.id.clone(),
            rank: Some(1.5),
            workflow_state: Some("triage".into()),
            priority: Some("p1".parse().unwrap()),
            effort: None,
            assignee_actor_id: Some(actor.id.clone()),
            initiative_id: None,
            branches: vec!["fix/leaks".into()],
            created_at: now,
            updated_at: now,
        };
        txn.upsert_work_overlay(&overlay).unwrap();
        txn.upsert_work_overlay(&WorkOverlay {
            updated_at: later_at,
            rank: Some(2.0),
            ..overlay.clone()
        })
        .unwrap();
        txn.insert_writeback(&WriteBackRecord {
            id: "wb_1".into(),
            at: now,
            project_id: Some(project.id.clone()),
            work_item_id: Some("wrk_1".into()),
            actor_id: actor.id.clone(),
            action: "comment".parse().unwrap(),
            subject_key: Some("gh:1".into()),
            policy: "on-demand".into(),
            outcome: "succeeded".parse().unwrap(),
            reason: "posted".into(),
            detail: None,
            source_url: None,
        })
        .unwrap();
        txn.commit().unwrap();

        let view = store.read().unwrap();
        let kept = view.work_overlay("gh:1").unwrap().unwrap();
        assert_eq!(kept.created_at, now);
        assert_eq!(kept.updated_at, later_at);
        assert_eq!(kept.rank, Some(2.0));
        assert_eq!(kept.priority, Some("p1".parse().unwrap()));
        assert_eq!(kept.branches, vec!["fix/leaks".to_string()]);
        assert_eq!(view.bound_branch_overlays(10).unwrap(), vec![kept.clone()]);
        let resolved = view.work_item_by_subject("gh:1").unwrap().unwrap();
        assert_eq!(resolved.reference, reference);
        assert_eq!(
            view.work_links_for_subjects(&["gh:1".into()])
                .unwrap()
                .get("gh:1")
                .map(String::as_str),
            Some(reference.as_str())
        );
        assert_eq!(
            view.work_links_for_subjects_by_item("wrk_1")
                .unwrap()
                .get("gh:1")
                .map(String::as_str),
            Some("issue")
        );
        let written = view.list_writeback_actions(Some("succeeded"), 10).unwrap();
        assert_eq!(written.len(), 1);
        assert!(view
            .list_writeback_actions(Some("failed"), 10)
            .unwrap()
            .is_empty());

        let event = store
            .publish_event(
                "observed",
                "work_item",
                "wrk_1",
                &serde_json::json!({"note": "seen"}),
                now,
            )
            .unwrap();
        assert_eq!(event.summary["note"], "seen");
        assert_eq!(event.actor_id, None);
        // Publishing draws a transaction id even though it writes no audit
        // row, so the next write's id counts it.
        let mut again = store.write().unwrap();
        let audit = again
            .append_audit(
                &actor,
                "note",
                "work_item",
                "wrk_1",
                "because",
                &"sha256:ab".repeat(32),
                now,
            )
            .unwrap();
        assert_eq!(audit.txn_id, "txn_0004");
        drop(again);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{InitiativeState, Moment, SequentialIds, StepClock};
    use crate::storage::interface::{DeclaredStore, ReadView, WriteTxn};

    pub(super) fn moment() -> Moment {
        Moment::from_unix(1_700_000_000, 0)
    }

    pub(super) fn store(dir: &Path) -> SqliteDeclaredStore<StepClock, SequentialIds> {
        let store = SqliteDeclaredStore::new(
            dir.join("declared.sqlite3"),
            StepClock::new(moment()),
            SequentialIds::new(None).unwrap(),
        );
        store.migrate().unwrap();
        let principal = Principal::new("local:test", ActorKind::Human, "Test").unwrap();
        store.bootstrap(&principal).unwrap();
        store
    }

    pub(super) fn project(
        store: &SqliteDeclaredStore<StepClock, SequentialIds>,
        slug: &str,
    ) -> Project {
        let now = moment();
        Project::new(
            &store
                .ids
                .lock()
                .expect("the shared clock and ids are not poisoned")
                .next("prj"),
            slug,
            slug,
            &format!("/srv/{slug}"),
            now,
        )
    }

    #[test]
    fn projects_are_listed_by_slug_with_paging() {
        let dir = std::env::temp_dir().join(format!("vogt-decl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = store(&dir);
        let made: Vec<_> = ["charlie", "alpha", "bravo"]
            .into_iter()
            .map(|slug| project(&store, slug))
            .collect();
        for item in &made {
            let mut txn = store.write().unwrap();
            txn.insert_project(item).unwrap();
            txn.commit().unwrap();
        }
        let view = store.read().unwrap();
        let page: Vec<_> = view
            .list_projects(2, 0)
            .unwrap()
            .into_iter()
            .map(|p| p.slug)
            .collect();
        assert_eq!(page, ["alpha", "bravo"]);
        let rest: Vec<_> = view
            .list_projects(2, 2)
            .unwrap()
            .into_iter()
            .map(|p| p.slug)
            .collect();
        assert_eq!(rest, ["charlie"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_stored_project_round_trips() {
        let dir = std::env::temp_dir().join(format!("vogt-decl2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = store(&dir);
        let original = project(&store, "round-trip");
        let mut txn = store.write().unwrap();
        txn.insert_project(&original).unwrap();
        txn.commit().unwrap();
        let loaded = store.read().unwrap().project_by_slug("round-trip").unwrap();
        assert_eq!(loaded.as_ref(), Some(&original));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn labels_initiatives_and_workflows_round_trip() {
        let dir = std::env::temp_dir().join(format!("vogt-decl3-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = store(&dir);
        let now = moment();
        let label = Label {
            id: store
                .ids
                .lock()
                .expect("the shared clock and ids are not poisoned")
                .next("lbl"),
            name: "backend".into(),
            color: Some("blue".into()),
            created_at: now,
        };
        let initiative = Initiative {
            id: store
                .ids
                .lock()
                .expect("the shared clock and ids are not poisoned")
                .next("ini"),
            slug: "north".into(),
            title: "North".into(),
            body: "the plan".into(),
            state: InitiativeState::Open,
            weight: 3,
            created_at: now,
            updated_at: now,
        };
        let mut txn = store.write().unwrap();
        txn.insert_label(&label).unwrap();
        txn.insert_initiative(&initiative).unwrap();
        let workflow = default_workflow("feature");
        txn.upsert_workflow(&workflow, now).unwrap();
        txn.commit().unwrap();
        let view = store.read().unwrap();
        assert_eq!(
            view.label_by_name("backend").unwrap().as_ref(),
            Some(&label)
        );
        assert_eq!(
            view.initiative_by_slug("north").unwrap().as_ref(),
            Some(&initiative)
        );
        // The stored definition is what round-trips; the in-memory default keeps
        // insertion order, which a sorted JSON object cannot.
        assert_eq!(
            view.workflow_for("feature").unwrap().to_definition_json(),
            workflow.to_definition_json()
        );
        // A kind never written by this transaction still has a row: migrate
        // seeds one per kind. Its stored definition is the shipped default.
        assert_eq!(
            view.workflow_for("question").unwrap().to_definition_json(),
            default_workflow("question").to_definition_json()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn redeclaring_a_contract_exemption_replaces_the_reason() {
        let dir = std::env::temp_dir().join(format!("vogt-decl4-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = store(&dir);
        let project = project(&store, "governed");
        let now = moment();
        let first = ContractExemption {
            id: "exc_1".into(),
            project_id: project.id.clone(),
            project_slug: None,
            rule: "readme".into(),
            target: "docs".into(),
            reason: "first look".into(),
            declared_by: "act".into(),
            declared_at: now,
        };
        let mut txn = store.write().unwrap();
        txn.insert_project(&project).unwrap();
        txn.insert_contract_exemption(&first).unwrap();
        txn.commit().unwrap();
        let again = ContractExemption {
            reason: "looked again".into(),
            ..first.clone()
        };
        let mut txn = store.write().unwrap();
        txn.insert_contract_exemption(&again).unwrap();
        txn.commit().unwrap();
        let rows = store
            .read()
            .unwrap()
            .contract_exemptions(&project.id)
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].reason, "looked again");
        assert_eq!(rows[0].project_slug.as_deref(), Some("governed"));
        let mut txn = store.write().unwrap();
        assert!(txn
            .delete_contract_exemption(&project.id, "readme", "docs")
            .unwrap());
        txn.commit().unwrap();
        assert!(store
            .read()
            .unwrap()
            .contract_exemptions(&project.id)
            .unwrap()
            .is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reading_before_bootstrap_is_refused() {
        let dir = std::env::temp_dir().join(format!("vogt-decl5-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = SqliteDeclaredStore::new(
            dir.join("declared.sqlite3"),
            StepClock::new(moment()),
            SequentialIds::new(None).unwrap(),
        );
        store.migrate().unwrap();
        let err = store.read().err().expect("read should fail");
        assert!(matches!(err, VogtError::NotInitialized(_)), "{err:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_dropped_write_rolls_back() {
        let dir = std::env::temp_dir().join(format!("vogt-decl6-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = store(&dir);
        let project = project(&store, "ephemeral");
        let txn = store.write().unwrap();
        // insert then drop without commit
        drop(txn);
        let mut txn = store.write().unwrap();
        txn.insert_project(&project).unwrap();
        drop(txn);
        assert!(store
            .read()
            .unwrap()
            .project_by_slug("ephemeral")
            .unwrap()
            .is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
#[cfg(test)]
mod more {
    use super::tests::{moment, project, store};
    use super::*;
    #[test]
    fn a_work_item_round_trips_with_its_label_relation_and_comment() {
        let dir = std::env::temp_dir().join(format!("vogt-work-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = store(&dir);
        let now = moment();
        let label = Label {
            id: "lbl_1".into(),
            name: "backend".into(),
            color: None,
            created_at: now,
        };
        let project = project(&store, "governed");
        let mut first = store.write().unwrap();
        let reference = first.next_work_ref().unwrap();
        first.insert_project(&project).unwrap();
        first.insert_label(&label).unwrap();
        first.commit().unwrap();
        let item = WorkItem {
            id: "wrk_1".into(),
            reference,
            kind: "bug".parse().unwrap(),
            title: "leaks".into(),
            body: String::new(),
            state: "open".into(),
            priority: "p1".parse().unwrap(),
            effort: None,
            project_id: Some(project.id.clone()),
            project_slug: None,
            initiative_id: None,
            origin: "created".parse().unwrap(),
            trust_state: "unverified".parse().unwrap(),
            assignee_actor_id: None,
            assignee_identity_ref: None,
            labels: vec!["backend".into()],
            relations: Vec::new(),
            superseded_by: None,
            created_at: now,
            updated_at: now,
        };
        let other = WorkItem {
            id: "wrk_2".into(),
            reference: String::new(),
            title: "other".into(),
            labels: Vec::new(),
            ..item.clone()
        };
        let mut txn = store.write().unwrap();
        let other_ref = txn.next_work_ref().unwrap();
        let other = WorkItem {
            reference: other_ref,
            ..other
        };
        txn.insert_work_item(&item).unwrap();
        txn.insert_work_item(&other).unwrap();
        txn.insert_relation(
            &item.id,
            &other.id,
            crate::core::RelationKind::DependsOn,
            now,
        )
        .unwrap();
        let actor = store.read().unwrap().list_actors(1, 0).unwrap().remove(0);
        txn.insert_comment(&Comment {
            id: "cmt_1".into(),
            work_item_id: item.id.clone(),
            actor_id: actor.id.clone(),
            actor_display_name: String::new(),
            body: "noted".into(),
            created_at: now,
        })
        .unwrap();
        let audit = txn
            .append_audit(
                &actor,
                "work.create",
                "work_item",
                &item.id,
                "filed",
                &format!("sha256:{}", "ab".repeat(32)),
                now,
            )
            .unwrap();
        let event = txn
            .append_event(
                "created",
                "work_item",
                &item.id,
                Some(&actor.id),
                Some(&audit.id),
                &serde_json::json!({"verb": "created"}),
                now,
            )
            .unwrap();
        txn.commit().unwrap();

        let view = store.read().unwrap();
        let loaded = view.work_item_by_id(&item.id).unwrap().unwrap();
        assert_eq!(loaded.reference, "WI-1");
        assert_eq!(loaded.project_slug.as_deref(), Some("governed"));
        assert_eq!(loaded.labels, ["backend".to_string()]);
        assert_eq!(loaded.relations.len(), 1);
        assert_eq!(loaded.relations[0].related_ref, "WI-2");
        assert_eq!(
            view.comments_for(&item.id, 10).unwrap()[0].actor_display_name,
            actor.display_name
        );
        // The store, not the caller, owns the transaction id and revision.
        assert_eq!(
            audit.txn_id,
            view.list_audit(&AuditQuery {
                limit: 10,
                offset: 0,
                actor_id: None,
                operation: Some("work.create".into()),
                entity_id: None,
                project_id: None,
                since: None,
                until: None
            })
            .unwrap()[0]
                .txn_id
        );
        assert_eq!(
            view.count_audit(&AuditQuery {
                limit: 0,
                offset: 0,
                actor_id: None,
                operation: None,
                entity_id: Some(item.id.clone()),
                project_id: None,
                since: None,
                until: None
            })
            .unwrap(),
            1
        );
        assert_eq!(event.seq, 1);
        assert_eq!(view.latest_event_seq().unwrap(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_dropped_work_item_write_does_not_burn_a_reference() {
        let dir = std::env::temp_dir().join(format!("vogt-ref-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = store(&dir);
        let txn = store.write().unwrap();
        assert_eq!(txn.revision(), 1);
        drop(txn);
        let mut txn = store.write().unwrap();
        assert_eq!(txn.next_work_ref().unwrap(), "WI-1");
        drop(txn);
        let mut txn = store.write().unwrap();
        assert_eq!(txn.next_work_ref().unwrap(), "WI-1");
        txn.commit().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn work_lists_share_a_filter_and_the_board_pages_one_cell() {
        let dir = std::env::temp_dir().join(format!("vogt-decl-board-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = store(&dir);
        let now = Moment::from_unix(1_700_000_000, 0);
        let later_at = Moment::from_unix(1_700_086_400, 0);
        let project = project(&store, "governed");
        let mut txn = store.write().unwrap();
        txn.insert_project(&project).unwrap();
        let mut make = |id: &str, state: &str, at: Moment| {
            let reference = txn.next_work_ref().unwrap();
            txn.insert_work_item(&WorkItem {
                id: id.into(),
                reference,
                kind: "bug".parse().unwrap(),
                title: "leaks".into(),
                body: String::new(),
                state: state.into(),
                priority: "p2".parse().unwrap(),
                effort: None,
                project_id: Some(project.id.clone()),
                project_slug: None,
                initiative_id: None,
                origin: "created".parse().unwrap(),
                trust_state: "unverified".parse().unwrap(),
                assignee_actor_id: None,
                assignee_identity_ref: None,
                labels: Vec::new(),
                relations: Vec::new(),
                superseded_by: None,
                created_at: at,
                updated_at: at,
            })
            .unwrap();
        };
        make("wrk_1", "open", now);
        make("wrk_2", "open", later_at);
        make("wrk_3", "done", now);
        // wrk_1 depends on wrk_2 (still open) and on wrk_3 (done), so exactly
        // one of the two targets is an unfinished blocker.
        txn.insert_relation("wrk_1", "wrk_2", crate::core::RelationKind::DependsOn, now)
            .unwrap();
        txn.insert_relation("wrk_1", "wrk_3", crate::core::RelationKind::DependsOn, now)
            .unwrap();
        let actor = store.read().unwrap().list_actors(1, 0).unwrap().remove(0);
        let actor_id = actor.id.clone();
        txn.insert_suppression(&Suppression {
            id: "sup_1".into(),
            match_kind: crate::core::MatchKind::Exact,
            subject_key_or_pattern: "subj".into(),
            scope_project_id: None,
            scope_project_slug: None,
            actor_id: actor.id.clone(),
            actor_identity_ref: None,
            reason: "noise".into(),
            created_at: now,
            revoked_at: None,
            revoked_reason: None,
        })
        .unwrap();
        txn.insert_suppression(&Suppression {
            id: "sup_2".into(),
            match_kind: crate::core::MatchKind::Pattern,
            subject_key_or_pattern: "subj-*".into(),
            scope_project_id: None,
            scope_project_slug: None,
            actor_id: actor_id.clone(),
            actor_identity_ref: None,
            reason: "more noise".into(),
            created_at: later_at,
            revoked_at: None,
            revoked_reason: None,
        })
        .unwrap();
        assert!(txn
            .revoke_suppression("sup_2", &actor_id, "settled", later_at)
            .unwrap());
        txn.commit().unwrap();

        let view = store.read().unwrap();
        let open_only = WorkFilter {
            states: vec!["open".into()],
            ..WorkFilter::default()
        };
        let listed = view.list_work_items(&open_only).unwrap();
        // Oldest first, so the earlier item leads even though it was inserted
        // in the same order.
        assert_eq!(
            listed
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            ["wrk_1", "wrk_2"]
        );
        assert_eq!(view.count_work_items(&open_only).unwrap(), 2);
        assert_eq!(view.count_work_items(&WorkFilter::default()).unwrap(), 3);

        let high_water = view.board_high_water(&WorkFilter::default()).unwrap();
        let counts = view
            .board_counts(&WorkFilter::default(), "none", high_water.as_ref())
            .unwrap();
        assert_eq!(counts.get(&("".into(), "open".into())), Some(&2));
        let cells = [BoardCellQuery {
            lane_key: String::new(),
            state: "open".into(),
            after_created_at: Some(now),
            after_ref: Some("WI-1".into()),
        }];
        let page = view
            .board_work_items(
                &WorkFilter::default(),
                "none",
                &cells,
                high_water.as_ref(),
                10,
            )
            .unwrap();
        let open = &page[&("".into(), "open".into())];
        assert_eq!(open.len(), 1, "the cursor skips the first open item");
        assert_eq!(open[0].id, "wrk_2");

        let fan = view
            .blocking_fan_out(&["wrk_2".into(), "wrk_3".into()])
            .unwrap();
        assert_eq!(fan.get("wrk_2"), Some(&1));
        let blockers = view
            .unfinished_blockers("wrk_1", &["done", "wont_do"])
            .unwrap();
        assert_eq!(blockers.len(), 1);
        assert_eq!(blockers[0].r#ref, "WI-2");

        let live = view.list_suppressions(false, 100).unwrap();
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].id, "sup_1");
        assert_eq!(view.list_suppressions(true, 100).unwrap().len(), 2);
        assert!(view
            .suppression_by_id("sup_2")
            .unwrap()
            .unwrap()
            .revoked_at
            .is_some());
        assert!(view.suppression_by_id("sup_missing").unwrap().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tokens_passwords_and_forge_accounts_round_trip() {
        let dir = std::env::temp_dir().join(format!("vogt-decl-creds-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = store(&dir);
        let now = Moment::from_unix(1_700_000_000, 0);
        let actor = store.read().unwrap().list_actors(1, 0).unwrap().remove(0);
        let mut txn = store.write().unwrap();
        txn.insert_token(
            &Token {
                id: "tok_1".into(),
                actor_id: actor.id.clone(),
                actor_identity_ref: None,
                name: "laptop".into(),
                scopes: vec!["read".into(), "write".into()],
                kind: crate::core::TokenKind::Api,
                created_at: now,
                expires_at: None,
                last_used_at: None,
                revoked_at: None,
                revoked_reason: None,
            },
            "hash-1",
        )
        .unwrap();
        txn.commit().unwrap();

        let view = store.read().unwrap();
        let token = view.token_by_hash("hash-1").unwrap().unwrap();
        assert_eq!(token.scopes, ["read", "write"]);
        assert_eq!(view.token_by_id("tok_1").unwrap().unwrap().name, "laptop");
        assert_eq!(view.list_tokens(false, 100).unwrap().len(), 1);

        let mut txn = store.write().unwrap();
        assert!(txn.revoke_token("tok_1", "lost", now).unwrap());
        assert!(!txn.revoke_token("tok_1", "again", now).unwrap());
        txn.commit().unwrap();
        let view = store.read().unwrap();
        assert!(view.list_tokens(false, 100).unwrap().is_empty());
        assert_eq!(view.list_tokens(true, 100).unwrap().len(), 1);
        assert_eq!(view.tokens_for_actor(&actor.id, true).unwrap().len(), 1);

        let mut txn = store.write().unwrap();
        txn.upsert_password_credential(&actor.id, "ada", "hash", &["admin".into()], now)
            .unwrap();
        txn.upsert_forge_account(&actor.id, "git.example", "ada", "repo", "cipher", now)
            .unwrap();
        txn.commit().unwrap();
        let view = store.read().unwrap();
        assert_eq!(
            view.password_credential_by_username("ada")
                .unwrap()
                .unwrap()
                .actor_id,
            actor.id
        );
        assert_eq!(
            view.password_hash(&actor.id).unwrap().as_deref(),
            Some("hash")
        );
        assert_eq!(
            view.forge_account_secret(&actor.id, "git.example")
                .unwrap()
                .as_deref(),
            Some("cipher")
        );
        let created = view
            .forge_account(&actor.id, "git.example")
            .unwrap()
            .unwrap()
            .created_at;

        // Re-linking rotates the secret but keeps the original created_at.
        let later_at = Moment::from_unix(1_700_086_400, 0);
        let mut txn = store.write().unwrap();
        txn.upsert_forge_account(
            &actor.id,
            "git.example",
            "ada",
            "repo",
            "cipher-2",
            later_at,
        )
        .unwrap();
        txn.commit().unwrap();
        let view = store.read().unwrap();
        let account = view
            .forge_account(&actor.id, "git.example")
            .unwrap()
            .unwrap();
        assert_eq!(account.created_at, created);
        assert_eq!(account.updated_at, later_at);
        assert_eq!(
            view.forge_account_secret(&actor.id, "git.example")
                .unwrap()
                .as_deref(),
            Some("cipher-2")
        );

        let mut txn = store.write().unwrap();
        assert!(txn.delete_password_credential(&actor.id).unwrap());
        assert!(txn.delete_forge_account(&actor.id, "git.example").unwrap());
        txn.commit().unwrap();
        let view = store.read().unwrap();
        assert!(view
            .password_credential_for_actor(&actor.id)
            .unwrap()
            .is_none());
        assert!(view.forge_accounts_for_actor(&actor.id).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn carrying_credentials_keeps_the_secret_and_revokes_the_rest() {
        let now = Moment::from_unix(1_700_000_000, 0);
        let source_dir = std::env::temp_dir().join(format!("vogt-decl-src-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&source_dir);
        let source = store(&source_dir);
        let source_actor = source.read().unwrap().list_actors(1, 0).unwrap().remove(0);
        let mut txn = source.write().unwrap();
        txn.insert_token(
            &Token {
                id: "tok_src".into(),
                actor_id: source_actor.id.clone(),
                actor_identity_ref: None,
                name: "laptop".into(),
                scopes: vec!["read".into()],
                kind: crate::core::TokenKind::Api,
                created_at: now,
                expires_at: None,
                last_used_at: None,
                revoked_at: None,
                revoked_reason: None,
            },
            "shared-secret",
        )
        .unwrap();
        txn.upsert_password_credential(&source_actor.id, "ada", "pw-hash", &["admin".into()], now)
            .unwrap();
        txn.commit().unwrap();
        let carried = source.credentials().unwrap();
        assert_eq!(carried.tokens.len(), 1);
        assert_eq!(carried.actors.len(), 1);

        let copy_dir = std::env::temp_dir().join(format!("vogt-decl-copy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&copy_dir);
        let copy = store(&copy_dir);
        let copy_actor = copy.read().unwrap().list_actors(1, 0).unwrap().remove(0);
        let mut txn = copy.write().unwrap();
        // A token the copy minted itself, which the carried set does not hold.
        txn.insert_token(
            &Token {
                id: "tok_local".into(),
                actor_id: copy_actor.id.clone(),
                actor_identity_ref: None,
                name: "stray".into(),
                scopes: vec!["read".into()],
                kind: crate::core::TokenKind::Api,
                created_at: now,
                expires_at: None,
                last_used_at: None,
                revoked_at: None,
                revoked_reason: None,
            },
            "local-secret",
        )
        .unwrap();
        let report = txn.carry_credentials(&carried, "cloned", now).unwrap();
        txn.commit().unwrap();
        assert_eq!(report.tokens_kept, 1);
        assert_eq!(report.source_tokens_revoked, 1);
        assert_eq!(report.password_logins_kept, 1);

        let view = copy.read().unwrap();
        // The shared secret survives, under the copy's own actor.
        let kept = view.token_by_hash("shared-secret").unwrap().unwrap();
        assert!(kept.revoked_at.is_none());
        assert_eq!(kept.actor_id, copy_actor.id);
        // The copy's own token is revoked.
        assert!(view
            .token_by_id("tok_local")
            .unwrap()
            .unwrap()
            .revoked_at
            .is_some());
        assert_eq!(
            view.password_hash(&copy_actor.id).unwrap().as_deref(),
            Some("pw-hash")
        );
        let _ = std::fs::remove_dir_all(&source_dir);
        let _ = std::fs::remove_dir_all(&copy_dir);
    }

    #[test]
    fn auth_decisions_prune_by_outcome_and_a_touch_never_revives_a_token() {
        let dir = std::env::temp_dir().join(format!("vogt-decl-auth-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = store(&dir);
        let early = Moment::from_unix(1_700_000_000, 0);
        let later_at = Moment::from_unix(1_700_086_400, 0);
        let actor = store.read().unwrap().list_actors(1, 0).unwrap().remove(0);
        let decision = |id: &str, outcome: crate::core::AuthOutcome, at: Moment| AuthDecision {
            id: id.into(),
            at,
            decision: outcome,
            reason_code: "ok".into(),
            operation: "work.list".into(),
            scope: None,
            actor_id: Some(actor.id.clone()),
            token_id: None,
            identity_ref: None,
            transport: "http".into(),
            detail: None,
        };
        store
            .record_auth_decision(&decision("dec_1", crate::core::AuthOutcome::Allow, early))
            .unwrap();
        store
            .record_auth_decision(&decision("dec_2", crate::core::AuthOutcome::Deny, early))
            .unwrap();
        store
            .record_auth_decision(&decision(
                "dec_3",
                crate::core::AuthOutcome::Allow,
                later_at,
            ))
            .unwrap();
        let view = store.read().unwrap();
        assert_eq!(view.list_auth_decisions(None, 100).unwrap().len(), 3);
        assert_eq!(
            view.list_auth_decisions(Some("deny"), 100).unwrap().len(),
            1
        );
        drop(view);

        // The horizon sits between the two stamps, so the early allow goes and
        // the early deny stays.
        let removed = store
            .prune_auth_decisions(later_at, Moment::from_unix(1_600_000_000, 0))
            .unwrap();
        assert_eq!(removed, 1);
        let view = store.read().unwrap();
        let remaining = view.list_auth_decisions(None, 100).unwrap();
        assert_eq!(remaining.len(), 2);
        assert!(remaining.iter().all(|row| row.id != "dec_1"));
        drop(view);

        let mut txn = store.write().unwrap();
        txn.insert_token(
            &Token {
                id: "tok_1".into(),
                actor_id: actor.id,
                actor_identity_ref: None,
                name: "session".into(),
                scopes: vec!["read".into()],
                kind: crate::core::TokenKind::Session,
                created_at: early,
                expires_at: Some(later_at),
                last_used_at: None,
                revoked_at: None,
                revoked_reason: None,
            },
            "hash-1",
        )
        .unwrap();
        assert!(txn.revoke_token("tok_1", "lost", early).unwrap());
        txn.commit().unwrap();
        // A renewal racing the revocation must not resurrect the token.
        let renewed = Moment::from_unix(1_800_000_000, 0);
        store.touch_token("tok_1", later_at, Some(renewed)).unwrap();
        let token = store.read().unwrap().token_by_id("tok_1").unwrap().unwrap();
        assert_eq!(token.expires_at, Some(later_at));
        assert_eq!(token.last_used_at, Some(later_at));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_persons_credential_latches_install_closed_and_an_agents_does_not() {
        let dir = std::env::temp_dir().join(format!("vogt-decl-latch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = store(&dir);
        let now = Moment::from_unix(1_700_000_000, 0);
        let latched = || -> bool {
            rusqlite::Connection::open(dir.join("declared.sqlite3"))
                .unwrap()
                .query_row("SELECT COUNT(*) FROM install_latch", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap()
                > 0
        };
        let mut txn = store.write().unwrap();
        txn.insert_actor(&Actor {
            id: "act_agent".into(),
            kind: crate::core::ActorKind::Agent,
            display_name: "stack".into(),
            identity_ref: "agent:stack".into(),
            disabled: false,
            created_at: now,
        })
        .unwrap();
        txn.insert_token(
            &Token {
                id: "tok_agent".into(),
                actor_id: "act_agent".into(),
                actor_identity_ref: None,
                name: "stack".into(),
                scopes: vec!["read".into()],
                kind: crate::core::TokenKind::Agent,
                created_at: now,
                expires_at: None,
                last_used_at: None,
                revoked_at: None,
                revoked_reason: None,
            },
            "agent-secret",
        )
        .unwrap();
        txn.commit().unwrap();
        assert!(!latched(), "an agent token leaves install mode open");
        assert!(!store.read().unwrap().install_closed().unwrap());

        let person = store.read().unwrap().list_actors(1, 0).unwrap().remove(0);
        let mut txn = store.write().unwrap();
        txn.upsert_password_credential(&person.id, "ada", "hash", &["admin".into()], now)
            .unwrap();
        txn.commit().unwrap();
        assert!(latched(), "a person's login closes it");
        assert!(store.read().unwrap().install_closed().unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn drift_inbox_and_preferences_round_trip() {
        let dir = std::env::temp_dir().join(format!("vogt-decl-drift-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = store(&dir);
        let now = Moment::from_unix(1_700_000_000, 0);
        let later_at = Moment::from_unix(1_700_086_400, 0);
        let actor = store.read().unwrap().list_actors(1, 0).unwrap().remove(0);
        let mut txn = store.write().unwrap();
        txn.insert_drift(&DriftProposal {
            id: "dft_1".into(),
            kind: "stale".into(),
            subject_kind: "work_item".into(),
            subject_id: "wrk_1".into(),
            project_id: None,
            project_slug: None,
            summary: "behind".into(),
            evidence_observation_id: Some("obs_1".into()),
            evidence_snapshot: serde_json::json!({"n": 1}),
            proposed_change: serde_json::json!({"state": "done"}),
            status: crate::core::DriftStatus::Open,
            opened_at: now,
            superseded_at: None,
            superseded_detail: None,
            resolved_by_actor_id: None,
            resolved_by_identity_ref: None,
            resolved_at: None,
            resolution_reason: None,
        })
        .unwrap();
        txn.upsert_actor_preference(&ActorPreference {
            actor_id: actor.id.clone(),
            key: "theme".into(),
            value: serde_json::json!({"mode": "dark"}),
            version: 1,
            updated_at: now,
        })
        .unwrap();
        txn.upsert_inbox_triage(&InboxTriage {
            entry_key: "entry-1".into(),
            state: crate::core::TriageState::Snoozed,
            snooze_until: Some(later_at),
            actor_id: actor.id.clone(),
            actor_identity_ref: None,
            decided_at: now,
            occurrence_snapshot: serde_json::json!({"count": 2}),
        })
        .unwrap();
        txn.commit().unwrap();

        let view = store.read().unwrap();
        let open = view.list_drift(Some("open"), None, None, 100).unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].evidence_snapshot["n"], 1);
        assert!(view.open_drift_subjects().unwrap().contains(&(
            "stale".into(),
            "work_item".into(),
            "wrk_1".into()
        )));
        assert!(view.drift_evidence_ids().unwrap().contains("obs_1"));
        assert_eq!(
            view.actor_preference(&actor.id, "theme")
                .unwrap()
                .unwrap()
                .version,
            1
        );
        assert_eq!(
            view.inbox_triage_by_key("entry-1")
                .unwrap()
                .unwrap()
                .snooze_until,
            Some(later_at)
        );
        drop(view);

        let mut txn = store.write().unwrap();
        assert!(txn
            .mark_drift_superseded("dft_1", Some("newer sweep"), Some(later_at))
            .unwrap());
        assert!(txn
            .resolve_drift("dft_1", "accepted", &actor.id, "agreed", later_at)
            .unwrap());
        // A resolved proposal is history: flagging it again changes nothing.
        assert!(!txn.mark_drift_superseded("dft_1", None, None).unwrap());
        txn.upsert_actor_preference(&ActorPreference {
            actor_id: actor.id.clone(),
            key: "theme".into(),
            value: serde_json::json!({"mode": "light"}),
            version: 2,
            updated_at: later_at,
        })
        .unwrap();
        txn.commit().unwrap();

        let view = store.read().unwrap();
        assert!(view
            .list_drift(Some("open"), None, None, 100)
            .unwrap()
            .is_empty());
        let resolved = view.drift_by_id("dft_1").unwrap().unwrap();
        assert_eq!(
            resolved.resolved_by_actor_id.as_deref(),
            Some(actor.id.as_str())
        );
        assert_eq!(view.actor_preferences(&actor.id).unwrap()[0].version, 2);
        assert_eq!(view.list_inbox_triage(100).unwrap().len(), 1);
        drop(view);

        // A value that is not an object reads back as an empty one.
        let conn = rusqlite::Connection::open(dir.join("declared.sqlite3")).unwrap();
        conn.execute(
            "UPDATE actor_preferences SET value = '[1]' WHERE actor_id = ? AND key = 'theme'",
            [&actor.id],
        )
        .unwrap();
        drop(conn);
        let value = store
            .read()
            .unwrap()
            .actor_preference(&actor.id, "theme")
            .unwrap()
            .unwrap()
            .value;
        assert_eq!(value, serde_json::json!({}));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sessions_stop_once_and_a_grant_round_trips() {
        let dir = std::env::temp_dir().join(format!("vogt-decl-sess-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = store(&dir);
        let now = Moment::from_unix(1_700_000_000, 0);
        let later_at = Moment::from_unix(1_700_086_400, 0);
        let project = project(&store, "governed");
        let actor = store.read().unwrap().list_actors(1, 0).unwrap().remove(0);
        let mut txn = store.write().unwrap();
        txn.insert_project(&project).unwrap();
        txn.insert_session(&CodingSession {
            id: "ses_1".into(),
            engine_session_id: "eng_1".into(),
            project_id: project.id.clone(),
            work_item_id: None,
            actor_id: actor.id.clone(),
            cwd: "/srv/governed".into(),
            template: None,
            model: Some("sonnet".into()),
            effort: None,
            reason: "look".into(),
            started_at: now,
            stopped_at: None,
        })
        .unwrap();
        txn.insert_session_grant(&SessionGrant {
            id: "grt_1".into(),
            target_engine_session_id: "eng_1".into(),
            kind: crate::core::GrantKind::Credential,
            var: Some("GITHUB_TOKEN".into()),
            project_id: Some(project.id.clone()),
            secret_name: Some("github".into()),
            capability: None,
            uses: crate::core::GrantUses::Once,
            ttl_seconds: 600,
            reason: "push".into(),
            requested_by: actor.id.clone(),
            requested_at: now,
            state: crate::core::GrantState::Pending,
            decided_by: None,
            decided_at: None,
            decision_reason: None,
            expires_at: None,
            revoked_by: None,
            revoked_at: None,
        })
        .unwrap();
        txn.commit().unwrap();

        let view = store.read().unwrap();
        assert_eq!(
            view.list_sessions(None, None, false, 100, 0).unwrap().len(),
            1
        );
        assert_eq!(
            view.session_by_engine_id("eng_1")
                .unwrap()
                .unwrap()
                .model
                .as_deref(),
            Some("sonnet")
        );
        assert_eq!(
            view.list_session_grants(Some("pending"), None, 100)
                .unwrap()
                .len(),
            1
        );
        drop(view);

        let mut txn = store.write().unwrap();
        txn.mark_session_stopped("ses_1", later_at).unwrap();
        // A second stop keeps the first time.
        txn.mark_session_stopped("ses_1", Moment::from_unix(1_800_000_000, 0))
            .unwrap();
        let mut grant = store
            .read()
            .unwrap()
            .session_grant("grt_1")
            .unwrap()
            .unwrap();
        grant.state = crate::core::GrantState::Approved;
        grant.decided_by = Some(actor.id.clone());
        grant.decided_at = Some(later_at);
        grant.expires_at = Some(Moment::from_unix(1_700_000_600, 0));
        txn.update_session_grant(&grant).unwrap();
        txn.commit().unwrap();

        let view = store.read().unwrap();
        assert!(view
            .list_sessions(None, None, false, 100, 0)
            .unwrap()
            .is_empty());
        let stopped = view.session_by_id("ses_1").unwrap().unwrap();
        assert_eq!(stopped.stopped_at, Some(later_at));
        let grant = view.session_grant("grt_1").unwrap().unwrap();
        assert_eq!(grant.state, crate::core::GrantState::Approved);
        assert_eq!(
            grant.effective_state(Moment::from_unix(1_700_000_700, 0)),
            "expired"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
