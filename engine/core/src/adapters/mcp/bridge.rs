//! `vogt-mcp-remote` — a stdio bridge to a remote Vogt. Ports
//! `adapters/mcp/bridge.py`.
//!
//! Some agent products can only spawn a local process; they cannot open an HTTP
//! MCP session. This bridge is the shim: stdio in, streamable HTTP out.
//!
//! It hardcodes no tools. It forwards everything and learns the tool count from
//! the client's own `tools/list` as it passes, so there is nothing here to
//! drift against the registry.
//!
//! Two rules:
//!
//! - **Version skew warns; it never blocks.** One line on stderr, and startup
//!   proceeds. A bridge that refuses to start because the server is a patch
//!   ahead turns a warning into an outage.
//! - **stdout is the framing channel.** Every diagnostic goes to stderr. A
//!   warning on stdout corrupts the stream and looks like a client bug.

use std::collections::HashMap;

use serde_json::{json, Value};

use super::framing::SUPPORTED_PROTOCOL_VERSIONS;

const URL_ENV: &str = "VOGT_URL";
const TOKEN_FILE_ENV: &str = "VOGT_TOKEN_FILE";
/// Set by a coding session for the agent it starts, and by the container's
/// auth broker for everything else. See [`resolve_token`] for which wins.
const HTTP_TOKEN_ENV: &str = "VOGT_HTTP_TOKEN";

/// One HTTP exchange. Injected in tests; the live transport is not this layer's
/// to own, because the HTTP client is WI-1064's.
pub trait BridgeTransport {
    fn exchange(
        &self,
        url: &str,
        headers: &[(&str, String)],
        body: &[u8],
    ) -> Result<(u16, Vec<u8>), String>;
}

/// What one bridge session did.
#[derive(Debug, Default)]
pub struct BridgeReport {
    pub messages_forwarded: u64,
    pub remote_tools: usize,
    pub remote_version: Option<String>,
    pub warned: Vec<String>,
}

/// Forwards newline-delimited JSON-RPC between stdio and a remote.
pub struct Bridge<'a, T: BridgeTransport> {
    url: String,
    token: Option<String>,
    transport: &'a T,
    version: &'a str,
    discovered: bool,
    announced: bool,
    pub report: BridgeReport,
}

impl<'a, T: BridgeTransport> Bridge<'a, T> {
    pub fn new(url: &str, token: Option<String>, transport: &'a T, version: &'a str) -> Self {
        Self {
            url: url.trim_end_matches('/').to_owned(),
            token,
            transport,
            version,
            discovered: false,
            announced: false,
            report: BridgeReport::default(),
        }
    }

    /// One line, recorded and never fatal.
    fn warn(&mut self, text: impl Into<String>) {
        self.report.warned.push(text.into());
    }

