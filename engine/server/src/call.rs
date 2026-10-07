//! The live call: `GET /api/assistant/call` (WI-960, WI-967).
//!
//! One WebSocket carries a whole spoken conversation with the assistant. The
//! client streams its microphone up as PCM16; the engine decides when the
//! user has finished a turn, transcribes it, runs the assistant's turn
//! *streamed*, and speaks the reply back a sentence at a time while the model
//! is still writing it. Speaking over the reply stops it.
//!
//! Everything a typed turn guarantees still holds, because a call turn *is*
//! an assistant turn — the same loop, tools, untrusted-data delimiting,
//! durable log and transcript (`AssistantRuntime::handle_message_streamed`).
//! In particular **nothing a person says approves anything.** A turn that
//! proposes a change ends at the approval gate exactly as a typed one does;
//! the card goes to the client to be pressed, and while it waits, an
//! utterance is answered with a fixed reminder and never reaches the model —
//! so "yes, do it" can neither approve the card nor (as a typed message
//! would) abandon it. Approve and Deny arrive as `action.resolve` control
//! frames, which only a button sends.
//!
//! ## Shape
//!
//! The pipeline itself — endpointing, transcription, the turn spoken as it
//! streams, barge-in, the approval invariant — is the generic `voxcall`
//! crate (`engine/voxcall`, see its `DESIGN.md`). This module is Vogt's side
//! of it: the route, authentication, the one-call slot, and the providers —
//! the assistant runtime as the `Llm`, the speech proxy as `Stt`/`Tts`, the
//! runtime's pending card as `Approvals`.
//!
//! The socket's handler owns the call's state and is the only thing that
//! reads the socket. A writer task owns the sending half. Each response —
//! transcribe, run the turn, speak it — is a task of its own, so the handler
//! keeps reading audio while a reply is generated and can hear the user
//! interrupt it. A response is cancelled through its `CancellationToken`;
//! where it is when that happens decides what the conversation records (see
//! `TurnStream`), and what the client had started playing decides how much of
//! the reply is kept as heard (`truncate_interrupted_reply`).
//!
//! ## Speech
//!
//! STT and TTS are the deployment's own OpenAI-compatible backends, called
//! through `AssistantSpeech` exactly as the turn-by-turn voice routes call
//! them: a call needs no speech protocol of its own. To hide transcription
//! time, the turn so far is transcribed as soon as the user pauses; if they
//! do not resume, that transcript is ready (or nearly) when the turn is
//! declared over. While they speak, the turn so far is re-transcribed every
//! `partial_interval_ms` and shown as a live caption.

use std::sync::Arc;
use std::time::Duration;

use axum::{
    extract::{
        ws::{
            rejection::WebSocketUpgradeRejection, CloseCode, CloseFrame, Message, WebSocket,
            WebSocketUpgrade,
        },
        State,
    },
    response::{IntoResponse, Response},
};
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use voxcall::{
    Approvals, BoxFuture, CallClientEvent, CallConfig, CallServerEvent, Clip, Endpointer, Inbound,
    Llm, LlmEvent, LlmSink, Outbound, ProviderError, Providers, ResponseReport, Stt, Tts,
    TurnOutcome, TurnRequest,
};

use crate::{
    app::AppState,
    assistant::{AssistantReply, AssistantRuntime, TurnEvent, TurnStream},
    assistant_speech::AssistantSpeech,
    auth::{self, TokenCapability},
    error::ApiError,
    vogt_tools::Caller,
};

/// The live call's settings (`ENGINE_ASSISTANT_CALL_*`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallPolicy {
    /// Off turns the route into a 404 and `/api/config` says so.
    pub enabled: bool,
    /// Silence after speech that ends the user's turn.
    pub end_of_turn_ms: u32,
    /// Voice needed, while a reply is playing, to stop it.
    pub barge_in_ms: u32,
    /// How often the turn so far is re-transcribed as a caption; 0 is never.
    pub partial_interval_ms: u32,
    /// Said while the model runs tools before its answer; empty is nothing.
    pub filler: String,
    /// Which voice detector judges the microphone.
    pub vad: CallVad,
}

