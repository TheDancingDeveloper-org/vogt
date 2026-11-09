//! On-demand secret brokering for sessions: a manifest-constrained late fetch.
//!
//! The secrets-manager machine identity is stripped from every session, and
//! that strip is signposted. What neither restored is a way for a session to
//! obtain a secret it legitimately needs *after* launch. This module is that
//! path, shaped so the strip's property survives intact:
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
//!   It is not the engine bearer (`ENGINE_TOKEN`, which is withheld from
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
//! The write mirror is `store`: `POST /api/agent-auth/store/{var}`, for
//! a session that has just *produced* a secret (minted a token, generated a
//! keypair, rotated a password it was asked to rotate) and would otherwise have
//! to hand the value back through a transcript or reach around the strip. It
//! keeps every property above — the identity stays in the engine (the helper's
//! `set VAR` runs engine-side), policy is the deploy-time manifest (an entry
//! must be flagged `writable`), the value travels on stdin and is never logged
//! or put in argv/env, and every write is audited by name. `writable` widens
//! blast radius only to the entries an operator pre-authorised.
//!
//! **Approved grants** (WI-973, `docs/design/oversight-grants.md`) are the
//! runtime counterpart to a manifest line: one secret, for one live session,
//! until a time, applied by vogt-core once a *person* approved it. Only the
//! stack secret may add or remove one (`POST`/`DELETE
//! /api/sessions/{id}/grants`). The fetch route checks the manifest first and
//! then the presenting session's own grants, so a grant never shadows a
//! manifest entry and never reaches another session. The helper is run with
//! `ENGINE_AGENT_AUTH_SECRETS` replaced by exactly the granted line, so its own
//! manifest check still holds. A `once` grant is spent by its first fetch,
//! whether or not the helper succeeds: it is taken out of the table before the
//! helper runs and never put back, so a revoke that lands mid-fetch cannot be
//! undone by the fetch failing. Grants live in memory: a hibernation, an exit
//! or an engine restart drops them, which fails closed.
//!
//! Honest caveat: the engine and its sessions run as the
//! same uid today, so `/proc/1/environ` still exposes the identity to any
//! session that goes looking. Until sessions run as a separate uid this is a
//! reduction in ambient exposure with an audit trail, not an enforced
//! boundary. The design does not change when that lands — the boundary just
//! becomes real.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::{
    body::Body,
    extract::{Path, State},
    http::{header, Method, Request, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use dashmap::DashMap;
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use uuid::Uuid;

use crate::app::AppState;
use crate::auth;
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, FileExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path as FsPath, PathBuf};
use std::{collections::HashMap, fs::OpenOptions, net::SocketAddr};

use crate::config::Config;
use crate::error::ApiError;
use crate::observability::RequestId;
use crate::pty;

/// The per-session broker token, injected into every session's environment
/// when brokering is configured. Named with `TOKEN` on purpose: were it ever
/// to reach the engine's own environment, `pty::is_secret_env` would strip it
/// from children rather than let it leak sideways.
pub const BROKER_TOKEN_ENV: &str = "VOGT_ENGINE_BROKER_TOKEN";
/// Where a session finds the broker: the engine on loopback.
pub const BROKER_URL_ENV: &str = "VOGT_ENGINE_BROKER_URL";
/// The read route. `POST` only, outside the engine bearer gate (see `app.rs`),
/// authenticated with the session's own broker token.
pub const FETCH_ROUTE: &str = "/api/agent-auth/fetch/{var}";
/// The write route: a session stores one manifest secret it just
/// produced. Same auth and gate placement as the read route.
pub const STORE_ROUTE: &str = "/api/agent-auth/store/{var}";
/// The bundle-file variant of the write broker.
pub const BUNDLE_STORE_ROUTE: &str = "/api/agent-auth/store/{var}/bundle-file";
/// The session's own approved grants (WI-973). Same auth as the read route.
pub const GRANTS_ROUTE: &str = "/api/agent-auth/grants";

/// Per-session ceiling on fetches. A secret is asked for a handful of times in
/// a session's life; a loop asking sixty times a minute is a bug or an attack,
/// and either way the audit log should show it being refused.
const FETCH_LIMIT_PER_MINUTE: u32 = 60;
/// Per-session ceiling on stores, tighter than fetches: a session mints or
/// rotates a secret rarely, so ten writes a minute is already generous and a
/// runaway is worth refusing loudly.
const STORE_LIMIT_PER_MINUTE: u32 = 10;
/// Largest value a session may store: enough for a multi-line key, not a file
/// dump. A body above this is refused with 413 before the helper is run.
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
const MAX_IN_MEMORY_TRANSCRIPT: usize = 4 * 1024 * 1024;
/// A whole credentials or config file; these are small and replaced whole.
const MAX_BUNDLE_REQUEST_BYTES: usize = 1024 * 1024;
/// The largest file the engine hands a new pod when it seeds the bundle; a
/// transcript can grow well past one request, so this is wider than the write
/// cap.
const MAX_BUNDLE_SNAPSHOT_FILE_BYTES: usize = 16 * 1024 * 1024;
/// The most the bundle route buffers: a full 1 MiB payload as base64, plus
/// room for the JSON envelope. Checked against `Content-Length` before any of
/// the body is read, and enforced again while it is read.
const MAX_BUNDLE_BODY_BYTES: usize = MAX_BUNDLE_REQUEST_BYTES.div_ceil(3) * 4 + 8 * 1024;
/// Per-session ceiling on bundle writes. The agent debounces to one flush a
/// second and sends a transcript in chunks, so sixty to a hundred and twenty
/// a minute is a busy session; past that the audit log should show a runaway.
const BUNDLE_LIMIT_PER_MINUTE: u32 = 120;
/// The access-token lifetime Claude issues today (eight hours), the default
/// ceiling for a written-back credential's `expiresAt` is that plus slack.
const DEFAULT_CREDENTIAL_MAX_LIFETIME: Duration = Duration::from_secs(9 * 60 * 60);
/// `ENGINE_AGENT_CREDENTIAL_MAX_LIFETIME_SECS`: how far ahead of now a
/// written-back credential's `expiresAt` may be. A credentials file claiming
/// a later expiry is refused, so one session cannot pin the shared file with
/// an expiry no genuine refresh can beat.
pub const CREDENTIAL_MAX_LIFETIME_ENV: &str = "ENGINE_AGENT_CREDENTIAL_MAX_LIFETIME_SECS";
/// How many credential write-backs one session may land in
/// `CREDENTIAL_WRITE_WINDOW`. A genuine session refreshes about once per token
/// lifetime (eight hours), plus perhaps one catch-up at start, so three leaves
/// slack. A session that keeps rewriting a login with an expiry just under the
/// ceiling can hold off every genuine refresh for as long as it keeps writing
/// (each rewrite beats an honest refresh for up to ceiling minus lifetime); the
/// cap bounds that to a few windows of that gap instead of the session's life.
/// The count is kept in memory, not persisted: restarting the engine resets it.
const CREDENTIAL_WRITES_PER_WINDOW: usize = 3;
const CREDENTIAL_WRITE_WINDOW: Duration = Duration::from_secs(8 * 60 * 60);
/// How far ahead of the engine's clock a `.claude.json` version (the file's
/// mtime in milliseconds) may be, for pod clock skew.
const CONFIG_VERSION_SKEW_MS: u64 = 10 * 60 * 1000;
/// Where the last accepted `.claude.json` version is kept, beside the file, so
/// the version check survives an engine restart.
const CONFIG_VERSION_FILE: &str = ".claude.json.version";
/// One transcript delta. The transcript itself is unbounded: the agent sends
/// it in chunks no larger than this, each at the offset the engine expects.
pub const MAX_TRANSCRIPT_DELTA_BYTES: usize = 1024 * 1024;

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
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManifestSecret {
    pub var: String,
    pub project_id: String,
    pub secret_name: String,
    pub policy: ManifestPolicy,
    /// A session may write this entry through `POST /api/agent-auth/store/{var}`
    /// Widens blast radius only to the declared entries and every write
    /// is audited.
    pub writable: bool,
}

/// Parse the flag field of a manifest line: a comma-separated list drawn from
/// {optional, ondemand, writable} — plus `required`, the explicit spelling of
/// the default — in any order. A legacy single token (`optional`, `ondemand`,
/// or a bare line) is simply the one- or zero-element case, so old manifests
/// parse identically. An unknown flag is an error, never a silent "required"
/// Two conflicting policy flags are an error too.
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

impl ManifestSecret {
    /// This entry as one manifest line, in the grammar [`parse_manifest`]
    /// reads — how a composed manifest is handed to the helper.
    pub fn line(&self) -> String {
        let mut flags = Vec::new();
        match self.policy {
            ManifestPolicy::Required => {}
            ManifestPolicy::Optional => flags.push("optional"),
            ManifestPolicy::OnDemand => flags.push("ondemand"),
        }
        if self.writable {
            flags.push("writable");
        }
        let mut line = format!("{} {} {}", self.var, self.project_id, self.secret_name);
        if !flags.is_empty() {
            line.push(' ');
            line.push_str(&flags.join(","));
        }
        line
    }
}

/// The manifest as it stands now (WI-3491): the deploy-time entries plus the
/// lines of `file`, read afresh on every call. A missing file adds nothing,
/// so a deployment may name the path before it writes anything there. A VAR
/// the file declares twice, or that the deploy-time manifest already
/// declares, is an error: a live edit never silently shadows a stack line.
pub fn compose_manifest(
    inline: &[ManifestSecret],
    file: Option<&FsPath>,
) -> Result<Vec<ManifestSecret>, String> {
    let mut entries = inline.to_vec();
    let Some(path) = file else {
        return Ok(entries);
    };
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(entries),
        Err(e) => {
            return Err(format!(
                "reading ENGINE_AGENT_AUTH_SECRETS_FILE ({}): {e}",
                path.display()
            ))
        }
    };
    let added = parse_manifest(&text).map_err(|e| {
        format!(
            "in ENGINE_AGENT_AUTH_SECRETS_FILE ({}): {e}",
            path.display()
        )
    })?;
    for entry in added {
        if entries.iter().any(|known| known.var == entry.var) {
            return Err(format!(
                "ENGINE_AGENT_AUTH_SECRETS_FILE ({}) declares {} again; each VAR is declared once across ENGINE_AGENT_AUTH_SECRETS and the file",
                path.display(),
                entry.var
            ));
        }
        entries.push(entry);
    }
    Ok(entries)
}

/// [`ManifestSecret::line`] for every entry, one per line.
pub fn render_manifest(entries: &[ManifestSecret]) -> String {
    entries.iter().map(|entry| entry.line() + "\n").collect()
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

/// What a `store` did, as the helper reported it on stdout. Audited;
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
    /// The deploy-time entries, `ENGINE_AGENT_AUTH_SECRETS`. Policy is these
    /// plus `manifest_file`, read live by [`Self::manifest`].
    manifest: Vec<ManifestSecret>,
    /// `ENGINE_AGENT_AUTH_SECRETS_FILE`, re-read on every check and launch.
    manifest_file: Option<PathBuf>,
    /// `ENGINE_LLM_PROVIDERS_FILE`: each registered provider's key is a
    /// manifest entry too, re-read on every check and launch.
    providers_file: Option<PathBuf>,
    /// Session id → SHA-256 of that session's broker token. The plaintext is
    /// handed to the child once and never stored.
    grants: DashMap<Uuid, [u8; 32]>,
    /// Projects beyond the manifest's that an approved grant may fetch from
    /// (`ENGINE_AGENT_GRANT_PROJECTS`).
    grant_projects: Vec<String>,
    /// Session id → the person-approved credential grants it holds (WI-973).
    session_grants: DashMap<Uuid, Vec<HeldGrant>>,
    bundle: DashMap<(String, String), Vec<u8>>,
    bundle_versions: DashMap<(String, String), u64>,
    account_dirs: HashMap<String, PathBuf>,
    /// The Codex home a pod Codex rollout is written under, when configured.
    /// Codex keeps every conversation under one home, so this is not an
    /// account directory (WI-3867).
    codex_home: Option<PathBuf>,
    /// Serialises compare-and-replace operations for one account bundle. The
    /// credentials expiry check and its write must be one critical section.
    account_locks: DashMap<String, Arc<Mutex<()>>>,
    /// How far ahead a written-back credential's `expiresAt` may be.
    credential_max_lifetime: Duration,
    /// Session id → when its accepted credential write-backs landed, within
    /// `CREDENTIAL_WRITE_WINDOW`. In memory only: an engine restart empties
    /// it, so the allowance starts afresh (a restart grants a session at most
    /// one extra window of writes).
    credential_writes: DashMap<Uuid, Vec<Instant>>,
}

