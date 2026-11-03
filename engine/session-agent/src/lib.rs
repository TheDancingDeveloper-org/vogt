//! The pod-side Vogt session agent.
//!
//! The agent owns the PTY and makes the connection to the engine; a session pod
//! never listens for an engine connection. See the crate README for the wire
//! protocol.

use std::{
    collections::VecDeque,
    io::{Read, Write},
    sync::Arc,
    time::Duration,
};

use futures_util::{SinkExt, StreamExt};
use portable_pty::{CommandBuilder, PtySize};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::{mpsc, Mutex};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{client::IntoClientRequest, http::HeaderValue, Message},
};

pub const DEFAULT_BUFFER_BYTES: usize = 1024 * 1024;

/// Messages exchanged on the authenticated engine stream. Binary PTY data is
/// represented as JSON byte arrays so this module remains usable by non-Rust
/// engine clients without a second codec.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Frame {
    Hello { session_id: String },
    Resume { cursor: u64 },
    Output { offset: u64, data: Vec<u8> },
    Input { data: Vec<u8> },
    Resize { cols: u16, rows: u16 },
    Signal { signal: String },
    Ack { offset: u64 },
    Heartbeat { cursor: u64 },
    Exited { status: Option<i32> },
    Error { code: String, message: String },
}

/// A bounded absolute-offset replay buffer. `next_offset` is the offset after
/// the last byte appended; `replay(cursor)` returns bytes starting at cursor.
#[derive(Debug, Clone)]
pub struct ReplayBuffer {
    capacity: usize,
    start: u64,
    bytes: VecDeque<u8>,
}

impl ReplayBuffer {
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0);
        Self {
            capacity,
            start: 0,
            bytes: VecDeque::new(),
        }
    }
    pub fn start(&self) -> u64 {
        self.start
    }
    pub fn end(&self) -> u64 {
        self.start + self.bytes.len() as u64
    }
    pub fn append(&mut self, data: &[u8]) {
        for &byte in data {
            self.bytes.push_back(byte);
        }
        while self.bytes.len() > self.capacity {
            self.bytes.pop_front();
            self.start += 1;
        }
    }
    pub fn ack(&mut self, offset: u64) {
        let n = offset
            .saturating_sub(self.start)
            .min(self.bytes.len() as u64) as usize;
        self.bytes.drain(..n);
        self.start += n as u64;
    }
    pub fn replay(&self, cursor: u64) -> Option<Vec<u8>> {
        if cursor < self.start || cursor > self.end() {
            return None;
        }
        Some(
            self.bytes
                .iter()
                .skip((cursor - self.start) as usize)
                .copied()
                .collect(),
        )
    }
}

