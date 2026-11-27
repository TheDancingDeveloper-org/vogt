//! Validate GitHub-bound brokered tokens (gh#1460).
//!
//! A manifest line can read a *copy* of a credential that a person rotates
//! somewhere else; when the source rotates, the copy goes dead and every
//! session that fetches it sees a bare `401 Bad credentials` with no hint that
//! a person has to refresh it. The broker therefore checks a GitHub-bound value
//! against `GET {api}/user` before handing it out:
//!
//! - **200**: hand it out. The verdict is cached per (variable, SHA-256 of the
//!   value) for [`DEFAULT_TTL`], so GitHub is not called on every fetch.
//! - **401**: refuse the fetch with a typed `credential_stale` error naming the
//!   variable and its source, and raise one Inbox item (deduplicated per
//!   variable and source) for a person.
//! - anything else, a network error or a timeout: **fail open** with a warning;
//!   a GitHub outage must never block a session's credentials.
//!
//! A background pass over the GitHub-bound manifest entries runs every
//! [`DEFAULT_INTERVAL`] too, so a dead token is reported within minutes even
//! when no session asks for it.
//!
//! Values are never logged or returned: only names and hash prefixes.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use dashmap::DashMap;
use sha2::{Digest, Sha256};

use crate::secret_broker::ManifestSecret;

/// Set to `0`/`false`/`off` to turn the check off entirely.
pub const GITHUB_CHECK_ENV: &str = "ENGINE_AGENT_AUTH_GITHUB_CHECK";
/// Comma- or space-separated manifest names that are GitHub-bound, beyond the
/// built-in [`DEFAULT_GITHUB_VARS`].
pub const GITHUB_VARS_ENV: &str = "ENGINE_AGENT_AUTH_GITHUB_VARS";
/// Seconds between background passes; `0` disables the background pass.
pub const GITHUB_CHECK_INTERVAL_ENV: &str = "ENGINE_AGENT_AUTH_GITHUB_CHECK_INTERVAL_SECS";
/// The GitHub API base. Must be `https` and `api.github.com` or a host listed
/// in [`GITHUB_ENTERPRISE_HOSTS_ENV`]; anything else is refused with a warning
/// and the default is used, so the broker never sends a token to an arbitrary
/// or cleartext endpoint.
pub const GITHUB_API_URL_ENV: &str = "ENGINE_AGENT_AUTH_GITHUB_API_URL";
/// Comma- or space-separated GitHub Enterprise API hosts allowed as the API
/// base, beyond `api.github.com`.
pub const GITHUB_ENTERPRISE_HOSTS_ENV: &str = "ENGINE_AGENT_AUTH_GITHUB_ENTERPRISE_HOSTS";

pub const DEFAULT_GITHUB_VARS: &[&str] = &["GH_TOKEN", "GITHUB_MCP_TOKEN"];
pub const GITHUB_TOKEN_PREFIXES: &[&str] = &["ghp_", "gho_", "ghs_", "github_pat_"];
pub const DEFAULT_API_URL: &str = "https://api.github.com";
pub const DEFAULT_TTL: Duration = Duration::from_secs(10 * 60);
/// How long an inconclusive answer (timeout, 5xx, rate limit) is remembered, so
/// a GitHub outage costs one probe a minute, not one per fetch.
pub const DEFAULT_UNKNOWN_TTL: Duration = Duration::from_secs(60);
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);
pub const DEFAULT_INTERVAL: Duration = Duration::from_secs(30 * 60);

/// Why `url` cannot be the GitHub API base, or the normalised base.
pub fn validate_api_url(url: &str, enterprise_hosts: &[String]) -> Result<String, String> {
    let parsed = reqwest::Url::parse(url.trim()).map_err(|_| "not a valid URL".to_string())?;
    if parsed.scheme() != "https" {
        return Err("must be https".into());
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("must not carry credentials".into());
    }
    let host = parsed.host_str().unwrap_or_default().to_ascii_lowercase();
    if host != "api.github.com"
        && !enterprise_hosts
            .iter()
            .any(|h| h.eq_ignore_ascii_case(&host))
    {
        return Err(format!(
            "host {host} is not api.github.com or listed in {GITHUB_ENTERPRISE_HOSTS_ENV}"
        ));
    }
    Ok(parsed.as_str().trim_end_matches('/').to_string())
}

