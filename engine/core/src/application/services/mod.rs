//! S4 use-cases. Ports `preferences.py`, `notifications.py`, the initiative
//! half of `taxonomy.py`, and the attention Inbox.
//!
//! Dispatch is `service_for`. Another worker owns the registry wiring, so this
//! module only exports the function pointer that worker will store. Nothing
//! here reaches into `registry.rs`, `main.rs`, or `adapters/`.
//!
//! `Built` is four concrete contexts and the registry owns the `ServiceFn`
//! signature, so it cannot be generic. The match over the four lives in
//! `dispatch!`, inside this module and nowhere else; every service is generic
//! over the clock and the id factory.

pub mod agent_activity;
pub mod agent_clis;
pub mod auth;
pub mod bootstrap;
pub mod charters;
pub mod chats;
pub mod connect;
pub mod engine_status;
pub mod forge_accounts;
pub mod forge_import;
pub mod forge_link;
pub mod forge_onboard;
pub mod forge_publish;
pub mod forge_repos;
pub mod forge_writeback;
pub mod freshness;
pub mod history;
pub mod install;
pub mod native_migration;
pub mod work;

pub mod decisions;
pub mod import_merge;
pub mod inbox;
pub mod initiatives;
pub mod lifecycle;
pub mod notifications;
pub mod preferences;
#[allow(dead_code)]
pub mod sessions;

/// The session resolver for callers outside `services`.
///
/// W9's `sessions::resolve_target` is `pub(super)`, so it reaches this module
/// and nothing further. The grants half (WI-1074) sits beside `services` and
/// must resolve a session the same way, so it calls through here instead of
/// keeping a second copy of `sessions.py`'s `_target`.
pub(crate) fn resolve_session<C, I>(
    ctx: &AppContext<C, I>,
    session_id: &str,
) -> Result<String, crate::errors::VogtError>
where
    C: Clock,
    I: IdFactory,
{
    sessions::resolve_target(ctx, session_id).map(|target| target.engine_session_id)
}

use crate::application::context::{AppContext, Built};
use crate::application::writes::WriteContext;
use crate::core::{Clock, IdFactory, Moment, Principal};
use crate::storage::sqlite::declared::SqliteDeclaredStore;
use std::sync::Arc;

#[cfg(test)]
mod decisions_test;
#[cfg(test)]
mod forge_module_test;
#[cfg(test)]
mod forge_ops_test;
#[cfg(test)]
mod project_import_test;
#[cfg(test)]
mod services_test;
#[cfg(test)]
mod upstream_truth_test;

/// The shape the registry calls. Named operations are `fn` items, not closures,
/// so the pointer is stable.
/// Call `path` with the concrete context `built` holds. The operation table
/// itself lives in `registry::service_for`; this only opens the context.
macro_rules! dispatch {
    ($built:expr, $path:path, $params:expr) => {{
        match $built {
            Built::SystemRandom(ctx) => $path(ctx, $params),
            Built::SystemSequential(ctx) => $path(ctx, $params),
            Built::StepRandom(ctx) => $path(ctx, $params),
            Built::StepSequential(ctx) => $path(ctx, $params),
        }
    }};
}
pub(super) use dispatch;

pub(super) fn now_of(clock: &std::sync::Arc<std::sync::Mutex<impl Clock>>) -> Moment {
    clock
        .lock()
        .expect("the shared clock is not poisoned")
        .now()
}

pub(super) fn next_id(
    ids: &std::sync::Arc<std::sync::Mutex<impl IdFactory>>,
    prefix: &str,
) -> String {
    ids.lock()
        .expect("the shared id factory is not poisoned")
        .next(prefix)
}

pub(super) fn write_context<'a, C, I>(
    declared: &'a SqliteDeclaredStore<C, I>,
    principal: &'a Principal,
    clock: Arc<std::sync::Mutex<C>>,
    ids: Arc<std::sync::Mutex<I>>,
) -> WriteContext<'a, C, I, SqliteDeclaredStore<C, I>>
where
    C: Clock,
    I: IdFactory,
{
    WriteContext::new(declared, principal, clock, ids)
}

type ContextParts<'a, C, I> = (
    &'a SqliteDeclaredStore<C, I>,
    &'a crate::storage::sqlite::observed::SqliteObservedStore<C, I>,
    &'a Principal,
    Arc<std::sync::Mutex<C>>,
    Arc<std::sync::Mutex<I>>,
    &'a crate::config::VogtConfig,
);

pub(super) fn context_parts<C, I>(ctx: &AppContext<C, I>) -> ContextParts<'_, C, I>
where
    C: Clock,
    I: IdFactory,
{
    (
        &ctx.declared,
        &ctx.observed,
        &ctx.principal,
        std::sync::Arc::clone(&ctx.clock),
        std::sync::Arc::clone(&ctx.id_factory),
        &ctx.config,
    )
}

#[cfg(test)]
mod sessions_test;

#[cfg(test)]
mod handoff_test;