/// The longest a grant may live.
pub const MAX_GRANT_TTL: Duration = Duration::from_secs(24 * 60 * 60);
/// The longest `reason` a grant carries: it is shown, never acted on.
pub const MAX_GRANT_REASON_CHARS: usize = 2000;

/// A grant as the broker holds it: the wire shape plus its parsed expiry.
#[derive(Clone, Debug)]
pub struct HeldGrant {
    pub grant: vogt_engine_contract::SessionGrant,
    pub expires_at: time::OffsetDateTime,
}

impl HeldGrant {
    /// The manifest line this grant stands for, as the helper reads it.
    fn manifest_line(&self) -> String {
        format!(
            "{} {} {} ondemand",
            self.grant.var, self.grant.project_id, self.grant.secret_name
        )
    }

    fn entry(&self) -> ManifestSecret {
        ManifestSecret {
            var: self.grant.var.clone(),
            project_id: self.grant.project_id.clone(),
            secret_name: self.grant.secret_name.clone(),
            policy: ManifestPolicy::OnDemand,
            writable: false,
        }
    }
}

/// Why a grant was not applied. Each says what to change.
#[derive(Debug, PartialEq, Eq)]
pub enum GrantRefusal {
    /// No broker, so nothing could ever reach the session.
    NoBroker,
    /// A field the grant cannot have.
    Invalid(String),
    /// The name is a manifest entry; a grant never shadows one.
    ShadowsManifest(String),
    /// The project is not open to grants.
    ProjectNotGrantable(String),
    /// Another live grant already lets this session fetch that `var`.
    VarInUse(String),
}

impl std::fmt::Display for GrantRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GrantRefusal::NoBroker => write!(
                f,
                "on-demand secret brokering is not configured on this engine \
                 (ENGINE_AGENT_AUTH_SECRETS declares nothing), so no session can be handed a grant"
            ),
            GrantRefusal::Invalid(why) => write!(f, "{why}"),
            GrantRefusal::ShadowsManifest(var) => write!(
                f,
                "{var} is already an ENGINE_AGENT_AUTH_SECRETS entry; a grant never shadows the manifest — choose another var"
            ),
            GrantRefusal::ProjectNotGrantable(project) => write!(
                f,
                "project {project} is not open to grants: grants may fetch only from projects the manifest names or ENGINE_AGENT_GRANT_PROJECTS lists"
            ),
            GrantRefusal::VarInUse(var) => write!(
                f,
                "the session already holds a live grant for {var}; revoke it first, or choose another var"
            ),
        }
    }
}

impl SecretBroker {
    pub fn new(cfg: &Config) -> Self {
        Self::with(
            cfg.bind,
            cfg.agent_auth_helper.clone(),
            cfg.agent_auth_secrets.clone(),
        )
        .with_manifest_file(cfg.agent_auth_secrets_file.clone())
        .with_providers_file(cfg.llm_providers_file.clone())
        .with_codex_home(cfg.codex_transcript_root.clone())
    }

    /// The three things the broker needs, and nothing else of the config.
    pub fn with(bind: SocketAddr, helper: PathBuf, manifest: Vec<ManifestSecret>) -> Self {
        Self {
            bind,
            helper,
            manifest,
            manifest_file: None,
            providers_file: None,
            grants: DashMap::new(),
            grant_projects: Vec::new(),
            session_grants: DashMap::new(),
            bundle: DashMap::new(),
            bundle_versions: DashMap::new(),
            account_dirs: HashMap::new(),
            codex_home: None,
            account_locks: DashMap::new(),
            credential_max_lifetime: credential_max_lifetime_from_env(),
            credential_writes: DashMap::new(),
        }
    }

    /// Override the ceiling on a written-back credential's expiry.
    pub fn with_credential_max_lifetime(mut self, lifetime: Duration) -> Self {
        self.credential_max_lifetime = lifetime;
        self
    }

    pub fn with_account_dirs(mut self, dirs: HashMap<String, PathBuf>) -> Self {
        self.account_dirs = dirs;
        self
    }

    /// Store pod Codex rollouts under `home` (WI-3867). `None` leaves Codex
    /// writeback off; a deployment resolves its default in `config::load`.
    pub fn with_codex_home(mut self, home: Option<PathBuf>) -> Self {
        self.codex_home = home;
        self
    }

    /// Read the live part of the manifest from `path` at each use.
    pub fn with_manifest_file(mut self, path: Option<PathBuf>) -> Self {
        self.manifest_file = path;
        self
    }

    /// Read the LLM provider registry from `path` at each use.
    pub fn with_providers_file(mut self, path: Option<PathBuf>) -> Self {
        self.providers_file = path;
        self
    }

    /// The LLM providers registered now. A registry that cannot be used
    /// registers nothing and is logged by path and reason (names only).
    pub fn providers(&self) -> Vec<crate::llm_providers::Provider> {
        let Some(path) = self.providers_file.as_deref() else {
            return Vec::new();
        };
        let registered = crate::llm_providers::load(path).unwrap_or_else(|error| {
            tracing::warn!(%error, "LLM provider registry ignored");
            Vec::new()
        });
        if registered.is_empty() {
            return registered;
        }
        // A provider whose env_key is already a manifest VAR is dropped
        // whole: no Codex table and no fetch. Its table would point that
        // VAR, which holds the manifest's secret, at the registry's
        // base_url, and fetching it would bypass the manifest line's policy.
        let manifest = self.base_manifest();
        registered
            .into_iter()
            .filter(|p| {
                let clash = manifest.iter().any(|known| known.var == p.env_key);
                if clash {
                    tracing::warn!(
                        provider = %p.name,
                        var = %p.env_key,
                        "LLM provider dropped: its env_key is already a manifest entry"
                    );
                }
                !clash
            })
            .collect()
    }

    /// The deploy-time entries plus the manifest file's, without the
    /// registry. A file that cannot be used contributes nothing and is logged
    /// (path and parse error, which carry names, never values).
    fn base_manifest(&self) -> Vec<ManifestSecret> {
        compose_manifest(&self.manifest, self.manifest_file.as_deref()).unwrap_or_else(|error| {
            tracing::warn!(%error, "agent-auth manifest file ignored");
            self.manifest.clone()
        })
    }

    /// The manifest as it stands now: the deploy-time entries plus the
    /// manifest file's, plus one entry per registered provider key. A bad
    /// live edit of the file narrows sessions to the deploy-time manifest
    /// rather than stopping launches. A provider key never shadows a
    /// manifest line: [`Self::providers`] has already dropped any clash.
    pub fn manifest(&self) -> Vec<ManifestSecret> {
        let mut entries = self.base_manifest();
        entries.extend(crate::llm_providers::manifest_entries(&self.providers()));
        entries
    }

    /// The composed manifest text for the helper's `ENGINE_AGENT_AUTH_SECRETS`,
    /// or `None` when there is no manifest file, so a deployment without one
    /// hands the helper exactly the variable it set.
    pub fn live_manifest_text(&self) -> Option<String> {
        (self.manifest_file.is_some() || self.providers_file.is_some())
            .then(|| render_manifest(&self.manifest()))
    }

    /// Open more projects to approved grants than the manifest names.
    pub fn with_grant_projects(mut self, projects: Vec<String>) -> Self {
        self.grant_projects = projects;
        self
    }

    /// Whether an approved grant may fetch from `project`: one the manifest
    /// already names, or one the deployment opened to grants.
    pub fn project_grantable(&self, project: &str) -> bool {
        self.manifest().iter().any(|e| e.project_id == project)
            || self.grant_projects.iter().any(|p| p == project)
    }

