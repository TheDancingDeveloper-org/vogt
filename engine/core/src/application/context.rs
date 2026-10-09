//! The context every use-case runs in. Ports `src/vogt/application/context.py`.
//!
//! Carries the configuration, both stores, the authenticated principal, and the
//! injectable clock and id factory. Adapters build one of these and hand it to
//! the registry; they never reach past it.
//!
//! The clock and the id factory are type parameters. A step clock is not a wall
//! clock and a sequential factory is not a random one, and the stores are built
//! with the same concrete types, so a context states which it holds.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::adapters::engine::EngineClient;
use crate::adapters::git::{
    clone_repository, push_branch, CloneOutcome, CloneRequest, PushOutcome, PushRequest,
};
use crate::adapters::peer::PeerClient;
use crate::config::VogtConfig;
use crate::core::{
    clock_from_env, fresh_id, hooks_active, local_principal, utc_now, Clock, IdFactory, Moment,
    Principal, SequentialIds, StepClock, Token, CLOCK_ENV, IDS_ENV,
};
use crate::errors::VogtError;
use crate::storage::sqlite::declared::SqliteDeclaredStore;
use crate::storage::sqlite::observed::SqliteObservedStore;

/// How `project.import` obtains a checkout. A function pointer, the way Python
/// stores a callable: tests substitute one without standing up a network.
pub type Cloner = fn(&CloneRequest) -> Result<CloneOutcome, VogtError>;

/// How `forge.publish` pushes the local default branch. The real function
/// never carries a force flag.
pub type Pusher = fn(&PushRequest) -> Result<PushOutcome, VogtError>;

/// Where a client should reach this instance. The core-only answer is the
/// configuration's own; behind a front door the adapter fills it per request,
/// because the door owns the address and the mount points.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PublicIdentity {
    pub public_url: Option<String>,
    pub api_path: Option<String>,
    pub mcp_path: Option<String>,
}

/// One request's worth of application state.
pub struct AppContext<C, I> {
    pub config: VogtConfig,
    pub declared: SqliteDeclaredStore<C, I>,
    pub observed: SqliteObservedStore<C, I>,
    pub principal: Principal,
    /// The token that authenticated this request, secret-free, or `None` on the
    /// local surface. Only `auth.logout` and `auth.whoami` read it: the
    /// principal is still the identity.
    pub token: Option<Token>,
    /// The one clock, shared with both stores. A clone would tick on its own.
    pub clock: Arc<std::sync::Mutex<C>>,
    /// The one id factory, shared with both stores. `SequentialIds` persists its
    /// counts, and a copy rewrites the file from what it alone has drawn.
    pub id_factory: Arc<std::sync::Mutex<I>>,
    pub cloner: Cloner,
    pub pusher: Pusher,
    /// The session engine, or `None` when none is configured. `None` is not an
    /// outage: the `session.*` operations say so, and nothing else is affected.
    pub engine: Option<EngineClient>,
    /// A peer instance `instance.diagnostics` can ask about itself, or `None`.
    pub peer: Option<PeerClient>,
    pub public_identity: PublicIdentity,
}

/// The wall clock. Reads the system; it holds no state of its own.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&mut self) -> Moment {
        utc_now()
    }
}

/// Fresh ULIDs, from the process clock and `/dev/urandom`. The default factory,
/// replaced by `SequentialIds` when `VOGT_TEST_IDS=sequential`.
#[derive(Debug, Clone, Copy, Default)]
pub struct RandomIds;

impl IdFactory for RandomIds {
    fn next(&mut self, prefix: &str) -> String {
        fresh_id(prefix)
    }
}

/// Which clock a built context holds.
pub enum HooksClock {
    System(SystemClock),
    Step(StepClock),
}

/// Which id factory a built context holds.
pub enum HooksIds {
    Random(RandomIds),
    Sequential(SequentialIds),
}

/// A context whose clock and id factory are whichever the hooks selected.
pub enum Built {
    SystemRandom(AppContext<SystemClock, RandomIds>),
    SystemSequential(AppContext<SystemClock, SequentialIds>),
    StepRandom(AppContext<StepClock, RandomIds>),
    StepSequential(AppContext<StepClock, SequentialIds>),
}