/// What GitHub said about one value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenVerdict {
    Valid,
    Stale,
    /// No definite answer (timeout, network error, 5xx, rate limit): fail open.
    Unknown,
}

/// A GitHub-bound manifest entry GitHub rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaleCredential {
    pub var: String,
    pub project_id: String,
    pub secret_name: String,
}

impl StaleCredential {
    pub fn of(entry: &ManifestSecret) -> Self {
        Self {
            var: entry.var.clone(),
            project_id: entry.project_id.clone(),
            secret_name: entry.secret_name.clone(),
        }
    }

    pub fn source(&self) -> String {
        format!("{}/{}", self.project_id, self.secret_name)
    }

    /// The refusal a session reads.
    pub fn message(&self) -> String {
        format!(
            "credential stale: {} (source {}) was rejected by GitHub (401); it needs a person to refresh it",
            self.var,
            self.source()
        )
    }
}

/// Where a newly stale credential is reported (the Inbox, in the engine).
pub type StaleSink = Arc<dyn Fn(StaleCredential) + Send + Sync>;

pub struct GithubTokenCheck {
    enabled: bool,
    api: String,
    vars: Vec<String>,
    ttl: Duration,
    interval: Option<Duration>,
    client: reqwest::Client,
    /// (var, value hash) → when the verdict was taken and whether it was valid.
    /// Only definite verdicts are cached.
    cache: DashMap<(String, [u8; 32]), (Instant, TokenVerdict)>,
    unknown_ttl: Duration,
    /// Probes sent to GitHub (tests assert a probe was attempted).
    probes: AtomicUsize,
    /// (var, source) already reported and not since seen valid.
    reported: DashMap<(String, String), ()>,
    sink: OnceLock<StaleSink>,
}

impl GithubTokenCheck {
    /// A check against `api` with the defaults.
    pub(crate) fn new(api: impl Into<String>) -> Self {
        Self::build(api.into(), DEFAULT_TIMEOUT)
    }

    /// A check against an unvalidated `api` (a loopback fake). Not reachable
    /// from configuration: production builds the base through
    /// [`validate_api_url`] in [`Self::from_env`].
    #[doc(hidden)]
    pub fn for_tests(api: impl Into<String>) -> Self {
        Self::new(api)
    }

    fn build(api: String, timeout: Duration) -> Self {
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .connect_timeout(timeout)
            .build()
            .unwrap_or_default();
        Self {
            enabled: true,
            api: api.trim_end_matches('/').to_string(),
            vars: DEFAULT_GITHUB_VARS.iter().map(|v| v.to_string()).collect(),
            ttl: DEFAULT_TTL,
            interval: Some(DEFAULT_INTERVAL),
            client,
            cache: DashMap::new(),
            unknown_ttl: DEFAULT_UNKNOWN_TTL,
            probes: AtomicUsize::new(0),
            reported: DashMap::new(),
            sink: OnceLock::new(),
        }
    }