/// The voice detector a call uses (`ENGINE_ASSISTANT_CALL_VAD`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CallVad {
    /// `earshot`'s small neural detector: the better judge of real speech.
    #[default]
    Earshot,
    /// The adaptive noise-floor energy detector, for a host where the neural
    /// one misjudges its microphones.
    Energy,
}

impl Default for CallPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            end_of_turn_ms: 700,
            barge_in_ms: 500,
            partial_interval_ms: 1_500,
            filler: "One moment.".to_string(),
            vad: CallVad::Earshot,
        }
    }
}

impl CallPolicy {
    fn call_config(&self) -> CallConfig {
        CallConfig {
            end_of_turn_ms: self.end_of_turn_ms,
            barge_in_ms: self.barge_in_ms,
            partial_interval_ms: self.partial_interval_ms,
            filler: self.filler.clone(),
            ..CallConfig::default()
        }
    }
}

/// The first frame must arrive within this.
const AUTH_DEADLINE: Duration = Duration::from_secs(5);

/// True when a call can be placed on this front door.
pub fn call_available(state: &AppState) -> bool {
    state.config.assistant_call.enabled
        && state.assistant.is_some()
        && state
            .assistant_speech
            .as_ref()
            .is_some_and(|speech| speech.stt_enabled() && speech.tts_enabled())
}