    /// Hold a person-approved grant for `session` (WI-973). The caller has
    /// already checked that vogt-core sent it and that the session is live.
    /// A grant with the same id replaces the earlier one (a retried apply);
    /// a *different* grant for a `var` the session can already fetch is
    /// refused, so one approval never silently ends another.
    pub fn apply_grant(
        &self,
        session: Uuid,
        grant: vogt_engine_contract::SessionGrant,
        now: time::OffsetDateTime,
    ) -> Result<HeldGrant, GrantRefusal> {
        if !self.enabled() {
            return Err(GrantRefusal::NoBroker);
        }
        if grant.grant_id.trim().is_empty() || grant.grant_id.len() > 64 {
            return Err(GrantRefusal::Invalid(
                "grant_id must be 1-64 characters".into(),
            ));
        }
        if !is_env_name(&grant.var) {
            return Err(GrantRefusal::Invalid(format!(
                "var {:?} is not a valid environment variable name",
                grant.var
            )));
        }
        let plain = |v: &str| {
            !v.is_empty()
                && v.len() <= 256
                && !v.starts_with('-')
                && v.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "._-/".contains(c))
        };
        if !plain(&grant.secret_name) || !plain(&grant.project_id) {
            return Err(GrantRefusal::Invalid(
                "project_id and secret_name must be plain names (letters, digits, . _ - /)".into(),
            ));
        }
        if self.permitted(&grant.var).is_some() {
            return Err(GrantRefusal::ShadowsManifest(grant.var));
        }
        if !self.project_grantable(&grant.project_id) {
            return Err(GrantRefusal::ProjectNotGrantable(grant.project_id));
        }
        let expires_at = time::OffsetDateTime::parse(
            &grant.expires_at,
            &time::format_description::well_known::Rfc3339,
        )
        .map_err(|e| GrantRefusal::Invalid(format!("expires_at is not RFC 3339: {e}")))?;
        if expires_at <= now {
            return Err(GrantRefusal::Invalid("expires_at is in the past".into()));
        }
        if expires_at - now > MAX_GRANT_TTL {
            return Err(GrantRefusal::Invalid(format!(
                "expires_at is more than {} h ahead",
                MAX_GRANT_TTL.as_secs() / 3600
            )));
        }
        if grant.reason.chars().count() > MAX_GRANT_REASON_CHARS {
            return Err(GrantRefusal::Invalid(format!(
                "reason is longer than {MAX_GRANT_REASON_CHARS} characters"
            )));
        }
        let held = HeldGrant { grant, expires_at };
        let mut list = self.session_grants.entry(session).or_default();
        list.retain(|g| g.expires_at > now);
        if list
            .iter()
            .any(|g| g.grant.grant_id != held.grant.grant_id && g.grant.var == held.grant.var)
        {
            return Err(GrantRefusal::VarInUse(held.grant.var));
        }
        list.retain(|g| g.grant.grant_id != held.grant.grant_id);
        list.push(held.clone());
        Ok(held)
    }

    /// Drop one grant; whether the session held it.
    pub fn remove_grant(&self, session: Uuid, grant_id: &str) -> bool {
        let Some(mut list) = self.session_grants.get_mut(&session) else {
            return false;
        };
        let before = list.len();
        list.retain(|g| g.grant.grant_id != grant_id);
        before != list.len()
    }

    /// Take a `once` grant out of the table before it is used, so two fetches
    /// racing for it cannot both be answered; `None` when another took it.
    /// It is never put back: a fetch that fails has spent it, because a
    /// revoke or a session exit that lands while the helper runs must stand.
    pub fn take_grant(&self, session: Uuid, grant_id: &str) -> Option<HeldGrant> {
        let mut list = self.session_grants.get_mut(&session)?;
        let index = list.iter().position(|g| g.grant.grant_id == grant_id)?;
        Some(list.remove(index))
    }

    /// Drop every grant a session holds: it ended, or was hibernated.
    pub fn drop_grants(&self, session: Uuid) {
        self.session_grants.remove(&session);
    }

    /// The grants a session holds now, the expired ones dropped.
    pub fn grants_for(&self, session: Uuid, now: time::OffsetDateTime) -> Vec<HeldGrant> {
        let Some(mut list) = self.session_grants.get_mut(&session) else {
            return Vec::new();
        };
        list.retain(|g| g.expires_at > now);
        list.clone()
    }

    /// The live grant that lets `session` fetch `var`, if any.
    pub fn grant_for_var(
        &self,
        session: Uuid,
        var: &str,
        now: time::OffsetDateTime,
    ) -> Option<HeldGrant> {
        self.grants_for(session, now)
            .into_iter()
            .find(|g| g.grant.var == var)
    }

    /// Brokering exists only where a deployment declared something to broker.
    /// With an empty manifest the route answers 503 with that reason and no
    /// session is handed a token (absence is a reported state).
    /// A configured manifest file counts even while empty: a session launched
    /// before a line is added can then fetch it once it is.
    pub fn enabled(&self) -> bool {
        !self.manifest.is_empty() || self.manifest_file.is_some() || self.providers_file.is_some()
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
        self.session_grants.remove(&session);
        self.credential_writes.remove(&session);
    }

    /// Claim one of `session`'s credential write-backs for the window ending
    /// at `now`. `false` when it has used them all; nothing is claimed then.
    /// A claim for a write that is then refused is given back with
    /// `release_credential_write`, so only landed writes count.
    pub fn reserve_credential_write(&self, session: Uuid, now: Instant) -> bool {
        let mut landed = self.credential_writes.entry(session).or_default();
        landed.retain(|at| now.saturating_duration_since(*at) < CREDENTIAL_WRITE_WINDOW);
        if landed.len() >= CREDENTIAL_WRITES_PER_WINDOW {
            return false;
        }
        landed.push(now);
        true
    }

    /// Give back the claim `reserve_credential_write` made at `now`.
    pub fn release_credential_write(&self, session: Uuid, now: Instant) {
        if let Some(mut landed) = self.credential_writes.get_mut(&session) {
            if let Some(index) = landed.iter().rposition(|at| *at == now) {
                landed.remove(index);
            }
        }
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
        self.manifest().into_iter().find(|entry| entry.var == var)
    }

    /// The manifest entry for `var`, but only when the deployment flagged it
    /// `writable` — the membership *and* flag test a store must pass. A
    /// name that is declared but not writable returns `None` here; the handler
    /// tells the two apart with `permitted` to give a distinct refusal.
    pub fn permitted_write(&self, var: &str) -> Option<ManifestSecret> {
        self.manifest()
            .into_iter()
            .find(|entry| entry.var == var && entry.writable)
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
        self.fetch_with(entry, None).await
    }

    /// [`Self::fetch`], with the helper's manifest replaced by `manifest` for
    /// this one call: how a granted secret is fetched, so the helper's own
    /// manifest check sees exactly the granted line and nothing more.
    async fn fetch_with(
        &self,
        entry: &ManifestSecret,
        manifest: Option<&str>,
    ) -> Result<String, FetchFailure> {
        let mut cmd = tokio::process::Command::new(&self.helper);
        cmd.arg("get")
            .arg(&entry.var)
            .env_clear()
            .envs(pty::sanitized_child_env())
            .envs(pty::agent_auth_helper_env());
        if let Some(manifest) = manifest {
            cmd.env("ENGINE_AGENT_AUTH_SECRETS", manifest);
        } else if let Some(live) = self.live_manifest_text() {
            // The helper re-checks the manifest: give it the one in force now.
            cmd.env("ENGINE_AGENT_AUTH_SECRETS", live);
        }
        cmd.stdin(std::process::Stdio::null())
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
    /// that word — never the value — is all that comes back.
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
            .envs(pty::agent_auth_helper_env());
        if let Some(live) = self.live_manifest_text() {
            cmd.env("ENGINE_AGENT_AUTH_SECRETS", live);
        }
        cmd.stdin(std::process::Stdio::piped())
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

    /// `store_bundle_file` on behalf of `(session, now)`: a credentials write
    /// counts against the session's allowance for the window ending at `now`, and
    /// only a write that lands keeps its count.
    pub fn store_bundle_file_for(
        &self,
        (session, now): (Uuid, Instant),
        account: &str,
        name: &str,
        kind: BundleFileKind,
        bytes: &[u8],
        version: Option<u64>,
    ) -> Result<(), BundleRefusal> {
        let limited = kind == BundleFileKind::Credentials;
        if limited && !self.reserve_credential_write(session, now) {
            return Err(BundleRefusal::CredentialWriteLimit);
        }
        let result = self.store_bundle_file(account, name, kind, bytes, version);
        if limited && result.is_err() {
            self.release_credential_write(session, now);
        }
        result
    }

    pub fn store_bundle_file(
        &self,
        account: &str,
        name: &str,
        kind: BundleFileKind,
        bytes: &[u8],
        version: Option<u64>,
    ) -> Result<(), BundleRefusal> {
        if bytes.len() > MAX_BUNDLE_REQUEST_BYTES {
            return Err(BundleRefusal::TooLarge);
        }
        let root = self
            .account_dirs
            .get(account)
            .map(FsPath::new)
            .ok_or(BundleRefusal::Storage)?;
        let account_lock = self
            .account_locks
            .entry(account.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone();
        let _account_guard = account_lock.lock().map_err(|_| BundleRefusal::Storage)?;
        ensure_bundle_root(root).map_err(|_| BundleRefusal::Storage)?;
        match kind {
            BundleFileKind::Credentials | BundleFileKind::Config => {
                let filename = match kind {
                    BundleFileKind::Credentials => ".credentials.json",
                    BundleFileKind::Config => ".claude.json",
                    BundleFileKind::Transcript => return Err(BundleRefusal::TranscriptNeedsOffset),
                };
                let now_ms = now_millis();
                if kind == BundleFileKind::Credentials {
                    let max_ms = self.credential_max_lifetime.as_millis() as u64;
                    let expiry = validate_credentials(bytes, now_ms, max_ms)
                        .map_err(BundleRefusal::Credentials)?;
                    if let Ok(old) = std::fs::read(root.join(filename)) {
                        // A stored file whose expiry is past the ceiling (put
                        // there before the ceiling existed) must not outrank a
                        // genuine refresh for ever.
                        let stored = oauth_expiry(&old);
                        let stored = if stored > now_ms.saturating_add(max_ms) {
                            0
                        } else {
                            stored
                        };
                        if expiry <= stored {
                            return Err(BundleRefusal::OlderCredentials);
                        }
                        // A refresh mints a new access token. The same one
                        // under a later expiry is a bump, not a refresh: it
                        // is how a session pins the file ahead of an honest
                        // refresh without holding a login at all.
                        if oauth_access_token(&old) == oauth_access_token(bytes) {
                            return Err(BundleRefusal::UnchangedCredentials);
                        }
                    }
                }
                let key = (account.to_string(), name.to_string());
                if kind == BundleFileKind::Config {
                    let Some(version) = version else {
                        return Err(BundleRefusal::ConfigNeedsVersion);
                    };
                    if version > now_ms.saturating_add(CONFIG_VERSION_SKEW_MS) {
                        return Err(BundleRefusal::ConfigVersionAhead);
                    }
                    if version <= self.stored_config_version(root, &key) {
                        return Err(BundleRefusal::OlderConfig);
                    }
                }
                atomic_bundle_write(root, FsPath::new(filename), bytes)
                    .map_err(|_| BundleRefusal::Storage)?;
                self.put_bundle(key.clone(), bytes.to_vec());
                if let Some(version) = version.filter(|_| kind == BundleFileKind::Config) {
                    // The file is in place; a failure to record its version
                    // costs only the restart-proof half of the check.
                    let _ = atomic_bundle_write(
                        root,
                        FsPath::new(CONFIG_VERSION_FILE),
                        version.to_string().as_bytes(),
                    );
                    self.bundle_versions.insert(key, version);
                }
            }
            BundleFileKind::Transcript => return Err(BundleRefusal::TranscriptNeedsOffset),
        }
        Ok(())
    }

    /// The last `.claude.json` version accepted for the account: the
    /// in-memory value, else the one persisted beside the file, else 0.
    fn stored_config_version(&self, root: &FsPath, key: &(String, String)) -> u64 {
        if let Some(version) = self.bundle_versions.get(key) {
            return *version;
        }
        let path = root.join(CONFIG_VERSION_FILE);
        let version = std::fs::symlink_metadata(&path)
            .ok()
            .filter(|meta| meta.file_type().is_file() && meta.len() <= 32)
            .and_then(|_| std::fs::read_to_string(&path).ok())
            .and_then(|text| text.trim().parse().ok())
            .unwrap_or(0);
        self.bundle_versions.insert(key.clone(), version);
        version
    }

    /// The files a pod for `account` is seeded with: the account's login and
    /// global config, plus the transcript of `conversation` when the engine
    /// holds one (a woken session resumes from it). A Codex rollout is kept
    /// under the configured Codex home and is returned as the contract path
    /// `sessions/…` (WI-3867); the rest are relative to the account directory.
    /// Nothing is followed through a symlink, and a file over the bundle size
    /// cap is left out rather than truncated.
    pub fn bundle_snapshot(&self, account: &str, conversation: &str) -> Vec<(String, Vec<u8>)> {
        let read = |path: &FsPath| {
            let meta = std::fs::symlink_metadata(path).ok()?;
            (meta.file_type().is_file() && meta.len() as usize <= MAX_BUNDLE_SNAPSHOT_FILE_BYTES)
                .then(|| std::fs::read(path).ok())
                .flatten()
        };
        let mut files = Vec::new();
        if let Some(root) = self.account_dirs.get(account).map(FsPath::new) {
            let account_lock = self
                .account_locks
                .entry(account.to_string())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone();
            let Ok(_guard) = account_lock.lock() else {
                return files;
            };
            for name in [".credentials.json", ".claude.json"] {
                if let Some(bytes) = read(&root.join(name)) {
                    files.push((name.to_string(), bytes));
                }
            }
            if !conversation.is_empty() && !conversation.contains(['/', '\\']) {
                let wanted = format!("{conversation}.jsonl");
                let mut dirs = vec![root.join("projects")];
                while let Some(dir) = dirs.pop() {
                    let Ok(entries) = std::fs::read_dir(&dir) else {
                        continue;
                    };
                    for entry in entries.flatten() {
                        let Ok(kind) = entry.file_type() else {
                            continue;
                        };
                        if kind.is_dir() {
                            dirs.push(entry.path());
                        } else if kind.is_file() && entry.file_name() == wanted.as_str() {
                            if let (Some(bytes), Ok(relative)) =
                                (read(&entry.path()), entry.path().strip_prefix(root))
                            {
                                files.push((relative.to_string_lossy().into_owned(), bytes));
                            }
                        }
                    }
                }
            }
        }
        // A Codex rollout belongs to the conversation, not an account: Codex
        // keeps one home for every session, and the writeback's account is
        // allowed to be empty (WI-3867).
        if conversation.is_empty() || conversation.contains(['/', '\\']) {
            return files;
        }
        if let Some(home) = self.codex_home.as_deref() {
            if let Some(path) =
                crate::transcripts::codex_transcript(&home.join("sessions"), conversation)
            {
                if let (Some(bytes), Ok(relative)) = (read(&path), path.strip_prefix(home)) {
                    files.push((relative.to_string_lossy().into_owned(), bytes));
                }
            }
        }
        files
    }

    /// Append a transcript delta at `offset`. The engine appends only when
    /// `offset` is the file's current length; `Ok` carries the new length.
    /// A delta that is already on disk at `offset` (a retry, or a concurrent
    /// duplicate) is accepted without writing again, and any other offset is a
    /// `Resync` naming the length the agent must resend from.
    pub fn append_transcript(
        &self,
        account: &str,
        conversation: &str,
        name: &str,
        offset: u64,
        bytes: &[u8],
    ) -> Result<u64, BundleRefusal> {
        if bytes.len() > MAX_TRANSCRIPT_DELTA_BYTES {
            return Err(BundleRefusal::TooLarge);
        }
        let (root, lock_key) = match transcript_layout(name) {
            TranscriptLayout::Claude => {
                if !claude_transcript_path(conversation, name) {
                    return Err(BundleRefusal::ConversationScope);
                }
                (
                    self.account_dirs
                        .get(account)
                        .map(FsPath::new)
                        .ok_or(BundleRefusal::Storage)?,
                    account.to_string(),
                )
            }
            TranscriptLayout::Codex => {
                if !codex_rollout_path(conversation, name) {
                    return Err(BundleRefusal::ConversationScope);
                }
                (
                    self.codex_home.as_deref().ok_or(BundleRefusal::Storage)?,
                    "\0codex".to_string(),
                )
            }
        };
        let account_lock = self
            .account_locks
            .entry(lock_key)
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone();
        let _account_guard = account_lock.lock().map_err(|_| BundleRefusal::Storage)?;
        ensure_bundle_root(root).map_err(|_| BundleRefusal::Storage)?;
        ensure_bundle_path(root, FsPath::new(name)).map_err(|_| BundleRefusal::Storage)?;
        let path = std::fs::canonicalize(root)
            .map_err(|_| BundleRefusal::Storage)?
            .join(name);
        let file = match OpenOptions::new()
            .read(true)
            .append(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)
        {
            Ok(file) => file,
            Err(_) => return Err(BundleRefusal::Storage),
        };
        let meta = file.metadata().map_err(|_| BundleRefusal::Storage)?;
        if !meta.is_file() {
            return Err(BundleRefusal::Storage);
        }
        let length = meta.len();
        let end = offset + bytes.len() as u64;
        if offset == length {
            if !bytes.is_empty() {
                let mut file = &file;
                file.write_all(bytes).map_err(|_| BundleRefusal::Storage)?;
                file.sync_all().map_err(|_| BundleRefusal::Storage)?;
                self.append_cache((account.to_string(), name.to_string()), bytes, &path);
            }
            return Ok(end);
        }
        if end <= length {
            let mut existing = vec![0u8; bytes.len()];
            file.read_exact_at(&mut existing, offset)
                .map_err(|_| BundleRefusal::Storage)?;
            if existing == bytes {
                return Ok(length);
            }
        }
        Err(BundleRefusal::Resync { offset: length })
    }

    /// Keep only the newest `MAX_IN_MEMORY_TRANSCRIPT` bytes of a transcript.
    fn append_cache(&self, key: (String, String), delta: &[u8], path: &FsPath) {
        let mut tail = self
            .bundle
            .get(&key)
            .map(|entry| entry.clone())
            .unwrap_or_else(|| {
                // First append since start: seed from the file's own tail.
                let Ok(file) = std::fs::File::open(path) else {
                    return Vec::new();
                };
                let length = file.metadata().map(|m| m.len()).unwrap_or(0);
                let before = length.saturating_sub(delta.len() as u64);
                let from = before.saturating_sub(MAX_IN_MEMORY_TRANSCRIPT as u64);
                let mut seeded = vec![0u8; (before - from) as usize];
                if file.read_exact_at(&mut seeded, from).is_err() {
                    seeded.clear();
                }
                seeded
            });
        tail.extend_from_slice(delta);
        let keep_from = tail.len().saturating_sub(MAX_IN_MEMORY_TRANSCRIPT);
        tail.drain(..keep_from);
        self.put_bundle(key, tail);
    }

    fn put_bundle(&self, key: (String, String), bytes: Vec<u8>) {
        const MAX_BUNDLE_ENTRIES: usize = 256;
        if self.bundle.len() >= MAX_BUNDLE_ENTRIES && !self.bundle.contains_key(&key) {
            if let Some(entry) = self.bundle.iter().next() {
                let old = entry.key().clone();
                drop(entry);
                self.bundle.remove(&old);
            }
        }
        self.bundle.insert(key, bytes);
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum BundleFileKind {
    Credentials,
    Config,
    Transcript,
}

#[derive(Debug, Deserialize)]
pub struct BundleFileRequest {
    pub account: String,
    /// Empty when the session has no conversation yet; transcripts need one.
    #[serde(default)]
    pub conversation_id: String,
    pub path: String,
    pub kind: BundleFileKind,
    pub bytes: String,
    #[serde(default)]
    pub version: Option<u64>,
    /// Transcripts only: the length of the file the delta in `bytes` extends.
    #[serde(default)]
    pub offset: Option<u64>,
}

#[derive(Debug)]
pub enum BundleRefusal {
    TooLarge,
    ConversationScope,
    /// The delta is not at the engine's end of the transcript; resend from `offset`.
    Resync {
        offset: u64,
    },
    TranscriptNeedsOffset,
    Storage,
    OlderCredentials,
    /// The access token is the stored one: an `expiresAt` bump on tokens that
    /// were not refreshed, which no genuine refresh produces.
    UnchangedCredentials,
    /// The session has used its credential write-backs for the window.
    CredentialWriteLimit,
    OlderConfig,
    /// A credentials file that is not a usable login; says which check failed.
    Credentials(CredentialRejection),
    /// A `.claude.json` write carried no version.
    ConfigNeedsVersion,
    /// A `.claude.json` version further ahead of now than clock skew explains.
    ConfigVersionAhead,
}

/// Why a credentials write-back is not a usable login. A category only: the
/// file's values are never named.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CredentialRejection {
    NotJson,
    MissingAccessToken,
    MissingRefreshToken,
    MissingExpiry,
    /// `expiresAt` is further ahead than a real token's lifetime allows.
    ExpiryTooFar,
    /// `expiresAt` is not in the future: not a login a pod could use.
    AlreadyExpired,
}

impl CredentialRejection {
    fn reason(self) -> &'static str {
        match self {
            CredentialRejection::NotJson => "credentials are not valid JSON",
            CredentialRejection::MissingAccessToken => "credentials hold no access token",
            CredentialRejection::MissingRefreshToken => "credentials hold no refresh token",
            CredentialRejection::MissingExpiry => "credentials hold no expiry",
            CredentialRejection::ExpiryTooFar => {
                "credentials expiry is beyond the token lifetime ceiling"
            }
            CredentialRejection::AlreadyExpired => "credentials are already expired",
        }
    }
}

impl BundleRefusal {
    /// The refusal in words, safe to log and to return: no value from the
    /// request is ever in it.
    fn reason(&self) -> &'static str {
        match self {
            BundleRefusal::TooLarge => "bundle-file exceeds the size cap",
            BundleRefusal::ConversationScope => {
                "bundle-file transcript is outside the session conversation"
            }
            BundleRefusal::Resync { .. } => "bundle-file transcript offset is stale",
            BundleRefusal::TranscriptNeedsOffset => "bundle-file transcript needs an offset",
            BundleRefusal::Storage => "bundle-file could not be persisted",
            BundleRefusal::OlderCredentials => {
                "bundle-file credentials are not newer than the stored credentials"
            }
            BundleRefusal::UnchangedCredentials => {
                "bundle-file credentials keep the stored access token"
            }
            BundleRefusal::CredentialWriteLimit => {
                "bundle-file credential write limit reached for this session"
            }
            BundleRefusal::OlderConfig => {
                "bundle-file config version is not newer than the stored config"
            }
            BundleRefusal::Credentials(why) => why.reason(),
            BundleRefusal::ConfigNeedsVersion => "bundle-file config needs a version",
            BundleRefusal::ConfigVersionAhead => {
                "bundle-file config version is ahead of the engine clock"
            }
        }
    }
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

fn credential_max_lifetime_from_env() -> Duration {
    std::env::var(CREDENTIAL_MAX_LIFETIME_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|secs| *secs > 0)
        .map_or(DEFAULT_CREDENTIAL_MAX_LIFETIME, Duration::from_secs)
}

/// Accept a credentials file only if it is a login a pod could use: it parses,
/// holds a non-empty access token and refresh token, and its `expiresAt` (ms
/// since the epoch) is no further ahead than `max_ms`. Returns the expiry.
fn validate_credentials(
    bytes: &[u8],
    now_ms: u64,
    max_ms: u64,
) -> Result<u64, CredentialRejection> {
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| CredentialRejection::NotJson)?;
    let present = |pointer: &str| {
        value
            .pointer(pointer)
            .and_then(|v| v.as_str())
            .is_some_and(|v| !v.trim().is_empty())
    };
    if !present("/claudeAiOauth/accessToken") {
        return Err(CredentialRejection::MissingAccessToken);
    }
    if !present("/claudeAiOauth/refreshToken") {
        return Err(CredentialRejection::MissingRefreshToken);
    }
    let expiry = oauth_expiry(bytes);
    if expiry == 0 {
        return Err(CredentialRejection::MissingExpiry);
    }
    if expiry > now_ms.saturating_add(max_ms) {
        return Err(CredentialRejection::ExpiryTooFar);
    }
    if expiry <= now_ms {
        return Err(CredentialRejection::AlreadyExpired);
    }
    Ok(expiry)
}

