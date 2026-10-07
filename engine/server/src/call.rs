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

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

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
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use vogt_engine_contract::{
    CallClientEvent, CallMetrics, CallResponseStatus, CallServerEvent, CallState, PendingAction,
};

use crate::{
    app::AppState,
    assistant::{AssistantReply, AssistantRuntime, PendingActionView, TurnEvent, TurnStream},
    assistant_speech::{AssistantSpeech, SpeechClip},
    auth::{self, TokenCapability},
    call_audio::{
        pcm16_from_le_bytes, wav_duration, wav_from_pcm16, EndpointConfig, EndpointEvent,
        Endpointer, EnergyVad, EnergyVadConfig, CALL_SAMPLE_RATE,
    },
    call_text::{speakable, SentenceChunker},
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
}

impl Default for CallPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            end_of_turn_ms: 700,
            barge_in_ms: 500,
            partial_interval_ms: 1_500,
            filler: "One moment.".to_string(),
        }
    }
}

/// Spoken when an utterance arrives while an approval card is waiting.
const APPROVAL_REMINDER: &str = "That change is waiting on your screen. Tap approve or deny there.";

/// Spoken when a turn stops at the approval gate without having said
/// anything itself.
const PROPOSED_LINE: &str = "I've put that change on your screen for you to approve.";

/// Spoken when the assistant could not produce a reply at all.
const FAILED_LINE: &str = "Sorry, I couldn't get an answer just then.";

/// The first frame must arrive within this.
const AUTH_DEADLINE: Duration = Duration::from_secs(5);

/// The largest audio frame accepted: two seconds of 16 kHz PCM16. The PWA
/// sends 20 ms frames; anything this big is not a microphone.
const MAX_AUDIO_FRAME_BYTES: usize = 64 * 1024;

/// Grace after a reply should have finished playing before a client that
/// does not report its playback is assumed to have finished.
const PLAYBACK_SLACK: Duration = Duration::from_millis(1_500);

/// A partial caption needs at least this much of the turn to say anything.
const MIN_PARTIAL_MS: u32 = 800;

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

enum Outbound {
    Event(CallServerEvent),
    Audio(Bytes),
}

/// What the call's own tasks tell its handler.
enum Internal {
    Partial {
        utterance: u64,
        text: String,
    },
    /// A response's first audio went out: the reply is now being spoken.
    AudioStarted {
        response_id: String,
    },
    /// A response task ended.
    Finished(Box<Finished>),
}

struct Finished {
    response_id: String,
    /// `None` when the turn turned out not to be one (nothing was said).
    status: Option<CallResponseStatus>,
    text: Option<String>,
    pending: Option<PendingAction>,
    /// The card this response was resolving, and whether that worked and
    /// how. Either way the card is no longer waiting.
    resolved: Option<(Uuid, Option<bool>)>,
    metrics: CallMetrics,
}

/// What the response task and the handler both see of one response.
#[derive(Default)]
struct Shared {
    /// For each piece sent, by index: where it ends in the reply text, or
    /// `None` for a piece that is not part of the recorded reply (a filler,
    /// a reminder, text said before a tool round).
    piece_ends: Vec<Option<usize>>,
    /// The reply text as recorded: what the model wrote after its last tool
    /// round.
    reply_text: String,
    /// The last piece the client reported it had started playing.
    last_started: Option<u32>,
    /// When each piece should start playing, reckoned from when it was sent
    /// and how long the ones before it play — what is used for a client
    /// that does not report its playback.
    estimated_starts: Vec<Instant>,
    /// When the last piece sent should finish playing, by the same reckoning.
    estimated_end: Option<Instant>,
    audio_sent: bool,
    first_audio: Option<Instant>,
    metrics: CallMetrics,
}

impl Shared {
    /// What the listener heard of the recorded reply: up to the end of the
    /// last of its pieces that had started playing — as the client reported
    /// it, or, from a client that reports nothing, as estimated.
    fn heard(&self, client_reports: bool) -> String {
        let last = if client_reports {
            self.last_started
        } else {
            let now = Instant::now();
            self.estimated_starts
                .iter()
                .rposition(|start| *start <= now)
                .map(|i| i as u32)
        };
        let Some(last) = last else {
            return String::new();
        };
        let end = self
            .piece_ends
            .iter()
            .take(last as usize + 1)
            .filter_map(|end| *end)
            .max()
            .unwrap_or(0);
        self.reply_text[..end.min(self.reply_text.len())]
            .trim()
            .to_string()
    }
}

