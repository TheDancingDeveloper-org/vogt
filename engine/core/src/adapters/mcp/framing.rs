//! MCP over JSON-RPC 2.0. Ports `adapters/mcp/stdio.py` and the dispatch half
//! of `adapters/mcp/http.py`.
//!
//! One dispatcher, two transports. The stdio server and the streamable-HTTP
//! route both hand a parsed message here and write back whatever comes out, so
//! the protocol lives in exactly one place. What differs between them — where
//! the bytes come from, and that HTTP filters the tool list by the caller's
//! grant — stays in the transport.
//!
//! Three rules, kept from the Python:
//!
//! - **A notification gets no response.** A message with no id is acknowledged
//!   by silence; over HTTP that silence is a 202.
//! - **A failed tool call is a result, not a protocol error.** The model is
//!   meant to see it and try something else, so it comes back with `isError`.
//! - **Identity is never a message field.** Nothing in `initialize` or a tool
//!   argument can name who is calling; the principal arrives from the
//!   transport's own authentication.

use serde_json::{json, Map, Value};

use crate::registry::{Operation, OperationRegistry, Transport};

/// Newest first. The head is what the server offers when it gets to choose.
pub const SUPPORTED_PROTOCOL_VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

const SERVER_NAME: &str = "vogt";

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

/// Answer with a version both sides can speak.
///
/// The handshake is a negotiation, not a gate: a server that does not recognise
/// the requested version answers with one it does support and lets the client
/// decide whether to continue. Refusing instead makes the server unusable by
/// every client newer than itself, which is the normal direction of drift.
pub fn negotiate_protocol_version(requested: Option<&str>) -> &'static str {
    match requested {
        Some(version) if SUPPORTED_PROTOCOL_VERSIONS.contains(&version) => {
            SUPPORTED_PROTOCOL_VERSIONS[SUPPORTED_PROTOCOL_VERSIONS
                .iter()
                .position(|supported| *supported == version)
                .expect("the contains check just matched")]
        }
        _ => SUPPORTED_PROTOCOL_VERSIONS[0],
    }
}

/// What one session did.
#[derive(Debug, Default)]
pub struct ServeReport {
    pub messages_handled: u64,
    pub protocol_version: Option<String>,
}

/// The operations a caller may see and run.
///
/// The stdio transport grants everything the registry exposes over MCP. The
/// HTTP transport narrows it to the caller's grant, so an ungranted tool is
/// absent rather than present and refusing.
pub trait ToolGrant {
    fn allows(&self, registry: &OperationRegistry, operation: &Operation) -> bool;

    /// Why a call was refused, in the words the caller sees.
    ///
    /// The default is the stdio path, which never reaches this: it grants
    /// everything. The HTTP grant names the cause, because "forbidden" tells a
    /// model nothing it can act on.
    fn denial(&self, operation: &Operation) -> String {
        let _ = operation;
        "forbidden".to_owned()
    }
}

/// Every MCP-exposed operation, which is what a local stdio session may do.
pub struct FullGrant;

impl ToolGrant for FullGrant {
    fn allows(&self, _registry: &OperationRegistry, _operation: &Operation) -> bool {
        true
    }
}

/// Whether the MCP transport carries this operation at all.
///
/// `LOCAL_ONLY` and `HTTP_ONLY` operations are absent from the tool list and
/// undispatchable through it — the same invisible-tool rule on both paths.
/// The registry decides which those are, so this asks it rather than repeating
/// one of the two lists.
pub(super) fn exposed_over_mcp(registry: &OperationRegistry, operation: &Operation) -> bool {
    registry
        .transports_for(operation.name)
        .contains(&Transport::Mcp)
}

/// Which transport is asking. The two disagree on a handful of error shapes,
/// and the disagreement is what the differential test pins down, so it is
/// named here rather than papered over with one behaviour.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum McpTransport {
    /// `stdio.py`: a missing method is `-32600`, a notification is any id that
    /// is not a string or an integer, and an unknown tool falls into the
    /// internal-error result because the surface raises before it can say
    /// "unknown".
    Stdio,
    /// `http.py`: a missing method and a non-object body are `-32602`, only an
    /// absent id is a notification, and an unknown tool is `-32601`.
    Http,
}
pub struct Dispatcher<'a, G: ToolGrant> {
    registry: &'a OperationRegistry,
    grant: &'a G,
    transport: McpTransport,
    report: ServeReport,
}

