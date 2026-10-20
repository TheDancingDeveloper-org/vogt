//! A small GitHub API client. Ports `src/vogt/adapters/github/client.py`.
//!
//! Built on the shared std HTTP exchange rather than a third-party library,
//! for the reason the Python gives: an optional adapter making a handful of
//! requests must not add a dependency to the core. The token is read from a
//! file and never from argv or a URL.
//!
//! This is the real implementation of the `ForgeTransport` the forge providers
//! read through (WI-1065). The trait itself lives in that lane; the methods
//! here are its contract: `get`, `send`, `identity`, `api_root`, `token`.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::time::Duration;

use serde_json::Value;

use crate::adapters::forge::{self, ForgeResponse, ForgeTransport, WriteMethod};
use crate::errors::VogtError;

pub const API_ROOT: &str = "https://api.github.com";
pub const USER_AGENT: &str = "vogt";
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(20);
/// One page is plenty for a sweep. A collector that paginates forever turns
/// one slow repository into a stalled estate sweep.
pub const DEFAULT_PER_PAGE: u32 = 100;

/// The one host this adapter can read.
pub const SUPPORTED_HOST: &str = "github.com";
/// Below this many core requests left in the hour, reads stop until the
/// window resets (WI-1838). The limit is per *user*, shared with every agent
/// session and CI helper on the same account, so the sweep leaves headroom.
pub const DEFAULT_RATE_LIMIT_FLOOR: i64 = 500;
/// How many conditional-request entries are kept. One per URL the sweep reads;
/// the estate is ~70 repositories at ~15 URLs each.
pub const ETAG_CACHE_SIZE: usize = 4096;

/// Process-wide state, as Python's module globals are. Under `cargo test` it
/// is per test thread instead, which is what the Python suite's autouse reset
/// fixture buys: one test's reads must not shape the next's.
macro_rules! process_state {
    ($(#[$meta:meta])* $vis:vis fn $name:ident($ty:ty)) => {
        $(#[$meta])*
        $vis fn $name<R>(f: impl FnOnce(&mut $ty) -> R) -> R {
            #[cfg(not(test))]
            {
                static STATE: std::sync::LazyLock<std::sync::Mutex<$ty>> =
                    std::sync::LazyLock::new(Default::default);
                let mut guard = STATE
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                f(&mut guard)
            }
            #[cfg(test)]
            {
                thread_local! {
                    static STATE: std::cell::RefCell<$ty> =
                        std::cell::RefCell::new(Default::default());
                }
                STATE.with(|state| f(&mut state.borrow_mut()))
            }
        }
    };
}
pub(crate) use process_state;

/// A non-reversible key for per-token state; never the token itself.
pub(crate) fn token_key(token: Option<&str>) -> String {
    use sha2::{Digest, Sha256};
    match token {
        Some(token) if !token.is_empty() => {
            hex::encode(Sha256::digest(token.as_bytes()))[..16].to_owned()
        }
        _ => "anonymous".to_owned(),
    }
}

/// ETag → body per (token, URL), so an unchanged resource costs nothing.
///
/// GitHub answers `If-None-Match` with 304 when nothing changed, and a 304
/// does not count against the rate limit. Process-wide because providers are
/// built per project per sweep; bounded LRU so it cannot grow without limit.
#[derive(Default)]
pub(crate) struct ConditionalCache {
    entries: HashMap<(String, String), (u64, String, Vec<u8>)>,
    order: BTreeMap<u64, (String, String)>,
    tick: u64,
}

impl ConditionalCache {
    fn touch(&mut self, key: &(String, String)) {
        self.tick += 1;
        if let Some(entry) = self.entries.get_mut(key) {
            self.order.remove(&entry.0);
            entry.0 = self.tick;
            self.order.insert(self.tick, key.clone());
        }
    }

    fn get(&mut self, key: &(String, String)) -> Option<(String, Vec<u8>)> {
        self.touch(key);
        self.entries
            .get(key)
            .map(|(_, etag, body)| (etag.clone(), body.clone()))
    }

    fn put(&mut self, key: (String, String), etag: String, body: Vec<u8>) {
        self.tick += 1;
        if let Some((old, _, _)) = self.entries.insert(key.clone(), (self.tick, etag, body)) {
            self.order.remove(&old);
        }
        self.order.insert(self.tick, key);
        while self.entries.len() > ETAG_CACHE_SIZE {
            let Some((_, oldest)) = self.order.pop_first() else {
                break;
            };
            self.entries.remove(&oldest);
        }
    }
}

/// The last `X-RateLimit-Remaining`/`-Reset` seen per token.
///
/// Read on every response. When remaining drops below the floor, reads are
/// refused locally until the reset time passes; then the budget is unknown
/// again and the next response re-learns it.
pub(crate) struct RateBudget {
    pub(crate) floor: i64,
    seen: HashMap<String, (i64, f64)>,
    /// A fixed "now" for tests; `None` reads the system clock.
    pub(crate) clock: Option<f64>,
}

impl Default for RateBudget {
    fn default() -> Self {
        Self {
            floor: DEFAULT_RATE_LIMIT_FLOOR,
            seen: HashMap::new(),
            clock: None,
        }
    }
}

impl RateBudget {
    fn now(&self) -> f64 {
        self.clock.unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_secs_f64())
                .unwrap_or(0.0)
        })
    }

    pub(crate) fn observe(&mut self, token_key: &str, headers: &BTreeMap<String, String>) {
        let (Some(remaining), Some(reset)) = (
            headers.get("x-ratelimit-remaining"),
            headers.get("x-ratelimit-reset"),
        ) else {
            return;
        };
        // Only the core resource gates the sweep; search and GraphQL have
        // their own buckets.
        if headers
            .get("x-ratelimit-resource")
            .map_or("core", String::as_str)
            != "core"
        {
            return;
        }
        let (Ok(remaining), Ok(reset)) =
            (remaining.trim().parse::<i64>(), reset.trim().parse::<f64>())
        else {
            return;
        };
        self.seen.insert(token_key.to_owned(), (remaining, reset));
    }

    /// The reset epoch while the budget is below the floor, else `None`.
    pub(crate) fn blocked_until(&mut self, token_key: &str) -> Option<f64> {
        let (remaining, reset) = *self.seen.get(token_key)?;
        if self.now() >= reset {
            self.seen.remove(token_key);
            return None;
        }
        (remaining < self.floor).then_some(reset)
    }
}

