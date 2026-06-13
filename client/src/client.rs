//! Async HTTP/SSE client for the MyDevEnv2 server REST API.
//!
//! Every call here runs on the background tokio runtime (see `crate::bridge`)
//! and reports results back to the GPUI thread over channels. The bearer token
//! is attached to every request; `wss`/`https` is derived from the base URL.

use anyhow::{anyhow, Context, Result};
use futures_util::{Stream, StreamExt};
use serde_json::json;
use uuid::Uuid;

use crate::protocol::{
    BranchInfo, DiffResp, FileEntry, FileRead, GitStatus, LogEntry, SearchHit, ServerEvent,
    SessionSpec, SessionSummary, TreeNode, WriteReq,
};

/// A cheap-to-clone handle to the server API.
#[derive(Clone)]
pub struct ApiClient {
    http: reqwest::Client,
    base: String,
    token: String,
}

/// Result of attaching's REST precursor: the session plus its decoded scrollback.
pub struct SessionWithScrollback {
    pub summary: SessionSummary,
    pub scrollback_pos: u64,
    pub scrollback: Vec<u8>,
}

impl ApiClient {
    pub fn new(base: impl Into<String>, token: impl Into<String>) -> Self {
        let http = reqwest::Client::builder()
            .user_agent(concat!("mydevenv2-client/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("reqwest client builds");
        Self {
            http,
            base: base.into().trim_end_matches('/').to_string(),
            token: token.into(),
        }
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base, path)
    }

    /// `ws(s)://…/api/sessions/{id}/attach` derived from the HTTP base.
    pub fn attach_ws_url(&self, id: Uuid) -> String {
        let ws_base = if let Some(rest) = self.base.strip_prefix("https://") {
            format!("wss://{rest}")
        } else if let Some(rest) = self.base.strip_prefix("http://") {
            format!("ws://{rest}")
        } else {
            self.base.clone()
        };
        format!("{ws_base}/api/sessions/{id}/attach")
    }

    fn auth(&self, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        rb.bearer_auth(&self.token)
    }

    async fn get_json<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        let resp = self
            .auth(self.http.get(self.url(path)))
            .send()
            .await
            .with_context(|| format!("GET {path}"))?;
        Self::json_or_status(resp, path).await
    }

