//! The optional graft integration (WI-2227; graft decision D10, plan §G2.9).
//!
//! graft is the estate's version-control server. An agent session that writes
//! to it needs a token of its own, so every changeset it makes carries the
//! session and the work item it was made for. This module is how the engine
//! hands it one:
//!
//! - **At start**, an agent session started through `POST /api/sessions` is
//!   given a graft token minted for it through graft's `MintToken` RPC, with
//!   the deployment's graft *service* token. graft's `MintToken` makes the
//!   new token a subset of the service token on every axis (scopes, life,
//!   identity), so the session can never be given more than the service
//!   token holds. The token names the Vogt session (`VOGT_SESSION_ID`, else
//!   the engine id) and its work item.
//! - **While the session lives**, the engine opens the session's workspace
//!   (`vogt/<session>`) with the session's token, so the session owns it, and
//!   renews its lease every third of the lease. A hibernated session still
//!   lives: its lease is kept, so its drafts survive to the wake.
//! - **When the session stops** (an exit that is not a hibernation, or the
//!   removal of a hibernated session), the engine closes the workspace. Its
//!   drafts are abandoned unless `ENGINE_GRAFT_KEEP_DRAFTS` is on.
//!
//! The session is told `GRAFT_TOKEN`, `GRAFT_ENDPOINT`, `GRAFT_REPO` and
//! `GRAFT_WORKSPACE`. Every name with `TOKEN` in it is kept out of the
//! hibernation record (`pty::is_secret_env`), so a wake mints again.
//!
//! The service token comes from a file (`ENGINE_GRAFT_SERVICE_TOKEN_FILE`) or
//! from the agent-auth helper (`ENGINE_GRAFT_SERVICE_SECRET`, fetched with a
//! one-line manifest so it is never an entry sessions could fetch). Neither
//! token is ever logged, put in argv, or written to disk by the engine.
//!
//! Off unless `ENGINE_GRAFT_ENABLED` is on. Optional stays optional: a graft
//! that cannot be reached, or refuses, is logged and the session starts
//! without graft variables.

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use dashmap::DashMap;
use graft_proto::pb;
use graft_proto::pb::admin_service_client::AdminServiceClient;
use graft_proto::pb::workspace_service_client::WorkspaceServiceClient;
use graft_proto::{ApiError, PROTOCOL_VERSION, VERSION_HEADER};
use tonic::codec::CompressionEncoding;
use tonic::metadata::MetadataValue;
use tonic::transport::{Channel, Endpoint};
use uuid::Uuid;

use crate::secret_broker::SecretBroker;

/// The variable the session's token is exported as; graft's CLI reads it.
pub const TOKEN_ENV: &str = "GRAFT_TOKEN";
/// The graft server URL, for the session's `graft --endpoint`.
pub const ENDPOINT_ENV: &str = "GRAFT_ENDPOINT";
/// The repository the token is scoped to, for `graft --repo`.
pub const REPO_ENV: &str = "GRAFT_REPO";
/// The workspace the engine opened for the session.
pub const WORKSPACE_ENV: &str = "GRAFT_WORKSPACE";

/// The name the service token is fetched under when it comes from the
/// agent-auth helper. It is never a session variable.
const SERVICE_SECRET_VAR: &str = "ENGINE_GRAFT_SERVICE_TOKEN";

/// The request header carrying the request id (graft-client's name for it).
const REQUEST_ID_HEADER: &str = "graft-request-id";
/// How long one RPC may take.
const RPC_TIMEOUT: Duration = Duration::from_secs(10);
/// How long to wait for a connection.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// How long a session start waits for graft before starting without it.
const START_TIMEOUT: Duration = Duration::from_secs(20);
/// How long a service token fetched from the helper is reused before it is
/// fetched again, so a rotation is picked up without a restart.
const SERVICE_TOKEN_REUSE: Duration = Duration::from_secs(600);
/// When a session's token has less than this left, the engine mints another
/// before it uses it to renew or close.
const TOKEN_MARGIN: Duration = Duration::from_secs(60);
/// The waits before each attempt to close a stopped session's workspace:
/// all inside a minute (graft plan §G2.9, "closed within one minute").
const CLOSE_BACKOFF: [Duration; 3] = [
    Duration::ZERO,
    Duration::from_secs(5),
    Duration::from_secs(20),
];

/// Where the graft service token comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServiceToken {
    /// A file holding the token (`ENGINE_GRAFT_SERVICE_TOKEN_FILE`), read
    /// at every mint so a rotated file is picked up.
    File(PathBuf),
    /// A secret the agent-auth helper fetches
    /// (`ENGINE_GRAFT_SERVICE_SECRET="PROJECT_ID SECRET_NAME"`).
    Secret {
        project_id: String,
        secret_name: String,
    },
}

/// The graft settings (`ENGINE_GRAFT_*`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraftConfig {
    /// `ENGINE_GRAFT_URL`: the graft server, e.g. `http://graft:7420`.
    pub url: String,
    /// `ENGINE_GRAFT_REPO`: the repository sessions work in.
    pub repo: String,
    /// `ENGINE_GRAFT_LINE` (`main`): the line a session's workspace tracks.
    pub line: String,
    /// `ENGINE_GRAFT_SCOPES` (`repo:<repo>:read,repo:<repo>:write`): what a
    /// session's token may do. The service token must hold every one.
    pub scopes: Vec<String>,
    /// `ENGINE_GRAFT_TOKEN_TTL` (`12h`): how long a session token lives.
    /// Zero asks for the most the server and the service token allow.
    pub token_ttl: Duration,
    /// `ENGINE_GRAFT_LEASE` (`30m`): the workspace lease; renewed every
    /// third of it.
    pub lease: Duration,
    /// `ENGINE_GRAFT_KEEP_DRAFTS` (off): closing a stopped session's
    /// workspace keeps its drafts instead of abandoning them.
    pub keep_drafts: bool,
    /// Where the service token comes from.
    pub service_token: ServiceToken,
}