struct ActiveResponse {
    id: String,
    cancel: CancellationToken,
    shared: Arc<parking_lot::Mutex<Shared>>,
    /// The task is still producing the response.
    generating: bool,
    /// Audio has been sent and the client has not said its queue ran dry
    /// (nor, for a client that never says, has the estimate run out).
    playing: bool,
    /// A user utterance long enough to be a barge-in has stopped it.
    cut: bool,
}

/// What starts a response.
enum Trigger {
    /// A finished user turn.
    Speech {
        wav: Vec<u8>,
        eager: Option<JoinHandle<Option<String>>>,
    },
    /// An approval card's button.
    Resolve { id: Uuid, approve: bool },
}

/// Everything a response task needs, cloned out of the handler.
#[derive(Clone)]
struct Ctx {
    runtime: Arc<AssistantRuntime>,
    speech: Arc<AssistantSpeech>,
    caller: Caller,
    profile: Option<String>,
    filler: String,
    out: mpsc::UnboundedSender<Outbound>,
    internal: mpsc::UnboundedSender<Internal>,
    clips: Arc<tokio::sync::Mutex<HashMap<String, SpeechClip>>>,
}

impl Ctx {
    fn send(&self, event: CallServerEvent) {
        let _ = self.out.send(Outbound::Event(event));
    }

    /// A fixed line's audio, synthesized once per call and reused.
    async fn canned(&self, text: &str) -> Option<SpeechClip> {
        if let Some(clip) = self.clips.lock().await.get(text) {
            return Some(clip.clone());
        }
        let clip = self.speech.synthesize(text).await.ok()?;
        self.clips
            .lock()
            .await
            .insert(text.to_string(), clip.clone());
        Some(clip)
    }
}

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
    let policy = state.config.assistant_call.clone();
    let call_id = Uuid::new_v4();
    tracing::info!(target: "vogt::call", %call_id, caller = %caller.token_name, "call started");

    let (mut sink, mut stream) = socket.split();
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
    let (internal_tx, mut internal_rx) = mpsc::unbounded_channel::<Internal>();

    let ctx = Ctx {
        runtime,
        speech,
        caller,
        profile: None,
        filler: policy.filler.clone(),
        out: out_tx,
        internal: internal_tx,
        clips: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
    };
    // Warm the fixed lines so the first time one is needed it is instant.
    {
        let ctx = ctx.clone();
        tokio::spawn(async move {
            for line in [ctx.filler.clone(), APPROVAL_REMINDER.to_string()] {
                if !line.is_empty() {
                    ctx.canned(&line).await;
                }
            }
        });
    }
    let mut call = Call::new(ctx, policy);
    call.ctx.send(CallServerEvent::SessionCreated {
        call_id,
        sample_rate: CALL_SAMPLE_RATE,
        end_of_turn_ms: call.policy.end_of_turn_ms,
        barge_in_ms: call.policy.barge_in_ms,
    });
    call.set_state(CallState::Listening);

    loop {
        tokio::select! {
            message = stream.next() => match message {
                Some(Ok(Message::Binary(bytes))) => call.audio(&bytes),
                Some(Ok(Message::Text(text))) => call.control(&text).await,
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(_)) => {}
            },
            Some(internal) = internal_rx.recv() => call.internal(internal).await,
        }
    }
    call.hang_up();
    drop(call);
    writer.abort();
    tracing::info!(target: "vogt::call", %call_id, "call ended");
}

/// The call's state, owned by the socket handler.
struct Call {
    ctx: Ctx,
    policy: CallPolicy,
    endpointer: Endpointer<EnergyVad>,
    state: CallState,
    /// Bumped at every new utterance, so a late partial or early transcript
    /// of an earlier one is recognised and dropped.
    utterance: u64,
    /// When the current utterance's voice last stopped.
    speech_end: Option<Instant>,
    eager: Option<JoinHandle<Option<String>>>,
    partial_in_flight: bool,
    last_partial: Instant,
    response: Option<ActiveResponse>,
    /// The previous response's task, still finishing after a cut. The next
    /// one waits for it, so a cut reply is truncated before a new turn lands.
    finishing: Option<JoinHandle<()>>,
    pending_card: Option<Uuid>,
    /// The client reports what it plays (`output_audio.started`/`idle`);
    /// until it has, playback is estimated from the clips' lengths.
    client_reports: bool,
}

