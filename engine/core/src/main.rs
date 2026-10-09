//! `vogt-core` — the Rust replacement for the Python `vogt` process.
//!
//! This chunk serves health and applies the shared SQL migrations. Product
//! behaviour lands in later port chunks.

mod actors;
#[allow(dead_code)]
mod adapters;
#[allow(dead_code)] // Consumed by the service layer (P1.10 onward).
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

/// The product version. Nothing sets `VOGT_VERSION` today, so this resolves to
/// the pinned fallback, which is what Python's `vogt.__version__` reports. It
/// deliberately does not read `VOGT_PRODUCT_VERSION`: the image build defaults
/// that to `local/dev` (see `engine/Dockerfile`), and the core must not announce
/// a version Python never would. `scripts/check_product_version.py` keeps the
/// fallback equal to `pyproject.toml`. Health and MCP both report this.
pub const VERSION: &str = match option_env!("VOGT_VERSION") {
    Some(value) if !value.is_empty() => value,
    _ => "0.7.8",
};

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
    // `vogt-mcp-remote`: stdio in, streamable HTTP out. A missing URL is exit 2,
    // because an agent that spawns the bridge with nothing configured should
    // see a setup error rather than a hang.
    if std::env::args()
        .next()
        .is_some_and(|arg| arg.ends_with("vogt-mcp-remote"))
    {
        let env: std::collections::HashMap<String, String> = std::env::vars().collect();
        let Some(url) = adapters::mcp::bridge::configured_url(&env) else {
            eprintln!(
                "vogt-mcp-remote: set VOGT_URL to the server's base URL (and VOGT_TOKEN_FILE to a file holding a token)"
            );
            return ExitCode::from(2);
        };
        let token = adapters::mcp::bridge::resolve_token(&env);
        let transport = adapters::mcp::bridge::UreqTransport;
        let mut bridge = adapters::mcp::bridge::Bridge::new(&url, token, &transport, VERSION);
        // A line at a time. A client sends `initialize` and waits for the
        // answer, so reading all of stdin first hangs the handshake.
        // Bytes, not lines of text. A line that is not UTF-8 is still a line
        // the client is waiting on, and Python answers it with -32700 rather
        // than skipping it. Lossy decoding keeps the valid bytes so the error
        // names where the line broke.
        let mut stdin = std::io::BufReader::new(std::io::stdin());
        let mut warned = 0;
        loop {
            let mut bytes = Vec::new();
            if std::io::BufRead::read_until(&mut stdin, b'\n', &mut bytes).is_err()
                || bytes.is_empty()
            {
                break;
            }
            let line = String::from_utf8_lossy(&bytes);
            let mut output = String::new();
            bridge.serve_line(&line, &mut output);
            if !output.is_empty() {
                use std::io::Write;
                let mut stdout = std::io::stdout().lock();
                // A client that closed its pipe is a stop, not a panic.
                if stdout.write_all(output.as_bytes()).is_err() {
                    return ExitCode::SUCCESS;
                }
            }
            for warning in bridge.report.warned.iter().skip(warned) {
                eprintln!("vogt-mcp-remote: {warning}");
            }
            warned = bridge.report.warned.len();
        }
        return ExitCode::SUCCESS;
    }
    let raw: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    let mut argv = Vec::with_capacity(raw.len());
    for arg in &raw {
        match arg.to_str() {
            Some(text) => argv.push(text.to_string()),
            None => {
                eprintln!("error: arguments must be valid UTF-8");
                return ExitCode::from(2);
            }
        }
    }
    // `init` and `serve` already have a Rust implementation in this binary.
    // The generated CLI must not steal them: a global flag before the command
    // (`--data-dir DIR init`) is still that command, and replacing it with the
    // not-ported stub breaks instance bootstrap. Every other registry command
    // goes through the generated adapter.
    if !matches!(command_word(&argv), Some("serve" | "init")) {
        if let Err(error) = validate_hooks(None) {
            eprintln!("{error}");
            return ExitCode::from(1);
        }
        let code = adapters::cli::main_cli(
            &argv,
            &registry::default_registry(),
            VERSION,
            &mut |operation, _params| {
                operation.run()?;
                if operation.handler == registry::Handler::RegistryDump {
                    return Ok(registry::dump());
                }
                Ok(serde_json::Value::Null)
            },
            &mut std::io::stdout(),
            &mut std::io::stderr(),
        );
        std::process::exit(code);
    }
    let cli = Cli::parse();
    if let Err(error) = validate_hooks(None) {
        eprintln!("{error}");
        return ExitCode::from(1);
    }
    match cli.command {
        Command::Serve {
            host,
            port,
            no_auth,
        } => serve(&host, port, cli.data_dir, cli.json, no_auth),
        Command::Init { check } => init(cli.data_dir, check, cli.json),
    }
}

