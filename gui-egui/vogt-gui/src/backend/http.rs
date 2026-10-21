//! The real backend: `ehttp` on desktop (a thread per request) and `fetch` in
//! the browser. The event stream is read with a streaming fetch rather than
//! `EventSource`, because `EventSource` cannot send an `Authorization` header.

use std::ops::ControlFlow;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use super::{AttachEvent, Backend, Reply, login_reply};
use crate::api::{
    ApiError, AttachControl, Auth, LoginResult, ServerEvent, SessionListBody, SessionScreen, WorkDetail, attach_url,
    encode_query, normalise_base,
};
use crate::sse::SseParser;

type Wake = Arc<dyn Fn() + Send + Sync>;

/// A cold attach asks for at most this much trailing output, so the first
/// paint does not replay the whole scrollback ring.
const COLD_SNAPSHOT_TAIL_BYTES: u64 = 256 * 1024;

/// The open terminal attach.
struct Attached {
    generation: u64,
    sender: ewebsock::WsSender,
    /// The first-frame auth, sent once the socket reports it is open. The
    /// bearer goes in a frame, never in the URL, so it stays out of logs.
    auth_frame: Option<String>,
}

/// Talks to a Vogt engine over HTTP.
pub struct HttpBackend {
    queue: Arc<Mutex<Vec<Reply>>>,
    wake: Wake,
    /// The generation callbacks may still report for; 0 means none.
    live_stream: Arc<AtomicU64>,
    /// The attach generation callbacks may still report for; 0 means none.
    live_attach: Arc<AtomicU64>,
    attached: Option<Attached>,
}

impl HttpBackend {
    /// A backend that calls `wake` whenever a reply arrives, so the UI repaints
    /// and polls (pass `ctx.request_repaint()`).
    pub fn new(wake: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            queue: Arc::default(),
            wake: Arc::new(wake),
            live_stream: Arc::default(),
            live_attach: Arc::default(),
            attached: None,
        }
    }

    fn sink(&self) -> Sink {
        Sink {
            queue: Arc::clone(&self.queue),
            wake: Arc::clone(&self.wake),
        }
    }
}

#[derive(Clone)]
struct Sink {
    queue: Arc<Mutex<Vec<Reply>>>,
    wake: Wake,
}

impl Sink {
    fn push(&self, reply: Reply) {
        if let Ok(mut queue) = self.queue.lock() {
            queue.push(reply);
        }
        (self.wake)();
    }
}

fn authed(request: ehttp::Request, auth: &Auth) -> ehttp::Request {
    request
        .with_header("Authorization", format!("Bearer {}", auth.token))
        .with_header("Accept", "application/json")
}

/// Decode a finished response as JSON `T`, or say why not.
fn decode<T: serde::de::DeserializeOwned>(result: ehttp::Result<ehttp::Response>) -> Result<T, ApiError> {
    let response = result.map_err(ApiError::transport)?;
    if !response.ok {
        return Err(ApiError::from_status(response.status, &response.bytes));
    }
    serde_json::from_slice(&response.bytes).map_err(|e| ApiError {
        status: Some(response.status),
        message: format!("unexpected reply: {e}"),
    })
}

impl Backend for HttpBackend {
    fn login(&mut self, base: &str, username: &str, password: &str) {
        let base = normalise_base(base);
        let body = serde_json::json!({ "username": username.trim(), "password": password, "session_name": "vogt-gui" });
        let request = ehttp::Request::post(format!("{base}/api/auth/login"), body.to_string().into_bytes())
            .with_header("Content-Type", "application/json")
            .with_header("Accept", "application/json");
        let sink = self.sink();
        ehttp::fetch(request, move |result| {
            sink.push(login_reply(&base, decode::<LoginResult>(result)))
        });
    }

    fn fetch_sessions(&mut self, auth: &Auth) {
        let request = authed(ehttp::Request::get(format!("{}/api/sessions", auth.base)), auth);
        let sink = self.sink();
        ehttp::fetch(request, move |result| {
            sink.push(Reply::Sessions(
                decode::<SessionListBody>(result).map(SessionListBody::into_sessions),
            ));
        });
    }

    fn fetch_work_item(&mut self, auth: &Auth, reference: &str) {
        let url = format!(
            "{}/api/vogt/work/get?ref={}&mode=full",
            auth.base,
            encode_query(reference)
        );
        let request = authed(ehttp::Request::get(url), auth);
        let sink = self.sink();
        let reference = reference.to_owned();
        ehttp::fetch(request, move |result| {
            sink.push(Reply::WorkItem(reference, decode::<WorkDetail>(result).map(Box::new)))
        });
    }