/// The two process-wide halves: every client for one token shares one budget
/// and one conditional cache, whichever collector built it.
#[derive(Default)]
pub(crate) struct BudgetState {
    pub(crate) etags: ConditionalCache,
    pub(crate) budget: RateBudget,
}

process_state!(
    /// Run `f` against the process-wide ETag cache and rate budget.
    pub(crate) fn with_budget(BudgetState)
);

/// Set the remaining-requests floor below which reads pause (WI-1838).
pub fn configure_rate_limit_floor(floor: i64) {
    with_budget(|state| state.budget.floor = floor.max(0));
}

/// `time.strftime("%H:%M:%SZ", time.gmtime(reset_at))`.
fn reset_clock(reset_at: f64) -> String {
    chrono::DateTime::from_timestamp(reset_at.floor() as i64, 0)
        .map(|when| when.format("%H:%M:%SZ").to_string())
        .unwrap_or_default()
}

/// A paused read, refused before it is sent.
pub(crate) fn budget_refusal(message: String, reset_at: f64) -> VogtError {
    VogtError::GitHubRateLimited {
        message,
        reset_at: Some(reset_at.floor() as i64),
    }
}

/// An exhausted 403/429: the reset comes from the header when it is digits.
pub(crate) fn exhausted_refusal(message: String, headers: &BTreeMap<String, String>) -> VogtError {
    let reset_at = headers
        .get("x-ratelimit-reset")
        .filter(|reset| !reset.is_empty() && reset.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|reset| reset.parse::<i64>().ok());
    VogtError::GitHubRateLimited { message, reset_at }
}

/// Who a token belongs to, and what it may do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHubIdentity {
    pub login: String,
    /// The raw `X-OAuth-Scopes` header. Empty when the response carries none.
    pub scopes: String,
}

/// How the client talks, so tests never need a network. Returns the status,
/// the body and the response headers (lower-cased).
pub type Transport = Box<
    dyn Fn(
        &str,
        &BTreeMap<String, String>,
        &[u8],
        &str,
    ) -> (u16, Vec<u8>, BTreeMap<String, String>),
