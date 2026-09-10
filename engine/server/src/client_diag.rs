//! Client diagnostics ingest — the PWA's own account of what it did, placed on
//! the engine's log stream.
//!
//! The engine can see that a phone asked for `POST /api/assistant/tts` and got
//! a 200; it cannot see what the WebView then did with the bytes — whether
//! `audio.play()` resolved or rejected (and with what), whether the element
//! fired `ended` at once, what the Blob's type was, or what the hands-free
//! state machine was doing. Debugging that from the server side was guesswork.
//! This route closes the loop: the client posts small batches of structured
//! events and each one is emitted here through `tracing`, under the
//! `client_diag` target with `client_diag` as the message, so it lands in the
//! same stdout an operator already reads (`docker logs`, Komodo) and is
//! trivially greppable. Nothing is stored; there is no query API.
//!
//! Bounded on purpose: a batch is at most `MAX_EVENTS` events, an event name is
//! a short identifier, and an event's fields serialise to at most
//! `MAX_FIELDS_BYTES`. Any valid token may post — a diagnostic is not a
//! capability — and the ordinary mutating-request rate limit applies. The client
//! sends lengths and names, never transcript text.

use axum::{http::StatusCode, Json};
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::error::{ApiError, Result};

/// Most events accepted in one batch.
pub const MAX_EVENTS: usize = 64;
/// Longest event name.
pub const MAX_EVENT_NAME: usize = 64;
/// Largest serialised `fields` object per event.
pub const MAX_FIELDS_BYTES: usize = 2048;

#[derive(Debug, Deserialize)]
pub struct ClientEvent {
    /// Client clock, ms since the epoch — kept so ordering survives batching.
    pub t: u64,
    /// Dotted identifier, e.g. `tts.play.rejected`.
    pub event: String,
    #[serde(default)]
    pub fields: Map<String, Value>,
}

#[derive(Debug, Deserialize)]
pub struct ClientLogBatch {
    pub events: Vec<ClientEvent>,
}

fn valid_event_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_EVENT_NAME
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// `POST /api/client-log` — validate the batch, emit each event, answer 204.
pub async fn ingest(Json(batch): Json<ClientLogBatch>) -> Result<StatusCode> {
    if batch.events.is_empty() {
        return Err(ApiError::BadRequest("no events".into()));
    }
    if batch.events.len() > MAX_EVENTS {
        return Err(ApiError::BadRequest(format!(
            "too many events in one batch (max {MAX_EVENTS})"
        )));
    }
    for event in &batch.events {
        if !valid_event_name(&event.event) {
            return Err(ApiError::BadRequest(
                "event name must be a short dotted identifier".into(),
            ));
        }
        let fields = serde_json::to_string(&event.fields).unwrap_or_default();
        if fields.len() > MAX_FIELDS_BYTES {
            return Err(ApiError::BadRequest(format!(
                "event fields too large (max {MAX_FIELDS_BYTES} bytes)"
            )));
        }
        tracing::info!(
            target: "client_diag",
            event = %event.event,
            t = event.t,
            fields = %fields,
            "client_diag"
        );
    }
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::valid_event_name;

    #[test]
    fn event_names_are_short_dotted_identifiers() {
        assert!(valid_event_name("tts.play.rejected"));
        assert!(valid_event_name("voice_state-2"));
        assert!(!valid_event_name(""));
        assert!(!valid_event_name("has space"));
        assert!(!valid_event_name(&"x".repeat(65)));
    }
}
