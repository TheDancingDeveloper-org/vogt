use std::{
    collections::{HashMap, VecDeque},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use axum::{
    extract::{Request, State},
    http::{header, HeaderMap, HeaderName, HeaderValue, Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::json;
use subtle::ConstantTimeEq;
use uuid::Uuid;

use crate::app::AppState;
use crate::core_auth::{CoreIdentity, ResolveError};
use crate::observability::RequestId;

static AUTH_FAILURES: AtomicU64 = AtomicU64::new(0);
const RATE_LIMIT_WINDOW: Duration = Duration::from_secs(60);
/// The name the optional static `ENGINE_TOKEN` authenticates as. A
/// break-glass credential: full capability, no actor of its own at the
/// core, so its Vogt reads and writes are attributed to the stack secret's
/// actor. Deployments that give every person a login never set it.
pub const PRIMARY_TOKEN_NAME: &str = "primary";
/// The name the stack secret authenticates as. It is the core's own
/// credential — what `vogt-core` presents when it starts a session on this
/// engine — so it holds exactly what the core needs and nothing a person
/// would.
pub const STACK_SECRET_NAME: &str = "vogt-core";

/// Header we attach to every response so operators can correlate audit log
/// lines with a specific request. Echoes back an incoming `X-Request-Id` if
/// present, otherwise mints a fresh one.
static REQUEST_ID_HEADER: HeaderName = HeaderName::from_static("x-request-id");
static RETRY_AFTER_HEADER: HeaderName = HeaderName::from_static("retry-after");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TokenCapability {
    Sessions,
    FilesystemWrite,
    GitWrite,
    GuiControl,
    AgentTasksWrite,
    PushWrite,
    HistoryWrite,
    /// Reading the archived session history (`GET /api/history/*`) — every
    /// past session's complete output, which routinely contains pasted
    /// secrets. Gated even though it is a GET, for the same reason
    /// `assistant` gates the durable interaction log: it is a cross-session
    /// record, not the caller's own live transcript, so "any valid token"
    /// is too broad a grant.
    History,
    Assistant,
    /// Writing to vogt-core through the front door (`/api/vogt`). Reads need
    /// only a valid token; a write needs to have been granted this, because
    /// what it changes is the estate's declared state and not this pod's.
    VogtWrite,
    /// Changing which version of an agent CLI new sessions run
    /// (`POST /api/agent-clis/{tool}`). A deploy-time decision made
    /// from a running pod: it runs the installer, which downloads and
    /// executes a package from npm, so it is an operator grant and not a
    /// thing every token that can open a session may do.
    AgentClisWrite,
}

pub const ALL_CAPABILITIES: [TokenCapability; 11] = [
    TokenCapability::Sessions,
    TokenCapability::FilesystemWrite,
    TokenCapability::GitWrite,
    TokenCapability::GuiControl,
    TokenCapability::AgentTasksWrite,
    TokenCapability::PushWrite,
    TokenCapability::HistoryWrite,
    TokenCapability::History,
    TokenCapability::Assistant,
    TokenCapability::VogtWrite,
    TokenCapability::AgentClisWrite,
];

/// What a core scope set is worth on this engine.
///
/// The engine has no token table, so it has no per-token capability list
/// either; what a caller may do here follows from what the core says they
/// may do there. The mapping is deliberately coarse and deliberately
/// documented in one place (`docs/ENGINE.md` §5):
///
/// - `admin` → everything, including the two operator grants (`gui-control`,
///   `agent-clis-write`) that amount to running arbitrary software in the pod
///   on a deploy-time decision.
/// - `work.write` or `project.write` → the interactive set: sessions, files,
///   git, agent tasks, push, history, the assistant, and Vogt writes. A person
///   trusted to change the estate's declared state is trusted to drive the
///   pod that holds its checkouts.
/// - `read` alone → `push-write` only, so a read-only device can still
///   subscribe to notifications. Reads on ungated routes need no capability.
/// - `writeback` adds nothing here; it is a core-side grant.
pub fn capabilities_for_scopes(scopes: &[String]) -> Vec<TokenCapability> {
    if scopes.iter().any(|s| s == "admin") {
        return ALL_CAPABILITIES.to_vec();
    }
    if scopes
        .iter()
        .any(|s| s == "work.write" || s == "project.write")
    {
        return vec![
            TokenCapability::Sessions,
            TokenCapability::FilesystemWrite,
            TokenCapability::GitWrite,
            TokenCapability::AgentTasksWrite,
            TokenCapability::PushWrite,
            TokenCapability::HistoryWrite,
            TokenCapability::History,
            TokenCapability::Assistant,
            TokenCapability::VogtWrite,
        ];
    }
    if scopes.iter().any(|s| s == "read") {
        return vec![TokenCapability::PushWrite];
    }
    Vec::new()
}

/// What the stack secret — the core's own credential — may do here.
const STACK_SECRET_CAPABILITIES: [TokenCapability; 2] =
    [TokenCapability::Sessions, TokenCapability::AgentClisWrite];

pub struct AuthRuntime {
    mutation_windows: Mutex<HashMap<String, VecDeque<Instant>>>,
}

impl Default for AuthRuntime {
    fn default() -> Self {
        Self {
            mutation_windows: Mutex::new(HashMap::new()),
        }
    }
}

impl AuthRuntime {
    pub fn check_mutation_rate_limit(
        &self,
        token_name: &str,
        limit_per_minute: u32,
    ) -> Option<Duration> {
        if limit_per_minute == 0 {
            return None;
        }

        let now = Instant::now();
        let mut windows = self.mutation_windows.lock();
        let window = windows.entry(token_name.to_string()).or_default();
        while let Some(oldest) = window.front().copied() {
            if now.duration_since(oldest) >= RATE_LIMIT_WINDOW {
                window.pop_front();
            } else {
                break;
            }
        }
        if window.len() >= limit_per_minute as usize {
            let retry_after = window
                .front()
                .map(|oldest| RATE_LIMIT_WINDOW.saturating_sub(now.duration_since(*oldest)))
                .unwrap_or(RATE_LIMIT_WINDOW);
            return Some(retry_after);
        }
        window.push_back(now);
        None
    }
}

/// Who the gate decided this request is, handed on to the handler behind it.
///
/// `require_bearer` authorizes and would otherwise discard who it resolved,
/// leaving every handler downstream unable to tell one caller from another.
/// A request extension is the mechanism because it is per-request state that
/// only the handlers on the gated router can see — the alternative,
/// re-deriving the identity in `vogt_core::api` from the `Authorization`
/// header, would mean a second look at a secret in a second place.
#[derive(Debug, Clone)]
pub struct AuthorizedIdentity {
    /// Who this is, for refusals and audit lines: `primary`, `vogt-core`, or
    /// the core actor's `identity_ref` (`human:ada`, `agent:session:…`).
    pub name: String,
    /// What the caller may do here.
    pub capabilities: Vec<TokenCapability>,
    /// The core scopes behind that, when the core resolved the caller.
    /// Empty for the two static credentials.
    pub scopes: Vec<String>,
    /// The credential `/api/vogt` presents to the core on this caller's
    /// behalf: the caller's own bearer when the core resolved it, the stack
    /// secret for the break-glass token, `None` when there is nothing to
    /// present (a break-glass token on a door with no stack secret).
    pub core_bearer: Option<String>,
    pub mutating_requests_per_minute: u32,
}

impl AuthorizedIdentity {
    pub fn allows(&self, capability: TokenCapability) -> bool {
        self.capabilities.contains(&capability)
    }
}

/// Why a bearer was not accepted.
#[derive(Debug)]
pub enum AuthRefusal {
    /// Nobody recognises it. A 401: try a different credential.
    Unauthorized,
    /// The core — the only party who could recognise it — could not be
    /// asked. A 503 naming the outage, never a 401.
    Unavailable(String),
}

/// Resolve a bearer to an identity, or say why not.
///
/// Three kinds of credential, checked in order of cost:
///
/// 1. the optional static `ENGINE_TOKEN`, constant-time;
/// 2. the stack secret (`vogt_core_token`), constant-time — the core's own
///    identity when it calls this engine;
/// 3. anything else is a core token, and the core says whose it is
///    (`core_auth`), cached briefly.
///
/// There is no fourth kind. A front door with no core and no static token
/// refuses everything, and `config::load` refuses to boot that way.
pub async fn authorize(
    state: &AppState,
    candidate: &str,
) -> Result<AuthorizedIdentity, AuthRefusal> {
    let cfg = state.config.as_ref();
    if let Some(primary) = cfg.token.as_deref() {
        if bool::from(candidate.as_bytes().ct_eq(primary.as_bytes())) {
            return Ok(AuthorizedIdentity {
                name: PRIMARY_TOKEN_NAME.to_string(),
                capabilities: ALL_CAPABILITIES.to_vec(),
                scopes: Vec::new(),
                // The break-glass token has no actor of its own at the core;
                // it borrows the stack secret's, which is the one shared
                // identity this design keeps, and keeps for exactly this.
                core_bearer: cfg.vogt_core_token.clone(),
                mutating_requests_per_minute: cfg.token_mutating_request_limit_per_minute,
            });
        }
    }
    if let Some(secret) = cfg.vogt_core_token.as_deref() {
        if bool::from(candidate.as_bytes().ct_eq(secret.as_bytes())) {
            return Ok(AuthorizedIdentity {
                name: STACK_SECRET_NAME.to_string(),
                capabilities: STACK_SECRET_CAPABILITIES.to_vec(),
                scopes: Vec::new(),
                core_bearer: Some(secret.to_string()),
                mutating_requests_per_minute: cfg.token_mutating_request_limit_per_minute,
            });
        }
    }
    let Some(core) = state.vogt_core.as_ref() else {
        return Err(AuthRefusal::Unauthorized);
    };
    let outcome = match state.core_identities.get(candidate) {
        Some(remembered) => remembered,
        None => {
            let fresh = core.whoami(candidate).await;
            state.core_identities.put(candidate, fresh.clone());
            fresh
        }
    };
    match outcome {
        Ok(identity) => Ok(from_core(identity, candidate, cfg)),
        Err(ResolveError::Rejected) => Err(AuthRefusal::Unauthorized),
        Err(ResolveError::Unavailable(detail)) => Err(AuthRefusal::Unavailable(detail)),
    }
}

fn from_core(
    identity: CoreIdentity,
    bearer: &str,
    cfg: &crate::config::Config,
) -> AuthorizedIdentity {
    AuthorizedIdentity {
        name: identity.identity_ref,
        capabilities: capabilities_for_scopes(&identity.scopes),
        scopes: identity.scopes,
        core_bearer: Some(bearer.to_string()),
        mutating_requests_per_minute: cfg.token_mutating_request_limit_per_minute,
    }
}

/// Bearer-token gate.
///
/// Also emits an audit log entry for every mutating request (POST/PUT/PATCH/
/// DELETE) — the dev pod has the host Docker socket mounted, so we want a
/// trail of who did what. The identity the core resolved is what the line
/// names, so a session's writes are attributable to the person behind it.
// Both arms are an axum `Response` by design — the rejection *is* a response
// this middleware returns directly, not an error boxed and rethrown. clippy
// 1.98's `result_large_err` flags the large `Err`; boxing only one arm would
// make the signature asymmetric for no runtime win, so the lint is allowed here.
#[allow(clippy::result_large_err)]
pub async fn require_bearer(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    mut request: Request,
    next: Next,
) -> Result<Response, Response> {
    // The id the access-log layer already assigned this request. Taken
    // from there rather than re-derived, because two ids for one request is
    // worse than none: the audit lines below and the access line would name
    // the same request differently, and the core — which is told this id —
    // would agree with neither. The fallback keeps this middleware standing
    // alone, as it did before there was an outer layer to ask.
    let request_id = request
        .extensions()
        .get::<RequestId>()
        .map(|id| id.0.clone())
        .or_else(|| {
            headers
                .get(&REQUEST_ID_HEADER)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let user_agent = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("-");

    let method = request.method().clone();
    let path = request.uri().path().to_owned();

    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let Some(token) = token else {
        record_auth_failure(&method, &path, &request_id, "missing").await;
        return Err(refusal(
            StatusCode::UNAUTHORIZED,
            &request_id,
            None,
            "unauthorized: no bearer token",
        ));
    };

    let access = match authorize(state.as_ref(), token).await {
        Ok(access) => access,
        Err(AuthRefusal::Unauthorized) => {
            record_auth_failure(&method, &path, &request_id, "wrong-token").await;
            return Err(refusal(
                StatusCode::UNAUTHORIZED,
                &request_id,
                None,
                "unauthorized: the bearer token is not valid here",
            ));
        }
        Err(AuthRefusal::Unavailable(detail)) => {
            tracing::warn!(
                target: "vogt::audit",
                request_id = %request_id,
                method = %method,
                path = %path,
                detail = %detail,
                "could not authenticate: vogt-core unavailable"
            );
            return Err(refusal(
                StatusCode::SERVICE_UNAVAILABLE,
                &request_id,
                Some(Duration::from_secs(2)),
                &format!(
                    "vogt-core is unavailable, so this credential cannot be checked: {detail}"
                ),
            ));
        }
    };

    let is_mutating = matches!(
        method,
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    );
    if let Some(required) = required_capability(&method, &path) {
        if !access.allows(required) {
            tracing::warn!(
                target: "vogt::audit",
                request_id = %request_id,
                token_name = %access.name,
                method = %method,
                path = %path,
                required_capability = ?required,
                user_agent = user_agent,
                "capability denied"
            );
            return Err(refusal(
                StatusCode::FORBIDDEN,
                &request_id,
                None,
                &format!("forbidden: this credential lacks the {required:?} capability"),
            ));
        }
    }

    if is_mutating {
        if let Some(retry_after) = state
            .auth
            .check_mutation_rate_limit(&access.name, access.mutating_requests_per_minute)
        {
            tracing::warn!(
                target: "vogt::audit",
                request_id = %request_id,
                token_name = %access.name,
                method = %method,
                path = %path,
                retry_after_ms = retry_after.as_millis() as u64,
                user_agent = user_agent,
                "mutation rate limit exceeded"
            );
            return Err(refusal(
                StatusCode::TOO_MANY_REQUESTS,
                &request_id,
                Some(retry_after),
                "too many mutating requests; slow down",
            ));
        }
    }

    // Every check has passed, so the handler may now be told who it is
    // serving. Inserted last on purpose: a refused request never carries an
    // identity, so nothing downstream can mistake "was going to be this
    // caller" for "is this caller".
    let name = access.name.clone();
    request.extensions_mut().insert(access);

    let mut response = next.run(request).await;
    if let Ok(value) = HeaderValue::from_str(&request_id) {
        response.headers_mut().insert(&REQUEST_ID_HEADER, value);
    }

    if is_mutating {
        tracing::info!(
            target: "vogt::audit",
            request_id = %request_id,
            token_name = %name,
            method = %method,
            path = %path,
            status = response.status().as_u16(),
            user_agent = user_agent,
            "mutating request"
        );
    }

    Ok(response)
}

pub(crate) async fn record_auth_failure(
    method: &Method,
    path: &str,
    request_id: &str,
    reason: &'static str,
) {
    let count = AUTH_FAILURES.fetch_add(1, Ordering::Relaxed) + 1;
    tracing::warn!(
        target: "vogt::audit",
        request_id = %request_id,
        method = %method,
        path = %path,
        reason = reason,
        total_failures = count,
        "auth failure"
    );
    // Cheap rate-limiting: scale a delay with the running failure count so a
    // mistyped curl is unaffected but a brute-forcer slows to a crawl. Capped
    // so a transient bad client doesn't pin a worker.
    let delay_ms = (count.min(50) * 50).min(2000);
    tokio::time::sleep(Duration::from_millis(delay_ms)).await;
}

/// Route a WebSocket attach auth failure through the same failure counter,
/// escalating delay and audit line the HTTP bearer gate uses.
///
/// WS auth ran before this with no online-guessing penalty: it validates the
/// token before any session lookup, so an attacker could open sockets and
/// guess tokens at network speed. Sharing `AUTH_FAILURES` means a brute-force
/// run over either surface slows the other too.
pub async fn record_ws_auth_failure(reason: &'static str) {
    let count = AUTH_FAILURES.fetch_add(1, Ordering::Relaxed) + 1;
    tracing::warn!(
        target: "vogt::audit",
        method = "WS",
        path = "/api/sessions/{id}/attach",
        reason = reason,
        total_failures = count,
        "auth failure"
    );
    let delay_ms = (count.min(50) * 50).min(2000);
    tokio::time::sleep(Duration::from_millis(delay_ms)).await;
}

pub async fn ws_token_allows_session_access(state: &AppState, candidate: &str) -> bool {
    authorize(state, candidate)
        .await
        .map(|access| access.allows(TokenCapability::Sessions))
        .unwrap_or(false)
}

fn required_capability(method: &Method, path: &str) -> Option<TokenCapability> {
    if path == "/api/sessions" && *method == Method::POST {
        return Some(TokenCapability::Sessions);
    }
    if path.starts_with("/api/sessions/") {
        if *method == Method::PATCH || *method == Method::DELETE {
            return Some(TokenCapability::Sessions);
        }
        if *method == Method::POST && (path.ends_with("/kill") || path.ends_with("/input")) {
            return Some(TokenCapability::Sessions);
        }
        // Reading a session's detail ships its full scrollback (routinely
        // pasted secrets), so it needs the sessions capability too: a
        // zero-capability "readonly" token must not read every live session's
        // transcript. The WS attach (also a read of live output) is
        // registered outside this gate and is unaffected here.
        if *method == Method::GET {
            return Some(TokenCapability::Sessions);
        }
    }
    // Reading the durable interaction log is scope-gated even though it is a
    // GET: it is a cross-conversation record attributable to
    // each actor, not the caller's own live transcript, so it takes the
    // `assistant` capability rather than merely a valid token. The in-memory
    // `/api/assistant/history` read stays ungated, as it was.
    if path == "/api/assistant/log" && *method == Method::GET {
        return Some(TokenCapability::Assistant);
    }
    if path.starts_with("/api/assistant") && *method != Method::GET {
        return Some(TokenCapability::Assistant);
    }
    if (path == "/api/files" && *method == Method::PUT)
        || (path == "/api/files/op" && *method == Method::POST)
    {
        return Some(TokenCapability::FilesystemWrite);
    }
    if path == "/api/git/op" && *method == Method::POST {
        return Some(TokenCapability::GitWrite);
    }
    if path.starts_with("/api/agent-clis/") && *method == Method::POST {
        return Some(TokenCapability::AgentClisWrite);
    }
    if (path == "/api/gui/launch" || path == "/api/gui/kill") && *method == Method::POST {
        return Some(TokenCapability::GuiControl);
    }
    if path == "/api/agent-tasks" && *method == Method::POST {
        return Some(TokenCapability::AgentTasksWrite);
    }
    if path.starts_with("/api/agent-tasks/") && *method != Method::GET {
        return Some(TokenCapability::AgentTasksWrite);
    }
    if matches!(
        path,
        "/api/push/subscribe"
            | "/api/push/update"
            | "/api/push/unsubscribe"
            | "/api/push/test"
            | "/api/push/flush-digests"
    ) && *method == Method::POST
    {
        return Some(TokenCapability::PushWrite);
    }
    if path.starts_with("/api/history/") && (*method == Method::DELETE || *method == Method::POST) {
        return Some(TokenCapability::HistoryWrite);
    }
    // Reading the archived history — every past session's complete output — is
    // gated behind its own read capability, for the same reason the
    // assistant log read is: it is a cross-session record, not this caller's
    // own live transcript.
    if path.starts_with("/api/history") && *method == Method::GET {
        return Some(TokenCapability::History);
    }
    // Everything under the Vogt front door that is not a read. The core
    // enforces its own rules on top of this — a reason on every write, the
    // scopes on the injected token — so this gate is about which front-door
    // holders may reach the write plane at all, not about which write.
    if path.starts_with("/api/vogt") && *method != Method::GET {
        return Some(TokenCapability::VogtWrite);
    }
    None
}

/// A refusal with the one body shape every engine error has —
/// `{"error": "<message>"}` — so a client reads a 401 from the gate the same
/// way it reads one from a handler.
fn refusal(
    status: StatusCode,
    request_id: &str,
    retry_after: Option<Duration>,
    message: &str,
) -> Response {
    let mut response = (status, Json(json!({ "error": message }))).into_response();
    if let Ok(value) = HeaderValue::from_str(request_id) {
        response.headers_mut().insert(&REQUEST_ID_HEADER, value);
    }
    if let Some(retry_after) = retry_after {
        let seconds = retry_after.as_secs().max(1);
        if let Ok(value) = HeaderValue::from_str(&seconds.to_string()) {
            response.headers_mut().insert(&RETRY_AFTER_HEADER, value);
        }
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scopes(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn admin_holds_every_capability() {
        assert_eq!(
            capabilities_for_scopes(&scopes(&["admin"])),
            ALL_CAPABILITIES.to_vec()
        );
    }

    #[test]
    fn a_writer_drives_the_pod_but_holds_no_operator_grant() {
        for grant in [&["read", "work.write"][..], &["project.write"][..]] {
            let caps = capabilities_for_scopes(&scopes(grant));
            assert!(caps.contains(&TokenCapability::Sessions));
            assert!(caps.contains(&TokenCapability::VogtWrite));
            assert!(caps.contains(&TokenCapability::Assistant));
            assert!(caps.contains(&TokenCapability::History));
            assert!(!caps.contains(&TokenCapability::GuiControl));
            assert!(!caps.contains(&TokenCapability::AgentClisWrite));
        }
    }

    #[test]
    fn a_reader_may_only_subscribe_to_push() {
        assert_eq!(
            capabilities_for_scopes(&scopes(&["read"])),
            vec![TokenCapability::PushWrite]
        );
        assert_eq!(
            capabilities_for_scopes(&scopes(&["read", "writeback"])),
            vec![TokenCapability::PushWrite]
        );
        assert!(capabilities_for_scopes(&scopes(&[])).is_empty());
    }

    #[test]
    fn maps_mutating_routes_to_capabilities() {
        assert_eq!(
            required_capability(&Method::POST, "/api/sessions"),
            Some(TokenCapability::Sessions)
        );
        assert_eq!(
            required_capability(&Method::PUT, "/api/files"),
            Some(TokenCapability::FilesystemWrite)
        );
        assert_eq!(
            required_capability(&Method::POST, "/api/git/op"),
            Some(TokenCapability::GitWrite)
        );
        assert_eq!(
            required_capability(&Method::POST, "/api/push/test"),
            Some(TokenCapability::PushWrite)
        );
        assert_eq!(
            required_capability(&Method::POST, "/api/push/update"),
            Some(TokenCapability::PushWrite)
        );
        assert_eq!(
            required_capability(&Method::POST, "/api/push/flush-digests"),
            Some(TokenCapability::PushWrite)
        );
        assert_eq!(
            required_capability(&Method::POST, "/api/history/cleanup"),
            Some(TokenCapability::HistoryWrite)
        );
        assert_eq!(
            required_capability(&Method::POST, "/api/agent-tasks/artifacts/cleanup"),
            Some(TokenCapability::AgentTasksWrite)
        );
        assert_eq!(required_capability(&Method::GET, "/api/sessions"), None);
        // A session's detail read ships its scrollback, so it is gated.
        assert_eq!(
            required_capability(&Method::GET, "/api/sessions/abc123"),
            Some(TokenCapability::Sessions)
        );
        // Reading archived history needs its own read capability.
        assert_eq!(
            required_capability(&Method::GET, "/api/history"),
            Some(TokenCapability::History)
        );
        assert_eq!(
            required_capability(&Method::GET, "/api/history/session/abc123"),
            Some(TokenCapability::History)
        );
        assert_eq!(
            required_capability(&Method::POST, "/api/sessions/abc123/input"),
            Some(TokenCapability::Sessions)
        );
        assert_eq!(
            required_capability(&Method::POST, "/api/assistant/message"),
            Some(TokenCapability::Assistant)
        );
        assert_eq!(
            required_capability(&Method::POST, "/api/assistant/actions/xyz"),
            Some(TokenCapability::Assistant)
        );
        assert_eq!(
            required_capability(&Method::POST, "/api/assistant/reset"),
            Some(TokenCapability::Assistant)
        );
        // The server-side speech routes are POST under `/api/assistant`,
        // so the same rule gates them — a speech route must never be an
        // ungated back door into the assistant.
        assert_eq!(
            required_capability(&Method::POST, "/api/assistant/stt"),
            Some(TokenCapability::Assistant)
        );
        assert_eq!(
            required_capability(&Method::POST, "/api/assistant/tts"),
            Some(TokenCapability::Assistant)
        );
        assert_eq!(
            required_capability(&Method::GET, "/api/assistant/history"),
            None
        );
        // The durable interaction log is scope-gated on read, unlike
        // the ephemeral in-memory transcript beside it.
        assert_eq!(
            required_capability(&Method::GET, "/api/assistant/log"),
            Some(TokenCapability::Assistant)
        );
    }
}
