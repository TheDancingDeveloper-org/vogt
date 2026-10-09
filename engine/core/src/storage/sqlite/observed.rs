//! Observed SQLite store. Ports `src/vogt/storage/sqlite/observed.py`.
//!
//! Append-oriented evidence plus collector coverage. Unlike the declared
//! store, every method opens and closes its own connection, and a write
//! begins with `BEGIN IMMEDIATE`.

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::rc::Rc;

use rusqlite::{params, params_from_iter, Connection, OptionalExtension, Row};

use crate::core::{Clock, DepRef, IdFactory, Moment, Observation, RefKind, Sweep, SweepOutcome};
use crate::errors::VogtError;
use crate::storage::interface::MigrationReport;
use crate::storage::observed_types::{
    ActivityBatch, ActivityEventRow, ActivityIndexStats, ActivityQuery, ActivitySessionRow,
    AppendStats, DepRefRow, PendingObservation, PruneReport, TranscriptCursor,
};
use crate::storage::sqlite::connection::connect_with;
use crate::storage::sqlite::migrator::{self, migrations_root, table_exists};

const META_INSTANCE_ID: &str = "instance_id";
const META_CREATED_AT: &str = "created_at";
const WITHHELD: &str = "[withheld: configuration or environment output]";
const OPEN_STATE_SQL: &str =
    "lower(coalesce(json_extract(payload, '$.state'), '')) NOT IN ('closed', 'merged')";
const CLOSED_STATE_SQL: &str =
    "lower(coalesce(json_extract(payload, '$.state'), '')) IN ('closed', 'merged')";

pub struct SqliteObservedStore<C, I> {
    path: PathBuf,
    /// Shared with the context and the declared store, so one tick or one draw
    /// advances the count everyone else sees.
    clock: Rc<std::cell::RefCell<C>>,
    ids: Rc<std::cell::RefCell<I>>,
    synchronous: String,
    has_evidence_cached: Cell<bool>,
}

impl<C, I> SqliteObservedStore<C, I>
where
    C: Clock,
    I: IdFactory,
{
    #[allow(dead_code)]
    pub fn new(path: PathBuf, clock: C, ids: I) -> Self {
        Self::shared(
            path,
            Rc::new(std::cell::RefCell::new(clock)),
            Rc::new(std::cell::RefCell::new(ids)),
            crate::storage::sqlite::connection::DEFAULT_SYNCHRONOUS,
        )
    }

    /// A store over a clock and an id factory something else also holds.
    pub fn shared(
        path: PathBuf,
        clock: Rc<std::cell::RefCell<C>>,
        ids: Rc<std::cell::RefCell<I>>,
        synchronous: &str,
    ) -> Self {
        Self {
            path,
            clock,
            ids,
            synchronous: synchronous.to_string(),
            has_evidence_cached: Cell::new(false),
        }
    }

    fn open(&self, create: bool) -> Result<Connection, VogtError> {
        connect_with(&self.path, create, &self.synchronous).map_err(sql_err)
    }

    fn next_id(&self, prefix: &str) -> String {
        self.ids.borrow_mut().next(prefix)
    }
}

