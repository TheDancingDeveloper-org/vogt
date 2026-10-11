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

type Labels = Vec<(&'static str, String)>;

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
    pub launch_secret_reads: CounterVec,
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
            launch_secret_reads: CounterVec::new(
                "vogt_session_launch_secret_reads_total",
                "Secret projects read at session launch, by mode (bulk or cli) and outcome.",
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
        self.launch_secret_reads.render(&mut out);
        out
    }
}

static METRICS: LazyLock<Metrics> = LazyLock::new(Metrics::new);

/// The process-wide registry.
pub fn metrics() -> &'static Metrics {
    &METRICS
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
}