    async fn json_or_status<T: serde::de::DeserializeOwned>(
        resp: reqwest::Response,
        path: &str,
    ) -> Result<T> {
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("{path}: HTTP {status}: {}", body.trim()));
        }
        resp.json::<T>()
            .await
            .with_context(|| format!("decode {path}"))
    }

    // ── Health ───────────────────────────────────────────────────────────────

    /// Cheap unauthenticated reachability probe against `/healthz`.
    pub async fn healthz(&self) -> Result<()> {
        let resp = self
            .http
            .get(self.url("/healthz"))
            .send()
            .await
            .context("GET /healthz")?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(anyhow!("/healthz: HTTP {}", resp.status()))
        }
    }

    // ── Sessions ───────────────────────────────────────────────────────────────

    pub async fn list_sessions(&self) -> Result<Vec<SessionSummary>> {
        self.get_json("/api/sessions").await
    }

    pub async fn create_session(&self, spec: &SessionSpec) -> Result<SessionSummary> {
        let resp = self
            .auth(self.http.post(self.url("/api/sessions")))
            .json(spec)
            .send()
            .await
            .context("POST /api/sessions")?;
        Self::json_or_status(resp, "/api/sessions").await
    }

    /// Fetch a session's summary plus its base64 scrollback snapshot, decoded.
    pub async fn get_session(&self, id: Uuid) -> Result<SessionWithScrollback> {
        use base64::Engine as _;
        let v: serde_json::Value = self.get_json(&format!("/api/sessions/{id}")).await?;
        let summary: SessionSummary =
            serde_json::from_value(v["summary"].clone()).context("decode session summary")?;
        let scrollback_pos = v["scrollback_pos"].as_u64().unwrap_or(0);
        let scrollback = v["scrollback_base64"]
            .as_str()
            .and_then(|s| base64::engine::general_purpose::STANDARD.decode(s).ok())
            .unwrap_or_default();
        Ok(SessionWithScrollback {
            summary,
            scrollback_pos,
            scrollback,
        })
    }

    pub async fn rename_session(&self, id: Uuid, name: &str) -> Result<()> {
        let resp = self
            .auth(self.http.patch(self.url(&format!("/api/sessions/{id}"))))
            .json(&json!({ "name": name }))
            .send()
            .await
            .context("PATCH /api/sessions/{id}")?;
        Self::expect_ok(resp).await
    }

    pub async fn delete_session(&self, id: Uuid) -> Result<()> {
        let resp = self
            .auth(self.http.delete(self.url(&format!("/api/sessions/{id}"))))
            .send()
            .await
            .context("DELETE /api/sessions/{id}")?;
        Self::expect_ok(resp).await
    }

    pub async fn kill_session(&self, id: Uuid) -> Result<()> {
        let resp = self
            .auth(
                self.http
                    .post(self.url(&format!("/api/sessions/{id}/kill"))),
            )
            .send()
            .await
            .context("POST /api/sessions/{id}/kill")?;
        Self::expect_ok(resp).await
    }

    // ── Files ───────────────────────────────────────────────────────────────

    pub async fn list_dir(&self, path: &str) -> Result<Vec<FileEntry>> {
        self.get_json(&format!("/api/dir?path={}", urlencode(path)))
            .await
    }

    pub async fn tree(&self, path: &str, depth: u32) -> Result<Vec<TreeNode>> {
        self.get_json(&format!("/api/tree?path={}&depth={depth}", urlencode(path)))
            .await
    }

    pub async fn read_file(&self, path: &str) -> Result<FileRead> {
        self.get_json(&format!("/api/files?path={}", urlencode(path)))
            .await
    }

    pub async fn write_file(&self, req: &WriteReq) -> Result<()> {
        let resp = self
            .auth(self.http.put(self.url("/api/files")))
            .json(req)
            .send()
            .await
            .context("PUT /api/files")?;
        Self::expect_ok(resp).await
    }

    pub async fn search(&self, query: &str, path: &str, max: usize) -> Result<Vec<SearchHit>> {
        self.get_json(&format!(
            "/api/search?q={}&path={}&max={max}",
            urlencode(query),
            urlencode(path)
        ))
        .await
    }

    // ── Git ───────────────────────────────────────────────────────────────

    pub async fn git_status(&self, repo: &str) -> Result<GitStatus> {
        self.get_json(&format!("/api/git/status?repo={}", urlencode(repo)))
            .await
    }

    pub async fn git_diff(&self, repo: &str, path: &str, staged: bool) -> Result<DiffResp> {
        self.get_json(&format!(
            "/api/git/diff?repo={}&path={}&staged={staged}",
            urlencode(repo),
            urlencode(path)
        ))
        .await
    }

    pub async fn git_log(&self, repo: &str, n: u32) -> Result<Vec<LogEntry>> {
        self.get_json(&format!("/api/git/log?repo={}&n={n}", urlencode(repo)))
            .await
    }

    pub async fn git_branch(&self, repo: &str) -> Result<BranchInfo> {
        self.get_json(&format!("/api/git/branch?repo={}", urlencode(repo)))
            .await
    }

    // ── SSE events ───────────────────────────────────────────────────────────

    /// Open the `/api/events` SSE stream, yielding decoded [`ServerEvent`]s.
    /// The returned stream ends when the connection drops; callers reconnect.
    pub async fn events(&self) -> Result<impl Stream<Item = ServerEvent>> {
        let resp = self
            .auth(self.http.get(self.url("/api/events")))
            .header(reqwest::header::ACCEPT, "text/event-stream")
            .send()
            .await
            .context("GET /api/events")?
            .error_for_status()
            .context("/api/events status")?;

        let mut bytes = resp.bytes_stream();
        let stream = async_stream::stream! {
            let mut buf = String::new();
            while let Some(chunk) = bytes.next().await {
                let Ok(chunk) = chunk else { break };
                buf.push_str(&String::from_utf8_lossy(&chunk));
                // Dispatch on blank-line event boundaries.
                while let Some(idx) = buf.find('\n') {
                    let line = buf[..idx].trim_end_matches('\r').to_string();
                    buf.drain(..=idx);
                    if let Some(data) = line.strip_prefix("data:") {
                        let data = data.trim();
                        if let Ok(ev) = serde_json::from_str::<ServerEvent>(data) {
                            yield ev;
                        }
                    }
                    // ":" comment lines (keep-alive) and field lines are ignored.
                }
            }
        };
        Ok(stream)
    }

    async fn expect_ok(resp: reqwest::Response) -> Result<()> {
        let status = resp.status();
        if status.is_success() {
            Ok(())
        } else {
            let body = resp.text().await.unwrap_or_default();
            Err(anyhow!("HTTP {status}: {}", body.trim()))
        }
    }
}

/// Minimal percent-encoding for query values (path, search terms).
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ws_url_derives_from_https_base() {
        let c = ApiClient::new("https://mydevenv2.sprooty.com", "tok");
        let id = Uuid::nil();
        assert_eq!(
            c.attach_ws_url(id),
            format!("wss://mydevenv2.sprooty.com/api/sessions/{id}/attach")
        );
    }

    #[test]
    fn ws_url_derives_from_http_base() {
        let c = ApiClient::new("http://127.0.0.1:8910", "tok");
        let id = Uuid::nil();
        assert_eq!(
            c.attach_ws_url(id),
            format!("ws://127.0.0.1:8910/api/sessions/{id}/attach")
        );
    }

    #[test]
    fn urlencode_escapes_spaces_and_slashes() {
        assert_eq!(urlencode("a b/c"), "a%20b%2Fc");
        assert_eq!(urlencode("Active/apps"), "Active%2Fapps");
    }
}