>;

/// The transport once a client holds it: shared, so a GitHub App client, its
/// token provider and the personal-token fallback all talk through the one
/// stand-in a caller passed (`app_auth.github_client_for`).
pub type SharedTransport = std::rc::Rc<
    dyn Fn(
        &str,
        &BTreeMap<String, String>,
        &[u8],
        &str,
    ) -> (u16, Vec<u8>, BTreeMap<String, String>),
>;

/// Read-only access to one GitHub installation, plus the one mutating method.
pub struct GitHubClient {
    token: Option<String>,
    api_root: String,
    transport: Option<SharedTransport>,
    timeout: Duration,
    /// When set, wins over `token`: asked for a bearer token on every request
    /// (a GitHub App installation, WI-1838).
    token_provider: Option<std::rc::Rc<super::github_app::GitHubAppAuth>>,
}

impl GitHubClient {
    pub fn api_root(&self) -> &str {
        &self.api_root
    }

    /// The fixed token, if this client has one. A GitHub App client has none:
    /// its bearer is minted per request ([`Self::bearer`]).
    pub fn token(&self) -> Option<&str> {
        self.token.as_deref()
    }

    /// The App this client authenticates as, if any.
    pub fn token_provider(&self) -> Option<&super::github_app::GitHubAppAuth> {
        self.token_provider.as_deref()
    }

    /// Build a client, or `None` when the adapter is not configured. `None` is
    /// the ordinary case: no token file means forge subjects are not collected.
    ///
    /// A missing path, a path that is not a file, and an empty file are all
    /// `Ok(None)`. A file that exists but cannot be read, or is not valid
    /// UTF-8, is an error: swallowing it would report a configured forge as
    /// unconfigured.
    pub fn from_token_file(
        path: Option<&Path>,
        transport: Option<Transport>,
    ) -> Result<Option<Self>, VogtError> {
        Self::from_token_file_shared(path, transport.map(SharedTransport::from))
    }

    /// [`Self::from_token_file`] over a transport already shared.
    pub(crate) fn from_token_file_shared(
        path: Option<&Path>,
        transport: Option<SharedTransport>,
    ) -> Result<Option<Self>, VogtError> {
        let Some(token) = read_token_file(path)? else {
            return Ok(None);
        };
        Ok(Some(Self::shared(Some(token), transport)))
    }

    /// A client with an explicit token, or none. A token-less client is what
    /// identity and public-import use; it never reads a file. An empty token
    /// is the same as none.
    pub fn new(token: Option<String>, transport: Option<Transport>) -> Self {
        Self::shared(token, transport.map(SharedTransport::from))
    }

    /// [`Self::new`] over a transport already shared.
    pub(crate) fn shared(token: Option<String>, transport: Option<SharedTransport>) -> Self {
        Self {
            token: token.filter(|token| !token.is_empty()),
            api_root: API_ROOT.to_string(),
            transport,
            timeout: DEFAULT_TIMEOUT,
            token_provider: None,
        }
    }

    /// A client whose bearer comes from a GitHub App installation.
    pub(crate) fn with_token_provider(
        provider: super::github_app::GitHubAppAuth,
        transport: Option<SharedTransport>,
    ) -> Self {
        Self {
            token_provider: Some(std::rc::Rc::new(provider)),
            ..Self::shared(None, transport)
        }
    }

    /// A token-less client whose requests go through `transport`.
    pub fn with_transport(transport: Transport) -> Self {
        Self::new(None, Some(transport))
    }

    /// The token a request to `path` (or a clone) would use now.
    pub fn bearer(&self, path: Option<&str>) -> Result<Option<String>, VogtError> {
        match &self.token_provider {
            Some(provider) => provider.token(path),
            None => Ok(self.token.clone()),
        }
    }

    /// The rate-limit and ETag bucket a request to `path` spends.
    pub(crate) fn budget_key(&self, path: Option<&str>) -> Result<String, VogtError> {
        match &self.token_provider {
            Some(provider) => provider.cache_key(path),
            None => Ok(token_key(self.token.as_deref())),
        }
    }