impl<'a, G: ToolGrant> Dispatcher<'a, G> {
    pub fn new(registry: &'a OperationRegistry, grant: &'a G, transport: McpTransport) -> Self {
        Self {
            registry,
            grant,
            transport,
            report: ServeReport::default(),
        }
    }

    pub fn report(&self) -> &ServeReport {
        &self.report
    }

    /// Handle one line of stdio framing. Blank lines are skipped, and a line
    /// that is not JSON is a parse error rather than a dropped message.
    pub fn handle_line(&mut self, line: &str) -> Option<Value> {
        let line = line.trim();
        if line.is_empty() {
            return None;
        }
        self.report.messages_handled += 1;
        match serde_json::from_str::<Value>(line) {
            Ok(Value::Object(message)) => self.handle(&message),
            Ok(_) => Some(error(
                None,
                INVALID_REQUEST,
                "message must be a JSON object",
            )),
            Err(failure) => Some(error(None, PARSE_ERROR, format!("invalid JSON: {failure}"))),
        }
    }

    /// Handle one parsed message.
    ///
    /// Validation runs before the id is read, because a message that is not a
    /// request still has to be answered: `{"params": []}` is an invalid
    /// request with a null id, not silence. The id itself is a string or an
    /// integer. A boolean counts — Python's `bool` is an `int` — and a float
    /// does not, so `1.5` and `1.0` are notifications over stdio.
    pub fn handle(&mut self, message: &Map<String, Value>) -> Option<Value> {
        let method = message.get("method").and_then(Value::as_str);
        let params = message.get("params").filter(|params| !json_falsy(params));
        if method.is_none() {
            let code = match self.transport {
                McpTransport::Stdio => INVALID_REQUEST,
                McpTransport::Http => INVALID_PARAMS,
            };
            return Some(error(self.message_id(message), code, "missing method"));
        }
        if let Some(params) = params {
            if !params.is_object() {
                return Some(error(
                    self.message_id(message),
                    INVALID_PARAMS,
                    "params must be an object",
                ));
            }
        }
        let params = params
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let message_id = self.message_id(message)?;

        let response = match method.expect("checked above") {
            "initialize" => self.initialize(&params),
            "ping" => json!({}),
            "tools/list" => json!({"tools": self.tools()}),
            "tools/call" => return Some(self.call(message_id, &params)),
            other => {
                return Some(error(
                    Some(message_id),
                    METHOD_NOT_FOUND,
                    format!("unknown method {}", python_repr(other)),
                ));
            }
        };
        Some(result(Some(message_id), response))
    }

    /// The id a response should echo, or `None` when the message is a
    /// notification.
    ///
    /// Over stdio anything that is not a string or an integer — a float, a
    /// boolean-shaped number aside, an object — is a notification. Over HTTP
    /// only an absent id is: `http.py` returns the raw id whenever the key is
    /// present, and answers it.
    fn message_id(&self, message: &Map<String, Value>) -> Option<Value> {
        let raw = message.get("id")?;
        let acceptable = match raw {
            Value::String(_) => true,
            Value::Number(number) => number.as_i64().is_some() || number.as_u64().is_some(),
            // Python's `bool` is a subclass of `int`, so `id: true` is a
            // request there. JSON has no integer/boolean overlap, so this is
            // the one place the two languages read the same byte differently.
            Value::Bool(_) => true,
            _ => false,
        };
        if acceptable || self.transport == McpTransport::Http {
            Some(raw.clone())
        } else {
            None
        }
    }

    fn initialize(&mut self, params: &Map<String, Value>) -> Value {
        let version =
            negotiate_protocol_version(params.get("protocolVersion").and_then(Value::as_str))
                .to_owned();
        self.report.protocol_version = Some(version.clone());
        json!({
            "protocolVersion": version,
            "capabilities": {"tools": {"listChanged": false}},
            "serverInfo": {"name": SERVER_NAME, "version": crate::VERSION},
        })
    }