impl Call {
    fn new(ctx: Ctx, policy: CallPolicy) -> Self {
        let endpoint = EndpointConfig {
            end_of_turn_ms: policy.end_of_turn_ms,
            sustained_ms: policy.barge_in_ms,
            ..EndpointConfig::default()
        };
        Self {
            ctx,
            endpointer: Endpointer::new(endpoint, EnergyVad::new(EnergyVadConfig::default())),
            policy,
            state: CallState::Listening,
            utterance: 0,
            speech_end: None,
            eager: None,
            partial_in_flight: false,
            last_partial: Instant::now(),
            response: None,
            finishing: None,
            pending_card: None,
            client_reports: false,
        }
    }

    fn set_state(&mut self, state: CallState) {
        if self.state != state {
            self.state = state;
            self.ctx.send(CallServerEvent::State { state });
        }
    }

    /// The state with nothing happening: listening, unless a card waits.
    fn resting_state(&self) -> CallState {
        if self.pending_card.is_some() {
            CallState::AwaitingApproval
        } else {
            CallState::Listening
        }
    }

    fn audio(&mut self, bytes: &[u8]) {
        if bytes.len() > MAX_AUDIO_FRAME_BYTES {
            return;
        }
        self.expire_estimated_playback();
        let samples = pcm16_from_le_bytes(bytes);
        for event in self.endpointer.push(&samples) {
            self.endpoint(event);
        }
        self.maybe_partial();
    }

    fn endpoint(&mut self, event: EndpointEvent) {
        match event {
            EndpointEvent::SpeechStarted => {
                self.utterance += 1;
                self.speech_end = None;
                self.last_partial = Instant::now();
                self.ctx.send(CallServerEvent::SpeechStarted);
                if self.response.is_none() {
                    self.set_state(CallState::UserSpeaking);
                }
            }
            EndpointEvent::Sustained => {
                if self.response.is_some() {
                    self.barge_in();
                }
                self.set_state(CallState::UserSpeaking);
            }
            EndpointEvent::PauseBegan => {
                self.speech_end = Some(
                    Instant::now()
                        - Duration::from_millis(EndpointConfig::default().pause_ms as u64),
                );
                if let Some(old) = self.eager.take() {
                    old.abort();
                }
                let wav = wav_from_pcm16(self.endpointer.segment(), CALL_SAMPLE_RATE);
                let speech = Arc::clone(&self.ctx.speech);
                self.eager = Some(tokio::spawn(async move { transcribe(&speech, wav).await }));
            }
            EndpointEvent::SpeechResumed => {
                self.speech_end = None;
                if let Some(eager) = self.eager.take() {
                    eager.abort();
                }
            }
            EndpointEvent::EndOfTurn { audio, speech_ms } => {
                self.ctx.send(CallServerEvent::SpeechStopped);
                let eager = self.eager.take();
                // Too short to have been a barge-in, said over a reply that
                // is still going: a backchannel ("mm", "right") or echo.
                if self.response.as_ref().is_some_and(|r| !r.cut)
                    && speech_ms < self.policy.barge_in_ms
                {
                    if let Some(eager) = eager {
                        eager.abort();
                    }
                    self.restore_state();
                    return;
                }
                let speech_end = self.speech_end.take().unwrap_or_else(|| {
                    Instant::now() - Duration::from_millis(self.policy.end_of_turn_ms as u64)
                });
                let wav = wav_from_pcm16(&audio, CALL_SAMPLE_RATE);
                self.start_response(Trigger::Speech { wav, eager }, Some(speech_end));
            }
            EndpointEvent::Discarded => {
                if let Some(eager) = self.eager.take() {
                    eager.abort();
                }
                self.restore_state();
            }
        }
    }

    /// For a client that does not report playback: once the reply should
    /// have finished playing, treat it as finished.
    fn expire_estimated_playback(&mut self) {
        if self.client_reports {
            return;
        }
        let over = self.response.as_ref().is_some_and(|r| {
            r.playing
                && r.shared
                    .lock()
                    .estimated_end
                    .is_some_and(|end| Instant::now() > end + PLAYBACK_SLACK)
        });
        if over {
            let id = self
                .response
                .as_ref()
                .map(|r| r.id.clone())
                .unwrap_or_default();
            self.playback_idle(&id);
        }
    }

    /// The reply stopped coming out of the speaker.
    fn playback_idle(&mut self, response_id: &str) {
        let Some(response) = self.response.as_mut().filter(|r| r.id == response_id) else {
            return;
        };
        response.playing = false;
        self.endpointer.set_playback(false);
        if !response.generating {
            self.response = None;
        }
        if !self.endpointer.in_turn() {
            self.restore_state();
        }
    }

    /// Back to whatever the call is doing once a stray sound has passed.
    fn restore_state(&mut self) {
        let state = match &self.response {
            Some(r) if r.playing => CallState::Speaking,
            Some(r) if r.generating => CallState::Thinking,
            _ => self.resting_state(),
        };
        self.set_state(state);
    }