    /// The account an App installation acts for, or `None` for a PAT.
    ///
    /// A user token creates repositories under its user; an installation token
    /// has no user, so `create_repo` targets the installation's account instead
    /// — the configured installation, or the only one discovered so far.
    pub fn default_owner(&self) -> Result<Option<serde_json::Map<String, Value>>, VogtError> {
        match &self.token_provider {
            Some(provider) => provider.account(None),
            None => Ok(None),
        }
    }

    /// GET one resource. A 404 is `Missing`, a 204 is `Empty`, and a 403 is
    /// `github_unavailable` rather than a silent empty answer.
    ///
    /// Spends the shared budget carefully (WI-1838): a read is refused locally
    /// while the token's budget is below the floor, an unchanged resource is a
    /// conditional request whose 304 replays the cached body, and an exhausted
    /// 403/429 is `github_rate_limited` rather than a plain refusal.
    pub fn get(&self, path: &str, query: &[(&str, String)]) -> Result<ForgeResponse, VogtError> {
        let url = self.url(path, query);
        let mut headers = self.headers(false);
        let key = self.budget_key(Some(path))?;
        let blocked = with_budget(|state| {
            let reset = state.budget.blocked_until(&key)?;
            Some((state.budget.floor, reset))
        });
        if let Some((floor, reset_at)) = blocked {
            return Err(budget_refusal(
                format!(
                    "GitHub rate budget below {floor}; skipping {path} until the reset at {}",
                    reset_clock(reset_at)
                ),
                reset_at,
            ));
        }
        let cache_key = (key.clone(), url.clone());
        let cached = with_budget(|state| state.etags.get(&cache_key));
        if let Some((etag, _)) = &cached {
            headers.insert("If-None-Match".to_string(), etag.clone());
        }

        let (mut status, mut body, response_headers) =
            self.authed_fetch(path, &url, headers, b"", "GET")?;
        with_budget(|state| state.budget.observe(&key, &response_headers));
        if let (304, Some((_, kept))) = (status, &cached) {
            // Unchanged since the last read, and free: replay what we kept.
            status = 200;
            body = kept.clone();
        } else if status == 200 {
            if let Some(etag) = response_headers.get("etag").filter(|etag| !etag.is_empty()) {
                let (etag, kept) = (etag.clone(), body.clone());
                with_budget(|state| state.etags.put(cache_key, etag, kept));
            }
        }
        let exhausted = response_headers
            .get("x-ratelimit-remaining")
            .is_some_and(|remaining| remaining == "0");
        if matches!(status, 403 | 429) && exhausted {
            return Err(exhausted_refusal(
                format!("GitHub rate limit exhausted reading {path}"),
                &response_headers,
            ));
        }
        self.read_outcome(path, status, &body)
    }