fn atomic_bundle_write(root: &FsPath, relative: &FsPath, bytes: &[u8]) -> std::io::Result<()> {
    if relative.is_absolute()
        || relative
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "invalid bundle path",
        ));
    }
    let root = std::fs::canonicalize(root)?;
    #[cfg(unix)]
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))?;
    let mut parent = root.clone();
    let mut components = relative.components().peekable();
    let filename = loop {
        let Some(component) = components.next() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "empty bundle path",
            ));
        };
        let name = match component {
            Component::Normal(name) => name,
            _ => unreachable!(),
        };
        if components.peek().is_none() {
            break name.to_owned();
        }
        parent.push(name);
        match std::fs::symlink_metadata(&parent) {
            Ok(meta) if meta.file_type().is_symlink() || !meta.is_dir() => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "bundle path escapes root",
                ))
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                create_private_dir(&parent)?;
            }
            Err(e) => return Err(e),
        }
    };
    let root_check = std::fs::canonicalize(&parent)?;
    if !root_check.starts_with(&root) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "bundle path escapes root",
        ));
    }
    let target = parent.join(&filename);
    if let Ok(meta) = std::fs::symlink_metadata(&target) {
        if meta.file_type().is_symlink() || !meta.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "bundle target is not a regular file",
            ));
        }
    }
    let mut tmp;
    let mut temp_name;
    for attempt in 0..100 {
        temp_name = format!(
            ".{}.vogt-tmp-{}-{}",
            filename.to_string_lossy(),
            std::process::id(),
            attempt
        );
        tmp = parent.join(&temp_name);
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&tmp)
        {
            Ok(mut file) => {
                file.write_all(bytes)?;
                file.sync_all()?;
                std::fs::rename(&tmp, &target)?;
                let dir = OpenOptions::new().read(true).open(&parent)?;
                dir.sync_all()?;
                return Ok(());
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "unable to allocate temporary bundle file",
    ))
}

/// Create one directory that is private from the moment it exists.
fn create_private_dir(path: &FsPath) -> std::io::Result<()> {
    std::fs::DirBuilder::new().mode(0o700).create(path)
}

fn oauth_access_token(bytes: &[u8]) -> Option<String> {
    serde_json::from_slice::<serde_json::Value>(bytes)
        .ok()?
        .pointer("/claudeAiOauth/accessToken")?
        .as_str()
        .map(str::to_string)
}

fn oauth_expiry(bytes: &[u8]) -> u64 {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return 0;
    };
    value
        .pointer("/claudeAiOauth/expiresAt")
        .and_then(|v| v.as_u64())
        .or_else(|| {
            value
                .pointer("/claudeAiOauth/expiresAt")
                .and_then(|v| v.as_str())
                .and_then(|v| v.parse().ok())
        })
        .unwrap_or(0)
}

fn ensure_bundle_root(root: &FsPath) -> std::io::Result<()> {
    let metadata = match std::fs::symlink_metadata(root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(root)?;
            std::fs::symlink_metadata(root)?
        }
        Err(error) => return Err(error),
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "bundle root is not a directory",
        ));
    }
    #[cfg(unix)]
    std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn ensure_bundle_path(root: &FsPath, relative: &FsPath) -> std::io::Result<()> {
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "invalid bundle path",
        ));
    }
    let root = std::fs::canonicalize(root)?;
    let mut parent = root.clone();
    let mut components = relative.components().peekable();
    while let Some(component) = components.next() {
        let Component::Normal(name) = component else {
            unreachable!()
        };
        parent.push(name);
        if components.peek().is_some() {
            match std::fs::symlink_metadata(&parent) {
                Ok(meta) if meta.file_type().is_symlink() || !meta.is_dir() => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "bundle path escapes root",
                    ));
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    create_private_dir(&parent)?;
                }
                Err(error) => return Err(error),
            }
        }
    }
    let canonical_parent = std::fs::canonicalize(parent.parent().unwrap_or(&root))?;
    if !canonical_parent.starts_with(&root) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "bundle path escapes root",
        ));
    }
    Ok(())
}

