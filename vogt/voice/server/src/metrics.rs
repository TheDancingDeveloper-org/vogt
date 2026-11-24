//! Prometheus process metrics for the voice sidecar.
//!
//! Served on its own listener, `VOGT_VOICE_METRICS_ADDR`, never on the API
//! port: the API port is the front door the engine talks to, and a scrape
//! endpoint belongs on the network the scraper sits on. Unset, nothing
//! listens.
//!
//! The names match the Prometheus process collector so a dashboard written
//! for `process_*` reads this sidecar the same way: resident memory, CPU
//! seconds, and OS threads. In-process model weights are a separate gauge,
//! the on-disk size of the files actually loaded (Whisper GGML, Piper ONNX),
//! not an allocator measurement — cheap, and stable across scrapes.

use std::{
    fmt::Write as _,
    fs::File,
    io::{BufRead, BufReader, Read},
    net::SocketAddr,
    path::Path,
};

use axum::{http::header, response::IntoResponse, routing::get, Router};

/// `process_resident_memory_bytes`: `VmRSS` from `/proc/self/status`, in bytes.
pub fn resident_memory_bytes() -> Option<u64> {
    let file = File::open("/proc/self/status").ok()?;
    for line in BufReader::new(file).lines() {
        let line = line.ok()?;
        let Some(rest) = line.strip_prefix("VmRSS:") else {
            continue;
        };
        let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
        return Some(kb.saturating_mul(1024));
    }
    None
}

/// `process_cpu_seconds_total`: user + system time from `/proc/self/stat`.
///
/// Fields after `comm` (which may contain spaces and parentheses) are counted
/// from the last `)`, so a comm that itself contains `)` does not shift the
/// utime/stime positions.
pub fn cpu_seconds_total() -> Option<f64> {
    let mut raw = String::new();
    File::open("/proc/self/stat")
        .ok()?
        .read_to_string(&mut raw)
        .ok()?;
    let after = raw.rsplit_once(')')?.1;
    let mut fields = after.split_whitespace();
    // After ')': state, ppid, pgrp, session, tty_nr, tpgid, flags, minflt,
    // cminflt, majflt, cmajflt, utime, stime.
    let utime: u64 = fields.nth(11)?.parse().ok()?;
    let stime: u64 = fields.next()?.parse().ok()?;
    Some(ticks_to_seconds(utime.saturating_add(stime)))
}

fn ticks_to_seconds(ticks: u64) -> f64 {
    // Linux user_hz is 100 on every architecture Vogt ships. Read it from
    // `/proc` rather than linking libc just for sysconf(_SC_CLK_TCK): the
    // btime line is not the tick rate, so the constant is stated here.
    const USER_HZ: f64 = 100.0;
    ticks as f64 / USER_HZ
}

/// `process_threads`: `Threads` from `/proc/self/status`.
pub fn thread_count() -> Option<u64> {
    let file = File::open("/proc/self/status").ok()?;
    for line in BufReader::new(file).lines() {
        let line = line.ok()?;
        let Some(rest) = line.strip_prefix("Threads:") else {
            continue;
        };
        return rest.split_whitespace().next()?.parse().ok();
    }
    None
}

/// Bytes of in-process model weights loaded at startup: the Whisper GGML file
/// and the Piper ONNX file beside its JSON config. A missing file is skipped;
/// an unconfigured half contributes nothing. This is file size, not resident
/// pages, so it does not double-count `process_resident_memory_bytes`.
pub fn model_bytes_loaded(stt_model_path: Option<&Path>, tts_config_path: Option<&Path>) -> u64 {
    let mut total = 0u64;
    if let Some(path) = stt_model_path {
        total = total.saturating_add(file_len(path));
    }
    if let Some(config) = tts_config_path {
        if let Some(onnx) = piper_onnx_path(config) {
            total = total.saturating_add(file_len(&onnx));
        }
    }
    total
}

