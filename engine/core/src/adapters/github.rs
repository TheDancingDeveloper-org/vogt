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

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use serde_json::Value;

use crate::adapters::forge::{self, ForgeResponse, ForgeTransport};
use crate::errors::VogtError;

pub const API_ROOT: &str = "https://api.github.com";
pub const USER_AGENT: &str = "vogt";
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(20);
/// One page is plenty for a sweep. A collector that paginates forever turns
/// one slow repository into a stalled estate sweep.
pub const DEFAULT_PER_PAGE: u32 = 100;

/// The one host this adapter can read.
pub const SUPPORTED_HOST: &str = "github.com";

/// Who a token belongs to, and what it may do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHubIdentity {
    pub login: String,
    /// The raw `X-OAuth-Scopes` header. Empty through a transport fake, which
    /// cannot carry response headers, and populated on the real network path.
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

/// Read-only access to one GitHub installation, plus the one mutating method.
pub struct GitHubClient {
    token: Option<String>,
    api_root: String,
    transport: Option<Transport>,
    timeout: Duration,
}

impl GitHubClient {
    pub fn api_root(&self) -> &str {
        &self.api_root
    }

    pub fn token(&self) -> Option<&str> {
        self.token.as_deref()
    }

    /// Build a client, or `None` when the adapter is not configured. `None` is
    /// the ordinary case: no token file means forge subjects are not collected.
    pub fn from_token_file(path: Option<&Path>, transport: Option<Transport>) -> Option<Self> {
        let path = path?;
        let token = std::fs::read_to_string(expand_user(path)).ok()?;
        let token = token.trim().to_string();
        if token.is_empty() {
            return None;
        }
        Some(Self {
            token: Some(token),
            api_root: API_ROOT.to_string(),
            transport,
            timeout: DEFAULT_TIMEOUT,
        })
    }

    pub fn with_transport(transport: Transport) -> Self {
        Self {
            token: None,
            api_root: API_ROOT.to_string(),
            transport: Some(transport),
            timeout: DEFAULT_TIMEOUT,
        }
    }

    /// GET one resource. A 404 is `Missing`, a 204 is `Empty`, and a 403 is
    /// `github_unavailable` rather than a silent empty answer.
    pub fn get(&self, path: &str, query: &[(&str, String)]) -> Result<ForgeResponse, VogtError> {
        let url = self.url(path, query);
        let (status, body, _) = self.fetch(&url, &self.headers(false), b"", "GET")?;
        self.read_outcome(path, status, &body)
    }

    /// Make a change upstream. The only mutating method, and deliberately not
    /// called `request`: every call site that changes somebody else's data is
    /// greppable, and there is no DELETE anywhere.
    pub fn send(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Value, VogtError> {
        if method != "POST" && method != "PATCH" {
            return Err(VogtError::GitHubUnavailable(format!(
                "{method} is not an additive operation; write-back is forward-only"
            )));
        }
        let url = self.url(path, &[]);
        let encoded = body.map(|value| value.to_string()).unwrap_or_default();
        let (status, response, _) =
            self.fetch(&url, &self.headers(true), encoded.as_bytes(), method)?;
        if status == 404 {
            return Ok(Value::Null);
        }
        if status >= 400 {
            return Err(VogtError::GitHubUnavailable(format!(
                "GitHub returned {status} for {method} {path}"
            )));
        }
        if response.iter().all(u8::is_ascii_whitespace) {
            return Ok(Value::Object(serde_json::Map::new()));
        }
        serde_json::from_slice(&response).map_err(|_| {
            VogtError::GitHubUnavailable(format!(
                "GitHub answered {method} {path} with something that is not JSON"
            ))
        })
    }

    /// Who this token is, or `None` when it is invalid (401/403/404). The one
    /// place a token is validated rather than merely used.
    pub fn identity(&self) -> Result<Option<GitHubIdentity>, VogtError> {
        let url = self.url("/user", &[]);
        let (status, body, headers) = self.fetch(&url, &self.headers(false), b"", "GET")?;
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
        if let Some(token) = &self.token {
            headers.insert("Authorization".to_string(), format!("Bearer {token}"));
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

    #[allow(clippy::type_complexity)]
    fn fetch(
        &self,
        url: &str,
        headers: &BTreeMap<String, String>,
        body: &[u8],
        method: &str,
    ) -> Result<(u16, Vec<u8>, BTreeMap<String, String>), VogtError> {
        if let Some(transport) = &self.transport {
            // The transport seam carries no response headers, so a caller that
            // needs one degrades honestly.
            let (status, body, response_headers) = transport(url, headers, body, method);
            return Ok((status, body, response_headers));
        }
        match super::engine::http1::exchange(url, method, headers, body, self.timeout) {
            Ok((status, body)) => Ok((status, body, BTreeMap::new())),
            Err(error) => Err(VogtError::GitHubUnavailable(format!(
                "GitHub unreachable: {error}"
            ))),
        }
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

    fn get(&self, path: &str, query: &[(&str, String)]) -> Result<ForgeResponse, VogtError> {
        self.get(path, query)
    }

    fn identity(&self) -> Result<Option<(String, String)>, VogtError> {
        Ok(GitHubClient::identity(self)?.map(|who| (who.login, who.scopes)))
    }

    fn send(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Value, VogtError> {
        self.send(method, path, body)
    }
}

#[allow(unused_imports)]
pub use forge::api_path;

/// Extract `(owner, repo)` from a project's repository URL. A project with no
/// GitHub URL is not an error — it is a project that does not live on GitHub.
pub fn repo_of(repo_url: Option<&str>) -> Option<(String, String)> {
    let candidate = repo_url?.trim().trim_start_matches("git+");
    if candidate.is_empty() {
        return None;
    }
    let candidate = candidate.replace("git@github.com:", "github.com/");
    let candidate = ["https://", "http://", "ssh://"]
        .iter()
        .find_map(|prefix| candidate.strip_prefix(prefix))
        .unwrap_or(&candidate);
    // A query or fragment carries injection metacharacters into no legitimate
    // repo URL, so its presence is itself disqualifying.
    if candidate.contains('?') || candidate.contains('#') {
        return None;
    }
    let (host, path) = candidate.split_once('/')?;
    let host = host.split('@').next_back().unwrap_or(host);
    if host != SUPPORTED_HOST {
        return None;
    }
    let path = path.trim_end_matches(".git").trim_matches('/');
    let mut parts = path.split('/');
    let owner = parts.next().unwrap_or("");
    let repo = parts.next().unwrap_or("");
    if !valid_name(owner) || !valid_name(repo) {
        return None;
    }
    Some((owner.to_string(), repo.to_string()))
}

/// What a forge permits in an owner or repository name. Owner and repo are
/// interpolated into URL builds, so a value carrying `..`, `?`, `#` or `%`
/// would steer the stored credential's request.
fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphanumeric() => {}
        _ => return false,
    }
    chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
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

fn expand_user(path: &Path) -> std::path::PathBuf {
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
    fn send_refuses_anything_but_post_and_patch() {
        let (_, client) = scripted(200, "{}");
        assert!(matches!(
            client.send("DELETE", "/repos/org/vogt", None).unwrap_err(),
            VogtError::GitHubUnavailable(_)
        ));
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
        assert!(GitHubClient::from_token_file(None, None).is_none());
    }
}