/// Announced once per process. `build_context` runs per request, and a warning
/// per request would bury the one fact that matters: this process is not using
/// wall-clock time.
fn announce_hooks(clock: Option<&str>, ids: Option<&str>) {
    static ANNOUNCED: std::sync::Once = std::sync::Once::new();
    let active = hooks_active(clock, ids);
    if active.is_empty() {
        return;
    }
    ANNOUNCED.call_once(|| {
        tracing::warn!(
            target: "vogt.context",
            "deterministic test hooks are active: {}",
            active.join(", "),
        );
    });
}

/// The step clock the environment asks for, unless the caller brought its own.
///
/// A caller that passed a clock is a test, and a test must not be silently
/// redirected by whatever the suite exported.
pub fn resolve_clock(given: Option<StepClock>) -> Result<HooksClock, VogtError> {
    if let Some(clock) = given {
        return Ok(HooksClock::Step(clock));
    }
    match clock_from_env(std::env::var(CLOCK_ENV).ok().as_deref())? {
        Some(clock) => Ok(HooksClock::Step(clock)),
        None => Ok(HooksClock::System(SystemClock)),
    }
}

/// The sequential id factory, unless the caller brought its own. `path` is
/// where the counters persist, so a second process continues the sequence.
pub fn resolve_ids(given: Option<SequentialIds>, path: PathBuf) -> Result<HooksIds, VogtError> {
    if let Some(ids) = given {
        return Ok(HooksIds::Sequential(ids));
    }
    match crate::core::ids_from_env(std::env::var(IDS_ENV).ok().as_deref(), Some(path))? {
        Some(ids) => Ok(HooksIds::Sequential(ids)),
        None => Ok(HooksIds::Random(RandomIds)),
    }
}

/// Build a context over the SQLite backend.
///
/// The principal is passed in by the adapter that authenticated it, so it is
/// never read from request data. `None` is the local principal.
///
/// `clock` and `ids` are `Some` only when the caller brought its own. `None`
/// takes the hook environment and, failing that, the wall clock and fresh ids.
#[allow(clippy::too_many_arguments)]
pub fn build_context(
    config: VogtConfig,
    principal: Option<Principal>,
    clock: Option<StepClock>,
    ids: Option<SequentialIds>,
    token: Option<Token>,
    engine: Option<EngineClient>,
    peer: Option<PeerClient>,
    public_identity: Option<PublicIdentity>,
) -> Result<Built, VogtError> {
    let clock_env = std::env::var(CLOCK_ENV).ok();
    let ids_env = std::env::var(IDS_ENV).ok();
    let resolved_clock = resolve_clock(clock)?;
    let resolved_ids = resolve_ids(ids, config.resolved_data_dir().join("test-ids.json"))?;
    announce_hooks(clock_env.as_deref(), ids_env.as_deref());
    Ok(assemble(
        config,
        principal,
        resolved_clock,
        resolved_ids,
        token,
        engine,
        peer,
        public_identity,
    ))
}

#[allow(clippy::too_many_arguments)]
fn assemble(
    config: VogtConfig,
    principal: Option<Principal>,
    clock: HooksClock,
    ids: HooksIds,
    token: Option<Token>,
    engine: Option<EngineClient>,
    peer: Option<PeerClient>,
    public_identity: Option<PublicIdentity>,
) -> Built {
    match (clock, ids) {
        (HooksClock::System(clock), HooksIds::Random(ids)) => Built::SystemRandom(context_with(
            config,
            principal,
            clock,
            ids,
            token,
            engine,
            peer,
            public_identity,
        )),
        (HooksClock::System(clock), HooksIds::Sequential(ids)) => {
            Built::SystemSequential(context_with(
                config,
                principal,
                clock,
                ids,
                token,
                engine,
                peer,
                public_identity,
            ))
        }
        (HooksClock::Step(clock), HooksIds::Random(ids)) => Built::StepRandom(context_with(
            config,
            principal,
            clock,
            ids,
            token,
            engine,
            peer,
            public_identity,
        )),
        (HooksClock::Step(clock), HooksIds::Sequential(ids)) => {
            Built::StepSequential(context_with(
                config,
                principal,
                clock,
                ids,
                token,
                engine,
                peer,
                public_identity,
            ))
        }
    }
}