fn file_len(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

/// `voice.onnx.json` -> `voice.onnx`, the naming piper-rs loads.
fn piper_onnx_path(config: &Path) -> Option<std::path::PathBuf> {
    let name = config.file_name()?.to_str()?;
    let stem = name
        .strip_suffix(".onnx.json")
        .or_else(|| name.strip_suffix(".json"))?;
    Some(config.with_file_name(format!("{stem}.onnx")))
}

/// The exposition text. Families with no reading (no `/proc`) are omitted
/// rather than reported as zero.
pub fn render(stt_model_path: Option<&Path>, tts_config_path: Option<&Path>) -> String {
    let mut out = String::new();
    if let Some(bytes) = resident_memory_bytes() {
        gauge(
            &mut out,
            "process_resident_memory_bytes",
            "Resident memory size in bytes.",
            bytes as f64,
        );
    }
    if let Some(seconds) = cpu_seconds_total() {
        counter(
            &mut out,
            "process_cpu_seconds_total",
            "Total user and system CPU time spent in seconds.",
            seconds,
        );
    }
    if let Some(threads) = thread_count() {
        gauge(
            &mut out,
            "process_threads",
            "Number of OS threads in the process.",
            threads as f64,
        );
    }
    gauge(
        &mut out,
        "vogt_voice_model_bytes",
        "On-disk size in bytes of in-process STT and TTS model weights loaded at startup.",
        model_bytes_loaded(stt_model_path, tts_config_path) as f64,
    );
    out
}

fn gauge(out: &mut String, name: &str, help: &str, value: f64) {
    let _ = writeln!(out, "# HELP {name} {help}");
    let _ = writeln!(out, "# TYPE {name} gauge");
    let _ = writeln!(out, "{name} {value}");
}

fn counter(out: &mut String, name: &str, help: &str, value: f64) {
    let _ = writeln!(out, "# HELP {name} {help}");
    let _ = writeln!(out, "# TYPE {name} counter");
    let _ = writeln!(out, "{name} {value}");
}

async fn scrape(
    axum::extract::State(paths): axum::extract::State<ModelPaths>,
) -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        render(paths.stt.as_deref(), paths.tts.as_deref()),
    )
}

#[derive(Clone)]
struct ModelPaths {
    stt: Option<std::path::PathBuf>,
    tts: Option<std::path::PathBuf>,
}

/// The scrape router: `GET /metrics` and nothing else.
pub fn router(stt_model_path: Option<&Path>, tts_config_path: Option<&Path>) -> Router {
    let paths = ModelPaths {
        stt: stt_model_path.map(std::path::Path::to_path_buf),
        tts: tts_config_path.map(std::path::Path::to_path_buf),
    };
    Router::new()
        .route("/metrics", get(scrape))
        .with_state(paths)
}

/// Serve `/metrics` on `addr` until the process exits. A bind failure is
/// logged and the sidecar carries on: metrics are worth having, not worth
/// refusing to start over.
pub fn spawn_metrics_listener(
    addr: SocketAddr,
    stt_model_path: Option<std::path::PathBuf>,
    tts_config_path: Option<std::path::PathBuf>,
) {
    tokio::spawn(async move {
        match tokio::net::TcpListener::bind(addr).await {
            Ok(listener) => {
                tracing::info!(%addr, "metrics listening");
                if let Err(error) = axum::serve(
                    listener,
                    router(stt_model_path.as_deref(), tts_config_path.as_deref()),
                )
                .await
                {
                    tracing::warn!(%addr, %error, "metrics listener stopped");
                }
            }
            Err(error) => {
                tracing::warn!(%addr, %error, "metrics listener could not bind; no /metrics");
            }
        }
    });
}

