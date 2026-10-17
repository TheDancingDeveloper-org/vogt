//! `connect`: how a client reaches this instance, and the configuration to
//! point it there. Ports `src/vogt/application/services/connect.py`.
//!
//! The address is configuration (`public_url`), an exposure value with no
//! default: when it is unset the answer says so rather than guessing, because
//! a URL the server invented would be wrong exactly where the field matters.
//! The configuration text is rendered here, in one place, whichever shape the
//! instance is deployed in.

use serde_json::{json, Value};

use super::{dispatch, Built};

use crate::adapters::mcp::framing::SUPPORTED_PROTOCOL_VERSIONS;
use crate::application::context::AppContext;
use crate::core::{Clock, IdFactory};
use crate::errors::VogtError;

/// What the instance says when no address is configured (Python `NOT_CONFIGURED`).
pub const NOT_CONFIGURED: &str = "no public_url is configured, so this instance cannot say where to reach it. That is not something a server can work out: it binds a container port and is published somewhere else. Set VOGT_PUBLIC_URL (or public_url) to the address clients actually use \u{2014} an exposure value, so it is never defaulted";

const API_PATH: &str = "/api";
const MCP_PATH: &str = "/mcp";
const UNSET_TARGET: &str = "<set public_url first>";

pub fn connect_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    let client = match params.get("client").and_then(Value::as_str) {
        None => "http".to_string(),
        Some(text) if text == "http" || text == "bridge" => text.to_string(),
        Some(_) => {
            return Err(VogtError::InvalidRequest(
                "client must be http or bridge".to_string(),
            ))
        }
    };
    let format = match params.get("format").and_then(Value::as_str) {
        None => "json".to_string(),
        Some(text) if text == "json" || text == "markdown" => text.to_string(),
        Some(_) => {
            return Err(VogtError::InvalidRequest(
                "format must be json or markdown".to_string(),
            ))
        }
    };
    dispatch!(built, connect, (&client, &format))
}

fn connect<C, I>(ctx: &AppContext<C, I>, args: (&str, &str)) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let (client, format) = args;
    let url = clean_url(ctx.public_identity.public_url.as_deref());
    let api_path = ctx
        .public_identity
        .api_path
        .clone()
        .unwrap_or_else(|| API_PATH.to_string());
    let mcp_path = ctx
        .public_identity
        .mcp_path
        .clone()
        .unwrap_or_else(|| MCP_PATH.to_string());
    let mcp_url = url.as_ref().map(|url| format!("{url}{mcp_path}"));
    let (configuration, requires_install) = if client == "bridge" {
        (bridge_config(url.as_deref(), format), true)
    } else {
        (http_config(mcp_url.as_deref(), format), false)
    };
    Ok(json!({
        "url": url,
        "api_path": api_path,
        "mcp_path": mcp_path,
        "mcp_url": mcp_url,
        "supported_mcp_protocol_versions": SUPPORTED_PROTOCOL_VERSIONS,
        "client": client,
        "requires_install": requires_install,
        "configuration": configuration,
        "detail": if url.is_some() { Value::Null } else { Value::String(NOT_CONFIGURED.to_string()) },
    }))
}

/// Python `_clean_url`: surrounding space and trailing slashes go; empty is unset.
fn clean_url(value: Option<&str>) -> Option<String> {
    let cleaned = value?.trim().trim_end_matches('/');
    (!cleaned.is_empty()).then(|| cleaned.to_string())
}

/// The recommended path: nothing installed, nothing to keep in step.
fn http_config(mcp_url: Option<&str>, format: &str) -> String {
    let target = mcp_url.unwrap_or(UNSET_TARGET);
    if format == "markdown" {
        let versions = SUPPORTED_PROTOCOL_VERSIONS.join(", ");
        return [
            "# Connecting to Vogt".to_string(),
            String::new(),
            format!("- MCP endpoint: `{target}`"),
            "- Transport: streamable HTTP".to_string(),
            format!("- Protocol versions: {versions}"),
            "- Authentication: `Authorization: Bearer <token>`".to_string(),
            String::new(),
            "Nothing needs installing. Issue a token with `vogt token issue`, and send it as a header \u{2014} never in the URL, which ends up in logs, proxies and history.".to_string(),
            String::new(),
            "```console".to_string(),
            format!("$ claude mcp add --transport http vogt \"{target}\" \\"),
            "    --header \"Authorization: Bearer $VOGT_TOKEN\"".to_string(),
            "```".to_string(),
        ]
        .join("\n");
    }
    format!(
        "{{\n  \"mcpServers\": {{\n    \"vogt\": {{\n      \"type\": \"http\",\n      \"url\": {},\n      \"headers\": {{\n        \"Authorization\": \"Bearer ${{VOGT_TOKEN}}\"\n      }}\n    }}\n  }}\n}}",
        json_string(target)
    )
}

/// For clients that can only spawn a local process: the bridge is Vogt's own
/// code on the client's machine, so it does need installing.
fn bridge_config(url: Option<&str>, format: &str) -> String {
    let target = url.unwrap_or(UNSET_TARGET);
    if format == "markdown" {
        return [
            "# Connecting to Vogt (stdio bridge)".to_string(),
            String::new(),
            "For clients that cannot speak streamable HTTP. Prefer the `http` client where you have the choice: it installs nothing and cannot skew against the server.".to_string(),
            String::new(),
            format!("- Remote: `{target}`"),
            "- Command: `vogt-mcp-remote`".to_string(),
            "- Token: `VOGT_TOKEN_FILE`, a file path \u{2014} never argv, never a URL.".to_string(),
        ]
        .join("\n");
    }
    format!(
        "{{\n  \"mcpServers\": {{\n    \"vogt\": {{\n      \"command\": \"vogt-mcp-remote\",\n      \"env\": {{\n        \"VOGT_URL\": {},\n        \"VOGT_TOKEN_FILE\": \"/path/to/token\"\n      }}\n    }}\n  }}\n}}",
        json_string(target)
    )
}

fn json_string(text: &str) -> String {
    serde_json::to_string(text).expect("a string serialises")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unset_or_blank_public_url_is_no_address() {
        assert_eq!(clean_url(None), None);
        assert_eq!(clean_url(Some("  /  ")), None);
        assert_eq!(
            clean_url(Some(" https://vogt.example.test/ ")).as_deref(),
            Some("https://vogt.example.test")
        );
    }

    #[test]
    fn the_http_configuration_names_the_placeholder_until_an_address_is_set() {
        let unset = http_config(None, "json");
        assert!(unset.contains("<set public_url first>"), "{unset}");
        let set = http_config(Some("https://vogt.example.test/mcp"), "json");
        assert!(
            set.contains("\"url\": \"https://vogt.example.test/mcp\""),
            "{set}"
        );
        assert!(set.contains("${VOGT_TOKEN}"), "{set}");
    }
}