    /// Ask the remote what it is. A failure here is a warning, not an exit: an
    /// agent that starts before its server is up should reconnect on the first
    /// real call rather than die at launch.
    pub fn discover(&mut self) {
        let (status, body) = match self.get(&format!("{}/connection-info", self.url)) {
            Ok(response) => response,
            Err(error) => {
                self.warn(format!("could not reach {}: {error}", self.url));
                return;
            }
        };
        if status != 200 {
            self.warn(format!("{}/connection-info returned {status}", self.url));
            return;
        }
        // A 200 is not a promise that the body is ours. Behind the merged front
        // door the PWA's index answers this path, and discovery dying over a
        // banner it did not need took the bridge down while `/mcp` was fine.
        let Ok(info) = serde_json::from_slice::<Value>(&body) else {
            self.warn(format!(
                "{}/connection-info returned 200 but not JSON; skipping discovery \
                 and forwarding anyway",
                self.url
            ));
            return;
        };
        let Some(info) = info.as_object() else {
            self.warn(format!(
                "{}/connection-info returned JSON that is not an object; skipping \
                 discovery and forwarding anyway",
                self.url
            ));
            return;
        };
        let remote_version = info.get("version").and_then(Value::as_str).unwrap_or("");
        if !remote_version.is_empty() {
            self.report.remote_version = Some(remote_version.to_owned());
            if remote_version != self.version {
                self.warn(format!(
                    "version skew: bridge {}, server {remote_version}. Continuing.",
                    self.version
                ));
            }
        }
        let remote_versions = info
            .get("supported_mcp_protocol_versions")
            .and_then(Value::as_array);
        if let Some(remote_versions) = remote_versions {
            let shared = remote_versions.iter().any(|version| {
                version
                    .as_str()
                    .is_some_and(|version| SUPPORTED_PROTOCOL_VERSIONS.contains(&version))
            });
            if !remote_versions.is_empty() && !shared {
                self.warn(format!(
                    "no MCP protocol version in common: bridge supports {}, server \
                     supports {}. Continuing.",
                    SUPPORTED_PROTOCOL_VERSIONS.join(", "),
                    remote_versions
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        }
    }

    /// Learn the tool count from the client's own `tools/list`. Not stored, not
    /// filtered, not remembered: the bridge forwards whatever the server
    /// accepts. Caching the list here is how a bridge starts lying about what
    /// a server can do.
    fn note_tools(&mut self, message: &Value, response: &Value) {
        if message.get("method").and_then(Value::as_str) != Some("tools/list") {
            return;
        }
        let Some(tools) = response.pointer("/result/tools").and_then(Value::as_array) else {
            return;
        };
        self.report.remote_tools = tools.len();
        if !self.announced {
            self.announced = true;
            self.warn(format!("connected: {} tools available", tools.len()));
        }
    }

    /// Forward `input` to the remote, appending each response to `output`.
    ///
    /// The client's first message is answered before discovery runs. Discovery
    /// is a courtesy on stderr; the handshake is the contract, and a bridge
    /// that pre-flights before reading stdin fails to connect precisely when
    /// the core is slow.
    pub fn serve(&mut self, input: &str, output: &mut String) {
        for line in input.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            self.report.messages_forwarded += 1;
            let message = match serde_json::from_str::<Value>(line) {
                Ok(message) => message,
                Err(error) => {
                    self.write(
                        output,
                        &json!({"jsonrpc": "2.0", "id": null,
                            "error": {"code": -32700, "message": format!("invalid JSON: {error}")}}),
                    );
                    continue;
                }
            };
            if let Some(response) = self.forward(&message) {
                self.note_tools(&message, &response);
                self.write(output, &response);
            }
            if !self.discovered {
                self.discovered = true;
                self.discover();
            }
        }
    }

    fn forward(&self, message: &Value) -> Option<Value> {
        let (status, body) = match self.post(&format!("{}/mcp", self.url), message) {
            Ok(response) => response,
            Err(error) => {
                // A notification expects no answer, so an unreachable server
                // stays silent for one. A null id is a notification too:
                // Python's `message.get("id") is None` is true for both an
                // absent key and a JSON null. A request gets the error.
                return message.get("id").filter(|id| !id.is_null()).map(|id| {
                    json!({"jsonrpc": "2.0", "id": id,
                        "error": {"code": -32000, "message": format!("vogt unreachable: {error}")}})
                });
            }
        };
        // Checked before the empty body: a 401 often has none, and treating it
        // as "no response" leaves the client waiting forever for an answer
        // that was refused.
        if status == 401 || status == 403 {
            return Some(json!({
                "jsonrpc": "2.0",
                "id": message.get("id"),
                "error": {"code": -32001,
                    "message": "the server rejected this token; check VOGT_TOKEN_FILE"},
            }));
        }
        if status == 202 || body.iter().all(u8::is_ascii_whitespace) {
            return None;
        }
        // A non-JSON body (a 500 HTML page) must not vanish: the client is
        // waiting on this id, and silence looks like a hung server.
        Some(serde_json::from_slice(&body).unwrap_or_else(|_| {
            json!({"jsonrpc": "2.0", "id": message.get("id"),
                "error": {"code": -32000,
                    "message": "the remote Vogt returned a non-JSON body"}})
        }))
    }

    fn write(&self, output: &mut String, message: &Value) {
        output.push_str(&message.to_string());
        output.push('\n');
    }

    fn headers(&self) -> Vec<(&str, String)> {
        let mut headers = vec![
            ("Content-Type", "application/json".to_owned()),
            ("Accept", "application/json".to_owned()),
        ];
        if let Some(token) = &self.token {
            headers.push(("Authorization", format!("Bearer {token}")));
        }
        headers
    }

    fn get(&self, url: &str) -> Result<(u16, Vec<u8>), String> {
        self.transport.exchange(url, &self.headers(), b"")
    }

    fn post(&self, url: &str, message: &Value) -> Result<(u16, Vec<u8>), String> {
        self.transport
            .exchange(url, &self.headers(), message.to_string().as_bytes())
    }
}

/// Read a token from a file. Never from argv or a URL.
pub fn read_token(path: Option<&str>) -> Option<String> {
    let path = path.filter(|path| !path.is_empty())?;
    let resolved = if let Some(rest) = path.strip_prefix("~/") {
        std::env::var_os("HOME")
            .map_or_else(|| path.into(), |home| format!("{}/{rest}", home.display()))
    } else {
        path.to_owned()
    };
    let text = std::fs::read_to_string(resolved).ok()?;
    let text = text.trim().to_owned();
    (!text.is_empty()).then_some(text)
}

/// The token this bridge should present, and which one wins.
///
/// Two sources, because two things provision one. A container brokers a shared
/// token into a file and points `VOGT_TOKEN_FILE` at it; a coding session hands
/// its own token to the process it starts, in `VOGT_HTTP_TOKEN`. Inside a
/// session the session's token wins: what the agent writes must be attributable
/// to *this* session, and falling back to the shared container token would file
/// every session's work under one identity while looking like it worked.
pub fn resolve_token(env: &HashMap<String, String>) -> Option<String> {
    // An empty session id is not a session. Inside a real one the session
    // token is the only acceptable source: a whitespace-only token resolves to
    // nothing rather than falling back to the shared file, which would file
    // this session's writes under another identity.
    if env.get("VOGT_SESSION_ID").is_some_and(|id| !id.is_empty()) {
        return env
            .get(HTTP_TOKEN_ENV)
            .map(|token| token.trim())
            .filter(|token| !token.is_empty())
            .map(str::to_owned);
    }
    if let Some(token) = read_token(env.get(TOKEN_FILE_ENV).map(String::as_str)) {
        return Some(token);
    }
    env.get(HTTP_TOKEN_ENV)
        .map(|token| token.trim())
        .filter(|token| !token.is_empty())
        .map(str::to_owned)
}

/// What `main` needs and cannot have: the URL. `None` is the exit-2 case.
pub fn configured_url(env: &HashMap<String, String>) -> Option<String> {
    env.get(URL_ENV).filter(|url| !url.is_empty()).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    struct Scripted {
        answers: RefCell<Vec<(u16, Vec<u8>)>>,
        seen: RefCell<Vec<String>>,
        fail: bool,
    }

    impl BridgeTransport for Scripted {
        fn exchange(
            &self,
            url: &str,
            headers: &[(&str, String)],
            _body: &[u8],
        ) -> Result<(u16, Vec<u8>), String> {
            self.seen.borrow_mut().push(format!(
                "{url} auth={}",
                headers.iter().any(|(name, _)| *name == "Authorization")
            ));
            if self.fail {
                return Err("connection refused".to_owned());
            }
            Ok(self.answers.borrow_mut().pop().unwrap_or((200, Vec::new())))
        }
    }

    fn bridge<'a>(transport: &'a Scripted) -> Bridge<'a, Scripted> {
        Bridge::new(
            "https://vogt.example/",
            Some("secret".into()),
            transport,
            "0.1.0",
        )
    }