    fn maybe_partial(&mut self) {
        if self.policy.partial_interval_ms == 0
            || self.partial_in_flight
            || !self.endpointer.in_turn()
            || self.eager.is_some()
        {
            return;
        }
        let segment_ms = self.endpointer.segment().len() as u32 * 1000 / CALL_SAMPLE_RATE;
        if segment_ms < MIN_PARTIAL_MS
            || self.last_partial.elapsed()
                < Duration::from_millis(self.policy.partial_interval_ms as u64)
        {
            return;
        }
        self.partial_in_flight = true;
        self.last_partial = Instant::now();
        let wav = wav_from_pcm16(self.endpointer.segment(), CALL_SAMPLE_RATE);
        let speech = Arc::clone(&self.ctx.speech);
        let internal = self.ctx.internal.clone();
        let utterance = self.utterance;
        tokio::spawn(async move {
            let text = transcribe(&speech, wav).await.unwrap_or_default();
            let _ = internal.send(Internal::Partial { utterance, text });
        });
    }

    /// Stop the reply: the user spoke over it, or tapped stop.
    fn barge_in(&mut self) {
        let Some(response) = self.response.as_mut() else {
            return;
        };
        response.cancel.cancel();
        response.cut = true;
        self.ctx.send(CallServerEvent::OutputAudioClear {
            response_id: response.id.clone(),
        });
        self.endpointer.set_playback(false);
        let response = self.response.take().expect("checked above");
        if !response.generating {
            // The reply had finished generating and was only still being
            // spoken, so no task is left to cut it back: do it here.
            let heard = response.shared.lock().heard(self.client_reports);
            let runtime = Arc::clone(&self.ctx.runtime);
            let ctx = self.ctx.clone();
            let id = response.id.clone();
            let previous = self.finishing.take();
            self.finishing = Some(tokio::spawn(async move {
                if let Some(previous) = previous {
                    let _ = previous.await;
                }
                if runtime.truncate_interrupted_reply(&heard).await {
                    ctx.send(CallServerEvent::ItemTruncated {
                        response_id: id,
                        text: heard,
                    });
                }
            }));
        }
        // A still-generating response truncates itself when it sees the
        // cancel; its `Finished` still arrives and is reported then.
    }

    fn start_response(&mut self, trigger: Trigger, speech_end: Option<Instant>) {
        if self.response.is_some() {
            self.barge_in();
        }
        let id = format!("resp_{}", Uuid::new_v4().simple());
        let cancel = CancellationToken::new();
        let shared = Arc::new(parking_lot::Mutex::new(Shared::default()));
        self.response = Some(ActiveResponse {
            id: id.clone(),
            cancel: cancel.clone(),
            shared: Arc::clone(&shared),
            generating: true,
            playing: false,
            cut: false,
        });
        self.set_state(CallState::Thinking);
        let ctx = self.ctx.clone();
        let previous = self.finishing.take();
        let client_reports = self.client_reports;
        let task = tokio::spawn(async move {
            if let Some(previous) = previous {
                let _ = previous.await;
            }
            let finished = respond(
                &ctx,
                id,
                trigger,
                cancel,
                shared,
                speech_end,
                client_reports,
            )
            .await;
            let _ = ctx.internal.send(Internal::Finished(Box::new(finished)));
        });
        self.finishing = Some(task);
    }

    async fn control(&mut self, text: &str) {
        let Ok(event) = serde_json::from_str::<CallClientEvent>(text) else {
            self.ctx.send(CallServerEvent::Error {
                message: "unrecognised control frame".into(),
            });
            return;
        };
        match event {
            CallClientEvent::Auth { .. } => {}
            CallClientEvent::Ping => self.ctx.send(CallServerEvent::Pong),
            CallClientEvent::SessionUpdate { profile } => self.ctx.profile = profile,
            CallClientEvent::ResponseCancel => self.barge_in(),
            CallClientEvent::OutputAudioStarted { response_id, index } => {
                self.client_reports = true;
                if let Some(response) = self.response.as_ref().filter(|r| r.id == response_id) {
                    let mut shared = response.shared.lock();
                    shared.last_started = Some(shared.last_started.map_or(index, |i| i.max(index)));
                }
            }
            CallClientEvent::OutputAudioIdle { response_id } => {
                self.client_reports = true;
                self.playback_idle(&response_id);
            }
            CallClientEvent::ActionResolve { id, approve } => {
                if self.pending_card != Some(id) {
                    self.ctx.send(CallServerEvent::Error {
                        message: "no such approval card is waiting".into(),
                    });
                    return;
                }
                self.start_response(Trigger::Resolve { id, approve }, None);
            }
        }
    }

