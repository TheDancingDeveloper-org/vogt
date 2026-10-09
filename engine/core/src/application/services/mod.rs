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

pub mod auth;
pub mod freshness;
pub mod history;
pub mod install;
pub mod work;

pub mod import_merge;
pub mod inbox;
pub mod initiatives;
pub mod lifecycle;
pub mod notifications;
pub mod preferences;
#[allow(dead_code)]
pub mod sessions;

use crate::application::context::{AppContext, Built};
use crate::application::writes::WriteContext;
use crate::core::{Clock, IdFactory, Moment, Principal};
use crate::storage::sqlite::declared::SqliteDeclaredStore;
use std::sync::Arc;

#[cfg(test)]
mod services_test;

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
