//! The storage interface the application layer is allowed to know about.
//!
//! Ports `src/vogt/storage/interface.py`. Nothing above this line may depend on
//! SQLite-only semantics, so a Postgres backend stays possible behind the same
//! interface. Concretely that means no rowids, no `INSERT OR REPLACE`, no
//! dynamic typing, and no SQL anywhere outside a backend package.
//!
//! Traits rather than protocols. A backend implements them; this module holds
//! no SQL. Methods the later store chunks have not filled in yet are still
//! named here, because an operation absent from the interface is absent from
//! the product.

use std::collections::{BTreeMap, BTreeSet};

use crate::core::{
    Actor, ActorPreference, AuditRecord, AuthDecision, CodingSession, Comment, ContractExemption,
    DepRef, DriftProposal, Event, ForgeAccount, InboxTriage, Initiative, Label, Moment,
    Observation, PasswordCredential, Principal, Project, RelationKind, SessionGrant, Suppression,
    Sweep, SweepOutcome, Token, WorkItem, WorkLink, WorkOverlay, Workflow, WriteBackRecord,
};
use crate::storage::observed_types::{
    ActivityBatch, ActivityEventRow, ActivityIndexStats, ActivityQuery, ActivitySessionRow,
    AppendStats, DepRefRow, PendingObservation, PruneReport, TranscriptCursor,
};

/// What `migrate()` did, so `status` and CI can say it plainly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationReport {
    pub store: String,
    pub applied: Vec<String>,
    pub version: i64,
}

/// Row counts behind `status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counts {
    pub projects: i64,
    pub actors: i64,
    pub events: i64,
    pub audit: i64,
    pub work_items: i64,
    pub initiatives: i64,
}

/// That this instance's data came from another instance's backup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloneStamp {
    pub source_instance_id: String,
    pub cloned_at: Moment,
    pub backup_taken_at: Moment,
}

/// An instance's own credentials, secrets included, held across a clone.
///
/// The rows are opaque column maps on purpose: the hashes and ciphertext they
/// hold are copied, never interpreted, and this type never reaches a result
/// model.
#[derive(Debug, Clone, PartialEq)]
pub struct CarriedCredentials {
    pub actors: Vec<Actor>,
    pub tokens: Vec<serde_json::Value>,
    pub password_credentials: Vec<serde_json::Value>,
    pub forge_accounts: Vec<serde_json::Value>,
}

/// What `carry_credentials` did to the cloned copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CarryReport {
    pub tokens_kept: i64,
    pub source_tokens_revoked: i64,
    pub password_logins_kept: i64,
    pub source_password_logins_dropped: i64,
    pub forge_accounts_kept: i64,
    pub source_forge_accounts_dropped: i64,
    pub actors_added: i64,
}

/// The outcome of creating an instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootstrapResult {
    pub instance_id: String,
    pub actor: Actor,
}

/// How the views narrow the work set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkFilter {
    pub project_id: Option<String>,
    pub kinds: Vec<String>,
    pub states: Vec<String>,
    pub priorities: Vec<String>,
    pub assignee_actor_id: Option<String>,
    pub initiative_id: Option<String>,
    pub label: Option<String>,
    pub trust_states: Vec<String>,
    /// Case-insensitive text the title, body or ref must contain. `None` (and
    /// the empty string) narrows nothing.
    pub text: Option<String>,
    pub exclude_terminal: bool,
    /// Leave out native rows whose project is unlinked. Off by default so
    /// `work.list`'s raw global query stays complete.
    pub exclude_unlinked_native: bool,
    /// Retired rows are excluded from every work view by default. Export is
    /// the one reader that wants everything.
    pub include_superseded: bool,
    pub limit: i64,
    pub offset: i64,
}

impl Default for WorkFilter {
    fn default() -> Self {
        Self {
            project_id: None,
            kinds: Vec::new(),
            states: Vec::new(),
            priorities: Vec::new(),
            assignee_actor_id: None,
            initiative_id: None,
            label: None,
            trust_states: Vec::new(),
            text: None,
            exclude_terminal: false,
            exclude_unlinked_native: false,
            include_superseded: false,
            limit: 100,
            offset: 0,
        }
    }
}

