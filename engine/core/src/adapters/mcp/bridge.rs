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
use crate::adapters::http_client;

const URL_ENV: &str = "VOGT_URL";
const TOKEN_FILE_ENV: &str = "VOGT_TOKEN_FILE";
/// Set by a coding session for the agent it starts, and by the container's
/// auth broker for everything else. See [`resolve_token`] for which wins.
const HTTP_TOKEN_ENV: &str = "VOGT_HTTP_TOKEN";

/// The budget for the optional banner read. Discovery is a courtesy, and a
/// client gives the whole handshake about 30 s, so a pre-flight that may take
/// 30 s of it turns a slow core into "server failed to connect".
pub const DISCOVERY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// Everything else. A call that has not answered in 30 s is not going to.
pub const DEFAULT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// The read budget for one POST. A `tools/call` that asks the core to hold for
/// `timeout_s`, `wait_s` or `wake_timeout_s` seconds (session_wait, chat sends,
/// session_input) gets that wait plus [`CALL_MARGIN`]; every other call keeps
/// [`DEFAULT_TIMEOUT`]. Never shorter than the default.
pub fn call_timeout(body: &[u8]) -> std::time::Duration {
    let Ok(message) = serde_json::from_slice::<Value>(body) else {
        return DEFAULT_TIMEOUT;
    };
    if message.get("method").and_then(Value::as_str) != Some("tools/call") {
        return DEFAULT_TIMEOUT;
    }
    let arguments = &message["params"]["arguments"];
    let longest_wait = ["timeout_s", "wait_s", "wake_timeout_s"]
        .iter()
        .filter_map(|key| arguments.get(*key).and_then(Value::as_u64))
        .max()
        .unwrap_or(0);
    let budget = std::time::Duration::from_secs(longest_wait).saturating_add(CALL_MARGIN);
    DEFAULT_TIMEOUT.max(budget)
}

/// Headroom past a call's own wait, for the handshake and the core's reply.
pub const CALL_MARGIN: std::time::Duration = std::time::Duration::from_secs(30);

/// The streamable-HTTP session header. A server that issues one on
/// `initialize` refuses every later request that does not send it back.
const MCP_SESSION_HEADER: &str = "Mcp-Session-Id";

/// What one HTTP exchange answered.
#[derive(Debug, Default)]
pub struct Reply {
    pub status: u16,
    pub body: Vec<u8>,
    /// The `Mcp-Session-Id` response header, when the server sent one.
    pub mcp_session: Option<String>,
}

impl From<(u16, Vec<u8>)> for Reply {
    fn from((status, body): (u16, Vec<u8>)) -> Self {
        Self {
            status,
            body,
            mcp_session: None,
        }
    }
}

