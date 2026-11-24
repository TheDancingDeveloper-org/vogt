//! Prometheus metrics for the engine (WI-927).
//!
//! The question these answer first is "how long does it take to get a usable
//! session", because on 2026-10-05 the answer was two minutes and nothing said
//! so: the launch wrapper was spending ~650 ms per secret on the vendor CLI's
//! telemetry, fifteen times in series, and the only evidence was process start
//! times read out of `/proc`. With these a p95 alert sees the next one.
//!
//! Deliberately small and dependency-free: a few histogram and counter
//! families with fixed buckets, rendered in the text exposition format. The
//! label sets are fixed by the code that records them (never a session id or
//! a name), so a family's series count is bounded by construction.
//!
//! Served on its own listener, `ENGINE_METRICS_ADDR`, never on the API port:
//! the API port is the front door and is reachable from wherever the GUI is;
//! a scrape endpoint belongs on the network the scraper sits on, like every
//! other exporter. Unset, nothing listens.

use std::{
    collections::BTreeMap,
    fmt::Write as _,
    net::SocketAddr,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, LazyLock,
    },
    time::Duration,
};

use axum::{http::header, response::IntoResponse, routing::get, Router};
use parking_lot::Mutex;

/// Seconds. Spans "instant" to "someone gave up": a healthy launch sits in the
/// first buckets and the 2026-10-05 incident (85–145 s) in the last.
const LAUNCH_BUCKETS: &[f64] = &[
    0.1, 0.25, 0.5, 1.0, 2.0, 3.0, 5.0, 10.0, 20.0, 30.0, 60.0, 120.0, 300.0,
];

/// Seconds. Prometheus' default buckets: request latency, 5 ms to 10 s.
const REQUEST_BUCKETS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.075, 0.1, 0.25, 0.5, 0.75, 1.0, 2.5, 5.0, 7.5, 10.0,
];

/// Response header the core sets to the route template it matched (WI-2697).
pub const CORE_ROUTE_HEADER: &str = "x-vogt-route";

/// The route label for a request no route claimed: one series, not one per path.
pub const UNMATCHED: &str = "unmatched";

