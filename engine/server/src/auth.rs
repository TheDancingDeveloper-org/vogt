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
};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;
use uuid::Uuid;

use crate::app::AppState;
use crate::observability::RequestId;

static AUTH_FAILURES: AtomicU64 = AtomicU64::new(0);
const RATE_LIMIT_WINDOW: Duration = Duration::from_secs(60);
const PRIMARY_TOKEN_NAME: &str = "primary";

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
    Assistant,
    /// Writing to vogt-core through the front door (`/api/vogt`). Reads need
    /// only a valid token; a write needs to have been granted this, because
    /// what it changes is the estate's declared state and not this pod's.
    VogtWrite,
}

const ALL_CAPABILITIES: [TokenCapability; 9] = [
    TokenCapability::Sessions,
    TokenCapability::FilesystemWrite,
    TokenCapability::GitWrite,
    TokenCapability::GuiControl,
    TokenCapability::AgentTasksWrite,
    TokenCapability::PushWrite,
    TokenCapability::HistoryWrite,
    TokenCapability::Assistant,
    TokenCapability::VogtWrite,
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScopedTokenConfig {
    pub name: String,
    pub token: String,
    #[serde(default)]
    pub capabilities: Vec<TokenCapability>,
    #[serde(default = "default_mutating_request_limit_per_minute")]
    pub mutating_requests_per_minute: u32,
    /// Path to the vogt-core token this front-door token is paired with
    /// (FR-S9), and the form a deployment should use.
    ///
    /// A file rather than a value because the front-door token beside it
    /// commonly arrives through `MYDEVENV2_EXTRA_TOKENS_JSON`, which is an
    /// environment variable: putting the *core* token in the same record
    /// would publish a second credential to `/proc/<pid>/environ` and every
    /// `docker inspect`, which is exactly what `VOGT_CORE_TOKEN_FILE` and
    /// FR-S7 exist to avoid. One brokered file per credential is already the
    /// stack's pattern — see the `pre_deploy` hook in
    /// `deploy/vogt-stack.compose.yml`.
    ///
    /// Read once, at `config::load`; the resolved value lands in
    /// `vogt_core_token` below.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vogt_core_token_file: Option<String>,
    /// The paired core token itself, for a deployment that keeps its whole
    /// token table in the config file — where the front-door token is already
    /// a literal, so the pairing is no more exposed than what it pairs with.
    ///
    /// `vogt_core_token_file` wins over this when both are set, for the reason
    /// the loader gives `VOGT_CORE_TOKEN_FILE` precedence: a deployment that
    /// went to the trouble of brokering a file should not have it silently
    /// undone by a value someone also left here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vogt_core_token: Option<String>,
}

fn default_mutating_request_limit_per_minute() -> u32 {
    600
}

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

pub struct AuthorizedToken<'a> {
    pub name: &'a str,
    pub mutating_requests_per_minute: u32,
    capabilities: &'a [TokenCapability],
    /// The core token this front-door token is paired with, if it has one of
    /// its own (FR-S9). `None` means "no pairing", not "no core token": what
    /// the front door falls back to is the proxy's business, not the gate's.
    vogt_core_token: Option<&'a str>,
}

impl AuthorizedToken<'_> {
    pub fn allows(&self, capability: TokenCapability) -> bool {
        self.capabilities.contains(&capability)
    }
}

/// Who the gate decided this request is, handed on to the handler behind it.
///
/// `require_bearer` authorizes and would otherwise discard which token it
/// matched, leaving every handler downstream unable to tell one caller from
/// another. A request extension is the mechanism because it is per-request
/// state that only the handlers on the gated router can see — the alternative,
/// re-deriving the identity in `vogt_core::api` from the `Authorization`
/// header, would mean a second comparison of a secret in a second place.
#[derive(Debug, Clone)]
pub struct AuthorizedIdentity {
    /// The configured name of the front-door token that authenticated this
    /// request — `primary`, or an entry in `extra_tokens`.
    pub token_name: String,
    /// The vogt-core token paired with it (FR-S9), or `None` if it has none.
    pub vogt_core_token: Option<String>,
}