    /// Configured from the `ENGINE_AGENT_AUTH_GITHUB_*` variables.
    pub fn from_env() -> Self {
        let hosts: Vec<String> = std::env::var(GITHUB_ENTERPRISE_HOSTS_ENV)
            .unwrap_or_default()
            .split([',', ' '])
            .map(|h| h.trim().to_string())
            .filter(|h| !h.is_empty())
            .collect();
        let api = match std::env::var(GITHUB_API_URL_ENV)
            .ok()
            .filter(|v| !v.trim().is_empty())
        {
            None => DEFAULT_API_URL.to_string(),
            Some(url) => validate_api_url(&url, &hosts).unwrap_or_else(|why| {
                tracing::warn!(
                    env = GITHUB_API_URL_ENV,
                    reason = %why,
                    "ignoring the configured GitHub API URL; using {DEFAULT_API_URL}"
                );
                DEFAULT_API_URL.to_string()
            }),
        };
        let mut check = Self::new(api);
        if let Ok(v) = std::env::var(GITHUB_CHECK_ENV) {
            if matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "0" | "false" | "off" | "no"
            ) {
                check.enabled = false;
            }
        }
        if let Ok(extra) = std::env::var(GITHUB_VARS_ENV) {
            check = check.with_vars(extra.split([',', ' ']).map(str::to_string));
        }
        if let Some(secs) = std::env::var(GITHUB_CHECK_INTERVAL_ENV)
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
        {
            check.interval = (secs > 0).then(|| Duration::from_secs(secs));
        }
        check
    }

    /// Add GitHub-bound names beyond the defaults.
    pub fn with_vars(mut self, vars: impl IntoIterator<Item = String>) -> Self {
        for var in vars {
            let var = var.trim().to_string();
            if !var.is_empty() && !self.vars.contains(&var) {
                self.vars.push(var);
            }
        }
        self
    }

    pub fn with_ttl(mut self, ttl: Duration) -> Self {
        self.ttl = ttl;
        self
    }

    pub fn with_unknown_ttl(mut self, ttl: Duration) -> Self {
        self.unknown_ttl = ttl;
        self
    }

    /// How many probes have been sent to GitHub.
    pub fn probe_count(&self) -> usize {
        self.probes.load(Ordering::SeqCst)
    }

    pub fn with_timeout(self, timeout: Duration) -> Self {
        let mut rebuilt = Self::build(self.api.clone(), timeout);
        rebuilt.enabled = self.enabled;
        rebuilt.vars = self.vars;
        rebuilt.ttl = self.ttl;
        rebuilt.unknown_ttl = self.unknown_ttl;
        rebuilt.interval = self.interval;
        rebuilt
    }

    pub fn disabled(mut self) -> Self {
        self.enabled = false;
        self
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// The background pass interval, `None` when it is off.
    pub fn interval(&self) -> Option<Duration> {
        self.interval.filter(|_| self.enabled)
    }

    /// Install where stale credentials are reported. The first call wins.
    pub fn set_sink(&self, sink: StaleSink) {
        let _ = self.sink.set(sink);
    }

    /// Whether `var` is GitHub-bound by name.
    pub fn is_github_var(&self, var: &str) -> bool {
        self.vars.iter().any(|v| v == var)
    }

    /// Whether a fetched value is GitHub-bound, by name or by token prefix.
    pub fn is_github_bound(&self, var: &str, value: &str) -> bool {
        self.enabled
            && (self.is_github_var(var)
                || GITHUB_TOKEN_PREFIXES
                    .iter()
                    .any(|p| value.trim().starts_with(p)))
    }

    /// Check `value` for `entry`, from the cache when it is fresh. A `Stale`
    /// verdict raises the Inbox item (once per variable and source).
    pub async fn check(&self, entry: &ManifestSecret, value: &str) -> TokenVerdict {
        let hash: [u8; 32] = Sha256::digest(value.trim().as_bytes()).into();
        let key = (entry.var.clone(), hash);
        let cached = self
            .cache
            .get(&key)
            .filter(|hit| {
                let ttl = if hit.1 == TokenVerdict::Unknown {
                    self.unknown_ttl
                } else {
                    self.ttl
                };
                hit.0.elapsed() < ttl
            })
            .map(|hit| hit.1);
        let verdict = match cached {
            Some(v) => v,
            None => {
                let verdict = self.ask_github(entry, value, &hash).await;
                self.cache.insert(key, (Instant::now(), verdict));
                verdict
            }
        };
        let stale = StaleCredential::of(entry);
        match verdict {
            TokenVerdict::Valid => {
                // Fixed: a later failure is a new incident.
                self.reported.remove(&(stale.var.clone(), stale.source()));
            }
            TokenVerdict::Stale => {
                self.report(stale);
            }
            TokenVerdict::Unknown => {}
        }
        verdict
    }

    /// Raise the Inbox item for `stale` unless it is already raised. Returns
    /// whether it was raised now.
    pub fn report(&self, stale: StaleCredential) -> bool {
        let key = (stale.var.clone(), stale.source());
        if self.reported.insert(key, ()).is_some() {
            return false;
        }
        tracing::warn!(
            target: "vogt::audit",
            var = %stale.var,
            source = %stale.source(),
            "brokered GitHub credential is stale; raising an Inbox item"
        );
        if let Some(sink) = self.sink.get() {
            sink(stale);
        }
        true
    }

    async fn ask_github(
        &self,
        entry: &ManifestSecret,
        value: &str,
        hash: &[u8; 32],
    ) -> TokenVerdict {
        let short = hex_prefix(hash);
        self.probes.fetch_add(1, Ordering::SeqCst);
        let response = self
            .client
            .get(format!("{}/user", self.api))
            .header(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {}", value.trim()),
            )
            .header(reqwest::header::ACCEPT, "application/vnd.github+json")
            .header(reqwest::header::USER_AGENT, "vogt-engine-broker")
            .send()
            .await;
        match response {
            Ok(r) if r.status().is_success() => TokenVerdict::Valid,
            Ok(r) if r.status() == reqwest::StatusCode::UNAUTHORIZED => TokenVerdict::Stale,
            Ok(r) => {
                tracing::warn!(
                    var = %entry.var,
                    value_sha256 = %short,
                    status = %r.status(),
                    "GitHub token check inconclusive; handing the token out (fail open)"
                );
                TokenVerdict::Unknown
            }
            Err(e) => {
                tracing::warn!(
                    var = %entry.var,
                    value_sha256 = %short,
                    timeout = e.is_timeout(),
                    "GitHub token check failed; handing the token out (fail open)"
                );
                TokenVerdict::Unknown
            }
        }
    }
}