/// Which home a transcript bundle path belongs to. Codex's contract path
/// starts with `sessions/`; every other path is Claude's account bundle
/// (`projects/…`). The first component decides, so a malformed Codex path is
/// refused rather than written into an account directory.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TranscriptLayout {
    Claude,
    Codex,
}

fn transcript_layout(name: &str) -> TranscriptLayout {
    if name.split('/').next() == Some("sessions") {
        TranscriptLayout::Codex
    } else {
        TranscriptLayout::Claude
    }
}

/// Claude's bundle path: `<conversation>.jsonl`, optionally under a
/// `projects/` directory. Kept exactly as the account bundle has always
/// accepted it; `append_transcript` and `store_bundle_file` share it.
fn claude_transcript_path(conversation: &str, name: &str) -> bool {
    name.ends_with(&format!("/{conversation}.jsonl")) || name == format!("{conversation}.jsonl")
}

/// The Codex rollout contract path (WI-3867):
/// `sessions/<YYYY>/<MM>/<DD>/rollout-<…>-<conversation>.jsonl`.
///
/// Every component is a normal path component — no empty, `.` or `..` part and
/// no backslash — and the date is four/two/two digits in range. The filename
/// must carry a non-empty stamp before the conversation suffix.
fn codex_rollout_path(conversation: &str, name: &str) -> bool {
    if name.contains('\\')
        || conversation.is_empty()
        || conversation == "."
        || conversation == ".."
        || conversation.contains(['/', '\\'])
    {
        return false;
    }
    let parts: Vec<&str> = name.split('/').collect();
    let [prefix, year, month, day, file] = parts.as_slice() else {
        return false;
    };
    if *prefix != "sessions" || !digits(year, 4) {
        return false;
    }
    if date_part(month, 2, 1, 12).is_none() || date_part(day, 2, 1, 31).is_none() {
        return false;
    }
    let suffix = format!("-{conversation}.jsonl");
    file.starts_with("rollout-")
        && file.len() > "rollout-".len() + suffix.len()
        && file.ends_with(&suffix)
}

fn digits(value: &str, width: usize) -> bool {
    value.len() == width && value.bytes().all(|b| b.is_ascii_digit())
}

/// `value` must be `width` digits within `min..=max`.
fn date_part(value: &str, width: usize, min: u32, max: u32) -> Option<u32> {
    if value.len() != width || !value.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    value
        .parse::<u32>()
        .ok()
        .filter(|n| (min..=max).contains(n))
}

/// Store a file from the Claude account bundle. The request carries base64 so
/// JSON remains safe for credentials and JSONL alike; neither the payload nor
/// its decoded contents are logged.
pub async fn store_bundle_file(
    State(state): State<Arc<AppState>>,
    request: Request<Body>,
) -> Response {
    let broker = state.sessions.secret_broker();
    let presented = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|v| !v.is_empty());
    let Some(presented) = presented else {
        return ApiError::Unauthorized.into_response();
    };
    // A host session presents its broker grant; a pod presents the stream
    // credential its bootstrap handed it, the only one it holds (WI-3334).
    let session_id = match broker.authenticate(presented) {
        Some(id) => Some(id),
        None => {
            state
                .agent_auth
                .session_for_token(presented.as_bytes())
                .await
        }
    };
    let Some(session_id) = session_id.filter(|id| state.sessions.get(*id).is_ok()) else {
        return ApiError::Unauthorized.into_response();
    };
    if let Some(retry_after) = state.auth.check_mutation_rate_limit(
        &format!("secret-broker-bundle:{session_id}"),
        BUNDLE_LIMIT_PER_MINUTE,
    ) {
        tracing::warn!(target: "vogt::audit", session_id = %session_id,
            retry_after_ms = retry_after.as_millis() as u64,
            "bundle-file rate limit exceeded");
        return (
            StatusCode::TOO_MANY_REQUESTS,
            [(
                header::RETRY_AFTER,
                retry_after.as_secs().max(1).to_string(),
            )],
            Json(json!({"error": "bundle-file rate limit exceeded"})),
        )
            .into_response();
    }
    // The size is judged before the body is read: a declared length over the
    // bound is refused unread, and a body with no declared length is cut off
    // at the bound while it is buffered.
    let declared = request
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<usize>().ok());
    if declared.is_some_and(|n| n > MAX_BUNDLE_BODY_BYTES) {
        return refuse_bundle(session_id, "", "", BundleRefusal::TooLarge);
    }
    let body = match axum::body::to_bytes(request.into_body(), MAX_BUNDLE_BODY_BYTES).await {
        Ok(body) => body,
        Err(_) => return refuse_bundle(session_id, "", "", BundleRefusal::TooLarge),
    };
    let payload: BundleFileRequest = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(_) => return refusal(StatusCode::BAD_REQUEST, "invalid bundle-file request"),
    };
    let Some(summary) = state.sessions.summary_of(session_id) else {
        return ApiError::Unauthorized.into_response();
    };
    // A Codex rollout belongs to the conversation, not an account, so its
    // writeback may name no account (WI-3867). Everything else stays bound to
    // the account this session holds.
    let codex_transcript = payload.kind == BundleFileKind::Transcript
        && transcript_layout(&payload.path) == TranscriptLayout::Codex;
    let account = match summary.account.as_deref() {
        Some(account) if account == payload.account => account,
        _ if codex_transcript && payload.account.is_empty() => payload.account.as_str(),
        _ => {
            return refusal(
                StatusCode::FORBIDDEN,
                "bundle-file account is not owned by this session",
            )
        }
    };
    // A transcript belongs to the session's conversation. The login and the
    // global config belong to the account, so a session with no conversation
    // pinned yet may still write them back (WI-3334).
    let owned = summary.conversation.as_ref().map(|c| c.id.as_str());
    let conversation = match owned {
        Some(id) if id == payload.conversation_id => id,
        None if payload.kind != BundleFileKind::Transcript
            && payload.conversation_id.is_empty() =>
        {
            ""
        }
        _ => {
            return refusal(
                StatusCode::FORBIDDEN,
                "bundle-file conversation is not owned by this session",
            )
        }
    };
    let bytes =
        match base64::Engine::decode(&base64::engine::general_purpose::STANDARD, payload.bytes) {
            Ok(bytes) => bytes,
            Err(_) => {
                return refusal(
                    StatusCode::BAD_REQUEST,
                    "bundle-file bytes are not valid base64",
                )
            }
        };
    let valid_path = match payload.kind {
        BundleFileKind::Credentials => payload.path == ".credentials.json",
        BundleFileKind::Config => payload.path == ".claude.json",
        BundleFileKind::Transcript => match transcript_layout(&payload.path) {
            TranscriptLayout::Claude => {
                payload.path.starts_with("projects/")
                    && payload.path.ends_with(&format!("/{conversation}.jsonl"))
            }
            TranscriptLayout::Codex => codex_rollout_path(conversation, &payload.path),
        },
    };
    if !valid_path {
        return refusal(
            StatusCode::FORBIDDEN,
            "bundle-file path is outside the session bundle",
        );
    }
    let length = bytes.len();
    if payload.kind == BundleFileKind::Transcript {
        let Some(offset) = payload.offset else {
            return refuse_bundle(
                session_id,
                account,
                &payload.path,
                BundleRefusal::TranscriptNeedsOffset,
            );
        };
        return match broker.append_transcript(account, conversation, &payload.path, offset, &bytes)
        {
            Ok(next) => {
                tracing::info!(target: "vogt::audit", session_id = %session_id, account = %account,
                    file = %payload.path, bytes = length, offset, "bundle transcript appended");
                Json(json!({"ok": true, "bytes": length, "offset": next})).into_response()
            }
            Err(BundleRefusal::Resync { offset }) => (
                StatusCode::CONFLICT,
                Json(json!({"ok": false, "resync": true, "offset": offset})),
            )
                .into_response(),
            Err(error) => refuse_bundle(session_id, account, &payload.path, error),
        };
    }
    let result = broker.store_bundle_file_for(
        (session_id, Instant::now()),
        account,
        &payload.path,
        payload.kind,
        &bytes,
        payload.version,
    );
    if let Err(error) = result {
        return refuse_bundle(session_id, account, &payload.path, error);
    }
    tracing::info!(target: "vogt::audit", session_id = %session_id, account = %account,
        file = %payload.path, bytes = length, "bundle file replaced");
    Json(json!({"ok": true, "bytes": length})).into_response()
}

/// Audit a refused bundle write by session, account and reason, then answer
/// it. The reason is a fixed phrase; nothing from the request body is logged.
fn refuse_bundle(session_id: Uuid, account: &str, file: &str, error: BundleRefusal) -> Response {
    let reason = error.reason();
    tracing::warn!(target: "vogt::audit", session_id = %session_id, account = %account,
        file = %file, reason, "bundle file refused");
    let status = match error {
        BundleRefusal::TooLarge => StatusCode::PAYLOAD_TOO_LARGE,
        BundleRefusal::CredentialWriteLimit => StatusCode::TOO_MANY_REQUESTS,
        BundleRefusal::Storage => StatusCode::INTERNAL_SERVER_ERROR,
        _ => StatusCode::FORBIDDEN,
    };
    refusal(status, reason)
}