    #[test]
    fn the_first_message_is_answered_before_discovery() {
        let transport = Scripted {
            answers: RefCell::new(vec![
                (
                    200,
                    br#"{"version":"9.9.9","supported_mcp_protocol_versions":["1999-01-01"]}"#
                        .to_vec(),
                ),
                (
                    200,
                    br#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-06-18"}}"#
                        .to_vec(),
                ),
            ]),
            seen: RefCell::new(Vec::new()),
            fail: false,
        };
        let mut output = String::new();
        let mut bridge = bridge(&transport);
        bridge.serve(
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\"}\n",
            &mut output,
        );
        assert!(output.contains("2025-06-18"), "{output}");
        let seen = transport.seen.borrow();
        assert!(
            seen[0].contains("/mcp"),
            "the handshake goes first: {seen:?}"
        );
        assert!(seen[1].contains("/connection-info"), "{seen:?}");
        assert!(seen.iter().all(|line| line.ends_with("auth=true")));
        assert!(bridge
            .report
            .warned
            .iter()
            .any(|warning| warning.contains("version skew")));
        assert!(bridge
            .report
            .warned
            .iter()
            .any(|warning| warning.contains("no MCP protocol")));
    }

    #[test]
    fn a_refused_token_is_an_error_even_with_an_empty_body() {
        let transport = Scripted {
            answers: RefCell::new(vec![(401, Vec::new())]),
            seen: RefCell::new(Vec::new()),
            fail: false,
        };
        let mut output = String::new();
        bridge(&transport).serve(
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n",
            &mut output,
        );
        assert!(output.contains("-32001"), "{output}");
        assert!(output.contains("VOGT_TOKEN_FILE"), "{output}");
    }

