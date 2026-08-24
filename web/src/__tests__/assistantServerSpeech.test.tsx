// The server-side speech pipeline (#190, FR-T12, VOICE_POC §3.5).
//
// This is the path a client with *no* on-device speech takes: the native
// Capacitor plugin is absent (mocked below), the browser's Web Speech is absent
// (jsdom ships neither name), and jsdom has no `speechSynthesis` — so the
// component must reach for the engine's `/api/assistant/stt` and
// `/api/assistant/tts` routes, and where those 404 (unconfigured) fall silently
// back to typed input. All three are asserted here; the human spoken
// validation pass is a person's job, not this file's.
import { render, fireEvent } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { fakeVogt, settle } from "./harness";

vi.mock("@capacitor/core", () => ({
  Capacitor: { isPluginAvailable: () => false },
}));

import Assistant from "../Assistant";

/** A stand-in for `MediaRecorder`, firing its data+stop the way `stop()` does. */
class FakeMediaRecorder {
  static instances: FakeMediaRecorder[] = [];
  state: "inactive" | "recording" = "inactive";
  mimeType = "audio/webm";
  private listeners: Record<string, ((event: { data?: Blob }) => void)[]> = {};
  constructor(public stream: unknown) {
    FakeMediaRecorder.instances.push(this);
  }
  addEventListener(type: string, cb: (event: { data?: Blob }) => void) {
    (this.listeners[type] ??= []).push(cb);
  }
  start() {
    this.state = "recording";
  }
  stop() {
    this.state = "inactive";
    for (const cb of this.listeners["dataavailable"] ?? []) {
      cb({ data: new Blob(["audio-bytes"], { type: "audio/webm" }) });
    }
    for (const cb of this.listeners["stop"] ?? []) cb({});
  }
}

/** A stand-in for `HTMLAudioElement`, capturing that a clip was played. */
class FakeAudio {
  static instances: FakeAudio[] = [];
  play = vi.fn(() => Promise.resolve());
  pause = vi.fn();
  addEventListener = vi.fn();
  constructor(public src: string) {
    FakeAudio.instances.push(this);
  }
}

const fakeTrack = { stop: vi.fn() };
const getUserMedia = vi.fn(async () => ({ getTracks: () => [fakeTrack] }));

let savedCreate: typeof URL.createObjectURL;
let savedRevoke: typeof URL.revokeObjectURL;

function installMediaGlobals() {
  vi.stubGlobal("MediaRecorder", FakeMediaRecorder as unknown as typeof MediaRecorder);
  vi.stubGlobal("Audio", FakeAudio as unknown as typeof Audio);
  Object.defineProperty(navigator, "mediaDevices", {
    configurable: true,
    value: { getUserMedia },
  });
  savedCreate = URL.createObjectURL;
  savedRevoke = URL.revokeObjectURL;
  URL.createObjectURL = vi.fn(() => "blob:fake");
  URL.revokeObjectURL = vi.fn();
}

/** The engine surface, with server speech advertised as configured. */
function engine(over: Record<string, unknown> = {}) {
  return {
    "GET /api/assistant/history": { body: { transcript: [], pending_action: null } },
    "GET /api/config": {
      body: {
        assistant_enabled: true,
        assistant_profiles: [],
        assistant_stt_enabled: true,
        assistant_tts_enabled: true,
        ...over,
      },
    },
    "POST /api/assistant/stt": { body: { text: "open the backlog" } },
    "POST /api/assistant/tts": { text: "fake-audio" },
    "POST /api/assistant/message": {
      body: { reply: "here is the backlog", pending_action: null, tool_trace: [] },
    },
  };
}

function calls(substr: string) {
  const stub = globalThis.fetch as unknown as {
    mock: { calls: [RequestInfo | URL, RequestInit?][] };
  };
  return stub.mock.calls.map(([input]) => String(input)).filter((url) => url.includes(substr));
}