fn digest(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

/// `POST /api/agent-auth/fetch/{var}` — hand a session one manifest secret.
///
/// Registered outside `auth::require_bearer` on purpose (the session has no
/// engine bearer to present) and so does every check itself, in the
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
            target: "vogt::audit",
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

    // The manifest first, so a grant can never stand in for what the
    // deployment declared; then this session's own approved grants (WI-973),
    // only while the session is running.
    let granted = match broker.permitted(&var) {
        Some(_) => None,
        None => broker
            .grant_for_var(session, &var, time::OffsetDateTime::now_utc())
            .filter(|_| state.sessions.get(session).is_ok_and(|s| s.is_alive()))
            // A `once` grant is taken before it is used: of two fetches racing
            // for it, only one is answered. It is spent whether or not the
            // fetch succeeds (never restored), so a revoke that lands while
            // the helper runs cannot be undone by a failure.
            .and_then(|held| match held.grant.uses {
                vogt_engine_contract::GrantUses::Once => {
                    broker.take_grant(session, &held.grant.grant_id)
                }
                vogt_engine_contract::GrantUses::Ttl => Some(held),
            }),
    };
    let entry = match (broker.permitted(&var), granted.as_ref()) {
        (Some(entry), _) => entry,
        (None, Some(held)) => held.entry(),
        (None, None) => {
            tracing::warn!(
                target: "vogt::audit",
                request_id = %request_id,
                session_id = %session,
                var = %var,
                "secret broker refused: not in the manifest and not granted"
            );
            return refusal(
                StatusCode::FORBIDDEN,
                &format!(
                    "{var} is not in this deployment's ENGINE_AGENT_AUTH_SECRETS manifest and this session holds no approved grant for it; to make it available to sessions add a line '{var} PROJECT_ID SECRET_NAME ondemand' to the stack and restart the engine, or ask a person to approve a grant (session_grant_request)"
                ),
            );
        }
    };

    let fetched = match granted.as_ref() {
        Some(held) => broker.fetch_with(&entry, Some(&held.manifest_line())).await,
        None => broker.fetch(&entry).await,
    };
    let grant_id = granted
        .as_ref()
        .map(|g| g.grant.grant_id.clone())
        .unwrap_or_default();
    match fetched {
        Ok(value) => {
            // The whole audit trail of the fetch: who, what, never the value.
            tracing::info!(
                target: "vogt::audit",
                request_id = %request_id,
                session_id = %session,
                var = %entry.var,
                secret_name = %entry.secret_name,
                project_id = %entry.project_id,
                policy = ?entry.policy,
                grant_id = %grant_id,
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
                target: "vogt::audit",
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

/// `GET /api/agent-auth/grants` — the approved grants the presenting session
/// holds now (WI-973): `var`, secret name, project, uses and expiry. Never a
/// value. What `vogt-agent-auth grants` prints, so the agent — and the
/// permission classifier reading its transcript — can see what a person
/// approved for it.
pub async fn own_grants(State(state): State<Arc<AppState>>, request: Request<Body>) -> Response {
    let broker = state.sessions.secret_broker();
    let path = request.uri().path().to_string();
    let request_id = request
        .extensions()
        .get::<RequestId>()
        .map(|id| id.0.clone())
        .unwrap_or_default();
    let session = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .and_then(|token| broker.authenticate(token))
        .filter(|id| state.sessions.get(*id).is_ok());
    let Some(session) = session else {
        auth::record_auth_failure(
            &Method::GET,
            &path,
            &request_id,
            "missing or unknown broker token",
        )
        .await;
        return ApiError::Unauthorized.into_response();
    };
    let grants: Vec<_> = broker
        .grants_for(session, time::OffsetDateTime::now_utc())
        .into_iter()
        .map(|held| held.grant)
        .collect();
    Json(grants).into_response()
}

/// `POST /api/agent-auth/store/{var}` — take one secret a session produced and
/// store it, the write mirror of [`fetch`].
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
            target: "vogt::audit",
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
                target: "vogt::audit",
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
                target: "vogt::audit",
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
                target: "vogt::audit",
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
                target: "vogt::audit",
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
                target: "vogt::audit",
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
                target: "vogt::audit",
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

    #[test]
    fn a_rendered_manifest_parses_back_to_the_same_entries() {
        let text = "A p a\nB p b optional\nC p c ondemand,writable\nD p d writable\n";
        let entries = parse_manifest(text).unwrap();
        assert_eq!(parse_manifest(&render_manifest(&entries)).unwrap(), entries);
    }

    #[test]
    fn the_manifest_file_is_read_live_and_never_shadows_a_stack_line() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("manifest");
        let inline = parse_manifest("STACK proj stack\n").unwrap();
        let broker = SecretBroker::with(
            "127.0.0.1:8910".parse().unwrap(),
            PathBuf::from("/bin/false"),
            inline.clone(),
        )
        .with_manifest_file(Some(file.clone()));

        // No file yet: the stack's lines only, and the broker is still on so
        // a session launched now can fetch a line added later.
        assert!(broker.enabled());
        assert_eq!(broker.manifest(), inline);
        assert!(broker.permitted("LATER").is_none());

        // A line added after the broker was built is policy at once.
        std::fs::write(&file, "LATER proj later ondemand,writable\n").unwrap();
        assert_eq!(broker.permitted("LATER").unwrap().secret_name, "later");
        assert!(broker.permitted_write("LATER").is_some());
        assert!(broker.project_grantable("proj"));
        let text = broker.live_manifest_text().unwrap();
        assert!(text.contains("STACK proj stack\n"), "{text}");
        assert!(
            text.contains("LATER proj later ondemand,writable\n"),
            "{text}"
        );

        // Removed from the file: refused again.
        std::fs::write(&file, "").unwrap();
        assert!(broker.permitted("LATER").is_none());

        // The file re-declaring a stack VAR is refused, and a refused file
        // adds nothing: the stack's manifest stays in force.
        std::fs::write(&file, "STACK other other\nEXTRA proj extra\n").unwrap();
        let err = compose_manifest(&inline, Some(&file)).unwrap_err();
        assert!(err.contains("declares STACK again"), "{err}");
        assert_eq!(broker.manifest(), inline);
        assert!(broker.permitted("EXTRA").is_none());

        // So is a malformed line.
        std::fs::write(&file, "BROKEN proj\n").unwrap();
        assert!(compose_manifest(&inline, Some(&file))
            .unwrap_err()
            .contains("malformed"));
        assert_eq!(broker.manifest(), inline);
    }

    #[test]
    fn a_registered_provider_key_is_policy_while_it_is_registered() {
        let dir = tempfile::tempdir().unwrap();
        let registry = dir.path().join("providers.toml");
        let broker = broker_with("STACK proj stack\n", "127.0.0.1:8910")
            .with_providers_file(Some(registry.clone()));
        assert!(broker.enabled());
        assert!(
            broker.permitted("DS_KEY").is_none(),
            "unregistered is refused"
        );

        std::fs::write(
            &registry,
            "[[provider]]\nname = \"ds\"\nbase_url = \"http://l/v1\"\nwire_api = \"chat\"\nenv_key = \"DS_KEY\"\nsecret_project = \"proj\"\nsecret_name = \"ds-secret\"\n",
        )
        .unwrap();
        let entry = broker.permitted("DS_KEY").unwrap();
        assert_eq!(entry.secret_name, "ds-secret");
        assert_eq!(entry.policy, ManifestPolicy::Optional);
        assert!(broker.permitted_write("DS_KEY").is_none(), "never writable");
        assert!(broker
            .live_manifest_text()
            .unwrap()
            .contains("DS_KEY proj ds-secret optional\n"));

        // A provider key never shadows a manifest line.
        std::fs::write(
            &registry,
            "[[provider]]\nname = \"ds\"\nbase_url = \"http://l/v1\"\nwire_api = \"chat\"\nenv_key = \"STACK\"\nsecret_project = \"other\"\nsecret_name = \"other\"\n",
        )
        .unwrap();
        assert_eq!(broker.permitted("STACK").unwrap().secret_name, "stack");

        // Unregistered again, refused again; a broken registry registers nothing.
        std::fs::write(&registry, "not toml [").unwrap();
        assert!(broker.providers().is_empty());
        assert!(broker.permitted("DS_KEY").is_none());
    }

    #[test]
    fn a_provider_whose_env_key_is_a_manifest_var_is_dropped_whole() {
        let dir = tempfile::tempdir().unwrap();
        let registry = dir.path().join("providers.toml");
        let file = dir.path().join("manifest");
        std::fs::write(&file, "LIVE proj live ondemand\n").unwrap();
        let broker = broker_with("STACK proj stack ondemand\n", "127.0.0.1:8910")
            .with_manifest_file(Some(file))
            .with_providers_file(Some(registry.clone()));
        let entry = |name: &str, var: &str| {
            format!(
                "[[provider]]\nname = \"{name}\"\nbase_url = \"http://evil/v1\"\nwire_api = \"chat\"\nenv_key = \"{var}\"\nsecret_project = \"proj\"\nsecret_name = \"{name}-secret\"\n"
            )
        };
        // One provider clashes with a deploy-time VAR, one with a manifest
        // file VAR, and one is clean.
        std::fs::write(
            &registry,
            entry("a", "STACK") + &entry("b", "LIVE") + &entry("c", "C_KEY"),
        )
        .unwrap();

        // The clashing providers are gone: no Codex table, so neither VAR
        // can be pointed at the registry's base_url.
        let names: Vec<_> = broker.providers().into_iter().map(|p| p.name).collect();
        assert_eq!(names, ["c"]);
        let wrapped = vec!["codex".to_string()];
        let argv = crate::agent_cli::with_codex_providers(&wrapped, &broker.providers()).unwrap();
        assert!(!argv
            .iter()
            .any(|a| a.contains("STACK") || a.contains("LIVE")));

        // And the manifest's lines keep their own secret and policy.
        for (var, secret) in [("STACK", "stack"), ("LIVE", "live")] {
            let kept = broker.permitted(var).unwrap();
            assert_eq!(kept.secret_name, secret);
            assert_eq!(kept.policy, ManifestPolicy::OnDemand);
        }
        assert_eq!(broker.manifest().len(), 3);
    }

    #[test]
    fn without_a_manifest_file_the_helper_keeps_the_engines_variable() {
        let broker = broker_with("A proj a\n", "127.0.0.1:8910");
        assert!(broker.live_manifest_text().is_none());
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

    /// A credentials file as Claude writes it, with `expiry` in ms. Each
    /// expiry is its own refresh, so it carries its own access token.
    fn credentials(expiry: u64) -> Vec<u8> {
        credentials_with("SECRET-ACCESS", "SECRET-REFRESH", expiry)
    }

    fn credentials_with(access: &str, refresh: &str, expiry: u64) -> Vec<u8> {
        format!(
            r#"{{"claudeAiOauth":{{"accessToken":"{access}-{expiry}","refreshToken":"{refresh}","expiresAt":{expiry}}}}}"#
        )
        .into_bytes()
    }

    /// An expiry `hours` from now, in ms.
    fn in_hours(hours: u64) -> u64 {
        now_millis() + hours * 60 * 60 * 1000
    }

    fn credential_broker(dir: &FsPath) -> SecretBroker {
        let mut dirs = HashMap::new();
        dirs.insert("acct".to_string(), dir.to_path_buf());
        broker_with("", "127.0.0.1:1").with_account_dirs(dirs)
    }

    fn store_credentials(broker: &SecretBroker, bytes: &[u8]) -> Result<(), BundleRefusal> {
        broker.store_bundle_file(
            "acct",
            ".credentials.json",
            BundleFileKind::Credentials,
            bytes,
            None,
        )
    }

    #[test]
    fn a_maximum_expiry_cannot_pin_the_shared_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let broker = credential_broker(dir.path());
        store_credentials(&broker, &credentials(in_hours(1))).unwrap();
        for poison in [u64::MAX, 253_402_300_799_000, in_hours(24 * 30)] {
            assert!(
                matches!(
                    store_credentials(&broker, &credentials(poison)),
                    Err(BundleRefusal::Credentials(
                        CredentialRejection::ExpiryTooFar
                    ))
                ),
                "{poison} must be refused"
            );
        }
        // The poisoned writes changed nothing, so a genuine refresh still lands.
        let genuine = credentials(in_hours(2));
        store_credentials(&broker, &genuine).unwrap();
        assert_eq!(
            std::fs::read(dir.path().join(".credentials.json")).unwrap(),
            genuine
        );
    }

    #[test]
    fn a_stored_expiry_past_the_ceiling_does_not_outrank_a_genuine_refresh() {
        let dir = tempfile::tempdir().unwrap();
        // What an engine without the ceiling would have accepted.
        std::fs::write(
            dir.path().join(".credentials.json"),
            credentials(u64::MAX / 2),
        )
        .unwrap();
        let broker = credential_broker(dir.path());
        let genuine = credentials(in_hours(1));
        store_credentials(&broker, &genuine).unwrap();
        assert_eq!(
            std::fs::read(dir.path().join(".credentials.json")).unwrap(),
            genuine
        );
    }

    #[test]
    fn the_expiry_ceiling_is_configurable() {
        let dir = tempfile::tempdir().unwrap();
        let broker =
            credential_broker(dir.path()).with_credential_max_lifetime(Duration::from_secs(3600));
        assert!(matches!(
            store_credentials(&broker, &credentials(in_hours(2))),
            Err(BundleRefusal::Credentials(
                CredentialRejection::ExpiryTooFar
            ))
        ));
        store_credentials(&broker, &credentials(now_millis() + 30 * 60 * 1000)).unwrap();
    }

    #[test]
    fn credentials_without_both_tokens_and_an_expiry_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let broker = credential_broker(dir.path());
        let expiry = in_hours(1);
        let cases: [(String, CredentialRejection); 8] = [
            ("not json".into(), CredentialRejection::NotJson),
            ("{}".into(), CredentialRejection::MissingAccessToken),
            (
                format!(r#"{{"claudeAiOauth":{{"refreshToken":"r","expiresAt":{expiry}}}}}"#),
                CredentialRejection::MissingAccessToken,
            ),
            (
                format!(
                    r#"{{"claudeAiOauth":{{"accessToken":"","refreshToken":"r","expiresAt":{expiry}}}}}"#
                ),
                CredentialRejection::MissingAccessToken,
            ),
            (
                format!(r#"{{"claudeAiOauth":{{"accessToken":"a","expiresAt":{expiry}}}}}"#),
                CredentialRejection::MissingRefreshToken,
            ),
            (
                format!(
                    r#"{{"claudeAiOauth":{{"accessToken":"a","refreshToken":"  ","expiresAt":{expiry}}}}}"#
                ),
                CredentialRejection::MissingRefreshToken,
            ),
            (
                r#"{"claudeAiOauth":{"accessToken":"a","refreshToken":"r"}}"#.into(),
                CredentialRejection::MissingExpiry,
            ),
            (
                r#"{"claudeAiOauth":{"accessToken":"a","refreshToken":"r","expiresAt":0}}"#.into(),
                CredentialRejection::MissingExpiry,
            ),
        ];
        for (body, why) in cases {
            assert!(
                matches!(
                    store_credentials(&broker, body.as_bytes()),
                    Err(BundleRefusal::Credentials(got)) if got == why
                ),
                "{body} should be refused as {why:?}"
            );
        }
        assert!(!dir.path().join(".credentials.json").exists());
    }

    #[test]
    fn a_genuine_newer_refresh_replaces_the_stored_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let broker = credential_broker(dir.path());
        store_credentials(&broker, &credentials(in_hours(1))).unwrap();
        let refreshed = credentials(in_hours(8));
        store_credentials(&broker, &refreshed).unwrap();
        assert_eq!(
            std::fs::read(dir.path().join(".credentials.json")).unwrap(),
            refreshed
        );
    }

    #[test]
    fn an_expiry_bump_on_the_stored_access_token_is_not_a_refresh() {
        let dir = tempfile::tempdir().unwrap();
        let broker = credential_broker(dir.path());
        let stored = credentials(in_hours(1));
        store_credentials(&broker, &stored).unwrap();
        // The same tokens under an expiry that outranks any honest refresh.
        let bump = format!(
            r#"{{"claudeAiOauth":{{"accessToken":"SECRET-ACCESS-{}","refreshToken":"SECRET-REFRESH","expiresAt":{}}}}}"#,
            oauth_expiry(&stored),
            in_hours(8),
        );
        assert!(matches!(
            store_credentials(&broker, bump.as_bytes()),
            Err(BundleRefusal::UnchangedCredentials)
        ));
        assert_eq!(
            std::fs::read(dir.path().join(".credentials.json")).unwrap(),
            stored
        );
        // A rotated refresh token behind the same access token is no better.
        let rotated = format!(
            r#"{{"claudeAiOauth":{{"accessToken":"SECRET-ACCESS-{}","refreshToken":"SECRET-OTHER","expiresAt":{}}}}}"#,
            oauth_expiry(&stored),
            in_hours(8),
        );
        assert!(matches!(
            store_credentials(&broker, rotated.as_bytes()),
            Err(BundleRefusal::UnchangedCredentials)
        ));
        // A genuine refresh (new access token) still lands afterwards.
        store_credentials(&broker, &credentials(in_hours(8))).unwrap();
    }

    #[test]
    fn credentials_that_are_already_expired_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let broker = credential_broker(dir.path());
        assert!(matches!(
            store_credentials(&broker, &credentials(now_millis().saturating_sub(1000))),
            Err(BundleRefusal::Credentials(
                CredentialRejection::AlreadyExpired
            ))
        ));
        assert!(!dir.path().join(".credentials.json").exists());
    }

    #[test]
    fn a_session_has_a_bounded_number_of_credential_writes() {
        let broker = broker_with("", "127.0.0.1:1");
        let (busy, quiet) = (Uuid::new_v4(), Uuid::new_v4());
        let start = Instant::now();
        for n in 0..CREDENTIAL_WRITES_PER_WINDOW {
            assert!(broker.reserve_credential_write(busy, start + Duration::from_secs(n as u64)));
        }
        assert!(
            !broker.reserve_credential_write(busy, start + Duration::from_secs(60)),
            "the window is used up"
        );
        // Another session has its own allowance.
        assert!(broker.reserve_credential_write(quiet, start));
        // A refused write gives its claim back, so only landed writes count.
        let at = start + Duration::from_secs(2 * 60 * 60);
        broker.release_credential_write(busy, start);
        assert!(broker.reserve_credential_write(busy, at));
        assert!(!broker.reserve_credential_write(busy, at + Duration::from_secs(1)));
        broker.release_credential_write(busy, at);
        assert!(broker.reserve_credential_write(busy, at + Duration::from_secs(2)));
        // The allowance returns as writes age out of the window.
        let later = start + CREDENTIAL_WRITE_WINDOW + Duration::from_secs(120);
        assert!(broker.reserve_credential_write(busy, later));
        // A revoked session starts clean.
        broker.revoke(busy);
        for n in 0..CREDENTIAL_WRITES_PER_WINDOW {
            assert!(broker.reserve_credential_write(busy, later + Duration::from_secs(n as u64)));
        }
    }

    #[test]
    fn only_credential_writes_that_land_use_up_the_session_allowance() {
        let dir = tempfile::tempdir().unwrap();
        let broker = credential_broker(dir.path());
        let session = Uuid::new_v4();
        let now = Instant::now();
        let write = |bytes: &[u8], at: Instant| {
            broker.store_bundle_file_for(
                (session, at),
                "acct",
                ".credentials.json",
                BundleFileKind::Credentials,
                bytes,
                None,
            )
        };
        // Refusals (a bump, a dead login) do not spend the allowance.
        let first = credentials(in_hours(1));
        write(&first, now).unwrap();
        for _ in 0..2 * CREDENTIAL_WRITES_PER_WINDOW {
            assert!(write(&first, now).is_err());
        }
        // The rest of the allowance is spent on refreshes that land...
        for hours in 2..=CREDENTIAL_WRITES_PER_WINDOW as u64 {
            write(&credentials(in_hours(hours)), now).unwrap();
        }
        // ...and the next genuine-looking one is refused as the limit.
        let before = std::fs::read(dir.path().join(".credentials.json")).unwrap();
        assert!(matches!(
            write(&credentials(in_hours(9)), now),
            Err(BundleRefusal::CredentialWriteLimit)
        ));
        assert_eq!(
            std::fs::read(dir.path().join(".credentials.json")).unwrap(),
            before
        );
        // It lands again once the window has passed.
        write(
            &credentials(in_hours(9)),
            now + CREDENTIAL_WRITE_WINDOW + Duration::from_secs(1),
        )
        .unwrap();
    }

    #[test]
    fn the_credential_write_limit_and_a_bump_are_audited_refusals() {
        let session = Uuid::new_v4();
        for (error, status, reason) in [
            (
                BundleRefusal::CredentialWriteLimit,
                StatusCode::TOO_MANY_REQUESTS,
                "credential write limit",
            ),
            (
                BundleRefusal::UnchangedCredentials,
                StatusCode::FORBIDDEN,
                "keep the stored access token",
            ),
            (
                BundleRefusal::Credentials(CredentialRejection::AlreadyExpired),
                StatusCode::FORBIDDEN,
                "already expired",
            ),
        ] {
            let log = audited(|| {
                let response = refuse_bundle(session, "acct", ".credentials.json", error);
                assert_eq!(response.status(), status);
            });
            assert!(log.contains(&session.to_string()), "session in: {log}");
            assert!(log.contains(reason), "{reason:?} in: {log}");
            assert!(log.contains("vogt::audit"), "audit target in: {log}");
        }
    }

    /// Collects what the audit log writes, so a test can read it back.
    #[derive(Clone, Default)]
    struct AuditCapture(Arc<Mutex<Vec<u8>>>);

    impl Write for AuditCapture {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for AuditCapture {
        type Writer = AuditCapture;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    fn audited(f: impl FnOnce()) -> String {
        let capture = AuditCapture::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(capture.clone())
            .with_ansi(false)
            .finish();
        tracing::subscriber::with_default(subscriber, f);
        let bytes = capture.0.lock().unwrap().clone();
        String::from_utf8(bytes).unwrap()
    }

    #[test]
    fn every_credential_refusal_is_audited_by_session_account_and_reason() {
        let dir = tempfile::tempdir().unwrap();
        let broker = credential_broker(dir.path());
        store_credentials(&broker, &credentials(in_hours(2))).unwrap();
        let session = Uuid::new_v4();
        let refused = [
            (credentials(u64::MAX), "beyond the token lifetime ceiling"),
            (
                br#"{"claudeAiOauth":{"accessToken":"SECRET-ACCESS","expiresAt":1}}"#.to_vec(),
                "no refresh token",
            ),
            (
                br#"{"claudeAiOauth":{"refreshToken":"SECRET-REFRESH","expiresAt":1}}"#.to_vec(),
                "no access token",
            ),
            (b"SECRET-ACCESS".to_vec(), "not valid JSON"),
            (credentials(in_hours(1)), "not newer than the stored"),
        ];
        for (body, reason) in refused {
            let log = audited(|| {
                let error = store_credentials(&broker, &body).unwrap_err();
                let response = refuse_bundle(session, "acct", ".credentials.json", error);
                assert_eq!(response.status(), StatusCode::FORBIDDEN);
            });
            assert!(log.contains(&session.to_string()), "session in: {log}");
            assert!(log.contains("acct"), "account in: {log}");
            assert!(log.contains(reason), "{reason:?} in: {log}");
            assert!(log.contains("vogt::audit"), "audit target in: {log}");
            assert!(!log.contains("SECRET-"), "no value in: {log}");
        }
    }

    #[test]
    fn a_config_write_needs_a_version_and_an_honest_one() {
        let dir = tempfile::tempdir().unwrap();
        let broker = credential_broker(dir.path());
        let write = |broker: &SecretBroker, version: Option<u64>| {
            broker.store_bundle_file(
                "acct",
                ".claude.json",
                BundleFileKind::Config,
                b"{}",
                version,
            )
        };
        assert!(matches!(
            write(&broker, None),
            Err(BundleRefusal::ConfigNeedsVersion)
        ));
        assert!(matches!(
            write(&broker, Some(now_millis() + 60 * 60 * 1000)),
            Err(BundleRefusal::ConfigVersionAhead)
        ));
        assert!(!dir.path().join(".claude.json").exists());
        write(&broker, Some(now_millis() - 5000)).unwrap();
        assert!(matches!(
            write(&broker, Some(now_millis() - 6000)),
            Err(BundleRefusal::OlderConfig)
        ));
        write(&broker, Some(now_millis() - 4000)).unwrap();
    }

    #[test]
    fn the_config_version_survives_an_engine_restart() {
        let dir = tempfile::tempdir().unwrap();
        let version = now_millis() - 1000;
        let write = |broker: &SecretBroker, version: u64| {
            broker.store_bundle_file(
                "acct",
                ".claude.json",
                BundleFileKind::Config,
                b"{}",
                Some(version),
            )
        };
        write(&credential_broker(dir.path()), version).unwrap();
        // A new broker has no memory of the write: only the disk remembers.
        let restarted = credential_broker(dir.path());
        assert!(matches!(
            write(&restarted, version - 1),
            Err(BundleRefusal::OlderConfig)
        ));
        assert!(matches!(
            write(&restarted, version),
            Err(BundleRefusal::OlderConfig)
        ));
        write(&restarted, version + 1).unwrap();
    }

    #[test]
    fn the_bundle_body_bound_fits_a_full_delta_and_no_more() {
        let full = MAX_TRANSCRIPT_DELTA_BYTES.div_ceil(3) * 4;
        assert!(MAX_BUNDLE_BODY_BYTES > full);
        assert!(MAX_BUNDLE_BODY_BYTES < full + 64 * 1024);
        assert_eq!(MAX_BUNDLE_REQUEST_BYTES, MAX_TRANSCRIPT_DELTA_BYTES);
    }

    #[cfg(unix)]
    #[test]
    fn bundle_writes_are_private_and_newer_credentials_only() {
        let dir = tempfile::tempdir().unwrap();
        let mut dirs = HashMap::new();
        dirs.insert("acct".to_string(), dir.path().to_path_buf());
        let broker = broker_with("", "127.0.0.1:1").with_account_dirs(dirs);
        let newer = &credentials(in_hours(2));
        let older = &credentials(in_hours(1));
        broker
            .store_bundle_file(
                "acct",
                ".credentials.json",
                BundleFileKind::Credentials,
                newer,
                None,
            )
            .unwrap();
        let mode = std::fs::metadata(dir.path().join(".credentials.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
        assert!(matches!(
            broker.store_bundle_file(
                "acct",
                ".credentials.json",
                BundleFileKind::Credentials,
                older,
                None,
            ),
            Err(BundleRefusal::OlderCredentials)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn bundle_store_creates_a_private_account_directory_before_comparing() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("new-account");
        let mut dirs = HashMap::new();
        dirs.insert("acct".to_string(), root.clone());
        let broker = broker_with("", "127.0.0.1:1").with_account_dirs(dirs);

        broker
            .store_bundle_file(
                "acct",
                ".credentials.json",
                BundleFileKind::Credentials,
                &credentials(in_hours(1)),
                None,
            )
            .unwrap();

        assert_eq!(
            std::fs::metadata(&root).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert!(root.join(".credentials.json").is_file());
    }

    #[test]
    fn concurrent_credential_stores_keep_the_newest_expiry() {
        let dir = tempfile::tempdir().unwrap();
        let mut dirs = HashMap::new();
        dirs.insert("acct".to_string(), dir.path().to_path_buf());
        let broker = Arc::new(broker_with("", "127.0.0.1:1").with_account_dirs(dirs));
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let mut threads = Vec::new();
        let (soon, later) = (in_hours(1), in_hours(2));
        for expiry in [soon, later] {
            let broker = Arc::clone(&broker);
            let barrier = Arc::clone(&barrier);
            threads.push(std::thread::spawn(move || {
                let bytes = credentials(expiry);
                barrier.wait();
                broker.store_bundle_file(
                    "acct",
                    ".credentials.json",
                    BundleFileKind::Credentials,
                    &bytes,
                    None,
                )
            }));
        }
        barrier.wait();
        let outcomes: Vec<_> = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect();

        assert!(outcomes.iter().any(Result::is_ok));
        let stored = std::fs::read(dir.path().join(".credentials.json")).unwrap();
        assert_eq!(oauth_expiry(&stored), later);
    }

    fn transcript_broker(dir: &FsPath) -> SecretBroker {
        let mut dirs = HashMap::new();
        dirs.insert("acct".to_string(), dir.to_path_buf());
        broker_with("", "127.0.0.1:1").with_account_dirs(dirs)
    }

    const TRANSCRIPT: &str = "projects/work/conv.jsonl";

    fn append(broker: &SecretBroker, offset: u64, bytes: &[u8]) -> Result<u64, BundleRefusal> {
        broker.append_transcript("acct", "conv", TRANSCRIPT, offset, bytes)
    }

    #[test]
    fn a_transcript_past_sixteen_mib_grows_in_chunks() {
        let dir = tempfile::tempdir().unwrap();
        let broker = transcript_broker(dir.path());
        let chunk = vec![b'x'; MAX_TRANSCRIPT_DELTA_BYTES];
        let mut offset = 0u64;
        for _ in 0..20 {
            offset = append(&broker, offset, &chunk).unwrap();
        }
        offset = append(&broker, offset, b"tail\n").unwrap();
        let expected = 20 * MAX_TRANSCRIPT_DELTA_BYTES as u64 + 5;
        assert!(expected > MAX_BUNDLE_REQUEST_BYTES as u64);
        assert_eq!(offset, expected);
        assert_eq!(
            std::fs::metadata(dir.path().join(TRANSCRIPT))
                .unwrap()
                .len(),
            expected
        );
        let cached = broker
            .bundle
            .get(&("acct".into(), TRANSCRIPT.into()))
            .unwrap()
            .len();
        assert_eq!(cached, MAX_IN_MEMORY_TRANSCRIPT);
    }

    #[test]
    fn a_delta_over_the_request_bound_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let broker = transcript_broker(dir.path());
        let big = vec![b'x'; MAX_TRANSCRIPT_DELTA_BYTES + 1];
        assert!(matches!(
            append(&broker, 0, &big),
            Err(BundleRefusal::TooLarge)
        ));
    }

    #[test]
    fn an_offset_mismatch_asks_for_a_resync_from_the_engine_length() {
        let dir = tempfile::tempdir().unwrap();
        let broker = transcript_broker(dir.path());
        assert_eq!(append(&broker, 0, b"one\n").unwrap(), 4);
        // Ahead of the engine: a gap the agent must fill.
        assert!(matches!(
            append(&broker, 10, b"gap\n"),
            Err(BundleRefusal::Resync { offset: 4 })
        ));
        // Behind the engine but not the same bytes: a rewrite, not a retry.
        assert!(matches!(
            append(&broker, 0, b"two\n"),
            Err(BundleRefusal::Resync { offset: 4 })
        ));
        // Resending from the reported offset lands.
        assert_eq!(append(&broker, 4, b"two\n").unwrap(), 8);
        assert_eq!(
            std::fs::read(dir.path().join(TRANSCRIPT)).unwrap(),
            b"one\ntwo\n"
        );
    }

    #[test]
    fn a_repeated_delta_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let broker = transcript_broker(dir.path());
        assert_eq!(append(&broker, 0, b"one\n").unwrap(), 4);
        assert_eq!(append(&broker, 0, b"one\n").unwrap(), 4);
        assert_eq!(
            std::fs::read(dir.path().join(TRANSCRIPT)).unwrap(),
            b"one\n"
        );
    }

    #[test]
    fn concurrent_duplicate_deltas_land_once() {
        let dir = tempfile::tempdir().unwrap();
        let broker = Arc::new(transcript_broker(dir.path()));
        let barrier = Arc::new(std::sync::Barrier::new(4));
        let threads: Vec<_> = (0..4)
            .map(|_| {
                let broker = Arc::clone(&broker);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    append(&broker, 0, b"same delta\n")
                })
            })
            .collect();
        for thread in threads {
            assert_eq!(thread.join().unwrap().unwrap(), 11);
        }
        assert_eq!(
            std::fs::read(dir.path().join(TRANSCRIPT)).unwrap(),
            b"same delta\n"
        );
    }

    #[test]
    fn a_transcript_outside_the_conversation_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let broker = transcript_broker(dir.path());
        assert!(matches!(
            broker.append_transcript("acct", "conv", "projects/work/other.jsonl", 0, b"x"),
            Err(BundleRefusal::ConversationScope)
        ));
    }

    #[test]
    fn the_whole_file_path_refuses_transcripts() {
        let dir = tempfile::tempdir().unwrap();
        let broker = transcript_broker(dir.path());
        assert!(matches!(
            broker.store_bundle_file("acct", TRANSCRIPT, BundleFileKind::Transcript, b"x", None),
            Err(BundleRefusal::TranscriptNeedsOffset)
        ));
    }

    const CODEX_ROLLOUT: &str = "sessions/2026/11/09/rollout-2026-11-09T21-00-00-conv.jsonl";

    fn codex_broker(home: &FsPath) -> SecretBroker {
        broker_with("", "127.0.0.1:1").with_codex_home(Some(home.to_path_buf()))
    }

    #[test]
    fn a_codex_rollout_lands_under_the_codex_home() {
        let home = tempfile::tempdir().unwrap();
        let broker = codex_broker(home.path());
        // An account is not required: the rollout belongs to the conversation.
        assert_eq!(
            broker
                .append_transcript("", "conv", CODEX_ROLLOUT, 0, b"one\n")
                .unwrap(),
            4
        );
        assert_eq!(
            broker
                .append_transcript("", "conv", CODEX_ROLLOUT, 4, b"two\n")
                .unwrap(),
            8
        );
        assert_eq!(
            std::fs::read(home.path().join(CODEX_ROLLOUT)).unwrap(),
            b"one\ntwo\n"
        );
        assert_eq!(
            broker.bundle_snapshot("", "conv"),
            vec![(CODEX_ROLLOUT.to_string(), b"one\ntwo\n".to_vec())]
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode =
                |path: &FsPath| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&home.path().join("sessions")), 0o700);
            assert_eq!(mode(&home.path().join(CODEX_ROLLOUT)), 0o600);
        }
    }

    #[test]
    fn codex_paths_outside_the_contract_are_refused() {
        let home = tempfile::tempdir().unwrap();
        let broker = codex_broker(home.path());
        for path in [
            // Traversal, in the date and in the filename.
            "sessions/2026/11/09/../rollout-x-conv.jsonl",
            "sessions/../../11/09/rollout-x-conv.jsonl",
            "sessions/2026/11/09/../conv.jsonl",
            "sessions/2026/11/09/rollout-x\\-conv.jsonl",
            // The wrong conversation, and a filename that is not a rollout.
            "sessions/2026/11/09/rollout-x-other.jsonl",
            "sessions/2026/11/09/conv.jsonl",
            "sessions/2026/11/09/rollout-conv.jsonl",
            // The wrong prefix, so it must not fall into the account bundle.
            "threads/2026/11/09/rollout-x-conv.jsonl",
            "projects/2026/11/09/rollout-x-conv.jsonl",
            // A date that is not the contract's four/two/two digits.
            "sessions/26/11/09/rollout-x-conv.jsonl",
            "sessions/2026/13/09/rollout-x-conv.jsonl",
            "sessions/2026/11/32/rollout-x-conv.jsonl",
        ] {
            assert!(
                matches!(
                    broker.append_transcript("", "conv", path, 0, b"x"),
                    Err(BundleRefusal::ConversationScope)
                ),
                "{path} was not refused"
            );
        }
        assert!(
            !home.path().join("sessions").exists(),
            "a refused path wrote something"
        );
    }

    #[test]
    fn codex_writeback_without_a_codex_home_is_refused() {
        let broker = broker_with("", "127.0.0.1:1");
        assert!(matches!(
            broker.append_transcript("", "conv", CODEX_ROLLOUT, 0, b"x"),
            Err(BundleRefusal::Storage)
        ));
    }

    #[test]
    fn the_bundle_snapshot_returns_only_the_conversations_newest_codex_rollout() {
        let home = tempfile::tempdir().unwrap();
        let broker = codex_broker(home.path());
        let old = "sessions/2026/11/08/rollout-2026-11-08T09-00-00-conv.jsonl";
        let new = CODEX_ROLLOUT;
        std::fs::create_dir_all(home.path().join("sessions/2026/11/08")).unwrap();
        std::fs::write(home.path().join(old), b"old\n").unwrap();
        broker
            .append_transcript("", "conv", new, 0, b"new\n")
            .unwrap();
        assert_eq!(
            broker.bundle_snapshot("", "conv"),
            vec![(new.to_string(), b"new\n".to_vec())]
        );
    }

    #[cfg(unix)]
    #[test]
    fn bundle_files_and_dirs_are_private_when_created() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("fresh/account");
        let broker = transcript_broker(&root);
        append(&broker, 0, b"entry\n").unwrap();
        broker
            .store_bundle_file(
                "acct",
                ".credentials.json",
                BundleFileKind::Credentials,
                &credentials(in_hours(1)),
                None,
            )
            .unwrap();
        let mode = |p: &FsPath| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&root), 0o700);
        assert_eq!(mode(&root.join("projects")), 0o700);
        assert_eq!(mode(&root.join("projects/work")), 0o700);
        assert_eq!(mode(&root.join(TRANSCRIPT)), 0o600);
        assert_eq!(mode(&root.join(".credentials.json")), 0o600);
        // The ancestors created on the way are private too, with no chmod after.
        assert_eq!(mode(&parent.path().join("fresh")), 0o700);
    }

    #[cfg(unix)]
    #[test]
    fn creation_modes_hold_under_a_permissive_umask() {
        let dir = tempfile::tempdir().unwrap();
        let old = unsafe { libc::umask(0) };
        let made = dir.path().join("made");
        create_private_dir(&made).unwrap();
        atomic_bundle_write(dir.path(), FsPath::new("file.json"), b"{}").unwrap();
        unsafe { libc::umask(old) };
        let mode = |p: &FsPath| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&made), 0o700);
        assert_eq!(mode(&dir.path().join("file.json")), 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn transcript_rejects_a_symlinked_project_directory() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("projects")).unwrap();
        symlink(outside.path(), dir.path().join("projects/work")).unwrap();
        let broker = transcript_broker(dir.path());

        assert!(matches!(
            append(&broker, 0, b"entry\n"),
            Err(BundleRefusal::Storage)
        ));
        assert!(!outside.path().join("conv.jsonl").exists());
    }

    #[cfg(unix)]
    #[test]
    fn transcript_rejects_a_symlinked_file() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("projects/work")).unwrap();
        symlink(outside.path().join("target"), dir.path().join(TRANSCRIPT)).unwrap();
        let broker = transcript_broker(dir.path());
        assert!(matches!(
            append(&broker, 0, b"entry\n"),
            Err(BundleRefusal::Storage)
        ));
        assert!(!outside.path().join("target").exists());
    }
}
