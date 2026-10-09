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
    /// `ref` in Python (`work.py` reads `.ref`). `r#ref` keeps the name.
    pub r#ref: String,
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
/// two must filter the same way. `limit` is required, as it is in Python, so
/// this does not implement `Default`. `count_audit` ignores `limit` and
/// `offset`: the total counts matches, not the page.
#[derive(Debug, Clone, PartialEq, Eq)]
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
    fn instance_id(&self) -> Result<String, crate::errors::VogtError>;
    fn clone_stamp(&self) -> Result<Option<CloneStamp>, crate::errors::VogtError>;
    fn current_revision(&self) -> Result<i64, crate::errors::VogtError>;
    fn latest_event_seq(&self) -> Result<i64, crate::errors::VogtError>;
    fn counts(&self) -> Result<Counts, crate::errors::VogtError>;

    fn actor_by_identity(
        &self,
        identity_ref: &str,
    ) -> Result<Option<Actor>, crate::errors::VogtError>;
    fn actor_by_id(&self, actor_id: &str) -> Result<Option<Actor>, crate::errors::VogtError>;
    fn list_actors(&self, limit: i64, offset: i64) -> Result<Vec<Actor>, crate::errors::VogtError>;

    fn project_by_slug(&self, slug: &str) -> Result<Option<Project>, crate::errors::VogtError>;
    fn project_by_id(&self, project_id: &str) -> Result<Option<Project>, crate::errors::VogtError>;
    fn list_projects(
        &self,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Project>, crate::errors::VogtError>;

    fn work_item_by_id(
        &self,
        work_item_id: &str,
    ) -> Result<Option<WorkItem>, crate::errors::VogtError>;
    fn work_item_by_ref(
        &self,
        reference: &str,
    ) -> Result<Option<WorkItem>, crate::errors::VogtError>;
    fn list_work_items(
        &self,
        filter: &WorkFilter,
    ) -> Result<Vec<WorkItem>, crate::errors::VogtError>;
    fn count_work_items(&self, filter: &WorkFilter) -> Result<i64, crate::errors::VogtError>;

    fn board_high_water(
        &self,
        filter: &WorkFilter,
    ) -> Result<Option<(Moment, String)>, crate::errors::VogtError>;
    fn board_counts(
        &self,
        filter: &WorkFilter,
        lane_mode: &str,
        high_water: Option<&(Moment, String)>,
    ) -> Result<BTreeMap<(String, String), i64>, crate::errors::VogtError>;
    fn board_work_items(
        &self,
        filter: &WorkFilter,
        lane_mode: &str,
        cells: &[BoardCellQuery],
        high_water: Option<&(Moment, String)>,
        limit: i64,
    ) -> Result<BTreeMap<(String, String), Vec<WorkItem>>, crate::errors::VogtError>;

    fn blocking_fan_out(
        &self,
        work_item_ids: &[String],
    ) -> Result<BTreeMap<String, i64>, crate::errors::VogtError>;
    fn unfinished_blockers(
        &self,
        work_item_id: &str,
        terminal_states: &[&str],
    ) -> Result<Vec<Blocker>, crate::errors::VogtError>;

    fn comments_for(
        &self,
        work_item_id: &str,
        limit: i64,
    ) -> Result<Vec<Comment>, crate::errors::VogtError>;
    fn label_by_name(&self, name: &str) -> Result<Option<Label>, crate::errors::VogtError>;
    fn list_labels(&self, limit: i64, offset: i64) -> Result<Vec<Label>, crate::errors::VogtError>;
    fn initiative_by_id(
        &self,
        initiative_id: &str,
    ) -> Result<Option<Initiative>, crate::errors::VogtError>;
    fn initiative_by_slug(
        &self,
        slug: &str,
    ) -> Result<Option<Initiative>, crate::errors::VogtError>;
    fn list_initiatives(
        &self,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Initiative>, crate::errors::VogtError>;
    fn workflow_for(&self, kind: &str) -> Result<Workflow, crate::errors::VogtError>;

    fn list_suppressions(
        &self,
        include_revoked: bool,
        limit: i64,
    ) -> Result<Vec<Suppression>, crate::errors::VogtError>;
    fn suppression_by_id(
        &self,
        suppression_id: &str,
    ) -> Result<Option<Suppression>, crate::errors::VogtError>;
    fn contract_exemptions(
        &self,
        project_id: &str,
    ) -> Result<Vec<ContractExemption>, crate::errors::VogtError>;

    fn work_links_for_subjects(
        &self,
        subject_keys: &[String],
    ) -> Result<BTreeMap<String, String>, crate::errors::VogtError>;
    fn work_links_for_subjects_by_item(
        &self,
        work_item_id: &str,
    ) -> Result<BTreeMap<String, String>, crate::errors::VogtError>;
    fn work_item_by_subject(
        &self,
        subject_key: &str,
    ) -> Result<Option<WorkItem>, crate::errors::VogtError>;
    fn work_overlay(
        &self,
        subject_key: &str,
    ) -> Result<Option<WorkOverlay>, crate::errors::VogtError>;
    fn work_overlays(
        &self,
        subject_keys: &[String],
    ) -> Result<BTreeMap<String, WorkOverlay>, crate::errors::VogtError>;
    fn bound_branch_overlays(
        &self,
        limit: i64,
    ) -> Result<Vec<WorkOverlay>, crate::errors::VogtError>;

    fn token_by_hash(&self, token_hash: &str) -> Result<Option<Token>, crate::errors::VogtError>;
    fn token_by_id(&self, token_id: &str) -> Result<Option<Token>, crate::errors::VogtError>;
    fn list_tokens(
        &self,
        include_revoked: bool,
        limit: i64,
    ) -> Result<Vec<Token>, crate::errors::VogtError>;
    fn tokens_for_actor(
        &self,
        actor_id: &str,
        include_revoked: bool,
    ) -> Result<Vec<Token>, crate::errors::VogtError>;
    /// Whether first-run install mode has closed: a latch row, a non-agent
    /// token, or a password login. Any one of them is enough.
    fn install_closed(&self) -> Result<bool, crate::errors::VogtError>;
    fn list_auth_decisions(
        &self,
        decision: Option<&str>,
        limit: i64,
    ) -> Result<Vec<AuthDecision>, crate::errors::VogtError>;

    fn password_credential_by_username(
        &self,
        username: &str,
    ) -> Result<Option<PasswordCredential>, crate::errors::VogtError>;
    fn password_credential_for_actor(
        &self,
        actor_id: &str,
    ) -> Result<Option<PasswordCredential>, crate::errors::VogtError>;
    fn password_hash(&self, actor_id: &str) -> Result<Option<String>, crate::errors::VogtError>;
    fn list_password_credentials(
        &self,
    ) -> Result<Vec<PasswordCredential>, crate::errors::VogtError>;

    fn forge_account(
        &self,
        actor_id: &str,
        host: &str,
    ) -> Result<Option<ForgeAccount>, crate::errors::VogtError>;
    fn forge_accounts_for_actor(
        &self,
        actor_id: &str,
    ) -> Result<Vec<ForgeAccount>, crate::errors::VogtError>;
    fn forge_account_secret(
        &self,
        actor_id: &str,
        host: &str,
    ) -> Result<Option<String>, crate::errors::VogtError>;

    fn list_drift(
        &self,
        status: Option<&str>,
        kind: Option<&str>,
        project_id: Option<&str>,
        limit: i64,
    ) -> Result<Vec<DriftProposal>, crate::errors::VogtError>;
    fn drift_by_id(
        &self,
        proposal_id: &str,
    ) -> Result<Option<DriftProposal>, crate::errors::VogtError>;
    fn open_drift_subjects(
        &self,
    ) -> Result<BTreeSet<(String, String, String)>, crate::errors::VogtError>;
    fn list_writeback_actions(
        &self,
        outcome: Option<&str>,
        limit: i64,
    ) -> Result<Vec<WriteBackRecord>, crate::errors::VogtError>;
    fn drift_evidence_ids(&self) -> Result<BTreeSet<String>, crate::errors::VogtError>;

    fn inbox_triage_by_key(
        &self,
        entry_key: &str,
    ) -> Result<Option<InboxTriage>, crate::errors::VogtError>;
    fn inbox_triage_by_keys(
        &self,
        entry_keys: &[String],
    ) -> Result<BTreeMap<String, InboxTriage>, crate::errors::VogtError>;
    fn list_inbox_triage(&self, limit: i64) -> Result<Vec<InboxTriage>, crate::errors::VogtError>;
    fn actor_preference(
        &self,
        actor_id: &str,
        key: &str,
    ) -> Result<Option<ActorPreference>, crate::errors::VogtError>;
    fn actor_preferences(
        &self,
        actor_id: &str,
    ) -> Result<Vec<ActorPreference>, crate::errors::VogtError>;

    fn session_by_id(
        &self,
        session_id: &str,
    ) -> Result<Option<CodingSession>, crate::errors::VogtError>;
    fn session_grant(
        &self,
        grant_id: &str,
    ) -> Result<Option<SessionGrant>, crate::errors::VogtError>;
    fn session_by_engine_id(
        &self,
        engine_session_id: &str,
    ) -> Result<Option<CodingSession>, crate::errors::VogtError>;
    fn list_session_grants(
        &self,
        state: Option<&str>,
        target_engine_session_id: Option<&str>,
        limit: i64,
    ) -> Result<Vec<SessionGrant>, crate::errors::VogtError>;
    fn list_sessions(
        &self,
        project_id: Option<&str>,
        work_item_id: Option<&str>,
        include_stopped: bool,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<CodingSession>, crate::errors::VogtError>;

    fn list_events(
        &self,
        after: i64,
        limit: i64,
        entity_id: Option<&str>,
    ) -> Result<Vec<Event>, crate::errors::VogtError>;
    fn list_audit(&self, query: &AuditQuery) -> Result<Vec<AuditRecord>, crate::errors::VogtError>;
    fn count_audit(&self, query: &AuditQuery) -> Result<i64, crate::errors::VogtError>;
}

/// A write transaction. Also a read view, so a writer never opens a second
/// connection to see what it just wrote.
pub trait WriteTxn: ReadView {
    /// Commit this transaction. Dropping it without committing rolls back,
    /// which is what Python's `write()` does when the body raises.
    fn commit(self) -> Result<(), crate::errors::VogtError>
    where
        Self: Sized;
    fn txn_id(&self) -> &str;
    fn revision(&self) -> i64;

    fn insert_actor(&mut self, actor: &Actor) -> Result<(), crate::errors::VogtError>;
    fn insert_project(&mut self, project: &Project) -> Result<(), crate::errors::VogtError>;
    fn update_project(
        &mut self,
        project_id: &str,
        update: &ProjectUpdate,
        at: Moment,
    ) -> Result<(), crate::errors::VogtError>;

    fn next_work_ref(&mut self) -> Result<String, crate::errors::VogtError>;
    fn insert_work_item(&mut self, item: &WorkItem) -> Result<(), crate::errors::VogtError>;
    fn update_work_item(
        &mut self,
        work_item_id: &str,
        update: &WorkItemUpdate,
        at: Moment,
    ) -> Result<(), crate::errors::VogtError>;

    fn insert_relation(
        &mut self,
        work_item_id: &str,
        related_id: &str,
        kind: RelationKind,
        at: Moment,
    ) -> Result<(), crate::errors::VogtError>;
    fn delete_relation(
        &mut self,
        work_item_id: &str,
        related_id: &str,
        kind: RelationKind,
    ) -> Result<bool, crate::errors::VogtError>;

    fn insert_label(&mut self, label: &Label) -> Result<(), crate::errors::VogtError>;
    fn insert_initiative(
        &mut self,
        initiative: &Initiative,
    ) -> Result<(), crate::errors::VogtError>;
    fn update_initiative(
        &mut self,
        initiative: &Initiative,
    ) -> Result<(), crate::errors::VogtError>;
    fn insert_comment(&mut self, comment: &Comment) -> Result<(), crate::errors::VogtError>;

    fn insert_suppression(
        &mut self,
        suppression: &Suppression,
    ) -> Result<(), crate::errors::VogtError>;
    fn revoke_suppression(
        &mut self,
        suppression_id: &str,
        actor_id: &str,
        reason: &str,
        at: Moment,
    ) -> Result<bool, crate::errors::VogtError>;
    fn insert_contract_exemption(
        &mut self,
        exemption: &ContractExemption,
    ) -> Result<(), crate::errors::VogtError>;
    fn delete_contract_exemption(
        &mut self,
        project_id: &str,
        rule: &str,
        target: &str,
    ) -> Result<bool, crate::errors::VogtError>;

    fn insert_work_link(&mut self, link: &WorkLink) -> Result<(), crate::errors::VogtError>;
    fn upsert_work_overlay(
        &mut self,
        overlay: &WorkOverlay,
    ) -> Result<(), crate::errors::VogtError>;

    fn insert_token(
        &mut self,
        token: &Token,
        token_hash: &str,
    ) -> Result<(), crate::errors::VogtError>;
    fn carry_credentials(
        &mut self,
        carried: &CarriedCredentials,
        reason: &str,
        at: Moment,
    ) -> Result<CarryReport, crate::errors::VogtError>;
    fn set_instance_identity(
        &mut self,
        instance_id: &str,
        stamp: &CloneStamp,
    ) -> Result<(), crate::errors::VogtError>;
    fn revoke_token(
        &mut self,
        token_id: &str,
        reason: &str,
        at: Moment,
    ) -> Result<bool, crate::errors::VogtError>;
    fn reinstate_token(&mut self, token_id: &str) -> Result<bool, crate::errors::VogtError>;

    fn upsert_password_credential(
        &mut self,
        actor_id: &str,
        username: &str,
        password_hash: &str,
        scopes: &[String],
        at: Moment,
    ) -> Result<(), crate::errors::VogtError>;
    fn delete_password_credential(
        &mut self,
        actor_id: &str,
    ) -> Result<bool, crate::errors::VogtError>;
    fn upsert_forge_account(
        &mut self,
        actor_id: &str,
        host: &str,
        login: &str,
        scopes: &str,
        encrypted_token: &str,
        at: Moment,
    ) -> Result<(), crate::errors::VogtError>;
    fn delete_forge_account(
        &mut self,
        actor_id: &str,
        host: &str,
    ) -> Result<bool, crate::errors::VogtError>;

    fn insert_writeback(
        &mut self,
        record: &WriteBackRecord,
    ) -> Result<(), crate::errors::VogtError>;
    fn insert_session(&mut self, session: &CodingSession) -> Result<(), crate::errors::VogtError>;
    fn set_session_stopped(
        &mut self,
        session_id: &str,
        stopped_at: Moment,
    ) -> Result<(), crate::errors::VogtError>;
    fn insert_session_grant(
        &mut self,
        grant: &SessionGrant,
    ) -> Result<(), crate::errors::VogtError>;
    fn update_session_grant(
        &mut self,
        grant: &SessionGrant,
    ) -> Result<(), crate::errors::VogtError>;
    fn set_session_work_item(
        &mut self,
        session_id: &str,
        work_item_id: Option<&str>,
    ) -> Result<(), crate::errors::VogtError>;
    fn mark_session_stopped(
        &mut self,
        session_id: &str,
        at: Moment,
    ) -> Result<(), crate::errors::VogtError>;

    fn insert_drift(&mut self, proposal: &DriftProposal) -> Result<(), crate::errors::VogtError>;
    fn upsert_inbox_triage(&mut self, triage: &InboxTriage)
        -> Result<(), crate::errors::VogtError>;
    fn upsert_actor_preference(
        &mut self,
        preference: &ActorPreference,
    ) -> Result<(), crate::errors::VogtError>;
    fn mark_drift_superseded(
        &mut self,
        proposal_id: &str,
        detail: Option<&str>,
        at: Option<Moment>,
    ) -> Result<bool, crate::errors::VogtError>;
    fn resolve_drift(
        &mut self,
        proposal_id: &str,
        status: &str,
        actor_id: &str,
        reason: &str,
        at: Moment,
    ) -> Result<bool, crate::errors::VogtError>;

    fn upsert_workflow(
        &mut self,
        workflow: &Workflow,
        at: Moment,
    ) -> Result<(), crate::errors::VogtError>;
    // The field list is Python's signature, not a struct: the transaction
    // allocates the id and stamps the revision, so the caller cannot pass them.
    #[allow(clippy::too_many_arguments)]
    fn append_audit(
        &mut self,
        actor: &Actor,
        operation: &str,
        entity_kind: &str,
        entity_id: &str,
        reason: &str,
        payload_digest: &str,
        at: Moment,
    ) -> Result<AuditRecord, crate::errors::VogtError>;
    #[allow(clippy::too_many_arguments)]
    fn append_event(
        &mut self,
        kind: &str,
        entity_kind: &str,
        entity_id: &str,
        actor_id: Option<&str>,
        audit_id: Option<&str>,
        summary: &serde_json::Value,
        at: Moment,
    ) -> Result<Event, crate::errors::VogtError>;
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
    fn credentials(&self) -> Result<CarriedCredentials, crate::errors::VogtError>;
    fn record_auth_decision(&self, decision: &AuthDecision)
        -> Result<(), crate::errors::VogtError>;
    /// The store's clock, read once. A step clock ticks on every read, so a caller
    /// that already holds an instant must not call this for it.
    fn now(&self) -> crate::core::Moment;
    /// Whether the clock moves on every read. A step clock does, and a wall clock
    /// does not. The touch lands one step after the decision only on a clock that
    /// moves, and asking is cheaper than guessing from the type's name.
    fn clock_steps(&self) -> bool {
        false
    }
    /// A fresh id and nothing else. The caller has already read the clock for the
    /// decision's `at`, and a step clock ticks on every read, so drawing the id
    /// must not read it again.
    fn next_id(&self, prefix: &str) -> String;
    fn touch_token(
        &self,
        token_id: &str,
        at: Moment,
        expires_at: Option<Moment>,
    ) -> Result<(), crate::errors::VogtError>;
    fn prune_auth_decisions(
        &self,
        allow_before: Moment,
        deny_before: Moment,
    ) -> Result<i64, crate::errors::VogtError>;
    fn publish_event(
        &self,
        kind: &str,
        entity_kind: &str,
        entity_id: &str,
        summary: &serde_json::Value,
        at: Moment,
    ) -> Result<Event, crate::errors::VogtError>;
    fn read(&self) -> Result<Self::Read<'_>, crate::errors::VogtError>;
    fn write(&self) -> Result<Self::Write<'_>, crate::errors::VogtError>;
}

/// The observed store: append-oriented evidence plus collector coverage.
pub trait ObservedStore {
    fn migrate(&self) -> Result<MigrationReport, crate::errors::VogtError>;
    fn is_initialized(&self) -> bool;
    fn schema_version(&self) -> i64;
    fn bundled_schema_version(&self) -> i64;

    fn bind_instance(&self, instance_id: &str) -> Result<(), crate::errors::VogtError>;
    fn rebind_instance(&self, instance_id: &str) -> Result<(), crate::errors::VogtError>;
    fn instance_id(&self) -> Result<Option<String>, crate::errors::VogtError>;
    fn has_evidence_tables(&self) -> Result<bool, crate::errors::VogtError>;

    fn begin_sweep(
        &self,
        collector: &str,
        scope: &[String],
        at: Moment,
    ) -> Result<Sweep, crate::errors::VogtError>;
    fn finish_sweep(
        &self,
        sweep_id: &str,
        outcome: SweepOutcome,
        stats: &[(&str, i64)],
        at: Moment,
        detail: Option<&str>,
    ) -> Result<(), crate::errors::VogtError>;
    fn append(
        &self,
        sweep_id: &str,
        findings: &[PendingObservation],
        at: Moment,
    ) -> Result<AppendStats, crate::errors::VogtError>;
    fn list_sweeps(
        &self,
        collector: Option<&str>,
        limit: i64,
    ) -> Result<Vec<Sweep>, crate::errors::VogtError>;
    fn coverage(&self) -> Result<BTreeMap<String, Sweep>, crate::errors::VogtError>;
    fn coverage_by_project(
        &self,
    ) -> Result<BTreeMap<String, BTreeMap<String, Moment>>, crate::errors::VogtError>;
    fn fail_sweeps(
        &self,
        sweep_ids: &[String],
        detail: &str,
    ) -> Result<(), crate::errors::VogtError>;

    fn list_observations(
        &self,
        kind: Option<&str>,
        project_id: Option<&str>,
        subject_key: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Observation>, crate::errors::VogtError>;
    fn latest(
        &self,
        kinds: &[String],
        project_id: Option<&str>,
        promoted_only: bool,
        exclude_closed: bool,
        limit: i64,
    ) -> Result<Vec<Observation>, crate::errors::VogtError>;
    fn latest_by_subject(
        &self,
        subject_key: &str,
    ) -> Result<Option<Observation>, crate::errors::VogtError>;
    fn count_closed(
        &self,
        kinds: &[String],
        project_id: Option<&str>,
    ) -> Result<i64, crate::errors::VogtError>;

    fn get_watermark(
        &self,
        collector: &str,
        project_id: &str,
    ) -> Result<Option<String>, crate::errors::VogtError>;
    fn set_watermark(
        &self,
        collector: &str,
        project_id: &str,
        watermark: Option<&str>,
        at: Moment,
    ) -> Result<(), crate::errors::VogtError>;
    fn touch_subjects(
        &self,
        subject_keys: &[String],
        at: Moment,
    ) -> Result<(), crate::errors::VogtError>;
    fn last_confirmed(
        &self,
        subject_keys: &[String],
    ) -> Result<BTreeMap<String, Moment>, crate::errors::VogtError>;

    fn dep_refs(
        &self,
        from_project_id: Option<&str>,
        to_project_id: Option<&str>,
    ) -> Result<Vec<DepRef>, crate::errors::VogtError>;
    fn counts(&self) -> Result<BTreeMap<String, i64>, crate::errors::VogtError>;
    fn rebuild_latest(&self) -> Result<i64, crate::errors::VogtError>;
    fn replace_dep_refs(&self, rows: &[DepRefRow]) -> Result<i64, crate::errors::VogtError>;
    fn prune(
        &self,
        before: Moment,
        protected_observation_ids: &BTreeSet<String>,
    ) -> Result<PruneReport, crate::errors::VogtError>;

    fn activity_cursors(
        &self,
    ) -> Result<BTreeMap<String, TranscriptCursor>, crate::errors::VogtError>;
    fn index_activity(
        &self,
        sweep_id: &str,
        batch: &ActivityBatch,
        at: Moment,
    ) -> Result<ActivityIndexStats, crate::errors::VogtError>;
    fn search_activity(
        &self,
        query: &ActivityQuery,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<ActivityEventRow>, crate::errors::VogtError>;
    fn summarize_activity(
        &self,
        query: &ActivityQuery,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<ActivitySessionRow>, crate::errors::VogtError>;
}
