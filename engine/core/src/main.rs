//! `vogt-core` — the Rust replacement for the Python `vogt` process.
//!
//! This chunk serves health and applies the shared SQL migrations. Product
//! behaviour lands in later port chunks.

mod actors;
mod adapters;
mod application;
mod auth;
mod branches;
mod collectors;
#[allow(dead_code)]
mod config;
mod core;
mod decisions;
mod delivery;
mod errors;
mod git_story;
mod merge;
mod observability;
#[allow(dead_code)] // Consumed by the HTTP, CLI and MCP adapters (P2.3–P2.5).
mod registry;
mod storage;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "vogt-core",
    version,
    about = "Vogt core — drop-in replacement for the Python vogt process"
)]
struct Cli {
    /// Emit results as JSON. Global, as on the Python CLI, so it applies to
    /// every command rather than being restated on each.
    #[arg(long, global = true)]
    json: bool,
    /// Data directory. Falls back to `VOGT_DATA_DIR`, then `$XDG_DATA_HOME/vogt`,
    /// then `~/.local/share/vogt`. Global so the argv matches Python's
    /// `vogt --data-dir … <command>`. Clap also accepts the flag after the
    /// subcommand, which Python rejects with exit 2; that leniency is
    /// deliberate, since the documented position is the one the harness uses.
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run the core HTTP server.
    Serve {
        /// Listen address. No default: it encodes exposure, and the deployment
        /// supplies it.
        #[arg(long)]
        host: String,
        /// Listen port. No default, for the same reason as host.
        #[arg(long)]
        port: u16,
        /// Do not require a bearer token. The default is to require one, which
        /// is what `/connection-info` reports as "bearer token".
        #[arg(long)]
        no_auth: bool,
    },
    /// Create or migrate the instance in a data directory.
    Init {
        /// Report pending migrations and change nothing. Exits 0 when none.
        #[arg(long)]
        check: bool,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    announce_hooks();
    match cli.command {
        Command::Serve {
            host,
            port,
            no_auth,
        } => serve(&host, port, cli.data_dir, cli.json, no_auth),
        Command::Init { check } => init(cli.data_dir, check, cli.json),
    }
}

/// Python's one startup warning: name the deterministic hooks that are set,
/// once, on stderr. A golden run sets both, so the line is part of what the
/// two binaries must agree on.
fn announce_hooks() {
    let mut active = Vec::new();
    for name in [core::CLOCK_ENV, core::IDS_ENV] {
        if std::env::var(name)
            .ok()
            .is_some_and(|value| !value.trim().is_empty())
        {
            active.push(name);
        }
    }
    if !active.is_empty() {
        eprintln!("deterministic test hooks are active: {}", active.join(", "));
    }
}

fn init(data_dir: Option<PathBuf>, check: bool, json: bool) -> ExitCode {
    let Some(data_dir) = resolve_data_dir(data_dir) else {
        return ExitCode::from(1);
    };
    if check {
        return match application::instance::pending(&data_dir) {
            Ok((declared, observed)) => {
                if json {
                    println!("{{\"pending\":{{\"declared\":{declared},\"observed\":{observed}}}}}");
                } else {
                    println!("pending declared={declared} observed={observed}");
                }
                if declared == 0 && observed == 0 {
                    ExitCode::SUCCESS
                } else {
                    ExitCode::from(1)
                }
            }
            Err(err) => {
                eprintln!("vogt-core init --check: {err}");
                ExitCode::from(1)
            }
        };
    }
    let now = iso_now();
    match application::instance::init(&data_dir, &now, &clock_after(&now, 1)) {
        Ok(outcome) => {
            let applied = outcome
                .declared
                .applied
                .iter()
                .map(|id| format!("declared:{id}"))
                .chain(
                    outcome
                        .observed
                        .applied
                        .iter()
                        .map(|id| format!("observed:{id}")),
                )
                .collect::<Vec<_>>();
            let body = serde_json::Value::Object(
                [
                    ("instance_id", outcome.instance_id.into()),
                    ("data_dir", data_dir.display().to_string().into()),
                    ("created", outcome.created.into()),
                    ("declared_schema_version", outcome.declared.version.into()),
                    ("observed_schema_version", outcome.observed.version.into()),
                    ("migrations_applied", applied.into()),
                    // Rust does not mint or adopt bootstrap tokens, so both stay
                    // at the default Python reports when neither file is set.
                    ("bootstrap_core_token", "not_configured".into()),
                    ("bootstrap_agent_token", "not_configured".into()),
                ]
                .into_iter()
                .map(|(key, value)| (key.to_string(), value))
                .collect(),
            );
            if json {
                println!("{}", serde_json::to_string_pretty(&body).expect("json"));
            } else {
                println!("{}", render_text(&body));
            }
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("vogt-core init: {err}");
            ExitCode::from(1)
        }
    }
}

/// Python's `cli/render.py`: `key: value`, booleans as `yes`/`no`, an empty
/// list as `(none)` and each list entry on its own indented line.
fn render_text(body: &serde_json::Value) -> String {
    fn scalar(value: &serde_json::Value) -> String {
        match value {
            serde_json::Value::Bool(true) => "yes".to_string(),
            serde_json::Value::Bool(false) => "no".to_string(),
            serde_json::Value::Null => "-".to_string(),
            serde_json::Value::String(text) => text.clone(),
            other => other.to_string(),
        }
    }
    let mut lines = Vec::new();
    let object = body.as_object().expect("init result is an object");
    for (key, value) in object {
        match value {
            serde_json::Value::Array(items) if items.is_empty() => {
                lines.push(format!("{key}: (none)"));
            }
            serde_json::Value::Array(items) => {
                lines.push(format!("{key}:"));
                for item in items {
                    lines.push(format!("  - {}", scalar(item)));
                }
            }
            other => lines.push(format!("{key}: {}", scalar(other))),
        }
    }
    lines.join("\n")
}

fn serve(host: &str, port: u16, data_dir: Option<PathBuf>, json: bool, no_auth: bool) -> ExitCode {
    let Some(data_dir) = resolve_data_dir(data_dir) else {
        return ExitCode::from(1);
    };
    let now = iso_now();
    if let Err(err) = application::instance::init(&data_dir, &now, &clock_after(&now, 1)) {
        eprintln!("vogt-core serve: {err}");
        return ExitCode::from(1);
    }
    let addr: SocketAddr = match format!("{host}:{port}").parse() {
        Ok(addr) => addr,
        Err(err) => {
            eprintln!("vogt-core serve: {err}");
            return ExitCode::from(1);
        }
    };
    let router = adapters::http::health::router(adapters::http::health::HealthState {
        data_dir: data_dir.clone(),
        version: PRODUCT_VERSION.to_string(),
        auth_enabled: !no_auth,
        writes_enabled: true,
    });
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("vogt-core serve: {err}");
            return ExitCode::from(1);
        }
    };
    if json {
        println!(
            "{{\"url\":\"http://{host}:{port}\",\"data_dir\":\"{}\"}}",
            data_dir.display()
        );
    }
    let result = runtime.block_on(async move {
        let listener = tokio::net::TcpListener::bind(addr).await?;
        axum::serve(listener, router)
            .with_graceful_shutdown(shutdown())
            .await
    });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("vogt-core serve: {err}");
            ExitCode::from(1)
        }
    }
}