    #[test]
    fn an_unreachable_server_answers_a_request_and_stays_silent_for_a_notification() {
        let transport = Scripted {
            answers: RefCell::new(Vec::new()),
            seen: RefCell::new(Vec::new()),
            fail: true,
        };
        let mut output = String::new();
        let mut bridge = bridge(&transport);
        bridge.serve(
            "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n\
             {\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"ping\"}\n",
            &mut output,
        );
        let lines: Vec<&str> = output.lines().collect();
        assert_eq!(lines.len(), 1, "{output}");
        assert!(lines[0].contains("-32000"), "{output}");
        assert!(bridge
            .report
            .warned
            .iter()
            .any(|warning| warning.contains("could not reach")));
    }

    #[test]
    fn discovery_survives_a_body_that_is_not_json() {
        let transport = Scripted {
            answers: RefCell::new(vec![
                (200, b"<html>index</html>".to_vec()),
                (202, Vec::new()),
            ]),
            seen: RefCell::new(Vec::new()),
            fail: false,
        };
        let mut output = String::new();
        let mut bridge = bridge(&transport);
        bridge.serve(
            "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n",
            &mut output,
        );
        assert!(output.is_empty(), "{output}");
        assert!(bridge
            .report
            .warned
            .iter()
            .any(|warning| warning.contains("not JSON")));
    }

    #[test]
    fn the_tool_count_comes_from_the_clients_own_list() {
        let transport = Scripted {
            answers: RefCell::new(vec![
                (200, b"{}".to_vec()),
                (
                    200,
                    br#"{"jsonrpc":"2.0","id":1,"result":{"tools":[{},{}]}}"#.to_vec(),
                ),
            ]),
            seen: RefCell::new(Vec::new()),
            fail: false,
        };
        let mut output = String::new();
        let mut bridge = bridge(&transport);
        bridge.serve(
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\"}\n",
            &mut output,
        );
        assert_eq!(bridge.report.remote_tools, 2);
        assert!(bridge
            .report
            .warned
            .iter()
            .any(|warning| warning.contains("2 tools")));
    }

    #[test]
    fn the_session_token_wins_over_the_file() {
        let mut env = HashMap::new();
        env.insert("VOGT_SESSION_ID".into(), "ses_1".into());
        env.insert(HTTP_TOKEN_ENV.into(), " session-token ".into());
        env.insert(TOKEN_FILE_ENV.into(), "/no/such/file".into());
        assert_eq!(resolve_token(&env).as_deref(), Some("session-token"));

        env.remove("VOGT_SESSION_ID");
        env.insert(HTTP_TOKEN_ENV.into(), "fallback".into());
        assert_eq!(resolve_token(&env).as_deref(), Some("fallback"));

        env.insert(HTTP_TOKEN_ENV.into(), "  ".into());
        assert!(resolve_token(&env).is_none());
        assert!(configured_url(&HashMap::new()).is_none());
    }
}
