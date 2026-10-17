//! The entity views the operations return: labels, actors, logins, tokens,
//! auth decisions, suppressions, workflows, preferences and ranking
//! contributions.
//!
//! Ports the `Label`, `Actor`, `PasswordCredential`, `Token`, `AuthDecision`
//! and `Suppression` entities from `src/vogt/core/entities.py`, and the
//! views from `src/vogt/application/models.py`. They are shared by several
//! operations, which is why they live here rather than beside one.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{date_time, integer, number, open_object, optional_date_time};

fn token_kind_api() -> TokenKind {
    TokenKind::Api
}

// `ActorKind` in Python: a literal, so the schema is inline.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[schemars(inline)]
pub enum ActorKind {
    #[serde(rename = "human")]
    Human,
    #[serde(rename = "agent")]
    Agent,
}

// `Literal["api", "session", "agent"]` in Python, inline.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[schemars(inline)]
pub enum TokenKind {
    #[serde(rename = "api")]
    Api,
    #[serde(rename = "session")]
    Session,
    #[serde(rename = "agent")]
    Agent,
}

// `Literal["allow", "deny"]` in Python, inline.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[schemars(inline)]
pub enum DecisionKind {
    #[serde(rename = "allow")]
    Allow,
    #[serde(rename = "deny")]
    Deny,
}

// `Literal["exact", "pattern"]` in Python, inline.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[schemars(inline)]
pub enum MatchKind {
    #[serde(rename = "exact")]
    Exact,
    #[serde(rename = "pattern")]
    Pattern,
}

/// A free-form tag, shared per instance and GitHub-label aligned.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Label {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub color: Option<String>,
    #[schemars(schema_with = "date_time")]
    pub created_at: String,
}

/// A human or an agent. Both are first-class.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Actor {
    pub id: String,
    pub kind: ActorKind,
    pub display_name: String,
    pub identity_ref: String,
    #[serde(default)]
    pub disabled: bool,
    #[schemars(schema_with = "date_time")]
    pub created_at: String,
}

/// A human's login: a username, the scopes a login grants, and nothing else.
///
/// The hash is deliberately not on the model. It is written and read by the
/// storage layer alone, so no listing, result or audit payload can carry it.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PasswordCredential {
    pub actor_id: String,
    #[serde(default)]
    pub actor_identity_ref: Option<String>,
    pub username: String,
    pub scopes: Vec<String>,
    #[schemars(schema_with = "date_time")]
    pub created_at: String,
    #[schemars(schema_with = "date_time")]
    pub updated_at: String,
}

/// A scoped credential bound to an actor.
///
/// Never carries the secret: only the hash is stored, and the secret is
/// shown once at issue time. A model that could round-trip the credential
/// is a model that leaks it into logs and API responses.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Token {
    pub id: String,
    pub actor_id: String,
    #[serde(default)]
    pub actor_identity_ref: Option<String>,
    pub name: String,
    pub scopes: Vec<String>,
    // `api` for a minted or adopted token, `session` for one a password login
    // produced (expiring, revoked by `auth.logout`), `agent` for a coding
    // session's own token. Checked identically at authentication.
    #[serde(default = "token_kind_api")]
    #[schemars(extend("default" = "api"))]
    pub kind: TokenKind,
    #[schemars(schema_with = "date_time")]
    pub created_at: String,
    #[serde(default)]
    #[schemars(schema_with = "optional_date_time")]
    pub expires_at: Option<String>,
    #[serde(default)]
    #[schemars(schema_with = "optional_date_time")]
    pub last_used_at: Option<String>,
    #[serde(default)]
    #[schemars(schema_with = "optional_date_time")]
    pub revoked_at: Option<String>,
    #[serde(default)]
    pub revoked_reason: Option<String>,
}

/// One allow or deny, recorded.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AuthDecision {
    pub id: String,
    #[schemars(schema_with = "date_time")]
    pub at: String,
    pub decision: DecisionKind,
    pub reason_code: String,
    pub operation: String,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub actor_id: Option<String>,
    #[serde(default)]
    pub token_id: Option<String>,
    #[serde(default)]
    pub identity_ref: Option<String>,
    pub transport: String,
    #[serde(default)]
    pub detail: Option<String>,
}