impl<C, I> crate::storage::interface::ObservedStore for SqliteObservedStore<C, I>
where
    C: Clock,
    I: IdFactory,
{
    fn migrate(&self) -> Result<MigrationReport, VogtError> {
        let now = self.clock.borrow_mut().now();
        let mut conn = self.open(true)?;
        let holder = format!("{}/{}", hostname(), std::process::id());
        migrator::migrate(
            &mut conn,
            "observed",
            migrations_root().as_deref(),
            &holder,
            &crate::core::to_iso(now),
        )
        .map_err(VogtError::from)
    }

    fn is_initialized(&self) -> bool {
        match self.instance_id() {
            Ok(Some(id)) => !id.is_empty(),
            _ => false,
        }
    }

    fn schema_version(&self) -> i64 {
        if !self.path.exists() {
            return 0;
        }
        let Ok(conn) = self.open(false) else {
            return 0;
        };
        migrator::applied_version(&conn).unwrap_or(0)
    }

    fn bundled_schema_version(&self) -> i64 {
        let Ok(migrations) = migrator::load_migrations("observed", migrations_root().as_deref())
        else {
            return 0;
        };
        migrations
            .last()
            .map(|migration| migration.number())
            .unwrap_or(0)
    }

    fn bind_instance(&self, instance_id: &str) -> Result<(), VogtError> {
        // A plain insert, not an upsert: binding twice is a constraint
        // failure, the same one Python's second INSERT raises. The created_at
        // row is what a restore checks alongside the instance id.
        let conn = self.open(true)?;
        conn.execute("BEGIN IMMEDIATE", []).map_err(sql_err)?;
        let outcome = (|| -> Result<(), rusqlite::Error> {
            conn.execute(
                "INSERT INTO meta (key, value) VALUES (?, ?)",
                params![META_INSTANCE_ID, instance_id],
            )?;
            conn.execute(
                "INSERT INTO meta (key, value) VALUES (?, ?)",
                params![
                    META_CREATED_AT,
                    crate::core::to_iso(self.clock.borrow_mut().now())
                ],
            )?;
            Ok(())
        })();
        finish(conn, outcome)
    }

    fn rebind_instance(&self, instance_id: &str) -> Result<(), VogtError> {
        // A clone re-stamps an existing file, so this never creates one.
        let conn = self.open(false)?;
        write_tx(&conn, || {
            let updated = conn.execute(
                "UPDATE meta SET value = ? WHERE key = ?",
                params![instance_id, META_INSTANCE_ID],
            )?;
            if updated == 0 {
                conn.execute(
                    "INSERT INTO meta (key, value) VALUES (?, ?)",
                    params![META_INSTANCE_ID, instance_id],
                )?;
            }
            Ok(())
        })
    }

    fn instance_id(&self) -> Result<Option<String>, VogtError> {
        if !self.path.exists() {
            return Ok(None);
        }
        let conn = self.open(false)?;
        if !table_exists(&conn, "meta").map_err(VogtError::from)? {
            return Ok(None);
        }
        conn.query_row(
            "SELECT value FROM meta WHERE key = ?",
            [META_INSTANCE_ID],
            |row| row.get(0),
        )
        .optional()
        .map_err(sql_err)
    }

    fn has_evidence_tables(&self) -> Result<bool, VogtError> {
        if !self.path.exists() {
            return Ok(false);
        }
        if self.has_evidence_cached.get() {
            return Ok(true);
        }
        let conn = self.open(false)?;
        let exists = table_exists(&conn, "observations").map_err(VogtError::from)?;
        if exists {
            self.has_evidence_cached.set(true);
        }
        Ok(exists)
    }

    fn begin_sweep(
        &self,
        collector: &str,
        scope: &[String],
        at: Moment,
    ) -> Result<Sweep, VogtError> {
        let sweep_id = self.next_id("swp");
        let rendered_scope = crate::decisions::python_json_dumps(&serde_json::json!(scope), false);
        let conn = self.open(true)?;
        write_tx(&conn, || {
            conn.execute(
                "INSERT INTO sweeps (id, collector, scope, started_at, outcome, stats) VALUES (?, ?, ?, ?, 'running', '{}')",
                params![sweep_id, collector, rendered_scope, crate::core::to_iso(at)],
            )?;
            Ok(())
        })?;
        Ok(Sweep {
            id: sweep_id,
            collector: collector.to_string(),
            scope: scope.to_vec(),
            started_at: at,
            finished_at: None,
            outcome: SweepOutcome::Running,
            stats: BTreeMap::new(),
            detail: None,
        })
    }

    fn finish_sweep(
        &self,
        sweep_id: &str,
        outcome: SweepOutcome,
        stats: &BTreeMap<String, i64>,
        at: Moment,
        detail: Option<&str>,
    ) -> Result<(), VogtError> {
        let rendered = render_stats(stats);
        let conn = self.open(true)?;
        write_tx(&conn, || {
            conn.execute(
                "UPDATE sweeps SET finished_at = ?, outcome = ?, stats = ?, detail = ? WHERE id = ?",
                params![
                    crate::core::to_iso(at),
                    vocab_text(outcome),
                    rendered,
                    detail,
                    sweep_id
                ],
            )?;
            Ok(())
        })
    }

    fn append(
        &self,
        sweep_id: &str,
        findings: &[PendingObservation],
        at: Moment,
    ) -> Result<AppendStats, VogtError> {
        let conn = self.open(true)?;
        conn.execute("BEGIN IMMEDIATE", []).map_err(sql_err)?;
        let outcome = (|| -> Result<AppendStats, rusqlite::Error> {
            let collector = collector_of(&conn, sweep_id)?;
            let mut stats = AppendStats::default();
            for finding in findings {
                // Dedup against the newest history row, not the projection:
                // the projection is rebuilt separately and is droppable, so
                // reading it here would re-insert a whole history after a
                // drop and would disagree with Python on an out-of-order
                // append.
                let current: Option<String> = conn
                    .query_row(
                        "SELECT content_digest FROM observations WHERE subject_key = ? ORDER BY observed_at DESC, id DESC LIMIT 1",
                        [&finding.subject_key],
                        |row| row.get(0),
                    )
                    .optional()?;
                if current.as_deref() == Some(finding.content_digest.as_str()) {
                    stats.unchanged += 1;
                    continue;
                }
                let observation_id = self.next_id("obs");
                let payload = crate::decisions::python_json_dumps(&finding.payload, true);
                conn.execute(
                    "INSERT INTO observations (id, sweep_id, collector, kind, project_id, subject_key, payload, content_digest, source_url, promoted, observed_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                    params![
                        observation_id,
                        sweep_id,
                        collector,
                        finding.kind,
                        finding.project_id,
                        finding.subject_key,
                        payload,
                        finding.content_digest,
                        finding.source_url,
                        finding.promoted as i64,
                        crate::core::to_iso(at),
                    ],
                )?;
                stats.new += 1;
            }
            Ok(stats)
        })();
        finish(conn, outcome)
    }

    fn list_sweeps(&self, collector: Option<&str>, limit: i64) -> Result<Vec<Sweep>, VogtError> {
        // Python's `if collector` treats "" as absent.
        let conn = self.open(false)?;
        match collector.filter(|name| !name.is_empty()) {
            Some(name) => many(
                &conn,
                "SELECT * FROM sweeps WHERE collector = ? ORDER BY started_at DESC, id DESC LIMIT ?",
                params![name, limit],
                row_sweep,
            ),
            None => many(
                &conn,
                "SELECT * FROM sweeps ORDER BY started_at DESC, id DESC LIMIT ?",
                [limit],
                row_sweep,
            ),
        }
    }

    fn coverage(&self) -> Result<BTreeMap<String, Sweep>, VogtError> {
        // The newest *completed* sweep per collector. A running sweep says
        // nothing about coverage yet, and freshness computed from one would
        // claim an answer newer than the evidence behind it.
        let conn = self.open(false)?;
        let rows = many(
            &conn,
            "SELECT * FROM sweeps WHERE finished_at IS NOT NULL ORDER BY collector, finished_at DESC, id DESC",
            [],
            row_sweep,
        )?;
        let mut newest: BTreeMap<String, Sweep> = BTreeMap::new();
        for sweep in rows {
            newest.entry(sweep.collector.clone()).or_insert(sweep);
        }
        Ok(newest)
    }

    fn coverage_by_project(&self) -> Result<BTreeMap<String, BTreeMap<String, Moment>>, VogtError> {
        // Per collector, when each project was last swept by it, taken from
        // the scopes of finished sweeps. Ascending, so a later sweep
        // overwrites an earlier one; a collector with zero findings still
        // appears, because the question is what looked at what.
        let conn = self.open(false)?;
        let rows = many(
            &conn,
            "SELECT * FROM sweeps WHERE finished_at IS NOT NULL ORDER BY collector, finished_at ASC, id ASC",
            [],
            row_sweep,
        )?;
        let mut seen: BTreeMap<String, BTreeMap<String, Moment>> = BTreeMap::new();
        for sweep in rows {
            let finished = sweep.finished_at.unwrap_or(sweep.started_at);
            let per_project = seen.entry(sweep.collector).or_default();
            for project_id in sweep.scope {
                per_project.insert(project_id, finished);
            }
        }
        Ok(seen)
    }

    fn fail_sweeps(&self, sweep_ids: &[String], detail: &str) -> Result<(), VogtError> {
        if sweep_ids.is_empty() {
            return Ok(());
        }
        let placeholders = vec!["?"; sweep_ids.len()].join(", ");
        let conn = self.open(true)?;
        write_tx(&conn, || {
            conn.execute(
                &format!(
                    "UPDATE sweeps SET outcome = 'failed', detail = ? WHERE id IN ({placeholders})"
                ),
                params_from_iter(
                    std::iter::once(detail.to_string()).chain(sweep_ids.iter().cloned()),
                ),
            )?;
            Ok(())
        })
    }

    fn list_observations(
        &self,
        kind: Option<&str>,
        project_id: Option<&str>,
        subject_key: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Observation>, VogtError> {
        let mut clauses = Vec::new();
        let mut values: Vec<String> = Vec::new();
        for (column, value) in [
            ("kind", kind),
            ("project_id", project_id),
            ("subject_key", subject_key),
        ] {
            if let Some(value) = value {
                clauses.push(format!("{column} = ?"));
                values.push(value.to_string());
            }
        }
        let where_sql = if clauses.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", clauses.join(" AND "))
        };
        let conn = self.open(false)?;
        many(
            &conn,
            &format!("SELECT * FROM observations {where_sql} ORDER BY observed_at DESC, id DESC LIMIT ? OFFSET ?"),
            params_from_iter(values.into_iter().chain([limit.to_string(), offset.to_string()])),
            row_observation,
        )
    }

    fn latest(
        &self,
        kinds: &[String],
        project_id: Option<&str>,
        promoted_only: bool,
        exclude_closed: bool,
        limit: i64,
    ) -> Result<Vec<Observation>, VogtError> {
        let mut clauses = Vec::new();
        let mut values: Vec<String> = Vec::new();
        if !kinds.is_empty() {
            clauses.push(format!("kind IN ({})", vec!["?"; kinds.len()].join(", ")));
            values.extend(kinds.iter().cloned());
        }
        if let Some(project) = project_id {
            clauses.push("project_id = ?".into());
            values.push(project.to_string());
        }
        if promoted_only {
            clauses.push("promoted = 1".into());
        }
        if exclude_closed {
            clauses.push(OPEN_STATE_SQL.into());
        }
        let where_sql = if clauses.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", clauses.join(" AND "))
        };
        values.push(limit.to_string());
        let conn = self.open(false)?;
        many(
            &conn,
            &format!(
                "SELECT subject_key, observation_id AS id, observation_id, collector, kind, project_id, payload, content_digest, source_url, promoted, observed_at, '' AS sweep_id FROM latest_observations {where_sql} ORDER BY observed_at DESC, subject_key LIMIT ?"
            ),
            params_from_iter(values),
            row_observation,
        )
    }

    fn latest_by_subject(&self, subject_key: &str) -> Result<Option<Observation>, VogtError> {
        let conn = self.open(false)?;
        one(
            &conn,
            "SELECT subject_key, observation_id AS id, observation_id, collector, kind, project_id, payload, content_digest, source_url, promoted, observed_at, '' AS sweep_id FROM latest_observations WHERE subject_key = ?",
            [subject_key],
            row_observation,
        )
    }

    fn count_closed(&self, kinds: &[String], project_id: Option<&str>) -> Result<i64, VogtError> {
        let mut clauses = vec![CLOSED_STATE_SQL.to_string()];
        let mut values: Vec<String> = Vec::new();
        if !kinds.is_empty() {
            clauses.push(format!("kind IN ({})", vec!["?"; kinds.len()].join(", ")));
            values.extend(kinds.iter().cloned());
        }
        if let Some(project) = project_id {
            clauses.push("project_id = ?".into());
            values.push(project.to_string());
        }
        let conn = self.open(false)?;
        conn.query_row(
            &format!(
                "SELECT COUNT(*) AS n FROM latest_observations WHERE {}",
                clauses.join(" AND ")
            ),
            params_from_iter(values),
            |row| row.get(0),
        )
        .map_err(sql_err)
    }

    fn get_watermark(
        &self,
        collector: &str,
        project_id: &str,
    ) -> Result<Option<String>, VogtError> {
        let conn = self.open(false)?;
        let row: Option<Option<String>> = conn
            .query_row(
                "SELECT watermark FROM sync_state WHERE collector = ? AND project_id = ?",
                params![collector, project_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(sql_err)?;
        Ok(row.flatten())
    }

    fn set_watermark(
        &self,
        collector: &str,
        project_id: &str,
        watermark: Option<&str>,
        at: Moment,
    ) -> Result<(), VogtError> {
        let conn = self.open(true)?;
        write_tx(&conn, || {
            conn.execute(
                "INSERT INTO sync_state (collector, project_id, watermark, updated_at) VALUES (?, ?, ?, ?) ON CONFLICT (collector, project_id) DO UPDATE SET watermark = excluded.watermark, updated_at = excluded.updated_at",
                params![collector, project_id, watermark, crate::core::to_iso(at)],
            )?;
            Ok(())
        })
    }

    fn touch_subjects(&self, subject_keys: &[String], at: Moment) -> Result<(), VogtError> {
        if subject_keys.is_empty() {
            return Ok(());
        }
        let stamp = crate::core::to_iso(at);
        let conn = self.open(true)?;
        write_tx(&conn, || {
            for key in subject_keys {
                conn.execute(
                    "INSERT INTO subject_seen (subject_key, last_confirmed_at) VALUES (?, ?) ON CONFLICT (subject_key) DO UPDATE SET last_confirmed_at = excluded.last_confirmed_at",
                    params![key, stamp],
                )?;
            }
            Ok(())
        })
    }

    fn last_confirmed(
        &self,
        subject_keys: &[String],
    ) -> Result<BTreeMap<String, Moment>, VogtError> {
        if subject_keys.is_empty() {
            return Ok(BTreeMap::new());
        }
        let placeholders = vec!["?"; subject_keys.len()].join(", ");
        let conn = self.open(false)?;
        let rows = many(
            &conn,
            &format!(
                "SELECT subject_key, last_confirmed_at FROM subject_seen WHERE subject_key IN ({placeholders})"
            ),
            params_from_iter(subject_keys),
            |row| Ok((row.get(0)?, moment(row, 1)?)),
        )?;
        Ok(rows.into_iter().collect())
    }

    fn dep_refs(
        &self,
        from_project_id: Option<&str>,
        to_project_id: Option<&str>,
    ) -> Result<Vec<DepRef>, VogtError> {
        let mut clauses = Vec::new();
        let mut values: Vec<String> = Vec::new();
        if let Some(from) = from_project_id {
            clauses.push("from_project_id = ?".to_string());
            values.push(from.to_string());
        }
        if let Some(to) = to_project_id {
            clauses.push("to_project_id = ?".to_string());
            values.push(to.to_string());
        }
        let where_sql = if clauses.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", clauses.join(" AND "))
        };
        let conn = self.open(false)?;
        many(
            &conn,
            &format!("SELECT * FROM latest_dep_refs {where_sql} ORDER BY subject_key"),
            params_from_iter(values),
            row_dep_ref,
        )
    }

    fn counts(&self) -> Result<BTreeMap<String, i64>, VogtError> {
        let conn = self.open(false)?;
        let mut counts = BTreeMap::new();
        for (name, table) in [
            ("sweeps", "sweeps"),
            ("observations", "observations"),
            ("subjects", "latest_observations"),
            ("dep_refs", "latest_dep_refs"),
        ] {
            counts.insert(name.to_string(), count_table(&conn, table)?);
        }
        Ok(counts)
    }

    fn rebuild_latest(&self) -> Result<i64, VogtError> {
        let conn = self.open(true)?;
        let outcome = write_tx(&conn, || {
            conn.execute("DELETE FROM latest_observations", [])?;
            conn.execute(
                "INSERT INTO latest_observations (subject_key, observation_id, collector, kind, project_id, payload, content_digest, source_url, promoted, observed_at) SELECT o.subject_key, o.id, o.collector, o.kind, o.project_id, o.payload, o.content_digest, o.source_url, o.promoted, o.observed_at FROM observations o WHERE o.id = (SELECT id FROM observations x WHERE x.subject_key = o.subject_key ORDER BY x.observed_at DESC, x.id DESC LIMIT 1)",
                [],
            )?;
            conn.query_row("SELECT COUNT(*) AS n FROM latest_observations", [], |row| {
                row.get(0)
            })
        })?;
        Ok(outcome)
    }

    fn replace_dep_refs(&self, rows: &[DepRefRow]) -> Result<i64, VogtError> {
        let conn = self.open(true)?;
        write_tx(&conn, || {
            conn.execute("DELETE FROM latest_dep_refs", [])?;
            for row in rows {
                conn.execute(
                    "INSERT INTO latest_dep_refs (subject_key, from_project_id, ref_kind, raw_target, manifest, to_project_id, observed_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
                    params![
                        row.subject_key,
                        row.from_project_id,
                        row.ref_kind,
                        row.raw_target,
                        row.manifest,
                        row.to_project_id,
                        crate::core::to_iso(row.observed_at),
                    ],
                )?;
            }
            Ok(())
        })?;
        Ok(rows.len() as i64)
    }

    fn prune(
        &self,
        before: Moment,
        protected_observation_ids: &BTreeSet<String>,
    ) -> Result<PruneReport, VogtError> {
        let conn = self.open(true)?;
        let outcome = write_tx(&conn, || {
            let newest: BTreeSet<String> = {
                let mut statement = conn.prepare(
                    "SELECT id FROM observations o WHERE o.id = (SELECT id FROM observations x WHERE x.subject_key = o.subject_key ORDER BY x.observed_at DESC, x.id DESC LIMIT 1)",
                )?;
                let rows = statement.query_map([], |row| row.get(0))?;
                rows.collect::<Result<BTreeSet<String>, _>>()?
            };
            let candidates: Vec<String> = {
                let mut statement =
                    conn.prepare("SELECT id FROM observations WHERE observed_at < ?")?;
                let rows = statement.query_map([crate::core::to_iso(before)], |row| row.get(0))?;
                rows.collect::<Result<Vec<String>, _>>()?
            };
            let doomed: Vec<&String> = candidates
                .iter()
                .filter(|candidate| {
                    let id: &String = candidate;
                    !newest.contains(id) && !protected_observation_ids.contains(id)
                })
                .collect();
            for observation_id in &doomed {
                conn.execute("DELETE FROM observations WHERE id = ?", [*observation_id])?;
            }
            Ok(PruneReport {
                removed: doomed.len() as i64,
                kept_latest: candidates.iter().filter(|c| newest.contains(*c)).count() as i64,
                kept_referenced: candidates
                    .iter()
                    .filter(|c| !newest.contains(*c) && protected_observation_ids.contains(*c))
                    .count() as i64,
            })
        })?;
        Ok(outcome)
    }

    fn activity_cursors(&self) -> Result<BTreeMap<String, TranscriptCursor>, VogtError> {
        let conn = self.open(false)?;
        let rows = many(&conn, "SELECT * FROM agent_activity_files", [], |row| {
            Ok((
                row.get::<_, String>("path")?,
                TranscriptCursor {
                    path: row.get("path")?,
                    agent: row.get("agent")?,
                    offset: row.get("byte_offset")?,
                    size: row.get("size")?,
                    agent_session_id: row.get("agent_session_id")?,
                    cwd: row.get("cwd")?,
                },
            ))
        })?;
        Ok(rows.into_iter().collect())
    }

    fn index_activity(
        &self,
        sweep_id: &str,
        batch: &ActivityBatch,
        at: Moment,
    ) -> Result<ActivityIndexStats, VogtError> {
        let conn = self.open(true)?;
        let outcome = write_tx(&conn, || {
            let mut calls = 0;
            for call in &batch.calls {
                let services = format!(
                    ",{}",
                    call.services
                        .iter()
                        .map(|tag| format!("{tag},"))
                        .collect::<String>()
                );
                let changed = conn.execute(
                    "INSERT OR IGNORE INTO agent_activity (id, sweep_id, source_path, call_id, agent, agent_session_id, cwd, tool, summary, services, withheld, at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                    params![
                        self.next_id("act"),
                        sweep_id,
                        call.source_path,
                        call.call_id,
                        call.agent,
                        call.agent_session_id,
                        call.cwd,
                        call.tool,
                        call.summary,
                        services,
                        call.withheld as i64,
                        crate::core::to_iso(call.at),
                    ],
                )?;
                calls += changed as i64;
            }
            let mut results = 0;
            for result in &batch.results {
                let changed = conn.execute(
                    "UPDATE agent_activity SET error = ?, excerpt = CASE WHEN withheld = 1 AND ? IS NOT NULL THEN ? ELSE ? END, finished_at = ? WHERE source_path = ? AND call_id = ?",
                    params![
                        result.error as i64,
                        result.excerpt,
                        WITHHELD,
                        result.excerpt,
                        result.at.map(crate::core::to_iso),
                        result.source_path,
                        result.call_id,
                    ],
                )?;
                results += changed as i64;
            }
            for cursor in &batch.cursors {
                conn.execute(
                    "INSERT INTO agent_activity_files (path, agent, byte_offset, size, agent_session_id, cwd, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?) ON CONFLICT (path) DO UPDATE SET agent = excluded.agent, byte_offset = excluded.byte_offset, size = excluded.size, agent_session_id = excluded.agent_session_id, cwd = excluded.cwd, updated_at = excluded.updated_at",
                    params![
                        cursor.path,
                        cursor.agent,
                        cursor.offset,
                        cursor.size,
                        cursor.agent_session_id,
                        cursor.cwd,
                        crate::core::to_iso(at),
                    ],
                )?;
            }
            Ok(ActivityIndexStats { calls, results })
        })?;
        Ok(outcome)
    }

    fn search_activity(
        &self,
        query: &ActivityQuery,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<ActivityEventRow>, VogtError> {
        let (where_sql, params) = activity_where(query);
        let conn = self.open(false)?;
        many(
            &conn,
            &format!(
                "SELECT id, agent, agent_session_id, cwd, tool, summary, services, error, excerpt, at, finished_at FROM agent_activity {where_sql} ORDER BY at DESC, id DESC LIMIT ? OFFSET ?"
            ),
            params_from_iter(params.into_iter().chain([limit.to_string(), offset.to_string()])),
            row_activity,
        )
    }

    fn summarize_activity(
        &self,
        query: &ActivityQuery,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<ActivitySessionRow>, VogtError> {
        let (where_sql, params) = activity_where(query);
        let conn = self.open(false)?;
        let groups = many(
            &conn,
            &format!(
                "SELECT agent_session_id, MIN(agent) AS agent, MIN(at) AS first_at, MAX(at) AS last_at, COUNT(*) AS calls, SUM(error) AS errors, SUM(finished_at IS NOT NULL) AS finished, SUM(CASE WHEN finished_at IS NULL THEN 0 ELSE MAX(0, CAST(ROUND((julianday(finished_at) - julianday(at)) * 86400000) AS INTEGER)) END) AS wait_ms FROM agent_activity {where_sql} GROUP BY agent_session_id ORDER BY last_at DESC, agent_session_id LIMIT ? OFFSET ?"
            ),
            params_from_iter(params.iter().cloned().chain([limit.to_string(), offset.to_string()])),
            |row| {
                Ok((
                    row.get::<_, String>("agent_session_id")?,
                    row.get::<_, String>("agent")?,
                    moment(row, "first_at")?,
                    moment(row, "last_at")?,
                    row.get::<_, i64>("calls")?,
                    row.get::<_, Option<i64>>("errors")?.unwrap_or(0),
                    row.get::<_, Option<i64>>("finished")?.unwrap_or(0),
                    row.get::<_, Option<i64>>("wait_ms")?.unwrap_or(0),
                ))
            },
        )?;
        if groups.is_empty() {
            return Ok(Vec::new());
        }
        let ids: Vec<String> = groups.iter().map(|group| group.0.clone()).collect();
        let marks = vec!["?"; ids.len()].join(", ");
        let joiner = if where_sql.is_empty() { "WHERE" } else { "AND" };
        let scoped = format!("{where_sql} {joiner} agent_session_id IN ({marks})");
        let scoped_params: Vec<String> =
            params.iter().cloned().chain(ids.iter().cloned()).collect();

        let mut tools: BTreeMap<String, BTreeMap<String, i64>> = BTreeMap::new();
        for (session, tool, count) in many(
            &conn,
            &format!(
                "SELECT agent_session_id, tool, COUNT(*) AS n FROM agent_activity {scoped} GROUP BY agent_session_id, tool"
            ),
            params_from_iter(scoped_params.iter()),
            |row| Ok((row.get(0)?, row.get(1)?, row.get::<_, i64>(2)?)),
        )? {
            tools.entry(session).or_default().insert(tool, count);
        }

        let mut services: BTreeMap<String, BTreeMap<String, i64>> = BTreeMap::new();
        for (session, stored, count) in many(
            &conn,
            &format!(
                "SELECT agent_session_id, services, COUNT(*) AS n FROM agent_activity {scoped} AND services != ',' GROUP BY agent_session_id, services"
            ),
            params_from_iter(scoped_params.iter()),
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, i64>(2)?)),
        )? {
            let per_service = services.entry(session).or_default();
            for tag in tags(&stored) {
                *per_service.entry(tag).or_default() += count;
            }
        }

        let mut cwds: BTreeMap<String, Option<String>> = BTreeMap::new();
        for session_id in &ids {
            let cwd: Option<String> = conn
                .query_row(
                    "SELECT cwd FROM agent_activity WHERE agent_session_id = ? AND cwd IS NOT NULL ORDER BY at DESC LIMIT 1",
                    [session_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(sql_err)?
                .flatten();
            cwds.insert(session_id.clone(), cwd);
        }

        Ok(groups
            .into_iter()
            .map(
                |(session, agent, first_at, last_at, calls, errors, finished, wait_ms)| {
                    ActivitySessionRow {
                        agent,
                        agent_session_id: session.clone(),
                        cwd: cwds.get(&session).cloned().flatten(),
                        first_at,
                        last_at,
                        calls,
                        errors,
                        finished,
                        wait_ms,
                        tools: ranked(tools.remove(&session).unwrap_or_default()),
                        services: ranked(services.remove(&session).unwrap_or_default()),
                    }
                },
            )
            .collect())
    }
}

fn sql_err(err: rusqlite::Error) -> VogtError {
    VogtError::MigrationError(err.to_string())
}

/// `socket.gethostname()`, which reads the kernel name rather than `$HOSTNAME`.
fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|text| text.trim().to_string())
        .unwrap_or_else(|_| "localhost".to_string())
}