/// The command word, skipping global flags that may precede it.
///
/// `--data-dir DIR init` is `init`. A flag this binary does not know is left
/// for whichever parser owns the command, so this only steps over the two
/// globals both parsers share.
fn command_word(argv: &[String]) -> Option<&str> {
    let mut index = 0usize;
    while index < argv.len() {
        match argv[index].as_str() {
            "--json" => index += 1,
            "--data-dir" => index += 2,
            _ => return Some(argv[index].as_str()),
        }
    }
    None
}

/// Validate the hooks and name the ones that are set, once. A value that is not
/// a timestamp, or an id mode other than `sequential`, exits 1 the way Python's
/// `InvalidRequest` does. `host` is checked when serving, so a deterministic
/// clock cannot run on a non-loopback bind. The warning is not repeated when
/// `serve` validates again, because Python announces it once per process.
fn validate_hooks(host: Option<&str>) -> Result<(), crate::errors::VogtError> {
    let clock = std::env::var(core::CLOCK_ENV).ok();
    let ids = std::env::var(core::IDS_ENV).ok();
    core::clock_from_env(clock.as_deref())?;
    core::ids_from_env(ids.as_deref(), None)?;
    if let Some(host) = host {
        core::refuse_hooks_off_loopback(host, clock.as_deref(), ids.as_deref())?;
    }
    let active = core::hooks_active(clock.as_deref(), ids.as_deref());
    if !active.is_empty() && host.is_none() {
        eprintln!("deterministic test hooks are active: {}", active.join(", "));
    }
    Ok(())
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
    // validate_hooks already refused a bad value, so these are the hook or none.
    let mut clock = core::clock_from_env(std::env::var(core::CLOCK_ENV).ok().as_deref())
        .expect("a bad clock value was refused at startup");
    let mut ids = core::ids_from_env(
        std::env::var(core::IDS_ENV).ok().as_deref(),
        Some(data_dir.join("test-ids.json")),
    )
    .expect("a bad id mode was refused at startup");
    match application::instance::init(&data_dir, &mut clock, &mut ids) {
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
    // Logging is a server concern. Python configures it in `serve` from the
    // resolved config and leaves the CLI and the stdio transport quiet, so a
    // JSON-log deployment and a `debug` level only apply here. `load_config`
    // applies the file, the environment and validation, so an invalid level
    // refuses to start rather than being silently accepted.
    let config = match config::load_config(&serde_json::Map::new()) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("vogt-core serve: {error}");
            return ExitCode::from(1);
        }
    };
    observability::configure_logging(config.log_level.as_str(), config.log_format.as_str());
    if let Err(error) = validate_hooks(Some(host)) {
        eprintln!("error: invalid_request: {error}");
        return ExitCode::from(1);
    }
    let Some(data_dir) = resolve_data_dir(data_dir) else {
        return ExitCode::from(1);
    };
    // validate_hooks already refused a bad value, so these are the hook or none.
    let mut clock = core::clock_from_env(std::env::var(core::CLOCK_ENV).ok().as_deref())
        .expect("a bad clock value was refused at startup");
    let mut ids = core::ids_from_env(
        std::env::var(core::IDS_ENV).ok().as_deref(),
        Some(data_dir.join("test-ids.json")),
    )
    .expect("a bad id mode was refused at startup");
    if let Err(err) = application::instance::init(&data_dir, &mut clock, &mut ids) {
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
    let health = adapters::http::health::router(adapters::http::health::HealthState {
        data_dir: data_dir.clone(),
        version: VERSION.to_string(),
        auth_enabled: !no_auth,
        writes_enabled: true,
    });
    // `/mcp` answers on the same port, behind the same gate as `/api`. The gate
    // authenticates every request and records the decision before a tool runs,
    // so an anonymous caller is refused and the refusal is a row.
    let router = health
        .merge(adapters::http::app::router(
            adapters::http::app::AppState::new(&data_dir, no_auth, true, core::FreshIds),
        ))
        .merge(adapters::http::mcp::router(
            adapters::http::mcp::McpState::new(&data_dir, no_auth, true, core::FreshIds),
        ));
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
