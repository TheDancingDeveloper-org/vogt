//! On-demand secret brokering for sessions: a manifest-constrained late fetch
//! (#568, direction 2 of #566).
//!
//! #511 strips the secrets-manager machine identity from every session, and
//! #566 signposted that strip. What neither restored is a way for a session to
//! obtain a secret it legitimately needs *after* launch. This module is that
//! path, shaped so the #511 property survives intact:
//!
//! - **The identity never leaves the engine.** A session asks the engine; the
//!   engine — which still holds the identity — runs the configured agent-auth
//!   helper's `get VAR` subcommand and hands back one value.
//! - **Policy is the deploy-time manifest, enforced here.** Only a `VAR` named
//!   in `ENGINE_AGENT_AUTH_SECRETS` can be asked for; anything else is refused
//!   with a message naming the manifest line to add. The helper re-checks the
//!   manifest too, but the engine's check is the one that counts: the helper
//!   is pluggable and this guarantee must not depend on it.
//! - **Every fetch is audited**, naming the session and the variable and
//!   never the value.
//! - **A session proves it is a session** with a per-session broker token the
//!   engine mints at spawn and revokes when the session record is forgotten.
//!   It is not the engine bearer (`ENGINE_TOKEN`, which #511 withholds from
//!   sessions on purpose) and the engine bearer does not work here either:
//!   the broker is for sessions, and a token that can create sessions has no
//!   business also reading their secrets.
//!
//! Why this is worth having even though every manifest secret is already
//! exported into the session at launch: the `ondemand` manifest flag. An
//! `ondemand` entry is *declared for* sessions but deliberately not resolved
//! at launch — it does not sit in the session's environment for the whole
//! session, readable by `env`, a prompt-injected `printenv`, or a crash dump.
//! It exists in the session only at the moment it is asked for, and that
//! moment is in the audit log. That is the narrowing this buys.
//!
//! The write mirror (#598) is `store`: `POST /api/agent-auth/store/{var}`, for
//! a session that has just *produced* a secret (minted a token, generated a
//! keypair, rotated a password it was asked to rotate) and would otherwise have
//! to hand the value back through a transcript or reach around the strip. It
//! keeps every property above — the identity stays in the engine (the helper's
//! `set VAR` runs engine-side), policy is the deploy-time manifest (an entry
//! must be flagged `writable`), the value travels on stdin and is never logged
//! or put in argv/env, and every write is audited by name. `writable` widens
//! blast radius only to the entries an operator pre-authorised.
//!
//! Honest caveat, unchanged from #566: the engine and its sessions run as the
//! same uid today, so `/proc/1/environ` still exposes the identity to any
//! session that goes looking. Until sessions run as a separate uid this is a
//! reduction in ambient exposure with an audit trail, not an enforced
//! boundary. The design does not change when that lands — the boundary just
//! becomes real.

use std::sync::Arc;
use std::time::Duration;

