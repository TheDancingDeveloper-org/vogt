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
    ///
    /// A missing path, a path that is not a file, and an empty file are all
    /// `Ok(None)`. A file that exists but cannot be read, or is not valid
    /// UTF-8, is an error: swallowing it would report a configured forge as
    /// unconfigured.
    pub fn from_token_file(
        path: Option<&Path>,
        transport: Option<Transport>,
    ) -> Result<Option<Self>, VogtError> {
        let Some(token) = read_token_file(path)? else {
            return Ok(None);
        };
        Ok(Some(Self::new(Some(token), transport)))
    }

    /// A client with an explicit token, or none. A token-less client is what
    /// identity and public-import use; it never reads a file. An empty token
    /// is the same as none.
    pub fn new(token: Option<String>, transport: Option<Transport>) -> Self {
        Self {
            token: token.filter(|token| !token.is_empty()),
            api_root: API_ROOT.to_string(),
            transport,
            timeout: DEFAULT_TIMEOUT,
        }
    }

    /// Whether `path` is a usable GitHub App private key: a file whose PEM
    /// parses as an RSA private key. Ports `GitHubAppAuth.from_key_file`'s
    /// gate, which is what makes `github_client_for` return a client when the
    /// App is configured even though no token file exists. A missing file, a
    /// directory, or a key that is not RSA is `false` — Python falls back to
    /// the token file in those cases, and a token file that is also absent
    /// means no credential.
    pub fn usable_app_key(path: &Path) -> bool {
        let Ok(bytes) = std::fs::read(path) else {
            return false;
        };
        rsa_private_key_pem(&bytes)
    }

    /// A token-less client whose requests go through `transport`.
    pub fn with_transport(transport: Transport) -> Self {
        Self::new(None, Some(transport))
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
    pub fn send(
        &self,
        method: WriteMethod,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Value, VogtError> {
        let verb = method.as_str();
        let url = self.url(path, &[]);
        let encoded = body.map(|value| value.to_string()).unwrap_or_default();
        let (status, response, _) = self.fetch(
            &url,
            &self.headers(true),
            encoded.as_bytes(),
            method.as_str(),
        )?;
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
            let (status, body, response_headers) = transport(url, headers, body, method);
            return Ok((status, body, response_headers));
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
fn read_token_file(path: Option<&Path>) -> Result<Option<String>, VogtError> {
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
fn expand_user(path: &Path) -> std::path::PathBuf {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return Path::new(&home).join(rest);
        }
    }
    path.to_path_buf()
}

/// True when `bytes` is a PEM that decodes as an unencrypted RSA private key.
///
/// Ports `GitHubAppAuth.from_key_file`: `load_pem_private_key(..., password=None)`
/// then `isinstance(..., RSAPrivateKey)`. PKCS#1 (`RSA PRIVATE KEY`) and PKCS#8
/// (`PRIVATE KEY`) both count when the key is actually RSA. An encrypted key, an
/// EC or other non-RSA key, and a body that does not decode are `false`. The
/// parse error is dropped: its text can echo key material, and this gate never
/// logs or returns it.
fn rsa_private_key_pem(bytes: &[u8]) -> bool {
    use rsa::pkcs1::DecodeRsaPrivateKey;
    use rsa::pkcs8::DecodePrivateKey;
    use rsa::RsaPrivateKey;
    RsaPrivateKey::from_pkcs1_pem(std::str::from_utf8(bytes).unwrap_or(""))
        .or_else(|_| RsaPrivateKey::from_pkcs8_pem(std::str::from_utf8(bytes).unwrap_or("")))
        .is_ok()
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
    fn usable_app_key_decodes_rsa_and_rejects_the_rest() {
        let dir = std::env::temp_dir().join(format!("vogt-app-key-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        use rsa::pkcs1::EncodeRsaPrivateKey;
        use rsa::pkcs8::EncodePrivateKey;
        let mut rng = rsa::rand_core::OsRng;
        let key = rsa::RsaPrivateKey::new(&mut rng, 2048).unwrap();
        let pkcs1 = dir.join("pkcs1.pem");
        std::fs::write(
            &pkcs1,
            key.to_pkcs1_pem(rsa::pkcs8::LineEnding::LF).unwrap(),
        )
        .unwrap();
        assert!(GitHubClient::usable_app_key(&pkcs1));
        let pkcs8 = dir.join("pkcs8.pem");
        std::fs::write(
            &pkcs8,
            key.to_pkcs8_pem(rsa::pkcs8::LineEnding::LF).unwrap(),
        )
        .unwrap();
        assert!(GitHubClient::usable_app_key(&pkcs8));
        let encrypted = dir.join("encrypted.pem");
        std::fs::write(
            &encrypted,
            "-----BEGIN RSA PRIVATE KEY-----\nProc-Type: 4,ENCRYPTED\nDEK-Info: AES-256-CBC,00000000000000000000000000000000\n\nAQIDBA==\n-----END RSA PRIVATE KEY-----\n",
        )
        .unwrap();
        assert!(!GitHubClient::usable_app_key(&encrypted));
        let ec = dir.join("ec.pem");
        std::fs::write(
            &ec,
            "-----BEGIN EC PRIVATE KEY-----\nMHQCAQEEIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAoE\n-----END EC PRIVATE KEY-----\n",
        )
        .unwrap();
        assert!(!GitHubClient::usable_app_key(&ec));
        let garbage = dir.join("garbage.pem");
        std::fs::write(
            &garbage,
            "-----BEGIN RSA PRIVATE KEY-----\nnot-a-key\n-----END RSA PRIVATE KEY-----\n",
        )
        .unwrap();
        assert!(!GitHubClient::usable_app_key(&garbage));
        assert!(!GitHubClient::usable_app_key(&dir.join("missing.pem")));
        let _ = std::fs::remove_dir_all(&dir);
    }

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
