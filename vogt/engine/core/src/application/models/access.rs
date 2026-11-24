//! The access operations' parameter and result models: users, actors,
//! tokens, the caller's own identity and logout, auth decisions and
//! suppressions.
//!
//! Ports the `CreateUser*`, `SetPassword*`, `CreateActor*`, `IssueToken*`,
//! `Whoami*`, `Logout*`, `AuthDecision*`, `Suppress*` and related classes in
//! `src/vogt/application/models.py`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::entities::{
    Actor, ActorKind, AuthDecision, DecisionKind, PasswordCredential, Suppression, Token,
};
use super::integer;

fn list_limit_default() -> i64 {
    100
}

fn write_scopes_default() -> String {
    "read,work.write,project.write".to_string()
}

fn read_scope_default() -> String {
    "read".to_string()
}

fn actor_kind_default() -> String {
    "agent".to_string()
}

fn revoke_sessions_default() -> bool {
    true
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListActorsParams {
    #[serde(default = "list_limit_default")]
    #[schemars(schema_with = "integer", range(min = 1, max = 500), extend("default" = 100))]
    pub limit: i64,
    #[serde(default)]
    #[schemars(schema_with = "integer", range(min = 0))]
    pub offset: i64,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ActorListResult {
    pub actors: Vec<Actor>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateActorParams {
    /// Stable identity, e.g. agent:claude-code or local:sprooty.
    #[schemars(length(min = 1))]
    pub identity_ref: String,
    /// human or agent.
    #[serde(default = "actor_kind_default")]
    #[schemars(extend("default" = "agent"))]
    pub kind: String,
    #[schemars(length(min = 1))]
    pub display_name: String,
    #[schemars(length(min = 1))]
    pub reason: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ActorResult {
    pub actor: Actor,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("properties" = {}))]
pub struct UserListParams {}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UserListResult {
    pub users: Vec<PasswordCredential>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateUserParams {
    /// Login name: lower-case letters, digits, '.', '-', '_'.
    #[schemars(length(min = 1))]
    pub username: String,
    /// At least 8 characters.
    #[schemars(length(min = 1))]
    pub password: String,
    /// Defaults to the username.
    #[serde(default)]
    #[schemars(length(min = 1))]
    pub display_name: Option<String>,
    /// An existing human actor's identity_ref to attach the login to. Omitted, a new actor human:<username> is created.
    #[serde(default)]
    #[schemars(length(min = 1))]
    pub actor: Option<String>,
    /// Comma-separated scopes every session this user logs in to holds.
    #[serde(default = "write_scopes_default")]
    #[schemars(length(min = 1), extend("default" = "read,work.write,project.write"))]
    pub scopes: String,
    #[schemars(length(min = 1))]
    pub reason: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UserResult {
    pub user: PasswordCredential,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetPasswordParams {
    #[schemars(length(min = 1))]
    pub username: String,
    /// At least 8 characters.
    #[schemars(length(min = 1))]
    pub password: String,
    /// Replace the login's scopes as well.
    #[serde(default)]
    #[schemars(length(min = 1))]
    pub scopes: Option<String>,
    /// Also revoke every live session this user holds.
    #[serde(default = "revoke_sessions_default")]
    #[schemars(extend("default" = true))]
    pub revoke_sessions: bool,
    #[schemars(length(min = 1))]
    pub reason: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct IssueTokenParams {
    /// Actor identity_ref the token is bound to.
    #[schemars(length(min = 1))]
    pub actor: String,
    /// What this token is for, e.g. 'claude-code'.
    #[schemars(length(min = 1))]
    pub name: String,
    /// Comma-separated: read, work.write, project.write, admin, writeback.
    #[serde(default = "read_scope_default")]
    #[schemars(length(min = 1), extend("default" = "read"))]
    pub scopes: String,
    /// Omit for a token that does not expire.
    #[serde(default)]
    #[schemars(range(min = 1, max = 3650))]
    pub expires_in_days: Option<i64>,
    #[schemars(length(min = 1))]
    pub reason: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct IssueTokenResult {
    pub token: Token,
    /// Shown once. Not stored, not recoverable — rotate if lost.
    pub secret: String,
    pub warning: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListTokensParams {
    #[serde(default)]
    pub include_revoked: bool,
    #[serde(default = "list_limit_default")]
    #[schemars(schema_with = "integer", range(min = 1, max = 500), extend("default" = 100))]
    pub limit: i64,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TokenListResult {
    pub tokens: Vec<Token>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RevokeTokenParams {
    pub id: String,
    #[schemars(length(min = 1))]
    pub reason: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TokenResult {
    pub token: Token,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("properties" = {}))]
pub struct WhoamiParams {}

/// Who the caller is, as authentication decided it.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WhoamiResult {
    pub identity_ref: String,
    pub kind: ActorKind,
    pub display_name: String,
    /// The effective scope set, implications applied.
    pub scopes: Vec<String>,
    /// The token that authenticated the call; absent on the local surface.
    #[serde(default)]
    pub token: Option<Token>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LogoutParams {
    #[schemars(length(min = 1))]
    pub reason: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LogoutResult {
    /// Whether a session token was revoked. False on a surface with no token behind it (the local CLI, a loopback listener).
    pub revoked: bool,
    #[serde(default)]
    pub token: Option<Token>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AuthDecisionListParams {
    /// Filter to allows or denials.
    #[serde(default)]
    pub decision: Option<DecisionKind>,
    #[serde(default = "list_limit_default")]
    #[schemars(schema_with = "integer", range(min = 1, max = 500), extend("default" = 100))]
    pub limit: i64,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AuthDecisionListResult {
    pub decisions: Vec<AuthDecision>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SuppressParams {
    /// An exact subject key, or a glob pattern when --pattern is set.
    pub subject: String,
    /// Treat `subject` as a glob pattern.
    #[serde(default)]
    pub pattern: bool,
    /// Limit the suppression to one project.
    #[serde(default)]
    pub project: Option<String>,
    #[schemars(length(min = 1))]
    pub reason: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SuppressionResult {
    pub suppression: Suppression,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListSuppressionsParams {
    #[serde(default)]
    pub include_revoked: bool,
    #[serde(default = "list_limit_default")]
    #[schemars(schema_with = "integer", range(min = 1, max = 500), extend("default" = 100))]
    pub limit: i64,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SuppressionListResult {
    pub suppressions: Vec<Suppression>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RevokeSuppressionParams {
    pub id: String,
    #[schemars(length(min = 1))]
    pub reason: String,
}
