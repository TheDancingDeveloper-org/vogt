//! Print the launch line a pod would run for its configured agent CLI.
//!
//! `scripts/pod-codex-smoke.sh` uses this to generate the line from the same
//! `command_line` the session agent runs, then executes that line inside the
//! pod image. Copying the flags into the smoke script instead would have let
//! the two drift, which is the failure the smoke exists to catch (WI-3864).

use std::process::ExitCode;

use vogt_session_agent::{command_line, Config};

fn main() -> ExitCode {
    match Config::from_env().and_then(|config| command_line(&config)) {
        Ok(line) => {
            println!("{line}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("print_command_line: {error}");
            ExitCode::FAILURE
        }
    }
}
