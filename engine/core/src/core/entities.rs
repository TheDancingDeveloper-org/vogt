//! Entity vocabulary and records. Ports the models of `src/vogt/core/`.

#![allow(dead_code)] // Domain vocabulary the bin re-exports; tests and later services use what the bin does not.
use std::collections::BTreeMap;

#[allow(unused_imports)]
// The test module below is the only reader of the first, second and last.
use super::{from_iso, require_text, ActorKind, Moment, DECLARABLE_RELATION_KINDS};

macro_rules! vocab {
    ($name:ident { $first:ident $(, $variant:ident)* $(,)? }) => {
        vocab!($name, serde, { $first $(, $variant)* });
    };
    ($name:ident, bare, { $first:ident $(, $variant:ident)* }) => {
        vocab!(@emit $name, , { $first $(, $variant)* });
    };
    ($name:ident, serde, { $first:ident $(, $variant:ident)* }) => {
        vocab!(@emit $name, #[derive(serde::Serialize, serde::Deserialize)] #[serde(rename_all = "snake_case")], { $first $(, $variant)* });
        vocab!(@text $name);
    };
    (@emit $name:ident, $(#[$serde:meta])* , { $first:ident $(, $variant:ident)* }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        $(#[$serde])*
        pub enum $name { $first, $($variant),* }

        impl $name {
            fn first() -> Self {
                Self::$first
            }
        }

        impl Default for $name {
            /// The first variant. The vocabularies whose pydantic default is
            /// something else name their own function instead of relying on
            /// this (`TrustState` defaults to `unverified`, `Priority` to `p2`).
            fn default() -> Self {
                Self::first()
            }
        }
    };
    (@text $name:ident) => {
        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                let text = serde_json::to_value(self).expect("an enum is a string");
                f.write_str(text.as_str().expect("an enum is a string"))
            }
        }

        /// The stored column is text. An unrecognised value is a data error,
        /// reported rather than mapped onto a default.
        impl std::str::FromStr for $name {
            type Err = String;

            fn from_str(text: &str) -> Result<Self, Self::Err> {
                serde_json::from_value(serde_json::Value::String(text.to_string()))
                    .map_err(|_| format!("unknown {}: {text:?}", stringify!($name)))
            }
        }

        impl rusqlite::types::ToSql for $name {
            fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
                Ok(self.to_string().into())
            }
        }

        impl rusqlite::types::FromSql for $name {
            fn column_result(
                value: rusqlite::types::ValueRef<'_>,
            ) -> rusqlite::types::FromSqlResult<Self> {
                value.as_str()?.parse().map_err(|err: String| {
                    rusqlite::types::FromSqlError::Other(err.into())
                })
            }
        }
    };
}

vocab!(WorkKind {
    Feature,
    Bug,
    Chore,
    Question
});
vocab!(ProjectLifecycle {
    Incubating,
    Active,
    Maintenance,
    Archived
});
// `not_applicable` is what a reader is told, not a stored value, but the
// column type is the same literal so a row can carry it.
vocab!(ComplianceStatus {
    Compliant,
    NonCompliant,
    NotChecked,
    NotApplicable
});
vocab!(LinkState { Unlinked, Linked });
vocab!(WriteBack, bare, { Disabled, CommentOnly, Full });

impl WriteBack {
    fn text(self) -> &'static str {
        match self {
            Self::Disabled => "none",
            Self::CommentOnly => "comment_only",
            Self::Full => "full",
        }
    }
}

impl std::fmt::Display for WriteBack {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.text())
    }
}

impl std::str::FromStr for WriteBack {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text {
            "none" => Ok(Self::Disabled),
            "comment_only" => Ok(Self::CommentOnly),
            "full" => Ok(Self::Full),
            other => Err(format!("unknown WriteBack: {other:?}")),
        }
    }
}

impl rusqlite::types::ToSql for WriteBack {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        Ok(self.to_string().into())
    }
}

impl rusqlite::types::FromSql for WriteBack {
    fn column_result(value: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<Self> {
        value
            .as_str()?
            .parse()
            .map_err(|err: String| rusqlite::types::FromSqlError::Other(err.into()))
    }
}

impl serde::Serialize for WriteBack {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(match self {
            Self::Disabled => "none",
            Self::CommentOnly => "comment_only",
            Self::Full => "full",
        })
    }
}

