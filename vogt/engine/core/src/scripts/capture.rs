//! `scripts/capture_transcript.py` in Rust.
//!
//! Runs a command under a PTY and writes the raw bytes, so the transcript
//! fixtures carry real escape sequences rather than anything synthetic. The
//! result goes through `sanitise-transcript` before it is committed.

use std::io::Read;
use std::time::Duration;

use portable_pty::{native_pty_system, CommandBuilder, PtySize};

const DEFAULT_MAX_BYTES: usize = 2 * 1024 * 1024;
/// How long a read waits for the child to write before the capture ends.
/// `scripts/capture_transcript.py` gives `select` the same ten seconds.
const IDLE_TIMEOUT: Duration = Duration::from_secs(10);
/// A hard ceiling so a child that writes forever cannot run past it.
const TOTAL_BUDGET: Duration = Duration::from_secs(30);

pub fn run(args: &[String]) -> Result<(), String> {
    let (pre, cmd) = match args.iter().position(|arg| arg == "--") {
        Some(index) => (&args[..index], &args[index + 1..]),
        None => (args, &[][..]),
    };
    let mut output: Option<&str> = None;
    let mut max_bytes = DEFAULT_MAX_BYTES;
    let mut index = 0;
    while index < pre.len() {
        let arg = pre[index].as_str();
        if let Some(value) = arg.strip_prefix("--max-bytes=") {
            max_bytes = parse_max(value)?;
            index += 1;
            continue;
        }
        if arg == "--max-bytes" {
            let value = pre
                .get(index + 1)
                .ok_or_else(|| "--max-bytes needs a number".to_string())?;
            max_bytes = parse_max(value)?;
            index += 2;
            continue;
        }
        if arg.starts_with('-') {
            return Err(format!("unknown option {arg}"));
        }
        if output.is_some() {
            return Err(format!("unexpected argument {arg}"));
        }
        output = Some(arg);
        index += 1;
    }
    let output = output.ok_or_else(|| "an output path is required".to_string())?;
    if cmd.is_empty() {
        return Err("a command is required after --".to_string());
    }
    let data = capture(cmd, max_bytes)?;
    let length = data.len();
    std::fs::write(output, &data).map_err(|err| format!("{output}: {err}"))?;
    println!("capture_transcript: {length} bytes -> {output}");
    Ok(())
}

fn parse_max(value: &str) -> Result<usize, String> {
    value
        .parse()
        .map_err(|_| format!("--max-bytes is not a number: {value}"))
}

/// The window is 120 by 40, matching the Python script, and the child gets the
/// same `TERM` it would there. Reading stops at EOF, the byte cap, or when the
/// child has written nothing for [`IDLE_TIMEOUT`].
fn capture(cmd: &[String], max_bytes: usize) -> Result<Vec<u8>, String> {
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 40,
            cols: 120,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|err| format!("openpty: {err}"))?;
    let mut builder = CommandBuilder::new(&cmd[0]);
    for arg in &cmd[1..] {
        builder.arg(arg);
    }
    builder.env("TERM", "xterm-256color");
    builder.env("COLUMNS", "120");
    builder.env("LINES", "40");
    let mut child = pair
        .slave
        .spawn_command(builder)
        .map_err(|err| format!("spawn: {err}"))?;
    drop(pair.slave);
    let mut reader = pair
        .master
        .try_clone_reader()
        .map_err(|err| format!("reader: {err}"))?;
    let mut out = Vec::new();
    let started = std::time::Instant::now();
    loop {
        if out.len() >= max_bytes || started.elapsed() > TOTAL_BUDGET {
            break;
        }
        let read = read_within(&mut reader, IDLE_TIMEOUT).map_err(|err| err.to_string())?;
        if read.is_empty() {
            break;
        }
        out.extend_from_slice(&read);
    }
    let _ = child.kill();
    let _ = child.wait();
    out.truncate(max_bytes);
    Ok(out)
}

/// One read, or an empty buffer when nothing arrives within `wait`. An empty
/// result ends the capture: EOF and a quiet child look the same to the caller.
fn read_within(reader: &mut Box<dyn Read + Send>, wait: Duration) -> std::io::Result<Vec<u8>> {
    let (send, recv) = std::sync::mpsc::channel();
    let mut taken = std::mem::replace(reader, Box::new(std::io::empty()));
    let handle = std::thread::spawn(move || {
        let mut buffer = [0u8; 65536];
        let result = taken.read(&mut buffer).map(|read| buffer[..read].to_vec());
        let _ = send.send((result, taken));
    });
    match recv.recv_timeout(wait) {
        Ok((result, returned)) => {
            let _ = handle.join();
            *reader = returned;
            result
        }
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Ok(Vec::new()),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Ok(Vec::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_quiet_command_ends_without_hanging() {
        let started = std::time::Instant::now();
        let data = capture(&["sleep".to_string(), "30".to_string()], DEFAULT_MAX_BYTES).unwrap();
        assert!(data.is_empty());
        assert!(started.elapsed() < Duration::from_secs(20));
    }

    #[test]
    fn a_short_command_is_captured() {
        let data = capture(
            &["printf".to_string(), "hello\\n".to_string()],
            DEFAULT_MAX_BYTES,
        )
        .unwrap();
        assert!(data.windows(5).any(|window| window == b"hello"));
    }
}