/// Bearer-token gate. Constant-time compare to avoid timing oracles even on a tailnet.
///
/// Also emits an audit log entry for every mutating request (POST/PUT/PATCH/
/// DELETE) — the dev pod has the host Docker socket mounted, so we want a
/// trail of who did what. The single bearer token doesn't give us a user
/// identity, so the log line records method + path + request id; that's
/// enough to correlate suspicious activity from the same client.
pub async fn require_bearer(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    mut request: Request,
    next: Next,
) -> Result<Response, Response> {
    // The id the access-log layer already assigned this request (#139). Taken
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
        .map(str::trim);

    let Some(token) = token else {
        record_auth_failure(&method, &path, &request_id, "missing").await;
        return Err(response_with_status(
            StatusCode::UNAUTHORIZED,
            &request_id,
            None,
        ));
    };

    let Some(access) = authorize_token(state.config.as_ref(), token) else {
        record_auth_failure(&method, &path, &request_id, "wrong-token").await;
        return Err(response_with_status(
            StatusCode::UNAUTHORIZED,
            &request_id,
            None,
        ));
    };

    let is_mutating = matches!(
        method,
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    );
    if let Some(required) = required_capability(&method, &path) {
        if !access.allows(required) {
            tracing::warn!(
                target: "mydevenv2::audit",
                request_id = %request_id,
                token_name = access.name,
                method = %method,
                path = %path,
                required_capability = ?required,
                user_agent = user_agent,
                "capability denied"
            );
            return Err(response_with_status(
                StatusCode::FORBIDDEN,
                &request_id,
                None,
            ));
        }
    }

    if is_mutating {
        if let Some(retry_after) = state
            .auth
            .check_mutation_rate_limit(access.name, access.mutating_requests_per_minute)
        {
            tracing::warn!(
                target: "mydevenv2::audit",
                request_id = %request_id,
                token_name = access.name,
                method = %method,
                path = %path,
                retry_after_ms = retry_after.as_millis() as u64,
                user_agent = user_agent,
                "mutation rate limit exceeded"
            );
            return Err(response_with_status(
                StatusCode::TOO_MANY_REQUESTS,
                &request_id,
                Some(retry_after),
            ));
        }
    }

    // Every check has passed, so the handler may now be told who it is
    // serving. Inserted last on purpose: a refused request never carries an
    // identity, so nothing downstream can mistake "was going to be this
    // caller" for "is this caller".
    request.extensions_mut().insert(AuthorizedIdentity {
        token_name: access.name.to_string(),
        vogt_core_token: access.vogt_core_token.map(str::to_owned),
    });

    let mut response = next.run(request).await;
    if let Ok(value) = HeaderValue::from_str(&request_id) {
        response.headers_mut().insert(&REQUEST_ID_HEADER, value);
    }

    if is_mutating {
        tracing::info!(
            target: "mydevenv2::audit",
            request_id = %request_id,
            token_name = access.name,
            method = %method,
            path = %path,
            status = response.status().as_u16(),
            user_agent = user_agent,
            "mutating request"
        );
    }

    Ok(response)
}

