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
#[allow(dead_code)]
mod input_delivery;
mod merge;
mod observability;
#[allow(dead_code)]
mod oversight;
#[allow(dead_code)] // Consumed by the HTTP, CLI and MCP adapters (P2.3–P2.5).
mod registry;
#[allow(dead_code)]
mod runtime;
mod storage;

/// The product version. Nothing sets `VOGT_VERSION` today, so this resolves to
/// the pinned fallback, which is what Python's `vogt.__version__` reports. It
/// deliberately does not read `VOGT_PRODUCT_VERSION`: the image build defaults
/// that to `local/dev` (see `engine/Dockerfile`), and the core must not announce
/// a version Python never would. `scripts/check_product_version.py` keeps the
/// fallback equal to `pyproject.toml`. Health and MCP both report this.
pub const VERSION: &str = match option_env!("VOGT_VERSION") {
    Some(value) if !value.is_empty() => value,
    _ => "0.7.9",
};

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
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
        /// Refuse every write. The same switch as `VOGT_READ_ONLY`.
        #[arg(long)]
        read_only: bool,
        /// Do not run the background sweep. There is no scheduler yet, so this
        /// changes nothing.
        #[arg(long)]
        no_schedule: bool,
        /// PEM file with the TLS certificate. Given with `--tls-key` or not at all.
        #[arg(long)]
        tls_cert: Option<PathBuf>,
        /// PEM file with the private key. Given with `--tls-cert` or not at all.
        #[arg(long)]
        tls_key: Option<PathBuf>,
        /// The inverse of `--read-only`, so a generated invocation that passes it
        /// boots. Writes are allowed unless `--read-only` says otherwise.
        #[arg(long, overrides_with = "read_only", hide = true)]
        no_read_only: bool,
    },
    /// Create or migrate the instance in a data directory.
    Init {
        /// Report pending migrations and change nothing. Exits 0 when none.
        #[arg(long)]
        check: bool,
    },
}

fn main() -> ExitCode {
    // The hash a missing username is verified against. Computed off the request
    // path, so the first unknown user costs one scrypt rather than two.
    std::thread::spawn(crate::application::services::auth::warm_dummy_hash);
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
    // `--help` on serve and init is the generated page, so it lists the schema
    // flags. clap accepts the same three: --read-only, --no-schedule and
    // --tls-cert. A deployment that passes them must boot. A unique prefix of
    // `--help` counts, because argparse treats `--he` and `--h` as `--help`.
    let help = argv.iter().any(|arg| is_help_flag(arg));
    let command = command_word(&argv);
    if matches!(command, Some("init" | "serve")) && global_after_command(&argv) {
        // `--json` is global on the root parser only. clap would accept it
        // after the subcommand; argparse reports it and does nothing.
        eprintln!(
            "usage: vogt [-h] [--version] [--data-dir DATA_DIR] [--json] <command> ...\nvogt: error: unrecognized arguments: --json"
        );
        return ExitCode::from(2);
    }
    if !matches!(command, Some("serve" | "init")) || help {
        if let Err(error) = validate_hooks(None) {
            eprintln!("{error}");
            return ExitCode::from(1);
        }
        let data_dir = adapters::cli::data_dir_of(&argv);
        let code = adapters::cli::main_cli(
            &argv,
            &registry::default_registry(),
            VERSION,
            &mut |operation, params| match operation.handler {
                // The manifest takes no parameters and reads no store.
                registry::Handler::RegistryDump => {
                    let _ = (&params, &data_dir);
                    operation.run(None, params)
                }
                registry::Handler::NotPorted => {
                    // The same resolution `init` uses: the config file, then
                    // VOGT_DATA_DIR, then `--data-dir` on top. A pod sets only
                    // VOGT_DATA_DIR, so a default here would read the wrong
                    // instance.
                    let dir = resolve_data_dir(data_dir.as_deref().map(PathBuf::from))
                        .map_err(crate::errors::VogtError::InvalidRequest)?;
                    let mut overrides = serde_json::Map::new();
                    overrides.insert(
                        "data_dir".to_string(),
                        serde_json::Value::String(dir.display().to_string()),
                    );
                    let config = config::load_config(&overrides)
                        .map_err(crate::errors::VogtError::InvalidRequest)?;
                    let built = application::context::build_context(
                        config, None, None, None, None, None, None, None,
                    )?;
                    operation.run(Some(&built), params)
                }
            },
            &mut std::io::stdout(),
            &mut std::io::stderr(),
        );
        std::process::exit(code);
    }
    let cli = Cli::parse_from(
        std::iter::once(std::ffi::OsString::from("vogt-core")).chain(
            expand_global_prefixes(&argv)
                .into_iter()
                .map(std::ffi::OsString::from),
        ),
    );
    if let Err(error) = validate_hooks(None) {
        eprintln!("{error}");
        return ExitCode::from(1);
    }
    match cli.command {
        Command::Serve {
            host,
            port,
            no_auth,
            read_only,
            no_schedule: _,
            tls_cert,
            tls_key,
            no_read_only: _,
        } => serve(ServeArgs {
            host: &host,
            port,
            data_dir: cli.data_dir,
            json: cli.json,
            no_auth,
            read_only,
            tls_cert: tls_cert.as_deref(),
            tls_key: tls_key.as_deref(),
        }),
        Command::Init { check } => init(cli.data_dir, check, cli.json),
    }
}

