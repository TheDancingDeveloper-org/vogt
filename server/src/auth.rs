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
}

const ALL_CAPABILITIES: [TokenCapability; 8] = [
    TokenCapability::Sessions,
    TokenCapability::FilesystemWrite,
    TokenCapability::GitWrite,
    TokenCapability::GuiControl,
    TokenCapability::AgentTasksWrite,
    TokenCapability::PushWrite,
    TokenCapability::HistoryWrite,
    TokenCapability::Assistant,
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScopedTokenConfig {
    pub name: String,
    pub token: String,
    #[serde(default)]
    pub capabilities: Vec<TokenCapability>,
    #[serde(default = "default_mutating_request_limit_per_minute")]
    pub mutating_requests_per_minute: u32,
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
}

impl AuthorizedToken<'_> {
    pub fn allows(&self, capability: TokenCapability) -> bool {
        self.capabilities.contains(&capability)
    }
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
    request: Request,
    next: Next,
) -> Result<Response, Response> {
    let request_id = headers
        .get(&REQUEST_ID_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
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
        });
    }

    cfg.extra_tokens.iter().find_map(|token| {
        bool::from(candidate.as_bytes().ct_eq(token.token.as_bytes())).then_some(AuthorizedToken {
            name: token.name.as_str(),
            capabilities: token.capabilities.as_slice(),
            mutating_requests_per_minute: token.mutating_requests_per_minute,
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
                },
                ScopedTokenConfig {
                    name: "sessions".into(),
                    token: SESSIONS_TOKEN.into(),
                    capabilities: vec![TokenCapability::Sessions],
                    mutating_requests_per_minute: 600,
                },
            ],
            scrollback_bytes: 64 * 1024,
            default_shell: "/bin/bash".into(),
            default_cwd: std::env::temp_dir(),
            activity_idle_after_ms: 200,
            idle_stall_after_ms: 10 * 60 * 1_000,
            workspace_root: std::env::temp_dir(),
            gui_stream_url: None,
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
            assistant_auto_type: false,
            assistant_max_tool_calls: 8,
            assistant_reasoning_effort: None,
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
        assert_eq!(required_capability(&Method::GET, "/api/assistant/history"), None);
    }
}