    async fn internal(&mut self, internal: Internal) {
        match internal {
            Internal::Partial { utterance, text } => {
                self.partial_in_flight = false;
                if utterance == self.utterance && self.endpointer.in_turn() && !text.is_empty() {
                    self.ctx
                        .send(CallServerEvent::TranscriptionPartial { text });
                }
            }
            Internal::AudioStarted { response_id } => {
                if let Some(response) = self.response.as_mut().filter(|r| r.id == response_id) {
                    response.playing = true;
                    self.endpointer.set_playback(true);
                    if !self.endpointer.in_turn() {
                        self.set_state(CallState::Speaking);
                    }
                }
            }
            Internal::Finished(finished) => self.finished(*finished),
        }
    }

    fn finished(&mut self, finished: Finished) {
        if let Some((id, outcome)) = finished.resolved {
            if self.pending_card == Some(id) {
                self.pending_card = None;
            }
            if let Some(approved) = outcome {
                self.ctx
                    .send(CallServerEvent::ActionResolved { id, approved });
            }
        }
        if let Some(action) = finished.pending {
            self.pending_card = Some(action.id());
            self.ctx.send(CallServerEvent::PendingAction { action });
        }
        if let Some(status) = finished.status {
            let m = &finished.metrics;
            tracing::info!(
                target: "vogt::call",
                response_id = %finished.response_id,
                status = ?status,
                endpoint_ms = m.endpoint_ms,
                stt_ms = m.stt_ms,
                llm_first_text_ms = m.llm_first_text_ms,
                tts_first_ms = m.tts_first_ms,
                speech_end_to_first_audio_ms = m.speech_end_to_first_audio_ms,
                tool_rounds = m.tool_rounds,
                filler = m.filler,
                "call response"
            );
            self.ctx.send(CallServerEvent::ResponseDone {
                response_id: finished.response_id.clone(),
                status,
                text: finished.text,
                metrics: finished.metrics,
            });
        }
        let ours = self
            .response
            .as_ref()
            .is_some_and(|r| r.id == finished.response_id);
        if ours {
            let response = self.response.as_mut().expect("checked above");
            response.generating = false;
            if !response.playing {
                self.response = None;
            }
        }
        if !self.endpointer.in_turn() {
            self.restore_state();
        }
    }

    fn hang_up(&mut self) {
        if let Some(response) = self.response.take() {
            response.cancel.cancel();
        }
        if let Some(eager) = self.eager.take() {
            eager.abort();
        }
    }
}

/// Transcribe a turn, or `None` if it said nothing a person would call words.
async fn transcribe(speech: &AssistantSpeech, wav: Vec<u8>) -> Option<String> {
    let text = speech
        .transcribe(wav, "turn.wav", "audio/wav", None)
        .await
        .ok()?;
    let text = text.trim().to_string();
    (!is_non_speech(&text)).then_some(text)
}

/// Whisper-family transcribers describe what they heard when it was not
/// speech — `[BLANK_AUDIO]`, `(silence)`, `[Music]` — and a turn made of
/// nothing else is no turn.
fn is_non_speech(text: &str) -> bool {
    let mut rest = text.trim();
    loop {
        rest = rest.trim_start();
        let close = match rest.chars().next() {
            Some('[') => ']',
            Some('(') => ')',
            Some('*') => '*',
            _ => break,
        };
        match rest[1..].find(close) {
            Some(end) => rest = &rest[end + 2..],
            None => break,
        }
    }
    !rest.chars().any(char::is_alphanumeric)
}

fn ms_between(start: Option<Instant>, end: Option<Instant>) -> Option<u64> {
    match (start, end) {
        (Some(start), Some(end)) if end >= start => Some((end - start).as_millis() as u64),
        _ => None,
    }
}

/// One piece to speak.
struct Piece {
    text: String,
    /// Where it ends in the recorded reply text, if it is part of it.
    end: Option<usize>,
    /// Already synthesized (a fixed line).
    clip: Option<SpeechClip>,
}

/// Times the response task records, for `CallMetrics`.
#[derive(Default)]
struct Times {
    speech_end: Option<Instant>,
    end_of_turn: Option<Instant>,
    transcript: Option<Instant>,
    first_text: Option<Instant>,
    first_piece: Option<Instant>,
}

