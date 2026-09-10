// Hands-free conversation, as a pure state machine (WI-174 v1).
//
// The Assistant surface already has spoken replies and a hold/tap microphone;
// what it lacks is anything that keeps *listening between turns*. This module is
// that loop, and only that loop: speak → go quiet → the turn sends → the reply
// is spoken → the mic re-opens → no touch between turns.
//
// It is deliberately backend-agnostic and side-effect-free. Every effect — open
// the mic, close it, send a turn, speak a reply — is a *port* the host supplies
// (`Assistant.tsx` on Android's native recognizer, Web Speech on the desktop,
// or server STT). The machine only decides *when*. That is what makes the loop
// testable without a recognizer, a synth, or a DOM, and what lets a different
// backend — a duplex realtime session, say — drop in behind the same ports
// later without touching the transitions here.
//
// There is no open standard for hands-free turn-taking; the shape below (a
// VAD/endpointing loop, an idle timeout, a mute that keeps the session alive, a
// listening/thinking/speaking state machine) is the de facto one shared by
// OpenAI Realtime, Gemini Live, Grok Voice Agent, LiveKit, Pipecat and Open
// WebUI Call mode. The config keys and their names mirror the OpenAI Realtime
// vocabulary so a Realtime-shaped backend can adopt them unrenamed.
//
// v1 is half-duplex: the mic is closed while a reply plays and re-opens when it
// finishes (the Android WebView has no echo-safe capture during playback, so
// barge-in — `interrupt_response` — is designed-for but off; see
// `docs/local/VOICE_HANDSFREE_DESIGN.md`).

/** The phases of a hands-free session. `muted` is orthogonal to these — a
 *  session stays in its phase while muted, it just stops capturing. */
export type VoiceState =
  | "idle" // not in a conversation
  | "arming" // opening the mic for a turn; not capturing yet
  | "listening" // capturing a turn
  | "endpointing" // the turn ended (silence or the recognizer stopped); settling before send
  | "sending" // the turn is with the engine, awaiting a reply
  | "speaking" // a reply is playing; the mic is closed (v1 half-duplex)
  | "paused_for_approval" // a reply carried a pending action; mic closed until it is resolved on screen
  | "ended"; // the conversation is over; `reason` says why

/** Why a conversation ended, for the status chip and for the caller. */
export type EndReason =
  | "user" // turned off, or the surface was left
  | "idle" // no speech for `idle_timeout_ms`
  | "empty_turns" // `max_empty_turns` in a row heard nothing
  | "no_backend" // STT or TTS went away mid-session
  | "error"; // an unrecoverable fault

/** Turn-detection and lifetime tuning, in the OpenAI Realtime vocabulary.
 *  Read from `vogt.assistant.voice.*` localStorage so any deployment can tune a
 *  session without a rebuild; the same `silence_duration_ms` /
 *  `final_result_grace_ms` the single-turn tap (WI-173) already reads. */
export interface VoiceConfig {
  /** Quiet after the last transcript change that ends a turn. Field numbers:
   *  OpenAI 500, Gemini ~800, LiveKit 550, voicemode 1000, Open WebUI 2000. On
   *  the Android recogniser (whose dictation-mode end-of-speech is late) JS
   *  owns this; 1000 is the honest default there, ~800 with a real VAD. */
  silence_duration_ms: number;
  /** Grace after the recogniser's own stop, because the plugin emits `stopped`
   *  before the final (best) result — one more partial. */
  final_result_grace_ms: number;
  /** A hard ceiling on a single turn, so a stuck-open recogniser still ends. */
  max_turn_ms: number;
  /** No speech for this long ends the whole session. Every stack has one. */
  idle_timeout_ms: number;
  /** This many turns in a row that heard nothing ends the session. */
  max_empty_turns: number;
  /** Barge-in: interrupt a playing reply when the speaker starts. Off in v1
   *  (no echo-safe capture on the WebView path); the machine is built so it can
   *  turn on in v2 without a transition change. */
  interrupt_response: boolean;
}

export const VOICE_CONFIG_DEFAULTS: VoiceConfig = {
  silence_duration_ms: 1000,
  final_result_grace_ms: 300,
  max_turn_ms: 30_000,
  idle_timeout_ms: 60_000,
  max_empty_turns: 3,
  interrupt_response: false,
};

const VOICE_CFG_PREFIX = "vogt.assistant.voice.";

/** Read the config, applying localStorage overrides over the generic defaults.
 *  A missing or malformed value keeps the default rather than throwing — the
 *  same defensive read the tap path uses, because a locked-down browser makes
 *  even `getItem` throw. Injectable for tests. */