    fn open_events(&mut self, auth: &Auth, generation: u64) {
        self.live_stream.store(generation, Ordering::SeqCst);
        let request = ehttp::Request::get(format!("{}/api/events", auth.base))
            .with_header("Authorization", format!("Bearer {}", auth.token))
            .with_header("Accept", "text/event-stream")
            // The stream is meant to stay open; the default 30 s timeout would cut it.
            .with_timeout(None);
        let sink = self.sink();
        let live = Arc::clone(&self.live_stream);
        let parser = Mutex::new(SseParser::new());
        ehttp::streaming::fetch(request, move |part| {
            // A newer stream (or a close) superseded this one: stop reading.
            if live.load(Ordering::SeqCst) != generation {
                return ControlFlow::Break(());
            }
            match part {
                Err(message) => {
                    sink.push(Reply::StreamClosed(generation, ApiError::transport(message)));
                    ControlFlow::Break(())
                }
                Ok(ehttp::streaming::Part::Response(head)) => {
                    if head.ok {
                        sink.push(Reply::StreamOpened(generation));
                        ControlFlow::Continue(())
                    } else {
                        sink.push(Reply::StreamClosed(generation, ApiError::from_status(head.status, b"")));
                        ControlFlow::Break(())
                    }
                }
                Ok(ehttp::streaming::Part::Chunk(bytes)) if bytes.is_empty() => {
                    sink.push(Reply::StreamClosed(
                        generation,
                        ApiError::transport("the event stream ended"),
                    ));
                    ControlFlow::Break(())
                }
                Ok(ehttp::streaming::Part::Chunk(bytes)) => {
                    let fed = match parser.lock() {
                        Ok(mut parser) => parser.feed(&bytes),
                        Err(_) => return ControlFlow::Break(()),
                    };
                    if fed.events.is_empty() && fed.heartbeat {
                        sink.push(Reply::StreamAlive(generation));
                    }
                    for data in fed.events {
                        match serde_json::from_str::<ServerEvent>(&data) {
                            Ok(event) => sink.push(Reply::StreamEvent(generation, event)),
                            Err(e) => log::debug!("ignoring malformed event frame: {e}"),
                        }
                    }
                    ControlFlow::Continue(())
                }
            }
        });
    }

    fn close_events(&mut self) {
        self.live_stream.store(0, Ordering::SeqCst);
    }

    fn fetch_screen(&mut self, auth: &Auth, id: &str) {
        let url = format!("{}/api/sessions/{}/screen", auth.base, encode_query(id));
        let request = authed(ehttp::Request::get(url), auth);
        let sink = self.sink();
        let id = id.to_owned();
        ehttp::fetch(request, move |result| {
            sink.push(Reply::Screen(id, decode::<SessionScreen>(result).map(Box::new)))
        });
    }

    fn attach(&mut self, auth: &Auth, id: &str, generation: u64, resume_from: Option<u64>) {
        self.detach();
        self.live_attach.store(generation, Ordering::SeqCst);
        let mut frame = serde_json::json!({ "type": "auth", "token": auth.token });
        match resume_from {
            Some(pos) => frame["resume_from"] = pos.into(),
            None => frame["snapshot_tail_bytes"] = COLD_SNAPSHOT_TAIL_BYTES.into(),
        }
        let sink = self.sink();
        let live = Arc::clone(&self.live_attach);
        let on_event = Box::new(move |event: ewebsock::WsEvent| {
            // A newer attach (or a detach) superseded this one: stop reading.
            if live.load(Ordering::SeqCst) != generation {
                return ControlFlow::Break(());
            }
            let (event, flow) = match event {
                ewebsock::WsEvent::Opened => (AttachEvent::Opened, ControlFlow::Continue(())),
                ewebsock::WsEvent::Message(ewebsock::WsMessage::Binary(bytes)) => {
                    (AttachEvent::Bytes(bytes), ControlFlow::Continue(()))
                }
                ewebsock::WsEvent::Message(ewebsock::WsMessage::Text(text)) => {
                    match serde_json::from_str::<AttachControl>(&text) {
                        Ok(control) => (AttachEvent::Control(control), ControlFlow::Continue(())),
                        Err(e) => {
                            log::debug!("ignoring malformed attach frame: {e}");
                            return ControlFlow::Continue(());
                        }
                    }
                }
                ewebsock::WsEvent::Message(_) => return ControlFlow::Continue(()),
                ewebsock::WsEvent::Error(message) => (AttachEvent::Closed(message), ControlFlow::Break(())),
                ewebsock::WsEvent::Closed => (
                    AttachEvent::Closed("the terminal connection closed".into()),
                    ControlFlow::Break(()),
                ),
            };
            sink.push(Reply::Attach(generation, event));
            flow
        });
        match ewebsock::ws_connect(attach_url(&auth.base, id), ewebsock::Options::default(), on_event) {
            Ok(sender) => {
                self.attached = Some(Attached {
                    generation,
                    sender,
                    auth_frame: Some(frame.to_string()),
                });
            }
            Err(message) => self
                .sink()
                .push(Reply::Attach(generation, AttachEvent::Closed(message))),
        }
    }

    fn attach_ping(&mut self, id: u64) {
        if let Some(attached) = &mut self.attached {
            let ping = serde_json::json!({ "type": "ping", "id": id });
            attached.sender.send(ewebsock::WsMessage::Text(ping.to_string()));
        }
    }

    fn detach(&mut self) {
        self.live_attach.store(0, Ordering::SeqCst);
        if let Some(mut attached) = self.attached.take() {
            attached.sender.close();
        }
    }

    fn poll(&mut self) -> Vec<Reply> {
        let replies = match self.queue.lock() {
            Ok(mut queue) => std::mem::take(&mut *queue),
            Err(_) => Vec::new(),
        };
        // Authenticate an attach as soon as its socket is open.
        if let Some(attached) = &mut self.attached
            && replies
                .iter()
                .any(|r| matches!(r, Reply::Attach(g, AttachEvent::Opened) if *g == attached.generation))
            && let Some(frame) = attached.auth_frame.take()
        {
            attached.sender.send(ewebsock::WsMessage::Text(frame));
        }
        replies
    }
}