    /// The tools this grant allows, in registry order, and only the ones the
    /// MCP transport carries. `session.token` is HTTP-only and must not appear.
    fn tools(&self) -> Vec<Value> {
        self.registry
            .iter()
            .filter(|operation| exposed_over_mcp(self.registry, operation))
            .filter(|operation| self.grant.allows(self.registry, operation))
            .map(tool_wire)
            .collect()
    }

    /// A tool call. A missing name or a non-object `arguments` is a protocol
    /// error (`-32602`), not a failed tool result: the call never reached a
    /// tool. An unknown tool differs by transport — HTTP names it with
    /// `-32601`, stdio's surface raises and the catch-all reports an internal
    /// error.
    fn call(&self, message_id: Value, params: &Map<String, Value>) -> Value {
        let Some(name) = params.get("name").and_then(Value::as_str) else {
            return error(
                Some(message_id),
                INVALID_PARAMS,
                "tools/call needs a tool name",
            );
        };
        if let Some(arguments) = params.get("arguments").filter(|value| !json_falsy(value)) {
            if !arguments.is_object() {
                return error(
                    Some(message_id),
                    INVALID_PARAMS,
                    "arguments must be an object",
                );
            }
        }
        let operation = match self.registry.by_mcp_tool(name) {
            Ok(operation) if exposed_over_mcp(self.registry, operation) => operation,
            Ok(_) | Err(_) => {
                return match self.transport {
                    McpTransport::Http => error(
                        Some(message_id),
                        METHOD_NOT_FOUND,
                        format!("unknown tool {}", python_repr(name)),
                    ),
                    McpTransport::Stdio => {
                        result(Some(message_id), tool_error("error", "internal error"))
                    }
                };
            }
        };
        if !self.grant.allows(self.registry, operation) {
            return result(
                Some(message_id),
                tool_error("forbidden", &self.grant.denial(operation)),
            );
        }
        // The service behind nearly every operation is not ported yet, and
        // saying so is a failed tool result the model can read — never a
        // protocol error, and never an empty success.
        let body = match operation.run() {
            Ok(()) => json!({
                "content": [{"type": "text", "text": "{}"}],
                "structuredContent": {},
                "isError": false,
            }),
            Err(error) => tool_error(error.code(), error.message()),
        };
        result(Some(message_id), body)
    }
}

/// The MCP `Tool` shape.
fn tool_wire(operation: &Operation) -> Value {
    json!({
        "name": operation.mcp_tool_name(),
        "description": operation.summary,
        "inputSchema": {"type": "object", "properties": {}},
    })
}

fn result(id: Option<Value>, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn error(id: Option<Value>, code: i64, message: impl Into<String>) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message.into()}})
}

/// A failed tool call: a result the model is meant to read and recover from.
fn tool_error(code: &str, message: &str) -> Value {
    json!({
        "content": [{"type": "text", "text": format!("{code}: {message}")}],
        "isError": true,
    })
}

/// Python's truthiness for a JSON value: `None`, `[]`, `0`, `""` and `false`
/// are all falsy, and both handlers write `params or {}`.
fn json_falsy(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Bool(value) => !value,
        Value::Number(number) => number.as_i64() == Some(0) || number.as_f64() == Some(0.0),
        Value::String(text) => text.is_empty(),
        Value::Array(items) => items.is_empty(),
        Value::Object(_) => false,
    }
}

/// Python's `repr` for a string: single quotes, with the usual escapes.
pub(super) fn python_repr(text: &str) -> String {
    let mut out = String::from("'");
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '\n' => out.push_str("\\n"),
            other => out.push(other),
        }
    }
    out.push('\'');
    out
}

