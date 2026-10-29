//! Prometheus metrics for the core (WI-1380). Ports
//! `src/vogt/adapters/http/metrics.py`.
//!
//! Served on its own listener, `VOGT_METRICS_ADDR`, never on the API port: the
//! API port is the front door, and a scrape endpoint belongs on the network the
//! scraper sits on. Unset, nothing listens.
//!
//! What it answers first is the process's CPU and memory, under the stock
//! process-exporter names (`process_resident_memory_bytes`,
//! `process_cpu_seconds_total`, `process_open_fds`, ...) so a panel written
//! against any process exporter reads this one. Every series carries
//! `component="core"`, which tells the core from the engine in a scrape that
//! sees both. Python's `python_gc_*` series have no Rust counterpart.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};

use axum::http::header;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;

use crate::errors::VogtError;

pub const METRICS_ENV: &str = "VOGT_METRICS_ADDR";
/// Suggested bind. 9464 is the engine's; 9466 is the voice sidecar's.
pub const DEFAULT_ADDR: &str = "0.0.0.0:9465";
pub const COMPONENT: &str = "core";

/// Set once a listener is up, so a second start cannot bind the port twice.
static STARTED: AtomicBool = AtomicBool::new(false);

/// `host:port`. A bare port or a missing host is refused, not guessed.
pub fn parse_addr(value: &str) -> Result<SocketAddr, VogtError> {
    let refused = || {
        VogtError::InvalidRequest(format!(
            "{METRICS_ENV}={value:?} is not an address like {DEFAULT_ADDR}"
        ))
    };
    let (host, port) = value.rsplit_once(':').ok_or_else(refused)?;
    if host.trim().is_empty() || port.is_empty() || !port.bytes().all(|b| b.is_ascii_digit()) {
        return Err(refused());
    }
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let port: u16 = port.parse().map_err(|_| refused())?;
    let ip = if host == "localhost" {
        std::net::IpAddr::from([127, 0, 0, 1])
    } else {
        host.parse().map_err(|_| refused())?
    };
    Ok(SocketAddr::new(ip, port))
}

/// The address asked for, or `None` when the variable is unset or blank.
pub fn addr_from_env(value: Option<&str>) -> Result<Option<SocketAddr>, VogtError> {
    match value.map(str::trim).filter(|value| !value.is_empty()) {
        None => Ok(None),
        Some(value) => parse_addr(value).map(Some),
    }
}

/// The scrape body, in the Prometheus text format.
pub fn render() -> String {
    let process = Process::read();
    let mut out = String::new();
    let mut series = |name: &str, kind: &str, help: &str, value: f64| {
        out.push_str(&format!(
            "# HELP {name} {help}\n# TYPE {name} {kind}\n{name}{{component=\"{COMPONENT}\"}} {value}\n"
        ));
    };
    series(
        "process_cpu_seconds_total",
        "counter",
        "Total user and system CPU time spent in seconds.",
        process.cpu_seconds,
    );
    series(
        "process_resident_memory_bytes",
        "gauge",
        "Resident memory size in bytes.",
        process.resident_bytes,
    );
    series(
        "process_virtual_memory_bytes",
        "gauge",
        "Virtual memory size in bytes.",
        process.virtual_bytes,
    );
    series(
        "process_open_fds",
        "gauge",
        "Number of open file descriptors.",
        process.open_fds,
    );
    series(
        "process_start_time_seconds",
        "gauge",
        "Start time of the process since unix epoch in seconds.",
        process.start_time,
    );
    out
}

/// What `/proc/self` says about this process. A field that cannot be read is
/// 0, as a scrape must not fail over one missing number.
#[derive(Debug, Default)]
struct Process {
    cpu_seconds: f64,
    resident_bytes: f64,
    virtual_bytes: f64,
    open_fds: f64,
    start_time: f64,
}

