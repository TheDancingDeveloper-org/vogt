//! `vogt-core` — the Rust replacement for the Python `vogt` process.
//!
//! This chunk serves health and applies the shared SQL migrations. Product
//! behaviour lands in later port chunks.

mod adapters;
mod application;
mod collectors;
#[allow(dead_code)]
mod config;
mod core;
mod decisions;
mod errors;
mod observability;
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
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run the core HTTP server.
    Serve {
        /// Bind address. Defaults to loopback.
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        /// Bind port.
        #[arg(long, default_value_t = 8080)]
        port: u16,
        /// Data directory holding the two SQLite files.
        #[arg(long)]
        data_dir: PathBuf,
    },
    /// Create or migrate the instance in a data directory.
    Init {
        /// Data directory. Created if it does not exist.
        #[arg(long)]
        data_dir: PathBuf,
        /// Report pending migrations and change nothing. Exits 0 when none.
        #[arg(long)]
        check: bool,
    },
}

fn main() -> ExitCode {
    match Cli::parse().command {
        Command::Serve {
            host,
            port,
            data_dir,
        } => serve(&host, port, data_dir),
        Command::Init { data_dir, check } => init(data_dir, check),
    }
}

fn init(data_dir: PathBuf, check: bool) -> ExitCode {
    if check {
        return match application::instance::pending(&data_dir) {
            Ok((declared, observed)) => {
                println!("pending declared={declared} observed={observed}");
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
    match application::instance::init(&data_dir, &now) {
        Ok(outcome) => {
            println!(
                "data_dir={} created={} declared={} observed={} applied={}",
                data_dir.display(),
                outcome.created,
                outcome.declared.version,
                outcome.observed.version,
                outcome.declared.applied.len() + outcome.observed.applied.len()
            );
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("vogt-core init: {err}");
            ExitCode::from(1)
        }
    }
}

fn serve(host: &str, port: u16, data_dir: PathBuf) -> ExitCode {
    let now = iso_now();
    if let Err(err) = application::instance::init(&data_dir, &now) {
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
        data_dir,
        version: env!("CARGO_PKG_VERSION").to_string(),
    });
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("vogt-core serve: {err}");
            return ExitCode::from(1);
        }
    };
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

async fn shutdown() {
    let _ = tokio::signal::ctrl_c().await;
}

fn iso_now() -> String {
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