    /// Make a change upstream. The only mutating method, and deliberately not
    /// called `request`: every call site that changes somebody else's data is
    /// greppable, and there is no DELETE anywhere.
    pub fn send(
        &self,
        method: WriteMethod,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Value, VogtError> {
        let verb = method.as_str();
        let url = self.url(path, &[]);
        let encoded = body.map(|value| value.to_string()).unwrap_or_default();
        // Before any installation lookup or token mint the write would cause.
        let key = self.budget_key(Some(path))?;
        let blocked = with_budget(|state| {
            let reset = state.budget.blocked_until(&key)?;
            Some((state.budget.floor, reset))
        });
        if let Some((floor, reset_at)) = blocked {
            return Err(budget_refusal(
                format!("GitHub rate budget below {floor}; not sending {path}"),
                reset_at,
            ));
        }
        let (status, response, response_headers) = self.authed_fetch(
            path,
            &url,
            self.headers(true),
            encoded.as_bytes(),
            method.as_str(),
        )?;
        let key = self.budget_key(Some(path))?;
        with_budget(|state| state.budget.observe(&key, &response_headers));
        if status == 404 {
            return Ok(Value::Null);
        }
        if status >= 400 {
            return Err(VogtError::GitHubUnavailable(format!(
                "GitHub returned {status} for {verb} {path}"
            )));
        }
        if response.iter().all(u8::is_ascii_whitespace) {
            return Ok(Value::Object(serde_json::Map::new()));
        }
        serde_json::from_slice(&response).map_err(|_| {
            VogtError::GitHubUnavailable(format!(
                "GitHub answered {verb} {path} with something that is not JSON"
            ))
        })
    }

    /// Who this token is, or `None` when it is invalid (401/403/404). The one
    /// place a token is validated rather than merely used.
    pub fn identity(&self) -> Result<Option<GitHubIdentity>, VogtError> {
        let url = self.url("/user", &[]);
        let (status, body, headers) =
            self.authed_fetch("/user", &url, self.headers(false), b"", "GET")?;
        if matches!(status, 401 | 403 | 404) {
            return Ok(None);
        }
        if status >= 400 {
            return Err(VogtError::GitHubUnavailable(format!(
                "GitHub returned {status} for /user"
            )));
        }
        let payload: Value = if body.iter().all(u8::is_ascii_whitespace) {
            Value::Object(serde_json::Map::new())
        } else {
            serde_json::from_slice(&body).unwrap_or(Value::Null)
        };
        let login = payload
            .get("login")
            .and_then(Value::as_str)
            .filter(|login| !login.is_empty());
        Ok(login.map(|login| GitHubIdentity {
            login: login.to_string(),
            scopes: headers.get("x-oauth-scopes").cloned().unwrap_or_default(),
        }))
    }

    fn read_outcome(
        &self,
        path: &str,
        status: u16,
        body: &[u8],
    ) -> Result<ForgeResponse, VogtError> {
        if status == 404 {
            return Ok(ForgeResponse::Missing);
        }
        if status == 403 {
            return Err(VogtError::GitHubUnavailable(format!(
                "GitHub refused {path} (403): rate limited or unauthorised"
            )));
        }
        if status >= 400 {
            return Err(VogtError::GitHubUnavailable(format!(
                "GitHub returned {status} for {path}"
            )));
        }
        if status == 204 || body.iter().all(u8::is_ascii_whitespace) {
            return Ok(ForgeResponse::Empty);
        }
        serde_json::from_slice(body)
            .map(ForgeResponse::Json)
            .map_err(|_| {
                VogtError::GitHubUnavailable(format!(
                    "GitHub answered {path} with something that is not JSON"
                ))
            })
    }

    /// The headers every request carries, without the credential, which
    /// [`Self::authed_fetch`] attaches.
    fn headers(&self, with_body: bool) -> BTreeMap<String, String> {
        let mut headers = BTreeMap::new();
        headers.insert(
            "Accept".to_string(),
            "application/vnd.github+json".to_string(),
        );
        headers.insert("X-GitHub-Api-Version".to_string(), "2022-11-28".to_string());
        headers.insert("User-Agent".to_string(), USER_AGENT.to_string());
        if with_body {
            headers.insert("Content-Type".to_string(), "application/json".to_string());
        }
        headers
    }

    fn url(&self, path: &str, query: &[(&str, String)]) -> String {
        let mut url = format!("{}{path}", self.api_root.trim_end_matches('/'));
        if !query.is_empty() {
            url.push('?');
            url.push_str(
                &query
                    .iter()
                    .map(|(key, value)| format!("{}={}", quote(key), quote(value)))
                    .collect::<Vec<_>>()
                    .join("&"),
            );
        }
        url
    }

    /// `fetch` with the bearer token attached, refreshing once on a 401.
    ///
    /// A fixed token that gets a 401 is simply wrong; a provided one may have
    /// been revoked or expired early, so it is minted again once. A fallback
    /// personal token is not refreshable, so a 401 on it is not retried with
    /// the same value.
    #[allow(clippy::type_complexity)]
    fn authed_fetch(
        &self,
        path: &str,
        url: &str,
        mut headers: BTreeMap<String, String>,
        body: &[u8],
        method: &str,
    ) -> Result<(u16, Vec<u8>, BTreeMap<String, String>), VogtError> {
        let bearer = self.bearer(Some(path))?;
        if let Some(token) = bearer.as_deref().filter(|token| !token.is_empty()) {
            headers.insert("Authorization".to_string(), format!("Bearer {token}"));
        }
        let mut answer = self.fetch(url, &headers, body, method)?;
        if let (401, Some(provider)) = (answer.0, &self.token_provider) {
            provider.invalidate(Some(path))?;
            let fresh = provider.token(Some(path))?;
            if let Some(fresh) = fresh.filter(|fresh| !fresh.is_empty()) {
                if Some(&fresh) != bearer.as_ref() {
                    headers.insert("Authorization".to_string(), format!("Bearer {fresh}"));
                    answer = self.fetch(url, &headers, body, method)?;
                }
            }
        }
        Ok(answer)
    }

    #[allow(clippy::type_complexity)]
    pub(crate) fn fetch(
        &self,
        url: &str,
        headers: &BTreeMap<String, String>,
        body: &[u8],
        method: &str,
    ) -> Result<(u16, Vec<u8>, BTreeMap<String, String>), VogtError> {
        if let Some(transport) = &self.transport {
            // Names lower-cased, as the network path already returns them. A
            // fake that supplies none gets no scopes, ETags or rate
            // accounting, honestly.
            let (status, body, response_headers) = transport(url, headers, body, method);
            let lowered = response_headers
                .into_iter()
                .map(|(name, value)| (name.to_ascii_lowercase(), value))
                .collect();
            return Ok((status, body, lowered));
        }
        super::engine::http1::exchange(url, method, headers, body, self.timeout)
            .map_err(|error| VogtError::GitHubUnavailable(format!("GitHub unreachable: {error}")))
    }
}

/// The real `ForgeTransport`. `identity` narrows the client's richer result to
/// the `(login, scopes)` the providers read.
impl ForgeTransport for GitHubClient {
    fn api_root(&self) -> &str {
        self.api_root()
    }