/// An audited decision that an observed subject is not work.
///
/// Lives in the declared store because it is a decision, not an
/// observation — which is exactly why it survives re-observation of the same
/// subject, and why a dismissal recorded in the evidence store could never
/// have worked.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Suppression {
    pub id: String,
    pub match_kind: MatchKind,
    pub subject_key_or_pattern: String,
    #[serde(default)]
    pub scope_project_id: Option<String>,
    #[serde(default)]
    pub scope_project_slug: Option<String>,
    pub actor_id: String,
    #[serde(default)]
    pub actor_identity_ref: Option<String>,
    pub reason: String,
    #[schemars(schema_with = "date_time")]
    pub created_at: String,
    #[serde(default)]
    #[schemars(schema_with = "optional_date_time")]
    pub revoked_at: Option<String>,
    #[serde(default)]
    pub revoked_reason: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkflowView {
    pub kind: String,
    pub initial_state: String,
    pub states: Vec<String>,
    pub transitions: BTreeMap<String, Vec<String>>,
}

/// One of the caller's own settings.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PreferenceView {
    pub key: String,
    #[schemars(schema_with = "open_object")]
    pub value: serde_json::Map<String, serde_json::Value>,
    #[schemars(schema_with = "integer")]
    pub version: i64,
    #[schemars(schema_with = "date_time")]
    pub updated_at: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ContributionView {
    pub input: String,
    pub detail: String,
    #[schemars(schema_with = "number")]
    pub value: f64,
    #[schemars(schema_with = "number")]
    pub weight: f64,
    #[schemars(schema_with = "number")]
    pub contribution: f64,
}

// `Literal["incubating", "active", "maintenance", "archived"]` in Python.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(inline)]
pub enum LifecycleState {
    Incubating,
    Active,
    Maintenance,
    Archived,
}

// `Literal["compliant", "non_compliant", "not_checked", "not_applicable"]`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(inline)]
pub enum ComplianceStatus {
    Compliant,
    NonCompliant,
    NotChecked,
    NotApplicable,
}

// `Literal["none", "comment_only", "full"]` in Python, inline.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(inline)]
pub enum WriteBack {
    None,
    CommentOnly,
    Full,
}

// `Literal["unlinked", "linked"]` in Python, inline.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(inline)]
pub enum LinkState {
    Unlinked,
    Linked,
}

// `Literal["verified", "stale", "unverified", "disputed"]` in Python, inline.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(inline)]
pub enum TrustState {
    Verified,
    Stale,
    Unverified,
    Disputed,
}

// `Literal["push_branch", "open_pr"]` in Python, inline.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(inline)]
pub enum WorkerLandsBy {
    PushBranch,
    OpenPr,
}

// `Literal["oversight", "operator"]` in Python, inline.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(inline)]
pub enum MergeBy {
    Oversight,
    Operator,
}

// `Literal["fast_forward", "rebase"]` in Python, inline.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(inline)]
pub enum MergeMode {
    FastForward,
    Rebase,
}

pub(super) fn lifecycle_active() -> LifecycleState {
    LifecycleState::Active
}

pub(super) fn compliance_not_checked() -> ComplianceStatus {
    ComplianceStatus::NotChecked
}

pub(super) fn write_back_none() -> WriteBack {
    WriteBack::None
}

pub(super) fn link_unlinked() -> LinkState {
    LinkState::Unlinked
}

pub(super) fn trust_unverified() -> TrustState {
    TrustState::Unverified
}

pub(super) fn branch_pattern_default() -> String {
    "^wi-{n}-[a-z0-9-]+$".to_string()
}

pub(super) fn operator_default() -> String {
    "operator".to_string()
}

pub(super) fn default_true() -> bool {
    true
}

pub(super) fn review_template() -> String {
    "claude".to_string()
}

pub(super) fn review_model() -> String {
    "opus".to_string()
}

pub(super) fn review_effort() -> String {
    "low".to_string()
}

pub(super) fn review_budget_turns() -> i64 {
    40
}