impl<'de> serde::Deserialize<'de> for WriteBack {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // A borrowed `&str` only works for a deserializer that lends its input.
        // `serde_json::from_value` yields owned strings, and that is how an
        // export's project is read, so the borrow fails on "none".
        match String::deserialize(deserializer)?.as_str() {
            "none" => Ok(Self::Disabled),
            "comment_only" => Ok(Self::CommentOnly),
            "full" => Ok(Self::Full),
            other => Err(serde::de::Error::unknown_variant(
                other,
                &["none", "comment_only", "full"],
            )),
        }
    }
}

fn lifecycle_default() -> ProjectLifecycle {
    ProjectLifecycle::Active
}

fn compliance_default() -> ComplianceStatus {
    ComplianceStatus::NotChecked
}

fn write_back_default() -> WriteBack {
    WriteBack::Disabled
}

fn link_state_default() -> LinkState {
    LinkState::Unlinked
}

fn priority_default() -> Priority {
    Priority::P2
}
vocab!(Priority { P0, P1, P2, P3, P4 });
vocab!(Effort { Xs, S, M, L, Xl });
fn trust_state_default() -> TrustState {
    TrustState::Unverified
}

vocab!(Origin {
    Created,
    Adopted,
    Observed
});
vocab!(TrustState {
    Verified,
    Stale,
    Unverified,
    Disputed
});
vocab!(RelationKind {
    DependsOn,
    RelatesTo,
    DuplicateOf,
    ParentOf,
    ImplementedBy
});
vocab!(InitiativeState { Open, Closed });
vocab!(MatchKind { Exact, Pattern });
vocab!(LinkRelation {
    Completion,
    Reference
});
vocab!(TokenKind {
    Api,
    Session,
    Agent
});
vocab!(AuthOutcome { Allow, Deny });
vocab!(WriteBackAction {
    Create,
    Comment,
    Label,
    Close,
    Reopen
});
vocab!(WriteBackOutcome {
    Attempted,
    Succeeded,
    Failed,
    Skipped
});

/// Pydantic defaults for fields a caller may omit. `Default` on the enum
/// itself would default every vocabulary's first variant, including the ones
/// Python requires.
fn initiative_state_default() -> InitiativeState {
    InitiativeState::Open
}

fn link_relation_default() -> LinkRelation {
    LinkRelation::Completion
}

fn grant_uses_default() -> GrantUses {
    GrantUses::Once
}

fn grant_state_default() -> GrantState {
    GrantState::Pending
}

fn empty_object() -> serde_json::Value {
    serde_json::json!({})
}