describe("the server-side speech pipeline", () => {
  beforeEach(() => {
    FakeMediaRecorder.instances = [];
    FakeAudio.instances = [];
    getUserMedia.mockClear();
    // jsdom ships no Web Speech; be explicit so an environment that adds one
    // does not silently steer the component onto the on-device path.
    vi.stubGlobal("webkitSpeechRecognition", undefined);
    vi.stubGlobal("SpeechRecognition", undefined);
    localStorage.clear();
  });
  afterEach(() => {
    vi.unstubAllGlobals();
    if (savedCreate) URL.createObjectURL = savedCreate;
    if (savedRevoke) URL.revokeObjectURL = savedRevoke;
    Reflect.deleteProperty(navigator, "mediaDevices");
  });

  it("uses the server STT route when no on-device recognizer exists", async () => {
    installMediaGlobals();
    fakeVogt({}, engine());
    const errors: string[] = [];
    const { container } = render(() => <Assistant onError={(m) => errors.push(m)} />);
    await settle();

    const mic = container.querySelector('[data-testid="mic"]') as HTMLButtonElement;
    expect(mic).toBeTruthy();

    fireEvent.pointerDown(mic, { pointerId: 1 });
    await settle();
    expect(getUserMedia).toHaveBeenCalledTimes(1);
    expect(FakeMediaRecorder.instances.at(-1)?.state).toBe("recording");
    expect(mic.dataset.listening).toBe("yes");

    fireEvent.pointerUp(mic, { pointerId: 1 });
    await settle();

    // The audio was posted to STT and the transcribed text sent as a message.
    expect(calls("/api/assistant/stt")).toHaveLength(1);
    const stub = globalThis.fetch as unknown as {
      mock: { calls: [RequestInfo | URL, RequestInit?][] };
    };
    const message = stub.mock.calls
      .filter(([input]) => String(input).includes("/api/assistant/message"))
      .map(([, init]) => JSON.parse(String(init?.body)) as Record<string, unknown>)[0];
    expect(message?.text).toBe("open the backlog");
    expect(mic.dataset.listening).toBe("no");
    expect(errors).toEqual([]);
  });

  // #335: the take is meant to auto-submit hands-free — ending it must call
  // `send` exactly once, with the transcript put back through the domain
  // repair pass first (FR-T13), and no manual "Send" tap in between.
  it("auto-submits the repaired transcript exactly once when the take ends", async () => {
    installMediaGlobals();
    // The recognizer heard the ref as words; the repair pass is what turns it
    // back into `WI-7` before it is sent.
    fakeVogt({}, { ...engine(), "POST /api/assistant/stt": { body: { text: "open issue seven" } } });
    const errors: string[] = [];
    const { container } = render(() => <Assistant onError={(m) => errors.push(m)} />);
    await settle();

    const mic = container.querySelector('[data-testid="mic"]') as HTMLButtonElement;
    fireEvent.pointerDown(mic, { pointerId: 1 });
    await settle();
    // Ending the take (button release) is the only act — no Send tap follows.
    fireEvent.pointerUp(mic, { pointerId: 1 });
    await settle();

    // Sent exactly once, and with the repaired text — not the raw transcript.
    const messages = calls("/api/assistant/message");
    expect(messages).toHaveLength(1);
    const stub = globalThis.fetch as unknown as {
      mock: { calls: [RequestInfo | URL, RequestInit?][] };
    };
    const sent = stub.mock.calls
      .filter(([input]) => String(input).includes("/api/assistant/message"))
      .map(([, init]) => JSON.parse(String(init?.body)) as Record<string, unknown>)[0];
    expect(sent?.text).toBe("open WI-7");
    // The repair is shown, not applied silently.
    expect(container.querySelector('[data-testid="voice-repair"]')?.textContent).toContain("WI-7");
    expect(errors).toEqual([]);
  });

  // #335: an empty transcript is a take that heard nothing — it must send
  // nothing rather than an empty turn, and surface no error.
  it("sends nothing when the take transcribes to empty", async () => {
    installMediaGlobals();
    fakeVogt({}, { ...engine(), "POST /api/assistant/stt": { body: { text: "   " } } });
    const errors: string[] = [];
    const { container } = render(() => <Assistant onError={(m) => errors.push(m)} />);
    await settle();

    const mic = container.querySelector('[data-testid="mic"]') as HTMLButtonElement;
    fireEvent.pointerDown(mic, { pointerId: 1 });
    await settle();
    fireEvent.pointerUp(mic, { pointerId: 1 });
    await settle();

    // The audio was still posted, but the empty result sent no message.
    expect(calls("/api/assistant/stt")).toHaveLength(1);
    expect(calls("/api/assistant/message")).toHaveLength(0);
    expect(errors).toEqual([]);
  });

  // #335: abandoning a take — leaving the surface mid-recording — must drop
  // the audio, never transcribe it, and never send a half-spoken turn.
  it("sends nothing when the take is abandoned by leaving the surface mid-recording", async () => {
    installMediaGlobals();
    fakeVogt({}, engine());
    const errors: string[] = [];
    const { container, unmount } = render(() => <Assistant onError={(m) => errors.push(m)} />);
    await settle();

    const mic = container.querySelector('[data-testid="mic"]') as HTMLButtonElement;
    fireEvent.pointerDown(mic, { pointerId: 1 });
    await settle();
    expect(mic.dataset.listening).toBe("yes");

    // Leave the surface while still recording — no release. The recorder's
    // `stop` must drop the audio rather than post it.
    unmount();
    await settle();

    expect(calls("/api/assistant/stt")).toHaveLength(0);
    expect(calls("/api/assistant/message")).toHaveLength(0);
    expect(errors).toEqual([]);
  });

  it("plays a spoken reply through the server TTS route when the browser cannot synthesize", async () => {
    installMediaGlobals();
    localStorage.setItem("vogt.assistant.tts", "1"); // TTS on from the start
    fakeVogt({}, engine());
    const errors: string[] = [];
    const { container } = render(() => <Assistant onError={(m) => errors.push(m)} />);
    await settle();

    const input = container.querySelector('.assistant-input') as HTMLTextAreaElement;
    fireEvent.input(input, { target: { value: "show me the backlog" } });
    const form = container.querySelector("form") as HTMLFormElement;
    fireEvent.submit(form);
    await settle();

    // jsdom has no speechSynthesis, so the reply was spoken via the server
    // route and played through an <audio> element.
    expect(calls("/api/assistant/tts")).toHaveLength(1);
    expect(FakeAudio.instances.at(-1)?.play).toHaveBeenCalledTimes(1);
    expect(errors).toEqual([]);
  });

  it("offers no microphone and stays silent when server speech is unconfigured", async () => {
    installMediaGlobals();
    // The engine reports both halves off (404 would follow any call): a client
    // with no on-device speech degrades to typed input with no error (FR-T6).
    fakeVogt({}, engine({ assistant_stt_enabled: false, assistant_tts_enabled: false }));
    const errors: string[] = [];
    const { container } = render(() => <Assistant onError={(m) => errors.push(m)} />);
    await settle();

    expect(container.querySelector('[data-testid="mic"]')).toBeNull();
    expect(container.querySelector('.assistant-input')).toBeTruthy();
    expect(errors).toEqual([]);
  });

  it("retires the server mic silently when the STT route 404s at use", async () => {
    installMediaGlobals();
    // Advertised as configured, so the mic appears — but the route answers 404,
    // the unconfigured case reached at use rather than at boot. The take must
    // degrade with no error surfaced.
    const routes = {
      ...engine(),
      "POST /api/assistant/stt": { status: 404, body: { error: "not found" } },
    };
    fakeVogt({}, routes);
    const errors: string[] = [];
    const { container } = render(() => <Assistant onError={(m) => errors.push(m)} />);
    await settle();

    const mic = container.querySelector('[data-testid="mic"]') as HTMLButtonElement;
    expect(mic).toBeTruthy();

    fireEvent.pointerDown(mic, { pointerId: 1 });
    await settle();
    fireEvent.pointerUp(mic, { pointerId: 1 });
    await settle();

    expect(calls("/api/assistant/stt")).toHaveLength(1);
    // No message was sent, and nothing was surfaced as an error.
    expect(calls("/api/assistant/message")).toHaveLength(0);
    expect(errors).toEqual([]);
    // The mic retired itself, degrading to typed input.
    expect(container.querySelector('[data-testid="mic"]')).toBeNull();
  });
});