    fn token(&self) -> Option<&str> {
        self.token()
    }

    fn bearer(&self, path: Option<&str>) -> Result<Option<String>, VogtError> {
        GitHubClient::bearer(self, path)
    }

    fn default_owner(&self) -> Result<Option<serde_json::Map<String, Value>>, VogtError> {
        GitHubClient::default_owner(self)
    }

    fn get(&self, path: &str, query: &[(&str, String)]) -> Result<ForgeResponse, VogtError> {
        self.get(path, query)
    }

    fn identity(&self) -> Result<Option<(String, String)>, VogtError> {
        Ok(GitHubClient::identity(self)?.map(|who| (who.login, who.scopes)))
    }

    fn send(
        &self,
        method: WriteMethod,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Value, VogtError> {
        self.send(method, path, body)
    }
}

#[allow(unused_imports)]
pub use forge::api_path;

/// Extract `(owner, repo)` from a project's repository URL. One definition,
/// the forge provider's, which matches Python's `urlsplit` host handling:
/// case-insensitive, with userinfo and port stripped, and an empty query
/// accepted.
pub fn repo_of(repo_url: Option<&str>) -> Option<(String, String)> {
    crate::adapters::forge::repo_of(repo_url)
}

fn quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// The token a file holds.
///
/// `Ok(None)` is the ordinary case: no path, a missing path, a path that is not
/// a file, or an empty file. A file that exists but cannot be read, or is not
/// valid UTF-8, is an error rather than a silent "not configured".
pub(crate) fn read_token_file(path: Option<&Path>) -> Result<Option<String>, VogtError> {
    let Some(path) = path else {
        return Ok(None);
    };
    let resolved = expand_user(path);
    if !resolved.is_file() {
        return Ok(None);
    }
    let token = std::fs::read_to_string(&resolved).map_err(|error| {
        VogtError::GitHubUnavailable(format!(
            "cannot read token file {}: {error}",
            resolved.display()
        ))
    })?;
    let token = token.trim().to_string();
    Ok((!token.is_empty()).then_some(token))
}

/// `~/x` to `{home}/x`; anything else unchanged. A missing home leaves the
/// tilde in place, so the later read fails honestly rather than guessing.
pub(crate) fn expand_user(path: &Path) -> std::path::PathBuf {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return Path::new(&home).join(rest);
        }
    }
    path.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn scripted(status: u16, body: &'static str) -> (Arc<Mutex<Vec<String>>>, GitHubClient) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&seen);
        let transport: Transport = Box::new(move |url, headers, body_bytes, method| {
            record.lock().unwrap().push(format!(
                "{method} {url} accept={} version={} auth={} body={}",
                headers.get("Accept").map(String::as_str).unwrap_or(""),
                headers.contains_key("X-GitHub-Api-Version"),
                headers.contains_key("Authorization"),
                String::from_utf8_lossy(body_bytes)
            ));
            (status, body.as_bytes().to_vec(), BTreeMap::new())
        });
        (seen, GitHubClient::with_transport(transport))
    }

