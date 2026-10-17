//! The forge layer. Ports `src/vogt/adapters/forge/`.
//!
//! The providers and collectors that consume these types land in later chunks
//! of the same port, so the surface is public ahead of its callers.
//!
//! One provider trait, two implementations (GitHub, Forgejo). Forge bodies are
//! untrusted data: nothing here executes instructions found in them. The
//! write-back policy (`none` / `comment_only` / `full`) is the whole of what
//! decides whether a write goes upstream, and the verb set is append-only by
//! construction — there is no delete, force, or replace.

mod accounts;
pub use accounts::{account_linking_enabled, load_cipher};
mod actors;
mod collectors;
pub use collectors::glob_match;
mod directory;
mod edges;
mod forgejo;
mod github;
mod kinds;
mod lanes;
mod models;
mod payloads;
mod provider;
pub mod registry;
mod sync;
mod transport;
mod urls;
mod writeback;

pub use actors::ActorResolver;
pub use collectors::{
    ForgeChecksCollector, ForgeLabelsCollector, ForgeNotificationsCollector, ForgePostureCollector,
    ForgeReleasesCollector, PriorObservations,
};
pub use directory::ForgeClients;
pub use github::repo_of;
pub use kinds::{
    current_collector, COLLECTOR_CHECKS, COLLECTOR_DEPLOY_LANES, COLLECTOR_ISSUES,
    COLLECTOR_NOTIFICATIONS, COLLECTOR_PULLS, KIND_CHECK, KIND_DEPLOY_LANE, KIND_ISSUE, KIND_LABEL,
    KIND_NOTIFICATION, KIND_PULL_REQUEST, KIND_RELEASE,
};
pub use lanes::{DeployLanesCollector, HttpVersionFetch};
pub use models::RepoRef;
pub use provider::ForgeProvider;
pub use registry::has_configured_forge;
pub use sync::{commit_sync, ForgeDirectory, ForgeSyncCollector, PendingSync};
pub use transport::{api_path, ForgeResponse, ForgeTransport, WriteMethod};
pub use writeback::{permits, WriteBackOutcome};