/// The command word, skipping global flags that may precede it.
///
/// `--data-dir DIR init` is `init`, and so is `--data DIR init`: argparse
/// resolves a unique prefix before it looks for the command, and this routing
/// has to agree or `vogt --data X init` falls through to the generated CLI and
/// never creates `X`. A flag this binary does not know is left for whichever
/// parser owns the command, so this only steps over the globals both parsers share.
fn command_word(argv: &[String]) -> Option<&str> {
    let mut index = 0usize;
    while index < argv.len() {
        let arg = argv[index].as_str();
        if arg == "--json" || is_global_prefix(arg, "json") {
            index += 1;
            continue;
        }
        if arg == "--data-dir" || is_global_prefix(arg, "data-dir") {
            // `--data=DIR` carries its value; `--data DIR` takes the next token.
            index += if arg.contains('=') { 1 } else { 2 };
            continue;
        }
        if arg == "--version" || is_global_prefix(arg, "version") || is_help_flag(arg) {
            index += 1;
            continue;
        }
        return Some(arg);
    }
    None
}

/// Whether `arg` is a unique prefix of one root option and not of another.
/// `--data` is `data-dir`; `--d` is too, because nothing else starts with `d`.
fn is_global_prefix(arg: &str, option: &str) -> bool {
    let name = arg.split_once('=').map(|(head, _)| head).unwrap_or(arg);
    let Some(name) = name.strip_prefix("--") else {
        return false;
    };
    if name.is_empty() || !option.starts_with(name) {
        return false;
    }
    const GLOBALS: [&str; 4] = ["help", "version", "data-dir", "json"];
    GLOBALS
        .iter()
        .filter(|candidate| candidate.starts_with(name))
        .count()
        == 1
}

/// `--help` and `-h`, or a prefix of `--help` that is not also a prefix of
/// another global. `--he` is help; `--h` is not, because it is also `--host`
/// on `serve`, and stealing that flag would stop a deployment booting.
fn is_help_flag(arg: &str) -> bool {
    arg == "--help" || arg == "-h" || is_global_prefix(arg, "help")
}

/// Whether a root-only global sits after the command word. `--json` is the one
/// clap would otherwise swallow, because it is declared `global = true`.
fn global_after_command(argv: &[String]) -> bool {
    let mut seen_command = false;
    for arg in argv {
        if !seen_command {
            if !arg.starts_with('-') {
                seen_command = true;
            }
            continue;
        }
        if arg == "--json" || arg.starts_with("--json=") {
            return true;
        }
    }
    false
}