impl GraftConfig {
    /// The settings from `get` (the environment, in `config::load`), or
    /// `None` when `ENGINE_GRAFT_ENABLED` is not on. An enabled integration
    /// with a setting missing or malformed is an error, so a deployment that
    /// asked for graft does not boot without it.
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Option<Self>, String> {
        let get = |name: &str| {
            get(name)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        let enabled = match get("ENGINE_GRAFT_ENABLED") {
            None => false,
            Some(v) => parse_bool("ENGINE_GRAFT_ENABLED", &v)?,
        };
        if !enabled {
            return Ok(None);
        }
        let required = |name: &str| {
            get(name).ok_or_else(|| format!("{name} is required when ENGINE_GRAFT_ENABLED is on"))
        };
        let url = required("ENGINE_GRAFT_URL")?;
        if !(url.starts_with("http://") || url.starts_with("https://"))
            || Endpoint::from_shared(url.clone()).is_err()
        {
            return Err(format!(
                "ENGINE_GRAFT_URL={url:?} is not a URL like http://graft:7420"
            ));
        }
        let repo = required("ENGINE_GRAFT_REPO")?;
        let line = get("ENGINE_GRAFT_LINE").unwrap_or_else(|| "main".to_string());
        let scopes: Vec<String> = match get("ENGINE_GRAFT_SCOPES") {
            Some(v) => v
                .split(|c: char| c == ',' || c.is_whitespace())
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect(),
            None => vec![format!("repo:{repo}:read"), format!("repo:{repo}:write")],
        };
        if scopes.is_empty() {
            return Err("ENGINE_GRAFT_SCOPES names no scope".into());
        }
        let token_ttl = match get("ENGINE_GRAFT_TOKEN_TTL") {
            None => Duration::from_secs(12 * 3600),
            Some(v) if v == "0" => Duration::ZERO,
            Some(v) => crate::hibernate_policy::parse_duration(&v).ok_or_else(|| {
                format!("ENGINE_GRAFT_TOKEN_TTL={v:?} is not a duration like 12h, 30m or 3600")
            })?,
        };
        let lease = match get("ENGINE_GRAFT_LEASE") {
            None => Duration::from_secs(30 * 60),
            Some(v) => crate::hibernate_policy::parse_duration(&v)
                .filter(|d| *d >= Duration::from_secs(30))
                .ok_or_else(|| {
                    format!("ENGINE_GRAFT_LEASE={v:?} is not a duration of at least 30s, like 30m")
                })?,
        };
        let keep_drafts = match get("ENGINE_GRAFT_KEEP_DRAFTS") {
            None => false,
            Some(v) => parse_bool("ENGINE_GRAFT_KEEP_DRAFTS", &v)?,
        };
        let service_token = match (
            get("ENGINE_GRAFT_SERVICE_TOKEN_FILE"),
            get("ENGINE_GRAFT_SERVICE_SECRET"),
        ) {
            (Some(path), None) => ServiceToken::File(PathBuf::from(path)),
            (None, Some(locator)) => {
                let mut fields = locator.split_whitespace();
                match (fields.next(), fields.next(), fields.next()) {
                    (Some(project_id), Some(secret_name), None) => ServiceToken::Secret {
                        project_id: project_id.to_string(),
                        secret_name: secret_name.to_string(),
                    },
                    _ => {
                        return Err(format!(
                            "ENGINE_GRAFT_SERVICE_SECRET={locator:?} is not 'PROJECT_ID SECRET_NAME'"
                        ))
                    }
                }
            }
            (Some(_), Some(_)) => {
                return Err(
                    "set one of ENGINE_GRAFT_SERVICE_TOKEN_FILE and ENGINE_GRAFT_SERVICE_SECRET, not both"
                        .into(),
                )
            }
            (None, None) => {
                return Err(
                    "ENGINE_GRAFT_SERVICE_TOKEN_FILE or ENGINE_GRAFT_SERVICE_SECRET is required when ENGINE_GRAFT_ENABLED is on"
                        .into(),
                )
            }
        };
        Ok(Some(Self {
            url,
            repo,
            line,
            scopes,
            token_ttl,
            lease,
            keep_drafts,
            service_token,
        }))
    }
}

fn parse_bool(name: &str, raw: &str) -> Result<bool, String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => Err(format!(
            "{name} must be one of 1/0, true/false, yes/no, or on/off"
        )),
    }
}

/// A bearer token. Its `Debug` never shows the value.
#[derive(Clone)]
struct Secret(String);

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<redacted>")
    }
}

/// A session's token and when it runs out (as asked for: graft does not say).
#[derive(Clone, Debug)]
struct SessionToken {
    value: Secret,
    expires: Instant,
}

/// What the engine holds for one session.
struct Held {
    /// The Vogt session the token names.
    session: String,
    work_item: Option<String>,
    /// The workspace, once opened.
    workspace: Option<String>,
    /// The latest token minted for the session, used to renew and close.
    token: Arc<tokio::sync::Mutex<SessionToken>>,
    renewer: Option<tokio::task::AbortHandle>,
}

/// Why a graft call failed. Never carries a token.
#[derive(Debug)]
pub enum GraftError {
    /// The service token could not be read.
    ServiceToken(String),
    /// graft answered with a typed error.
    Api(ApiError),
    /// graft could not be reached, or answered with no typed error.
    Transport(String),
}

impl std::fmt::Display for GraftError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GraftError::ServiceToken(e) => write!(f, "graft service token: {e}"),
            GraftError::Api(e) => write!(f, "graft: {e}"),
            GraftError::Transport(e) => write!(f, "graft unreachable: {e}"),
        }
    }
}

fn status_error(status: tonic::Status) -> GraftError {
    match ApiError::from_status(&status) {
        Some(e) => GraftError::Api(e),
        None => GraftError::Transport(format!("{:?}: {}", status.code(), status.message())),
    }
}

/// The graft integration: one per engine, shared by the session registry.
pub struct Graft {
    cfg: GraftConfig,
    broker: Arc<SecretBroker>,
    channel: OnceLock<Channel>,
    /// A service token the helper fetched, and when.
    service: tokio::sync::Mutex<Option<(Secret, Instant)>>,
    held: DashMap<Uuid, Held>,
}

