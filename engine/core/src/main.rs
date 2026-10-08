//! `vogt-core` — the Rust replacement for the Python `vogt serve` process.
//!
//! This binary is a skeleton. Product behaviour lands in later port chunks;
//! the contract it must meet is the parity harness, not this file.

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
    /// Run the core HTTP server (not implemented yet).
    Serve,
}

fn main() -> ExitCode {
    match Cli::parse().command {
        Command::Serve => serve(),
    }
}

/// The server is not part of this chunk. Returning rather than exiting lets
/// the binary's exit status stay the one contract this skeleton has.
fn serve() -> ExitCode {
    eprintln!("vogt-core serve is not implemented yet");
    ExitCode::from(2)
}
