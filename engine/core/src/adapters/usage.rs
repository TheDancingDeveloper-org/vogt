//! Where the usage board's evidence lives (WI-2040 / WI-2144).
//!
//! The reading is [`crate::core::usage_board`]; this module only fetches
//! text. Reads are bounded: the newest few transcripts an account wrote in
//! the last week, the tail of each.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;

/// How many of an account's newest transcripts are read, and how much of each.
pub const MAX_FILES: usize = 20;
pub const TAIL_BYTES: u64 = 512 * 1024;
/// Transcripts older than this say nothing about a limit that still binds.
pub const MAX_AGE_SECONDS: u64 = 7 * 24 * 3600;
const KEY_INFO_TIMEOUT: Duration = Duration::from_secs(10);

/// `(url, key) -> (status, body)`. Injectable so tests make no request.
pub trait KeyInfoFetch {
    fn fetch(&self, url: &str, key: &str) -> Result<(u16, Vec<u8>), String>;
}

pub struct HttpKeyInfo;

impl KeyInfoFetch for HttpKeyInfo {
    fn fetch(&self, url: &str, key: &str) -> Result<(u16, Vec<u8>), String> {
        if !(url.starts_with("https://") || url.starts_with("http://")) {
            return Err("LiteLLM url must be http(s)".to_string());
        }
        let endpoint = format!("{}/key/info", url.trim_end_matches('/'));
        let agent = ureq::AgentBuilder::new().timeout(KEY_INFO_TIMEOUT).build();
        let request = agent
            .get(&endpoint)
            .set("Authorization", &format!("Bearer {key}"))
            .set("Accept", "application/json");
        match request.call() {
            Ok(response) => {
                let status = response.status();
                let mut body = Vec::new();
                response
                    .into_reader()
                    .take(65_536)
                    .read_to_end(&mut body)
                    .map_err(|error| error.to_string())?;
                Ok((status, body))
            }
            Err(ureq::Error::Status(status, response)) => {
                let mut body = Vec::new();
                let _ = response.into_reader().take(65_536).read_to_end(&mut body);
                Ok((status, body))
            }
            Err(error) => Err(error.to_string()),
        }
    }
}

/// The tails of the account's newest transcripts, newest first.
pub fn transcript_tails(agent: &str, directory: &Path, now_unix: u64) -> Vec<String> {
    let root = directory.join(if agent == "claude" {
        "projects"
    } else {
        "sessions"
    });
    if !root.is_dir() {
        return Vec::new();
    }
    let cutoff = now_unix.saturating_sub(MAX_AGE_SECONDS);
    let mut found: Vec<(u64, std::path::PathBuf)> = Vec::new();
    let mut stack = vec![root.clone()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            let Ok(modified) = meta.modified() else {
                continue;
            };
            let mtime = modified
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            if mtime >= cutoff {
                found.push((mtime, path));
            }
        }
    }
    found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
    let mut tails = Vec::new();
    for (_mtime, path) in found.into_iter().take(MAX_FILES) {
        if let Some(tail) = read_tail(&path) {
            tails.push(tail);
        }
    }
    tails
}

fn read_tail(path: &Path) -> Option<String> {
    let mut handle = File::open(path).ok()?;
    let size = handle.seek(SeekFrom::End(0)).ok()?;
    handle
        .seek(SeekFrom::Start(size.saturating_sub(TAIL_BYTES)))
        .ok()?;
    let mut bytes = Vec::new();
    handle.read_to_end(&mut bytes).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// The parsed `/key/info` answer, or why there is none. The key is never
/// included in the error.
pub fn key_info(url: &str, key: &str, fetch: &dyn KeyInfoFetch) -> (Option<Value>, Option<String>) {
    let (status, body) = match fetch.fetch(url, key) {
        Ok(pair) => pair,
        Err(error) => return (None, Some(format!("LiteLLM unreachable: {error}"))),
    };
    if status >= 400 {
        return (None, Some(format!("LiteLLM /key/info answered {status}")));
    }
    let parsed: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(_) => {
            return (
                None,
                Some("LiteLLM /key/info did not answer JSON".to_string()),
            )
        }
    };
    if !parsed.is_object() {
        return (
            None,
            Some("LiteLLM /key/info did not answer a JSON object".to_string()),
        );
    }
    (Some(parsed), None)
}

/// Wall clock for transcript age. Tests pass an explicit instant instead.
pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
