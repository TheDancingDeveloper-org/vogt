//! GitHub App installation tokens for the forge adapter. Ports
//! `src/vogt/adapters/github/app_auth.py` (WI-1838).
//!
//! A personal token's core rate limit is per *user*, shared with everything
//! else on that account; an App installation has its own. The App signs a short
//! RS256 JWT with its private key and exchanges it for an installation token
//! that lives an hour. This module keeps each installation's token until five
//! minutes before it expires.
//!
//! One App can be installed on several accounts and organisations. With no
//! installation configured, the installation for a request is discovered from
//! the owner in its path (`GET /repos/{owner}/{repo}/installation`, else
//! `/orgs/{owner}/installation` or `/users/{owner}/installation`) and
//! remembered, so a forge config that spans several organisations works with
//! one App.
//!
//! Signed with `ring`, which the TLS stack already pulls in, rather than a JWT
//! library: RS256 is one PKCS#1 v1.5 signature over two base64url segments.
//! Setup is documented in `docs/GITHUB_APP.md`.
//!
//! Python keeps one `GitHubAppAuth` per App, installation setting, key file and
//! transport. Here the key and the token state are what is shared — per App,
//! installation setting and key file — and each client build wraps them with
//! its own transport handle, which is the same stand-in a context hands out
//! fresh per call.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::rc::Rc;
use std::sync::{Arc, Mutex, PoisonError};

use base64::Engine as _;
use ring::rand::SystemRandom;
use ring::signature::{RsaKeyPair, RSA_PKCS1_SHA256};
use serde_json::{Map, Value};

use super::github::{
    budget_refusal, exhausted_refusal, expand_user, process_state, token_key, with_budget,
    GitHubClient, SharedTransport, API_ROOT, USER_AGENT,
};
use crate::config::VogtConfig;
use crate::errors::VogtError;

/// GitHub refuses a JWT whose lifetime exceeds ten minutes; 9 leaves room for
/// clock skew on top of the backdated `iat`.
pub const JWT_LIFETIME_SECONDS: i64 = 540;
/// `iat` is backdated so a server clock slightly ahead of ours still accepts it.
pub const JWT_BACKDATE_SECONDS: i64 = 60;
/// Refresh this long before `expires_at`, so a sweep never starts on a token
/// that dies halfway through it.
pub const REFRESH_MARGIN_SECONDS: f64 = 300.0;
/// GitHub documents installation tokens as valid for one hour.
pub const DEFAULT_TOKEN_LIFETIME_SECONDS: f64 = 3600.0;
/// How long "the App is not installed on this owner" is remembered. Requests
/// for that owner use the personal token meanwhile, and make no App calls; an
/// installation added later is picked up when the entry expires.
pub const NOT_INSTALLED_TTL_SECONDS: f64 = 3600.0;

fn b64url(raw: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw)
}

/// `json.dumps(text)`: a JSON string with every non-ASCII character escaped.
fn ascii_json(text: &str) -> String {
    let mut out = String::new();
    for ch in Value::String(text.to_owned()).to_string().chars() {
        if ch.is_ascii() {
            out.push(ch);
        } else {
            let mut units = [0u16; 2];
            for unit in ch.encode_utf16(&mut units) {
                out.push_str(&format!("\\u{unit:04x}"));
            }
        }
    }
    out
}

/// An RS256 App JWT: `iss` = App or client ID, `iat` = now−60, `exp` = now+540.
pub fn sign_jwt(private_key: &RsaKeyPair, issuer: &str, now: f64) -> String {
    let now = now.floor() as i64;
    let header = r#"{"alg":"RS256","typ":"JWT"}"#;
    let claims = format!(
        r#"{{"iat":{},"exp":{},"iss":{}}}"#,
        now - JWT_BACKDATE_SECONDS,
        now + JWT_LIFETIME_SECONDS,
        ascii_json(issuer)
    );
    let signing_input = format!(
        "{}.{}",
        b64url(header.as_bytes()),
        b64url(claims.as_bytes())
    );
    let mut signature = vec![0u8; private_key.public().modulus_len()];
    private_key
        .sign(
            &RSA_PKCS1_SHA256,
            &SystemRandom::new(),
            signing_input.as_bytes(),
            &mut signature,
        )
        .expect("PKCS#1 v1.5 signing with a loaded RSA key does not fail");
    format!("{signing_input}.{}", b64url(&signature))
}