fn hex_prefix(hash: &[u8; 32]) -> String {
    hash[..6].iter().map(|b| format!("{b:02x}")).collect()
}

/// The Inbox item for a stale credential: a pending decision for a person.
pub fn decision_body(stale: &StaleCredential) -> serde_json::Value {
    serde_json::json!({
        "reason": format!("gh#1460: brokered GitHub credential {} is stale", stale.var),
        "question": format!(
            "Brokered credential {} was rejected by GitHub (401). Refresh its source {}?",
            stale.var,
            stale.source()
        ),
        "context": format!(
            "The agent-auth manifest line '{} {} {}' reads Infisical {}, and GitHub answers 401 for it. \
             The broker now refuses to hand it to sessions. Refresh the source secret (often a stale copy \
             of a rotated PAT); the broker serves it again once GitHub accepts it.",
            stale.var,
            stale.project_id,
            stale.secret_name,
            stale.source()
        ),
        "options": [
            {"label": "Refreshed", "description": format!("I refreshed {}; the broker re-checks on the next fetch.", stale.source()), "recommended": true},
            {"label": "Remove line", "description": format!("Drop {} from the manifest.", stale.var), "recommended": false},
        ],
        "ttl_seconds": 7 * 24 * 60 * 60,
    })
}

/// Post each newly stale credential to the core as an Inbox decision, and run
/// the background pass over the GitHub-bound manifest entries.
pub fn spawn(state: Arc<crate::app::AppState>) {
    let broker = Arc::clone(state.sessions.secret_broker());
    let check = broker.github_check();
    if !check.enabled() {
        return;
    }
    if let Some(core) = state.vogt_core.clone() {
        check.set_sink(Arc::new(move |stale: StaleCredential| {
            let core = Arc::clone(&core);
            if let Ok(rt) = tokio::runtime::Handle::try_current() {
                rt.spawn(async move {
                    if let Err(e) = core.post_json("/decisions", &decision_body(&stale)).await {
                        tracing::warn!(var = %stale.var, error = %e, "could not raise the stale-credential Inbox item");
                    }
                });
            }
        }));
    }
    let Some(every) = check.interval() else {
        return;
    };
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(every).await;
            let check = broker.github_check();
            for entry in broker.manifest() {
                if !check.is_github_var(&entry.var) {
                    continue;
                }
                match broker.fetch(&entry).await {
                    Ok(value) => {
                        let _ = check.check(&entry, &value).await;
                    }
                    Err(e) => {
                        tracing::warn!(var = %entry.var, error = %e, "background GitHub token check could not fetch");
                    }
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU16, AtomicUsize, Ordering};
    use std::sync::Mutex;

    use axum::{extract::State, http::StatusCode, routing::get, Router};

    use crate::secret_broker::ManifestPolicy;

    struct Fake {
        status: AtomicU16,
        hits: AtomicUsize,
        delay: Duration,
    }

    async fn serve(status: u16, delay: Duration) -> (String, Arc<Fake>) {
        let fake = Arc::new(Fake {
            status: AtomicU16::new(status),
            hits: AtomicUsize::new(0),
            delay,
        });
        let app = Router::new()
            .route(
                "/user",
                get(|State(f): State<Arc<Fake>>| async move {
                    f.hits.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(f.delay).await;
                    StatusCode::from_u16(f.status.load(Ordering::SeqCst)).unwrap()
                }),
            )
            .with_state(Arc::clone(&fake));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}"), fake)
    }

    fn entry() -> ManifestSecret {
        ManifestSecret {
            var: "GH_TOKEN".into(),
            project_id: "vogt-dev-llm".into(),
            secret_name: "GH_TOKEN".into(),
            policy: ManifestPolicy::OnDemand,
            writable: false,
            tier: false,
        }
    }

    fn recording(check: &GithubTokenCheck) -> Arc<Mutex<Vec<StaleCredential>>> {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        check.set_sink(Arc::new(move |s| sink.lock().unwrap().push(s)));
        seen
    }

    #[tokio::test]
    async fn a_valid_token_passes() {
        let (url, _) = serve(200, Duration::ZERO).await;
        let check = GithubTokenCheck::new(url);
        let seen = recording(&check);
        assert_eq!(check.check(&entry(), "ghp_x").await, TokenVerdict::Valid);
        assert!(seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_rejected_token_is_stale_and_raises_one_item() {
        let (url, _) = serve(401, Duration::ZERO).await;
        let check = GithubTokenCheck::new(url).with_ttl(Duration::ZERO);
        let seen = recording(&check);
        assert_eq!(check.check(&entry(), "ghp_dead").await, TokenVerdict::Stale);
        assert_eq!(check.check(&entry(), "ghp_dead").await, TokenVerdict::Stale);
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1, "deduplicated per var and source");
        let message = seen[0].message();
        assert_eq!(
            message,
            "credential stale: GH_TOKEN (source vogt-dev-llm/GH_TOKEN) was rejected by GitHub (401); it needs a person to refresh it"
        );
        assert!(!message.contains("ghp_dead"));
    }

    #[tokio::test]
    async fn a_timeout_fails_open() {
        let (url, _) = serve(401, Duration::from_secs(5)).await;
        let check = GithubTokenCheck::new(url).with_timeout(Duration::from_millis(100));
        let seen = recording(&check);
        assert_eq!(check.check(&entry(), "ghp_x").await, TokenVerdict::Unknown);
        assert!(seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn an_unreachable_api_fails_open() {
        let check = GithubTokenCheck::new("http://127.0.0.1:1");
        assert_eq!(check.check(&entry(), "ghp_x").await, TokenVerdict::Unknown);
    }

    #[tokio::test]
    async fn the_cache_ttl_is_honoured() {
        let (url, fake) = serve(200, Duration::ZERO).await;
        let check = GithubTokenCheck::new(url.clone());
        check.check(&entry(), "ghp_x").await;
        check.check(&entry(), "ghp_x").await;
        assert_eq!(fake.hits.load(Ordering::SeqCst), 1, "cached within the TTL");
        check.check(&entry(), "ghp_other").await;
        assert_eq!(fake.hits.load(Ordering::SeqCst), 2, "keyed by value hash");

        let (url, fake) = serve(200, Duration::ZERO).await;
        let check = GithubTokenCheck::new(url).with_ttl(Duration::from_millis(50));
        check.check(&entry(), "ghp_x").await;
        tokio::time::sleep(Duration::from_millis(80)).await;
        check.check(&entry(), "ghp_x").await;
        assert_eq!(
            fake.hits.load(Ordering::SeqCst),
            2,
            "re-checked after the TTL"
        );
    }

    #[tokio::test]
    async fn a_refreshed_token_clears_the_report() {
        let (url, fake) = serve(401, Duration::ZERO).await;
        let check = GithubTokenCheck::new(url).with_ttl(Duration::ZERO);
        let seen = recording(&check);
        check.check(&entry(), "ghp_x").await;
        fake.status.store(200, Ordering::SeqCst);
        assert_eq!(check.check(&entry(), "ghp_x").await, TokenVerdict::Valid);
        fake.status.store(401, Ordering::SeqCst);
        check.check(&entry(), "ghp_x").await;
        assert_eq!(seen.lock().unwrap().len(), 2);
    }

    #[test]
    fn the_api_base_must_be_https_on_an_allowed_host() {
        let ghe = vec!["ghe.example.com".to_string()];
        assert_eq!(
            validate_api_url("https://api.github.com/", &ghe).unwrap(),
            "https://api.github.com"
        );
        assert!(validate_api_url("https://ghe.example.com/api/v3", &ghe).is_ok());
        assert!(validate_api_url("http://api.github.com", &ghe).is_err());
        assert!(validate_api_url("http://127.0.0.1:9", &ghe).is_err());
        assert!(validate_api_url("https://evil.example.com", &ghe).is_err());
        assert!(validate_api_url("https://api.github.com.evil.com", &ghe).is_err());
        assert!(validate_api_url("https://user:pw@api.github.com", &ghe).is_err());
        assert!(validate_api_url("not a url", &ghe).is_err());
        assert!(validate_api_url("https://ghe.example.com", &[]).is_err());
    }

    #[tokio::test]
    async fn an_inconclusive_answer_is_cached_briefly() {
        let (url, fake) = serve(503, Duration::ZERO).await;
        let check = GithubTokenCheck::new(url).with_unknown_ttl(Duration::from_millis(80));
        assert_eq!(check.check(&entry(), "ghp_x").await, TokenVerdict::Unknown);
        assert_eq!(check.check(&entry(), "ghp_x").await, TokenVerdict::Unknown);
        assert_eq!(fake.hits.load(Ordering::SeqCst), 1, "not re-probed at once");
        tokio::time::sleep(Duration::from_millis(120)).await;
        check.check(&entry(), "ghp_x").await;
        assert_eq!(
            fake.hits.load(Ordering::SeqCst),
            2,
            "re-probed after the short TTL"
        );
    }

    #[test]
    fn github_bound_by_name_or_prefix() {
        let check = GithubTokenCheck::new(DEFAULT_API_URL).with_vars(["MY_GH".to_string()]);
        assert!(check.is_github_bound("GH_TOKEN", "x"));
        assert!(check.is_github_bound("GITHUB_MCP_TOKEN", "x"));
        assert!(check.is_github_bound("MY_GH", "x"));
        assert!(check.is_github_bound("OTHER", "github_pat_abc"));
        assert!(check.is_github_bound("OTHER", "ghs_abc"));
        assert!(!check.is_github_bound("OTHER", "sk-abc"));
        assert!(!check.disabled().is_github_bound("GH_TOKEN", "ghp_x"));
    }
}