/// Rewrite a unique prefix of a global option to the spelling clap knows.
///
/// clap does not abbreviate, so `vogt --data DIR init` reached this parser and
/// exited 2. `--data` and `--data-d` become `--data-dir`; `--js` becomes
/// `--json`. An attached value stays attached (`--data=DIR`). A prefix that is
/// not unique, or that belongs to `serve` (`--h` is also `--host`), is left
/// alone.
fn expand_global_prefixes(argv: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(argv.len());
    let mut index = 0usize;
    // Only the tokens before the command word are globals. `serve --host` must
    // not be rewritten because `--host` starts with `h`.
    let mut before_command = true;
    while index < argv.len() {
        let arg = &argv[index];
        if !before_command {
            out.push(arg.clone());
            index += 1;
            continue;
        }
        if let Some(option) = canonical_global(arg) {
            let value = arg.split_once('=').map(|(_, value)| value);
            match option {
                "data-dir" => {
                    out.push("--data-dir".to_string());
                    if let Some(value) = value {
                        out.push(value.to_string());
                    }
                }
                "json" => out.push("--json".to_string()),
                other => out.push(format!("--{other}")),
            }
            index += 1;
            continue;
        }
        if !arg.starts_with('-') {
            before_command = false;
        }
        out.push(arg.clone());
        index += 1;
    }
    out
}

/// The canonical global name when `arg` is an exact match or a unique prefix.
fn canonical_global(arg: &str) -> Option<&'static str> {
    const GLOBALS: [&str; 4] = ["help", "version", "data-dir", "json"];
    let name = arg.split_once('=').map(|(head, _)| head).unwrap_or(arg);
    let name = name.strip_prefix("--")?;
    if GLOBALS.contains(&name) {
        return GLOBALS.iter().copied().find(|option| *option == name);
    }
    let matches: Vec<&&str> = GLOBALS
        .iter()
        .filter(|option| option.starts_with(name) && !name.is_empty())
        .collect();
    match matches.as_slice() {
        [one] => Some(one),
        _ => None,
    }
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
    let data_dir = match resolve_data_dir(data_dir) {
        Ok(dir) => dir,
        Err(err) => {
            eprintln!("vogt-core: {err}");
            return ExitCode::from(1);
        }
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

struct ServeArgs<'a> {
    host: &'a str,
    port: u16,
    data_dir: Option<PathBuf>,
    json: bool,
    no_auth: bool,
    read_only: bool,
    tls_cert: Option<&'a Path>,
    tls_key: Option<&'a Path>,
}