/// The private key a PEM file holds, or `None` when it is not an unencrypted
/// RSA key (`PRIVATE KEY` as PKCS#8, or `RSA PRIVATE KEY` as PKCS#1).
pub fn load_rsa_pem(pem: &[u8]) -> Option<RsaKeyPair> {
    let text = std::str::from_utf8(pem).ok()?;
    let begin = text.find("-----BEGIN ")?;
    let rest = &text[begin + "-----BEGIN ".len()..];
    let label_end = rest.find("-----")?;
    let label = &rest[..label_end];
    let body_start = label_end + "-----".len();
    let footer = format!("-----END {label}-----");
    let body_end = rest.find(&footer)?;
    let body: String = rest[body_start..body_end]
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .collect();
    let der = base64::engine::general_purpose::STANDARD
        .decode(body.as_bytes())
        .ok()?;
    match label {
        "PRIVATE KEY" => RsaKeyPair::from_pkcs8(&der).ok(),
        "RSA PRIVATE KEY" => RsaKeyPair::from_der(&der).ok(),
        _ => None,
    }
}

/// An ISO-8601 `expires_at` as an epoch, or `None` when it does not parse.
fn parse_expiry(value: Option<&Value>) -> Option<f64> {
    let text = value?.as_str().filter(|text| !text.is_empty())?;
    chrono::DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|when| when.timestamp() as f64 + f64::from(when.timestamp_subsec_micros()) / 1e6)
}

/// The owner (and repository, if any) a request path is about.
fn owner_repo(path: Option<&str>) -> (Option<String>, Option<String>) {
    let Some(path) = path.filter(|path| !path.is_empty()) else {
        return (None, None);
    };
    let head = path.split('?').next().unwrap_or("");
    let parts: Vec<&str> = head.trim_matches('/').split('/').collect();
    if parts.len() >= 3 && parts[0] == "repos" {
        return (Some(parts[1].to_owned()), Some(parts[2].to_owned()));
    }
    if parts.len() >= 2 && matches!(parts[0], "orgs" | "users") {
        return (Some(parts[1].to_owned()), None);
    }
    (None, None)
}

/// What an App has learned, shared by every client built for it.
#[derive(Default)]
struct AppState {
    /// installation id → (token, expires_at)
    tokens: HashMap<i64, (String, f64)>,
    /// owner login (lower-cased) → installation id
    owners: HashMap<String, i64>,
    /// installation id → its account (`login`, `type`)
    accounts: HashMap<i64, Map<String, Value>>,
    /// owner login (lower-cased) → when "not installed" stops being trusted
    not_installed: HashMap<String, f64>,
}

/// The key and the learned state, cached per App, installation setting and
/// key file so providers built per project per sweep share one token.
struct SharedApp {
    key: RsaKeyPair,
    state: Mutex<AppState>,
}

fn now_epoch() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs_f64())
        .unwrap_or(0.0)
}

/// A token provider that mints and caches installation tokens.
///
/// [`GitHubClient`] calls [`Self::token`] before every request; it only goes
/// to the network when that installation's cached token is missing or within
/// five minutes of expiry. [`Self::invalidate`] drops it, which the client
/// does once on a 401.
pub struct GitHubAppAuth {
    pub app_id: String,
    pub installation_id: Option<i64>,
    shared: Arc<SharedApp>,
    api_root: String,
    transport: Option<SharedTransport>,
    clock: Rc<dyn Fn() -> f64>,
    /// The personal token used for owners the App is not installed on; `None`
    /// sends those requests unauthenticated.
    pub fallback_token: Option<String>,
}

impl GitHubAppAuth {
    pub fn new(
        app_id: &str,
        private_key: RsaKeyPair,
        installation_id: Option<i64>,
        transport: Option<SharedTransport>,
        clock: Option<Rc<dyn Fn() -> f64>>,
    ) -> Self {
        Self::over(
            app_id,
            Arc::new(SharedApp {
                key: private_key,
                state: Mutex::new(AppState::default()),
            }),
            installation_id,
            transport,
            clock,
        )
    }

    fn over(
        app_id: &str,
        shared: Arc<SharedApp>,
        installation_id: Option<i64>,
        transport: Option<SharedTransport>,
        clock: Option<Rc<dyn Fn() -> f64>>,
    ) -> Self {
        Self {
            app_id: app_id.to_owned(),
            installation_id,
            shared,
            api_root: API_ROOT.to_owned(),
            transport,
            clock: clock.unwrap_or_else(|| Rc::new(now_epoch)),
            fallback_token: None,
        }
    }

    /// Load the PEM, or `None` when the file is missing or not an RSA key.
    pub fn from_key_file(
        app_id: &str,
        key_file: &Path,
        installation_id: Option<i64>,
        transport: Option<SharedTransport>,
    ) -> Option<Self> {
        let key = read_key(key_file)?;
        Some(Self::new(app_id, key, installation_id, transport, None))
    }