/// One independently continued Board cell inside a batched read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoardCellQuery {
    pub lane_key: String,
    pub state: String,
    pub after_created_at: Option<Moment>,
    pub after_ref: Option<String>,
}

/// An unfinished `depends_on` target, named so a rejection can list it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Blocker {
    pub reference: String,
    pub state: String,
}

/// Fields of a project a write may change. Unset fields are untouched.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProjectUpdate {
    pub lifecycle_state: Option<String>,
    pub repo_url: Option<String>,
    pub current_version: Option<String>,
    pub compliance_status: Option<String>,
    pub compliance_checked_at: Option<Moment>,
    pub write_back: Option<String>,
    pub link_state: Option<String>,
    pub exclusions: Option<Vec<String>>,
    pub contract_adopted_at: Option<Moment>,
    /// Adoption is reversible, and `None` already means "leave alone", so
    /// declining the contract needs a flag of its own.
    pub clear_contract_adopted_at: bool,
}

/// Fields of a work item a write may change. Unset fields are untouched.
///
/// `None` means "leave alone"; clearing a nullable field is expressed by the
/// matching `clear_*` flag.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WorkItemUpdate {
    pub title: Option<String>,
    pub body: Option<String>,
    pub state: Option<String>,
    pub priority: Option<String>,
    pub effort: Option<String>,
    pub assignee_actor_id: Option<String>,
    pub initiative_id: Option<String>,
    pub project_id: Option<String>,
    pub clear_effort: bool,
    pub clear_assignee: bool,
    pub clear_initiative: bool,
    pub add_labels: Vec<String>,
    pub remove_labels: Vec<String>,
    /// The retire marker: the subject key a migrated native item became.
    pub superseded_by: Option<String>,
}

/// How `list_audit` and `count_audit` narrow the ledger. One object because the
/// two must filter the same way.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AuditQuery {
    pub limit: i64,
    pub offset: i64,
    pub actor_id: Option<String>,
    pub operation: Option<String>,
    pub entity_id: Option<String>,
    pub project_id: Option<String>,
    pub since: Option<Moment>,
    pub until: Option<Moment>,
}

/// Reads. A transaction and a store's read handle both implement this.
pub trait ReadView {
    fn instance_id(&self) -> String;
    fn clone_stamp(&self) -> Option<CloneStamp>;
    fn current_revision(&self) -> i64;
    fn latest_event_seq(&self) -> i64;
    fn counts(&self) -> Counts;

    fn actor_by_identity(&self, identity_ref: &str) -> Option<Actor>;
    fn actor_by_id(&self, actor_id: &str) -> Option<Actor>;
    fn list_actors(&self, limit: i64, offset: i64) -> Vec<Actor>;

    fn project_by_slug(&self, slug: &str) -> Option<Project>;
    fn project_by_id(&self, project_id: &str) -> Option<Project>;
    fn list_projects(&self, limit: i64, offset: i64) -> Vec<Project>;

    fn work_item_by_id(&self, work_item_id: &str) -> Option<WorkItem>;
    fn work_item_by_ref(&self, reference: &str) -> Option<WorkItem>;
    fn list_work_items(&self, filter: &WorkFilter) -> Vec<WorkItem>;
    fn count_work_items(&self, filter: &WorkFilter) -> i64;

    fn board_high_water(&self, filter: &WorkFilter) -> Option<(Moment, String)>;
    fn board_counts(
        &self,
        filter: &WorkFilter,
        lane_mode: &str,
        high_water: Option<&(Moment, String)>,
    ) -> BTreeMap<(String, String), i64>;
    fn board_work_items(
        &self,
        filter: &WorkFilter,
        lane_mode: &str,
        cells: &[BoardCellQuery],
        high_water: Option<&(Moment, String)>,
        limit: i64,
    ) -> BTreeMap<(String, String), Vec<WorkItem>>;

    fn blocking_fan_out(&self, work_item_ids: &[String]) -> BTreeMap<String, i64>;
    fn unfinished_blockers(&self, work_item_id: &str, terminal_states: &[&str]) -> Vec<Blocker>;