/// `GET /api/assistant/call` — upgrade, then authenticate on the first frame.
///
/// Availability is answered before the upgrade is even looked at, so a plain
/// GET learns "no call here" (404) the same way the other assistant routes
/// say it.
pub async fn call(
    State(state): State<Arc<AppState>>,
    ws: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Response {
    if !call_available(&state) {
        return ApiError::NotFound.into_response();
    }
    match ws {
        Ok(ws) => ws.on_upgrade(move |socket| run(socket, state)),
        Err(rejection) => rejection.into_response(),
    }
}

async fn close_with(socket: &mut WebSocket, code: CloseCode, reason: &'static str) {
    let _ = socket
        .send(Message::Close(Some(CloseFrame {
            code,
            reason: reason.into(),
        })))
        .await;
}

/// Read the first frame; it must be `auth` with a bearer that carries the
/// `assistant` capability. The caller it names drives every turn of the call
/// and is the identity any approval made on it is attributed to.
async fn authenticate(socket: &mut WebSocket, state: &AppState) -> Option<Caller> {
    let first = match tokio::time::timeout(AUTH_DEADLINE, socket.recv()).await {
        Ok(Some(Ok(Message::Text(text)))) => text,
        Ok(Some(Ok(_))) => {
            auth::record_ws_auth_failure("malformed").await;
            close_with(socket, 4401, "auth frame required").await;
            return None;
        }
        Ok(_) => return None,
        Err(_) => {
            close_with(socket, 4408, "auth timeout").await;
            return None;
        }
    };
    let Ok(CallClientEvent::Auth { token }) = serde_json::from_str::<CallClientEvent>(&first)
    else {
        auth::record_ws_auth_failure("malformed").await;
        close_with(socket, 4401, "auth frame required").await;
        return None;
    };
    match auth::authorize(state, &token).await {
        Ok(identity) if identity.allows(TokenCapability::Assistant) => {
            Some(Caller::from_identity(Some(identity)))
        }
        Ok(_) => {
            close_with(socket, 4403, "assistant capability required").await;
            None
        }
        Err(_) => {
            auth::record_ws_auth_failure("wrong-token").await;
            close_with(socket, 4401, "unauthorized").await;
            None
        }
    }
}

/// Inbound frames buffered between the socket and the pipeline: about five
/// seconds of 20 ms audio.
const INBOUND_BUFFER: usize = 256;

async fn run(mut socket: WebSocket, state: Arc<AppState>) {
    let Some(caller) = authenticate(&mut socket, &state).await else {
        return;
    };
    let (Some(runtime), Some(speech)) = (state.assistant.clone(), state.assistant_speech.clone())
    else {
        close_with(&mut socket, 1011, "assistant not configured").await;
        return;
    };
    let Ok(_slot) = Arc::clone(&state.call_slot).try_lock_owned() else {
        let busy = CallServerEvent::Error {
            message: "another call is already in progress".into(),
        };
        let _ = socket
            .send(Message::Text(
                serde_json::to_string(&busy).unwrap_or_default().into(),
            ))
            .await;
        close_with(&mut socket, 4409, "call in progress").await;
        return;
    };
    let policy = &state.config.assistant_call;
    let call_id = Uuid::new_v4().to_string();
    tracing::info!(target: "vogt::call", %call_id, caller = %caller.token_name, "call started");

    let (mut sink, mut stream) = socket.split();
    let (in_tx, in_rx) = mpsc::channel::<Inbound>(INBOUND_BUFFER);
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Outbound>();
    let writer = tokio::spawn(async move {
        while let Some(item) = out_rx.recv().await {
            let message = match item {
                Outbound::Event(event) => match serde_json::to_string(&event) {
                    Ok(text) => Message::Text(text.into()),
                    Err(_) => continue,
                },
                Outbound::Audio(bytes) => Message::Binary(bytes),
            };
            if sink.send(message).await.is_err() {
                break;
            }
        }
        let _ = sink.close().await;
    });
    let reader = tokio::spawn(async move {
        while let Some(Ok(message)) = stream.next().await {
            let inbound = match message {
                Message::Binary(bytes) => Inbound::Audio(bytes),
                Message::Text(text) => Inbound::Control(text.to_string()),
                Message::Close(_) => break,
                _ => continue,
            };
            if in_tx.send(inbound).await.is_err() {
                break;
            }
        }
    });

    let config = policy.call_config();
    let detector: Box<dyn voxcall::TurnDetector> = match policy.vad {
        CallVad::Earshot => Box::new(Endpointer::new(
            config.endpoint_config(),
            voxcall::EarshotVad::default(),
        )),
        CallVad::Energy => Box::new(Endpointer::new(
            config.endpoint_config(),
            voxcall::EnergyVad::new(voxcall::EnergyVadConfig::default()),
        )),
    };
    let providers = Providers {
        stt: Arc::new(SpeechProvider(Arc::clone(&speech))),
        tts: Arc::new(SpeechProvider(speech)),
        llm: Arc::new(AssistantTurn {
            runtime: Arc::clone(&runtime),
            caller: caller.clone(),
            profile: parking_lot::Mutex::new(None),
        }),
        approvals: Arc::new(AssistantCards { runtime, caller }),
        observer: Some(Arc::new(log_response)),
    };
    voxcall::run(call_id.clone(), config, providers, detector, in_rx, out_tx).await;
    reader.abort();
    // Let the last events (the final `response.done`) drain before closing.
    let _ = tokio::time::timeout(Duration::from_secs(2), writer).await;
    tracing::info!(target: "vogt::call", %call_id, "call ended");
}

fn log_response(report: &ResponseReport) {
    let m = &report.metrics;
    tracing::info!(
        target: "vogt::call",
        response_id = %report.response_id,
        status = ?report.status,
        endpoint_ms = m.endpoint_ms,
        stt_ms = m.stt_ms,
        llm_first_text_ms = m.llm_first_text_ms,
        tts_first_ms = m.tts_first_ms,
        speech_end_to_first_audio_ms = m.speech_end_to_first_audio_ms,
        tool_rounds = m.tool_rounds,
        filler = m.filler,
        "call response"
    );
}

/// The deployment's own STT and TTS backends, through the speech proxy.
struct SpeechProvider(Arc<AssistantSpeech>);

impl Stt for SpeechProvider {
    fn transcribe(&self, wav: Vec<u8>) -> BoxFuture<'_, Result<String, ProviderError>> {
        Box::pin(async move {
            self.0
                .transcribe(wav, "turn.wav", "audio/wav", None)
                .await
                .map_err(|e| ProviderError(e.to_string()))
        })
    }
}