    /// Requests signed with the JWT share the App's own rate limit.
    fn app_budget_key(&self) -> String {
        format!("github-app:{}:jwt", self.app_id)
    }

    fn now(&self) -> f64 {
        (self.clock)()
    }

    fn state(&self) -> std::sync::MutexGuard<'_, AppState> {
        self.shared
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    // -- the TokenProvider surface ------------------------------------------

    /// Stable across token refreshes: the rate limit is per installation.
    ///
    /// An owner without the App shares the personal token's bucket, the same
    /// key a token-file client uses.
    pub fn cache_key(&self, path: Option<&str>) -> Result<String, VogtError> {
        Ok(match self.installation_for(path)? {
            None => token_key(self.fallback_token.as_deref()),
            Some(installation) => format!("github-app:{}:{installation}", self.app_id),
        })
    }

    pub fn token(&self, path: Option<&str>) -> Result<Option<String>, VogtError> {
        let Some(installation) = self.installation_for(path)? else {
            return Ok(self.fallback_token.clone());
        };
        let mut state = self.state();
        if let Some((token, expires_at)) = state.tokens.get(&installation) {
            if self.now() < expires_at - REFRESH_MARGIN_SECONDS {
                return Ok(Some(token.clone()));
            }
        }
        let (token, expires_at) = self.mint(installation)?;
        state
            .tokens
            .insert(installation, (token.clone(), expires_at));
        Ok(Some(token))
    }

    pub fn invalidate(&self, path: Option<&str>) -> Result<(), VogtError> {
        if let Some(installation) = self.installation_for(path)? {
            self.state().tokens.remove(&installation);
        }
        Ok(())
    }

    /// The account (`login`, `type`) of the installation `path` routes to.
    ///
    /// `None` when that owner has no installation. With no path and nothing
    /// configured or discovered, the App's installations are listed: exactly
    /// one is used, more refuse (set `github_app_installation_id`).
    pub fn account(&self, path: Option<&str>) -> Result<Option<Map<String, Value>>, VogtError> {
        let installation = if path.is_none() && self.installation_id.is_none() {
            self.sole_installation()?
        } else {
            self.installation_for(path)?
        };
        let Some(installation) = installation else {
            return Ok(None);
        };
        let known = self.state().accounts.get(&installation).cloned();
        let known = match known {
            Some(known) => known,
            None => {
                let payload = self.app_get(&format!("/app/installations/{installation}"))?;
                let account = payload
                    .as_ref()
                    .and_then(|payload| payload.get("account"))
                    .and_then(Value::as_object)
                    .cloned()
                    .unwrap_or_default();
                self.state().accounts.insert(installation, account.clone());
                account
            }
        };
        Ok((!known.is_empty()).then_some(known))
    }

    // -- internals -----------------------------------------------------------

    pub fn jwt(&self) -> String {
        sign_jwt(&self.shared.key, &self.app_id, self.now())
    }

    /// The installation for `path`'s owner; `None` when it has none.
    fn installation_for(&self, path: Option<&str>) -> Result<Option<i64>, VogtError> {
        if let Some(installation) = self.installation_id {
            return Ok(Some(installation));
        }
        let (owner, repo) = owner_repo(path);
        let known: HashSet<i64> = {
            let state = self.state();
            if let Some(owner) = &owner {
                let lowered = owner.to_lowercase();
                if let Some(installation) = state.owners.get(&lowered) {
                    return Ok(Some(*installation));
                }
                if let Some(until) = state.not_installed.get(&lowered) {
                    if self.now() < *until {
                        return Ok(None);
                    }
                }
            }
            state.owners.values().copied().collect()
        };
        let Some(owner) = owner else {
            // Nothing in the path names an owner (`/user`, a bare publish); one
            // discovered installation is unambiguous, more are not.
            if known.len() == 1 {
                return Ok(known.into_iter().next());
            }
            return Err(VogtError::GitHubUnavailable(
                "this GitHub request names no owner, and with no \
                 github_app_installation_id configured the App cannot tell \
                 which installation to use"
                    .to_owned(),
            ));
        };
        let installation = self.discover(&owner, repo.as_deref())?;
        {
            let mut state = self.state();
            let lowered = owner.to_lowercase();
            match installation {
                None => {
                    let until = self.now() + NOT_INSTALLED_TTL_SECONDS;
                    state.not_installed.insert(lowered, until);
                }
                Some(installation) => {
                    state.not_installed.remove(&lowered);
                    state.owners.insert(lowered, installation);
                }
            }
        }
        if installation.is_none() {
            tracing::info!(
                target: "vogt.github.app",
                "GitHub App {} is not installed on {}; using the personal token for it for the next {}s",
                self.app_id,
                crate::core::py_repr(&owner),
                NOT_INSTALLED_TTL_SECONDS as i64
            );
        }
        Ok(installation)
    }