    fn comments_for(&self, work_item_id: &str, limit: i64) -> Vec<Comment>;
    fn label_by_name(&self, name: &str) -> Option<Label>;
    fn list_labels(&self, limit: i64, offset: i64) -> Vec<Label>;
    fn initiative_by_id(&self, initiative_id: &str) -> Option<Initiative>;
    fn initiative_by_slug(&self, slug: &str) -> Option<Initiative>;
    fn list_initiatives(&self, limit: i64, offset: i64) -> Vec<Initiative>;
    fn workflow_for(&self, kind: &str) -> Workflow;

    fn list_suppressions(&self, include_revoked: bool, limit: i64) -> Vec<Suppression>;
    fn suppression_by_id(&self, suppression_id: &str) -> Option<Suppression>;
    fn contract_exemptions(&self, project_id: &str) -> Vec<ContractExemption>;

    fn work_links_for_subjects(&self, subject_keys: &[String]) -> BTreeMap<String, String>;
    fn work_links_for_subjects_by_item(&self, work_item_id: &str) -> BTreeMap<String, String>;
    fn work_item_by_subject(&self, subject_key: &str) -> Option<WorkItem>;
    fn work_overlay(&self, subject_key: &str) -> Option<WorkOverlay>;
    fn work_overlays(&self, subject_keys: &[String]) -> BTreeMap<String, WorkOverlay>;
    fn bound_branch_overlays(&self, limit: i64) -> Vec<WorkOverlay>;

    fn token_by_hash(&self, token_hash: &str) -> Option<Token>;
    fn token_by_id(&self, token_id: &str) -> Option<Token>;
    fn list_tokens(&self, include_revoked: bool, limit: i64) -> Vec<Token>;
    fn tokens_for_actor(&self, actor_id: &str, include_revoked: bool) -> Vec<Token>;
    fn list_auth_decisions(&self, decision: Option<&str>, limit: i64) -> Vec<AuthDecision>;

    fn password_credential_by_username(&self, username: &str) -> Option<PasswordCredential>;
    fn password_credential_for_actor(&self, actor_id: &str) -> Option<PasswordCredential>;
    fn password_hash(&self, actor_id: &str) -> Option<String>;
    fn list_password_credentials(&self) -> Vec<PasswordCredential>;

    fn forge_account(&self, actor_id: &str, host: &str) -> Option<ForgeAccount>;
    fn forge_accounts_for_actor(&self, actor_id: &str) -> Vec<ForgeAccount>;
    fn forge_account_secret(&self, actor_id: &str, host: &str) -> Option<String>;

    fn list_drift(
        &self,
        status: &str,
        kind: Option<&str>,
        project_id: Option<&str>,
        limit: i64,
    ) -> Vec<DriftProposal>;
    fn drift_by_id(&self, proposal_id: &str) -> Option<DriftProposal>;
    fn open_drift_subjects(&self) -> BTreeSet<(String, String, String)>;
    fn list_writeback_actions(&self, outcome: Option<&str>, limit: i64) -> Vec<WriteBackRecord>;
    fn drift_evidence_ids(&self) -> BTreeSet<String>;

    fn inbox_triage_by_key(&self, entry_key: &str) -> Option<InboxTriage>;
    fn inbox_triage_by_keys(&self, entry_keys: &[String]) -> BTreeMap<String, InboxTriage>;
    fn list_inbox_triage(&self, limit: i64) -> Vec<InboxTriage>;
    fn actor_preference(&self, actor_id: &str, key: &str) -> Option<ActorPreference>;
    fn actor_preferences(&self, actor_id: &str) -> Vec<ActorPreference>;

    fn session_by_id(&self, session_id: &str) -> Option<CodingSession>;
    fn session_grant(&self, grant_id: &str) -> Option<SessionGrant>;
    fn session_by_engine_id(&self, engine_session_id: &str) -> Option<CodingSession>;
    fn list_session_grants(
        &self,
        state: Option<&str>,
        target_engine_session_id: Option<&str>,
        limit: i64,
    ) -> Vec<SessionGrant>;
    fn list_sessions(
        &self,
        project_id: Option<&str>,
        work_item_id: Option<&str>,
        include_stopped: bool,
        limit: i64,
        offset: i64,
    ) -> Vec<CodingSession>;