impl Graft {
    pub fn new(cfg: GraftConfig, broker: Arc<SecretBroker>) -> Arc<Self> {
        Arc::new(Self {
            cfg,
            broker,
            channel: OnceLock::new(),
            service: tokio::sync::Mutex::new(None),
            held: DashMap::new(),
        })
    }

    /// The settings in use.
    pub fn config(&self) -> &GraftConfig {
        &self.cfg
    }

    /// Whether the engine holds graft state for session `id`.
    pub fn holds(&self, id: Uuid) -> bool {
        self.held.contains_key(&id)
    }

    /// The workspace held for session `id`, if one is open.
    pub fn workspace_of(&self, id: Uuid) -> Option<String> {
        self.held.get(&id).and_then(|h| h.workspace.clone())
    }

    /// The workspace name for Vogt session `session`: `vogt/<session>`,
    /// lowercased, with anything graft does not allow in a name replaced.
    pub fn workspace_name(session: &str) -> String {
        let segment: String = session
            .chars()
            .map(|c| {
                let c = c.to_ascii_lowercase();
                if c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-') {
                    c
                } else {
                    '-'
                }
            })
            .collect();
        let segment = if segment.starts_with(|c: char| c.is_ascii_alphanumeric()) {
            segment
        } else {
            format!("s{segment}")
        };
        format!("vogt/{segment}")
    }

    /// The subject a session's token is minted for.
    fn subject(session: &str) -> String {
        format!("agent:{session}")
    }

    fn channel(&self) -> Result<Channel, GraftError> {
        if let Some(c) = self.channel.get() {
            return Ok(c.clone());
        }
        // Built on first use, inside the runtime: a lazy channel spawns its
        // worker as it is made.
        let channel = Endpoint::from_shared(self.cfg.url.clone())
            .map_err(|e| GraftError::Transport(e.to_string()))?
            .connect_timeout(CONNECT_TIMEOUT)
            .tcp_nodelay(true)
            .connect_lazy();
        Ok(self.channel.get_or_init(|| channel).clone())
    }

    fn admin(&self) -> Result<AdminServiceClient<Channel>, GraftError> {
        Ok(AdminServiceClient::new(self.channel()?)
            .send_compressed(CompressionEncoding::Gzip)
            .accept_compressed(CompressionEncoding::Gzip))
    }

    fn workspaces(&self) -> Result<WorkspaceServiceClient<Channel>, GraftError> {
        Ok(WorkspaceServiceClient::new(self.channel()?)
            .send_compressed(CompressionEncoding::Gzip)
            .accept_compressed(CompressionEncoding::Gzip))
    }

    /// `msg` with the protocol version, a request id, the bearer and a
    /// deadline. The bearer is marked sensitive so tonic never prints it.
    fn request<T>(msg: T, bearer: &Secret) -> Result<tonic::Request<T>, GraftError> {
        let mut req = tonic::Request::new(msg);
        let md = req.metadata_mut();
        md.insert(
            VERSION_HEADER,
            MetadataValue::try_from(PROTOCOL_VERSION.to_string())
                .map_err(|_| GraftError::Transport("protocol version".into()))?,
        );
        if let Ok(v) = MetadataValue::try_from(Uuid::new_v4().to_string()) {
            md.insert(REQUEST_ID_HEADER, v);
        }
        let mut v = MetadataValue::try_from(format!("Bearer {}", bearer.0))
            .map_err(|_| GraftError::ServiceToken("not a valid header value".into()))?;
        v.set_sensitive(true);
        md.insert("authorization", v);
        req.set_timeout(RPC_TIMEOUT);
        Ok(req)
    }

    /// The service token: the file read now, or the helper's value reused for
    /// [`SERVICE_TOKEN_REUSE`].
    async fn service_token(&self) -> Result<Secret, GraftError> {
        match &self.cfg.service_token {
            ServiceToken::File(path) => {
                let raw = tokio::fs::read_to_string(path).await.map_err(|e| {
                    GraftError::ServiceToken(format!("reading {}: {e}", path.display()))
                })?;
                let value = raw.trim().to_string();
                if value.is_empty() {
                    return Err(GraftError::ServiceToken(format!(
                        "{} is empty",
                        path.display()
                    )));
                }
                Ok(Secret(value))
            }
            ServiceToken::Secret {
                project_id,
                secret_name,
            } => {
                let mut cached = self.service.lock().await;
                if let Some((value, at)) = cached.as_ref() {
                    if at.elapsed() < SERVICE_TOKEN_REUSE {
                        return Ok(value.clone());
                    }
                }
                let value = self
                    .broker
                    .fetch_for_engine(SERVICE_SECRET_VAR, project_id, secret_name)
                    .await
                    .map_err(|e| GraftError::ServiceToken(e.to_string()))?;
                let value = Secret(value.trim().to_string());
                *cached = Some((value.clone(), Instant::now()));
                Ok(value)
            }
        }
    }

    /// Mint a token for `session`. When the configured life is more than the
    /// service token may grant, graft says how much it may, and the mint is
    /// asked again for that.
    async fn mint(
        &self,
        session: &str,
        work_item: Option<&str>,
    ) -> Result<SessionToken, GraftError> {
        let service = self.service_token().await?;
        let mut ttl_s = self.cfg.token_ttl.as_secs();
        for _ in 0..2 {
            let msg = pb::MintTokenRequest {
                subject: Self::subject(session),
                scopes: self.cfg.scopes.clone(),
                ttl_s,
                session: Some(session.to_string()),
                work_item: work_item.map(str::to_string),
            };
            match self
                .admin()?
                .mint_token(Self::request(msg, &service)?)
                .await
            {
                Ok(resp) => {
                    let token = resp.into_inner().token;
                    // Asking for zero means "the most allowed", which graft
                    // does not report: renew as if it were the margin's worth,
                    // so the engine mints again rather than trust a guess.
                    let life = if ttl_s == 0 {
                        TOKEN_MARGIN
                    } else {
                        Duration::from_secs(ttl_s)
                    };
                    return Ok(SessionToken {
                        value: Secret(token),
                        expires: Instant::now() + life,
                    });
                }
                Err(status) => match status_error(status) {
                    GraftError::Api(ApiError::LimitExceeded { limit, max })
                        if limit == "token-ttl" && max > 0 && max < ttl_s =>
                    {
                        ttl_s = max;
                    }
                    other => return Err(other),
                },
            }
        }
        Err(GraftError::Api(ApiError::LimitExceeded {
            limit: "token-ttl".into(),
            max: ttl_s,
        }))
    }

    /// Open (or find again) `workspace` with the session's own token, so
    /// the session owns it.
    async fn open(&self, workspace: &str, token: &Secret) -> Result<i64, GraftError> {
        let msg = pb::OpenWorkspaceRequest {
            repo: self.cfg.repo.clone(),
            name: Some(workspace.to_string()),
            line: self.cfg.line.clone(),
            at: None,
            lease_ms: self.lease_ms(),
        };
        let view = self
            .workspaces()?
            .open_workspace(Self::request(msg, token)?)
            .await
            .map_err(status_error)?;
        Ok(view.into_inner().lease_until_ms)
    }

    async fn renew(&self, workspace: &str, token: &Secret) -> Result<i64, GraftError> {
        let msg = pb::RenewLeaseRequest {
            workspace: Some(pb::WorkspaceRef {
                repo: self.cfg.repo.clone(),
                name: workspace.to_string(),
            }),
            lease_ms: self.lease_ms(),
        };
        let view = self
            .workspaces()?
            .renew_lease(Self::request(msg, token)?)
            .await
            .map_err(status_error)?;
        Ok(view.into_inner().lease_until_ms)
    }

    async fn close(&self, workspace: &str, token: &Secret) -> Result<(), GraftError> {
        let msg = pb::CloseWorkspaceRequest {
            workspace: Some(pb::WorkspaceRef {
                repo: self.cfg.repo.clone(),
                name: workspace.to_string(),
            }),
            keep_drafts: self.cfg.keep_drafts,
        };
        self.workspaces()?
            .close_workspace(Self::request(msg, token)?)
            .await
            .map_err(status_error)?;
        Ok(())
    }

    fn lease_ms(&self) -> u64 {
        u64::try_from(self.cfg.lease.as_millis()).unwrap_or(u64::MAX)
    }

    /// A token for the session good for at least [`TOKEN_MARGIN`]: the one
    /// held, or a new one minted in its place.
    async fn fresh_token(
        &self,
        slot: &tokio::sync::Mutex<SessionToken>,
        session: &str,
        work_item: Option<&str>,
        margin: Duration,
    ) -> Result<Secret, GraftError> {
        let mut held = slot.lock().await;
        if held.expires.saturating_duration_since(Instant::now()) <= margin {
            *held = self.mint(session, work_item).await?;
        }
        Ok(held.value.clone())
    }

    /// Give session `id` (Vogt session `session`, working on `work_item`) a
    /// graft token and a workspace, and keep its lease. Returns the variables
    /// to start it with, or `None` when graft could not be had (logged; the
    /// session starts without them). Starting again (a wake) mints a new
    /// token for the new process and keeps the workspace it had.
    pub async fn begin(
        self: &Arc<Self>,
        id: Uuid,
        session: &str,
        work_item: Option<&str>,
    ) -> Option<Vec<(String, String)>> {
        match tokio::time::timeout(START_TIMEOUT, self.begin_inner(id, session, work_item)).await {
            Ok(Ok(env)) => Some(env),
            Ok(Err(e)) => {
                tracing::warn!(
                    target: "vogt::audit",
                    event = "graft.session.begin",
                    session_id = %id,
                    vogt_session = %session,
                    error = %e,
                    "could not give the session a graft token; it starts without one"
                );
                None
            }
            Err(_) => {
                tracing::warn!(
                    target: "vogt::audit",
                    event = "graft.session.begin",
                    session_id = %id,
                    vogt_session = %session,
                    "graft did not answer in time; the session starts without a graft token"
                );
                None
            }
        }
    }

    async fn begin_inner(
        self: &Arc<Self>,
        id: Uuid,
        session: &str,
        work_item: Option<&str>,
    ) -> Result<Vec<(String, String)>, GraftError> {
        let token = self.mint(session, work_item).await?;
        let workspace = Self::workspace_name(session);
        tracing::info!(
            target: "vogt::audit",
            event = "graft.token.minted",
            session_id = %id,
            vogt_session = %session,
            work_item = work_item.unwrap_or(""),
            repo = %self.cfg.repo,
            scopes = %self.cfg.scopes.join(","),
            "graft token minted for the session"
        );
        // The open is idempotent for the same owner and line, so a wake (or
        // an engine restart inside the lease) finds the workspace it had.
        let opened = match self.open(&workspace, &token.value).await {
            Ok(_) => Some(workspace.clone()),
            Err(e) => {
                tracing::warn!(
                    target: "vogt::audit",
                    event = "graft.workspace.open",
                    session_id = %id,
                    workspace = %workspace,
                    error = %e,
                    "could not open the session's graft workspace; the session keeps its token"
                );
                None
            }
        };
        let mut env = vec![
            (TOKEN_ENV.to_string(), token.value.0.clone()),
            (ENDPOINT_ENV.to_string(), self.cfg.url.clone()),
            (REPO_ENV.to_string(), self.cfg.repo.clone()),
        ];
        if let Some(ws) = &opened {
            env.push((WORKSPACE_ENV.to_string(), ws.clone()));
        }
        let slot = Arc::new(tokio::sync::Mutex::new(token));
        let renewer = opened
            .as_ref()
            .map(|ws| self.spawn_renewer(id, session.to_string(), work_item, ws.clone(), &slot));
        let held = Held {
            session: session.to_string(),
            work_item: work_item.map(str::to_string),
            workspace: opened,
            token: slot,
            renewer,
        };
        if let Some(previous) = self.held.insert(id, held) {
            if let Some(r) = previous.renewer {
                r.abort();
            }
        }
        Ok(env)
    }

    fn spawn_renewer(
        self: &Arc<Self>,
        id: Uuid,
        session: String,
        work_item: Option<&str>,
        workspace: String,
        slot: &Arc<tokio::sync::Mutex<SessionToken>>,
    ) -> tokio::task::AbortHandle {
        let me = Arc::downgrade(self);
        let slot = Arc::clone(slot);
        let work_item = work_item.map(str::to_string);
        let every = (self.cfg.lease / 3).max(Duration::from_millis(10));
        let task = tokio::spawn(async move {
            let mut tick = tokio::time::interval(every);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            tick.tick().await;
            loop {
                tick.tick().await;
                let Some(graft) = me.upgrade() else { return };
                graft
                    .renew_once(id, &session, work_item.as_deref(), &workspace, &slot)
                    .await;
            }
        });
        task.abort_handle()
    }

    /// One renewal: a token that outlives the next lease, then `RenewLease`.
    /// A workspace graft has ended (its lease lapsed while graft was out of
    /// reach) is opened again under the same name.
    async fn renew_once(
        &self,
        id: Uuid,
        session: &str,
        work_item: Option<&str>,
        workspace: &str,
        slot: &tokio::sync::Mutex<SessionToken>,
    ) {
        let margin = self.cfg.lease.max(TOKEN_MARGIN);
        let result = match self.fresh_token(slot, session, work_item, margin).await {
            Ok(token) => match self.renew(workspace, &token).await {
                Err(GraftError::Api(ApiError::LeaseExpired { .. } | ApiError::NotFound { .. })) => {
                    tracing::warn!(
                        target: "vogt::audit",
                        event = "graft.workspace.reopen",
                        session_id = %id,
                        workspace = %workspace,
                        "the session's graft workspace had ended; opening it again"
                    );
                    self.open(workspace, &token).await
                }
                other => other,
            },
            Err(e) => Err(e),
        };
        if let Err(e) = result {
            tracing::warn!(
                session_id = %id,
                workspace = %workspace,
                error = %e,
                "could not renew the session's graft workspace lease"
            );
        }
    }

    /// The session has stopped: stop renewing and close its workspace,
    /// retrying inside a minute. A no-op for a session graft never held.
    pub async fn finish(&self, id: Uuid) {
        let Some((_, held)) = self.held.remove(&id) else {
            return;
        };
        if let Some(r) = held.renewer {
            r.abort();
        }
        let Some(workspace) = held.workspace else {
            return;
        };
        let mut last = None;
        for wait in CLOSE_BACKOFF {
            tokio::time::sleep(wait).await;
            let attempt = match self
                .fresh_token(
                    &held.token,
                    &held.session,
                    held.work_item.as_deref(),
                    TOKEN_MARGIN,
                )
                .await
            {
                Ok(token) => self.close(&workspace, &token).await,
                Err(e) => Err(e),
            };
            match attempt {
                Ok(()) => {
                    tracing::info!(
                        target: "vogt::audit",
                        event = "graft.workspace.closed",
                        session_id = %id,
                        workspace = %workspace,
                        keep_drafts = self.cfg.keep_drafts,
                        "the stopped session's graft workspace is closed"
                    );
                    return;
                }
                Err(e) => last = Some(e),
            }
        }
        tracing::warn!(
            target: "vogt::audit",
            event = "graft.workspace.closed",
            session_id = %id,
            workspace = %workspace,
            error = %last.map(|e| e.to_string()).unwrap_or_default(),
            "could not close the stopped session's graft workspace; graft ends it when its lease lapses"
        );
    }

    /// [`Self::finish`] from a synchronous caller (an exit hook, a removal):
    /// spawned on the current runtime, or skipped (logged) without one.
    pub fn end(self: &Arc<Self>, id: Uuid) {
        if !self.held.contains_key(&id) {
            return;
        }
        match tokio::runtime::Handle::try_current() {
            Ok(runtime) => {
                let me = Arc::clone(self);
                runtime.spawn(async move { me.finish(id).await });
            }
            Err(_) => tracing::warn!(
                session_id = %id,
                "no runtime to close the session's graft workspace from; its lease will lapse"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    use graft_proto::pb::admin_service_server::{AdminService, AdminServiceServer};
    use graft_proto::pb::workspace_service_server::{WorkspaceService, WorkspaceServiceServer};
    use tonic::{Request, Response, Status};

    const SERVICE: &str = "svc-token-0123456789";

    /// What the fake server saw, by RPC: the bearer presented and what was
    /// asked for.
    #[derive(Default)]
    struct Seen {
        mints: Vec<(String, pb::MintTokenRequest)>,
        opens: Vec<(String, pb::OpenWorkspaceRequest)>,
        renews: Vec<(String, pb::RenewLeaseRequest)>,
        closes: Vec<(String, pb::CloseWorkspaceRequest)>,
    }

    /// A graft server with just enough of `MintToken` and the workspace RPCs:
    /// it refuses a mint not made with the service token or longer than
    /// `max_ttl_s`, mints `tok-<n>`, and refuses workspace calls not made
    /// with a token it minted.
    #[derive(Clone)]
    struct Fake {
        seen: Arc<Mutex<Seen>>,
        max_ttl_s: u64,
        minted: Arc<Mutex<Vec<String>>>,
        lapsed: Arc<std::sync::atomic::AtomicBool>,
    }

    impl Fake {
        fn new(max_ttl_s: u64) -> Self {
            Self {
                seen: Arc::default(),
                max_ttl_s,
                minted: Arc::default(),
                lapsed: Arc::default(),
            }
        }

        fn bearer<T>(req: &Request<T>) -> Result<String, Status> {
            let version = req
                .metadata()
                .get(VERSION_HEADER)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            if version != PROTOCOL_VERSION.to_string() {
                return Err(Status::failed_precondition("protocol"));
            }
            req.metadata()
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.strip_prefix("Bearer "))
                .map(str::to_string)
                .ok_or_else(|| Status::from(ApiError::Unauthenticated))
        }

        fn session_bearer<T>(&self, req: &Request<T>) -> Result<String, Status> {
            let bearer = Self::bearer(req)?;
            if self.minted.lock().unwrap().contains(&bearer) {
                Ok(bearer)
            } else {
                Err(Status::from(ApiError::Unauthenticated))
            }
        }

        fn view(name: &str) -> pb::WorkspaceView {
            pb::WorkspaceView {
                name: name.to_string(),
                lease_until_ms: 1,
                ..Default::default()
            }
        }
    }

    #[tonic::async_trait]
    impl AdminService for Fake {
        async fn create_repo(
            &self,
            _: Request<pb::CreateRepoRequest>,
        ) -> Result<Response<pb::Empty>, Status> {
            Err(Status::unimplemented("fake"))
        }
        async fn import_changesets(
            &self,
            _: Request<pb::ImportChangesetsRequest>,
        ) -> Result<Response<pb::CreatedList>, Status> {
            Err(Status::unimplemented("fake"))
        }
        async fn gc(&self, _: Request<pb::GcRequest>) -> Result<Response<pb::GcReport>, Status> {
            Err(Status::unimplemented("fake"))
        }
        async fn mint_token(
            &self,
            req: Request<pb::MintTokenRequest>,
        ) -> Result<Response<pb::Token>, Status> {
            let bearer = Self::bearer(&req)?;
            let msg = req.into_inner();
            self.seen
                .lock()
                .unwrap()
                .mints
                .push((bearer.clone(), msg.clone()));
            if bearer != SERVICE {
                return Err(ApiError::Unauthenticated.into());
            }
            if msg.ttl_s > self.max_ttl_s {
                return Err(ApiError::LimitExceeded {
                    limit: "token-ttl".into(),
                    max: self.max_ttl_s,
                }
                .into());
            }
            let mut minted = self.minted.lock().unwrap();
            let token = format!("tok-{}", minted.len());
            minted.push(token.clone());
            Ok(Response::new(pb::Token { token }))
        }
    }

    #[tonic::async_trait]
    impl WorkspaceService for Fake {
        async fn open_workspace(
            &self,
            req: Request<pb::OpenWorkspaceRequest>,
        ) -> Result<Response<pb::WorkspaceView>, Status> {
            let bearer = self.session_bearer(&req)?;
            let msg = req.into_inner();
            let name = msg.name.clone().unwrap_or_default();
            self.seen.lock().unwrap().opens.push((bearer, msg));
            self.lapsed
                .store(false, std::sync::atomic::Ordering::SeqCst);
            Ok(Response::new(Self::view(&name)))
        }
        async fn get_workspace(
            &self,
            _: Request<pb::WorkspaceRef>,
        ) -> Result<Response<pb::WorkspaceView>, Status> {
            Err(Status::unimplemented("fake"))
        }
        async fn list_workspaces(
            &self,
            _: Request<pb::ListWorkspacesRequest>,
        ) -> Result<Response<pb::WorkspaceViews>, Status> {
            Err(Status::unimplemented("fake"))
        }
        async fn renew_lease(
            &self,
            req: Request<pb::RenewLeaseRequest>,
        ) -> Result<Response<pb::WorkspaceView>, Status> {
            let bearer = self.session_bearer(&req)?;
            let msg = req.into_inner();
            let name = msg.workspace.clone().unwrap_or_default().name;
            self.seen.lock().unwrap().renews.push((bearer, msg));
            if self.lapsed.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(ApiError::LeaseExpired { workspace: name }.into());
            }
            Ok(Response::new(Self::view(&name)))
        }
        async fn set_head(
            &self,
            _: Request<pb::SetHeadRequest>,
        ) -> Result<Response<pb::WorkspaceView>, Status> {
            Err(Status::unimplemented("fake"))
        }
        async fn sync(&self, _: Request<pb::SyncRequest>) -> Result<Response<pb::Synced>, Status> {
            Err(Status::unimplemented("fake"))
        }
        async fn stack(
            &self,
            _: Request<pb::StackRequest>,
        ) -> Result<Response<pb::WorkspaceView>, Status> {
            Err(Status::unimplemented("fake"))
        }
        async fn close_workspace(
            &self,
            req: Request<pb::CloseWorkspaceRequest>,
        ) -> Result<Response<pb::Empty>, Status> {
            let bearer = self.session_bearer(&req)?;
            let msg = req.into_inner();
            self.seen.lock().unwrap().closes.push((bearer, msg));
            Ok(Response::new(pb::Empty {}))
        }
    }

    /// Serve `fake` on a loopback port and return its URL.
    async fn serve(fake: Fake) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
        tokio::spawn(
            tonic::transport::Server::builder()
                .add_service(
                    AdminServiceServer::new(fake.clone())
                        .accept_compressed(CompressionEncoding::Gzip)
                        .send_compressed(CompressionEncoding::Gzip),
                )
                .add_service(
                    WorkspaceServiceServer::new(fake)
                        .accept_compressed(CompressionEncoding::Gzip)
                        .send_compressed(CompressionEncoding::Gzip),
                )
                .serve_with_incoming(incoming),
        );
        format!("http://{addr}")
    }

    fn config(url: String, token_file: PathBuf, lease: Duration) -> GraftConfig {
        GraftConfig {
            url,
            repo: "rusttorrent".into(),
            line: "main".into(),
            scopes: vec![
                "repo:rusttorrent:read".into(),
                "repo:rusttorrent:write".into(),
            ],
            token_ttl: Duration::from_secs(12 * 3600),
            lease,
            keep_drafts: false,
            service_token: ServiceToken::File(token_file),
        }
    }

    fn broker() -> Arc<SecretBroker> {
        Arc::new(SecretBroker::with(
            "127.0.0.1:0".parse().unwrap(),
            PathBuf::from("/nonexistent/vogt-agent-auth"),
            Vec::new(),
        ))
    }

    fn token_file(dir: &tempfile::TempDir) -> PathBuf {
        let path = dir.path().join("graft-service-token");
        std::fs::write(&path, format!("{SERVICE}\n")).unwrap();
        path
    }

    fn env_value<'a>(env: &'a [(String, String)], key: &str) -> Option<&'a str> {
        env.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    async fn eventually(mut done: impl FnMut() -> bool) {
        for _ in 0..200 {
            if done() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("condition not met in time");
    }

    #[tokio::test]
    async fn a_session_is_minted_a_token_and_its_workspace_is_renewed_then_closed() {
        let fake = Fake::new(12 * 3600);
        let url = serve(fake.clone()).await;
        let dir = tempfile::tempdir().unwrap();
        let graft = Graft::new(
            config(url.clone(), token_file(&dir), Duration::from_millis(90)),
            broker(),
        );
        let id = Uuid::new_v4();

        let env = graft
            .begin(id, "ses_01M5JHGFHVVZ", Some("WI-2227"))
            .await
            .expect("graft env");
        assert_eq!(env_value(&env, TOKEN_ENV), Some("tok-0"));
        assert_eq!(env_value(&env, ENDPOINT_ENV), Some(url.as_str()));
        assert_eq!(env_value(&env, REPO_ENV), Some("rusttorrent"));
        assert_eq!(
            env_value(&env, WORKSPACE_ENV),
            Some("vogt/ses_01m5jhgfhvvz")
        );

        {
            let seen = fake.seen.lock().unwrap();
            let (bearer, mint) = &seen.mints[0];
            assert_eq!(bearer, SERVICE, "minted with the service token");
            assert_eq!(mint.subject, "agent:ses_01M5JHGFHVVZ");
            assert_eq!(mint.session.as_deref(), Some("ses_01M5JHGFHVVZ"));
            assert_eq!(mint.work_item.as_deref(), Some("WI-2227"));
            assert_eq!(
                mint.scopes,
                vec!["repo:rusttorrent:read", "repo:rusttorrent:write"]
            );
            assert_eq!(mint.ttl_s, 12 * 3600);
            let (bearer, open) = &seen.opens[0];
            assert_eq!(bearer, "tok-0", "opened with the session's own token");
            assert_eq!(open.repo, "rusttorrent");
            assert_eq!(open.line, "main");
            assert_eq!(open.name.as_deref(), Some("vogt/ses_01m5jhgfhvvz"));
            assert_eq!(open.lease_ms, 90);
        }

        // Renewed every third of the lease, with the session's token.
        eventually(|| fake.seen.lock().unwrap().renews.len() >= 2).await;
        {
            let seen = fake.seen.lock().unwrap();
            for (bearer, renew) in &seen.renews {
                assert_eq!(bearer, "tok-0");
                assert_eq!(
                    renew.workspace.as_ref().unwrap().name,
                    "vogt/ses_01m5jhgfhvvz"
                );
            }
        }

        graft.finish(id).await;
        assert!(!graft.holds(id));
        let renews_at_close = {
            let seen = fake.seen.lock().unwrap();
            assert_eq!(seen.closes.len(), 1);
            let (bearer, close) = &seen.closes[0];
            assert_eq!(bearer, "tok-0");
            assert!(!close.keep_drafts);
            seen.renews.len()
        };
        // Renewal stops with the session.
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(fake.seen.lock().unwrap().renews.len(), renews_at_close);
    }

    #[tokio::test]
    async fn a_ttl_over_the_service_tokens_limit_is_minted_at_the_limit() {
        let fake = Fake::new(3600);
        let url = serve(fake.clone()).await;
        let dir = tempfile::tempdir().unwrap();
        let graft = Graft::new(
            config(url, token_file(&dir), Duration::from_secs(600)),
            broker(),
        );
        let env = graft.begin(Uuid::new_v4(), "ses_a", None).await;
        assert!(env.is_some());
        let seen = fake.seen.lock().unwrap();
        let ttls: Vec<u64> = seen.mints.iter().map(|(_, m)| m.ttl_s).collect();
        assert_eq!(ttls, vec![12 * 3600, 3600]);
        assert_eq!(seen.mints[1].1.work_item, None);
    }

    #[tokio::test]
    async fn a_lapsed_workspace_is_opened_again_by_the_renewer() {
        let fake = Fake::new(12 * 3600);
        let url = serve(fake.clone()).await;
        let dir = tempfile::tempdir().unwrap();
        let graft = Graft::new(
            config(url, token_file(&dir), Duration::from_millis(90)),
            broker(),
        );
        let id = Uuid::new_v4();
        graft.begin(id, "ses_b", None).await.unwrap();
        fake.lapsed.store(true, std::sync::atomic::Ordering::SeqCst);
        eventually(|| fake.seen.lock().unwrap().opens.len() >= 2).await;
        graft.finish(id).await;
    }

    #[tokio::test]
    async fn an_unreachable_or_refusing_graft_starts_the_session_without_it() {
        let dir = tempfile::tempdir().unwrap();
        // Nothing listens here.
        let unreachable = Graft::new(
            config(
                "http://127.0.0.1:9".into(),
                token_file(&dir),
                Duration::from_secs(600),
            ),
            broker(),
        );
        assert!(unreachable
            .begin(Uuid::new_v4(), "ses_c", None)
            .await
            .is_none());

        // A service token graft does not accept.
        let fake = Fake::new(12 * 3600);
        let url = serve(fake.clone()).await;
        let wrong = dir.path().join("wrong");
        std::fs::write(&wrong, "not-the-service-token").unwrap();
        let refused = Graft::new(config(url, wrong, Duration::from_secs(600)), broker());
        let id = Uuid::new_v4();
        assert!(refused.begin(id, "ses_c", None).await.is_none());
        assert!(!refused.holds(id));
        // Finishing a session graft never held is a no-op.
        refused.finish(id).await;
        assert!(fake.seen.lock().unwrap().closes.is_empty());
    }

    #[tokio::test]
    async fn a_missing_service_token_file_is_an_error_that_names_the_file_not_a_value() {
        let graft = Graft::new(
            config(
                "http://127.0.0.1:9".into(),
                PathBuf::from("/nonexistent/graft-token"),
                Duration::from_secs(600),
            ),
            broker(),
        );
        let err = graft.mint("ses_d", None).await.unwrap_err().to_string();
        assert!(err.contains("/nonexistent/graft-token"), "{err}");
    }

    #[test]
    fn a_bearer_never_shows_in_debug() {
        let token = SessionToken {
            value: Secret("tok-secret-value".into()),
            expires: Instant::now(),
        };
        assert!(!format!("{token:?}").contains("tok-secret-value"));
    }

    #[test]
    fn workspace_names_are_ones_graft_accepts() {
        assert_eq!(
            Graft::workspace_name("ses_01M5JHGFHVVZRCS33EAP97NK9F"),
            "vogt/ses_01m5jhgfhvvzrcs33eap97nk9f"
        );
        assert_eq!(
            Graft::workspace_name("4f1c7a52-9b0e-4d6a-8a43-0f5e2b7c9d11"),
            "vogt/4f1c7a52-9b0e-4d6a-8a43-0f5e2b7c9d11"
        );
        assert_eq!(Graft::workspace_name("_x y/z"), "vogt/s_x-y-z");
    }

    fn lookup(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: std::collections::HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k| map.get(k).cloned()
    }

    #[test]
    fn graft_is_off_unless_enabled() {
        assert_eq!(GraftConfig::from_lookup(lookup(&[])).unwrap(), None);
        assert_eq!(
            GraftConfig::from_lookup(lookup(&[
                ("ENGINE_GRAFT_ENABLED", "0"),
                ("ENGINE_GRAFT_URL", "http://graft:7420"),
            ]))
            .unwrap(),
            None
        );
        assert!(GraftConfig::from_lookup(lookup(&[("ENGINE_GRAFT_ENABLED", "maybe")])).is_err());
    }

    #[test]
    fn an_enabled_graft_takes_defaults_and_requires_what_it_cannot_guess() {
        let cfg = GraftConfig::from_lookup(lookup(&[
            ("ENGINE_GRAFT_ENABLED", "1"),
            ("ENGINE_GRAFT_URL", "http://graft:7420"),
            ("ENGINE_GRAFT_REPO", "rusttorrent"),
            (
                "ENGINE_GRAFT_SERVICE_SECRET",
                "proj-123 VOGT_GRAFT_SERVICE_TOKEN",
            ),
        ]))
        .unwrap()
        .unwrap();
        assert_eq!(cfg.line, "main");
        assert_eq!(
            cfg.scopes,
            vec!["repo:rusttorrent:read", "repo:rusttorrent:write"]
        );
        assert_eq!(cfg.token_ttl, Duration::from_secs(12 * 3600));
        assert_eq!(cfg.lease, Duration::from_secs(1800));
        assert!(!cfg.keep_drafts);
        assert_eq!(
            cfg.service_token,
            ServiceToken::Secret {
                project_id: "proj-123".into(),
                secret_name: "VOGT_GRAFT_SERVICE_TOKEN".into()
            }
        );

        let cfg = GraftConfig::from_lookup(lookup(&[
            ("ENGINE_GRAFT_ENABLED", "on"),
            ("ENGINE_GRAFT_URL", "http://graft:7420"),
            ("ENGINE_GRAFT_REPO", "r"),
            ("ENGINE_GRAFT_LINE", "release/1.4"),
            ("ENGINE_GRAFT_SCOPES", "repo:r:read, repo:r:write:src/"),
            ("ENGINE_GRAFT_TOKEN_TTL", "0"),
            ("ENGINE_GRAFT_LEASE", "10m"),
            ("ENGINE_GRAFT_KEEP_DRAFTS", "yes"),
            ("ENGINE_GRAFT_SERVICE_TOKEN_FILE", "/run/secrets/graft"),
        ]))
        .unwrap()
        .unwrap();
        assert_eq!(cfg.line, "release/1.4");
        assert_eq!(cfg.scopes, vec!["repo:r:read", "repo:r:write:src/"]);
        assert_eq!(cfg.token_ttl, Duration::ZERO);
        assert_eq!(cfg.lease, Duration::from_secs(600));
        assert!(cfg.keep_drafts);
        assert_eq!(
            cfg.service_token,
            ServiceToken::File("/run/secrets/graft".into())
        );

        let base = [
            ("ENGINE_GRAFT_ENABLED", "1"),
            ("ENGINE_GRAFT_URL", "http://graft:7420"),
            ("ENGINE_GRAFT_REPO", "r"),
            ("ENGINE_GRAFT_SERVICE_TOKEN_FILE", "/run/secrets/graft"),
        ];
        for missing in [
            "ENGINE_GRAFT_URL",
            "ENGINE_GRAFT_REPO",
            "ENGINE_GRAFT_SERVICE_TOKEN_FILE",
        ] {
            let pairs: Vec<_> = base
                .iter()
                .copied()
                .filter(|(k, _)| *k != missing)
                .collect();
            let err = GraftConfig::from_lookup(lookup(&pairs)).unwrap_err();
            assert!(err.contains(missing) || err.contains("SERVICE"), "{err}");
        }
        for (key, bad) in [
            ("ENGINE_GRAFT_URL", "graft:7420"),
            ("ENGINE_GRAFT_LEASE", "5s"),
            ("ENGINE_GRAFT_TOKEN_TTL", "soon"),
            ("ENGINE_GRAFT_SERVICE_SECRET", "only-one-field"),
        ] {
            let mut pairs: Vec<_> = base.iter().copied().filter(|(k, _)| *k != key).collect();
            if key == "ENGINE_GRAFT_SERVICE_SECRET" {
                pairs.retain(|(k, _)| *k != "ENGINE_GRAFT_SERVICE_TOKEN_FILE");
            }
            pairs.push((key, bad));
            assert!(
                GraftConfig::from_lookup(lookup(&pairs)).is_err(),
                "{key}={bad} should be refused"
            );
        }
        // Both sources at once is ambiguous.
        let mut both = base.to_vec();
        both.push(("ENGINE_GRAFT_SERVICE_SECRET", "p n"));
        assert!(GraftConfig::from_lookup(lookup(&both)).is_err());
    }
}