fn token_kind_default() -> TokenKind {
    TokenKind::Api
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Actor {
    pub id: String,
    pub kind: ActorKind,
    pub display_name: String,
    pub identity_ref: String,
    pub disabled: bool,
    pub created_at: Moment,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Project {
    pub id: String,
    pub slug: String,
    pub name: String,
    pub root_path: String,
    pub repo_url: Option<String>,
    #[serde(default = "lifecycle_default")]
    pub lifecycle_state: ProjectLifecycle,
    pub current_version: Option<String>,
    pub contract_version: Option<String>,
    #[serde(default = "compliance_default")]
    pub compliance_status: ComplianceStatus,
    pub compliance_checked_at: Option<Moment>,
    /// The project's agent policy, stored whole. An empty document reads as no
    /// policy, which is what a project that states nothing holds.
    #[serde(default)]
    pub agent_policy: AgentPolicy,
    pub contract_adopted_at: Option<Moment>,
    #[serde(default = "write_back_default")]
    pub write_back: WriteBack,
    #[serde(default = "link_state_default")]
    pub link_state: LinkState,
    #[serde(default)]
    pub exclusions: Vec<String>,
    #[serde(default = "trust_state_default")]
    pub trust_state: TrustState,
    pub created_at: Moment,
    pub updated_at: Moment,
}

impl Project {
    pub fn new(id: &str, slug: &str, name: &str, root_path: &str, now: Moment) -> Self {
        Self {
            id: id.to_string(),
            slug: slug.to_string(),
            name: name.to_string(),
            root_path: root_path.to_string(),
            repo_url: None,
            lifecycle_state: ProjectLifecycle::Active,
            current_version: None,
            contract_version: None,
            compliance_status: ComplianceStatus::NotChecked,
            compliance_checked_at: None,
            agent_policy: AgentPolicy::default(),
            contract_adopted_at: None,
            write_back: WriteBack::Disabled,
            link_state: LinkState::Unlinked,
            exclusions: Vec::new(),
            trust_state: TrustState::Unverified,
            created_at: now,
            updated_at: now,
        }
    }
}

/// A project's agent policy, stored as one document and read whole. An empty
/// document is no policy.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct AgentPolicy {
    #[serde(default)]
    pub forbidden_text: Vec<String>,
    #[serde(default)]
    pub merge_deploys_prod: bool,
    #[serde(default)]
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
    pub rules: Vec<String>,
    #[serde(default)]
    pub session_wall_clock_minutes: Option<i64>,
    #[serde(default)]
    pub landing: Option<serde_json::Value>,
    #[serde(default)]
    pub review: Option<serde_json::Value>,
    #[serde(default)]
    pub verification: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkItem {
    pub id: String,
    #[serde(rename = "ref")]
    pub reference: String,
    pub kind: WorkKind,
    pub title: String,
    #[serde(default)]
    pub body: String,
    pub state: String,
    #[serde(default = "priority_default")]
    pub priority: Priority,
    pub effort: Option<Effort>,
    pub project_id: Option<String>,
    pub project_slug: Option<String>,
    pub initiative_id: Option<String>,
    #[serde(default)]
    pub origin: Origin,
    #[serde(default = "trust_state_default")]
    pub trust_state: TrustState,
    pub assignee_actor_id: Option<String>,
    pub assignee_identity_ref: Option<String>,
    pub labels: Vec<String>,
    pub relations: Vec<Relation>,
    pub superseded_by: Option<String>,
    pub created_at: Moment,
    pub updated_at: Moment,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Observation {
    pub id: String,
    pub sweep_id: String,
    pub collector: String,
    pub kind: String,
    pub project_id: Option<String>,
    pub subject_key: String,
    pub payload: serde_json::Value,
    pub content_digest: String,
    pub source_url: Option<String>,
    pub promoted: bool,
    pub observed_at: Moment,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkOverlay {
    pub subject_key: String,
    pub project_id: String,
    pub rank: Option<f64>,
    pub workflow_state: Option<String>,
    pub priority: Option<Priority>,
    pub effort: Option<Effort>,
    pub assignee_actor_id: Option<String>,
    pub initiative_id: Option<String>,
    #[serde(default)]
    pub branches: Vec<String>,
    pub created_at: Moment,
    pub updated_at: Moment,
}

vocab!(GrantKind {
    Credential,
    Capability
});
vocab!(GrantUses { Once, Ttl });
vocab!(GrantState {
    Pending,
    Approved,
    Denied,
    Revoked
});

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionGrant {
    pub id: String,
    pub target_engine_session_id: String,
    pub kind: GrantKind,
    pub var: Option<String>,
    pub project_id: Option<String>,
    pub secret_name: Option<String>,
    pub capability: Option<String>,
    #[serde(default = "grant_uses_default")]
    pub uses: GrantUses,
    pub ttl_seconds: i64,
    pub reason: String,
    pub requested_by: String,
    pub requested_at: Moment,
    #[serde(default = "grant_state_default")]
    pub state: GrantState,
    pub decided_by: Option<String>,
    pub decided_at: Option<Moment>,
    pub decision_reason: Option<String>,
    pub expires_at: Option<Moment>,
    pub revoked_by: Option<String>,
    pub revoked_at: Option<Moment>,
}

impl SessionGrant {
    /// `expired` for an approved grant past its expiry, else the stored state.
    /// `expired` is not itself a state, so it comes back as text.
    pub fn effective_state(&self, now: Moment) -> String {
        if self.state == GrantState::Approved {
            if let Some(expires) = self.expires_at {
                if expires <= now {
                    return "expired".to_string();
                }
            }
        }
        self.state.to_string()
    }
}

vocab!(DecisionState {
    Pending,
    Answered,
    Withdrawn
});

/// One answer a decision offers. Exactly one per decision is recommended.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionOption {
    pub label: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub recommended: bool,
}

/// The option a person chose, and what they added.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionAnswer {
    pub label: String,
    #[serde(default)]
    pub note: String,
}

/// A question an agent put to a person, with options (WI-1321). Shaped like
/// `SessionGrant`. `expired` is not a state: it is a pending decision past
/// `expires_at`, computed on read, and it implies no answer.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Decision {
    pub id: String,
    pub asker_actor_id: String,
    pub asker_session: Option<String>,
    pub work_item_id: Option<String>,
    pub question: String,
    pub options: Vec<DecisionOption>,
    #[serde(default)]
    pub context: String,
    #[serde(default = "decision_state_default")]
    pub state: DecisionState,
    pub answer: Option<DecisionAnswer>,
    pub answered_by_actor_id: Option<String>,
    pub created_at: Moment,
    pub answered_at: Option<Moment>,
    pub expires_at: Moment,
    pub reason: String,
}

impl Decision {
    /// `expired` for a pending decision past its expiry, else the stored state.
    pub fn effective_state(&self, now: Moment) -> String {
        if self.state == DecisionState::Pending && self.expires_at <= now {
            return "expired".to_string();
        }
        self.state.to_string()
    }
}

fn decision_state_default() -> DecisionState {
    DecisionState::Pending
}

vocab!(CharterScopeKind {
    Workspace,
    Project,
    Initiative
});

/// One version of a scope's standing instructions (WI-2038). Append-only:
/// setting a charter writes the next version. An empty body clears it. The
/// workspace charter has `scope_id` "", a project's is its id, an initiative's
/// is its id.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Charter {
    pub scope_kind: CharterScopeKind,
    pub scope_id: String,
    pub version: i64,
    pub body: String,
    pub author_actor_id: String,
    pub created_at: Moment,
    pub reason: String,
}