/// Read newline-delimited JSON-RPC from `input`, writing each response to
/// `output`. Diagnostics would go to stderr; this layer has none of its own.
///
/// Returns the session report. A caller that wants the bytes — a test, the
/// HTTP transport — passes its own buffers.
pub fn serve_stdio<G: ToolGrant>(
    input: &str,
    output: &mut String,
    registry: &OperationRegistry,
    grant: &G,
) -> ServeReport {
    let mut dispatcher = Dispatcher::new(registry, grant, McpTransport::Stdio);
    for line in input.lines() {
        if let Some(response) = dispatcher.handle_line(line) {
            output.push_str(&response.to_string());
            output.push('\n');
        }
    }
    ServeReport {
        messages_handled: dispatcher.report.messages_handled,
        protocol_version: dispatcher.report.protocol_version.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> OperationRegistry {
        OperationRegistry::new(crate::registry::operations::build_operations()).unwrap()
    }

    fn line(message: Value) -> String {
        format!("{message}\n")
    }

    fn responses(input: &str) -> Vec<Value> {
        let mut output = String::new();
        serve_stdio(input, &mut output, &registry(), &FullGrant);
        output
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[test]
    fn an_unknown_version_negotiates_down_instead_of_refusing() {
        assert_eq!(negotiate_protocol_version(Some("2025-03-26")), "2025-03-26");
        assert_eq!(negotiate_protocol_version(Some("2099-01-01")), "2025-06-18");
        assert_eq!(negotiate_protocol_version(None), "2025-06-18");
    }

    #[test]
    fn initialize_answers_with_the_negotiated_version() {
        let response = &responses(&line(json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"protocolVersion": "2024-11-05"}
        })))[0];
        assert_eq!(response["result"]["protocolVersion"], "2024-11-05");
        assert_eq!(response["result"]["serverInfo"]["name"], "vogt");
        assert_eq!(
            response["result"]["capabilities"]["tools"]["listChanged"],
            false
        );
    }

    #[test]
    fn a_notification_gets_silence_and_a_blank_line_is_skipped() {
        let mut output = String::new();
        let report = serve_stdio(
            "\n\n{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n",
            &mut output,
            &registry(),
            &FullGrant,
        );
        assert!(output.is_empty(), "{output}");
        assert_eq!(report.messages_handled, 1);
    }

    #[test]
    fn tools_list_comes_from_the_registry_and_hides_local_only() {
        let response = &responses(&line(
            json!({"jsonrpc": "2.0", "id": "a", "method": "tools/list"}),
        ))[0];
        let tools = response["result"]["tools"].as_array().unwrap();
        let names: Vec<&str> = tools
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"work_list"), "{names:?}");
        assert!(
            !names.contains(&"mcp_stdio"),
            "a local-only operation is absent, not present and refusing"
        );
        assert!(
            !names.contains(&"session_token"),
            "an http-only operation is absent too"
        );
        assert!(tools
            .iter()
            .all(|tool| tool["inputSchema"]["type"] == "object"));
    }

    #[test]
    fn a_tool_call_against_an_unported_service_is_a_failed_result() {
        let response = &responses(&line(json!({
            "jsonrpc": "2.0", "id": 7, "method": "tools/call",
            "params": {"name": "work_list", "arguments": {}}
        })))[0];
        assert!(
            response.get("error").is_none(),
            "a tool failure is a result"
        );
        assert_eq!(response["result"]["isError"], true);
        let text = response["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("not available"), "{text}");
    }

    #[test]
    fn a_local_only_tool_cannot_be_called_either() {
        let response = &responses(&line(json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": "init"}
        })))[0];
        assert_eq!(response["result"]["isError"], true);
        assert!(response["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with("error: internal error"));
    }

    #[test]
    fn malformed_input_is_a_protocol_error_naming_the_cause() {
        let parsed = &responses("not json\n{\"id\": 1, \"params\": []}\n")[0];
        assert_eq!(parsed["error"]["code"], PARSE_ERROR);
        let missing = &responses("{\"id\": 1, \"params\": []}\n")[0];
        assert_eq!(missing["error"]["code"], INVALID_REQUEST);
        let params =
            &responses("{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\",\"params\":[1]}\n")[0];
        assert_eq!(params["error"]["code"], INVALID_PARAMS);
    }

    #[test]
    fn an_unknown_method_names_itself() {
        let response = &responses(&line(
            json!({"jsonrpc": "2.0", "id": 1, "method": "resources/list"}),
        ))[0];
        assert_eq!(response["error"]["code"], METHOD_NOT_FOUND);
        assert!(response["error"]["message"]
            .as_str()
            .unwrap()
            .contains("'resources/list'"));
    }
}
