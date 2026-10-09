//! The HTTP boundary a forge provider reads and writes through.
//!
//! Ports the slice of `adapters/github/client.py` the providers actually use.
//! WI-1064 owns the real `GitHubClient`; until it lands, a provider takes this
//! trait and tests supply a fixture map. Nothing here opens a socket.

use crate::errors::VogtError;

/// One answer from the forge. `None` is a 404, [`Empty`](ForgeResponse::Empty)
/// is a 204, and both mean "nothing to read" rather than a failure.
#[derive(Debug, Clone, PartialEq)]
pub enum ForgeResponse {
    Json(serde_json::Value),
    Empty,
    Missing,
}

/// The transport. A provider builds paths and reads shapes; this sends them.
///
/// `get` returns the three read outcomes. `send` is a write and reports a
/// refusal as [`VogtError`], with the HTTP status in the message — that status
/// text is what `create_repo` reads to tell a 422 from any other failure.
pub trait ForgeTransport {
    fn api_root(&self) -> &str;

    fn token(&self) -> Option<&str>;

    fn get(&self, path: &str, query: &[(&str, String)]) -> Result<ForgeResponse, VogtError>;

    /// `GET /user` plus the `X-OAuth-Scopes` header, which is how a token is
    /// *validated*: `(login, scopes)`, or `None` for a 401/403/404. Scopes are
    /// empty when the transport cannot see headers.
    fn identity(&self) -> Result<Option<(String, String)>, VogtError>;

    fn send(
        &self,
        method: &str,
        path: &str,
        body: Option<&serde_json::Value>,
    ) -> Result<serde_json::Value, VogtError>;
}

/// `GET` result narrowed to the list of objects a reader iterates. `Missing`
/// and `Empty` are both "nothing here".
pub fn as_list(response: &ForgeResponse) -> Vec<&serde_json::Map<String, serde_json::Value>> {
    let ForgeResponse::Json(serde_json::Value::Array(items)) = response else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(serde_json::Value::as_object)
        .collect()
}

/// `{api_root}/repos/...` to `/repos/...`, or `None` for anything outside this
/// API's repository tree. The URL comes from a forge payload, so the token
/// must not follow one the payload chose.
pub fn api_path(api_url: &str, api_root: &str) -> Option<String> {
    let root = api_root.trim_end_matches('/');
    let rest = api_url.strip_prefix(&format!("{root}/repos/"))?;
    let path = format!("/repos/{}", rest.split(['?', '#']).next().unwrap_or(""));
    if path.split('/').any(|part| part == "..") {
        return None;
    }
    Some(path)
}