/// Run one response: transcribe (for speech), then the assistant turn,
/// spoken as it streams.
async fn respond(
    ctx: &Ctx,
    response_id: String,
    trigger: Trigger,
    cancel: CancellationToken,
    shared: Arc<parking_lot::Mutex<Shared>>,
    speech_end: Option<Instant>,
    client_reports: bool,
) -> Finished {
    let mut times = Times {
        speech_end,
        end_of_turn: speech_end.map(|_| Instant::now()),
        ..Times::default()
    };
    let mut finished = Finished {
        response_id: response_id.clone(),
        status: None,
        text: None,
        pending: None,
        resolved: None,
        metrics: CallMetrics::default(),
    };

    // 1. What was said, for a spoken turn; which card, for a button.
    let (transcript, resolve) = match trigger {
        Trigger::Speech { wav, eager } => {
            let text = tokio::select! {
                _ = cancel.cancelled() => return finished,
                text = async {
                    match eager {
                        Some(eager) => match eager.await {
                            Ok(Some(text)) => Some(text),
                            _ => transcribe(&ctx.speech, wav).await,
                        },
                        None => transcribe(&ctx.speech, wav).await,
                    }
                } => text,
            };
            times.transcript = Some(Instant::now());
            let Some(text) = text else {
                return finished;
            };
            ctx.send(CallServerEvent::TranscriptionCompleted { text: text.clone() });
            (Some(text), None)
        }
        Trigger::Resolve { id, approve } => (None, Some((id, approve))),
    };

    ctx.send(CallServerEvent::ResponseCreated {
        response_id: response_id.clone(),
    });

    // 2. The speaker: synthesizes pieces in order as they come, and sends
    // each the moment its audio exists.
    let (piece_tx, piece_rx) = mpsc::unbounded_channel::<Piece>();
    let speaker = tokio::spawn(speak(
        ctx.clone(),
        response_id.clone(),
        piece_rx,
        cancel.clone(),
        Arc::clone(&shared),
    ));

    // A spoken turn while a card waits is answered without the model: the
    // card is approved by its button and by nothing else.
    if let Some(text) = &transcript {
        if let Some(card) = ctx.runtime.pending_action().await {
            ctx.runtime.record_held_utterance(&ctx.caller, text).await;
            // The card may have come from a typed turn on another screen;
            // this one shows it too.
            finished.pending = contract_action(&card);
            let clip = ctx.canned(APPROVAL_REMINDER).await;
            let _ = piece_tx.send(Piece {
                text: APPROVAL_REMINDER.to_string(),
                end: None,
                clip,
            });
            drop(piece_tx);
            let _ = speaker.await;
            finished.status = Some(if cancel.is_cancelled() {
                CallResponseStatus::Interrupted
            } else {
                CallResponseStatus::Completed
            });
            finished.text = Some(APPROVAL_REMINDER.to_string());
            finished.metrics = metrics(&times, &shared);
            return finished;
        }
    }

    // 3. The turn, streamed.
    let (event_tx, mut event_rx) = mpsc::unbounded_channel::<TurnEvent>();
    let stream = TurnStream {
        sink: Arc::new(move |event| {
            let _ = event_tx.send(event);
        }),
        cancel: cancel.clone(),
        spoken: true,
    };
    let runtime = Arc::clone(&ctx.runtime);
    let caller = ctx.caller.clone();
    let profile = ctx.profile.clone();
    let turn = async move {
        match (transcript, resolve) {
            (Some(text), _) => {
                runtime
                    .handle_message_streamed(caller, text.clone(), Some(text), profile, &stream)
                    .await
            }
            (None, Some((id, approve))) => {
                runtime
                    .resolve_action_streamed(caller, id, approve, &stream)
                    .await
            }
            (None, None) => Err(ApiError::BadRequest("nothing to respond to".into())),
        }
    };
    tokio::pin!(turn);

    let mut chunker = SentenceChunker::new();
    let mut offset = 0usize;
    let mut spoke_anything = false;
    let mut on_event = |event: TurnEvent,
                        times: &mut Times,
                        spoke_anything: &mut bool,
                        metrics_rounds: &mut u32| {
        match event {
            TurnEvent::TextDelta(delta) => {
                times.first_text.get_or_insert_with(Instant::now);
                ctx.send(CallServerEvent::ResponseTextDelta {
                    response_id: response_id.clone(),
                    delta: delta.clone(),
                });
                shared.lock().reply_text.push_str(&delta);
                for piece in chunker.push(&delta) {
                    let end = piece_end(&shared.lock().reply_text, &piece, &mut offset);
                    times.first_piece.get_or_insert_with(Instant::now);
                    *spoke_anything = true;
                    let _ = piece_tx.send(Piece {
                        text: piece,
                        end,
                        clip: None,
                    });
                }
            }
            TurnEvent::ToolRound { .. } => {
                *metrics_rounds += 1;
                // Whatever the model said before asking for tools is spoken,
                // but it is not part of the reply the conversation records:
                // that is only what it writes after its last round.
                if let Some(piece) = chunker.finish() {
                    *spoke_anything = true;
                    let _ = piece_tx.send(Piece {
                        text: piece,
                        end: None,
                        clip: None,
                    });
                }
                shared.lock().reply_text.clear();
                offset = 0;
                if !*spoke_anything && !ctx.filler.is_empty() {
                    *spoke_anything = true;
                    shared.lock().metrics.filler = true;
                    let _ = piece_tx.send(Piece {
                        text: ctx.filler.clone(),
                        end: None,
                        clip: None,
                    });
                }
            }
        }
    };
    let mut tool_rounds = 0u32;
    let result = loop {
        tokio::select! {
            biased;
            Some(event) = event_rx.recv() => {
                on_event(event, &mut times, &mut spoke_anything, &mut tool_rounds);
            }
            result = &mut turn => {
                while let Ok(event) = event_rx.try_recv() {
                    on_event(event, &mut times, &mut spoke_anything, &mut tool_rounds);
                }
                break result;
            }
        }
    };
    // The last piece, and what the turn ended on.
    drop(on_event);
    let last = chunker.finish();
    if let Some(piece) = last {
        let end = piece_end(&shared.lock().reply_text, &piece, &mut offset);
        times.first_piece.get_or_insert_with(Instant::now);
        spoke_anything = true;
        let _ = piece_tx.send(Piece {
            text: piece,
            end,
            clip: None,
        });
    }
    finished.resolved = resolve.map(|(id, approve)| (id, result.is_ok().then_some(approve)));
    let status = match &result {
        Ok(reply) if reply.pending_action.is_some() => {
            if !spoke_anything {
                let _ = piece_tx.send(Piece {
                    text: PROPOSED_LINE.to_string(),
                    end: None,
                    clip: None,
                });
            }
            finished.pending = reply.pending_action.as_ref().and_then(contract_action);
            CallResponseStatus::PendingApproval
        }
        Ok(reply) if reply.interrupted => CallResponseStatus::Interrupted,
        Ok(_) => CallResponseStatus::Completed,
        Err(e) => {
            tracing::warn!(target: "vogt::call", "call turn failed: {e}");
            ctx.send(CallServerEvent::Error {
                message: e.to_string(),
            });
            if !spoke_anything {
                let _ = piece_tx.send(Piece {
                    text: FAILED_LINE.to_string(),
                    end: None,
                    clip: None,
                });
            }
            CallResponseStatus::Failed
        }
    };
    drop(piece_tx);
    let _ = speaker.await;
    shared.lock().metrics.tool_rounds = tool_rounds;

    // 4. A cut response keeps only what was heard.
    let recorded = result.ok().and_then(|reply: AssistantReply| reply.reply);
    if cancel.is_cancelled() {
        let heard = shared.lock().heard(client_reports);
        if recorded.is_some() && ctx.runtime.truncate_interrupted_reply(&heard).await {
            finished.text = Some(heard);
        }
        finished.status = Some(CallResponseStatus::Interrupted);
    } else {
        finished.text = recorded;
        finished.status = Some(status);
    }
    finished.metrics = metrics(&times, &shared);
    finished
}