export function readVoiceConfig(
  read: (key: string) => string | null = (key) => {
    try {
      return localStorage.getItem(key);
    } catch {
      return null;
    }
  },
): VoiceConfig {
  const num = (key: keyof VoiceConfig, fallback: number): number => {
    const raw = read(VOICE_CFG_PREFIX + key);
    if (raw === null) return fallback;
    const n = Number(raw);
    return Number.isFinite(n) && n >= 0 ? n : fallback;
  };
  const bool = (key: keyof VoiceConfig, fallback: boolean): boolean => {
    const raw = read(VOICE_CFG_PREFIX + key);
    if (raw === null) return fallback;
    return raw === "1" || raw === "true";
  };
  return {
    silence_duration_ms: num("silence_duration_ms", VOICE_CONFIG_DEFAULTS.silence_duration_ms),
    final_result_grace_ms: num("final_result_grace_ms", VOICE_CONFIG_DEFAULTS.final_result_grace_ms),
    max_turn_ms: num("max_turn_ms", VOICE_CONFIG_DEFAULTS.max_turn_ms),
    idle_timeout_ms: num("idle_timeout_ms", VOICE_CONFIG_DEFAULTS.idle_timeout_ms),
    max_empty_turns: num("max_empty_turns", VOICE_CONFIG_DEFAULTS.max_empty_turns),
    interrupt_response: bool("interrupt_response", VOICE_CONFIG_DEFAULTS.interrupt_response),
  };
}

/** A reply the engine returned for a sent turn, reduced to the two facts the
 *  loop turns on: whether there is something to speak, and whether it carried a
 *  pending action that must be approved on screen before the loop goes on. */
export interface VoiceReply {
  /** The spoken text, or null/empty when the reply was an action only. */
  text: string | null;
  /** A `pending_action` was attached: the loop pauses, mic closed, until the
   *  on-screen approve/deny — voice never approves anything (ENGINE.md §6). */
  hasPendingAction: boolean;
}

/** The effects the machine asks the host to perform. Every one is a command,
 *  never a query: the machine holds no reference to a recogniser or a synth. */
export interface VoicePorts {
  /** Begin capturing a turn. The host confirms the recogniser is live by
   *  calling `micReady()`; a capture failure is reported via `end("error")`. */
  openMic(): void;
  /** Stop capturing. Idempotent — called on every turn end and on mute. */
  closeMic(): void;
  /** Send the captured turn's text to the engine. The host reports the outcome
   *  through `replied()` or `sendFailed()`. */
  sendTurn(text: string): void;
  /** Speak a reply. The host calls `speechFinished()` when playback ends (or at
   *  once when there is nothing to play). */
  speak(text: string): void;
  /** Stop any reply currently playing (session end, barge-in in v2). */
  stopSpeaking(): void;
  /** State changed — for the status chip. Called with the new phase and the
   *  live mute flag on every transition. */
  onChange(state: VoiceState, muted: boolean): void;
  /** The conversation ended; `reason` is why. The host turns the toggle off. */
  onEnded(reason: EndReason): void;
}

/** A tiny timer seam so tests drive time deterministically. Real one below. */
export interface Scheduler {
  set(fn: () => void, ms: number): number;
  clear(id: number): void;
}

export const realScheduler: Scheduler = {
  set: (fn, ms) => setTimeout(fn, ms) as unknown as number,
  clear: (id) => clearTimeout(id),
};

/**
 * The hands-free loop. Constructed with its ports; driven by the host calling
 * the event methods (`begin`, `micReady`, `partial`, `recognizerStopped`,
 * `replied`, `speechFinished`, `sendFailed`, `toggleMute`, `end`). It performs
 * no I/O of its own beyond the injected scheduler.
 */
export class VoiceConversation {
  private state: VoiceState = "idle";
  private muted = false;
  private text = ""; // the current turn's best transcript so far
  private emptyTurns = 0;
  private silenceTimer: number | null = null;
  private maxTurnTimer: number | null = null;
  private idleTimer: number | null = null;
  // A release/stop that arrived while the mic was still arming: applied on
  // `micReady`, never against a recogniser that is not up yet (the WI-173 race,
  // at the loop's altitude).
  private endRequestedWhileArming: EndReason | null = null;
  // True while we are closing the mic ourselves. Web Speech's `stop()` fires
  // `onend` (which the host forwards as `recognizerStopped`); a synchronous one
  // would otherwise re-enter the turn end we are already in — a double send.
  private closingMic = false;

