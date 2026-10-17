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