use axum::{
    body::Body,
    extract::{Path, State},
    http::{header, Method, Request, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use dashmap::DashMap;
use serde_json::json;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use uuid::Uuid;

use crate::app::AppState;
use crate::auth;
use std::net::SocketAddr;
use std::path::PathBuf;

use crate::config::Config;
use crate::error::ApiError;
use crate::observability::RequestId;
use crate::pty;

/// The per-session broker token, injected into every session's environment
/// when brokering is configured. Named with `TOKEN` on purpose: were it ever
/// to reach the engine's own environment, `pty::is_secret_env` would strip it
/// from children rather than let it leak sideways.
pub const BROKER_TOKEN_ENV: &str = "MYDEVENV2_BROKER_TOKEN";
/// Where a session finds the broker: the engine on loopback.
pub const BROKER_URL_ENV: &str = "MYDEVENV2_BROKER_URL";
/// The read route. `POST` only, outside the engine bearer gate (see `app.rs`),
/// authenticated with the session's own broker token.
pub const FETCH_ROUTE: &str = "/api/agent-auth/fetch/{var}";
/// The write route (#598): a session stores one manifest secret it just
/// produced. Same auth and gate placement as the read route.
pub const STORE_ROUTE: &str = "/api/agent-auth/store/{var}";

/// Per-session ceiling on fetches. A secret is asked for a handful of times in
/// a session's life; a loop asking sixty times a minute is a bug or an attack,
/// and either way the audit log should show it being refused.
const FETCH_LIMIT_PER_MINUTE: u32 = 60;
/// Per-session ceiling on stores, tighter than fetches: a session mints or
/// rotates a secret rarely, so ten writes a minute is already generous and a
/// runaway is worth refusing loudly (#598).
const STORE_LIMIT_PER_MINUTE: u32 = 10;
/// Largest value a session may store: enough for a multi-line key, not a file
/// dump. A body above this is refused with 413 before the helper is run (#598).
const MAX_STORE_BYTES: usize = 64 * 1024;
/// How much the store route will actually read before giving up. A body between
/// the cap and this is still drained so the refusal is clean (the client is not
/// left blocked writing to a socket the server stopped reading); a body past
/// this is an abuse and errors out of `to_bytes` unbuffered.
const STORE_DRAIN_BYTES: usize = 2 * MAX_STORE_BYTES;
/// A universal-auth login plus one read against a healthy secrets manager
/// takes a few seconds; a helper still running after this is stuck.
const HELPER_TIMEOUT: Duration = Duration::from_secs(30);
/// How much of the helper's stderr a refusal repeats. Enough to name the
/// secret and the cause, not enough to carry a value by accident.
const STDERR_EXCERPT: usize = 300;

/// What a manifest line says about when its secret is resolved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManifestPolicy {
    /// Fetched at launch; missing or empty fails the launch. The default.
    Required,
    /// Fetched at launch; missing or empty is tolerated.
    Optional,
    /// Declared for sessions but not fetched at launch: available only
    /// through the broker, on request, audited.
    OnDemand,
}

/// One line of `ENGINE_AGENT_AUTH_SECRETS`:
/// `VAR PROJECT_ID SECRET_NAME [flags]`, where `flags` is a comma-separated
/// list drawn from {optional, ondemand, writable}. `writable` is orthogonal to
/// the read policy: it says a session may *store* this entry through the broker
/// (#598).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManifestSecret {
    pub var: String,
    pub project_id: String,
    pub secret_name: String,
    pub policy: ManifestPolicy,
    /// A session may write this entry through `POST /api/agent-auth/store/{var}`
    /// (#598). Widens blast radius only to the declared entries and every write
    /// is audited.
    pub writable: bool,
}

/// Parse the flag field of a manifest line: a comma-separated list drawn from
/// {optional, ondemand, writable} — plus `required`, the explicit spelling of
/// the default — in any order. A legacy single token (`optional`, `ondemand`,
/// or a bare line) is simply the one- or zero-element case, so old manifests
/// parse identically. An unknown flag is an error, never a silent "required"
/// (#598); two conflicting policy flags are an error too.
fn parse_flags(field: &str, line: &str) -> Result<(ManifestPolicy, bool), String> {
    let mut policy: Option<ManifestPolicy> = None;
    let mut writable = false;
    if field.is_empty() {
        return Ok((ManifestPolicy::Required, false));
    }
    for flag in field.split(',') {
        let named = match flag {
            "required" => ManifestPolicy::Required,
            "optional" => ManifestPolicy::Optional,
            "ondemand" => ManifestPolicy::OnDemand,
            "writable" => {
                writable = true;
                continue;
            }
            other => {
                return Err(format!(
                    "ENGINE_AGENT_AUTH_SECRETS entry has unknown flag {other:?} (flags are a comma-separated list of optional, ondemand, writable): {line}"
                ));
            }
        };
        if policy.is_some_and(|existing| existing != named) {
            return Err(format!(
                "ENGINE_AGENT_AUTH_SECRETS entry sets conflicting policy flags: {line}"
            ));
        }
        policy = Some(named);
    }
    Ok((policy.unwrap_or(ManifestPolicy::Required), writable))
}

/// Parse the manifest with the grammar `engine/deploy/agent-auth.sh` uses —
/// one entry per line, `#` starts a comment, whitespace-separated, a fourth
/// field of comma-separated flags — so the engine and the helper cannot
/// disagree about what a deployment declared.
pub fn parse_manifest(text: &str) -> Result<Vec<ManifestSecret>, String> {
    let mut entries = Vec::new();
    for raw in text.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let mut fields = line.split_whitespace();
        let (Some(var), Some(project_id), Some(secret_name)) =
            (fields.next(), fields.next(), fields.next())
        else {
            return Err(format!(
                "malformed ENGINE_AGENT_AUTH_SECRETS entry (want 'VAR PROJECT_ID SECRET_NAME [flags]'): {line}"
            ));
        };
        if !is_env_name(var) {
            return Err(format!(
                "ENGINE_AGENT_AUTH_SECRETS entry names {var:?}, which is not a valid environment variable name"
            ));
        }
        let (policy, writable) = parse_flags(fields.next().unwrap_or(""), line)?;
        entries.push(ManifestSecret {
            var: var.to_string(),
            project_id: project_id.to_string(),
            secret_name: secret_name.to_string(),
            policy,
            writable,
        });
    }
    Ok(entries)
}