/// One HTTP exchange. Injected in tests; [`UreqTransport`] is the live one.
pub trait BridgeTransport {
    fn exchange(&self, url: &str, headers: &[(&str, String)], body: &[u8])
        -> Result<Reply, String>;
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
    /// The session the server issued on `initialize`, if it issues one, and
    /// the client's `initialize` that got it, for re-establishing a session
    /// the server has forgotten.
    mcp_session: Option<String>,
    initialize: Option<Value>,
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
            mcp_session: None,
            initialize: None,
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
            Ok(reply) => (reply.status, reply.body),
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
        // Python's `str()` renders a number, a bool and a string alike, so a
        // server that advertises `7` still triggers the skew warning.
        let remote_version = info.get("version").map(python_str).unwrap_or_default();
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
            self.serve_line(line, output);
        }
    }

    /// One line of the client's stdin. Empty lines are skipped; a line that is
    /// not JSON is answered with `-32700` rather than dropped, because the
    /// client is waiting on it.
    pub fn serve_line(&mut self, line: &str, output: &mut String) {
        let line = super::super::text::python_strip(line);
        if line.is_empty() {
            return;
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
                return;
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

    /// POST one message, keeping the server's MCP session (WI-4288).
    ///
    /// A streamable-HTTP server may issue an `Mcp-Session-Id` on `initialize`
    /// and refuse every later request without it (Cadastre does, with 400).
    /// Vogt's own `/mcp` issues none, and then nothing is sent. A client
    /// `initialize` starts over: the old session is not presented. A session
    /// the server no longer knows (404, or 400 naming the header — a restart)
    /// is re-established once by replaying the client's `initialize` and
    /// `notifications/initialized`; their answers are the bridge's and never
    /// reach stdout.
    fn exchange(&mut self, message: &Value) -> Result<Reply, String> {
        let url = format!("{}/mcp", self.url);
        let is_initialize = message.get("method").and_then(Value::as_str) == Some("initialize");
        if is_initialize {
            self.mcp_session = None;
            self.initialize = Some(message.clone());
        }
        let sent_session = self.mcp_session.is_some();
        let reply = self.post(&url, message)?;
        if is_initialize {
            self.capture_session(&reply);
            return Ok(reply);
        }
        if sent_session && session_refused(&reply) {
            if let Some(initialize) = self.initialize.clone() {
                // A tool call is never replayed: the refusal is the server's
                // word, not proof the call did not run. The next request
                // uses the fresh session.
                let replay = message.get("method").and_then(Value::as_str) != Some("tools/call");
                if self.reinitialize(&url, &initialize)? && replay {
                    return self.post(&url, message);
                }
            }
        }
        Ok(reply)
    }

    fn capture_session(&mut self, reply: &Reply) -> bool {
        if reply.status != 200 {
            return false;
        }
        let session = reply
            .mcp_session
            .as_deref()
            .map(str::trim)
            .filter(|session| !session.is_empty());
        self.mcp_session = session.map(str::to_owned);
        self.mcp_session.is_some()
    }

    /// Open a fresh server session for a client that already has one.
    fn reinitialize(&mut self, url: &str, initialize: &Value) -> Result<bool, String> {
        self.mcp_session = None;
        let reply = self.post(url, initialize)?;
        if !self.capture_session(&reply) {
            return Ok(false);
        }
        self.warn("the server forgot this MCP session; started a new one");
        self.post(
            url,
            &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        )?;
        Ok(true)
    }

    fn forward(&mut self, message: &Value) -> Option<Value> {
        let (status, body) = match self.exchange(message) {
            Ok(reply) => (reply.status, reply.body),
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
        // Python's `json.dumps` defaults: spaced separators and escaped
        // non-ASCII, so a raw U+2028 cannot split a line the client reads.
        output.push_str(&crate::decisions::python_json_dumps(message, false));
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
        if let Some(session) = &self.mcp_session {
            headers.push((MCP_SESSION_HEADER, session.clone()));
        }
        headers
    }

    fn get(&self, url: &str) -> Result<Reply, String> {
        self.transport.exchange(url, &self.headers(), b"")
    }

    fn post(&self, url: &str, message: &Value) -> Result<Reply, String> {
        self.transport.exchange(
            url,
            &self.headers(),
            crate::decisions::python_json_dumps(message, false).as_bytes(),
        )
    }
}

/// The server clearly says it no longer knows the session this request
/// presented. The spec answers 404 for an expired session; Cadastre answers
/// 400 with the JSON-RPC error -32602 "MCP request needs a valid
/// Mcp-Session-Id". A 400 whose body only mentions the header in free text is
/// not that signal.
fn session_refused(reply: &Reply) -> bool {
    if reply.status == 404 {
        return true;
    }
    if reply.status != 400 {
        return false;
    }
    let Ok(body) = serde_json::from_slice::<Value>(&reply.body) else {
        return false;
    };
    let error = &body["error"];
    error["code"] == -32602
        && error["message"].as_str().is_some_and(|message| {
            message
                .to_ascii_lowercase()
                .starts_with("mcp request needs a valid mcp-session-id")
        })
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
    let text = super::super::text::python_strip(&text).to_owned();
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
    // The session branch is taken only when the session actually supplied a
    // token. An absent or empty one falls through to the token file: the
    // session has not overridden anything, so the shared credential stands.
    // A whitespace-only token is an override that strips to nothing, and that
    // resolves to None rather than to the file.
    if env.get("VOGT_SESSION_ID").is_some_and(|id| !id.is_empty()) {
        if let Some(token) = env.get(HTTP_TOKEN_ENV).filter(|token| !token.is_empty()) {
            let token = super::super::text::python_strip(token);
            return (!token.is_empty()).then(|| token.to_owned());
        }
    }
    if let Some(token) = read_token(env.get(TOKEN_FILE_ENV).map(String::as_str)) {
        return Some(token);
    }
    env.get(HTTP_TOKEN_ENV)
        .map(|token| super::super::text::python_strip(token))
        .filter(|token| !token.is_empty())
        .map(str::to_owned)
}

/// What `main` needs and cannot have: the URL. `None` is the exit-2 case.
pub fn configured_url(env: &HashMap<String, String>) -> Option<String> {
    env.get(URL_ENV).filter(|url| !url.is_empty()).cloned()
}

/// Python's `str()` for the JSON values a server might put in `version`.
fn python_str(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Number(number) => number.to_string(),
        Value::Bool(true) => "True".to_owned(),
        Value::Bool(false) => "False".to_owned(),
        Value::Null => "None".to_owned(),
        other => other.to_string(),
    }
}

/// The live transport. An empty body is a GET, because discovery sends one and
/// a call never does; everything else is a POST. Discovery gets the short
/// budget, a call the long one.
pub struct UreqTransport;

impl BridgeTransport for UreqTransport {
    fn exchange(
        &self,
        url: &str,
        headers: &[(&str, String)],
        body: &[u8],
    ) -> Result<Reply, String> {
        let timeout = if body.is_empty() {
            DISCOVERY_TIMEOUT
        } else {
            call_timeout(body)
        };
        let agent: ureq::Agent = http_client::builder()
            .timeout_global(Some(timeout))
            .http_status_as_error(false)
            .build()
            .into();
        let mut request = ureq::http::Request::builder()
            .method(if body.is_empty() { "GET" } else { "POST" })
            .uri(url);
        for (name, value) in headers {
            request = request.header(*name, value);
        }
        let request = request.body(body).map_err(|error| error.to_string())?;
        agent
            .run(request)
            .map_err(|error| error.to_string())
            .and_then(read_response)
    }
}

fn read_response(response: ureq::http::Response<ureq::Body>) -> Result<Reply, String> {
    let status = response.status().as_u16();
    let mcp_session = response
        .headers()
        .get(MCP_SESSION_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let mut bytes = Vec::new();
    use std::io::Read;
    response
        .into_body()
        .as_reader()
        .take(8 * 1024 * 1024)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    Ok(Reply {
        status,
        body: bytes,
        mcp_session,
    })
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
        ) -> Result<Reply, String> {
            self.seen.borrow_mut().push(format!(
                "{url} auth={}",
                headers.iter().any(|(name, _)| *name == "Authorization")
            ));
            if self.fail {
                return Err("connection refused".to_owned());
            }
            Ok(self
                .answers
                .borrow_mut()
                .pop()
                .unwrap_or((200, Vec::new()))
                .into())
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
    fn an_empty_session_token_falls_through_to_the_file() {
        let dir = std::env::temp_dir().join(format!("vogt-token-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("token");
        std::fs::write(&file, "from-file\n").unwrap();
        let path = file.display().to_string();
        let mut env = HashMap::new();
        env.insert("VOGT_SESSION_ID".to_owned(), "s".to_owned());
        env.insert(TOKEN_FILE_ENV.to_owned(), path.clone());
        assert_eq!(resolve_token(&env).as_deref(), Some("from-file"));
        env.insert(HTTP_TOKEN_ENV.to_owned(), String::new());
        assert_eq!(resolve_token(&env).as_deref(), Some("from-file"));
        env.insert(HTTP_TOKEN_ENV.to_owned(), "   ".to_owned());
        assert_eq!(resolve_token(&env), None);
        std::fs::remove_dir_all(&dir).ok();
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

    /// A streamable-HTTP server on loopback that issues an `Mcp-Session-Id`
    /// on `initialize` and answers every later request without a live one
    /// with Cadastre's 400 (WI-4288). Driven through the live
    /// [`UreqTransport`], so the header really crosses the wire.
    /// (JSON-RPC method, `Mcp-Session-Id` it carried), in arrival order.
    type Seen = std::sync::Arc<std::sync::Mutex<Vec<(String, Option<String>)>>>;

    struct SessionServer {
        url: String,
        seen: Seen,
        live: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
        /// Answer every `tools/call` with a 400 that merely mentions the header.
        free_text: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }

    impl SessionServer {
        fn start() -> Self {
            use std::io::{BufRead, BufReader, Read, Write};
            use std::sync::{Arc, Mutex};
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let seen = Arc::new(Mutex::new(Vec::new()));
            let live = Arc::new(Mutex::new(Vec::<String>::new()));
            let (seen_in, live_in) = (Arc::clone(&seen), Arc::clone(&live));
            let free_text = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let free_in = Arc::clone(&free_text);
            std::thread::spawn(move || {
                let mut issued = 0;
                for stream in listener.incoming() {
                    let Ok(mut stream) = stream else { return };
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut request_line = String::new();
                    reader.read_line(&mut request_line).unwrap();
                    let (mut length, mut session) = (0, None);
                    loop {
                        let mut line = String::new();
                        reader.read_line(&mut line).unwrap();
                        let line = line.trim_end();
                        if line.is_empty() {
                            break;
                        }
                        let (name, value) = line.split_once(':').unwrap();
                        match name.to_ascii_lowercase().as_str() {
                            "content-length" => length = value.trim().parse().unwrap(),
                            "mcp-session-id" => session = Some(value.trim().to_owned()),
                            _ => {}
                        }
                    }
                    let mut body = vec![0; length];
                    reader.read_exact(&mut body).unwrap();
                    let reply = |status: &str, extra: &str, body: &str| {
                        format!(
                            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n\
                             Content-Length: {}\r\nConnection: close\r\n{extra}\r\n{body}",
                            body.len()
                        )
                    };
                    let answer = if request_line.starts_with("GET") {
                        reply("404 Not Found", "", "")
                    } else {
                        let message: Value = serde_json::from_slice(&body).unwrap();
                        let method = message["method"].as_str().unwrap_or("").to_owned();
                        seen_in
                            .lock()
                            .unwrap()
                            .push((method.clone(), session.clone()));
                        let id = message.get("id").cloned().unwrap_or(Value::Null);
                        if method == "initialize" {
                            issued += 1;
                            let new = format!("sess-{issued}");
                            live_in.lock().unwrap().push(new.clone());
                            reply(
                                "200 OK",
                                &format!("Mcp-Session-Id: {new}\r\n"),
                                &json!({"jsonrpc": "2.0", "id": id,
                                    "result": {"protocolVersion": "2025-06-18"}})
                                .to_string(),
                            )
                        } else if !session.is_some_and(|s| live_in.lock().unwrap().contains(&s)) {
                            reply(
                                "400 Bad Request",
                                "",
                                &json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32602,
                                    "message": "MCP request needs a valid Mcp-Session-Id"}})
                                .to_string(),
                            )
                        } else if method == "tools/call"
                            && free_in.load(std::sync::atomic::Ordering::SeqCst)
                        {
                            reply(
                                "400 Bad Request",
                                "",
                                &json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32602,
                                    "message": "tool ran, then rejected its Mcp-Session-Id argument"}})
                                .to_string(),
                            )
                        } else if id.is_null() {
                            reply("202 Accepted", "", "")
                        } else {
                            reply(
                                "200 OK",
                                "",
                                &json!({"jsonrpc": "2.0", "id": id,
                                    "result": {"tools": [{"name": "lookup"}]}})
                                .to_string(),
                            )
                        }
                    };
                    stream.write_all(answer.as_bytes()).unwrap();
                }
            });
            Self {
                url,
                seen,
                live,
                free_text,
            }
        }

        fn seen(&self) -> Vec<(String, Option<String>)> {
            self.seen.lock().unwrap().clone()
        }
    }

    const INITIALIZE: &str =
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}\n";
    const INITIALIZED: &str = "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n";

    fn tools(id: u32) -> String {
        format!("{{\"jsonrpc\":\"2.0\",\"id\":{id},\"method\":\"tools/list\"}}\n")
    }

    fn pair(method: &str, session: Option<&str>) -> (String, Option<String>) {
        (method.to_owned(), session.map(str::to_owned))
    }

    #[test]
    fn the_session_id_from_initialize_is_sent_on_every_later_request() {
        let server = SessionServer::start();
        let transport = UreqTransport;
        let mut bridge = Bridge::new(&server.url, Some("t".into()), &transport, "0.1.0");
        let mut output = String::new();
        bridge.serve(
            &format!("{INITIALIZE}{INITIALIZED}{}", tools(2)),
            &mut output,
        );
        let last = output.lines().last().unwrap();
        assert!(last.contains("lookup"), "{output}");
        assert_eq!(
            server.seen(),
            vec![
                pair("initialize", None),
                pair("notifications/initialized", Some("sess-1")),
                pair("tools/list", Some("sess-1")),
            ]
        );
    }

    #[test]
    fn a_client_reinitialize_starts_a_fresh_session() {
        let server = SessionServer::start();
        let transport = UreqTransport;
        let mut bridge = Bridge::new(&server.url, Some("t".into()), &transport, "0.1.0");
        let mut output = String::new();
        bridge.serve(
            &format!("{INITIALIZE}{INITIALIZED}{INITIALIZE}{}", tools(3)),
            &mut output,
        );
        let seen = server.seen();
        assert_eq!(seen[2], pair("initialize", None), "{seen:?}");
        assert_eq!(seen[3], pair("tools/list", Some("sess-2")), "{seen:?}");
    }

    #[test]
    fn a_session_the_server_forgot_is_reestablished_once() {
        let server = SessionServer::start();
        let transport = UreqTransport;
        let mut bridge = Bridge::new(&server.url, Some("t".into()), &transport, "0.1.0");
        let mut output = String::new();
        bridge.serve(
            &format!("{INITIALIZE}{INITIALIZED}{}", tools(2)),
            &mut output,
        );
        server.live.lock().unwrap().clear(); // a server restart
        let mut after = String::new();
        bridge.serve(&tools(3), &mut after);
        assert_eq!(
            after.lines().count(),
            1,
            "only the client's answer: {after}"
        );
        assert!(after.contains("lookup"), "{after}");
        assert_eq!(
            server.seen()[3..],
            [
                pair("tools/list", Some("sess-1")),
                pair("initialize", None),
                pair("notifications/initialized", Some("sess-2")),
                pair("tools/list", Some("sess-2")),
            ]
        );
    }

    #[test]
    fn a_400_that_only_mentions_the_header_is_not_resent() {
        let server = SessionServer::start();
        server
            .free_text
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let transport = UreqTransport;
        let mut bridge = Bridge::new(&server.url, Some("t".into()), &transport, "0.1.0");
        let mut output = String::new();
        let call = "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"x\"}}\n";
        bridge.serve(&format!("{INITIALIZE}{INITIALIZED}{call}"), &mut output);
        let calls = server
            .seen()
            .iter()
            .filter(|(m, _)| m == "tools/call")
            .count();
        assert_eq!(calls, 1, "{:?}", server.seen());
        assert_eq!(server.seen().len(), 3, "no re-initialize either");
    }

    #[test]
    fn a_tool_call_is_never_replayed_after_the_session_is_refused() {
        let server = SessionServer::start();
        let transport = UreqTransport;
        let mut bridge = Bridge::new(&server.url, Some("t".into()), &transport, "0.1.0");
        let mut output = String::new();
        bridge.serve(&format!("{INITIALIZE}{INITIALIZED}"), &mut output);
        server.live.lock().unwrap().clear();
        let call = "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"x\"}}\n";
        let mut after = String::new();
        bridge.serve(call, &mut after);
        let calls = server
            .seen()
            .iter()
            .filter(|(m, _)| m == "tools/call")
            .count();
        assert_eq!(calls, 1, "{:?}", server.seen());
        let mut next = String::new();
        bridge.serve(&tools(3), &mut next);
        assert!(next.contains("lookup"), "{next}");
    }

    /// A `tools/call` body with the given arguments, as a client sends it.
    fn call_body(name: &str, arguments: &str) -> String {
        format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":4,\"method\":\"tools/call\",\"params\":{{\"name\":\"{name}\",\"arguments\":{arguments}}}}}"
        )
    }

    #[test]
    fn a_session_wait_longer_than_thirty_seconds_is_not_cut_off_at_thirty() {
        let body = call_body("session_wait", r#"{"id":"s","timeout_s":90}"#);
        assert_eq!(
            call_timeout(body.as_bytes()),
            std::time::Duration::from_secs(90) + CALL_MARGIN,
        );
        let max = call_body("session_wait", r#"{"id":"s","timeout_s":600}"#);
        assert_eq!(
            call_timeout(max.as_bytes()),
            std::time::Duration::from_secs(600) + CALL_MARGIN,
        );
    }

    #[test]
    fn the_longest_wait_argument_sets_the_budget() {
        let body = call_body("chat_send", r#"{"wait_s":300,"wake_timeout_s":120}"#);
        assert_eq!(
            call_timeout(body.as_bytes()),
            std::time::Duration::from_secs(300) + CALL_MARGIN,
        );
        let input = call_body("session_input", r#"{"wake_timeout_s":120}"#);
        assert_eq!(
            call_timeout(input.as_bytes()),
            std::time::Duration::from_secs(120) + CALL_MARGIN,
        );
    }

    #[test]
    fn a_short_wait_gets_its_own_margin_on_top() {
        let body = call_body("session_wait", r#"{"id":"s","timeout_s":5}"#);
        assert_eq!(
            call_timeout(body.as_bytes()),
            std::time::Duration::from_secs(5) + CALL_MARGIN,
        );
        let zero = call_body("session_wait", r#"{"id":"s","timeout_s":0}"#);
        assert_eq!(call_timeout(zero.as_bytes()), DEFAULT_TIMEOUT);
    }

    #[test]
    fn a_call_without_a_wait_keeps_the_default() {
        for body in [
            call_body("session_list", "{}"),
            call_body("session_wait", r#"{"id":"s","timeout_s":"soon"}"#),
            "{\"jsonrpc\":\"2.0\",\"id\":5,\"method\":\"tools/list\"}".to_owned(),
            "not json at all".to_owned(),
        ] {
            assert_eq!(call_timeout(body.as_bytes()), DEFAULT_TIMEOUT, "{body}");
        }
    }
}
