//! One structured line per request, with a duration on it. Ports
//! `src/vogt/adapters/http/access_log.py`.
//!
//! Two durations are recorded because they answer different questions. `ttfb_ms`
//! is time to the response *starting* — the number that says an endpoint is
//! slow, and the only meaningful one for a stream that stays open for an hour.
//! `duration_ms` is the whole exchange, which is what a reader expects an access
//! log to say, so the line is written when the body finishes or is dropped. The
//! slow-request warning is judged on the first, so an SSE subscription does not
//! report itself as a pathological request every time a client disconnects.
//!
//! The body is wrapped rather than buffered: the middleware reads frames as they
//! pass and touches nothing else, so `/mcp` keeps streaming.

use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Instant;

use axum::body::{Body, Bytes};
use axum::extract::{ConnectInfo, Request, State};
use axum::http::HeaderValue;
use axum::middleware::Next;
use axum::response::Response;
use http_body::{Frame, SizeHint};

use crate::config::VogtConfig;
use crate::observability::{
    accepted_request_id, current_actor, with_request_id, REQUEST_ID_HEADER,
};

/// What the operator decided about the access log.
#[derive(Clone, Debug)]
pub struct AccessLogSettings {
    /// Off makes this middleware a request-id middleware and nothing else: the
    /// correlation header is still honoured and echoed, because a deployment that
    /// turns the access log down still has to follow one request across runtimes.
    pub enabled: bool,
    /// Above this, a request is logged at WARNING instead of INFO.
    pub slow_request_ms: f64,
    /// Paths logged at DEBUG rather than INFO — health and readiness probes.
    pub quiet_paths: Vec<String>,
}

impl AccessLogSettings {
    pub fn from_config(config: &VogtConfig) -> Self {
        Self {
            enabled: config.log_requests,
            slow_request_ms: config.log_slow_request_ms.max(0) as f64,
            quiet_paths: config.log_quiet_paths.clone(),
        }
    }
}

/// A request id for one that arrived without a usable one: 16 hex characters,
/// as Python's `uuid.uuid4().hex[:16]`.
pub fn new_request_id() -> String {
    let mut bytes = [0u8; 8];
    // The OS entropy source, as the token minting uses. A failure here cannot
    // be surfaced from a header, so the id falls back to zeroes rather than
    // refusing the request; the id only correlates log lines.
    let _ = getrandom::getrandom(&mut bytes);
    hex::encode(bytes)
}

/// Wrap a router in the access log. The one place the middleware is applied, so
/// `serve` and the adapter's tests cannot disagree about where it sits.
pub fn layer(router: axum::Router, settings: AccessLogSettings) -> axum::Router {
    router.layer(axum::middleware::from_fn_with_state(
        std::sync::Arc::new(settings),
        access_log,
    ))
}

/// Assign a request id, run the request, then say what happened and how long it
/// took. Outermost on the router, so the line covers routing, authentication, the
/// error handlers, and the requests that never reach a route at all.
pub async fn access_log(
    State(settings): State<std::sync::Arc<AccessLogSettings>>,
    request: Request,
    next: Next,
) -> Response {
    let request_id = accepted_request_id(
        request
            .headers()
            .get(REQUEST_ID_HEADER)
            .and_then(|value| value.to_str().ok()),
    )
    .unwrap_or_else(new_request_id);
    let started = Instant::now();
    let method = request.method().to_string();
    let path = request.uri().path().to_string();
    let query = request.uri().query().unwrap_or_default().to_string();
    let client = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|info| info.0.ip().to_string())
        .unwrap_or_default();

    // The actor is set by authentication inside the handler, so it is read
    // inside the request's scope, once the response has been produced.
    let (mut response, actor) = with_request_id(request_id.clone(), async {
        let response = next.run(request).await;
        (response, current_actor().unwrap_or_default())
    })
    .await;

    // The caller gets the id back whether or not it sent one, so a slow answer can
    // be quoted in a bug report and found in the log.
    if let Ok(value) = HeaderValue::from_str(&request_id) {
        response.headers_mut().insert(REQUEST_ID_HEADER, value);
    }
    if !settings.enabled {
        return response;
    }

    let ttfb_ms = round1(started.elapsed().as_secs_f64() * 1000.0);
    let line = AccessLine {
        method,
        path,
        query,
        client,
        actor,
        status: response.status().as_u16(),
        started,
        ttfb_ms,
        settings,
    };
    let (parts, body) = response.into_parts();
    Response::from_parts(
        parts,
        Body::new(Exchange {
            inner: body,
            line,
            bytes: 0,
        }),
    )
}

fn round1(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

/// Everything the line needs, held until the body is finished or dropped.
struct AccessLine {
    method: String,
    path: String,
    query: String,
    client: String,
    actor: String,
    status: u16,
    started: Instant,
    ttfb_ms: f64,
    settings: std::sync::Arc<AccessLogSettings>,
}

impl AccessLine {
    fn write(&self, bytes: u64) {
        let duration_ms = round1(self.started.elapsed().as_secs_f64() * 1000.0);
        let (method, path, query) = (&self.method, &self.path, &self.query);
        let (status, ttfb_ms, client, actor) =
            (self.status, self.ttfb_ms, &self.client, &self.actor);
        // Python's rule, in the same order: a failure or slow response is a
        // warning, a quiet path is debug, everything else is info.
        if status >= 500 || ttfb_ms >= self.settings.slow_request_ms {
            tracing::warn!(
                target: "vogt.http",
                method = %method, path = %path, query = %query, status,
                duration_ms, ttfb_ms, bytes, client = %client, actor = %actor,
                "slow or failed request"
            );
        } else if self.settings.quiet_paths.iter().any(|quiet| quiet == path) {
            tracing::debug!(
                target: "vogt.http",
                method = %method, path = %path, query = %query, status,
                duration_ms, ttfb_ms, bytes, client = %client, actor = %actor,
                "request"
            );
        } else {
            tracing::info!(
                target: "vogt.http",
                method = %method, path = %path, query = %query, status,
                duration_ms, ttfb_ms, bytes, client = %client, actor = %actor,
                "request"
            );
        }
    }
}

/// A body that counts its bytes as they pass and writes the line when it ends.
/// Dropping it early (a client that went away) still writes the line.
struct Exchange {
    inner: Body,
    line: AccessLine,
    bytes: u64,
}

impl Drop for Exchange {
    fn drop(&mut self) {
        self.line.write(self.bytes);
    }
}

impl http_body::Body for Exchange {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, axum::Error>>> {
        let polled = Pin::new(&mut self.inner).poll_frame(cx);
        if let Poll::Ready(Some(Ok(frame))) = &polled {
            if let Some(data) = frame.data_ref() {
                self.bytes += data.len() as u64;
            }
        }
        polled
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_generated_request_id_is_sixteen_hex_characters() {
        let id = new_request_id();
        assert_eq!(id.len(), 16);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(id, new_request_id());
    }

    #[test]
    fn durations_are_rounded_to_a_tenth_of_a_millisecond() {
        assert_eq!(round1(12.34), 12.3);
        assert_eq!(round1(12.36), 12.4);
    }

    #[test]
    fn settings_follow_the_config() {
        let config = VogtConfig {
            log_requests: false,
            log_slow_request_ms: 250,
            log_quiet_paths: vec!["/health/ready".to_string()],
            ..VogtConfig::default()
        };
        let settings = AccessLogSettings::from_config(&config);
        assert!(!settings.enabled);
        assert_eq!(settings.slow_request_ms, 250.0);
        assert_eq!(settings.quiet_paths, vec!["/health/ready".to_string()]);
    }
}