fn is_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c == '_' || c.is_ascii_alphabetic())
        && chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

/// The credential and address a spawned session is handed so it can reach
/// the broker. Built by [`SecretBroker::grant`], consumed by `pty::spawn`.
#[derive(Clone, Debug)]
pub struct BrokerGrant {
    pub token: String,
    pub url: String,
}

/// Why a fetch did not produce a value. None of these carry a value.
#[derive(Debug)]
pub enum FetchFailure {
    /// The helper could not be started at all.
    Spawn(std::io::Error),
    /// The helper ran past [`HELPER_TIMEOUT`].
    Timeout,
    /// The helper exited non-zero; `stderr` is an excerpt of what it said.
    Failed { status: String, stderr: String },
    /// The helper exited zero but printed nothing, which the reference helper
    /// never does: it fails rather than hand back an empty secret.
    Empty,
}

impl std::fmt::Display for FetchFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FetchFailure::Spawn(e) => write!(f, "agent-auth helper could not be started: {e}"),
            FetchFailure::Timeout => write!(
                f,
                "agent-auth helper did not answer within {}s",
                HELPER_TIMEOUT.as_secs()
            ),
            FetchFailure::Failed { status, stderr } if stderr.is_empty() => {
                write!(f, "agent-auth helper failed ({status})")
            }
            FetchFailure::Failed { status, stderr } => {
                write!(f, "agent-auth helper failed ({status}): {stderr}")
            }
            FetchFailure::Empty => write!(f, "agent-auth helper returned an empty value"),
        }
    }
}

/// What a `store` did, as the helper reported it on stdout (#598). Audited;
/// never carries a value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoreOutcome {
    /// The secret did not exist and was created.
    Created,
    /// The secret existed and was overwritten.
    Updated,
    /// The helper stored the value but did not say which of the two it was.
    Stored,
}

impl StoreOutcome {
    fn from_helper_stdout(stdout: &[u8]) -> Self {
        match String::from_utf8_lossy(stdout).trim() {
            "created" => StoreOutcome::Created,
            "updated" => StoreOutcome::Updated,
            _ => StoreOutcome::Stored,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            StoreOutcome::Created => "created",
            StoreOutcome::Updated => "updated",
            StoreOutcome::Stored => "stored",
        }
    }
}

pub struct SecretBroker {
    bind: SocketAddr,
    helper: PathBuf,
    manifest: Vec<ManifestSecret>,
    /// Session id → SHA-256 of that session's broker token. The plaintext is
    /// handed to the child once and never stored.
    grants: DashMap<Uuid, [u8; 32]>,
}

impl SecretBroker {
    pub fn new(cfg: &Config) -> Self {
        Self::with(
            cfg.bind,
            cfg.agent_auth_helper.clone(),
            cfg.agent_auth_secrets.clone(),
        )
    }

    /// The three things the broker needs, and nothing else of the config.
    pub fn with(bind: SocketAddr, helper: PathBuf, manifest: Vec<ManifestSecret>) -> Self {
        Self {
            bind,
            helper,
            manifest,
            grants: DashMap::new(),
        }
    }

    /// Brokering exists only where a deployment declared something to broker.
    /// With an empty manifest the route answers 503 with that reason and no
    /// session is handed a token (NFR-O5: absence is a reported state).
    pub fn enabled(&self) -> bool {
        !self.manifest.is_empty()
    }

    /// Mint a fresh token for `session`, replacing any earlier one, and return
    /// the plaintext for the child's environment. Two v4 UUIDs' worth of
    /// randomness (244 bits), from the same source every id here comes from.
    pub fn issue(&self, session: Uuid) -> String {
        let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        self.grants.insert(session, digest(&token));
        token
    }

    /// The grant for a session about to be spawned, or `None` when there is
    /// nothing to broker — in which case the session's environment says
    /// nothing about a broker, rather than pointing at one that refuses.
    pub fn grant(&self, session: Uuid) -> Option<BrokerGrant> {
        if !self.enabled() {
            return None;
        }
        Some(BrokerGrant {
            token: self.issue(session),
            url: self.loopback_url(),
        })
    }

    /// Forget a session's token. A token a session was handed stays valid for
    /// exactly as long as the engine remembers the session.
    pub fn revoke(&self, session: Uuid) {
        self.grants.remove(&session);
    }