    /// The one installation this App has, for an owner-less publish.
    fn sole_installation(&self) -> Result<Option<i64>, VogtError> {
        let known: HashSet<i64> = self.state().owners.values().copied().collect();
        if known.len() == 1 {
            return Ok(known.into_iter().next());
        }
        if known.is_empty() {
            if let Some(Value::Array(listed)) = self.app_get("/app/installations")? {
                let ids: Vec<&Map<String, Value>> = listed
                    .iter()
                    .filter_map(Value::as_object)
                    .filter(|item| item.get("id").and_then(Value::as_i64).is_some())
                    .collect();
                if let [only] = ids.as_slice() {
                    let number = only.get("id").and_then(Value::as_i64).unwrap_or_default();
                    if let Some(account) = only.get("account").and_then(Value::as_object) {
                        self.state().accounts.insert(number, account.clone());
                    }
                    return Ok(Some(number));
                }
            }
        }
        Err(VogtError::GitHubUnavailable(
            "the GitHub App has more than one installation (or none), so \
             there is no default owner to create a repository under; set \
             github_app_installation_id"
                .to_owned(),
        ))
    }

    fn discover(&self, owner: &str, repo: Option<&str>) -> Result<Option<i64>, VogtError> {
        let mut candidates = vec![
            format!("/orgs/{owner}/installation"),
            format!("/users/{owner}/installation"),
        ];
        if let Some(repo) = repo {
            candidates.insert(0, format!("/repos/{owner}/{repo}/installation"));
        }
        for candidate in candidates {
            let payload = self.app_get(&candidate)?;
            let Some(found) = payload.as_ref().and_then(Value::as_object) else {
                continue;
            };
            let Some(number) = found.get("id").and_then(Value::as_i64) else {
                continue;
            };
            if let Some(account) = found.get("account").and_then(Value::as_object) {
                self.state().accounts.insert(number, account.clone());
            }
            return Ok(Some(number));
        }
        Ok(None)
    }

    /// A request authenticated as the App itself (the JWT).
    ///
    /// Bypasses the client's ETag cache: the JWT changes on every call. The
    /// App's own rate budget is checked first and observed after, so a low
    /// budget stops installation lookups and token mints before they are sent.
    fn app_request(
        &self,
        path: &str,
        method: &str,
        body: &[u8],
    ) -> Result<(u16, Option<Value>), VogtError> {
        let budget_key = self.app_budget_key();
        if let Some(reset_at) = with_budget(|state| state.budget.blocked_until(&budget_key)) {
            return Err(budget_refusal(
                format!("GitHub App rate budget is below the floor; skipping {path}"),
                reset_at,
            ));
        }
        let client = GitHubClient::shared(None, self.transport.clone());
        let headers = BTreeMap::from([
            (
                "Accept".to_owned(),
                "application/vnd.github+json".to_owned(),
            ),
            ("X-GitHub-Api-Version".to_owned(), "2022-11-28".to_owned()),
            ("User-Agent".to_owned(), USER_AGENT.to_owned()),
            ("Authorization".to_owned(), format!("Bearer {}", self.jwt())),
        ]);
        let url = format!("{}{path}", self.api_root);
        let (status, raw, response_headers) = client.fetch(&url, &headers, body, method)?;
        with_budget(|state| state.budget.observe(&budget_key, &response_headers));
        let exhausted = response_headers
            .get("x-ratelimit-remaining")
            .is_some_and(|remaining| remaining == "0");
        if matches!(status, 403 | 429) && exhausted {
            return Err(exhausted_refusal(
                format!("GitHub App rate limit exhausted on {path}"),
                &response_headers,
            ));
        }
        if raw.iter().all(u8::is_ascii_whitespace) {
            return Ok((status, None));
        }
        let payload = serde_json::from_slice(&raw).map_err(|_| {
            VogtError::GitHubUnavailable(format!(
                "GitHub answered App request {path} with something that is not JSON"
            ))
        })?;
        Ok((status, Some(payload)))
    }

