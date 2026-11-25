//! Coding sessions — the work opening a terminal on its own project (WI-1073,
//! sessions part I).
//!
//! Ports the first half of `src/vogt/application/services/sessions.py`: start,
//! stop, list, sweep, log tail, input, screen, wait, wake, keep awake,
//! hibernate, set role and answer, plus the shared helpers both halves use.
//! Sessions part II (history, search, replies, bind, blocked reports, rename,
//! remove, grants, wait any) lands in the same module.
//!
//! The declared half is the Rust `CodingSession`: an engine id, a project, an
//! optional work item, the actor it runs as, its cwd, template, model, effort
//! and reason. The live half — name, activity, approval, role, hibernation —
//! belongs to the engine and is read, never stored.
//!
//! The terminal starts before the declared write. The engine and SQLite cannot
//! share a transaction, so the ordering is: start the terminal, then record it;
//! if the write fails, kill what was started and raise the original failure.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use super::preferences::{optional_i64, optional_string, require_string};

use crate::adapters::engine::{
    CreateSession, EngineApproval, EngineBlocked, EngineClient, EngineHibernation, EngineResources,
    EngineScreen, EngineSession, EngineSweepEntry, EngineWait,
};
use crate::adapters::transcripts;
use crate::application::brief::{self, ComposedBrief, WhyResult, WorkBrief};
use crate::application::context::{write_of, AppContext};
use crate::application::dispatch as dispatch_profiles;
use crate::application::resolve;
use crate::application::session_blocked::{self, known_or, BOUNDARIES, NEEDS};
use crate::application::session_commit;
use crate::application::writes::{self, audited_action, audited_write, WriteOutcome};
use crate::auth::{self, TOKEN_ENTROPY_BYTES};
use crate::core::cold_wake;
use crate::core::{py_repr, Actor, ActorKind, CodingSession, Moment, Token, TokenKind, WorkItem};
use crate::core::{Clock, IdFactory};
use crate::decisions;
use crate::delivery::{self};
use crate::errors::VogtError;
use crate::storage::interface::{DeclaredStore, ReadView, WriteTxn};

const SESSION_START: &str = "session.start";
/// The audit operation (and event) an oversight `override` writes beside the
/// start it allowed (WI-1329); the Inbox lists them.
pub const DISPATCH_OVERRIDE: &str = "dispatch.override";
const SESSION_STOP: &str = "session.stop";
const SESSION_INPUT: &str = "session.input";
const SESSION_ANSWER: &str = "session.answer";
const SESSION_HIBERNATE: &str = "session.hibernate";
const SESSION_WAKE: &str = "session.wake";
const SESSION_KEEP_AWAKE: &str = "session.keep_awake";
const SESSION_SET_ROLE: &str = "session.set_role";
const SESSION_RENAME: &str = "session.rename";
const SESSION_REMOVE: &str = "session.remove";
const SESSION_TOKEN: &str = "session.token";

/// The longest `session.input` text the engine accepts in one write.
const SESSION_INPUT_MAX_BYTES: usize = 64 * 1024;

/// How long `session.input` watches for what became of a submitted input: up to
/// this many screen reads, this far apart.
const CONFIRM_READS: usize = 10;
const CONFIRM_INTERVAL: Duration = Duration::from_millis(200);
const MAX_ENTERS: usize = 3;
const SETTLE: Duration = Duration::from_secs(10);
const READY_GATE: Duration = Duration::from_secs(30);
const TYPED_READS: usize = 10;

/// The keys `session.input` accepts.
const INPUT_KEYS: [&str; 10] = [
    "enter",
    "esc",
    "tab",
    "up",
    "down",
    "left",
    "right",
    "ctrl-c",
    "ctrl-d",
    "backspace",
];

#[cfg(test)]
mod tests;

mod answer;
mod budget;
mod digest;
mod input;
mod lifecycle;
mod list;
mod manage;
mod models;
mod screen;
mod start;
mod subject;
mod summary;
mod support;
mod sweep;
mod wait;
mod wire;

pub use self::answer::*;
pub use self::budget::*;
use self::digest::*;
pub use self::input::*;
pub use self::lifecycle::*;
pub use self::list::*;
pub use self::manage::*;
pub use self::models::*;
pub use self::screen::*;
pub use self::start::*;
use self::subject::*;
pub(in crate::application::services) use self::summary::*;
pub(crate) use self::support::*;
pub use self::sweep::*;
pub use self::wait::*;
pub use self::wire::*;
