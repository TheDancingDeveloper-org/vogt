// One live call: the microphone streaming up the call socket, the reply's
// clips queued for playback as they arrive, and the view the call screen
// renders — with every dependency injected, so the whole exchange can be
// driven in a unit test with a fake socket, microphone and speaker.
//
// The engine does the turn-taking (docs/ENGINE.md, "Live call contract");
// this side streams, plays, reports what it played, and gets out of the way:
// on `output_audio.clear` the speaker goes quiet at once.

import type { RuntimeSocket } from "./runtimeTransport";
import type { Capture, CaptureOptions } from "./callCapture";
import type { CallPlayerEvents } from "./callPlayer";
import {
  type CallClientEvent,
  type CallServerEvent,
  type CallView,
  initialCallView,
  parseCallEvent,
  reduceCall,
} from "./callProtocol";

export interface CallPlayerLike {
  enqueue(responseId: string, index: number, bytes: ArrayBuffer): void;
  clear(): void;
}

export interface CallDeps {
  /** The call socket's URL (`ws(s)://…/api/assistant/call`). */
  url: string;
  token: string;
  profile?: string;
  openSocket(url: string): RuntimeSocket;
  startCapture(options: CaptureOptions): Promise<Capture>;
  createPlayer(events: CallPlayerEvents): CallPlayerLike;
  /** Diagnostics (`diag.ts`): names and numbers only, never what was said. */
  log?: (event: string, fields: Record<string, unknown>) => void;
  /** A finished response — time to refresh the conversation view. */
  onResponseDone?: () => void;
  now?: () => number;
  setTimer?: (fn: () => void, ms: number) => ReturnType<typeof setTimeout>;
  clearTimer?: (timer: ReturnType<typeof setTimeout>) => void;
}

/** How often the socket is pinged, so an idle proxy does not drop the call. */
const PING_MS = 20_000;
/** Reconnection waits after an unexpected drop; then the call ends. */
const RECONNECT_MS = [1_000, 2_000, 4_000];
const SILENT_FRAME = new ArrayBuffer(640);
const OPEN = 1;

export class CallSession {
  private view: CallView = initialCallView();
  private muted = false;
  private socket: RuntimeSocket | null = null;
  private capture: Capture | null = null;
  private player: CallPlayerLike;
  /** The `response.audio.start` whose binary frame comes next. */
  private pendingAudio: { response_id: string; index: number } | null = null;
  private live = false;
  private ended = false;
  private attempts = 0;
  private pingTimer: ReturnType<typeof setTimeout> | null = null;
  private endOfTurnMs = 700;
  private speechStoppedAt: number | null = null;
  private now: () => number;
  private setTimer: (fn: () => void, ms: number) => ReturnType<typeof setTimeout>;
  private clearTimer: (timer: ReturnType<typeof setTimeout>) => void;

  constructor(
    private readonly deps: CallDeps,
    private readonly onChange: (view: CallView, muted: boolean) => void,
  ) {
    this.now = deps.now ?? (() => performance.now());
    this.setTimer = deps.setTimer ?? ((fn, ms) => setTimeout(fn, ms));
    this.clearTimer = deps.clearTimer ?? ((timer) => clearTimeout(timer));
    this.player = deps.createPlayer({
      onStarted: (responseId, index) =>
        this.send({ type: "output_audio.started", response_id: responseId, index }),
      onIdle: (responseId) => this.send({ type: "output_audio.idle", response_id: responseId }),
      onFirstAudio: () => {
        if (this.speechStoppedAt === null) return;
        // The server declared the turn over `end_of_turn_ms` after the voice
        // stopped; the first audio reaching the speaker is measured from there.
        const ms = Math.round(this.now() - this.speechStoppedAt + this.endOfTurnMs);
        this.speechStoppedAt = null;
        this.deps.log?.("call.first_audio", { speech_end_to_heard_ms: ms });
      },
      onError: (error) => this.deps.log?.("call.decode_error", { error: String(error) }),
    });
  }

  /**
   * Open the microphone, then the socket. Call from the gesture that placed
   * the call, so the permission prompt and audio start are allowed.
   */
  async start(): Promise<void> {
    this.emit();
    try {
      this.capture = await this.deps.startCapture({ onFrame: (frame) => this.frame(frame) });
    } catch (error) {
      this.ended = true;
      this.view = { ...this.view, phase: "ended", error: microphoneError(error) };
      this.emit();
      return;
    }
    this.connect();
  }

  private connect(): void {
    const socket = this.deps.openSocket(this.deps.url);
    socket.binaryType = "arraybuffer";
    this.socket = socket;
    socket.addEventListener("open", () => {
      socket.send(JSON.stringify({ type: "auth", token: this.deps.token } satisfies CallClientEvent));
      if (this.deps.profile) this.send({ type: "session.update", profile: this.deps.profile });
      this.schedulePing();
    });
    socket.addEventListener("message", (event: MessageEvent) => this.message(event.data));
    socket.addEventListener("close", (event: Event) =>
      this.closed(socket, (event as CloseEvent).code),
    );
    socket.addEventListener("error", () => {
      /* `close` follows and decides */
    });
  }