impl Tts for SpeechProvider {
    fn synthesize(&self, text: String) -> BoxFuture<'_, Result<Clip, ProviderError>> {
        Box::pin(async move {
            self.0
                .synthesize(&text)
                .await
                .map(|clip| Clip {
                    content_type: clip.content_type,
                    bytes: clip.bytes,
                })
                .map_err(|e| ProviderError(e.to_string()))
        })
    }
}

/// The assistant's turn, streamed and told it is on a call. Every turn is
/// made as the caller who placed the call — including the resumed turn
/// after a card's button, so a write is attributed to whoever pressed it.
struct AssistantTurn {
    runtime: Arc<AssistantRuntime>,
    caller: Caller,
    profile: parking_lot::Mutex<Option<String>>,
}

impl Llm for AssistantTurn {
    fn turn(
        &self,
        request: TurnRequest,
        sink: LlmSink,
        cancel: CancellationToken,
    ) -> BoxFuture<'_, Result<TurnOutcome, ProviderError>> {
        Box::pin(async move {
            let stream = TurnStream {
                sink: Arc::new(move |event| {
                    sink(match event {
                        TurnEvent::TextDelta(text) => LlmEvent::TextDelta(text),
                        TurnEvent::ToolRound { .. } => LlmEvent::ToolRound,
                    })
                }),
                cancel,
                spoken: true,
            };
            let reply = match request {
                TurnRequest::Utterance(text) => {
                    let profile = self.profile.lock().clone();
                    self.runtime
                        .handle_message_streamed(
                            self.caller.clone(),
                            text.clone(),
                            Some(text),
                            profile,
                            &stream,
                        )
                        .await
                }
                TurnRequest::Resolve { card_id, approve } => {
                    let id = Uuid::parse_str(&card_id)
                        .map_err(|_| ProviderError("not a card id".into()))?;
                    self.runtime
                        .resolve_action_streamed(self.caller.clone(), id, approve, &stream)
                        .await
                }
            };
            reply.map(outcome).map_err(|e| ProviderError(e.to_string()))
        })
    }

    fn truncate_reply(&self, heard: String) -> BoxFuture<'_, bool> {
        Box::pin(async move { self.runtime.truncate_interrupted_reply(&heard).await })
    }

    fn session_update(&self, options: serde_json::Map<String, Value>) {
        if let Some(profile) = options.get("profile") {
            *self.profile.lock() = profile.as_str().map(str::to_string);
        }
    }
}

fn outcome(reply: AssistantReply) -> TurnOutcome {
    if let Some(card) = reply.pending_action {
        return TurnOutcome::AwaitingApproval {
            card: serde_json::to_value(card).unwrap_or(Value::Null),
        };
    }
    if reply.interrupted {
        TurnOutcome::Interrupted { reply: reply.reply }
    } else {
        TurnOutcome::Completed { reply: reply.reply }
    }
}

/// The runtime's one pending card.
struct AssistantCards {
    runtime: Arc<AssistantRuntime>,
    caller: Caller,
}

impl Approvals for AssistantCards {
    fn pending(&self) -> BoxFuture<'_, Option<Value>> {
        Box::pin(async move {
            self.runtime
                .pending_action()
                .await
                .and_then(|card| serde_json::to_value(card).ok())
        })
    }

    fn held_utterance(&self, text: String) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            self.runtime
                .record_held_utterance(&self.caller, &text)
                .await
        })
    }
}
