//! A small Forgejo/Gitea API client. Ports `src/vogt/adapters/forge/forgejo.py`.
//!
//! Its own client rather than a reuse of [`super::github::GitHubClient`]: the
//! API root lives under the forge's own domain, the auth header is `token …`
//! rather than `Bearer …`, and the errors name Forgejo. The shape — token from
//! a file, a transport seam for tests, 404 as missing, one greppable mutating
//! method — matches the GitHub client so both implement [`ForgeTransport`].

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use serde_json::Value;

use crate::adapters::forge::{ForgeResponse, ForgeTransport, WriteMethod};
use crate::errors::VogtError;

pub const USER_AGENT: &str = "vogt";
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(20);

/// The status, body and lower-cased response headers of one exchange.
type Exchange = (u16, Vec<u8>, BTreeMap<String, String>);

/// How the client talks, so tests never need a network.
pub type Transport = crate::adapters::github::Transport;

/// Thin access to one Forgejo/Gitea installation's `/api/v1`.
pub struct ForgejoClient {
    host: String,
    /// `https://{host}/api/v1`, stored so the trait can hand back a borrow.
    api_root: String,
    token: Option<String>,
    transport: Option<Transport>,
    timeout: Duration,
}

impl ForgejoClient {
    /// A client for `host`, with an explicit token or none. A token-less client
    /// is what identity and public-import use; it never reads a file.
    pub fn new(
        host: impl Into<String>,
        token: Option<String>,
        transport: Option<Transport>,
    ) -> Self {
        let host = host.into();
        let api_root = format!("https://{host}/api/v1");
        Self {
            host,
            api_root,
            token: token.filter(|token| !token.is_empty()),
            transport,
            timeout: DEFAULT_TIMEOUT,
        }
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn api_root(&self) -> &str {
        &self.api_root
    }

    pub fn token(&self) -> Option<&str> {
        self.token.as_deref()
    }

    /// Build a client, or `None` when this host is not configured. `None` is
    /// the ordinary case: no token file means the host's subjects are not
    /// collected.
    ///
    /// A missing path, a path that is not a file, and an empty file are all
    /// `Ok(None)`. A file that exists but cannot be read, or is not valid
    /// UTF-8, is an error: swallowing it would report a configured forge as
    /// unconfigured.
    pub fn from_token_file(
        host: impl Into<String>,
        path: Option<&Path>,
        transport: Option<Transport>,
    ) -> Result<Option<Self>, VogtError> {
        let Some(token) = read_token_file(path)? else {
            return Ok(None);
        };
        Ok(Some(Self::new(host, Some(token), transport)))
    }

    pub fn get(&self, path: &str, query: &[(&str, String)]) -> Result<ForgeResponse, VogtError> {
        let url = self.url(path, query);
        let (status, body, _) = self.fetch(&url, &self.headers(false), b"", "GET")?;
        self.read_outcome(path, status, &body)
    }

    /// Make a change upstream. POST and PATCH only: every call site that
    /// changes somebody else's data is greppable, and there is no DELETE.
    pub fn send(
        &self,
        method: WriteMethod,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Value, VogtError> {
        let verb = method.as_str();
        let url = self.url(path, &[]);
        let encoded = body.map(|value| value.to_string()).unwrap_or_default();
        let (status, response, _) =
            self.fetch(&url, &self.headers(true), encoded.as_bytes(), verb)?;
        if status == 404 {
            return Ok(Value::Null);
        }
        if status >= 400 {
            return Err(VogtError::ForgeHttpStatus {
                status,
                forge: Some(crate::errors::Forge::Forgejo),
                message: format!("{} returned {status} for {verb} {path}", self.host),
            });
        }
        if response.iter().all(u8::is_ascii_whitespace) {
            return Ok(Value::Object(serde_json::Map::new()));
        }
        serde_json::from_slice(&response).map_err(|_| {
            VogtError::ForgejoUnavailable(format!(
                "{} answered {verb} {path} with something that is not JSON",
                self.host
            ))
        })
    }

    /// Who this token is. A rejected token is an error, the same one `get` raises;
    /// `None` is only a 404 or a body with no login. Forgejo reports no scope
    /// header, so the second half stays empty.
    pub fn identity(&self) -> Result<Option<(String, String)>, VogtError> {
        // The same read the provider makes: 401 and 403 come back as errors
        // through `get`, and only a missing user or a body with no login is
        // `None`. Forgejo reports no scope header, so the second half is empty.
        let ForgeResponse::Json(Value::Object(user)) = self.get("/user", &[])? else {
            return Ok(None);
        };
        Ok(user
            .get("login")
            .and_then(Value::as_str)
            .filter(|login| !login.is_empty())
            .map(|login| (login.to_owned(), String::new())))
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
            return Err(VogtError::ForgejoUnavailable(format!(
                "{} refused {path} (403): rate limited or unauthorised",
                self.host
            )));
        }
        if status >= 400 {
            return Err(VogtError::ForgejoUnavailable(format!(
                "{} returned {status} for {path}",
                self.host
            )));
        }
        if status == 204 || body.iter().all(u8::is_ascii_whitespace) {
            return Ok(ForgeResponse::Empty);
        }
        serde_json::from_slice(body)
            .map(ForgeResponse::Json)
            .map_err(|_| {
                VogtError::ForgejoUnavailable(format!(
                    "{} answered {path} with something that is not JSON",
                    self.host
                ))
            })
    }

    fn headers(&self, with_body: bool) -> BTreeMap<String, String> {
        let mut headers = BTreeMap::new();
        headers.insert("Accept".to_string(), "application/json".to_string());
        headers.insert("User-Agent".to_string(), USER_AGENT.to_string());
        if with_body {
            headers.insert("Content-Type".to_string(), "application/json".to_string());
        }
        if let Some(token) = &self.token {
            // Forgejo/Gitea's documented scheme is `token {token}`, not Bearer.
            headers.insert("Authorization".to_string(), format!("token {token}"));
        }
        headers
    }

    fn url(&self, path: &str, query: &[(&str, String)]) -> String {
        let mut url = format!("{}{path}", self.api_root().trim_end_matches('/'));
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

    fn fetch(
        &self,
        url: &str,
        headers: &BTreeMap<String, String>,
        body: &[u8],
        method: &str,
    ) -> Result<Exchange, VogtError> {
        if let Some(transport) = &self.transport {
            let (status, body, response_headers) = transport(url, headers, body, method);
            return Ok((status, body, response_headers));
        }
        super::engine::http1::exchange(url, method, headers, body, self.timeout).map_err(|error| {
            VogtError::ForgejoUnavailable(format!("{} unreachable: {error}", self.host))
        })
    }
}

impl ForgeTransport for ForgejoClient {
    fn api_root(&self) -> &str {
        ForgejoClient::api_root(self)
    }

    fn token(&self) -> Option<&str> {
        ForgejoClient::token(self)
    }

    fn get(&self, path: &str, query: &[(&str, String)]) -> Result<ForgeResponse, VogtError> {
        ForgejoClient::get(self, path, query)
    }

    fn identity(&self) -> Result<Option<(String, String)>, VogtError> {
        ForgejoClient::identity(self)
    }

    fn send(
        &self,
        method: WriteMethod,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Value, VogtError> {
        ForgejoClient::send(self, method, path, body)
    }
}

fn quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
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
        VogtError::ForgejoUnavailable(format!(
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

#[cfg(test)]
mod tests {
    use super::*;

    fn client(transport: Transport) -> ForgejoClient {
        ForgejoClient::new(
            "forge.example",
            Some("fj-token".to_owned()),
            Some(transport),
        )
    }

    fn answering(status: u16, body: &'static [u8]) -> Transport {
        Box::new(move |url, headers, _body, method| {
            assert!(url.starts_with("https://forge.example/api/v1"), "{url}");
            assert_eq!(
                headers.get("Authorization").map(String::as_str),
                Some("token fj-token")
            );
            assert_eq!(headers.get("User-Agent").map(String::as_str), Some("vogt"));
            let _ = method;
            (status, body.to_vec(), BTreeMap::new())
        })
    }

    #[test]
    fn the_api_root_is_the_hosts_own() {
        let client = ForgejoClient::new("forge.example", None, None);
        assert_eq!(client.api_root(), "https://forge.example/api/v1");
        assert_eq!(ForgeTransport::api_root(&client), client.api_root());
        assert!(client.token().is_none());
    }

    #[test]
    fn an_empty_token_is_no_token() {
        let client = ForgejoClient::new("forge.example", Some(String::new()), None);
        assert!(client.token().is_none());
    }

    #[test]
    fn get_reads_missing_empty_and_refused() {
        match client(answering(404, b"")).get("/repos/a/b", &[]).unwrap() {
            ForgeResponse::Missing => {}
            other => panic!("expected missing, got {other:?}"),
        }
        match client(answering(204, b"")).get("/repos/a/b", &[]).unwrap() {
            ForgeResponse::Empty => {}
            other => panic!("expected empty, got {other:?}"),
        }
        let refused = client(answering(403, b"no"))
            .get("/repos/a/b", &[])
            .unwrap_err();
        assert!(refused.to_string().contains("forge.example"), "{refused}");
        assert!(refused.to_string().contains("403"), "{refused}");
    }

    #[test]
    fn identity_follows_get_so_a_rejected_token_is_an_error() {
        // 401 is not "no identity": `get` raises, and this read goes through it.
        let rejected = client(answering(401, br#"{"message":"bad"}"#))
            .identity()
            .unwrap_err();
        assert!(rejected.to_string().contains("401"), "{rejected}");
        // Forgejo reports no scope header, so a known user still has empty scopes.
        let known = client(Box::new(|_url, _headers, _body, _method| {
            let mut headers = BTreeMap::new();
            headers.insert("x-oauth-scopes".to_owned(), "read:user".to_owned());
            (200, br#"{"login":"ada"}"#.to_vec(), headers)
        }));
        assert_eq!(
            known.identity().unwrap(),
            Some(("ada".to_owned(), String::new()))
        );
        assert!(client(answering(404, b"")).identity().unwrap().is_none());
    }

    #[test]
    fn send_answers_the_verb_it_was_given() {
        let sent = client(answering(200, b"{}"))
            .send(WriteMethod::Patch, "/repos/a/b", None)
            .unwrap();
        assert_eq!(sent, serde_json::json!({}));
    }

    #[test]
    fn from_token_file_is_none_for_a_missing_or_empty_file() {
        assert!(ForgejoClient::from_token_file("forge.example", None, None)
            .unwrap()
            .is_none());
        assert!(
            ForgejoClient::from_token_file("forge.example", Some(Path::new("/no/such")), None)
                .unwrap()
                .is_none()
        );

        let dir = std::env::temp_dir().join(format!("vogt-forgejo-token-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let empty = dir.join("empty");
        std::fs::write(&empty, "  \n").unwrap();
        assert!(
            ForgejoClient::from_token_file("forge.example", Some(&empty), None)
                .unwrap()
                .is_none()
        );
        // A directory is a path, but not a token file.
        assert!(
            ForgejoClient::from_token_file("forge.example", Some(&dir), None)
                .unwrap()
                .is_none()
        );

        let present = dir.join("token");
        std::fs::write(&present, "  fj-token \n").unwrap();
        let built = ForgejoClient::from_token_file("forge.example", Some(&present), None)
            .unwrap()
            .unwrap();
        assert_eq!(built.token(), Some("fj-token"));

        let bad = dir.join("bad");
        std::fs::write(&bad, [0xff, 0xfe]).unwrap();
        let Err(error) = ForgejoClient::from_token_file("forge.example", Some(&bad), None) else {
            panic!("a non-UTF-8 token file must be an error");
        };
        assert!(error.to_string().contains("valid UTF-8"), "{error}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