fn serve(args: ServeArgs<'_>) -> ExitCode {
    let ServeArgs {
        host,
        port,
        data_dir,
        json,
        no_auth,
        read_only,
        tls_cert,
        tls_key,
    } = args;
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
    let data_dir = match resolve_data_dir(data_dir) {
        Ok(dir) => dir,
        Err(err) => {
            eprintln!("vogt-core: {err}");
            return ExitCode::from(1);
        }
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
    let built = application::context::build_context(
        config.clone(),
        None,
        clock,
        ids,
        None,
        None,
        None,
        None,
    )
    .expect("the hooks were validated at startup");
    // `--read-only` and `VOGT_READ_ONLY` are the same switch. Python reads the
    // flag from the invocation and the variable from the environment.
    let writes_enabled = !read_only && std::env::var("VOGT_READ_ONLY").ok().is_none();
    let health = adapters::http::openapi::router().merge(adapters::http::health::router(
        adapters::http::health::HealthState {
            data_dir: data_dir.clone(),
            version: VERSION.to_string(),
            auth_enabled: !no_auth,
            writes_enabled,
        },
    ));
    // Both routes join the context's store, so their recorded rows use the same
    // clock and id sequence as the rest of the process, hooks included.
    let router = match &built {
        application::context::Built::SystemRandom(ctx) => health
            .merge(adapters::http::app::router_system_random(
                adapters::http::app::AppState::joined(
                    &data_dir,
                    no_auth,
                    writes_enabled,
                    &ctx.declared,
                ),
            ))
            .merge(adapters::http::mcp::router_system_random(
                adapters::http::mcp::McpState::joined(
                    &data_dir,
                    no_auth,
                    writes_enabled,
                    &ctx.declared,
                ),
            )),
        application::context::Built::SystemSequential(ctx) => health
            .merge(adapters::http::app::router_system_sequential(
                adapters::http::app::AppState::joined(
                    &data_dir,
                    no_auth,
                    writes_enabled,
                    &ctx.declared,
                ),
            ))
            .merge(adapters::http::mcp::router_system_sequential(
                adapters::http::mcp::McpState::joined(
                    &data_dir,
                    no_auth,
                    writes_enabled,
                    &ctx.declared,
                ),
            )),
        application::context::Built::StepRandom(ctx) => health
            .merge(adapters::http::app::router_step_random(
                adapters::http::app::AppState::joined(
                    &data_dir,
                    no_auth,
                    writes_enabled,
                    &ctx.declared,
                ),
            ))
            .merge(adapters::http::mcp::router_step_random(
                adapters::http::mcp::McpState::joined(
                    &data_dir,
                    no_auth,
                    writes_enabled,
                    &ctx.declared,
                ),
            )),
        application::context::Built::StepSequential(ctx) => health
            .merge(adapters::http::app::router_step_sequential(
                adapters::http::app::AppState::joined(
                    &data_dir,
                    no_auth,
                    writes_enabled,
                    &ctx.declared,
                ),
            ))
            .merge(adapters::http::mcp::router_step_sequential(
                adapters::http::mcp::McpState::joined(
                    &data_dir,
                    no_auth,
                    writes_enabled,
                    &ctx.declared,
                ),
            )),
    };
    // `--tls-cert` and `--tls-key` arrive together or not at all. One without the
    // other would either serve plain HTTP while the operator believes TLS is on,
    // or refuse a certificate Python accepts. A path that is not a file is named.
    if let Err(error) = check_tls(tls_cert, tls_key) {
        eprintln!("error: invalid_request: {error}");
        return ExitCode::from(1);
    }
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("vogt-core serve: {err}");
            return ExitCode::from(1);
        }
    };
    let scheme = if tls_cert.is_some() { "https" } else { "http" };
    if json {
        println!(
            "{{\"url\":\"{scheme}://{host}:{port}\",\"data_dir\":\"{}\"}}",
            data_dir.display()
        );
    }
    let result = runtime.block_on(async move {
        match tls_cert {
            Some(pem) => {
                let tls = load_tls(pem, tls_key)?;
                let handle = axum_server::Handle::new();
                let signal = handle.clone();
                tokio::spawn(async move {
                    shutdown().await;
                    signal.graceful_shutdown(None);
                });
                axum_server::bind_rustls(addr, tls)
                    .handle(handle)
                    .serve(router.into_make_service())
                    .await
            }
            None => {
                let listener = tokio::net::TcpListener::bind(addr).await?;
                axum::serve(listener, router)
                    .with_graceful_shutdown(shutdown())
                    .await
            }
        }
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
fn resolve_data_dir(given: Option<PathBuf>) -> Result<PathBuf, String> {
    let mut overrides = serde_json::Map::new();
    if let Some(dir) = &given {
        overrides.insert(
            "data_dir".to_string(),
            serde_json::Value::String(dir.display().to_string()),
        );
    }
    config::load_config(&overrides).map(|config| config.resolved_data_dir().to_path_buf())
}

async fn shutdown() {
    let _ = tokio::signal::ctrl_c().await;
}

/// `--tls-cert` and `--tls-key` are a pair. Python refuses one without the
/// other, and names a path that is not a file, both as `invalid_request`.
fn check_tls(cert: Option<&Path>, key: Option<&Path>) -> Result<(), String> {
    if cert.is_some() != key.is_some() {
        return Err("--tls-cert and --tls-key are given together or not at all".to_string());
    }
    for (label, path) in [("--tls-cert", cert), ("--tls-key", key)] {
        if let Some(path) = path {
            if !path.is_file() {
                return Err(format!("{label}: no such file: {}", path.display()));
            }
        }
    }
    Ok(())
}

/// The TLS config for `--tls-cert`. Python loads the PEM with stdlib `ssl` and
/// reports the failure as `invalid_request`; a file that will not parse, or one
/// with no private key, is the same error here.
fn load_tls(
    cert: &Path,
    key: Option<&Path>,
) -> std::io::Result<axum_server::tls_rustls::RustlsConfig> {
    let cert_bytes = std::fs::read(cert)?;
    let key_bytes = match key {
        Some(path) => std::fs::read(path)?,
        None => cert_bytes.clone(),
    };
    let mut reader = std::io::BufReader::new(cert_bytes.as_slice());
    let certs = rustls_pemfile::certs(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(std::io::Error::other)?;
    let mut reader = std::io::BufReader::new(key_bytes.as_slice());
    let key = rustls_pemfile::private_key(&mut reader)
        .map_err(std::io::Error::other)?
        .ok_or_else(|| std::io::Error::other("the PEM holds no private key"))?;
    Ok(axum_server::tls_rustls::RustlsConfig::from_config(
        std::sync::Arc::new(rustls_server_config(certs, key)?),
    ))
}

fn rustls_server_config(
    certs: Vec<rustls::pki_types::CertificateDer<'static>>,
    key: rustls::pki_types::PrivateKeyDer<'static>,
) -> Result<rustls::ServerConfig, std::io::Error> {
    // Two crates pull rustls in with different cryptography: ureq with ring,
    // axum-server with aws-lc-rs. Neither is the default, so `builder()` panics
    // with exit 101 on any valid certificate. ring is named explicitly. aws-lc-rs
    // needs a C toolchain to build, and nothing here asks for it.
    let provider = std::sync::Arc::new(rustls::crypto::ring::default_provider());
    rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|err| std::io::Error::other(err.to_string()))?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|err| std::io::Error::other(err.to_string()))
}

#[cfg(test)]
mod tls_tests {
    use super::check_tls;
    use std::path::Path;

    #[test]
    fn one_flag_without_the_other_is_refused() {
        let text = check_tls(Some(Path::new("c.pem")), None).unwrap_err();
        assert_eq!(
            text,
            "--tls-cert and --tls-key are given together or not at all"
        );
        assert!(check_tls(None, Some(Path::new("k.pem"))).is_err());
    }

    #[test]
    fn a_missing_file_is_named() {
        let text = check_tls(
            Some(Path::new("no-such-cert.pem")),
            Some(Path::new("no-such-key.pem")),
        )
        .unwrap_err();
        assert_eq!(text, "--tls-cert: no such file: no-such-cert.pem");
    }

    #[test]
    fn neither_flag_is_fine() {
        assert!(check_tls(None, None).is_ok());
    }
}

#[cfg(test)]
mod data_dir_tests {
    use super::resolve_data_dir;

    /// The config tests read and write the same variables, and cargo runs the
    /// suite in parallel, so a test that touches the environment takes the lock
    /// those tests take. Without it these two fail intermittently.
    fn with_data_dir(value: Option<&std::path::Path>, body: impl FnOnce()) {
        let _guard = crate::config::env_lock();
        let saved = std::env::var("VOGT_DATA_DIR").ok();
        match value {
            Some(dir) => unsafe { std::env::set_var("VOGT_DATA_DIR", dir) },
            None => unsafe { std::env::remove_var("VOGT_DATA_DIR") },
        }
        body();
        match saved {
            Some(previous) => unsafe { std::env::set_var("VOGT_DATA_DIR", previous) },
            None => unsafe { std::env::remove_var("VOGT_DATA_DIR") },
        }
    }

    /// `VOGT_DATA_DIR` alone names the instance. A pod sets nothing else, so a
    /// status that fell back to the XDG default would read the wrong one.
    #[test]
    fn the_env_var_names_the_data_dir_when_no_flag_is_given() {
        let dir = std::env::temp_dir().join(format!("vogt-datadir-{}", std::process::id()));
        with_data_dir(Some(&dir), || {
            let resolved = resolve_data_dir(None).unwrap();
            assert_eq!(resolved.as_path(), dir.as_path());
        });
    }

    #[test]
    fn the_flag_overrides_the_env_var() {
        let from_env = std::env::temp_dir().join("vogt-from-env");
        let from_flag = std::env::temp_dir().join("vogt-from-flag");
        with_data_dir(Some(&from_env), || {
            let resolved = resolve_data_dir(Some(from_flag.clone())).unwrap();
            assert_eq!(resolved.as_path(), from_flag.as_path());
        });
    }
}