/// How long `text` takes to say, for a clip whose container does not say:
/// about fifteen characters a second.
fn spoken_length(text: &str) -> Duration {
    Duration::from_millis(400 + text.chars().count() as u64 * 1000 / 15)
}

/// Where `piece` ends in `text`, searching from `offset` and moving it on.
/// Pieces are trimmed, so each is found rather than counted.
fn piece_end(text: &str, piece: &str, offset: &mut usize) -> Option<usize> {
    let start = (*offset).min(text.len());
    let found = text[start..].find(piece)?;
    let end = start + found + piece.len();
    *offset = end;
    Some(end)
}

/// The server's card, in the contract's shape — the same JSON on the wire.
fn contract_action(view: &PendingActionView) -> Option<PendingAction> {
    serde_json::to_value(view)
        .ok()
        .and_then(|value| serde_json::from_value(value).ok())
}

fn metrics(times: &Times, shared: &parking_lot::Mutex<Shared>) -> CallMetrics {
    let shared = shared.lock();
    let mut metrics = shared.metrics.clone();
    metrics.endpoint_ms = ms_between(times.speech_end, times.end_of_turn);
    metrics.stt_ms = ms_between(times.end_of_turn, times.transcript);
    metrics.llm_first_text_ms = ms_between(times.transcript, times.first_text);
    metrics.speech_end_to_first_audio_ms = ms_between(times.speech_end, shared.first_audio);
    metrics
}