    pub(crate) fn app_get(&self, path: &str) -> Result<Option<Value>, VogtError> {
        let (status, payload) = self.app_request(path, "GET", b"")?;
        if status == 404 {
            return Ok(None);
        }
        if status >= 400 {
            return Err(VogtError::GitHubUnavailable(format!(
                "GitHub returned {status} for App request {path}"
            )));
        }
        Ok(payload)
    }

    fn mint(&self, installation: i64) -> Result<(String, f64), VogtError> {
        let path = format!("/app/installations/{installation}/access_tokens");
        let (status, payload) = self.app_request(&path, "POST", b"{}")?;
        let token = payload
            .as_ref()
            .and_then(|payload| payload.get("token"))
            .and_then(|token| match token {
                Value::String(text) if !text.is_empty() => Some(text.clone()),
                Value::Number(number) if number.as_f64() != Some(0.0) => Some(number.to_string()),
                _ => None,
            });
        let Some(token) = token.filter(|_| status < 400) else {
            return Err(VogtError::GitHubUnavailable(format!(
                "GitHub App {} could not mint a token for installation {installation} (HTTP {status})",
                self.app_id
            )));
        };
        let expires = parse_expiry(
            payload
                .as_ref()
                .and_then(|payload| payload.get("expires_at")),
        )
        .unwrap_or_else(|| self.now() + DEFAULT_TOKEN_LIFETIME_SECONDS);
        Ok((token, expires))
    }
}

/// The key a file holds, or `None` when it is missing, unreadable or not an
/// unencrypted RSA PEM key.
fn read_key(key_file: &Path) -> Option<RsaKeyPair> {
    let resolved = expand_user(key_file);
    if !resolved.is_file() {
        return None;
    }
    load_rsa_pem(&std::fs::read(&resolved).ok()?)
}

/// What the process keeps between provider builds: the shared App state per
/// (App, installation setting, key file), and the key files already warned
/// about, so a broken key logs once, not per sweep.
#[derive(Default)]
struct AuthCache {
    auths: HashMap<(String, Option<i64>, String), Arc<SharedApp>>,
    warned: HashSet<String>,
}

process_state!(fn with_auths(AuthCache));

/// The configured App, or `None` unless its ID and a usable key are set.
pub fn app_auth_from_config(
    config: &VogtConfig,
    transport: Option<SharedTransport>,
) -> Option<GitHubAppAuth> {
    let app_id = config
        .github_app_id
        .as_deref()
        .filter(|id| !id.is_empty())?;
    let key_file = config.github_app_private_key_file.as_deref()?;
    let installation = config.github_app_installation_id;
    let shown = expand_user(key_file).to_string_lossy().into_owned();
    let cache_key = (app_id.to_owned(), installation, shown.clone());
    if let Some(shared) = with_auths(|cache| cache.auths.get(&cache_key).cloned()) {
        return Some(GitHubAppAuth::over(
            app_id,
            shared,
            installation,
            transport,
            None,
        ));
    }
    match read_key(key_file) {
        Some(key) => {
            let shared = Arc::new(SharedApp {
                key,
                state: Mutex::new(AppState::default()),
            });
            with_auths(|cache| {
                cache.auths.insert(cache_key, Arc::clone(&shared));
                cache.warned.remove(&shown);
            });
            Some(GitHubAppAuth::over(
                app_id,
                shared,
                installation,
                transport,
                None,
            ))
        }
        None => {
            // Once per key file, not per provider build: the registry builds
            // one per project per sweep.
            if with_auths(|cache| cache.warned.insert(shown.clone())) {
                tracing::warn!(
                    target: "vogt.github.app",
                    "GitHub App {app_id} is configured but its private key file {shown} is \
                     missing or not an RSA PEM key; falling back to github_token_file"
                );
            }
            None
        }
    }
}

/// Forget every cached App token (tests; a rotated key).
pub fn clear_app_auth_cache() {
    with_auths(|cache| *cache = AuthCache::default());
}

/// The App client when the App is configured, else the token-file client.
pub fn github_client_for(
    config: &VogtConfig,
    token_file: Option<&Path>,
    transport: Option<super::github::Transport>,
) -> Result<Option<GitHubClient>, VogtError> {
    let transport = transport.map(SharedTransport::from);
    let pat = GitHubClient::from_token_file_shared(token_file, transport.clone())?;
    if let Some(mut auth) = app_auth_from_config(config, transport.clone()) {
        // Owners the App is not installed on are read with the personal token.
        auth.fallback_token = pat.as_ref().and_then(|pat| pat.token().map(str::to_owned));
        return Ok(Some(GitHubClient::with_token_provider(auth, transport)));
    }
    Ok(pat)
}

#[cfg(test)]
#[path = "github_app_test.rs"]
mod tests;
