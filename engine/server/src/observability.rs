//! One request id, on both sides of the front door (#139).
//!
//! The engine already had structured logging — `tracing` with `EnvFilter`,
//! thirty-odd call sites with real fields — and the core had almost none. The
//! half that was missing from *both* was a way to follow one request across
//! them: the engine proxies `/api/vogt` and `/mcp` straight through to a
//! process in another language with its own logging stack, so "the GUI was
//! slow" could not be turned into "this request, here, took nine seconds".
//!
//! So this middleware does two things and nothing else. It gives every
//! request an id — the caller's, when the caller sent a usable one, so a
//! browser or an agent can name the request it is asking about — and it
//! writes one line per request with the status and how long the answer took
//! to start. `vogt_core::forward` sends that id upstream, and the core
//! attaches it to every line it writes while serving the request, which is
//! what makes the two logs one timeline.
//!
//! **The duration is time to the response starting**, not to its last byte:
//! `next.run` returns once the handler has produced a response, and the
//! bodies most worth measuring here — SSE at `/api/events`, a proxied `/mcp`
//! stream, a file download — are open long after that. A log that reported an
//! hour-long event stream as an hour-long request would bury the four-second
//! `/api/git/status` that is the actual complaint.

use std::time::Instant;

use axum::{extract::Request, http::HeaderValue, middleware::Next, response::Response};

/// The header carried between the door and the core, and echoed to the
/// caller. Lower case: it is written by machines and read from logs.
pub const REQUEST_ID_HEADER: &str = "x-request-id";

/// A request slower than this to answer is logged at `warn` rather than
/// `info`. A constant rather than configuration because the verbosity knob
/// this deployment already has is `RUST_LOG`, and a threshold nobody has ever
/// wanted to change is not a setting — it is a number with a comment.
const SLOW_REQUEST_MS: u128 = 1_000;

/// Paths logged at `debug`: the probe an orchestrator calls forever, and the
/// static assets one page load fetches thirty of. Suppressed rather than
/// dropped — the slow check below runs first, so a `/healthz` that took nine
/// seconds still says so, which is the case that mattered on 2026-08-19.
const QUIET_PATHS: [&str; 4] = ["/healthz", "/health/live", "/health/ready", "/version"];
const QUIET_PREFIX: &str = "/assets/";

/// The id assigned to the request being served. Read by `vogt_core::forward`,
/// which is the whole point: the core is told which request this is.
#[derive(Clone, Debug)]
pub struct RequestId(pub String);

/// A correlation id ends up in log lines, so what a caller sends is checked
/// rather than trusted: an identifier's characters, and a bounded length. A
/// value that fails this is replaced, not rejected — the request is fine, it
/// is only the label that was unusable.
fn accepted(raw: &str) -> Option<String> {
    let candidate = raw.trim();
    let usable = !candidate.is_empty()
        && candidate.len() <= 64
        && candidate
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    usable.then(|| candidate.to_string())
}

fn new_request_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..16].to_string()
}

fn is_quiet(path: &str) -> bool {
    QUIET_PATHS.contains(&path) || path.starts_with(QUIET_PREFIX)
}

/// A request the client walked away from before a response was ready.
///
/// hyper drops the whole handler future when the connection closes, so the
/// line below `next.run` is never reached and the request never appears in
/// the log. That is how the 2026-09-05 stall was invisible from the front
/// door: the browser aborted every badge read at its deadline, the door
/// logged nothing, and the core logged 27 reads that took minutes each
/// (#581). The guard says so on drop, with the time the client waited —
/// which is the number an operator needs, because a request abandoned at
/// exactly the client's deadline is a request the server was too slow for.
struct Abandoned {
    id: String,
    method: axum::http::Method,
    path: String,
    started: Instant,
    answered: bool,
}

impl Drop for Abandoned {
    fn drop(&mut self) {
        if self.answered {
            return;
        }
        tracing::warn!(
            request_id = %self.id, method = %self.method, path = %self.path,
            duration_ms = %self.started.elapsed().as_millis(),
            "request abandoned by the client before a response was ready"
        );
    }
}

/// Assign the id, serve the request, say what happened.
pub async fn access_log(mut request: Request, next: Next) -> Response {
    let id = request
        .headers()
        .get(REQUEST_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .and_then(accepted)
        .unwrap_or_else(new_request_id);
    let method = request.method().clone();
    let path = request.uri().path().to_string();

    request.extensions_mut().insert(RequestId(id.clone()));

    let started = Instant::now();
    let mut guard = Abandoned {
        id: id.clone(),
        method: method.clone(),
        path: path.clone(),
        started,
        answered: false,
    };
    let mut response = next.run(request).await;
    guard.answered = true;
    let duration_ms = started.elapsed().as_millis();
    let status = response.status().as_u16();

    if let Ok(value) = HeaderValue::from_str(&id) {
        response.headers_mut().insert(REQUEST_ID_HEADER, value);
    }

    if status >= 500 || duration_ms >= SLOW_REQUEST_MS {
        tracing::warn!(
            request_id = %id, method = %method, path = %path,
            status, duration_ms = %duration_ms, "slow or failed request"
        );
    } else if is_quiet(&path) {
        tracing::debug!(
            request_id = %id, method = %method, path = %path,
            status, duration_ms = %duration_ms, "request"
        );
    } else {
        tracing::info!(
            request_id = %id, method = %method, path = %path,
            status, duration_ms = %duration_ms, "request"
        );
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_callers_id_is_kept_when_it_is_an_identifier() {
        assert_eq!(
            accepted("engine-abc_123.4"),
            Some("engine-abc_123.4".into())
        );
        assert_eq!(accepted("  trimmed  "), Some("trimmed".into()));
    }

    #[test]
    fn an_unusable_id_is_refused_so_it_cannot_reach_a_log_line() {
        assert_eq!(accepted(""), None);
        assert_eq!(accepted("has space"), None);
        assert_eq!(accepted("line\nbreak"), None);
        assert_eq!(accepted(&"x".repeat(65)), None);
    }

    #[test]
    fn a_generated_id_is_short_and_stable_in_shape() {
        let id = new_request_id();
        assert_eq!(id.len(), 16);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn probes_and_assets_are_the_quiet_ones() {
        assert!(is_quiet("/healthz"));
        assert!(is_quiet("/assets/index-abc123.js"));
        assert!(!is_quiet("/api/vogt/work"));
    }
}