/// One explicitly registered repository or folder.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Project {
    pub id: String,
    pub slug: String,
    pub name: String,
    pub root_path: String,
    #[serde(default)]
    pub repo_url: Option<String>,
    #[serde(default = "lifecycle_active")]
    pub lifecycle_state: LifecycleState,
    #[serde(default)]
    pub current_version: Option<String>,
    #[serde(default)]
    pub contract_version: Option<String>,
    #[serde(default = "compliance_not_checked")]
    pub compliance_status: ComplianceStatus,
    #[serde(default)]
    #[schemars(schema_with = "optional_date_time")]
    pub compliance_checked_at: Option<String>,
    #[serde(default)]
    pub agent_policy: AgentPolicy,
    #[serde(default)]
    #[schemars(schema_with = "optional_date_time")]
    pub contract_adopted_at: Option<String>,
    #[serde(default = "write_back_none")]
    pub write_back: WriteBack,
    #[serde(default = "link_unlinked")]
    pub link_state: LinkState,
    #[serde(default)]
    #[schemars(extend("x-keep-default" = true))]
    pub exclusions: Vec<String>,
    #[serde(default = "trust_unverified")]
    pub trust_state: TrustState,
    #[schemars(schema_with = "date_time")]
    pub created_at: String,
    #[schemars(schema_with = "date_time")]
    pub updated_at: String,
}

/// A project's agent policy, stored as data and shown in every session brief.
///
/// `forbidden_text` and `merge_deploys_prod` are the two facts the session
/// hooks refuse on. `landing`, `review` and `verification` are recorded so
/// the brief can state them; nothing enforces them yet. Everything else is
/// guidance the brief carries and nothing enforces: compliance stays a fact
/// to read, never a precondition.
#[derive(Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AgentPolicy {
    #[serde(default)]
    #[schemars(extend("x-keep-default" = true))]
    pub forbidden_text: Vec<String>,
    #[serde(default)]
    pub merge_deploys_prod: bool,
    #[serde(default)]
    #[schemars(extend("x-keep-default" = true))]
    pub main_branches: Vec<String>,
    #[serde(default)]
    pub base_branch: String,
    #[serde(default)]
    pub commit_identity: String,
    #[serde(default)]
    pub test_recipe: String,
    #[serde(default)]
    pub deployed_setup: String,
    #[serde(default)]
    #[schemars(extend("x-keep-default" = true))]
    pub rules: Vec<String>,
    #[serde(default)]
    pub session_wall_clock_minutes: Option<i64>,
    #[serde(default)]
    pub landing: Option<LandingPolicy>,
    #[serde(default)]
    pub review: Option<ReviewPolicy>,
    #[serde(default)]
    pub verification: Option<VerificationPolicy>,
}

/// How a worker's branch is cut and landed on this project.
///
/// Recorded so a brief can state the rule and, later, so the engine can
/// enforce it. Nothing here is enforced yet: a later item reads it.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LandingPolicy {
    pub base_branch: String,
    #[serde(default = "branch_pattern_default")]
    pub branch_pattern: String,
    pub worker_lands_by: WorkerLandsBy,
    pub merge_by: MergeBy,
    pub merge_mode: MergeMode,
    #[serde(default = "default_true")]
    pub linear_history: bool,
    #[serde(default = "operator_default")]
    pub deploy_by: String,
}

/// How a review of this project's work is run. Guidance until enforced.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReviewPolicy {
    #[serde(default = "review_template")]
    pub template: String,
    #[serde(default = "review_model")]
    pub model: String,
    #[serde(default = "review_effort")]
    pub effort: String,
    #[serde(default)]
    #[schemars(extend("x-keep-default" = true))]
    pub never_models: Vec<String>,
    #[serde(default = "review_budget_turns")]
    pub budget_turns: i64,
}

/// Whether closing an item needs a verification record, and where.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VerificationPolicy {
    #[serde(default)]
    pub required_for_done: bool,
    #[serde(default)]
    #[schemars(extend("x-keep-default" = true))]
    pub lanes: Vec<String>,
}

// `Literal["fresh", "never_swept", "partial"]` in Python; a fresh view takes never_swept.
#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(inline)]
pub enum FreshnessStatus {
    Fresh,
    #[default]
    NeverSwept,
    Partial,
}

/// How old the evidence behind an answer is.
///
/// Every aggregating answer carries one. `oldest_relevant_sweep` is the
/// honest number: an answer is exactly as fresh as the least fresh thing it
/// depends on.
#[derive(Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Freshness {
    #[serde(default)]
    pub status: FreshnessStatus,
    #[serde(default)]
    #[schemars(schema_with = "optional_date_time")]
    pub oldest_relevant_sweep: Option<String>,
    #[serde(default)]
    pub age_seconds: Option<i64>,
    /// Per collector: how long ago it last completed.
    #[serde(default)]
    #[schemars(extend("x-keep-default" = true))]
    pub collectors: BTreeMap<String, String>,
    #[serde(default)]
    pub detail: Option<String>,
}