// The signature mirrors `build_context` in context.py; splitting it would
// hide which argument is which.
#[allow(clippy::too_many_arguments)]
fn context_with<C, I>(
    config: VogtConfig,
    principal: Option<Principal>,
    clock: C,
    ids: I,
    token: Option<Token>,
    engine: Option<EngineClient>,
    peer: Option<PeerClient>,
    public_identity: Option<PublicIdentity>,
) -> AppContext<C, I>
where
    C: Clock,
    I: IdFactory,
{
    let resolved = principal.unwrap_or_else(|| local_principal(&crate::core::os_user()));
    let engine = engine.or_else(|| {
        EngineClient::from_config(
            config.engine_url.as_deref(),
            // One stack secret for both directions: a deployment that set no
            // distinct engine credential still has the core token.
            config
                .engine_token_file
                .as_deref()
                .or(config.bootstrap_core_token_file.as_deref()),
        )
    });
    let peer = peer.or_else(|| {
        PeerClient::from_config(
            config.diagnostics_peer_url.as_deref(),
            config.diagnostics_peer_token_file.as_deref(),
            None,
        )
    });
    let clock = Arc::new(std::sync::Mutex::new(clock));
    let ids = Arc::new(std::sync::Mutex::new(ids));
    let synchronous = config.sqlite_synchronous.as_str();
    AppContext {
        declared: SqliteDeclaredStore::shared(
            config.declared_db_path(),
            Arc::clone(&clock),
            Arc::clone(&ids),
            synchronous,
        ),
        observed: SqliteObservedStore::shared(
            config.observed_db_path(),
            Arc::clone(&clock),
            Arc::clone(&ids),
            synchronous,
        ),
        principal: resolved,
        token,
        clock,
        id_factory: ids,
        cloner: clone_repository,
        pusher: push_branch,
        engine,
        peer,
        public_identity: public_identity.unwrap_or_else(|| identity_of(&config)),
        config,
    }
}

/// What the configuration itself says about where clients arrive.
fn identity_of(config: &VogtConfig) -> PublicIdentity {
    PublicIdentity {
        public_url: config.public_url.clone(),
        api_path: None,
        mcp_path: None,
    }
}

/// The same context over the two store files in another directory.
///
/// For `clone`, which migrates and sanitises a staged copy of a backup before
/// it replaces the live stores — so a failure part-way leaves the live stores
/// exactly as they were.
pub fn with_stores_at<C, I>(ctx: &AppContext<C, I>, data_dir: &Path) -> AppContext<C, I>
where
    C: Clock,
    I: IdFactory,
{
    let mut config = ctx.config.clone();
    config.data_dir = data_dir.to_path_buf();
    let synchronous = config.sqlite_synchronous.as_str();
    AppContext {
        declared: SqliteDeclaredStore::shared(
            data_dir.join(crate::storage::sqlite::DECLARED_DB_NAME),
            Arc::clone(&ctx.clock),
            Arc::clone(&ctx.id_factory),
            synchronous,
        ),
        observed: SqliteObservedStore::shared(
            data_dir.join(crate::storage::sqlite::OBSERVED_DB_NAME),
            Arc::clone(&ctx.clock),
            Arc::clone(&ctx.id_factory),
            synchronous,
        ),
        config,
        principal: ctx.principal.clone(),
        token: ctx.token.clone(),
        clock: Arc::clone(&ctx.clock),
        id_factory: Arc::clone(&ctx.id_factory),
        cloner: ctx.cloner,
        pusher: ctx.pusher,
        engine: None,
        peer: None,
        public_identity: ctx.public_identity.clone(),
    }
}

/// The `WriteContext` an audited write takes.
///
/// The clock and the id factory come off the context, and they are the same
/// ones the stores hold, so a draw inside the write continues the sequence the
/// store has already counted.
pub fn write_of<C, I>(
    ctx: &AppContext<C, I>,
) -> crate::application::writes::WriteContext<'_, C, I, SqliteDeclaredStore<C, I>> {
    crate::application::writes::WriteContext::new(
        &ctx.declared,
        &ctx.principal,
        Arc::clone(&ctx.clock),
        Arc::clone(&ctx.id_factory),
    )
}