    /// Which live session presented this token, if any. Constant-time per
    /// entry, as the engine bearer check is.
    pub fn authenticate(&self, presented: &str) -> Option<Uuid> {
        let candidate = digest(presented);
        self.grants
            .iter()
            .find(|entry| bool::from(entry.value().ct_eq(&candidate)))
            .map(|entry| *entry.key())
    }

    /// The manifest entry for `var`, if the deployment declared one.
    pub fn permitted(&self, var: &str) -> Option<ManifestSecret> {
        self.manifest.iter().find(|entry| entry.var == var).cloned()
    }

    /// The manifest entry for `var`, but only when the deployment flagged it
    /// `writable` — the membership *and* flag test a store must pass (#598). A
    /// name that is declared but not writable returns `None` here; the handler
    /// tells the two apart with `permitted` to give a distinct refusal.
    pub fn permitted_write(&self, var: &str) -> Option<ManifestSecret> {
        self.manifest
            .iter()
            .find(|entry| entry.var == var && entry.writable)
            .cloned()
    }

    /// Where a session reaches this engine: loopback, on whatever port the
    /// engine binds. `0.0.0.0`/`::` bind to every interface, loopback
    /// included; a specific address is used as-is.
    pub fn loopback_url(&self) -> String {
        let bind = self.bind;
        let host = if bind.ip().is_unspecified() {
            if bind.is_ipv6() {
                "[::1]".to_string()
            } else {
                "127.0.0.1".to_string()
            }
        } else if bind.is_ipv6() {
            format!("[{}]", bind.ip())
        } else {
            bind.ip().to_string()
        };
        format!("http://{host}:{}", bind.port())
    }

    /// Resolve one manifest entry by running the configured helper's
    /// `get VAR` with exactly the environment the helper is spawned with for
    /// a session (`sanitized_child_env` plus the re-granted identity), and
    /// nothing else. The value is the helper's stdout, verbatim.
    pub async fn fetch(&self, entry: &ManifestSecret) -> Result<String, FetchFailure> {
        let mut cmd = tokio::process::Command::new(&self.helper);
        cmd.arg("get")
            .arg(&entry.var)
            .env_clear()
            .envs(pty::sanitized_child_env())
            .envs(pty::agent_auth_helper_env())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        let output = match tokio::time::timeout(HELPER_TIMEOUT, cmd.output()).await {
            Ok(Ok(output)) => output,
            Ok(Err(e)) => return Err(FetchFailure::Spawn(e)),
            Err(_) => return Err(FetchFailure::Timeout),
        };
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stderr: String = stderr.trim().chars().take(STDERR_EXCERPT).collect();
            return Err(FetchFailure::Failed {
                status: output.status.to_string(),
                stderr,
            });
        }
        let value = String::from_utf8_lossy(&output.stdout).into_owned();
        if value.is_empty() {
            return Err(FetchFailure::Empty);
        }
        Ok(value)
    }

    /// Store one manifest entry by running the configured helper's `set VAR`
    /// with the same re-granted environment `fetch` uses. The value is fed on
    /// **stdin**, never argv or env, so it cannot appear in `ps`, shell history
    /// or an audit excerpt. The helper prints `created`/`updated` (or nothing);
    /// that word — never the value — is all that comes back (#598).
    pub async fn store(
        &self,
        entry: &ManifestSecret,
        value: &[u8],
    ) -> Result<StoreOutcome, FetchFailure> {
        use tokio::io::AsyncWriteExt;
        let mut cmd = tokio::process::Command::new(&self.helper);
        cmd.arg("set")
            .arg(&entry.var)
            .env_clear()
            .envs(pty::sanitized_child_env())
            .envs(pty::agent_auth_helper_env())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(e) => return Err(FetchFailure::Spawn(e)),
        };
        // Feed stdin from a task and close it so the helper's read sees EOF.
        // Writing concurrently with waiting means a value that fills the pipe
        // cannot deadlock a helper that has not started draining stdout yet.
        let stdin = child.stdin.take();
        let payload = value.to_vec();
        let writer = tokio::spawn(async move {
            if let Some(mut stdin) = stdin {
                let _ = stdin.write_all(&payload).await;
                let _ = stdin.shutdown().await;
            }
        });
        let output = match tokio::time::timeout(HELPER_TIMEOUT, child.wait_with_output()).await {
            Ok(Ok(output)) => output,
            Ok(Err(e)) => {
                writer.abort();
                return Err(FetchFailure::Spawn(e));
            }
            Err(_) => {
                writer.abort();
                return Err(FetchFailure::Timeout);
            }
        };
        let _ = writer.await;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stderr: String = stderr.trim().chars().take(STDERR_EXCERPT).collect();
            return Err(FetchFailure::Failed {
                status: output.status.to_string(),
                stderr,
            });
        }
        Ok(StoreOutcome::from_helper_stdout(&output.stdout))
    }
}