    fn list_events(&self, after: i64, limit: i64, entity_id: Option<&str>) -> Vec<Event>;
    fn list_audit(&self, query: &AuditQuery) -> Vec<AuditRecord>;
    fn count_audit(&self, query: &AuditQuery) -> i64;
}

/// A write transaction. Also a read view, so a writer never opens a second
/// connection to see what it just wrote.
pub trait WriteTxn: ReadView {
    fn txn_id(&self) -> &str;
    fn revision(&self) -> i64;

    fn insert_actor(&mut self, actor: &Actor);
    fn insert_project(&mut self, project: &Project);
    fn update_project(&mut self, project_id: &str, update: &ProjectUpdate, at: Moment);

    fn next_work_ref(&mut self) -> String;
    fn insert_work_item(&mut self, item: &WorkItem);
    fn update_work_item(&mut self, work_item_id: &str, update: &WorkItemUpdate, at: Moment);

    fn insert_relation(
        &mut self,
        work_item_id: &str,
        related_id: &str,
        kind: RelationKind,
        at: Moment,
    );
    fn delete_relation(&mut self, work_item_id: &str, related_id: &str, kind: RelationKind)
        -> bool;

    fn insert_label(&mut self, label: &Label);
    fn insert_initiative(&mut self, initiative: &Initiative);
    fn update_initiative(&mut self, initiative: &Initiative);
    fn insert_comment(&mut self, comment: &Comment);

    fn insert_suppression(&mut self, suppression: &Suppression);
    fn revoke_suppression(&mut self, suppression_id: &str, at: Moment) -> bool;
    fn insert_contract_exemption(&mut self, exemption: &ContractExemption);
    fn delete_contract_exemption(&mut self, project_id: &str, rule: &str, target: &str) -> bool;

    fn insert_work_link(&mut self, link: &WorkLink);
    fn upsert_work_overlay(&mut self, overlay: &WorkOverlay);

    fn insert_token(&mut self, token: &Token, token_hash: &str);
    fn carry_credentials(
        &mut self,
        carried: &CarriedCredentials,
        reason: &str,
        at: Moment,
    ) -> CarryReport;
    fn set_instance_identity(&mut self, instance_id: &str, stamp: &CloneStamp);
    fn revoke_token(&mut self, token_id: &str, at: Moment) -> bool;
    fn reinstate_token(&mut self, token_id: &str) -> bool;

    fn upsert_password_credential(
        &mut self,
        actor_id: &str,
        username: &str,
        password_hash: &str,
        scopes: &str,
        at: Moment,
    );
    fn delete_password_credential(&mut self, actor_id: &str) -> bool;
    fn upsert_forge_account(
        &mut self,
        actor_id: &str,
        host: &str,
        login: &str,
        scopes: &str,
        encrypted_token: &str,
        at: Moment,
    );
    fn delete_forge_account(&mut self, actor_id: &str, host: &str) -> bool;

    fn insert_writeback(&mut self, record: &WriteBackRecord);
    fn insert_session(&mut self, session: &CodingSession);
    fn insert_session_grant(&mut self, grant: &SessionGrant);
    fn update_session_grant(&mut self, grant: &SessionGrant);
    fn set_session_work_item(&mut self, session_id: &str, work_item_id: Option<&str>);
    fn mark_session_stopped(&mut self, session_id: &str, at: Moment);

    fn insert_drift(&mut self, proposal: &DriftProposal);
    fn upsert_inbox_triage(&mut self, triage: &InboxTriage);
    fn upsert_actor_preference(&mut self, preference: &ActorPreference);
    fn mark_drift_superseded(&mut self, proposal_id: &str, at: Moment) -> bool;
    fn resolve_drift(&mut self, proposal_id: &str, at: Moment) -> bool;

    fn upsert_workflow(&mut self, workflow: &Workflow, at: Moment);
    fn append_audit(&mut self, record: &AuditRecord) -> AuditRecord;
    fn append_event(&mut self, event: &Event) -> Event;
}

/// The declared store: the write plane.
pub trait DeclaredStore {
    type Read<'a>: ReadView
    where
        Self: 'a;
    type Write<'a>: WriteTxn
    where
        Self: 'a;

