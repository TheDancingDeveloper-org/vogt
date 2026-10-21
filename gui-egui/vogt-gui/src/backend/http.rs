//! The real backend: `ehttp` on desktop (a thread per request) and `fetch` in
//! the browser. The event stream is read with a streaming fetch rather than
//! `EventSource`, because `EventSource` cannot send an `Authorization` header.

use std::ops::ControlFlow;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use super::{Backend, Reply, login_reply};
use crate::api::{ApiError, Auth, LoginResult, ServerEvent, SessionListBody, WorkDetail, encode_query, normalise_base};
use crate::sse::SseParser;

type Wake = Arc<dyn Fn() + Send + Sync>;

/// Talks to a Vogt engine over HTTP.
pub struct HttpBackend {
    queue: Arc<Mutex<Vec<Reply>>>,
    wake: Wake,
    /// The generation callbacks may still report for; 0 means none.
    live_stream: Arc<AtomicU64>,
}

impl HttpBackend {
    /// A backend that calls `wake` whenever a reply arrives, so the UI repaints
    /// and polls (pass `ctx.request_repaint()`).
    pub fn new(wake: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            queue: Arc::default(),
            wake: Arc::new(wake),
            live_stream: Arc::default(),
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

    fn poll(&mut self) -> Vec<Reply> {
        match self.queue.lock() {
            Ok(mut queue) => std::mem::take(&mut *queue),
            Err(_) => Vec::new(),
        }
    }
}