/// Parse `VOGT_VOICE_METRICS_ADDR`. Empty or unset is off.
pub fn metrics_addr_from_env() -> Result<Option<SocketAddr>, String> {
    match std::env::var("VOGT_VOICE_METRICS_ADDR") {
        Err(_) => Ok(None),
        Ok(value) if value.trim().is_empty() => Ok(None),
        Ok(value) => value.parse::<SocketAddr>().map(Some).map_err(|_| {
            format!("VOGT_VOICE_METRICS_ADDR={value:?} is not an address like 0.0.0.0:9465")
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn process_gauges_read_on_linux() {
        let rss = resident_memory_bytes().expect("VmRSS");
        assert!(rss > 0, "{rss}");
        let cpu = cpu_seconds_total().expect("cpu");
        assert!(cpu >= 0.0, "{cpu}");
        let threads = thread_count().expect("threads");
        assert!(threads >= 1, "{threads}");
    }

    #[test]
    fn render_uses_prometheus_process_names() {
        let text = render(None, None);
        assert!(
            text.contains("# TYPE process_resident_memory_bytes gauge"),
            "{text}"
        );
        assert!(
            text.contains("# TYPE process_cpu_seconds_total counter"),
            "{text}"
        );
        assert!(text.contains("# TYPE process_threads gauge"), "{text}");
        assert!(text.contains("vogt_voice_model_bytes 0\n"), "{text}");
    }

    #[test]
    fn model_bytes_sum_existing_files() {
        let dir = std::env::temp_dir().join(format!("vogt-voice-metrics-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let ggml = dir.join("model.bin");
        let onnx = dir.join("voice.onnx");
        let config = dir.join("voice.onnx.json");
        std::fs::write(&ggml, vec![1u8; 100]).unwrap();
        std::fs::write(&onnx, vec![2u8; 250]).unwrap();
        std::fs::write(&config, b"{}").unwrap();
        assert_eq!(model_bytes_loaded(Some(&ggml), Some(&config)), 350);
        assert_eq!(model_bytes_loaded(None, None), 0);
        assert_eq!(
            model_bytes_loaded(Some(dir.join("missing.bin").as_path()), None),
            0
        );
        let text = render(Some(&ggml), Some(&config));
        assert!(text.contains("vogt_voice_model_bytes 350\n"), "{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_metrics_addr_is_off_and_garbage_is_an_error() {
        // The process env is shared; these tests serialise on the var.
        std::env::remove_var("VOGT_VOICE_METRICS_ADDR");
        assert_eq!(metrics_addr_from_env().unwrap(), None);
        std::env::set_var("VOGT_VOICE_METRICS_ADDR", "");
        assert_eq!(metrics_addr_from_env().unwrap(), None);
        std::env::set_var("VOGT_VOICE_METRICS_ADDR", "not-an-addr");
        assert!(metrics_addr_from_env().is_err());
        std::env::set_var("VOGT_VOICE_METRICS_ADDR", "127.0.0.1:9");
        assert_eq!(
            metrics_addr_from_env().unwrap(),
            Some("127.0.0.1:9".parse().unwrap())
        );
        std::env::remove_var("VOGT_VOICE_METRICS_ADDR");
    }

    #[tokio::test]
    async fn listener_serves_expected_names() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        spawn_metrics_listener(addr, None, None);
        let url = format!("http://{addr}/metrics");
        let body = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(response) = reqwest_get(&url).await {
                    return response;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("metrics listener did not answer");
        assert!(body.contains("process_resident_memory_bytes"), "{body}");
        assert!(body.contains("process_cpu_seconds_total"), "{body}");
        assert!(body.contains("process_threads"), "{body}");
        assert!(body.contains("vogt_voice_model_bytes"), "{body}");
    }

    async fn reqwest_get(url: &str) -> Result<String, ()> {
        // Avoid a new dependency: speak HTTP/1.0 ourselves.
        let addr: SocketAddr = url
            .trim_start_matches("http://")
            .trim_end_matches("/metrics")
            .parse()
            .map_err(|_| ())?;
        let mut stream = tokio::net::TcpStream::connect(addr).await.map_err(|_| ())?;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        stream
            .write_all(b"GET /metrics HTTP/1.0\r\nHost: localhost\r\n\r\n")
            .await
            .map_err(|_| ())?;
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).await.map_err(|_| ())?;
        let text = String::from_utf8_lossy(&buf);
        if !text.contains("200") {
            return Err(());
        }
        Ok(text.into_owned())
    }
}