    fn migrate(&self) -> Result<MigrationReport, crate::errors::VogtError>;
    fn is_initialized(&self) -> bool;
    fn schema_version(&self) -> i64;
    fn bundled_schema_version(&self) -> i64;
    fn bootstrap(&self, principal: &Principal)
        -> Result<BootstrapResult, crate::errors::VogtError>;
    fn credentials(&self) -> CarriedCredentials;
    fn record_auth_decision(&self, decision: &AuthDecision);
    fn touch_token(&self, token_id: &str, at: Moment, expires_at: Option<Moment>);
    fn prune_auth_decisions(&self, allow_before: Moment, deny_before: Moment) -> i64;
    fn publish_event(
        &self,
        kind: &str,
        entity_kind: &str,
        entity_id: &str,
        summary: &str,
        at: Moment,
    ) -> Event;
    fn read(&self) -> Self::Read<'_>;
    fn write(&self) -> Self::Write<'_>;
}

/// The observed store: append-oriented evidence plus collector coverage.
pub trait ObservedStore {
    fn migrate(&self) -> Result<MigrationReport, crate::errors::VogtError>;
    fn is_initialized(&self) -> bool;
    fn schema_version(&self) -> i64;
    fn bundled_schema_version(&self) -> i64;

    fn bind_instance(&self, instance_id: &str);
    fn rebind_instance(&self, instance_id: &str);
    fn instance_id(&self) -> Option<String>;
    fn has_evidence_tables(&self) -> bool;

    fn begin_sweep(&self, collector: &str, scope: &[String], at: Moment) -> Sweep;
    fn finish_sweep(
        &self,
        sweep_id: &str,
        outcome: SweepOutcome,
        stats: &AppendStats,
        at: Moment,
        detail: Option<&str>,
    );
    fn append(&self, sweep_id: &str, findings: &[PendingObservation], at: Moment) -> AppendStats;
    fn list_sweeps(&self, collector: Option<&str>, limit: i64) -> Vec<Sweep>;
    fn coverage(&self) -> BTreeMap<String, Sweep>;
    fn coverage_by_project(&self) -> BTreeMap<String, BTreeMap<String, Moment>>;
    fn fail_sweeps(&self, sweep_ids: &[String], detail: &str);

    fn list_observations(
        &self,
        kind: Option<&str>,
        project_id: Option<&str>,
        subject_key: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> Vec<Observation>;
    fn latest(
        &self,
        kinds: &[String],
        project_id: Option<&str>,
        promoted_only: bool,
        exclude_closed: bool,
        limit: i64,
    ) -> Vec<Observation>;
    fn latest_by_subject(&self, subject_key: &str) -> Option<Observation>;
    fn count_closed(&self, kinds: &[String], project_id: Option<&str>) -> i64;

    fn get_watermark(&self, collector: &str, project_id: &str) -> Option<String>;
    fn set_watermark(&self, collector: &str, project_id: &str, watermark: &str, at: Moment);
    fn touch_subjects(&self, subject_keys: &[String], at: Moment);
    fn last_confirmed(&self, subject_keys: &[String]) -> BTreeMap<String, Moment>;

    fn dep_refs(&self, from_project_id: Option<&str>, to_project_id: Option<&str>) -> Vec<DepRef>;
    fn counts(&self) -> BTreeMap<String, i64>;
    fn rebuild_latest(&self) -> i64;
    fn replace_dep_refs(&self, rows: &[DepRefRow]) -> i64;
    fn prune(&self, before: Moment, protected_observation_ids: &BTreeSet<String>) -> PruneReport;

    fn activity_cursors(&self) -> BTreeMap<String, TranscriptCursor>;
    fn index_activity(
        &self,
        sweep_id: &str,
        batch: &ActivityBatch,
        at: Moment,
    ) -> ActivityIndexStats;
    fn search_activity(
        &self,
        query: &ActivityQuery,
        limit: i64,
        offset: i64,
    ) -> Vec<ActivityEventRow>;
    fn summarize_activity(
        &self,
        query: &ActivityQuery,
        limit: i64,
        offset: i64,
    ) -> Vec<ActivitySessionRow>;
}