#[derive(Debug, Error)]
pub enum AgentError {
    #[error("missing environment variable {0}")]
    MissingEnv(&'static str),
    #[error("invalid engine URL: {0}")]
    Url(#[from] url::ParseError),
    #[error("invalid token header")]
    TokenHeader,
    #[error("PTY error: {0}")]
    Pty(String),
    #[error("websocket error: {0}")]
    WebSocket(#[from] tokio_tungstenite::tungstenite::Error),
    #[error("protocol error: {0}")]
    Protocol(#[from] serde_json::Error),
}

#[derive(Debug, Clone)]
pub struct Config {
    pub session_id: String,
    pub engine_url: String,
    pub token: String,
    pub command: String,
    pub cwd: Option<String>,
    pub cols: u16,
    pub rows: u16,
    pub buffer_bytes: usize,
}

impl Config {
    pub fn from_env() -> Result<Self, AgentError> {
        let get = |key: &'static str| std::env::var(key).map_err(|_| AgentError::MissingEnv(key));
        Ok(Self {
            session_id: get("VOGT_SESSION_ID")?,
            engine_url: get("VOGT_ENGINE_URL")?,
            token: get("VOGT_AGENT_TOKEN")?,
            command: get("VOGT_SESSION_COMMAND")?,
            cwd: std::env::var("VOGT_SESSION_CWD").ok(),
            cols: std::env::var("VOGT_PTY_COLS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(120),
            rows: std::env::var("VOGT_PTY_ROWS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(40),
            buffer_bytes: std::env::var("VOGT_AGENT_BUFFER_BYTES")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(DEFAULT_BUFFER_BYTES),
        })
    }
    pub fn ws_url(&self) -> Result<String, AgentError> {
        let mut url = url::Url::parse(&self.engine_url)?;
        let scheme = match url.scheme() {
            "http" => "ws",
            "https" => "wss",
            "ws" => "ws",
            "wss" => "wss",
            _ => return Err(AgentError::Url(url::ParseError::RelativeUrlWithoutBase)),
        };
        url.set_scheme(scheme)
            .map_err(|_| url::ParseError::RelativeUrlWithoutBase)?;
        let path = format!("/api/sessions/{}/agent", self.session_id);
        url.set_path(&path);
        Ok(url.to_string())
    }
}

pub fn authorize(request: &mut http::Request<()>, token: &str) -> Result<(), AgentError> {
    let value =
        HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_| AgentError::TokenHeader)?;
    request.headers_mut().insert("authorization", value);
    Ok(())
}

/// Spawn a command in a PTY and return its reader, writer and child handle.
type PtyChild = Box<dyn portable_pty::Child + Send>;
type PtyIo = (Box<dyn Read + Send>, Box<dyn Write + Send>, PtyChild);

pub fn spawn_pty(config: &Config) -> Result<PtyIo, AgentError> {
    let system = portable_pty::native_pty_system();
    let pair = system
        .openpty(PtySize {
            rows: config.rows,
            cols: config.cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| AgentError::Pty(e.to_string()))?;
    let mut command = CommandBuilder::new("/bin/sh");
    command.arg("-lc");
    command.arg(&config.command);
    if let Some(cwd) = &config.cwd {
        command.cwd(cwd);
    }
    let child = pair
        .slave
        .spawn_command(command)
        .map_err(|e| AgentError::Pty(e.to_string()))?;
    let reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| AgentError::Pty(e.to_string()))?;
    let writer = pair
        .master
        .take_writer()
        .map_err(|e| AgentError::Pty(e.to_string()))?;
    Ok((reader, writer, child))
}

pub async fn run(config: Config) -> Result<(), AgentError> {
    let (reader, writer, child) = spawn_pty(&config)?;
    let writer = Arc::new(Mutex::new(writer));
    let (output_tx, mut output_rx) = mpsc::channel::<Vec<u8>>(32);
    std::thread::spawn(move || {
        let mut reader = reader;
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if output_tx.blocking_send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });
    let mut child = Some(child);
    let mut replay = ReplayBuffer::new(config.buffer_bytes);
    let mut cursor = 0u64;
    let mut acked = 0u64;
    let mut backoff = Duration::from_millis(100);
    loop {
        let mut req = config.ws_url()?.into_client_request()?;
        authorize(&mut req, &config.token)?;
        match connect_async(req).await {
            Ok((socket, _)) => {
                backoff = Duration::from_millis(100);
                let (mut sink, mut stream) = socket.split();
                sink.send(Message::Text(
                    serde_json::to_string(&Frame::Hello {
                        session_id: config.session_id.clone(),
                    })?
                    .into(),
                ))
                .await?;
                sink.send(Message::Text(
                    serde_json::to_string(&Frame::Resume { cursor: acked })?.into(),
                ))
                .await?;
                if let Some(bytes) = replay.replay(acked) {
                    if !bytes.is_empty() {
                        sink.send(Message::Text(
                            serde_json::to_string(&Frame::Output {
                                offset: acked,
                                data: bytes,
                            })?
                            .into(),
                        ))
                        .await?;
                    }
                }
                loop {
                    tokio::select! {
                        Some(data) = output_rx.recv() => { replay.append(&data); let frame = Frame::Output { offset: cursor, data: data.clone() }; cursor += data.len() as u64; sink.send(Message::Text(serde_json::to_string(&frame)?.into())).await?; }
                        Some(message) = stream.next() => match message? { Message::Text(text) => match serde_json::from_str::<Frame>(&text)? { Frame::Input { data } => { writer.lock().await.write_all(&data).map_err(|e| AgentError::Pty(e.to_string()))?; }, Frame::Resize { .. } => {}, Frame::Signal { signal } if signal == "kill" => { if let Some(c) = child.as_mut() { c.kill().map_err(|e| AgentError::Pty(e.to_string()))?; } }, Frame::Ack { offset } => { if offset <= cursor { replay.ack(offset); acked = acked.max(offset); } }, Frame::Heartbeat { .. } => {}, _ => {} }, Message::Ping(data) => { sink.send(Message::Pong(data)).await?; }, Message::Close(_) => break, _ => {} },
                        _ = tokio::time::sleep(Duration::from_secs(15)) => { sink.send(Message::Text(serde_json::to_string(&Frame::Heartbeat { cursor })?.into())).await?; }
                    }
                }
            }
            Err(_) => {
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(10));
            }
        }
        if let Some(c) = child.as_mut() {
            if let Ok(Some(status)) = c.try_wait() {
                let code = status.exit_code() as i32;
                if let Ok((socket, _)) = connect_async(config.ws_url()?).await {
                    let (mut sink, _) = socket.split();
                    let _ = sink
                        .send(Message::Text(
                            serde_json::to_string(&Frame::Exited { status: Some(code) })?.into(),
                        ))
                        .await;
                }
                return Ok(());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn replay_round_trip() {
        let mut b = ReplayBuffer::new(64);
        b.append(b"hello ");
        b.append(b"world");
        assert_eq!(b.replay(0).unwrap(), b"hello world");
        b.ack(6);
        assert_eq!(b.replay(6).unwrap(), b"world");
    }
    #[test]
    fn overflow_reports_gap() {
        let mut b = ReplayBuffer::new(4);
        b.append(b"abcdef");
        assert_eq!(b.start(), 2);
        assert!(b.replay(0).is_none());
        assert_eq!(b.replay(2).unwrap(), b"cdef");
    }
    #[test]
    fn frames_are_stable_json() {
        let f = Frame::Output {
            offset: 4,
            data: b"ok".to_vec(),
        };
        assert_eq!(
            serde_json::from_str::<Frame>(&serde_json::to_string(&f).unwrap()).unwrap(),
            f
        );
    }
    #[test]
    fn auth_is_bearer() {
        let mut req = http::Request::new(());
        authorize(&mut req, "secret").unwrap();
        assert_eq!(req.headers()["authorization"], "Bearer secret");
    }
}