vocab!(WatchState {
    Active,
    Fired,
    Failed,
    Cancelled,
    Expired
});

/// A session waiting on an event instead of polling (WI-2036). `expired` is
/// stored once the loop sees an active watch past `expires_at`;
/// `effective_state` also computes it on read.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Watch {
    pub id: String,
    pub owner_actor_id: String,
    pub target_session: String,
    pub kind: String,
    pub trigger: serde_json::Value,
    #[serde(default)]
    pub note: String,
    #[serde(default = "watch_state_default")]
    pub state: WatchState,
    pub created_at: Moment,
    pub expires_at: Moment,
    pub fired_at: Option<Moment>,
    #[serde(default)]
    pub outcome: Option<serde_json::Value>,
    #[serde(default)]
    pub attempts: i64,
    pub last_error: Option<String>,
    pub reason: String,
}

fn watch_state_default() -> WatchState {
    WatchState::Active
}

impl Watch {
    /// `expired` for an active watch past its expiry, else the stored state.
    pub fn effective_state(&self, now: Moment) -> String {
        if self.state == WatchState::Active && self.expires_at <= now {
            return "expired".to_string();
        }
        self.state.to_string()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Label {
    pub id: String,
    pub name: String,
    pub color: Option<String>,
    pub created_at: Moment,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Initiative {
    pub id: String,
    pub slug: String,
    pub title: String,
    #[serde(default)]
    pub body: String,
    #[serde(default = "initiative_state_default")]
    pub state: InitiativeState,
    #[serde(default)]
    pub weight: i64,
    pub created_at: Moment,
    pub updated_at: Moment,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Relation {
    pub kind: RelationKind,
    pub related_id: String,
    pub related_ref: String,
    pub related_title: String,
    pub related_state: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Comment {
    pub id: String,
    pub work_item_id: String,
    pub actor_id: String,
    pub actor_display_name: String,
    pub body: String,
    pub created_at: Moment,
    /// `note` or `handoff`. A hand-off carries `handoff`; a note does not.
    #[serde(default = "note_kind")]
    pub kind: String,
    #[serde(default)]
    pub handoff: Option<serde_json::Value>,
}

fn note_kind() -> String {
    "note".to_string()
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Suppression {
    pub id: String,
    pub match_kind: MatchKind,
    pub subject_key_or_pattern: String,
    pub scope_project_id: Option<String>,
    pub scope_project_slug: Option<String>,
    pub actor_id: String,
    pub actor_identity_ref: Option<String>,
    pub reason: String,
    pub created_at: Moment,
    pub revoked_at: Option<Moment>,
    pub revoked_reason: Option<String>,
}

impl Suppression {
    /// A suppression holds until someone revokes it.
    pub fn active(&self) -> bool {
        self.revoked_at.is_none()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContractExemption {
    pub id: String,
    pub project_id: String,
    pub project_slug: Option<String>,
    pub rule: String,
    pub target: String,
    pub reason: String,
    pub declared_by: String,
    pub declared_at: Moment,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkLink {
    pub work_item_id: String,
    pub subject_key: String,
    pub origin_kind: String,
    pub source_url: Option<String>,
    #[serde(default = "link_relation_default")]
    pub relation: LinkRelation,
    pub created_at: Moment,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodingSession {
    pub id: String,
    pub engine_session_id: String,
    pub project_id: String,
    pub work_item_id: Option<String>,
    pub actor_id: String,
    pub cwd: String,
    pub template: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub reason: String,
    /// The dispatch purpose the start named (WI-1323), if any.
    #[serde(default)]
    pub purpose: Option<String>,
    /// The engine id of the session that started this one, when a session did.
    #[serde(default)]
    pub started_by_session: Option<String>,
    /// Whether the starter was a person, or a session in the oversight or
    /// worker role at the time (WI-1323).
    #[serde(default)]
    pub started_by_kind: Option<String>,
    pub started_at: Moment,
    pub stopped_at: Option<Moment>,
}

/// The hash only. A model that could round-trip the secret is one that leaks
/// it into logs and API responses.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Token {
    pub id: String,
    pub actor_id: String,
    pub actor_identity_ref: Option<String>,
    pub name: String,
    pub scopes: Vec<String>,
    #[serde(default = "token_kind_default")]
    pub kind: TokenKind,
    pub created_at: Moment,
    pub expires_at: Option<Moment>,
    pub last_used_at: Option<Moment>,
    pub revoked_at: Option<Moment>,
    pub revoked_reason: Option<String>,
}

impl Token {
    pub fn active(&self) -> bool {
        self.revoked_at.is_none()
    }
}

/// The hash is deliberately absent: the storage layer writes and reads it, so
/// no listing, result or audit payload can carry it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PasswordCredential {
    pub actor_id: String,
    pub actor_identity_ref: Option<String>,
    pub username: String,
    pub scopes: Vec<String>,
    pub created_at: Moment,
    pub updated_at: Moment,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthDecision {
    pub id: String,
    pub at: Moment,
    pub decision: AuthOutcome,
    pub reason_code: String,
    pub operation: String,
    pub scope: Option<String>,
    pub actor_id: Option<String>,
    pub token_id: Option<String>,
    pub identity_ref: Option<String>,
    pub transport: String,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WriteBackRecord {
    pub id: String,
    pub at: Moment,
    pub project_id: Option<String>,
    pub work_item_id: Option<String>,
    pub actor_id: String,
    pub action: WriteBackAction,
    pub subject_key: Option<String>,
    pub policy: String,
    pub outcome: WriteBackOutcome,
    pub reason: String,
    pub detail: Option<String>,
    pub source_url: Option<String>,
}

/// No token field. The encrypted PAT lives in its own column and is read
/// through a dedicated accessor, so this entity can never carry it out.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForgeAccount {
    pub actor_id: String,
    pub host: String,
    pub login: String,
    pub scopes: String,
    pub created_at: Moment,
    pub updated_at: Moment,
}

vocab!(SweepOutcome {
    Running,
    Ok,
    Partial,
    Failed
});
vocab!(RefKind {
    Path,
    Git,
    Declared,
    Inherited
});
vocab!(DriftStatus {
    Open,
    Accepted,
    Rejected,
    Contested
});
vocab!(TriageState {
    Active,
    Archived,
    Snoozed
});

/// Append-only ledger row. Ports `AuditRecord` (`entities.py`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditRecord {
    pub id: String,
    pub txn_id: String,
    pub revision: i64,
    pub actor_id: String,
    pub actor_identity_ref: String,
    pub operation: String,
    pub entity_kind: String,
    pub entity_id: String,
    pub reason: String,
    pub payload_digest: String,
    pub at: Moment,
}

/// Published change. Ports `Event`. `summary` is the JSON object the feed
/// carries; an absent actor or audit row stays `None`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Event {
    pub seq: i64,
    pub kind: String,
    pub entity_kind: String,
    pub entity_id: String,
    pub actor_id: Option<String>,
    pub audit_id: Option<String>,
    #[serde(default = "empty_object")]
    pub summary: serde_json::Value,
    pub at: Moment,
}

/// A machine-raised question, resolved by a human or an agent. Ports
/// `DriftProposal`. `status` is the four-value literal, not a free string.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DriftProposal {
    pub id: String,
    pub kind: String,
    pub subject_kind: String,
    pub subject_id: String,
    pub project_id: Option<String>,
    pub project_slug: Option<String>,
    pub summary: String,
    pub evidence_observation_id: Option<String>,
    pub evidence_snapshot: serde_json::Value,
    pub proposed_change: serde_json::Value,
    pub status: DriftStatus,
    pub opened_at: Moment,
    pub superseded_at: Option<Moment>,
    pub superseded_detail: Option<String>,
    pub resolved_by_actor_id: Option<String>,
    pub resolved_by_identity_ref: Option<String>,
    pub resolved_at: Option<Moment>,
    pub resolution_reason: Option<String>,
}

/// The shared, audited decision attached to one Inbox occurrence. Ports
/// `InboxTriage`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InboxTriage {
    pub entry_key: String,
    pub state: TriageState,
    pub snooze_until: Option<Moment>,
    pub actor_id: String,
    pub actor_identity_ref: Option<String>,
    pub decided_at: Moment,
    pub occurrence_snapshot: serde_json::Value,
}

/// One per-actor setting. Ports `ActorPreference`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActorPreference {
    pub actor_id: String,
    pub key: String,
    pub value: serde_json::Value,
    pub version: i64,
    pub updated_at: Moment,
}

/// One reference from a project to another. Ports `DepRef`. No lockfile is
/// parsed and no package version is resolved.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DepRef {
    pub subject_key: String,
    pub from_project_id: String,
    pub from_project_slug: Option<String>,
    pub ref_kind: RefKind,
    pub raw_target: String,
    pub manifest: Option<String>,
    pub to_project_id: Option<String>,
    pub to_project_slug: Option<String>,
    pub observed_at: Moment,
}

/// A coverage record. Ports `Sweep`. `outcome` defaults to `running` until a
/// sweep finishes, which is what makes "absent" different from "not collected".
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sweep {
    pub id: String,
    pub collector: String,
    pub scope: Vec<String>,
    pub started_at: Moment,
    pub finished_at: Option<Moment>,
    pub outcome: SweepOutcome,
    pub stats: BTreeMap<String, i64>,
    pub detail: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_back_reads_from_an_owned_json_value() {
        // serde_json::from_value yields owned strings. Deserializing from a
        // borrowed &str fails there, and that is how an export's project is read.
        for (text, expected) in [
            ("none", WriteBack::Disabled),
            ("comment_only", WriteBack::CommentOnly),
            ("full", WriteBack::Full),
        ] {
            let owned = serde_json::Value::String(text.to_string());
            let parsed: WriteBack = serde_json::from_value(owned).unwrap();
            assert_eq!(parsed, expected);
            assert_eq!(
                serde_json::to_value(parsed).unwrap(),
                serde_json::json!(text)
            );
        }
    }

    #[test]
    fn a_missing_trust_state_reads_as_unverified() {
        // Python's default is unverified. The first variant is verified, so a
        // blanket Default would mark an imported item as checked.
        let item: WorkItem = serde_json::from_str(
        r#"{"id":"wrk_1","ref":"WI-1","kind":"bug","title":"t","state":"open","labels":[],"relations":[],"created_at":"2026-01-01T00:00:00+00:00","updated_at":"2026-01-01T00:00:00+00:00"}"#,
    )
    .unwrap();
        assert_eq!(item.trust_state, TrustState::Unverified);
        assert_eq!(item.origin, Origin::Created);
    }

    #[test]
    fn a_suppression_is_active_until_it_is_revoked() {
        assert_eq!(
            serde_json::to_string(&MatchKind::Pattern).unwrap(),
            "\"pattern\""
        );
        let mut suppression = Suppression {
            id: "sup_0001".to_string(),
            match_kind: MatchKind::Exact,
            subject_key_or_pattern: "git:main".to_string(),
            scope_project_id: None,
            scope_project_slug: None,
            actor_id: "act_0001".to_string(),
            actor_identity_ref: None,
            reason: "noise".to_string(),
            created_at: from_iso("2026-01-02T03:04:05+00:00").unwrap(),
            revoked_at: None,
            revoked_reason: None,
        };
        assert!(suppression.active());
        suppression.revoked_at = Some(suppression.created_at);
        assert!(!suppression.active());
    }

    #[test]
    fn the_vocabulary_serialises_as_pythons_literals() {
        assert_eq!(
            serde_json::to_string(&WorkKind::Feature).unwrap(),
            "\"feature\""
        );
        assert_eq!(serde_json::to_string(&Priority::P2).unwrap(), "\"p2\"");
        assert_eq!(serde_json::to_string(&Effort::Xl).unwrap(), "\"xl\"");
        assert_eq!(
            serde_json::to_string(&Origin::Observed).unwrap(),
            "\"observed\""
        );
        assert_eq!(
            serde_json::to_string(&TrustState::Unverified).unwrap(),
            "\"unverified\""
        );
        assert_eq!(
            serde_json::to_string(&RelationKind::ImplementedBy).unwrap(),
            "\"implemented_by\""
        );
        assert_eq!(
            serde_json::to_string(&InitiativeState::Closed).unwrap(),
            "\"closed\""
        );
        assert!(serde_json::from_str::<RelationKind>("\"invented\"").is_err());
        assert_eq!(
            serde_json::to_string(&LinkRelation::Completion).unwrap(),
            "\"completion\""
        );
        assert_eq!(
            serde_json::to_string(&TokenKind::Agent).unwrap(),
            "\"agent\""
        );
        assert_eq!(
            serde_json::to_string(&AuthOutcome::Deny).unwrap(),
            "\"deny\""
        );
        assert_eq!(
            serde_json::to_string(&WriteBackAction::Reopen).unwrap(),
            "\"reopen\""
        );
        assert_eq!(
            serde_json::to_string(&WriteBackOutcome::Skipped).unwrap(),
            "\"skipped\""
        );
    }

    /// A field pydantic defaults must decode when the payload omits it.
    #[test]
    fn omitted_fields_take_the_pydantic_default() {
        let initiative: Initiative = serde_json::from_str(
        r#"{"id":"i","slug":"s","title":"t","created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z"}"#,
    )
    .unwrap();
        assert_eq!(initiative.body, "");
        assert_eq!(initiative.state, InitiativeState::Open);
        assert_eq!(initiative.weight, 0);

        let link: WorkLink = serde_json::from_str(
        r#"{"work_item_id":"w","subject_key":"k","origin_kind":"git","created_at":"2026-01-01T00:00:00Z"}"#,
    )
    .unwrap();
        assert_eq!(link.relation, LinkRelation::Completion);

        let token: Token = serde_json::from_str(
        r#"{"id":"t","actor_id":"a","name":"n","scopes":[],"created_at":"2026-01-01T00:00:00Z"}"#,
    )
    .unwrap();
        assert_eq!(token.kind, TokenKind::Api);
    }

    /// pydantic's `extra="forbid"`: a key the model does not declare is an error.
    #[test]
    fn an_unknown_field_is_refused() {
        let err = serde_json::from_str::<Label>(
            r#"{"id":"l","name":"bug","color":null,"created_at":"2026-01-01T00:00:00Z","extra":1}"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("unknown field"), "{err}");
    }

    #[test]
    fn the_json_names_match_python() {
        // ActorKind is snake_case and WorkItem's handle is "ref", as pydantic
        // emits them. A moment's JSON form is the Z suffix pydantic writes for
        // model_dump(mode="json"); the +00:00 form is what the columns store.
        assert_eq!(
            serde_json::to_string(&ActorKind::Human).unwrap(),
            "\"human\""
        );
        let moment = from_iso("2026-01-02T03:04:05.123456+00:00").unwrap();
        assert_eq!(moment.to_iso(), "2026-01-02T03:04:05.123456+00:00");
        assert_eq!(
            serde_json::to_string(&moment).unwrap(),
            "\"2026-01-02T03:04:05.123456Z\""
        );
        let whole = from_iso("2026-01-02T03:04:05+00:00").unwrap();
        assert_eq!(
            serde_json::to_string(&whole).unwrap(),
            "\"2026-01-02T03:04:05Z\""
        );
        let item = WorkItem {
            id: "wrk_0001".to_string(),
            reference: "WI-1".to_string(),
            kind: "bug".parse().unwrap(),
            title: "a title".to_string(),
            body: String::new(),
            state: "open".to_string(),
            priority: "p2".parse().unwrap(),
            effort: None,
            project_id: None,
            project_slug: None,
            initiative_id: None,
            origin: "created".parse().unwrap(),
            trust_state: "unverified".parse().unwrap(),
            assignee_actor_id: None,
            assignee_identity_ref: None,
            labels: Vec::new(),
            relations: Vec::new(),
            superseded_by: None,
            created_at: from_iso("2026-01-02T03:04:05Z").unwrap(),
            updated_at: from_iso("2026-01-02T03:04:05Z").unwrap(),
        };
        let json = serde_json::to_value(&item).unwrap();
        assert_eq!(json["ref"], "WI-1");
        assert!(json.get("reference").is_none());
        assert_eq!(json["created_at"], "2026-01-02T03:04:05Z");
        let back: WorkItem = serde_json::from_value(json).unwrap();
        assert_eq!(back, item);
    }

    #[test]
    fn entities_carry_the_python_defaults() {
        let now = Moment::from_unix(1_700_000_000, 0);
        let project = Project::new("prj_1", "vogt", "Vogt", "/src", now);
        assert_eq!(project.lifecycle_state, ProjectLifecycle::Active);
        assert_eq!(project.compliance_status, ComplianceStatus::NotChecked);
        assert_eq!(project.link_state, LinkState::Unlinked);
        assert_eq!(project.write_back.to_string(), "none");
        assert!(require_text("  ").is_err());
        assert_eq!(require_text("  because  ").unwrap(), "because");
        assert!(!DECLARABLE_RELATION_KINDS.contains(&"implemented_by"));

        let grant = SessionGrant {
            id: "g".into(),
            target_engine_session_id: "eng".into(),
            kind: GrantKind::Credential,
            var: None,
            project_id: None,
            secret_name: None,
            capability: None,
            uses: GrantUses::Once,
            ttl_seconds: 60,
            reason: "push".into(),
            requested_by: "act".into(),
            requested_at: now,
            state: GrantState::Approved,
            decided_by: None,
            decided_at: None,
            decision_reason: None,
            expires_at: Some(now),
            revoked_by: None,
            revoked_at: None,
        };
        assert_eq!(grant.effective_state(now), "expired");
        assert_eq!(
            grant.effective_state(Moment::from_unix(1_699_999_999, 0)),
            "approved"
        );
        let pending = SessionGrant {
            state: GrantState::Pending,
            expires_at: Some(now),
            ..grant
        };
        assert_eq!(
            pending.effective_state(Moment::from_unix(1_800_000_000, 0)),
            "pending"
        );
    }

    #[test]
    fn the_seven_ported_entities_round_trip_python_json() {
        // Pinned against pydantic model_dump(mode="json") for the same values.
        let cases: &[(&str, &str)] = &[
            (
                "AuditRecord",
                r#"{"actor_id":"act_1","actor_identity_ref":"local:a","at":"2024-01-02T00:00:00Z","entity_id":"prj_1","entity_kind":"project","id":"aud_1","operation":"op","payload_digest":"sha256:0000000000000000000000000000000000000000000000000000000000000000","reason":"because","revision":3,"txn_id":"txn_1"}"#,
            ),
            (
                "Event",
                r#"{"actor_id":null,"at":"2024-01-02T00:00:00Z","audit_id":null,"entity_id":"prj_1","entity_kind":"project","kind":"created","seq":1,"summary":{"verb":"created"}}"#,
            ),
            (
                "DriftProposal",
                r#"{"evidence_observation_id":null,"evidence_snapshot":{},"id":"dft_1","kind":"status","opened_at":"2024-01-02T00:00:00Z","project_id":null,"project_slug":null,"proposed_change":{},"resolution_reason":null,"resolved_at":null,"resolved_by_actor_id":null,"resolved_by_identity_ref":null,"status":"open","subject_id":"prj_1","subject_kind":"project","summary":"s","superseded_at":null,"superseded_detail":null}"#,
            ),
            (
                "InboxTriage",
                r#"{"actor_id":"act_1","actor_identity_ref":null,"decided_at":"2024-01-02T00:00:00Z","entry_key":"k","occurrence_snapshot":{},"snooze_until":null,"state":"active"}"#,
            ),
            (
                "ActorPreference",
                r#"{"actor_id":"act_1","key":"theme","updated_at":"2024-01-02T00:00:00Z","value":{"mode":"dark"},"version":1}"#,
            ),
            (
                "DepRef",
                r#"{"from_project_id":"prj_1","from_project_slug":null,"manifest":null,"observed_at":"2024-01-02T00:00:00Z","raw_target":"../x","ref_kind":"path","subject_key":"sub","to_project_id":null,"to_project_slug":null}"#,
            ),
            (
                "Sweep",
                r#"{"collector":"git","detail":null,"finished_at":null,"id":"swp_1","outcome":"running","scope":["prj_1"],"started_at":"2024-01-02T00:00:00Z","stats":{}}"#,
            ),
        ];
        for (name, json) in cases {
            let value: serde_json::Value = serde_json::from_str(json).unwrap();
            let back = match *name {
                "AuditRecord" => serde_json::to_value(
                    serde_json::from_value::<AuditRecord>(value.clone()).unwrap(),
                )
                .unwrap(),
                "Event" => {
                    serde_json::to_value(serde_json::from_value::<Event>(value.clone()).unwrap())
                        .unwrap()
                }
                "DriftProposal" => serde_json::to_value(
                    serde_json::from_value::<DriftProposal>(value.clone()).unwrap(),
                )
                .unwrap(),
                "InboxTriage" => serde_json::to_value(
                    serde_json::from_value::<InboxTriage>(value.clone()).unwrap(),
                )
                .unwrap(),
                "ActorPreference" => serde_json::to_value(
                    serde_json::from_value::<ActorPreference>(value.clone()).unwrap(),
                )
                .unwrap(),
                "DepRef" => {
                    serde_json::to_value(serde_json::from_value::<DepRef>(value.clone()).unwrap())
                        .unwrap()
                }
                "Sweep" => {
                    serde_json::to_value(serde_json::from_value::<Sweep>(value.clone()).unwrap())
                        .unwrap()
                }
                _ => unreachable!(),
            };
            assert_eq!(back, value, "{name}");
        }
    }
}
