// The desktop microphone, via Web Speech (#189, FR-T13, VOICE_POC §3.4).
//
// The phone's microphone is the Capacitor native plugin and is covered by
// `assistant.test.tsx`, which mocks that plugin as present. Here the plugin is
// *absent* — the desktop case — so the component must reach for the browser's
// own `webkitSpeechRecognition`, and where that too is missing (Firefox) fall
// silently back to typed input. Both halves are asserted below; the spoken
// five-utterance validation pass is a person's job, not this file's.
import { render, fireEvent } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { fakeVogt, settle } from "./harness";

vi.mock("@capacitor/core", () => ({
  Capacitor: { isPluginAvailable: () => false },
}));

import Assistant from "../Assistant";

// The assistant reads its history, config and message reply from the engine,
// not from Vogt — stub them so a clean mount records no unrelated 404s and the
// error channel shows only what the microphone put there.
const ENGINE = {
  "GET /api/assistant/history": { body: { transcript: [], pending_action: null } },
  "GET /api/config": { body: { assistant_enabled: true, assistant_profiles: [] } },
  "POST /api/assistant/message": {
    body: { reply: null, pending_action: null, tool_trace: [] },
  },
};

/** A stand-in for the browser recognizer, capturing the instance under test. */
class FakeRecognition {
  static instances: FakeRecognition[] = [];
  continuous = false;
  interimResults = false;
  lang = "";
  onresult: ((event: { results: unknown }) => void) | null = null;
  onend: (() => void) | null = null;
  onerror: ((event: { error: string }) => void) | null = null;
  start = vi.fn();
  stop = vi.fn(() => {
    this.onend?.();
  });
  abort = vi.fn();
  constructor() {
    FakeRecognition.instances.push(this);
  }
}

/** Deliver a final transcript the way the Web Speech API shapes its results. */
function deliver(recognition: FakeRecognition, transcript: string) {
  recognition.onresult?.({ results: [[{ transcript }]] });
}

function assistantMessages(): string[] {
  const stub = globalThis.fetch as unknown as {
    mock: { calls: [RequestInfo | URL, RequestInit?][] };
  };
  return stub.mock.calls
    .map(([input]) => String(input))
    .filter((url) => url.includes("/api/assistant/message"));
}

describe("the desktop microphone (Web Speech)", () => {
  beforeEach(() => {
    FakeRecognition.instances = [];
  });
  afterEach(() => vi.unstubAllGlobals());

  it("gives a Chrome/Edge user a working microphone when the native plugin is absent", async () => {
    vi.stubGlobal("webkitSpeechRecognition", FakeRecognition);
    fakeVogt({}, ENGINE);
    const errors: string[] = [];
    const { container } = render(() => <Assistant onError={(m) => errors.push(m)} />);
    await settle();

    const mic = container.querySelector('[data-testid="mic"]') as HTMLButtonElement;
    expect(mic).toBeTruthy();

    fireEvent.pointerDown(mic, { pointerId: 1 });
    await settle();
    const recognition = FakeRecognition.instances.at(-1);
    expect(recognition?.start).toHaveBeenCalledTimes(1);
    expect(mic.dataset.listening).toBe("yes");

    deliver(recognition!, "open the backlog");
    await settle();

    fireEvent.pointerUp(mic, { pointerId: 1 });
    await settle();

    expect(assistantMessages()).toHaveLength(1);
    expect(mic.dataset.listening).toBe("no");
    expect(errors).toEqual([]);
  });

  it("runs the recognized text through the repair pass before sending", async () => {
    vi.stubGlobal("webkitSpeechRecognition", FakeRecognition);
    fakeVogt({}, ENGINE);
    const { container } = render(() => <Assistant onError={() => {}} />);
    await settle();

    const mic = container.querySelector('[data-testid="mic"]') as HTMLButtonElement;
    fireEvent.pointerDown(mic, { pointerId: 1 });
    await settle();
    // The default harness registers `alpha`; the repair is against what
    // `project.list` returned, not a list in the source.
    deliver(FakeRecognition.instances.at(-1)!, "work on issue twelve for alfa");
    await settle();
    fireEvent.pointerUp(mic, { pointerId: 1 });
    await settle();

    const stub = globalThis.fetch as unknown as {
      mock: { calls: [RequestInfo | URL, RequestInit?][] };
    };
    const body = stub.mock.calls
      .filter(([input]) => String(input).includes("/api/assistant/message"))
      .map(([, init]) => JSON.parse(String(init?.body)) as Record<string, unknown>)[0];
    expect(body).toEqual({
      text: "work on WI-12 for alpha",
      utterance: "work on issue twelve for alfa",
    });
    expect(container.querySelector('[data-testid="voice-repair"]')?.textContent).toContain(
      "WI-12",
    );
  });

  it("degrades to typed input with no error when no recognizer exists (Firefox)", async () => {
    vi.stubGlobal("webkitSpeechRecognition", undefined);
    vi.stubGlobal("SpeechRecognition", undefined);
    fakeVogt({}, ENGINE);
    const errors: string[] = [];
    const { container } = render(() => <Assistant onError={(m) => errors.push(m)} />);
    await settle();

    expect(container.querySelector('[data-testid="mic"]')).toBeNull();
    expect(container.querySelector('.assistant-input')).toBeTruthy();
    expect(errors).toEqual([]);
  });
});
