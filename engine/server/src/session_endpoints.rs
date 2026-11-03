//! Pure endpoint selection for local and pod-backed sessions.

use std::net::SocketAddr;

use serde_json::{json, Value};

use crate::sessions::engine_self_url;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionEndpoints {
    pub broker_url: Option<String>,
    pub mcp: Value,
}

/// Select the addresses a session may use without coupling this policy to a
/// backend launcher. WI-3182 supplies the selected result when it launches a
/// pod; local callers get the historical loopback output unchanged.
pub fn for_backend(
    backend: &str,
    bind: SocketAddr,
    public_url: Option<&str>,
    cadastre_url: Option<&str>,
) -> SessionEndpoints {
    let broker_url = if backend == "pod" {
        public_url.map(str::to_owned)
    } else {
        engine_self_url(bind)
    };
    let vogt_url = broker_url.as_deref().map(|url| format!("{url}/mcp"));
    let mut servers = serde_json::Map::new();
    if let Some(url) = vogt_url {
        servers.insert("vogt".into(), json!({"url": url}));
    }
    if let Some(url) = cadastre_url {
        servers.insert("cadastre".into(), json!({"url": format!("{url}/mcp")}));
    }
    SessionEndpoints {
        broker_url,
        mcp: Value::Object(servers),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_output_preserves_loopback_shape() {
        let actual = for_backend("local", "0.0.0.0:8910".parse().unwrap(), None, None);
        assert_eq!(actual.broker_url.as_deref(), Some("http://127.0.0.1:8910"));
        assert_eq!(
            actual.mcp,
            json!({
                "vogt": {"url": "http://127.0.0.1:8910/mcp"}
            })
        );
        assert_eq!(
            serde_json::to_vec(&actual.mcp).unwrap(),
            br#"{"vogt":{"url":"http://127.0.0.1:8910/mcp"}}"#,
            "the local MCP payload must stay byte-identical"
        );
    }

    #[test]
    fn pod_uses_public_url_and_omits_unconfigured_cadastre() {
        let actual = for_backend(
            "pod",
            "127.0.0.1:8910".parse().unwrap(),
            Some("https://192.168.1.75:18197"),
            None,
        );
        assert_eq!(
            actual.broker_url.as_deref(),
            Some("https://192.168.1.75:18197")
        );
        assert_eq!(
            actual.mcp,
            json!({
                "vogt": {"url": "https://192.168.1.75:18197/mcp"}
            })
        );
    }

    #[test]
    fn pod_includes_configured_cadastre() {
        let actual = for_backend(
            "pod",
            "127.0.0.1:8910".parse().unwrap(),
            Some("https://192.168.1.75:18197"),
            Some("https://cadastre.example.test"),
        );
        assert_eq!(
            actual.mcp["cadastre"]["url"],
            "https://cadastre.example.test/mcp"
        );
    }
}
