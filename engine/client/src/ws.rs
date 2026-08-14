//! PTY attach over the server's WebSocket protocol (`server/src/ws.rs`).
//!
//! Protocol, in order:
//!   1. client sends `{"type":"auth","token":"…"}` as the FIRST text frame
//!      (the server drops the socket after 5s otherwise);
//!   2. server sends `snapshot-start` (text), then ≤64 KiB binary scrollback
//!      chunks, then `snapshot-done` (text);
//!   3. live binary frames stream both ways: server→client is PTY output,
//!      client→server binary is written verbatim to PTY stdin;
//!   4. client text control frames are JSON (`resize`/`ping`).
//!
//! All scrollback arrives over this socket, so the terminal can start empty and
//! simply feed every [`AttachEvent::Output`] into the parser — no REST-snapshot
//! dedup needed.

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

use crate::protocol::{ClientControl, ServerControl};

/// Client → attach task.
#[derive(Debug)]
pub enum AttachInput {
    /// Raw bytes to write to the PTY stdin.
    Data(Vec<u8>),
    Resize {
        cols: u16,
        rows: u16,
    },
    Ping,
    Close,
}

/// Attach task → client (drained on the GPUI thread).
#[derive(Debug)]
pub enum AttachEvent {
    /// PTY output bytes (scrollback replay and live output, in order).
    Output(Vec<u8>),
    /// `snapshot-done` seen — initial scrollback replay is complete.
    SnapshotReady,
    /// Server reported the client is lagging and is closing the socket.
    Lag(String),
    /// Socket closed (clean or otherwise). Terminal should mark detached.
    Closed,
    /// A connection/protocol error; the string is user-displayable.
    Error(String),
}

/// Handle to a live attach: send input, receive output events.
pub struct AttachHandle {
    pub input_tx: mpsc::UnboundedSender<AttachInput>,
    pub event_rx: mpsc::UnboundedReceiver<AttachEvent>,
}

impl AttachHandle {
    /// Best-effort send of input bytes to the PTY.
    pub fn send_input(&self, bytes: Vec<u8>) {
        let _ = self.input_tx.send(AttachInput::Data(bytes));
    }

    /// Tell the server to resize the PTY.
    pub fn resize(&self, cols: u16, rows: u16) {
        let _ = self.input_tx.send(AttachInput::Resize { cols, rows });
    }

    /// Request a clean close of the attach socket.
    pub fn close(&self) {
        let _ = self.input_tx.send(AttachInput::Close);
    }
}

/// Connect, authenticate, and spawn the duplex pump on the current tokio
/// runtime. Returns immediately with a handle; connection errors surface as an
/// [`AttachEvent::Error`] on `event_rx`.
pub fn spawn_attach(ws_url: String, token: String) -> AttachHandle {
    let (input_tx, input_rx) = mpsc::unbounded_channel::<AttachInput>();
    let (event_tx, event_rx) = mpsc::unbounded_channel::<AttachEvent>();

    tokio::spawn(async move {
        if let Err(e) = run_attach(&ws_url, &token, input_rx, &event_tx).await {
            let _ = event_tx.send(AttachEvent::Error(format!("{e:#}")));
        }
        let _ = event_tx.send(AttachEvent::Closed);
    });

    AttachHandle { input_tx, event_rx }
}

async fn run_attach(
    ws_url: &str,
    token: &str,
    mut input_rx: mpsc::UnboundedReceiver<AttachInput>,
    event_tx: &mpsc::UnboundedSender<AttachEvent>,
) -> Result<()> {
    let (ws, _resp) = tokio_tungstenite::connect_async(ws_url)
        .await
        .with_context(|| format!("connect {ws_url}"))?;
    let (mut sink, mut stream) = ws.split();

    // 1. Auth must be the first frame.
    let auth = ClientControl::Auth {
        token: token.to_string(),
        resume_from: None,
    }
    .to_json();
    sink.send(Message::Text(auth))
        .await
        .context("send auth frame")?;

    loop {
        tokio::select! {
            // Server → client.
            msg = stream.next() => {
                let Some(msg) = msg else { break };
                let msg = msg.context("ws recv")?;
                match msg {
                    Message::Binary(data) => {
                        let _ = event_tx.send(AttachEvent::Output(data.to_vec()));
                    }
                    Message::Text(text) => {
                        if let Ok(ctrl) = serde_json::from_str::<ServerControl>(&text) {
                            match ctrl {
                                ServerControl::SnapshotStart { .. } => {}
                                ServerControl::SnapshotDone => {
                                    let _ = event_tx.send(AttachEvent::SnapshotReady);
                                }
                                ServerControl::Lag { note } => {
                                    let _ = event_tx.send(AttachEvent::Lag(note));
                                    break;
                                }
                            }
                        }
                    }
                    Message::Ping(p) => {
                        let _ = sink.send(Message::Pong(p)).await;
                    }
                    Message::Close(_) => break,
                    Message::Pong(_) | Message::Frame(_) => {}
                }
            }

            // Client → server.
            input = input_rx.recv() => {
                let Some(input) = input else { break };
                match input {
                    AttachInput::Data(bytes) => {
                        sink.send(Message::Binary(bytes)).await.context("send input")?;
                    }
                    AttachInput::Resize { cols, rows } => {
                        let frame = ClientControl::Resize { cols, rows }.to_json();
                        sink.send(Message::Text(frame)).await.context("send resize")?;
                    }
                    AttachInput::Ping => {
                        let frame = ClientControl::Ping.to_json();
                        sink.send(Message::Text(frame)).await.context("send ping")?;
                    }
                    AttachInput::Close => {
                        let _ = sink.send(Message::Close(None)).await;
                        break;
                    }
                }
            }
        }
    }

    Ok(())
}
