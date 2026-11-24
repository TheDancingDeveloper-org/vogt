//! The forge operations' parameter and result models: account link and
//! unlink, account status and onboarding.
//!
//! Ports `ForgeAccount*` and `Onboard*` in `src/vogt/application/models.py`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{integer, optional_string};

fn github_host() -> String {
    "github.com".to_string()
}

fn max_pages_default() -> i64 {
    10
}

fn supported_default() -> bool {
    true
}

/// One linked account, never carrying the token.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ForgeAccountView {
    pub host: String,
    pub login: String,
    /// The token's granted scopes as the forge reports them (`X-OAuth-Scopes`), or empty when the forge did not say.
    pub scopes: String,
    #[serde(default = "supported_default")]
    #[schemars(extend("default" = true))]
    pub linked: bool,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ForgeAccountUnlinkParams {
    /// The forge host to unlink. Deletes the stored token.
    #[serde(default = "github_host")]
    #[schemars(extend("default" = "github.com"))]
    pub host: String,
    /// Why this write is being made (audited).
    #[schemars(length(min = 1))]
    pub reason: String,
}

/// The outcome of a link or unlink — status only, never the token.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ForgeAccountResult {
    pub host: String,
    /// The linked login; null once unlinked.
    #[serde(default)]
    pub login: Option<String>,
    #[serde(default)]
    pub scopes: String,
    pub linked: bool,
}

/// Link the acting actor's own forge account by pasting a PAT.
///
/// The token is validated against the forge, then stored encrypted at rest
/// and never echoed. Linking arms upstream writes attributed to *you*: the
/// scope of what those writes can do is the scope of the token you paste.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ForgeAccountLinkParams {
    /// The configured forge host to link, for example github.com or a Forgejo host named under forge_token_files.
    #[serde(default = "github_host")]
    #[schemars(extend("default" = "github.com"))]
    pub host: String,
    /// The Personal Access Token. Validated, then stored encrypted at rest under `forge_account_key_file` and never returned by any surface. To revoke it, unlink here and revoke it upstream too.
    #[schemars(length(min = 1))]
    pub token: String,
    /// Why this write is being made (audited).
    #[schemars(length(min = 1))]
    pub reason: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ForgeAccountStatusParams {
    /// Restrict to one host; unset lists every host you linked.
    #[serde(default)]
    pub host: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ForgeAccountStatusResult {
    pub accounts: Vec<ForgeAccountView>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OnboardParams {
    /// Project slug to consolidate.
    pub project: String,
    /// How far back to walk. Bounded so a busy repository cannot turn onboarding into an unbounded run; coverage reports what was actually swept.
    #[serde(default = "max_pages_default")]
    #[schemars(schema_with = "integer", range(min = 1, max = 100), extend("default" = 10))]
    pub max_pages: i64,
    #[schemars(length(min = 1))]
    pub reason: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OnboardResult {
    pub project: String,
    #[schemars(schema_with = "optional_string")]
    pub repo: Option<String>,
    #[schemars(schema_with = "integer")]
    pub issues: i64,
    #[schemars(schema_with = "integer")]
    pub pull_requests: i64,
    #[schemars(schema_with = "integer")]
    pub labels: i64,
    #[schemars(schema_with = "integer")]
    pub releases: i64,
    #[schemars(schema_with = "integer")]
    pub new: i64,
    #[schemars(schema_with = "integer")]
    pub unchanged: i64,
    /// Always zero. Onboarding is read-only; the field exists so the claim is asserted rather than assumed.
    #[serde(default)]
    #[schemars(schema_with = "integer")]
    pub mutations: i64,
    /// Whether an adapter could read this repository's host at all. False makes the zeros below meaningless rather than informative: nothing was read, so nothing was found, and `detail` says why. The import playbook treats an empty consolidation as a signal, and this is what makes that signal readable.
    #[serde(default = "supported_default")]
    #[schemars(extend("default" = true))]
    pub supported: bool,
    #[serde(default)]
    pub detail: Option<String>,
}