const PRODUCT_VERSION: &str = match option_env!("VOGT_PRODUCT_VERSION") {
    Some(value) if !value.is_empty() => value,
    _ => "local/dev",
};

/// Where the instance lives. An explicit `--data-dir` wins, then `VOGT_DATA_DIR`,
/// then the TOML file named by `VOGT_CONFIG_FILE`, then the XDG default. That is
/// the order `load_config` applies, so a stack that sets `data_dir` in its config
/// file lands in the same place as Python.
fn resolve_data_dir(given: Option<PathBuf>) -> Option<PathBuf> {
    let mut overrides = serde_json::Map::new();
    if let Some(dir) = &given {
        overrides.insert(
            "data_dir".to_string(),
            serde_json::Value::String(dir.display().to_string()),
        );
    }
    match config::load_config(&overrides) {
        Ok(config) => Some(config.resolved_data_dir().to_path_buf()),
        Err(err) => {
            eprintln!("vogt-core: {err}");
            None
        }
    }
}

async fn shutdown() {
    let _ = tokio::signal::ctrl_c().await;
}

fn clock_after(start: &str, steps: i64) -> String {
    core::from_iso(start)
        .map(|moment| core::to_iso(core::Moment::from_unix(moment.unix_seconds() + steps, 0)))
        .unwrap_or_else(|_| start.to_string())
}

fn iso_now() -> String {
    // The step clock, when the harness asks for one. Python reads it for the
    // migration timestamp, so a recorded init and a live one agree on applied_at.
    if let Ok(start) = std::env::var("VOGT_TEST_CLOCK_START") {
        if let Ok(moment) = core::from_iso(&start) {
            return core::to_iso(moment);
        }
    }
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    format_unix(secs)
}

fn format_unix(secs: u64) -> String {
    let days = secs / 86_400;
    let time = secs % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}+00:00",
        time / 3600,
        (time % 3600) / 60,
        time % 60
    )
}

fn civil_from_days(days: u64) -> (i64, u32, u32) {
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };
    (year, month as u32, day as u32)
}
