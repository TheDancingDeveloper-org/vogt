//! A small client for a peer Vogt instance's diagnostics.
//!
//! Ports `src/vogt/adapters/peer.py`. A dev instance asking prod "what are you
//! running, and are you well" through the peer's own REST surface. The token
//! is read from a file, never from argv or a URL, and the answer is returned as
//! parsed JSON and never interpreted.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use serde_json::Value;

use crate::errors::VogtError;

pub const USER_AGENT: &str = "vogt";
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// Where a peer serves its diagnostics, relative to its REST base.
pub const DIAGNOSTICS_PATH: &str = "/instance/diagnostics";

/// Ceiling on a peer's answer. Diagnostics are a few kilobytes.
pub const MAX_RESPONSE_BYTES: usize = 512 * 1024;

/// How the client talks, so tests never need a peer. Same shape as the engine
/// client's transport.
pub type Transport = Box<dyn Fn(&str, &BTreeMap<String, String>, &[u8], &str) -> (u16, Vec<u8>)>;

/// Access to one peer instance's REST surface.
pub struct PeerClient {
    base_url: String,
    token: Option<String>,
    transport: Option<Transport>,
    timeout: Duration,
}

impl PeerClient {
    /// Build a client, or `None` when no peer is configured.
    pub fn from_config(
        url: Option<&str>,
        token_file: Option<&Path>,
        transport: Option<Transport>,
    ) -> Option<Self> {
        let url = url.map(str::trim).filter(|url| !url.is_empty())?;
        let token = token_file.and_then(read_token_file);
        Some(Self {
            base_url: url.trim_end_matches('/').to_string(),
            token,
            transport,
            timeout: DEFAULT_TIMEOUT,
        })
    }

    /// The peer's own `instance.diagnostics`, without asking it for *its* peer.
    pub fn diagnostics(&self, log_lines: i64) -> Result<Value, VogtError> {
        let url = format!(
            "{}{DIAGNOSTICS_PATH}?peer=false&log_lines={log_lines}",
            self.base_url
        );
        let mut headers = BTreeMap::new();
        headers.insert("Accept".to_string(), "application/json".to_string());
        headers.insert("User-Agent".to_string(), USER_AGENT.to_string());
        if let Some(token) = &self.token {
            headers.insert("Authorization".to_string(), format!("Bearer {token}"));
        }
        let (status, body) = self.fetch(&url, &headers)?;
        if status == 401 || status == 403 {
            return Err(refused(format!(
                "the peer refused the request ({status}): the token is missing, wrong, or lacks the read scope"
            )));
        }
        if status >= 400 {
            let kind = if status < 500 {
                "refused"
            } else {
                "unreachable"
            };
            return Err(peer_error(
                kind,
                format!("the peer answered {status} for {DIAGNOSTICS_PATH}"),
            ));
        }
        let payload: Value = serde_json::from_slice(&body).map_err(|_| {
            peer_error(
                "invalid_response",
                "the peer's answer is not JSON".to_string(),
            )
        })?;
        if !payload.is_object() {
            return Err(peer_error(
                "invalid_response",
                "the peer's answer is not a JSON object".to_string(),
            ));
        }
        Ok(payload)
    }

    fn fetch(
        &self,
        url: &str,
        headers: &BTreeMap<String, String>,
    ) -> Result<(u16, Vec<u8>), VogtError> {
        if let Some(transport) = &self.transport {
            let (status, mut body) = transport(url, headers, b"", "GET");
            body.truncate(MAX_RESPONSE_BYTES);
            return Ok((status, body));
        }
        match super::engine::http1::exchange(url, "GET", headers, b"", self.timeout) {
            Ok((status, mut body)) => {
                body.truncate(MAX_RESPONSE_BYTES);
                Ok((status, body))
            }
            Err(error) => Err(peer_error(
                "unreachable",
                format!("the peer is not answering: {error}"),
            )),
        }
    }
}

fn refused(message: String) -> VogtError {
    peer_error("refused", message)
}

fn peer_error(status: &str, message: String) -> VogtError {
    VogtError::PeerUnavailable {
        status: status.to_string(),
        message,
    }
}

fn read_token_file(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(expand_user(path)).ok()?;
    let trimmed = text.trim().to_string();
    (!trimmed.is_empty()).then_some(trimmed)
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

    fn client(status: u16, body: &'static [u8]) -> (Arc<Mutex<String>>, PeerClient) {
        let seen = Arc::new(Mutex::new(String::new()));
        let record = Arc::clone(&seen);
        let transport: Transport = Box::new(move |url, headers, _, method| {
            *record.lock().unwrap() = format!(
                "{method} {url} auth={}",
                headers.contains_key("Authorization")
            );
            (status, body.to_vec())
        });
        (
            seen,
            PeerClient::from_config(Some("http://peer.example/"), None, Some(transport)).unwrap(),
        )
    }

    #[test]
    fn diagnostics_asks_the_peer_endpoint_with_the_cap() {
        let (seen, client) = client(200, br#"{"version":"1"}"#);
        let payload = client.diagnostics(40).unwrap();
        assert_eq!(payload["version"], "1");
        assert_eq!(
            seen.lock().unwrap().as_str(),
            "GET http://peer.example/instance/diagnostics?peer=false&log_lines=40 auth=false"
        );
    }

    #[test]
    fn no_url_means_no_client() {
        assert!(PeerClient::from_config(Some("  "), None, None).is_none());
    }

    #[test]
    fn a_refusal_is_peer_unavailable_not_empty() {
        let (_, client) = client(401, b"");
        match client.diagnostics(1).unwrap_err() {
            VogtError::PeerUnavailable { status, .. } => assert_eq!(status, "refused"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_server_error_is_unreachable() {
        let (_, client) = client(503, b"");
        match client.diagnostics(1).unwrap_err() {
            VogtError::PeerUnavailable { status, .. } => assert_eq!(status, "unreachable"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_non_object_is_an_invalid_response() {
        let (_, client) = client(200, b"[1,2]");
        match client.diagnostics(1).unwrap_err() {
            VogtError::PeerUnavailable { status, .. } => assert_eq!(status, "invalid_response"),
            other => panic!("{other:?}"),
        }
    }
}