pub type Labels = Vec<(&'static str, String)>;

struct Histogram {
    buckets: &'static [f64],
    counts: Vec<AtomicU64>,
    sum_micros: AtomicU64,
    count: AtomicU64,
}

impl Histogram {
    fn new(buckets: &'static [f64]) -> Self {
        Self {
            buckets,
            counts: buckets.iter().map(|_| AtomicU64::new(0)).collect(),
            sum_micros: AtomicU64::new(0),
            count: AtomicU64::new(0),
        }
    }

    fn observe(&self, value: Duration) {
        let secs = value.as_secs_f64();
        for (bound, count) in self.buckets.iter().zip(&self.counts) {
            if secs <= *bound {
                count.fetch_add(1, Ordering::Relaxed);
            }
        }
        self.sum_micros
            .fetch_add(value.as_micros() as u64, Ordering::Relaxed);
        self.count.fetch_add(1, Ordering::Relaxed);
    }
}

/// A histogram family: one series per label set.
pub struct HistogramVec {
    name: &'static str,
    help: &'static str,
    buckets: &'static [f64],
    series: Mutex<BTreeMap<Labels, Arc<Histogram>>>,
}

impl HistogramVec {
    fn new(name: &'static str, help: &'static str, buckets: &'static [f64]) -> Self {
        Self {
            name,
            help,
            buckets,
            series: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn observe(&self, labels: &[(&'static str, &str)], value: Duration) {
        let key: Labels = labels.iter().map(|(k, v)| (*k, (*v).to_string())).collect();
        let histogram = Arc::clone(
            self.series
                .lock()
                .entry(key)
                .or_insert_with(|| Arc::new(Histogram::new(self.buckets))),
        );
        histogram.observe(value);
    }

    fn render(&self, out: &mut String) {
        let _ = writeln!(out, "# HELP {} {}", self.name, self.help);
        let _ = writeln!(out, "# TYPE {} histogram", self.name);
        for (labels, h) in self.series.lock().iter() {
            for (bound, count) in h.buckets.iter().zip(&h.counts) {
                let mut with_le = labels.clone();
                with_le.push(("le", format_bound(*bound)));
                let _ = writeln!(
                    out,
                    "{}_bucket{} {}",
                    self.name,
                    format_labels(&with_le),
                    count.load(Ordering::Relaxed)
                );
            }
            let mut inf = labels.clone();
            inf.push(("le", "+Inf".to_string()));
            let count = h.count.load(Ordering::Relaxed);
            let _ = writeln!(out, "{}_bucket{} {count}", self.name, format_labels(&inf));
            let _ = writeln!(
                out,
                "{}_sum{} {}",
                self.name,
                format_labels(labels),
                h.sum_micros.load(Ordering::Relaxed) as f64 / 1e6
            );
            let _ = writeln!(out, "{}_count{} {count}", self.name, format_labels(labels));
        }
    }
}

/// A histogram whose observations are raw magnitudes (token counts, turn
/// counts) rather than durations: the same exposition shape, but the sum is
/// the sum of the values, not microseconds divided back into seconds.
struct ValueHistogram {
    buckets: &'static [f64],
    counts: Vec<AtomicU64>,
    sum: AtomicU64,
    count: AtomicU64,
}

impl ValueHistogram {
    fn new(buckets: &'static [f64]) -> Self {
        Self {
            buckets,
            counts: buckets.iter().map(|_| AtomicU64::new(0)).collect(),
            sum: AtomicU64::new(0),
            count: AtomicU64::new(0),
        }
    }

    fn observe(&self, value: f64) {
        for (bound, count) in self.buckets.iter().zip(&self.counts) {
            if value <= *bound {
                count.fetch_add(1, Ordering::Relaxed);
            }
        }
        self.sum.fetch_add(value as u64, Ordering::Relaxed);
        self.count.fetch_add(1, Ordering::Relaxed);
    }
}

/// A family of raw-value histograms, one series per label set.
pub struct ValueHistogramVec {
    name: &'static str,
    help: &'static str,
    buckets: &'static [f64],
    series: Mutex<BTreeMap<Labels, Arc<ValueHistogram>>>,
}

impl ValueHistogramVec {
    fn new(name: &'static str, help: &'static str, buckets: &'static [f64]) -> Self {
        Self {
            name,
            help,
            buckets,
            series: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn observe(&self, labels: &[(&'static str, &str)], value: f64) {
        let key: Labels = labels.iter().map(|(k, v)| (*k, (*v).to_string())).collect();
        let histogram = Arc::clone(
            self.series
                .lock()
                .entry(key)
                .or_insert_with(|| Arc::new(ValueHistogram::new(self.buckets))),
        );
        histogram.observe(value);
    }

    fn render(&self, out: &mut String) {
        let _ = writeln!(out, "# HELP {} {}", self.name, self.help);
        let _ = writeln!(out, "# TYPE {} histogram", self.name);
        for (labels, h) in self.series.lock().iter() {
            for (bound, count) in h.buckets.iter().zip(&h.counts) {
                let mut with_le = labels.clone();
                with_le.push(("le", format_bound(*bound)));
                let _ = writeln!(
                    out,
                    "{}_bucket{} {}",
                    self.name,
                    format_labels(&with_le),
                    count.load(Ordering::Relaxed)
                );
            }
            let mut inf = labels.clone();
            inf.push(("le", "+Inf".to_string()));
            let count = h.count.load(Ordering::Relaxed);
            let _ = writeln!(out, "{}_bucket{} {count}", self.name, format_labels(&inf));
            let _ = writeln!(
                out,
                "{}_sum{} {}",
                self.name,
                format_labels(labels),
                h.sum.load(Ordering::Relaxed)
            );
            let _ = writeln!(out, "{}_count{} {count}", self.name, format_labels(labels));
        }
    }
}

/// A counter family: one series per label set.
pub struct CounterVec {
    name: &'static str,
    help: &'static str,
    series: Mutex<BTreeMap<Labels, u64>>,
}

impl CounterVec {
    fn new(name: &'static str, help: &'static str) -> Self {
        Self {
            name,
            help,
            series: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn inc(&self, labels: &[(&'static str, &str)]) {
        let key: Labels = labels.iter().map(|(k, v)| (*k, (*v).to_string())).collect();
        *self.series.lock().entry(key).or_insert(0) += 1;
    }

    fn render(&self, out: &mut String) {
        let _ = writeln!(out, "# HELP {} {}", self.name, self.help);
        let _ = writeln!(out, "# TYPE {} counter", self.name);
        for (labels, value) in self.series.lock().iter() {
            let _ = writeln!(out, "{}{} {value}", self.name, format_labels(labels));
        }
    }
}

/// A gauge family whose whole series set is replaced at once (WI-1373). The
/// per-session resource gauges are written this way by the sampler every
/// round, so a session that stops or hibernates drops out of the next scrape
/// instead of leaving a stale series behind: the series count is bounded by
/// the sessions alive now, not by every session the engine has ever run.
pub struct GaugeVec {
    name: &'static str,
    help: &'static str,
    series: Mutex<BTreeMap<Labels, f64>>,
}

impl GaugeVec {
    fn new(name: &'static str, help: &'static str) -> Self {
        Self {
            name,
            help,
            series: Mutex::new(BTreeMap::new()),
        }
    }

    /// Replace every series with `values`.
    pub fn replace(&self, values: impl IntoIterator<Item = (Labels, f64)>) {
        *self.series.lock() = values.into_iter().collect();
    }

    fn render(&self, out: &mut String) {
        let _ = writeln!(out, "# HELP {} {}", self.name, self.help);
        let _ = writeln!(out, "# TYPE {} gauge", self.name);
        for (labels, value) in self.series.lock().iter() {
            let _ = writeln!(out, "{}{} {value}", self.name, format_labels(labels));
        }
    }
}

fn format_bound(bound: f64) -> String {
    let text = format!("{bound}");
    if text.contains('.') {
        text
    } else {
        format!("{text}.0")
    }
}

fn format_labels(labels: &[(&'static str, String)]) -> String {
    if labels.is_empty() {
        return String::new();
    }
    let body: Vec<String> = labels
        .iter()
        .map(|(k, v)| {
            let escaped = v
                .replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('\n', "\\n");
            format!("{k}=\"{escaped}\"")
        })
        .collect();
    format!("{{{}}}", body.join(","))
}

/// Every metric the engine exports.
pub struct Metrics {
    /// Spawn to the session's first byte of output, by launcher
    /// (`agent-auth` when the launch wrapper runs first, `direct` otherwise).
    pub first_output: HistogramVec,
    /// The launch wrapper's own total, spawn to handover, by command and
    /// outcome, from its launch report.
    pub launch: HistogramVec,
    /// One launch stage, from the launch report: `login`, `secrets` (one
    /// observation per project read), `bootstrap`.
    pub launch_stage: HistogramVec,
    /// Sessions the engine started, by origin and outcome.
    pub session_starts: CounterVec,
    /// Secret projects read at launch, by how (`bulk`: one request; `cli`:
    /// one vendor-CLI run per secret, the slow fallback) and outcome.
    pub launch_vault_read_count: CounterVec,
    /// Turns a session had when it ended, by template and model. Observed once,
    /// at exit or hibernation, from the session's economics snapshot.
    pub session_turns: ValueHistogramVec,
    /// Peak context window a session reached, in tokens.
    pub session_peak_context: ValueHistogramVec,
    /// Compactions a session performed.
    pub session_compactions: ValueHistogramVec,
    /// Resident memory of each live session's process tree, from the
    /// resource sampler, by session, template, agent, model and project.
    pub session_rss: GaugeVec,
    /// CPU of each live session's tree over the last sampling interval.
    pub session_cpu: GaugeVec,
    /// Processes in each live session's tree.
    pub session_processes: GaugeVec,
    /// Resident memory summed over every live session's tree (unlabelled).
    pub sessions_rss_total: GaugeVec,
    /// Resident memory of every process the engine can see: in a container,
    /// the whole pod, sessions or not (unlabelled).
    pub pod_rss: GaugeVec,
    /// Terminal attaches that authenticated and found a live session.
    pub ws_attach: CounterVec,
    /// Terminal attach sockets ended, by the close code the engine sent
    /// (`none` when the client went first or the socket failed).
    pub ws_close: CounterVec,
    /// Terminal attach sockets closed `4000 idle` by the server keepalive.
    pub ws_idle_close: CounterVec,
    /// Time to response start of every engine request, by route template,
    /// method and status (WI-2697). The `/api/vogt` proxy hop is labelled by
    /// the core's route template, from `x-vogt-route`.
    pub http_request: HistogramVec,
}

impl Metrics {
    fn new() -> Self {
        Self {
            first_output: HistogramVec::new(
                "vogt_session_first_output_seconds",
                "Time from spawning a session to its first byte of output.",
                LAUNCH_BUCKETS,
            ),
            launch: HistogramVec::new(
                "vogt_session_launch_seconds",
                "Time the launch wrapper took from spawn to handing over to the shell or agent.",
                LAUNCH_BUCKETS,
            ),
            launch_stage: HistogramVec::new(
                "vogt_session_launch_stage_seconds",
                "Time one stage of a session launch took (login, secrets per project, bootstrap).",
                LAUNCH_BUCKETS,
            ),
            session_starts: CounterVec::new(
                "vogt_session_starts_total",
                "Sessions the engine started, by origin and outcome.",
            ),
            launch_vault_read_count: CounterVec::new(
                "vogt_session_launch_secret_reads_total",
                "Secret projects read at session launch, by mode (bulk or cli) and outcome.",
            ),
            session_turns: ValueHistogramVec::new(
                "vogt_session_turns",
                "Turns a session had when it ended, by template and model.",
                &[10.0, 25.0, 50.0, 100.0, 150.0, 250.0, 500.0, 1000.0],
            ),
            session_peak_context: ValueHistogramVec::new(
                "vogt_session_peak_context_tokens",
                "Peak context window a session reached, in tokens, by template and model.",
                &[
                    10_000.0,
                    25_000.0,
                    50_000.0,
                    100_000.0,
                    200_000.0,
                    400_000.0,
                    1_000_000.0,
                ],
            ),
            session_compactions: ValueHistogramVec::new(
                "vogt_session_compactions",
                "Compactions a session performed, by template and model.",
                &[0.0, 1.0, 2.0, 3.0, 5.0, 10.0, 25.0, 50.0],
            ),
            session_rss: GaugeVec::new(
                "vogt_session_rss_bytes",
                "Resident memory of a live session's process tree, in bytes.",
            ),
            session_cpu: GaugeVec::new(
                "vogt_session_cpu_percent",
                "CPU of a live session's process tree over the last sample, in percent of one core.",
            ),
            session_processes: GaugeVec::new(
                "vogt_session_processes",
                "Processes in a live session's process tree.",
            ),
            sessions_rss_total: GaugeVec::new(
                "vogt_sessions_rss_bytes",
                "Resident memory summed over every live session's process tree, in bytes.",
            ),
            pod_rss: GaugeVec::new(
                "vogt_pod_rss_bytes",
                "Resident memory summed over every process the engine can see (the pod), in bytes.",
            ),
            ws_attach: CounterVec::new(
                "vogt_ws_attach_total",
                "Terminal attaches that authenticated and found a live session.",
            ),
            ws_close: CounterVec::new(
                "vogt_ws_close_total",
                "Terminal attach sockets ended, by the close code the engine sent (none: client or transport).",
            ),
            ws_idle_close: CounterVec::new(
                "vogt_ws_idle_close_total",
                "Terminal attach sockets closed by the server keepalive after missed pongs.",
            ),
            http_request: HistogramVec::new(
                "vogt_engine_http_request_seconds",
                "Time the engine took to start answering a request, by route template, method and status.",
                REQUEST_BUCKETS,
            ),
        }
    }

    /// The exposition text for every family.
    pub fn render(&self) -> String {
        let mut out = String::new();
        self.first_output.render(&mut out);
        self.launch.render(&mut out);
        self.launch_stage.render(&mut out);
        self.session_starts.render(&mut out);
        self.launch_vault_read_count.render(&mut out);
        self.session_turns.render(&mut out);
        self.session_peak_context.render(&mut out);
        self.session_compactions.render(&mut out);
        self.session_rss.render(&mut out);
        self.session_cpu.render(&mut out);
        self.session_processes.render(&mut out);
        self.sessions_rss_total.render(&mut out);
        self.pod_rss.render(&mut out);
        self.ws_attach.render(&mut out);
        self.ws_close.render(&mut out);
        self.ws_idle_close.render(&mut out);
        self.http_request.render(&mut out);
        out
    }
}

static METRICS: LazyLock<Metrics> = LazyLock::new(Metrics::new);

/// The process-wide registry.
pub fn metrics() -> &'static Metrics {
    &METRICS
}

/// The route label for one request: axum's matched template, except on the
/// `/api/vogt` proxy hop, where it is the core's template (from the core's
/// `x-vogt-route`, mapped back under `/api/vogt`) so the label names the core
/// route rather than the catch-all. Never a raw path: a path carries ids.
pub fn route_label(matched: Option<&str>, core_route: Option<&str>) -> String {
    let Some(matched) = matched else {
        return UNMATCHED.to_string();
    };
    if matched.starts_with(crate::vogt_core::API_PREFIX) {
        if let Some(core) = core_route.filter(|r| r.starts_with('/') && *r != UNMATCHED) {
            let rest = core.strip_prefix("/api").unwrap_or(core);
            return format!("{}{rest}", crate::vogt_core::API_PREFIX);
        }
    }
    matched.to_string()
}

/// Route-layer middleware: time the request to its response start and record
/// it under the matched route template. A route layer, not an outer one,
/// because axum only knows the [`MatchedPath`] once it has routed; requests
/// no route claims fall to the catch-alls, which are routes too.
pub async fn record_request(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let matched = request
        .extensions()
        .get::<axum::extract::MatchedPath>()
        .map(|m| m.as_str().to_string());
    let method = request.method().as_str().to_string();
    let started = std::time::Instant::now();
    let response = next.run(request).await;
    let core_route = response
        .headers()
        .get(CORE_ROUTE_HEADER)
        .and_then(|v| v.to_str().ok());
    let route = route_label(matched.as_deref(), core_route);
    let status = response.status().as_u16().to_string();
    metrics().http_request.observe(
        &[("route", &route), ("method", &method), ("status", &status)],
        started.elapsed(),
    );
    response
}

async fn scrape() -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        metrics().render(),
    )
}

/// The scrape router: `GET /metrics` and nothing else.
pub fn router() -> Router {
    Router::new().route("/metrics", get(scrape))
}

/// Serve `/metrics` on `addr` until the process exits. A bind failure is
/// logged and the engine carries on: metrics are worth having, not worth
/// refusing to start over.
pub fn spawn_listener(addr: SocketAddr) {
    tokio::spawn(async move {
        match tokio::net::TcpListener::bind(addr).await {
            Ok(listener) => {
                tracing::info!(addr = %addr, "metrics listening");
                if let Err(e) = axum::serve(listener, router()).await {
                    tracing::warn!(addr = %addr, error = %e, "metrics listener stopped");
                }
            }
            Err(e) => {
                tracing::warn!(addr = %addr, error = %e, "metrics listener could not bind; no /metrics")
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_histogram_renders_cumulative_buckets_sum_and_count() {
        let h = HistogramVec::new("t_seconds", "help text", &[1.0, 5.0]);
        h.observe(&[("launcher", "agent-auth")], Duration::from_millis(500));
        h.observe(&[("launcher", "agent-auth")], Duration::from_secs(3));
        h.observe(&[("launcher", "agent-auth")], Duration::from_secs(90));
        let mut out = String::new();
        h.render(&mut out);
        assert!(out.contains("# TYPE t_seconds histogram"), "{out}");
        assert!(
            out.contains("t_seconds_bucket{launcher=\"agent-auth\",le=\"1.0\"} 1"),
            "{out}"
        );
        assert!(
            out.contains("t_seconds_bucket{launcher=\"agent-auth\",le=\"5.0\"} 2"),
            "{out}"
        );
        assert!(
            out.contains("t_seconds_bucket{launcher=\"agent-auth\",le=\"+Inf\"} 3"),
            "{out}"
        );
        assert!(
            out.contains("t_seconds_sum{launcher=\"agent-auth\"} 93.5"),
            "{out}"
        );
        assert!(
            out.contains("t_seconds_count{launcher=\"agent-auth\"} 3"),
            "{out}"
        );
    }

    #[test]
    fn the_proxy_hop_is_labelled_by_the_core_route_template() {
        assert_eq!(
            route_label(Some("/api/vogt/{*path}"), Some("/api/work/list")),
            "/api/vogt/work/list"
        );
        // No header (core down, refused before forwarding): the catch-all.
        assert_eq!(
            route_label(Some("/api/vogt/{*path}"), None),
            "/api/vogt/{*path}"
        );
        assert_eq!(
            route_label(Some("/api/vogt/{*path}"), Some(UNMATCHED)),
            "/api/vogt/{*path}"
        );
        // An engine route keeps its own template whatever a header says.
        assert_eq!(
            route_label(Some("/api/sessions/{id}"), Some("/api/x")),
            "/api/sessions/{id}"
        );
        assert_eq!(route_label(None, None), UNMATCHED);
    }

    #[test]
    fn a_counter_counts_per_label_set_and_escapes_values() {
        let c = CounterVec::new("t_total", "help");
        c.inc(&[("outcome", "ok")]);
        c.inc(&[("outcome", "ok")]);
        c.inc(&[("outcome", "a\"b")]);
        let mut out = String::new();
        c.render(&mut out);
        assert!(out.contains("t_total{outcome=\"ok\"} 2"), "{out}");
        assert!(out.contains("t_total{outcome=\"a\\\"b\"} 1"), "{out}");
    }

    #[test]
    fn a_gauge_replace_drops_series_that_are_not_given_again() {
        let g = GaugeVec::new("t_bytes", "help");
        g.replace([
            (vec![("session", "a".to_string())], 10.0),
            (vec![("session", "b".to_string())], 20.0),
        ]);
        let mut out = String::new();
        g.render(&mut out);
        assert!(out.contains("# TYPE t_bytes gauge"), "{out}");
        assert!(out.contains("t_bytes{session=\"a\"} 10"), "{out}");
        assert!(out.contains("t_bytes{session=\"b\"} 20"), "{out}");
        g.replace([(vec![("session", "b".to_string())], 30.0)]);
        let mut out = String::new();
        g.render(&mut out);
        assert!(!out.contains("session=\"a\""), "{out}");
        assert!(out.contains("t_bytes{session=\"b\"} 30"), "{out}");
    }
}