/// Synthesize and send pieces in order until the channel closes or the
/// response is cut.
async fn speak(
    ctx: Ctx,
    response_id: String,
    mut pieces: mpsc::UnboundedReceiver<Piece>,
    cancel: CancellationToken,
    shared: Arc<parking_lot::Mutex<Shared>>,
) {
    let mut index = 0u32;
    loop {
        let piece = tokio::select! {
            _ = cancel.cancelled() => return,
            piece = pieces.recv() => match piece {
                Some(piece) => piece,
                None => return,
            },
        };
        let text = speakable(&piece.text);
        if text.is_empty() {
            continue;
        }
        let queued = Instant::now();
        let clip = match piece.clip {
            Some(clip) => Some(clip),
            None if piece.end.is_none() && piece.text == ctx.filler => ctx.canned(&text).await,
            None => tokio::select! {
                _ = cancel.cancelled() => return,
                clip = ctx.speech.synthesize(&text) => clip.ok(),
            },
        };
        let Some(clip) = clip else {
            tracing::debug!(target: "vogt::call", "a reply piece could not be synthesized");
            continue;
        };
        if cancel.is_cancelled() {
            return;
        }
        let first = {
            let mut shared = shared.lock();
            let now = Instant::now();
            let first = !shared.audio_sent;
            if first {
                shared.audio_sent = true;
                shared.first_audio = Some(now);
                shared.metrics.tts_first_ms = Some(queued.elapsed().as_millis() as u64);
            }
            shared.piece_ends.push(piece.end);
            let start = shared.estimated_end.filter(|end| *end > now).unwrap_or(now);
            let length = wav_duration(&clip.bytes).unwrap_or_else(|| spoken_length(&text));
            shared.estimated_starts.push(start);
            shared.estimated_end = Some(start + length);
            first
        };
        if first {
            let _ = ctx.internal.send(Internal::AudioStarted {
                response_id: response_id.clone(),
            });
        }
        ctx.send(CallServerEvent::ResponseAudioStart {
            response_id: response_id.clone(),
            index,
            text,
            content_type: clip.content_type.clone(),
            bytes: clip.bytes.len() as u64,
        });
        let _ = ctx.out.send(Outbound::Audio(clip.bytes));
        index += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transcriber_noise_labels_are_not_a_turn() {
        for text in [
            "",
            "  ",
            "[BLANK_AUDIO]",
            "(silence)",
            "[Music] (wind)",
            "*coughs*",
            "...",
        ] {
            assert!(is_non_speech(text), "{text:?}");
        }
        for text in ["yes", "[laughs] okay then", "(um) stop"] {
            assert!(!is_non_speech(text), "{text:?}");
        }
    }

    #[test]
    fn pieces_are_found_in_order_even_when_they_repeat() {
        let text = "Yes. Yes. Done.";
        let mut offset = 0;
        assert_eq!(piece_end(text, "Yes.", &mut offset), Some(4));
        assert_eq!(piece_end(text, "Yes.", &mut offset), Some(9));
        assert_eq!(piece_end(text, "Done.", &mut offset), Some(15));
        assert_eq!(piece_end(text, "Missing.", &mut offset), None);
    }

    fn shared_with(pieces: &[Option<usize>], text: &str) -> Shared {
        Shared {
            piece_ends: pieces.to_vec(),
            reply_text: text.to_string(),
            ..Shared::default()
        }
    }

    #[test]
    fn what_was_heard_ends_at_the_last_piece_that_started_playing() {
        let mut shared = shared_with(&[None, Some(6), Some(14)], "First. Second.");
        assert_eq!(shared.heard(true), "", "nothing reported, nothing heard");
        shared.last_started = Some(0);
        assert_eq!(shared.heard(true), "", "only the filler had started");
        shared.last_started = Some(1);
        assert_eq!(shared.heard(true), "First.");
        shared.last_started = Some(2);
        assert_eq!(shared.heard(true), "First. Second.");
    }

    #[test]
    fn a_client_that_reports_nothing_is_reckoned_by_the_clock() {
        let now = Instant::now();
        let mut shared = shared_with(&[Some(6), Some(14)], "First. Second.");
        shared.estimated_starts = vec![now - Duration::from_secs(1), now + Duration::from_secs(5)];
        assert_eq!(shared.heard(false), "First.");
    }
}