impl Process {
    fn read() -> Self {
        let mut process = Process::default();
        // USER_HZ and the page size as Linux ships them on every target the
        // stack image builds for.
        let ticks = 100.0;
        let page = 4096.0;
        if let Ok(stat) = std::fs::read_to_string("/proc/self/stat") {
            // The command name is parenthesised and may hold spaces.
            if let Some((_, rest)) = stat.rsplit_once(')') {
                let fields: Vec<&str> = rest.split_whitespace().collect();
                // Fields after the name start at field 3 (state).
                let field = |n: usize| -> f64 {
                    fields
                        .get(n - 3)
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(0.0)
                };
                process.cpu_seconds = (field(14) + field(15)) / ticks;
                process.virtual_bytes = field(23);
                process.resident_bytes = field(24) * page;
                let boot = std::fs::read_to_string("/proc/stat")
                    .ok()
                    .and_then(|text| {
                        text.lines()
                            .find_map(|line| line.strip_prefix("btime "))
                            .and_then(|v| v.trim().parse::<f64>().ok())
                    })
                    .unwrap_or(0.0);
                process.start_time = boot + field(22) / ticks;
            }
        }
        if let Ok(entries) = std::fs::read_dir("/proc/self/fd") {
            process.open_fds = entries.count() as f64;
        }
        process
    }
}

/// The listener's routes: `GET /metrics` and nothing else. The handler logs
/// nothing: a scrape is polled forever.
pub fn router() -> Router {
    Router::new().route(
        "/metrics",
        get(|| async {
            (
                [(
                    header::CONTENT_TYPE,
                    "text/plain; version=0.0.4; charset=utf-8",
                )],
                render(),
            )
                .into_response()
        }),
    )
}

/// Listen for `GET /metrics` on `addr` in a background task. Idempotent: a
/// second call while a listener is up changes nothing. Returns whether this
/// call started one.
pub async fn start(addr: SocketAddr) -> std::io::Result<bool> {
    if STARTED.swap(true, Ordering::SeqCst) {
        return Ok(false);
    }
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(listener) => listener,
        Err(err) => {
            STARTED.store(false, Ordering::SeqCst);
            return Err(err);
        }
    };
    eprintln!(
        "{{\"msg\":\"metrics listening\",\"bind\":\"{addr}\",\"component\":\"{COMPONENT}\"}}"
    );
    tokio::spawn(async move {
        let _ = axum::serve(listener, router()).await;
    });
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn series<'a>(body: &'a str, name: &str) -> Vec<&'a str> {
        body.lines()
            .filter(|line| line.starts_with(name) && !line.starts_with('#'))
            .collect()
    }

    #[test]
    fn nothing_listens_without_the_env() {
        assert_eq!(addr_from_env(None).unwrap(), None);
        assert_eq!(addr_from_env(Some("  ")).unwrap(), None);
    }

    #[test]
    fn a_bad_address_is_refused() {
        for bad in ["9465", ":9465", "host:", "127.0.0.1:port"] {
            assert!(
                matches!(parse_addr(bad), Err(VogtError::InvalidRequest(_))),
                "{bad}"
            );
        }
        assert_eq!(
            parse_addr("127.0.0.1:9465").unwrap(),
            "127.0.0.1:9465".parse().unwrap()
        );
        assert!(parse_addr(DEFAULT_ADDR).is_ok());
    }

    #[test]
    fn every_series_is_labelled_as_the_core() {
        let body = render();
        for name in [
            "process_resident_memory_bytes",
            "process_cpu_seconds_total",
            "process_open_fds",
        ] {
            let lines = series(&body, name);
            assert!(!lines.is_empty(), "{name}");
            assert!(
                lines.iter().all(|line| line.contains("component=\"core\"")),
                "{lines:?}"
            );
        }
        let rss: f64 = series(&body, "process_resident_memory_bytes")[0]
            .rsplit(' ')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        assert!(rss > 0.0, "a running process has resident memory");
    }

    #[test]
    fn the_listener_serves_a_scrape_and_starting_twice_does_not_rebind() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let port = std::net::TcpListener::bind("127.0.0.1:0")
                .unwrap()
                .local_addr()
                .unwrap()
                .port();
            let addr = parse_addr(&format!("127.0.0.1:{port}")).unwrap();
            assert!(start(addr).await.unwrap());
            assert!(
                !start(addr).await.unwrap(),
                "the second start changes nothing"
            );
            let body = tokio::task::spawn_blocking(move || {
                use std::io::{Read, Write};
                let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
                stream
                    .write_all(b"GET /metrics HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
                    .unwrap();
                let mut response = String::new();
                stream.read_to_string(&mut response).unwrap();
                response
            })
            .await
            .unwrap();
            assert!(body.starts_with("HTTP/1.1 200"), "{body}");
            assert!(!series(&body, "process_resident_memory_bytes").is_empty());
        });
    }
}