  constructor(
    private readonly ports: VoicePorts,
    private readonly cfg: VoiceConfig = readVoiceConfig(),
    private readonly clock: Scheduler = realScheduler,
  ) {}

  getState(): VoiceState {
    return this.state;
  }
  isMuted(): boolean {
    return this.muted;
  }
  /** In a live conversation (not idle, not ended). */
  isActive(): boolean {
    return this.state !== "idle" && this.state !== "ended";
  }

  // -- events the host feeds in ------------------------------------------

  /** The user turned Conversation on. */
  begin(): void {
    if (this.isActive()) return;
    this.muted = false;
    this.emptyTurns = 0;
    this.endRequestedWhileArming = null;
    this.arm();
  }

  /** The recogniser is live and capturing (host confirms `openMic` succeeded). */
  micReady(): void {
    if (this.state !== "arming") return;
    if (this.endRequestedWhileArming) {
      const reason = this.endRequestedWhileArming;
      this.endRequestedWhileArming = null;
      // Out of `arming` first, so `end` runs its real teardown rather than
      // re-deferring: the recogniser is up now, so closing it is correct.
      this.state = "listening";
      this.end(reason);
      return;
    }
    this.text = "";
    this.enter("listening");
    // The turn's hard ceiling, and the session idle clock, both start now.
    this.armMaxTurn();
    this.armIdle();
  }

  /** A partial transcript arrived. Captures the best text and rearms the turn's
   *  silence clock; any speech also resets the session idle clock. */
  partial(text: string): void {
    if (this.muted) return;
    if (this.state !== "listening" && this.state !== "endpointing" && this.state !== "arming") {
      return;
    }
    // A partial can beat `micReady` on a fast device; treat it as listening.
    if (this.state === "arming") {
      this.text = "";
      this.enter("listening");
      this.armMaxTurn();
    }
    if (text.trim()) this.text = text.trim();
    // In the grace window after the recogniser's own stop, a late partial is
    // the final (best) result: capture it and let the grace timer fire. Do not
    // rearm the long silence window — the turn has already ended.
    if (this.state === "endpointing") return;
    this.armSilence();
    this.armIdle();
  }

  /** The recogniser stopped on its own — the fallback turn end. Waits out the
   *  grace so the final result (one more partial after `stopped`) is included. */
  recognizerStopped(): void {
    // Not a real end if we are the ones closing the mic (turn already ending).
    if (this.closingMic) return;
    if (this.muted) return;
    if (this.state !== "listening" && this.state !== "endpointing") return;
    this.enter("endpointing");
    this.clearSilence();
    this.silenceTimer = this.clock.set(() => this.endpoint(), this.cfg.final_result_grace_ms);
  }

  /** The engine answered a sent turn (or a resolved approval). */
  replied(reply: VoiceReply): void {
    if (this.state !== "sending" && this.state !== "paused_for_approval") return;
    if (reply.hasPendingAction) {
      // Announce (the host speaks it) and wait for the on-screen decision. No
      // mic: a misheard "yes" must authorise nothing.
      this.enter("paused_for_approval");
      if (reply.text && reply.text.trim()) this.ports.speak(reply.text);
      return;
    }
    if (reply.text && reply.text.trim()) {
      this.enter("speaking");
      this.ports.speak(reply.text);
      return;
    }
    // Nothing to say — straight back to listening.
    this.resumeListening();
  }

  /** A reply finished playing (or there was nothing to play). Half-duplex: the
   *  mic re-opens now. */
  speechFinished(): void {
    if (this.state !== "speaking") return;
    this.resumeListening();
  }

  /** Barge-in (v2): the speaker started talking over a playing reply. Only
   *  honoured when `interrupt_response` is on and a reply is actually playing —
   *  the host runs an echo-cancelled VAD during `speaking` and calls this on a
   *  confident onset. Halts the reply and re-opens the mic at once, so the
   *  interrupting words are captured as the next turn. A no-op otherwise, so a
   *  false onset outside playback cannot disturb the loop. The reply's own
   *  `speechFinished` that follows the halt is ignored (we have left `speaking`). */
  speechDetected(): void {
    if (!this.cfg.interrupt_response) return;
    if (this.state !== "speaking") return;
    this.ports.stopSpeaking();
    this.resumeListening();
  }

  /** The turn's send failed. The mode stays (the surface shows the failed
   *  bubble + Retry); the loop re-opens the mic for another turn. */
  sendFailed(): void {
    if (this.state !== "sending") return;
    this.resumeListening();
  }