fn finish<T>(conn: Connection, outcome: Result<T, rusqlite::Error>) -> Result<T, VogtError> {
    match outcome {
        Ok(value) => {
            conn.execute("COMMIT", []).map_err(sql_err)?;
            Ok(value)
        }
        Err(err) => {
            let _ = conn.execute("ROLLBACK", []);
            Err(sql_err(err))
        }
    }
}

fn write_tx<T>(
    conn: &Connection,
    body: impl FnOnce() -> Result<T, rusqlite::Error>,
) -> Result<T, VogtError> {
    conn.execute("BEGIN IMMEDIATE", []).map_err(sql_err)?;
    finish_borrowed(conn, body())
}

fn finish_borrowed<T>(
    conn: &Connection,
    outcome: Result<T, rusqlite::Error>,
) -> Result<T, VogtError> {
    match outcome {
        Ok(value) => {
            conn.execute("COMMIT", []).map_err(sql_err)?;
            Ok(value)
        }
        Err(err) => {
            let _ = conn.execute("ROLLBACK", []);
            Err(sql_err(err))
        }
    }
}

fn collector_of(conn: &Connection, sweep_id: &str) -> Result<String, rusqlite::Error> {
    let found: Option<String> = conn
        .query_row(
            "SELECT collector FROM sweeps WHERE id = ?",
            [sweep_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(found.unwrap_or_default())
}

fn count_table(conn: &Connection, table: &str) -> Result<i64, VogtError> {
    conn.query_row(&format!("SELECT COUNT(*) AS n FROM {table}"), [], |row| {
        row.get(0)
    })
    .map_err(sql_err)
}

fn render_stats(stats: &BTreeMap<String, i64>) -> String {
    let payload = serde_json::Value::Object(
        stats
            .iter()
            .map(|(key, value)| (key.clone(), serde_json::json!(value)))
            .collect(),
    );
    crate::decisions::python_json_dumps(&payload, true)
}

fn vocab_text<T: serde::Serialize>(value: T) -> String {
    match serde_json::to_value(value).expect("vocabulary serializes") {
        serde_json::Value::String(text) => text,
        other => other.to_string(),
    }
}

fn vocab_cell<T: serde::de::DeserializeOwned>(row: &Row<'_>, column: &str) -> rusqlite::Result<T> {
    let text: String = row.get(column)?;
    serde_json::from_value(serde_json::Value::String(text)).map_err(|err| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(err))
    })
}

