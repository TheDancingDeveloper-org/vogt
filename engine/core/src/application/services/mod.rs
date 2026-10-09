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
mod history;
pub mod install;
pub mod work;

pub mod import_merge;
mod inbox;
mod initiatives;
pub mod lifecycle;
mod notifications;
mod preferences;

use crate::application::context::{AppContext, Built};
use crate::application::writes::WriteContext;
use crate::core::{Clock, IdFactory, Moment, Principal};
use crate::errors::VogtError;
use crate::storage::sqlite::declared::SqliteDeclaredStore;
use std::sync::Arc;

#[cfg(test)]
mod services_test;

/// The shape the registry calls. Named operations are `fn` items, not closures,
/// so the pointer is stable.
pub type ServiceFn = fn(&Built, serde_json::Value) -> Result<serde_json::Value, VogtError>;

/// The S4 operations this module owns. An unknown name is `None`; the registry
/// decides what that means.
pub fn service_for(name: &str) -> Option<ServiceFn> {
    Some(match name {
        "preference.get" => preferences::preference_get_op,
        "preference.set" => preferences::preference_set_op,
        "notifications" => notifications::notifications_op,
        "initiative.create" => initiatives::initiative_create_op,
        "initiative.list" => initiatives::initiative_list_op,
        "initiative.update" => initiatives::initiative_update_op,
        "initiative.publish" => initiatives::initiative_publish_op,
        "inbox.list" => inbox::inbox_list_op,
        "inbox.archive" => inbox::inbox_archive_op,
        "inbox.snooze" => inbox::inbox_snooze_op,
        "inbox.restore" => inbox::inbox_restore_op,
        "backup" => lifecycle::backup_op,
        "restore" => lifecycle::restore_op,
        "export" => lifecycle::export_op,
        "import" => import_merge::import_op,
        "events.list" => history::events_list_op,
        "audit.list" => history::audit_list_op,
        _ => return None,
    })
}

/// Call `path` with the concrete context `built` holds.
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