  /** Toggle mute. Muting keeps the session alive but stops capture; unmuting
   *  re-opens the mic if the session is in a capture phase. */
  toggleMute(): void {
    if (!this.isActive()) return;
    this.muted = !this.muted;
    if (this.muted) {
      this.clearSilence();
      this.clearMaxTurn();
      this.closeMic();
      // The idle clock keeps running while muted: a muted session left forever
      // still ends.
      this.emit();
      return;
    }
    // Unmuted: if we were in a capture phase, start a fresh turn.
    if (this.state === "listening" || this.state === "endpointing") {
      this.arm();
    } else {
      this.emit();
    }
  }

  /** End the conversation. Safe from any state; idempotent once ended. */
  end(reason: EndReason = "user"): void {
    if (this.state === "ended" || this.state === "idle") {
      // Nothing live, but still surface the reason for a caller that asked.
      if (this.state !== "ended") {
        this.state = "ended";
        this.ports.onEnded(reason);
      }
      return;
    }
    if (this.state === "arming") {
      // The mic is still coming up; remember the end and apply it at micReady,
      // rather than closing a recogniser that is not up yet.
      this.endRequestedWhileArming = reason;
      return;
    }
    this.clearAll();
    this.closeMic();
    this.ports.stopSpeaking();
    this.state = "ended";
    this.ports.onChange(this.state, this.muted);
    this.ports.onEnded(reason);
  }

  // -- internal transitions ----------------------------------------------

  /** Open the mic for a turn (or wait, if muted). */
  private arm(): void {
    this.enter("arming");
    this.armIdle();
    if (this.muted) return; // a muted session sits armed until unmuted
    this.ports.openMic();
  }

  /** The turn ended: send what was heard, or count an empty turn. */
  private endpoint(): void {
    this.clearSilence();
    this.clearMaxTurn();
    const heard = this.text.trim();
    if (heard) {
      this.emptyTurns = 0;
      // Enter `sending` first, then close: an induced stop reads as sending, not
      // as another turn to end.
      this.enter("sending");
      this.closeMic();
      this.ports.sendTurn(heard);
      return;
    }
    // Heard nothing this turn.
    this.emptyTurns += 1;
    if (this.emptyTurns >= this.cfg.max_empty_turns) {
      this.end("empty_turns");
      return;
    }
    // Try again: a fresh turn on the same open session.
    this.closeMic();
    this.arm();
  }

  /** Reopen the mic after a reply (or a no-op reply / failed send). */
  private resumeListening(): void {
    this.text = "";
    this.arm();
  }

  private enter(state: VoiceState): void {
    this.state = state;
    this.emit();
  }

  private emit(): void {
    this.ports.onChange(this.state, this.muted);
  }

  /** Close the mic, fenced so a synchronous `onend` → `recognizerStopped` from
   *  our own stop cannot re-enter the turn end. */
  private closeMic(): void {
    this.closingMic = true;
    try {
      this.ports.closeMic();
    } finally {
      this.closingMic = false;
    }
  }

  private armSilence(): void {
    this.clearSilence();
    this.silenceTimer = this.clock.set(() => this.endpoint(), this.cfg.silence_duration_ms);
  }
  private clearSilence(): void {
    if (this.silenceTimer !== null) {
      this.clock.clear(this.silenceTimer);
      this.silenceTimer = null;
    }
  }
  private armMaxTurn(): void {
    this.clearMaxTurn();
    this.maxTurnTimer = this.clock.set(() => this.endpoint(), this.cfg.max_turn_ms);
  }
  private clearMaxTurn(): void {
    if (this.maxTurnTimer !== null) {
      this.clock.clear(this.maxTurnTimer);
      this.maxTurnTimer = null;
    }
  }
  private armIdle(): void {
    this.clearIdle();
    this.idleTimer = this.clock.set(() => this.end("idle"), this.cfg.idle_timeout_ms);
  }
  private clearIdle(): void {
    if (this.idleTimer !== null) {
      this.clock.clear(this.idleTimer);
      this.idleTimer = null;
    }
  }
  private clearAll(): void {
    this.clearSilence();
    this.clearMaxTurn();
    this.clearIdle();
  }
}

/** A short, human label for the status chip, given the phase and mute flag. */
export function voiceStatusLabel(state: VoiceState, muted: boolean): string {
  if (muted && (state === "listening" || state === "arming" || state === "endpointing")) {
    return "Muted";
  }
  switch (state) {
    case "arming":
    case "listening":
      return "Listening…";
    case "endpointing":
      return "Listening…";
    case "sending":
      return "Sending";
    case "speaking":
      return "Speaking";
    case "paused_for_approval":
      return "Paused for approval";
    case "ended":
      return "Ended";
    default:
      return "";
  }
}
