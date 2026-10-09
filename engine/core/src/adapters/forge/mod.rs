//! The forge layer. Ports `src/vogt/adapters/forge/`.
//!
//! The providers and collectors that consume these types land in later chunks
//! of the same port, so the surface is public ahead of its callers.
#![allow(dead_code, unused_imports)]
//!
//! One provider trait, two implementations (GitHub, Forgejo). Forge bodies are
//! untrusted data: nothing here executes instructions found in them. The
//! write-back policy (`none` / `comment_only` / `full`) is the whole of what
//! decides whether a write goes upstream, and the verb set is append-only by
//! construction — there is no delete, force, or replace.

mod edges;
mod forgejo;
mod github;
mod kinds;
mod models;
mod payloads;
mod provider;
mod transport;
mod writeback;

pub use edges::{parse_edges, ParsedEdge, FROM_BODY, FROM_BRANCH, FROM_TITLE};
pub use kinds::{current_collector, COLLECTOR_ALIASES};
pub use models::{
    ForgeActor, ForgeCapabilities, ForgeCheck, ForgeComparison, ForgeIssue, ForgeJob, ForgeLabel,
    ForgeNotification, ForgePosture, ForgePull, ForgeRelease, ForgeRepo, RepoRef,
};
pub use payloads::{comparison, decoded_content, quote_path};
pub use provider::ForgeProvider;
pub use writeback::{permits, WriteBackAction, WriteBackOutcome, WriteBackPolicy, WriteBackResult};