fn digest(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

/// `POST /api/agent-auth/fetch/{var}` — hand a session one manifest secret.
///
/// Registered outside `auth::require_bearer` on purpose (the session has no
/// engine bearer to present, #511) and so does every check itself, in the
/// order that leaks least: the token first, so an unauthenticated caller
/// learns nothing about the manifest; the rate limit next; then whether the
/// name is declared; and only then the fetch.
pub async fn fetch(
    State(state): State<Arc<AppState>>,
    Path(var): Path<String>,
    request: Request<Body>,
) -> Response {
    let broker = state.sessions.secret_broker();
    let path = request.uri().path().to_string();
    let request_id = request
        .extensions()
        .get::<RequestId>()
        .map(|id| id.0.clone())
        .unwrap_or_default();

    if !broker.enabled() {
        return refusal(
            StatusCode::SERVICE_UNAVAILABLE,
            "on-demand secret brokering is not configured on this engine (ENGINE_AGENT_AUTH_SECRETS declares nothing)",
        );
    }

    let presented = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|v| !v.is_empty());
    let Some(presented) = presented else {
        auth::record_auth_failure(&Method::POST, &path, &request_id, "missing broker token").await;
        return ApiError::Unauthorized.into_response();
    };
    // A token is valid for as long as the engine remembers its session, and
    // the registry is asked as well as the grant table so the two cannot
    // drift: a session the registry has forgotten is refused even if a
    // revoke was somehow missed.
    let session = match broker
        .authenticate(presented)
        .filter(|id| state.sessions.get(*id).is_ok())
    {
        Some(id) => id,
        None => {
            auth::record_auth_failure(&Method::POST, &path, &request_id, "unknown broker token")
                .await;
            return ApiError::Unauthorized.into_response();
        }
    };

    if let Some(retry_after) = state
        .auth
        .check_mutation_rate_limit(&format!("secret-broker:{session}"), FETCH_LIMIT_PER_MINUTE)
    {
        tracing::warn!(
            target: "mydevenv2::audit",
            request_id = %request_id,
            session_id = %session,
            var = %var,
            retry_after_ms = retry_after.as_millis() as u64,
            "secret broker rate limit exceeded"
        );
        return (
            StatusCode::TOO_MANY_REQUESTS,
            [(
                header::RETRY_AFTER,
                retry_after.as_secs().max(1).to_string(),
            )],
            Json(json!({ "error": "too many secret fetches from this session" })),
        )
            .into_response();
    }

    let Some(entry) = broker.permitted(&var) else {
        tracing::warn!(
            target: "mydevenv2::audit",
            request_id = %request_id,
            session_id = %session,
            var = %var,
            "secret broker refused: not in the manifest"
        );
        return refusal(
            StatusCode::FORBIDDEN,
            &format!(
                "{var} is not in this deployment's ENGINE_AGENT_AUTH_SECRETS manifest; to make it available to sessions add a line '{var} PROJECT_ID SECRET_NAME ondemand' to the stack and restart the engine"
            ),
        );
    };

    match broker.fetch(&entry).await {
        Ok(value) => {
            // The whole audit trail of the fetch: who, what, never the value.
            tracing::info!(
                target: "mydevenv2::audit",
                request_id = %request_id,
                session_id = %session,
                var = %entry.var,
                secret_name = %entry.secret_name,
                project_id = %entry.project_id,
                policy = ?entry.policy,
                "secret brokered to session"
            );
            (
                StatusCode::OK,
                [
                    (header::CONTENT_TYPE, "text/plain; charset=utf-8"),
                    (header::CACHE_CONTROL, "no-store"),
                ],
                value,
            )
                .into_response()
        }
        Err(failure) => {
            tracing::warn!(
                target: "mydevenv2::audit",
                request_id = %request_id,
                session_id = %session,
                var = %entry.var,
                secret_name = %entry.secret_name,
                error = %failure,
                "secret broker fetch failed"
            );
            ApiError::BadGateway(failure.to_string()).into_response()
        }
    }
}

