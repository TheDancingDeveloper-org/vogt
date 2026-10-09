//! Declared SQLite store, first half. Ports the project, actor, label,
//! workflow, initiative and contract parts of
//! `src/vogt/storage/sqlite/declared.py`.
//!
//! SQL stays next to the Python so ordering and the `ON CONFLICT` sites match.
//! Methods owned by later store chunks return `VogtError::InvalidRequest`
//! naming the chunk, rather than a guessed row.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OptionalExtension, Row};

use crate::core::{
    default_workflow, from_iso, to_iso, Actor, ActorKind, ActorPreference, AuditRecord,
    AuthDecision, Clock, CodingSession, Comment, ContractExemption, DriftProposal, Event,
    ForgeAccount, IdFactory, InboxTriage, Initiative, InitiativeState, Label, Moment,
    PasswordCredential, Principal, Project, RelationKind, SessionGrant, Suppression, Token,
    WorkItem, WorkLink, WorkOverlay, Workflow, WriteBackRecord,
};
use crate::errors::VogtError;
use crate::storage::interface::{
    AuditQuery, Blocker, BoardCellQuery, BootstrapResult, CarriedCredentials, CarryReport,
    CloneStamp, Counts, DeclaredStore, MigrationReport, ProjectUpdate, ReadView, WorkFilter,
    WorkItemUpdate, WriteTxn,
};
use crate::storage::sqlite::connection::{connect_with, DEFAULT_SYNCHRONOUS};
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

fn later<T>(name: &str) -> Result<T, VogtError> {
    Err(VogtError::InvalidRequest(format!(
        "{name} belongs to a later declared-store chunk"
    )))
}

fn sql_err(err: rusqlite::Error) -> VogtError {
    VogtError::InvalidRequest(err.to_string())
}

pub struct SqliteDeclaredStore<C, I> {
    path: PathBuf,
    clock: std::cell::RefCell<C>,
    ids: std::cell::RefCell<I>,
    synchronous: String,
}

#[allow(dead_code)]
impl<C, I> SqliteDeclaredStore<C, I>
where
    C: Clock,
    I: IdFactory,
{
    pub fn new(path: PathBuf, clock: C, ids: I) -> Self {
        Self {
            path,
            clock: std::cell::RefCell::new(clock),
            ids: std::cell::RefCell::new(ids),
            synchronous: DEFAULT_SYNCHRONOUS.to_string(),
        }
    }

    fn open(&self, create: bool) -> Result<Connection, VogtError> {
        connect_with(&self.path, create, &self.synchronous).map_err(sql_err)
    }
}