    #[test]
    fn a_get_sends_the_github_headers_and_parses_json() {
        let (seen, client) = scripted(200, r#"{"name":"vogt"}"#);
        let response = client
            .get("/repos/org/vogt", &[("per_page", "100".to_string())])
            .unwrap();
        assert_eq!(
            response,
            ForgeResponse::Json(serde_json::json!({"name": "vogt"}))
        );
        let sent = seen.lock().unwrap()[0].clone();
        assert!(sent.contains("GET https://api.github.com/repos/org/vogt?per_page=100"));
        assert!(sent.contains("accept=application/vnd.github+json"));
        assert!(sent.contains("version=true"));
    }

    #[test]
    fn a_404_is_missing_and_a_204_is_empty() {
        let (_, missing) = scripted(404, "");
        assert_eq!(
            missing.get("/repos/org/gone", &[]).unwrap(),
            ForgeResponse::Missing
        );
        let (_, empty) = scripted(204, "");
        assert_eq!(
            empty
                .get("/repos/org/vogt/vulnerability-alerts", &[])
                .unwrap(),
            ForgeResponse::Empty
        );
    }

    #[test]
    fn a_403_is_github_unavailable() {
        let (_, client) = scripted(403, "");
        assert!(matches!(
            client.get("/repos/org/vogt", &[]).unwrap_err(),
            VogtError::GitHubUnavailable(_)
        ));
    }

    #[test]
    fn send_answers_the_verb_it_was_given() {
        let (_, client) = scripted(200, "{}");
        let sent = client
            .send(WriteMethod::Patch, "/repos/org/vogt", None)
            .unwrap();
        assert_eq!(sent, serde_json::json!({}));
    }

    #[test]
    fn an_invalid_token_is_no_identity() {
        let (_, client) = scripted(401, "");
        assert_eq!(client.identity().unwrap(), None);
    }

    #[test]
    fn repo_of_parses_the_forms_and_rejects_the_lookalikes() {
        assert_eq!(
            repo_of(Some("https://github.com/org/repo.git")),
            Some(("org".into(), "repo".into()))
        );
        assert_eq!(
            repo_of(Some("git@github.com:org/repo")),
            Some(("org".into(), "repo".into()))
        );
        assert_eq!(repo_of(Some("https://github.com.evil.com/org/repo")), None);
        assert_eq!(repo_of(Some("https://github.com/org/repo?x=1")), None);
        assert_eq!(repo_of(Some("https://github.com/org/../repo")), None);
        assert_eq!(repo_of(None), None);
    }

    #[test]
    fn api_path_refuses_a_payload_chosen_url() {
        assert_eq!(
            api_path("https://api.github.com/repos/org/repo/issues/1", API_ROOT),
            Some("/repos/org/repo/issues/1".to_string())
        );
        assert_eq!(
            api_path("https://evil.example/repos/org/repo", API_ROOT),
            None
        );
        assert_eq!(
            api_path("https://api.github.com/repos/org/../../etc", API_ROOT),
            None
        );
    }

    #[test]
    fn no_token_file_means_no_client() {
        assert!(GitHubClient::from_token_file(None, None).unwrap().is_none());
    }

    #[test]
    fn from_token_file_errors_when_the_file_cannot_be_read() {
        let dir = std::env::temp_dir().join(format!("vogt-github-token-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // A directory is a path, but not a token file.
        assert!(GitHubClient::from_token_file(Some(&dir), None)
            .unwrap()
            .is_none());
        let bad = dir.join("bad");
        std::fs::write(&bad, [0xff, 0xfe]).unwrap();
        let Err(error) = GitHubClient::from_token_file(Some(&bad), None) else {
            panic!("a non-UTF-8 token file must be an error");
        };
        assert!(error.to_string().contains("valid UTF-8"), "{error}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