/// `POST /api/agent-auth/store/{var}` — take one secret a session produced and
/// store it (#598), the write mirror of [`fetch`].
///
/// Same gate placement and auth as the read route, and the same "leaks least"
/// check order: the token first, the rate limit (tighter than fetch) next, then
/// whether the name is declared *and* `writable`, then the body, and only then
/// the helper. The value arrives as the request body and is passed to the
/// helper on stdin — it is never logged, put in an env var, or placed on a
/// command line.
pub async fn store(
    State(state): State<Arc<AppState>>,
    Path(var): Path<String>,
    request: Request<Body>,
) -> Response {
    let broker = state.sessions.secret_broker();
    let path = request.uri().path().to_string();
    let request_id = request
        .extensions()
        .get::<RequestId>()
        .map(|id| id.0.clone())
        .unwrap_or_default();

    if !broker.enabled() {
        return refusal(
            StatusCode::SERVICE_UNAVAILABLE,
            "on-demand secret brokering is not configured on this engine (ENGINE_AGENT_AUTH_SECRETS declares nothing)",
        );
    }

    let presented = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|v| !v.is_empty());
    let Some(presented) = presented else {
        auth::record_auth_failure(&Method::POST, &path, &request_id, "missing broker token").await;
        return ApiError::Unauthorized.into_response();
    };
    let session = match broker
        .authenticate(presented)
        .filter(|id| state.sessions.get(*id).is_ok())
    {
        Some(id) => id,
        None => {
            auth::record_auth_failure(&Method::POST, &path, &request_id, "unknown broker token")
                .await;
            return ApiError::Unauthorized.into_response();
        }
    };

    if let Some(retry_after) = state.auth.check_mutation_rate_limit(
        &format!("secret-broker-store:{session}"),
        STORE_LIMIT_PER_MINUTE,
    ) {
        tracing::warn!(
            target: "mydevenv2::audit",
            request_id = %request_id,
            session_id = %session,
            var = %var,
            retry_after_ms = retry_after.as_millis() as u64,
            "secret broker store rate limit exceeded"
        );
        return (
            StatusCode::TOO_MANY_REQUESTS,
            [(
                header::RETRY_AFTER,
                retry_after.as_secs().max(1).to_string(),
            )],
            Json(json!({ "error": "too many secret stores from this session" })),
        )
            .into_response();
    }

    // Declared and writable are separate refusals so an operator sees which
    // manifest edit is needed: add the line, or add the `writable` flag.
    let entry = match broker.permitted(&var) {
        None => {
            tracing::warn!(
                target: "mydevenv2::audit",
                request_id = %request_id,
                session_id = %session,
                var = %var,
                "secret broker store refused: not in the manifest"
            );
            return refusal(
                StatusCode::FORBIDDEN,
                &format!(
                    "{var} is not in this deployment's ENGINE_AGENT_AUTH_SECRETS manifest; to let sessions store it add a line '{var} PROJECT_ID SECRET_NAME ondemand,writable' to the stack and restart the engine"
                ),
            );
        }
        Some(entry) if !entry.writable => {
            tracing::warn!(
                target: "mydevenv2::audit",
                request_id = %request_id,
                session_id = %session,
                var = %var,
                "secret broker store refused: declared but not writable"
            );
            return refusal(
                StatusCode::FORBIDDEN,
                &format!(
                    "{var} is declared in ENGINE_AGENT_AUTH_SECRETS but not writable; add the `writable` flag to its line (e.g. 'ondemand,writable') and restart the engine"
                ),
            );
        }
        Some(entry) => entry,
    };

    // Read with a drain margin above the cap, then reject by length. A merely
    // over-cap body (up to `STORE_DRAIN_BYTES`) is fully read — so the client
    // finishes sending and gets a clean 413 rather than deadlocking mid-write on
    // a socket the server stopped reading — and `to_bytes` still errors out an
    // abusively huge body before it is buffered.
    let bytes = match axum::body::to_bytes(request.into_body(), STORE_DRAIN_BYTES).await {
        Ok(bytes) if bytes.len() > MAX_STORE_BYTES => {
            tracing::warn!(
                target: "mydevenv2::audit",
                request_id = %request_id,
                session_id = %session,
                var = %entry.var,
                "secret broker store refused: value over the size cap"
            );
            return refusal(
                StatusCode::PAYLOAD_TOO_LARGE,
                &format!("stored value exceeds the {MAX_STORE_BYTES}-byte cap"),
            );
        }
        Ok(bytes) => bytes,
        Err(_) => {
            tracing::warn!(
                target: "mydevenv2::audit",
                request_id = %request_id,
                session_id = %session,
                var = %entry.var,
                "secret broker store refused: value over the size cap"
            );
            return refusal(
                StatusCode::PAYLOAD_TOO_LARGE,
                &format!("stored value exceeds the {MAX_STORE_BYTES}-byte cap"),
            );
        }
    };
    if bytes.is_empty() {
        return refusal(
            StatusCode::BAD_REQUEST,
            "stored value is empty; the broker stores exactly the request body and will not store nothing",
        );
    }

    match broker.store(&entry, &bytes).await {
        Ok(outcome) => {
            tracing::info!(
                target: "mydevenv2::audit",
                request_id = %request_id,
                session_id = %session,
                var = %entry.var,
                secret_name = %entry.secret_name,
                project_id = %entry.project_id,
                policy = ?entry.policy,
                bytes = bytes.len(),
                outcome = %outcome.as_str(),
                "secret stored from session"
            );
            (
                StatusCode::OK,
                [
                    (header::CONTENT_TYPE, "text/plain; charset=utf-8"),
                    (header::CACHE_CONTROL, "no-store"),
                ],
                outcome.as_str(),
            )
                .into_response()
        }
        Err(failure) => {
            tracing::warn!(
                target: "mydevenv2::audit",
                request_id = %request_id,
                session_id = %session,
                var = %entry.var,
                secret_name = %entry.secret_name,
                error = %failure,
                "secret broker store failed"
            );
            ApiError::BadGateway(failure.to_string()).into_response()
        }
    }
}