impl<C, I> DeclaredStore for SqliteDeclaredStore<C, I>
where
    C: Clock,
    I: IdFactory,
{
    type Read<'a>
        = SqliteReadView
    where
        Self: 'a;
    type Write<'a>
        = SqliteWrite
    where
        Self: 'a;

    fn migrate(&self) -> Result<MigrationReport, VogtError> {
        let mut conn = self.open(true)?;
        migrator::migrate(
            &mut conn,
            "declared",
            migrations_root().as_deref(),
            "vogt-core",
            &to_iso(crate::core::utc_now()),
        )
        .map_err(VogtError::from)
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
        let now = self.clock.borrow_mut().now();
        let instance_id = self.ids.borrow_mut().next("ins");
        let actor_id = self.ids.borrow_mut().next("act");
        let audit_id = self.ids.borrow_mut().next("aud");
        let txn_id = self.ids.borrow_mut().next("txn");
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
        later("credentials")
    }
    fn record_auth_decision(&self, _: &AuthDecision) -> Result<(), VogtError> {
        later("record_auth_decision")
    }
    fn touch_token(&self, _: &str, _: Moment, _: Option<Moment>) -> Result<(), VogtError> {
        later("touch_token")
    }
    fn prune_auth_decisions(&self, _: Moment, _: Moment) -> Result<i64, VogtError> {
        later("prune_auth_decisions")
    }
    fn publish_event(
        &self,
        _: &str,
        _: &str,
        _: &str,
        _: &serde_json::Value,
        _: Moment,
    ) -> Result<Event, VogtError> {
        later("publish_event")
    }

    fn read(&self) -> Result<Self::Read<'_>, VogtError> {
        if !self.is_initialized() {
            return Err(VogtError::NotInitialized(format!(
                "no instance at {}",
                self.path.display()
            )));
        }
        Ok(SqliteReadView {
            conn: self.open(false)?,
            workflow_cache: std::cell::RefCell::new(BTreeMap::new()),
        })
    }

    fn write(&self) -> Result<Self::Write<'_>, VogtError> {
        if !self.is_initialized() {
            return Err(VogtError::NotInitialized(format!(
                "no instance at {}",
                self.path.display()
            )));
        }
        let txn_id = self.ids.borrow_mut().next("txn");
        let conn = self.open(false)?;
        conn.execute_batch("BEGIN IMMEDIATE").map_err(sql_err)?;
        let revision = meta_get(&conn, META_REVISION)?
            .and_then(|v| v.parse().ok())
            .unwrap_or(0)
            + 1;
        meta_set(&conn, META_REVISION, &revision.to_string())?;
        Ok(SqliteWrite {
            view: SqliteReadView {
                conn,
                workflow_cache: std::cell::RefCell::new(BTreeMap::new()),
            },
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
pub struct SqliteWrite {
    view: SqliteReadView,
    txn_id: String,
    revision: i64,
    open: bool,
}

impl SqliteWrite {
    pub fn commit(mut self) -> Result<(), VogtError> {
        self.view.conn.execute_batch("COMMIT").map_err(sql_err)?;
        self.open = false;
        Ok(())
    }
}
impl Drop for SqliteWrite {
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
        Ok(meta_get(&self.conn, META_REVISION)?
            .and_then(|v| v.parse().ok())
            .unwrap_or(0))
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
    fn list_work_items(&self, _: &WorkFilter) -> Result<Vec<WorkItem>, VogtError> {
        later("list_work_items")
    }
    fn count_work_items(&self, _: &WorkFilter) -> Result<i64, VogtError> {
        later("count_work_items")
    }
    fn board_high_water(&self, _: &WorkFilter) -> Result<Option<(Moment, String)>, VogtError> {
        later("board_high_water")
    }
    fn board_counts(
        &self,
        _: &WorkFilter,
        _: &str,
        _: Option<&(Moment, String)>,
    ) -> Result<BTreeMap<(String, String), i64>, VogtError> {
        later("board_counts")
    }
    fn board_work_items(
        &self,
        _: &WorkFilter,
        _: &str,
        _: &[BoardCellQuery],
        _: Option<&(Moment, String)>,
        _: i64,
    ) -> Result<BTreeMap<(String, String), Vec<WorkItem>>, VogtError> {
        later("board_work_items")
    }
    fn blocking_fan_out(&self, _: &[String]) -> Result<BTreeMap<String, i64>, VogtError> {
        later("blocking_fan_out")
    }
    fn unfinished_blockers(&self, _: &str, _: &[&str]) -> Result<Vec<Blocker>, VogtError> {
        later("unfinished_blockers")
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
    fn list_suppressions(&self, _: bool, _: i64) -> Result<Vec<Suppression>, VogtError> {
        later("list_suppressions")
    }
    fn suppression_by_id(&self, _: &str) -> Result<Option<Suppression>, VogtError> {
        later("suppression_by_id")
    }
    fn contract_exemptions(&self, project_id: &str) -> Result<Vec<ContractExemption>, VogtError> {
        many(&self.conn, "SELECT e.*, p.slug AS project_slug FROM contract_exemptions e JOIN projects p ON p.id = e.project_id WHERE e.project_id = ? ORDER BY e.rule, e.target", params![project_id], row_exemption)
    }
    fn work_links_for_subjects(&self, _: &[String]) -> Result<BTreeMap<String, String>, VogtError> {
        later("work_links")
    }
    fn work_links_for_subjects_by_item(
        &self,
        _: &str,
    ) -> Result<BTreeMap<String, String>, VogtError> {
        later("work_links_by_item")
    }
    fn work_item_by_subject(&self, _: &str) -> Result<Option<WorkItem>, VogtError> {
        later("work_item_by_subject")
    }
    fn work_overlay(&self, _: &str) -> Result<Option<WorkOverlay>, VogtError> {
        later("work_overlay")
    }
    fn work_overlays(&self, _: &[String]) -> Result<BTreeMap<String, WorkOverlay>, VogtError> {
        later("work_overlays")
    }
    fn bound_branch_overlays(&self, _: i64) -> Result<Vec<WorkOverlay>, VogtError> {
        later("bound_branch_overlays")
    }
    fn token_by_hash(&self, _: &str) -> Result<Option<Token>, VogtError> {
        later("token_by_hash")
    }
    fn token_by_id(&self, _: &str) -> Result<Option<Token>, VogtError> {
        later("token_by_id")
    }
    fn list_tokens(&self, _: bool, _: i64) -> Result<Vec<Token>, VogtError> {
        later("list_tokens")
    }
    fn tokens_for_actor(&self, _: &str, _: bool) -> Result<Vec<Token>, VogtError> {
        later("tokens_for_actor")
    }
    fn list_auth_decisions(&self, _: Option<&str>, _: i64) -> Result<Vec<AuthDecision>, VogtError> {
        later("list_auth_decisions")
    }
    fn password_credential_by_username(
        &self,
        _: &str,
    ) -> Result<Option<PasswordCredential>, VogtError> {
        later("password_credential_by_username")
    }
    fn password_credential_for_actor(
        &self,
        _: &str,
    ) -> Result<Option<PasswordCredential>, VogtError> {
        later("password_credential_for_actor")
    }
    fn password_hash(&self, _: &str) -> Result<Option<String>, VogtError> {
        later("password_hash")
    }
    fn list_password_credentials(&self) -> Result<Vec<PasswordCredential>, VogtError> {
        later("list_password_credentials")
    }
    fn forge_account(&self, _: &str, _: &str) -> Result<Option<ForgeAccount>, VogtError> {
        later("forge_account")
    }
    fn forge_accounts_for_actor(&self, _: &str) -> Result<Vec<ForgeAccount>, VogtError> {
        later("forge_accounts_for_actor")
    }
    fn forge_account_secret(&self, _: &str, _: &str) -> Result<Option<String>, VogtError> {
        later("forge_account_secret")
    }
    fn list_drift(
        &self,
        _: Option<&str>,
        _: Option<&str>,
        _: Option<&str>,
        _: i64,
    ) -> Result<Vec<DriftProposal>, VogtError> {
        later("list_drift")
    }
    fn drift_by_id(&self, _: &str) -> Result<Option<DriftProposal>, VogtError> {
        later("drift_by_id")
    }
    fn open_drift_subjects(&self) -> Result<BTreeSet<(String, String, String)>, VogtError> {
        later("open_drift_subjects")
    }
    fn list_writeback_actions(
        &self,
        _: Option<&str>,
        _: i64,
    ) -> Result<Vec<WriteBackRecord>, VogtError> {
        later("list_writeback_actions")
    }
    fn drift_evidence_ids(&self) -> Result<BTreeSet<String>, VogtError> {
        later("drift_evidence_ids")
    }
    fn inbox_triage_by_key(&self, _: &str) -> Result<Option<InboxTriage>, VogtError> {
        later("inbox_triage_by_key")
    }
    fn inbox_triage_by_keys(
        &self,
        _: &[String],
    ) -> Result<BTreeMap<String, InboxTriage>, VogtError> {
        later("inbox_triage_by_keys")
    }
    fn list_inbox_triage(&self, _: i64) -> Result<Vec<InboxTriage>, VogtError> {
        later("list_inbox_triage")
    }
    fn actor_preference(&self, _: &str, _: &str) -> Result<Option<ActorPreference>, VogtError> {
        later("actor_preference")
    }
    fn actor_preferences(&self, _: &str) -> Result<Vec<ActorPreference>, VogtError> {
        later("actor_preferences")
    }
    fn session_by_id(&self, _: &str) -> Result<Option<CodingSession>, VogtError> {
        later("session_by_id")
    }
    fn session_grant(&self, _: &str) -> Result<Option<SessionGrant>, VogtError> {
        later("session_grant")
    }
    fn session_by_engine_id(&self, _: &str) -> Result<Option<CodingSession>, VogtError> {
        later("session_by_engine_id")
    }
    fn list_session_grants(
        &self,
        _: Option<&str>,
        _: Option<&str>,
        _: i64,
    ) -> Result<Vec<SessionGrant>, VogtError> {
        later("list_session_grants")
    }
    fn list_sessions(
        &self,
        _: Option<&str>,
        _: Option<&str>,
        _: bool,
        _: i64,
        _: i64,
    ) -> Result<Vec<CodingSession>, VogtError> {
        later("list_sessions")
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
        many(&self.conn, &format!("SELECT a.*, ac.identity_ref AS actor_identity_ref FROM audit a JOIN actors ac ON ac.id = a.actor_id {where_sql} ORDER BY a.at, a.id LIMIT ? OFFSET ?"), refs.as_slice(), row_audit)
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

impl ReadView for SqliteWrite {
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

impl WriteTxn for SqliteWrite {
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
            params![project.id, project.slug, project.name, project.root_path, project.repo_url, project.lifecycle_state, project.current_version, project.contract_version, project.compliance_status, project.compliance_checked_at.map(to_iso), project.contract_adopted_at.map(to_iso), project.write_back, project.link_state, serde_json::to_string(&project.exclusions).unwrap(), project.trust_state, to_iso(project.created_at), to_iso(project.updated_at)],
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
            values.push(Box::new(serde_json::to_string(exclusions).unwrap()));
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
        let next = meta_get(&self.view.conn, META_WORK_REF_SEQ)?
            .unwrap_or_else(|| "0".into())
            .parse::<i64>()
            .unwrap_or(0)
            + 1;
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
    fn insert_work_link(&mut self, _: &WorkLink) -> Result<(), VogtError> {
        later("insert_work_link")
    }
    fn upsert_work_overlay(&mut self, _: &WorkOverlay) -> Result<(), VogtError> {
        later("upsert_work_overlay")
    }
    fn insert_token(&mut self, _: &Token, _: &str) -> Result<(), VogtError> {
        later("insert_token")
    }
    fn carry_credentials(
        &mut self,
        _: &CarriedCredentials,
        _: &str,
        _: Moment,
    ) -> Result<CarryReport, VogtError> {
        later("carry_credentials")
    }
    fn set_instance_identity(&mut self, _: &str, _: &CloneStamp) -> Result<(), VogtError> {
        later("set_instance_identity")
    }
    fn revoke_token(&mut self, _: &str, _: &str, _: Moment) -> Result<bool, VogtError> {
        later("revoke_token")
    }
    fn reinstate_token(&mut self, _: &str) -> Result<bool, VogtError> {
        later("reinstate_token")
    }
    fn upsert_password_credential(
        &mut self,
        _: &str,
        _: &str,
        _: &str,
        _: &[String],
        _: Moment,
    ) -> Result<(), VogtError> {
        later("upsert_password_credential")
    }
    fn delete_password_credential(&mut self, _: &str) -> Result<bool, VogtError> {
        later("delete_password_credential")
    }
    fn upsert_forge_account(
        &mut self,
        _: &str,
        _: &str,
        _: &str,
        _: &str,
        _: &str,
        _: Moment,
    ) -> Result<(), VogtError> {
        later("upsert_forge_account")
    }
    fn delete_forge_account(&mut self, _: &str, _: &str) -> Result<bool, VogtError> {
        later("delete_forge_account")
    }
    fn insert_writeback(&mut self, _: &WriteBackRecord) -> Result<(), VogtError> {
        later("insert_writeback")
    }
    fn insert_session(&mut self, _: &CodingSession) -> Result<(), VogtError> {
        later("insert_session")
    }
    fn insert_session_grant(&mut self, _: &SessionGrant) -> Result<(), VogtError> {
        later("insert_session_grant")
    }
    fn update_session_grant(&mut self, _: &SessionGrant) -> Result<(), VogtError> {
        later("update_session_grant")
    }
    fn set_session_work_item(&mut self, _: &str, _: Option<&str>) -> Result<(), VogtError> {
        later("set_session_work_item")
    }
    fn mark_session_stopped(&mut self, _: &str, _: Moment) -> Result<(), VogtError> {
        later("mark_session_stopped")
    }
    fn insert_drift(&mut self, _: &DriftProposal) -> Result<(), VogtError> {
        later("insert_drift")
    }
    fn upsert_inbox_triage(&mut self, _: &InboxTriage) -> Result<(), VogtError> {
        later("upsert_inbox_triage")
    }
    fn upsert_actor_preference(&mut self, _: &ActorPreference) -> Result<(), VogtError> {
        later("upsert_actor_preference")
    }
    fn mark_drift_superseded(
        &mut self,
        _: &str,
        _: Option<&str>,
        _: Option<Moment>,
    ) -> Result<bool, VogtError> {
        later("mark_drift_superseded")
    }
    fn resolve_drift(
        &mut self,
        _: &str,
        _: &str,
        _: &str,
        _: &str,
        _: Moment,
    ) -> Result<bool, VogtError> {
        later("resolve_drift")
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
    fn append_audit(&mut self, record: &AuditRecord) -> Result<AuditRecord, VogtError> {
        let stored = AuditRecord {
            txn_id: self.txn_id.clone(),
            revision: self.revision,
            ..record.clone()
        };
        insert_audit(&self.view.conn, &stored)?;
        Ok(stored)
    }
    fn append_event(&mut self, event: &Event) -> Result<Event, VogtError> {
        self.view.conn.execute("INSERT INTO events (kind, entity_kind, entity_id, actor_id, audit_id, summary, at) VALUES (?, ?, ?, ?, ?, ?, ?)", params![event.kind, event.entity_kind, event.entity_id, event.actor_id, event.audit_id, event.summary.to_string(), to_iso(event.at)]).map_err(sql_err)?;
        Ok(Event {
            seq: self.view.conn.last_insert_rowid(),
            ..event.clone()
        })
    }
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
        exclusions: serde_json::from_str(&row.get::<_, String>("exclusions")?).unwrap_or_default(),
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
            &store.ids.borrow_mut().next("prj"),
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
            id: store.ids.borrow_mut().next("lbl"),
            name: "backend".into(),
            color: Some("blue".into()),
            created_at: now,
        };
        let initiative = Initiative {
            id: store.ids.borrow_mut().next("ini"),
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
        // A kind never stored falls back to the shipped default.
        assert_eq!(
            view.workflow_for("question").unwrap(),
            default_workflow("question")
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
            kind: "bug".into(),
            title: "leaks".into(),
            body: String::new(),
            state: "open".into(),
            priority: "p1".into(),
            effort: None,
            project_id: Some(project.id.clone()),
            project_slug: None,
            initiative_id: None,
            origin: "created".into(),
            trust_state: "unverified".into(),
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
            .append_audit(&AuditRecord {
                id: "aud_x".into(),
                txn_id: String::new(),
                revision: 0,
                actor_id: actor.id.clone(),
                actor_identity_ref: actor.identity_ref.clone(),
                operation: "work.create".into(),
                entity_kind: "work_item".into(),
                entity_id: item.id.clone(),
                reason: "filed".into(),
                payload_digest: format!("sha256:{}", "ab".repeat(32)),
                at: now,
            })
            .unwrap();
        let event = txn
            .append_event(&Event {
                seq: 0,
                kind: "created".into(),
                entity_kind: "work_item".into(),
                entity_id: item.id.clone(),
                actor_id: Some(actor.id.clone()),
                audit_id: Some(audit.id.clone()),
                summary: serde_json::json!({"verb": "created"}),
                at: now,
            })
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
}

const WORK_SELECT: &str = "SELECT w.*, p.slug AS project_slug, ac.identity_ref AS assignee_identity_ref FROM work_items w LEFT JOIN projects p ON p.id = w.project_id LEFT JOIN actors ac ON ac.id = w.assignee_actor_id";

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
        summary: serde_json::from_str(&summary).unwrap_or(serde_json::Value::Null),
        at: moment(row, "at")?,
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
    let mut add = |clause: &str, value: String| {
        clauses.push(clause.into());
        params_box.push(Box::new(value));
    };
    if let Some(actor) = &query.actor_id {
        add("a.actor_id = ?", actor.clone());
    }
    if let Some(operation) = &query.operation {
        add("a.operation = ?", operation.clone());
    }
    if let Some(entity) = &query.entity_id {
        add("a.entity_id = ?", entity.clone());
    }
    if let Some(project) = &query.project_id {
        add(
            "a.entity_id IN (SELECT id FROM work_items WHERE project_id = ?)",
            project.clone(),
        );
    }
    if let Some(since) = query.since {
        add("a.at >= ?", to_iso(since));
    }
    if let Some(until) = query.until {
        add("a.at < ?", to_iso(until));
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