async fn record_auth_failure(method: &Method, path: &str, request_id: &str, reason: &'static str) {
    let count = AUTH_FAILURES.fetch_add(1, Ordering::Relaxed) + 1;
    tracing::warn!(
        target: "mydevenv2::audit",
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

pub fn ws_token_allows_session_access(state: &AppState, candidate: &str) -> bool {
    authorize_token(&state.config, candidate)
        .map(|access| access.allows(TokenCapability::Sessions))
        .unwrap_or(false)
}

fn authorize_token<'a>(
    cfg: &'a crate::config::Config,
    candidate: &str,
) -> Option<AuthorizedToken<'a>> {
    if bool::from(candidate.as_bytes().ct_eq(cfg.token.as_bytes())) {
        return Some(AuthorizedToken {
            name: PRIMARY_TOKEN_NAME,
            capabilities: &ALL_CAPABILITIES,
            mutating_requests_per_minute: cfg.token_mutating_request_limit_per_minute,
            // The primary token's pairing is the deployment-wide
            // `vogt_core_token`, which is also the fallback — so it is left
            // unset here and picked up there. Naming it in both places would
            // make one of them look optional.
            vogt_core_token: None,
        });
    }

    cfg.extra_tokens.iter().find_map(|token| {
        bool::from(candidate.as_bytes().ct_eq(token.token.as_bytes())).then_some(AuthorizedToken {
            name: token.name.as_str(),
            capabilities: token.capabilities.as_slice(),
            mutating_requests_per_minute: token.mutating_requests_per_minute,
            vogt_core_token: token.vogt_core_token.as_deref(),
        })
    })
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
    // ContextKeeper's two effectful posts. `launch` starts a terminal and
    // `approve` is what permits it to, so they are session control by another
    // name — and until this arm existed any valid token could reach them,
    // including a read-only one, while every other write in this server was
    // capability-gated. The gap was silent: nothing failed, a token simply
    // did more than it was granted.
    if path.starts_with("/api/contextkeeper/") && *method == Method::POST {
        return Some(TokenCapability::Sessions);
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

fn response_with_status(
    status: StatusCode,
    request_id: &str,
    retry_after: Option<Duration>,
) -> Response {
    let mut response = status.into_response();
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
    use crate::config::Config;

    const PRIMARY_TOKEN: &str = "primary-token-1234567890";
    const READONLY_TOKEN: &str = "readonly-token-123456789";
    const SESSIONS_TOKEN: &str = "sessions-token-123456789";

    fn test_config() -> Config {
        Config {
            bind: "127.0.0.1:0".parse().unwrap(),
            token: PRIMARY_TOKEN.to_string(),
            token_mutating_request_limit_per_minute: 600,
            extra_tokens: vec![
                ScopedTokenConfig {
                    name: "readonly".into(),
                    token: READONLY_TOKEN.into(),
                    capabilities: vec![],
                    mutating_requests_per_minute: 600,
                    vogt_core_token_file: None,
                    vogt_core_token: None,
                },
                ScopedTokenConfig {
                    name: "sessions".into(),
                    token: SESSIONS_TOKEN.into(),
                    capabilities: vec![TokenCapability::Sessions],
                    mutating_requests_per_minute: 600,
                    vogt_core_token_file: None,
                    vogt_core_token: Some("core-token-for-sessions".into()),
                },
            ],
            scrollback_bytes: 64 * 1024,
            default_shell: "/bin/bash".into(),
            default_cwd: std::env::temp_dir(),
            activity_idle_after_ms: 200,
            idle_stall_after_ms: 10 * 60 * 1_000,
            workspace_root: std::env::temp_dir(),
            gui_stream_url: None,
            gui_stream_verified: false,
            state_dir: tempfile::tempdir().unwrap().keep(),
            fcm_service_account_json: None,
            vapid_subject: "mailto:test@example.invalid".into(),
            allowed_origins: vec![],
            auto_agent_auth: false,
            agent_auth_helper: "/usr/local/bin/mydevenv2-agent-auth".into(),
            session_templates: vec![],
            assistant_api_key: None,
            assistant_base_url: "https://api.theclawbay.com/v1".into(),
            assistant_model: "gpt-5.4-mini".into(),
            assistant_max_tool_calls: 8,
            assistant_allow_claude_proxy: false,
            assistant_reasoning_effort: None,
            assistant_profiles: vec![],
            assistant_default_profile: None,
            contextkeeper_url: None,
            contextkeeper_token: None,
            public_url: None,
            vogt_core_url: None,
            vogt_import_root: None,
            vogt_engine_state_dir: None,
            vogt_core_token: None,
        }
    }

    #[test]
    fn scoped_tokens_limit_capabilities() {
        let cfg = test_config();

        let primary = authorize_token(&cfg, PRIMARY_TOKEN).expect("primary token");
        assert!(primary.allows(TokenCapability::PushWrite));

        let readonly = authorize_token(&cfg, READONLY_TOKEN).expect("readonly token");
        assert!(!readonly.allows(TokenCapability::Sessions));
        assert!(!readonly.allows(TokenCapability::PushWrite));

        let sessions = authorize_token(&cfg, SESSIONS_TOKEN).expect("sessions token");
        assert!(sessions.allows(TokenCapability::Sessions));
        assert!(!sessions.allows(TokenCapability::PushWrite));
    }

    #[test]
    fn a_scoped_token_carries_its_own_core_pairing() {
        let cfg = test_config();

        let sessions = authorize_token(&cfg, SESSIONS_TOKEN).expect("sessions token");
        assert_eq!(sessions.vogt_core_token, Some("core-token-for-sessions"));

        let readonly = authorize_token(&cfg, READONLY_TOKEN).expect("readonly token");
        assert_eq!(readonly.vogt_core_token, None);

        // The primary token's pairing is the deployment-wide fallback, applied
        // by the proxy rather than named here.
        let primary = authorize_token(&cfg, PRIMARY_TOKEN).expect("primary token");
        assert_eq!(primary.vogt_core_token, None);
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
        assert_eq!(
            required_capability(&Method::GET, "/api/assistant/history"),
            None
        );
    }
}