fn refusal(status: StatusCode, reason: &str) -> Response {
    (status, Json(json!({ "error": reason }))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_grammar_matches_the_helper() {
        let parsed = parse_manifest(
            "# a comment line\n\
             GH_TOKEN proj-a github-token\n\
             WIN_SSH_KEY proj-b 100.109.218.11_SSH optional # trailing comment\n\
             \n\
             DB_PASSWORD proj-c prod-db ondemand\n\
             LEGACY proj-d thing required\n",
        )
        .unwrap();
        assert_eq!(parsed.len(), 4);
        assert_eq!(parsed[0].var, "GH_TOKEN");
        assert_eq!(parsed[0].policy, ManifestPolicy::Required);
        assert_eq!(parsed[1].secret_name, "100.109.218.11_SSH");
        assert_eq!(parsed[1].policy, ManifestPolicy::Optional);
        assert_eq!(parsed[2].policy, ManifestPolicy::OnDemand);
        // Anything that is not `optional`/`ondemand` means required, as the
        // helper's `"${flag:-required}" != "optional"` has always meant.
        assert_eq!(parsed[3].policy, ManifestPolicy::Required);
    }

    #[test]
    fn writable_is_a_flag_orthogonal_to_the_read_policy() {
        let parsed = parse_manifest(
            "A p s\n\
             B p s ondemand,writable\n\
             C p s writable,optional\n\
             D p s writable\n\
             E p s optional\n",
        )
        .unwrap();
        // Legacy lines: writable defaults off.
        assert!(!parsed[0].writable);
        assert_eq!(parsed[0].policy, ManifestPolicy::Required);
        // Order-independent flag lists, both policy halves preserved.
        assert!(parsed[1].writable);
        assert_eq!(parsed[1].policy, ManifestPolicy::OnDemand);
        assert!(parsed[2].writable);
        assert_eq!(parsed[2].policy, ManifestPolicy::Optional);
        // `writable` alone is still Required at launch (a rotate-in-place entry).
        assert!(parsed[3].writable);
        assert_eq!(parsed[3].policy, ManifestPolicy::Required);
        assert!(!parsed[4].writable);
    }

    #[test]
    fn an_unknown_flag_is_an_error_not_a_silent_required() {
        let err = parse_manifest("X p s bogus\n").unwrap_err();
        assert!(err.contains("unknown flag"), "{err}");
        assert!(err.contains("bogus"), "names the flag: {err}");
        // A trailing comma leaves an empty flag, which is also rejected.
        assert!(parse_manifest("X p s ondemand,\n").is_err());
    }

    #[test]
    fn conflicting_policy_flags_are_an_error() {
        let err = parse_manifest("X p s optional,ondemand\n").unwrap_err();
        assert!(err.contains("conflicting policy flags"), "{err}");
    }

    #[test]
    fn permitted_write_needs_membership_and_the_writable_flag() {
        let broker = broker_with(
            "A p s ondemand\nB p s ondemand,writable\nC p s writable\n",
            "127.0.0.1:1",
        );
        // Declared but not writable: absent from the write policy, present in read.
        assert!(broker.permitted_write("A").is_none());
        assert!(broker.permitted("A").is_some());
        // Declared and writable.
        assert!(broker.permitted_write("B").is_some());
        assert!(broker.permitted_write("C").is_some());
        // Not declared at all.
        assert!(broker.permitted_write("D").is_none());
        assert!(broker.permitted_write("b").is_none(), "names are exact");
    }

    #[test]
    fn store_outcome_reads_only_the_helper_word() {
        assert_eq!(
            StoreOutcome::from_helper_stdout(b"created\n"),
            StoreOutcome::Created
        );
        assert_eq!(
            StoreOutcome::from_helper_stdout(b"  updated  "),
            StoreOutcome::Updated
        );
        assert_eq!(
            StoreOutcome::from_helper_stdout(b"anything else"),
            StoreOutcome::Stored
        );
        assert_eq!(StoreOutcome::Created.as_str(), "created");
    }

    #[test]
    fn a_short_line_is_a_named_error_not_a_silent_skip() {
        let err = parse_manifest("GH_TOKEN proj-a\n").unwrap_err();
        assert!(
            err.contains("malformed ENGINE_AGENT_AUTH_SECRETS entry"),
            "{err}"
        );
        assert!(err.contains("GH_TOKEN proj-a"), "names the line: {err}");
    }

    #[test]
    fn a_var_that_is_not_an_env_name_is_refused() {
        let err = parse_manifest("bad-name proj secret\n").unwrap_err();
        assert!(
            err.contains("not a valid environment variable name"),
            "{err}"
        );
    }

    #[test]
    fn empty_and_comment_only_manifests_declare_nothing() {
        assert!(parse_manifest("").unwrap().is_empty());
        assert!(parse_manifest("# nothing\n\n   \n").unwrap().is_empty());
    }

    fn broker_with(manifest: &str, bind: &str) -> SecretBroker {
        SecretBroker::with(
            bind.parse().unwrap(),
            PathBuf::from("/nonexistent/helper"),
            parse_manifest(manifest).unwrap(),
        )
    }

    #[test]
    fn a_token_identifies_its_session_and_only_while_it_lives() {
        let broker = broker_with("X p s ondemand\n", "127.0.0.1:8910");
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let token_a = broker.issue(a);
        let token_b = broker.issue(b);
        assert_ne!(token_a, token_b);
        assert_eq!(broker.authenticate(&token_a), Some(a));
        assert_eq!(broker.authenticate(&token_b), Some(b));
        assert_eq!(broker.authenticate("not-a-token"), None);
        assert_eq!(broker.authenticate(""), None);

        // Re-issuing replaces: the old plaintext stops working.
        let token_a2 = broker.issue(a);
        assert_eq!(broker.authenticate(&token_a), None);
        assert_eq!(broker.authenticate(&token_a2), Some(a));

        broker.revoke(a);
        assert_eq!(broker.authenticate(&token_a2), None);
        assert_eq!(broker.authenticate(&token_b), Some(b), "b is untouched");
    }

    #[test]
    fn a_grant_exists_only_where_there_is_something_to_broker() {
        let empty = broker_with("", "127.0.0.1:8910");
        assert!(!empty.enabled());
        assert!(empty.grant(Uuid::new_v4()).is_none());

        let declared = broker_with("X p s ondemand\n", "0.0.0.0:8910");
        assert!(declared.enabled());
        let grant = declared.grant(Uuid::new_v4()).unwrap();
        assert_eq!(grant.url, "http://127.0.0.1:8910");
        assert_eq!(grant.token.len(), 64, "two simple v4 uuids");
    }

    #[test]
    fn policy_is_manifest_membership() {
        let broker = broker_with("A p s\nB p s optional\nC p s ondemand\n", "127.0.0.1:1");
        assert!(broker.permitted("A").is_some());
        assert!(broker.permitted("B").is_some());
        assert_eq!(
            broker.permitted("C").unwrap().policy,
            ManifestPolicy::OnDemand
        );
        assert!(broker.permitted("D").is_none());
        assert!(broker.permitted("a").is_none(), "names are exact");
    }

    #[test]
    fn loopback_url_follows_the_bind() {
        assert_eq!(
            broker_with("", "0.0.0.0:8910").loopback_url(),
            "http://127.0.0.1:8910"
        );
        assert_eq!(
            broker_with("", "10.0.0.5:9000").loopback_url(),
            "http://10.0.0.5:9000"
        );
        assert_eq!(
            broker_with("", "[::]:8910").loopback_url(),
            "http://[::1]:8910"
        );
    }
}