fn moment(row: &Row<'_>, column: impl rusqlite::RowIndex) -> rusqlite::Result<Moment> {
    crate::core::from_iso(&row.get::<_, String>(column)?)
        .map_err(rusqlite::Error::InvalidColumnName)
}

fn opt_moment(row: &Row<'_>, column: &str) -> rusqlite::Result<Option<Moment>> {
    row.get::<_, Option<String>>(column)?
        .map(|text| crate::core::from_iso(&text).map_err(rusqlite::Error::InvalidColumnName))
        .transpose()
}

fn json_cell(row: &Row<'_>, column: &str) -> rusqlite::Result<serde_json::Value> {
    let text: String = row.get(column)?;
    serde_json::from_str(&text).map_err(|err| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(err))
    })
}

fn one<T>(
    conn: &Connection,
    sql: &str,
    params: impl rusqlite::Params,
    map: fn(&Row<'_>) -> rusqlite::Result<T>,
) -> Result<Option<T>, VogtError> {
    let mut statement = conn.prepare(sql).map_err(sql_err)?;
    statement.query_row(params, map).optional().map_err(sql_err)
}

fn many<T>(
    conn: &Connection,
    sql: &str,
    params: impl rusqlite::Params,
    map: fn(&Row<'_>) -> rusqlite::Result<T>,
) -> Result<Vec<T>, VogtError> {
    let mut statement = conn.prepare(sql).map_err(sql_err)?;
    let rows = statement.query_map(params, map).map_err(sql_err)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(sql_err)
}

fn row_sweep(row: &Row<'_>) -> rusqlite::Result<Sweep> {
    let scope = json_cell(row, "scope")?;
    let stats = json_cell(row, "stats")?;
    Ok(Sweep {
        id: row.get("id")?,
        collector: row.get("collector")?,
        scope: scope
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
        started_at: moment(row, "started_at")?,
        finished_at: opt_moment(row, "finished_at")?,
        outcome: vocab_cell::<SweepOutcome>(row, "outcome")?,
        stats: stats
            .as_object()
            .map(|object| {
                object
                    .iter()
                    .filter_map(|(key, value)| value.as_i64().map(|number| (key.clone(), number)))
                    .collect()
            })
            .unwrap_or_default(),
        detail: row.get("detail")?,
    })
}

fn row_observation(row: &Row<'_>) -> rusqlite::Result<Observation> {
    Ok(Observation {
        id: row.get("id")?,
        sweep_id: row.get("sweep_id")?,
        collector: row.get("collector")?,
        kind: row.get("kind")?,
        project_id: row.get("project_id")?,
        subject_key: row.get("subject_key")?,
        payload: json_cell(row, "payload")?,
        content_digest: row.get("content_digest")?,
        source_url: row.get("source_url")?,
        promoted: row.get::<_, i64>("promoted")? != 0,
        observed_at: moment(row, "observed_at")?,
    })
}

fn row_dep_ref(row: &Row<'_>) -> rusqlite::Result<DepRef> {
    Ok(DepRef {
        subject_key: row.get("subject_key")?,
        from_project_id: row.get("from_project_id")?,
        from_project_slug: None,
        ref_kind: vocab_cell::<RefKind>(row, "ref_kind")?,
        raw_target: row.get("raw_target")?,
        manifest: row.get("manifest")?,
        to_project_id: row.get("to_project_id")?,
        to_project_slug: None,
        observed_at: moment(row, "observed_at")?,
    })
}

fn row_activity(row: &Row<'_>) -> rusqlite::Result<ActivityEventRow> {
    Ok(ActivityEventRow {
        id: row.get("id")?,
        agent: row.get("agent")?,
        agent_session_id: row.get("agent_session_id")?,
        cwd: row.get("cwd")?,
        tool: row.get("tool")?,
        summary: row.get("summary")?,
        services: tags(&row.get::<_, String>("services")?),
        error: row.get::<_, i64>("error")? != 0,
        excerpt: row.get("excerpt")?,
        at: moment(row, "at")?,
        finished_at: opt_moment(row, "finished_at")?,
    })
}

fn tags(stored: &str) -> Vec<String> {
    stored
        .split(',')
        .filter(|tag| !tag.is_empty())
        .map(str::to_string)
        .collect()
}

fn ranked(counts: BTreeMap<String, i64>) -> BTreeMap<String, i64> {
    let mut pairs: Vec<(String, i64)> = counts.into_iter().collect();
    pairs.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    pairs.into_iter().collect()
}

fn like_escape(text: &str) -> String {
    text.replace('!', "!!")
        .replace('%', "!%")
        .replace('_', "!_")
}

fn activity_where(query: &ActivityQuery) -> (String, Vec<String>) {
    let mut clauses = Vec::new();
    let mut params = Vec::new();
    if let Some(q) = query.q.as_deref().filter(|text| !text.is_empty()) {
        let needle = format!("%{}%", like_escape(q));
        clauses.push(
            "(summary LIKE ? ESCAPE '!' OR tool LIKE ? ESCAPE '!' OR excerpt LIKE ? ESCAPE '!')"
                .to_string(),
        );
        params.extend([needle.clone(), needle.clone(), needle]);
    }
    if let Some(service) = query.service.as_deref().filter(|text| !text.is_empty()) {
        clauses.push("services LIKE ? ESCAPE '!'".to_string());
        params.push(format!("%,{},%", like_escape(service)));
    }
    if let Some(tool) = query.tool.as_deref().filter(|text| !text.is_empty()) {
        clauses.push("tool = ?".to_string());
        params.push(tool.to_string());
    }
    if query.errors_only {
        clauses.push("error = 1".to_string());
    }
    if let Some(since) = query.since {
        clauses.push("at >= ?".to_string());
        params.push(crate::core::to_iso(since));
    }
    if let Some(until) = query.until {
        clauses.push("at < ?".to_string());
        params.push(crate::core::to_iso(until));
    }
    if let Some(ids) = &query.agent_session_ids {
        if ids.is_empty() {
            clauses.push("0".to_string());
        } else {
            clauses.push(format!(
                "agent_session_id IN ({})",
                vec!["?"; ids.len()].join(", ")
            ));
            params.extend(ids.iter().cloned());
        }
    }
    if let Some(roots) = &query.cwd_roots {
        let mut alternatives = Vec::new();
        for root in roots {
            let trimmed = root.trim_end_matches('/');
            let trimmed = if trimmed.is_empty() { "/" } else { trimmed };
            let prefix = if trimmed.ends_with('/') {
                trimmed.to_string()
            } else {
                format!("{trimmed}/")
            };
            alternatives.push("(cwd = ? OR substr(cwd, 1, ?) = ?)".to_string());
            params.push(trimmed.to_string());
            params.push(prefix.len().to_string());
            params.push(prefix);
        }
        clauses.push(if alternatives.is_empty() {
            "0".to_string()
        } else {
            format!("({})", alternatives.join(" OR "))
        });
    }
    if clauses.is_empty() {
        (String::new(), params)
    } else {
        (format!("WHERE {}", clauses.join(" AND ")), params)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{SequentialIds, StepClock};
    use crate::storage::interface::ObservedStore;
    use crate::storage::observed_types::{ActivityCall, ActivityResult};

    fn moment() -> Moment {
        Moment::from_unix(1_700_000_000, 0)
    }

    fn store(dir: &std::path::Path) -> SqliteObservedStore<StepClock, SequentialIds> {
        let store = SqliteObservedStore::new(
            dir.join("observed.sqlite3"),
            StepClock::new(moment()),
            SequentialIds::new(None).unwrap(),
        );
        store.migrate().unwrap();
        store.bind_instance("inst_1").unwrap();
        store
    }

    #[test]
    fn a_digest_match_is_unchanged_and_retention_keeps_the_newest_row() {
        let dir = std::env::temp_dir().join(format!("vogt-observed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = store(&dir);
        let now = moment();
        let sweep = store.begin_sweep("git", &["proj_1".into()], now).unwrap();
        assert_eq!(sweep.outcome, SweepOutcome::Running);

        let finding = PendingObservation {
            kind: "commit".into(),
            subject_key: "proj_1:abc".into(),
            payload: serde_json::json!({"state": "open", "sha": "abc"}),
            content_digest: "sha256:1".into(),
            project_id: Some("proj_1".into()),
            source_url: Some("https://example/abc".into()),
            promoted: false,
        };
        let first = store
            .append(&sweep.id, std::slice::from_ref(&finding), now)
            .unwrap();
        assert_eq!(
            first,
            AppendStats {
                new: 1,
                unchanged: 0
            }
        );
        let again = store
            .append(&sweep.id, std::slice::from_ref(&finding), now)
            .unwrap();
        assert_eq!(
            again,
            AppendStats {
                new: 0,
                unchanged: 1
            }
        );

        let moved = PendingObservation {
            content_digest: "sha256:2".into(),
            payload: serde_json::json!({"state": "closed", "sha": "def"}),
            ..finding.clone()
        };
        let later = Moment::from_unix(1_700_000_900, 0);
        store.append(&sweep.id, &[moved], later).unwrap();
        store
            .finish_sweep(
                &sweep.id,
                SweepOutcome::Ok,
                &BTreeMap::from([("new".into(), 2)]),
                later,
                None,
            )
            .unwrap();

        // The projection is rebuilt separately, never by append.
        let rebuilt = store.rebuild_latest().unwrap();
        assert_eq!(rebuilt, 1);

        let latest = store.latest_by_subject("proj_1:abc").unwrap().unwrap();
        assert_eq!(latest.content_digest, "sha256:2");
        assert_eq!(latest.source_url.as_deref(), Some("https://example/abc"));
        assert_eq!(latest.sweep_id, "");
        assert_eq!(store.count_closed(&["commit".into()], None).unwrap(), 1);
        assert!(store
            .latest(&["commit".into()], None, false, true, 10)
            .unwrap()
            .is_empty());

        // Dedup reads history, not the projection: appending an older
        // payload and then the current one again counts the second as
        // unchanged and writes no duplicate row.
        let older = PendingObservation {
            content_digest: "sha256:1".into(),
            payload: serde_json::json!({"sha": "abc", "state": "open"}),
            ..finding
        };
        let back = Moment::from_unix(1_700_000_450, 0);
        let out_of_order = store
            .append(&sweep.id, std::slice::from_ref(&older), back)
            .unwrap();
        assert_eq!(
            out_of_order,
            AppendStats {
                new: 1,
                unchanged: 0
            }
        );
        let repeated = store
            .append(
                &sweep.id,
                &[PendingObservation {
                    content_digest: "sha256:2".into(),
                    payload: serde_json::json!({"sha": "def", "state": "closed"}),
                    ..older
                }],
                later,
            )
            .unwrap();
        assert_eq!(
            repeated,
            AppendStats {
                new: 0,
                unchanged: 1
            }
        );
        assert_eq!(store.counts().unwrap().get("observations"), Some(&3));

        // `before` is exclusive of the newest row, so both older rows are
        // candidates and neither is the newest, so both go.
        let report = store.prune(later, &BTreeSet::new()).unwrap();
        assert_eq!(report.removed, 2);
        assert_eq!(report.kept_latest, 0);
        assert_eq!(store.counts().unwrap().get("observations"), Some(&1));

        let coverage = store.coverage().unwrap();
        assert_eq!(coverage["git"].outcome, SweepOutcome::Ok);
        assert_eq!(coverage["git"].stats.get("new"), Some(&2));
        // A running sweep is not coverage: freshness must not claim an
        // answer newer than the evidence behind it.
        store.begin_sweep("git", &["proj_2".into()], later).unwrap();
        assert_eq!(store.coverage().unwrap()["git"].id, sweep.id);
        let by_project = store.coverage_by_project().unwrap();
        assert_eq!(by_project["git"]["proj_1"], later);
        assert!(!by_project["git"].contains_key("proj_2"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_withheld_call_keeps_its_excerpt_redacted_and_a_rebind_is_refused() {
        let dir = std::env::temp_dir().join(format!("vogt-activity-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = store(&dir);
        let now = moment();
        let sweep = store.begin_sweep("activity", &[], now).unwrap();
        let batch = ActivityBatch {
            calls: vec![ActivityCall {
                source_path: "/tmp/t.jsonl".into(),
                call_id: "c1".into(),
                agent: "claude".into(),
                agent_session_id: "ses_1".into(),
                cwd: Some("/work/proj".into()),
                tool: "bash".into(),
                summary: "env dump".into(),
                services: vec!["shell".into()],
                withheld: true,
                at: now,
            }],
            results: vec![ActivityResult {
                source_path: "/tmp/t.jsonl".into(),
                call_id: "c1".into(),
                error: false,
                excerpt: Some("TOKEN=secret".into()),
                at: Some(Moment::from_unix(1_700_000_002, 0)),
            }],
            cursors: vec![TranscriptCursor {
                path: "/tmp/t.jsonl".into(),
                agent: "claude".into(),
                offset: 40,
                size: 40,
                agent_session_id: Some("ses_1".into()),
                cwd: Some("/work/proj".into()),
            }],
            ..ActivityBatch::default()
        };
        let stats = store.index_activity(&sweep.id, &batch, now).unwrap();
        assert_eq!(
            stats,
            ActivityIndexStats {
                calls: 1,
                results: 1
            }
        );
        let repeated = store.index_activity(&sweep.id, &batch, now).unwrap();
        assert_eq!(repeated.calls, 0);

        let rows = store
            .search_activity(&ActivityQuery::default(), 10, 0)
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].excerpt.as_deref(), Some(WITHHELD));
        assert_eq!(rows[0].services, vec!["shell".to_string()]);

        let summary = store
            .summarize_activity(&ActivityQuery::default(), 10, 0)
            .unwrap();
        assert_eq!(summary.len(), 1);
        assert_eq!(summary[0].calls, 1);
        assert_eq!(summary[0].finished, 1);
        assert_eq!(summary[0].cwd.as_deref(), Some("/work/proj"));
        assert_eq!(summary[0].wait_ms, 2000);
        assert_eq!(summary[0].tools.get("bash"), Some(&1));

        let cursors = store.activity_cursors().unwrap();
        assert_eq!(cursors["/tmp/t.jsonl"].offset, 40);

        let error = store.rebind_instance("inst_2");
        assert!(error.is_ok());
        // Binding again is a constraint failure, the same one Python's
        // second INSERT raises; a clone uses rebind_instance instead.
        let conflict = store.bind_instance("inst_3");
        assert!(matches!(conflict, Err(VogtError::MigrationError(_))));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