  private message(data: unknown): void {
    if (typeof data === "string") {
      const event = parseCallEvent(data);
      if (event) this.event(event);
      return;
    }
    if (data instanceof ArrayBuffer && this.pendingAudio) {
      const { response_id, index } = this.pendingAudio;
      this.pendingAudio = null;
      this.player.enqueue(response_id, index, data);
    }
  }

  private event(event: CallServerEvent): void {
    switch (event.type) {
      case "session.created":
        this.live = true;
        this.attempts = 0;
        this.endOfTurnMs = event.end_of_turn_ms;
        this.deps.log?.("call.connected", { call_id: event.call_id });
        break;
      case "response.audio.start":
        this.pendingAudio = { response_id: event.response_id, index: event.index };
        break;
      case "output_audio.clear":
        this.player.clear();
        this.pendingAudio = null;
        break;
      case "input_audio_buffer.speech_stopped":
        this.speechStoppedAt = this.now();
        break;
      case "response.done":
        this.deps.log?.("call.response", { status: event.status, ...event.metrics });
        this.deps.onResponseDone?.();
        break;
      default:
        break;
    }
    this.view = reduceCall(this.view, event);
    this.emit();
  }

  private frame(frame: ArrayBuffer): void {
    if (!this.live || this.socket?.readyState !== OPEN) return;
    // Muted still streams — silence — so a turn in progress ends normally
    // rather than hanging open on the last thing the microphone heard.
    this.socket.send(this.muted ? SILENT_FRAME : frame);
  }

  private send(event: CallClientEvent): void {
    if (this.socket?.readyState === OPEN) this.socket.send(JSON.stringify(event));
  }

  private schedulePing(): void {
    if (this.pingTimer) this.clearTimer(this.pingTimer);
    this.pingTimer = this.setTimer(() => {
      this.send({ type: "ping" });
      if (!this.ended) this.schedulePing();
    }, PING_MS);
  }

  private closed(socket: RuntimeSocket, code?: number): void {
    if (socket !== this.socket) return;
    const refusal = closeReason(code);
    if (refusal) this.view = { ...this.view, error: refusal, phase: "connecting" };
    this.live = false;
    this.socket = null;
    this.player.clear();
    if (this.ended) return;
    const wait = RECONNECT_MS[this.attempts];
    if (wait === undefined || this.view.phase === "connecting") {
      // Never got a call, or ran out of retries: the call is over.
      this.finish(this.view.error ?? "The call was disconnected.");
      return;
    }
    this.attempts += 1;
    this.view = { ...this.view, phase: "reconnecting" };
    this.emit();
    this.deps.log?.("call.reconnect", { attempt: this.attempts });
    this.setTimer(() => {
      if (!this.ended) this.connect();
    }, wait);
  }

  private finish(error: string | null): void {
    this.ended = true;
    this.capture?.stop();
    this.capture = null;
    if (this.pingTimer) this.clearTimer(this.pingTimer);
    this.view = { ...this.view, phase: "ended", error };
    this.emit();
  }

  private emit(): void {
    this.onChange(this.view, this.muted);
  }

  /** End the call. Idempotent. */
  hangUp(): void {
    if (this.ended) return;
    this.ended = true;
    this.player.clear();
    const socket = this.socket;
    this.socket = null;
    this.live = false;
    socket?.close(1000, "hang up");
    this.finish(null);
    this.deps.log?.("call.end", {});
  }

  setMuted(muted: boolean): void {
    this.muted = muted;
    this.emit();
  }

  /** Stop the reply now. */
  interrupt(): void {
    this.player.clear();
    this.send({ type: "response.cancel" });
  }

  /**
   * Press Approve or Deny on the call's card. The engine resumes the turn and
   * speaks how it went. Returns false when the call cannot carry it.
   */
  resolve(id: string, approve: boolean): boolean {
    if (!this.live || this.socket?.readyState !== OPEN) return false;
    this.send({ type: "action.resolve", id, approve });
    return true;
  }

  current(): CallView {
    return this.view;
  }
}

/** What a refusal close code means, for the person placing the call. */
function closeReason(code: number | undefined): string | null {
  switch (code) {
    case 4401:
      return "The call's sign-in was refused. Sign in again and retry.";
    case 4403:
      return "This sign-in is not allowed to use the assistant.";
    case 4409:
      return "Another call is already in progress.";
    default:
      return null;
  }
}

function microphoneError(error: unknown): string {
  const name = (error as { name?: string })?.name ?? "";
  if (name === "NotAllowedError" || name === "SecurityError") {
    return "Microphone access was refused, so the call could not start.";
  }
  if (name === "NotFoundError") return "No microphone was found.";
  return "The microphone could not be opened.";
}

/** The call socket's URL for an engine at `base` (empty: this origin). */
export function callSocketUrl(base: string, location: { protocol: string; host: string }): string {
  const origin = base || `${location.protocol}//${location.host}`;
  return `${origin.replace(/^http/, "ws")}/api/assistant/call`;
}
