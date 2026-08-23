// The assistant's microphone is held, not toggled (FR-T5).
//
// This file is the first thing to mount `Assistant.tsx`. §6.2a's row for
// FR-T2 has said "Assistant.tsx is mounted by nothing" through two audit
// passes; what follows covers the voice control only, so that row is
// narrowed rather than retired.
//
// The native plugin is mocked because it is native: STT is
// `@capacitor-community/speech-recognition` and exists only inside the APK.
// What is being asserted is this component's own contract with it — when it
// opens the microphone, when it closes it, and what it does with what was
// said — which is exactly the part that changed and the part a device demo
// is worst at checking, because a demo cannot show you the take that was
// sent twice.
import { render, fireEvent } from "@solidjs/testing-library";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { fakeVogt, settle } from "./harness";

const recognition = {
  requestPermissions: vi.fn(async () => ({ speechRecognition: "granted" })),
  available: vi.fn(async () => ({ available: true })),
  start: vi.fn(async () => {}),
  stop: vi.fn(async () => {}),
  addListener: vi.fn(async () => {}),
  removeAllListeners: vi.fn(async () => {}),
};

vi.mock("@capacitor-community/speech-recognition", () => ({
  SpeechRecognition: recognition,
}));

vi.mock("@capacitor/core", () => ({
  Capacitor: { isPluginAvailable: () => true },
}));

import Assistant from "../Assistant";

/** The listener the component registered for a given plugin event. */
function listenerFor(event: string): ((data: unknown) => void) | undefined {
  const calls = recognition.addListener.mock.calls as unknown as [
    string,
    (data: unknown) => void,
  ][];
  return calls.find(([name]) => name === event)?.[1];
}

function assistantRequests(): string[] {
  const stub = globalThis.fetch as unknown as {
    mock: { calls: [RequestInfo | URL, RequestInit?][] };
  };
  return stub.mock.calls
    .map(([input]) => String(input))
    .filter((url) => url.includes("/api/assistant"));
}

async function mountAssistant(engine: Record<string, unknown> = {}) {
  fakeVogt({}, engine as Parameters<typeof fakeVogt>[1]);
  const errors: string[] = [];
  const mounted = render(() => <Assistant onError={(m) => errors.push(m)} />);
  await settle();
  const mic = mounted.container.querySelector(
    '[data-testid="mic"]',
  ) as HTMLButtonElement;
  return { mic, errors, ...mounted };
}

describe("the assistant's microphone", () => {
  beforeEach(() => {
    for (const fn of Object.values(recognition)) fn.mockClear();
  });

  it("opens only while the button is held", async () => {
    const { mic } = await mountAssistant();
    expect(mic).toBeTruthy();
    expect(mic.dataset.listening).toBe("no");

    fireEvent.pointerDown(mic, { pointerId: 1 });
    await settle();
    expect(recognition.start).toHaveBeenCalledTimes(1);
    expect(mic.dataset.listening).toBe("yes");

    fireEvent.pointerUp(mic, { pointerId: 1 });
    await settle();
    expect(recognition.stop).toHaveBeenCalledTimes(1);
    expect(mic.dataset.listening).toBe("no");
  });

  it("does not leave the microphone open when the press ends off the button", async () => {
    // A finger that slides while speaking is ordinary. A microphone that
    // stays open because of it is not, and under a toggle this was the
    // failure mode: the take never ended, and the recognizer sent whatever
    // it eventually heard.
    const { mic } = await mountAssistant();
    fireEvent.pointerDown(mic, { pointerId: 1 });
    await settle();
    fireEvent.pointerCancel(mic, { pointerId: 1 });
    await settle();
    expect(mic.dataset.listening).toBe("no");
    expect(recognition.stop).toHaveBeenCalled();
  });

  it("can be held from the keyboard, and opens the microphone once", async () => {
    // Hold-to-talk is a pointer idiom, and a control only pointers can work
    // is a control some people cannot use at all.
    //
    // Two things hold the "once": the `!e.repeat` guard on the handler and
    // `startListening`'s own `if (listening()) return`. Removing either
    // leaves this green, which is worth saying rather than implying the
    // assertion pins both — it pins the behaviour, and the behaviour has a
    // spare.
    const { mic } = await mountAssistant();
    fireEvent.keyDown(mic, { key: " " });
    await settle();
    expect(recognition.start).toHaveBeenCalledTimes(1);

    fireEvent.keyDown(mic, { key: " ", repeat: true });
    await settle();
    expect(recognition.start).toHaveBeenCalledTimes(1);

    fireEvent.keyUp(mic, { key: " " });
    await settle();
    expect(mic.dataset.listening).toBe("no");
  });

  it("sends what was said when the button is released", async () => {
    const { mic } = await mountAssistant();
    fireEvent.pointerDown(mic, { pointerId: 1 });
    await settle();

    // What the recognizer heard, delivered the way the plugin delivers it.
    listenerFor("partialResults")?.({ matches: ["open the backlog"] });
    await settle();

    fireEvent.pointerUp(mic, { pointerId: 1 });
    await settle();
    const sent = assistantRequests().filter((url) => url.includes("message"));
    expect(sent).toHaveLength(1);
  });

  it("sends one message when the recognizer stops before the button is released", async () => {
    // Both ends of a take can arrive and either can be first. A double send
    // is one thing said once and answered twice — which on a phone means two
    // spoken replies over each other.
    const { mic } = await mountAssistant();
    fireEvent.pointerDown(mic, { pointerId: 1 });
    await settle();
    listenerFor("partialResults")?.({ matches: ["what is on top"] });
    await settle();

    listenerFor("listeningState")?.({ status: "stopped" });
    await settle();
    fireEvent.pointerUp(mic, { pointerId: 1 });
    await settle();

    expect(
      assistantRequests().filter((url) => url.includes("message")),
    ).toHaveLength(1);
    // And the take is closed once. This is the assertion that pins the
    // guard: the message count alone stays at one even without it, because
    // sending clears the draft and the second end finds nothing to send —
    // which is true today and is not what makes it correct. `send` declines
    // while the assistant is busy *without* clearing the draft, and that is
    // the path where a second end would say the same thing twice.
    expect(recognition.stop).toHaveBeenCalledTimes(1);
  });

  it("sends nothing when nothing was heard", async () => {
    const { mic } = await mountAssistant();
    fireEvent.pointerDown(mic, { pointerId: 1 });
    await settle();
    fireEvent.pointerUp(mic, { pointerId: 1 });
    await settle();
    expect(
      assistantRequests().filter((url) => url.includes("message")),
    ).toHaveLength(0);
  });
});


// -- FR-T5's spoken replies, and the half a speaker cannot check -----------
//
// §6.2b lists "spoken replies" as needing "a device with a speaker". That is
// true of the sound and false of everything else: whether the app asks for
// speech, with what text, and — most of all — whether it ever offers a voice
// route to approving something, are decided here and were asserted nowhere.
// A speaker is the worst instrument for the last one, because what is being
// checked is the absence of a sentence.

const TTS_KEY = "vogt.assistant.tts";

function captureSpeech(): { spoken: () => string[]; cancels: () => number } {
  const spoken: string[] = [];
  let cancels = 0;
  class Utterance {
    text: string;
    constructor(text: string) {
      this.text = text;
    }
  }
  vi.stubGlobal("SpeechSynthesisUtterance", Utterance);
  vi.stubGlobal("speechSynthesis", {
    speak: (u: { text: string }) => spoken.push(u.text),
    cancel: () => {
      cancels += 1;
    },
  });
  return { spoken: () => spoken.filter((t) => t.trim()), cancels: () => cancels };
}

/** Say something, and let the engine answer it. */
async function say(container: HTMLElement, text: string): Promise<void> {
  const input = container.querySelector<HTMLInputElement>('input[type="text"]')!;
  fireEvent.input(input, { target: { value: text } });
  fireEvent.submit(container.querySelector("form")!);
  await settle();
}

describe("FR-T5 — replies are spoken, and approval is never one of them", () => {
  it("speaks a reply in sentences when the toggle is on", async () => {
    const speech = captureSpeech();
    localStorage.setItem(TTS_KEY, "1");
    const { container } = await mountAssistant({
      "POST /api/assistant/message": {
        body: {
          reply: "The forge adapter is on top. It has been for a week.",
          pending_action: null,
          tool_trace: [],
        },
      },
    });
    await say(container, "what is on top?");
    // Sentence chunks, not one utterance: the synth stays responsive and
    // interruptible, which is what makes a spoken answer stoppable.
    expect(speech.spoken()).toEqual([
      "The forge adapter is on top.",
      "It has been for a week.",
    ]);
  });

  it("says nothing when the toggle is off", async () => {
    const speech = captureSpeech();
    localStorage.setItem(TTS_KEY, "0");
    const { container } = await mountAssistant({
      "POST /api/assistant/message": {
        body: { reply: "Nothing is on top.", pending_action: null, tool_trace: [] },
      },
    });
    await say(container, "what is on top?");
    expect(speech.spoken()).toHaveLength(0);
  });

  it("announces a pending action in words no voice can answer", async () => {
    // FR-T2: approval is an on-screen act, so a misheard "yes" authorises
    // nothing. The announcement says what is about to happen and where to
    // approve it, and offers no spoken answer at all.
    const speech = captureSpeech();
    localStorage.setItem(TTS_KEY, "1");
    const { container } = await mountAssistant({
      "POST /api/assistant/message": {
        body: {
          reply: null,
          pending_action: {
            id: "act_01",
            kind: "vogt_write",
            operation: "work.transition",
            target: "WI-7",
            reason: "the migration landed",
            payload: "{}",
          },
          tool_trace: [],
        },
      },
    });
    await say(container, "move WI-7 to done");

    const said = speech.spoken().join(" ");
    // Whole, rather than "work." then "transition on WI-7": the operation is
    // the one word in that sentence that says what is about to happen, and
    // the chunker used to break it at its own full stop.
    expect(said).toContain("work.transition");
    expect(said).toContain("Approve on screen");
    for (const invitation of ["say yes", "say approve", "yes or no", "say ok"]) {
      expect(said.toLowerCase()).not.toContain(invitation);
    }
  });

  it("stops speaking when the next thing is said", async () => {
    // A reply still being read aloud while the user is already asking
    // something else is the phone talking over its owner.
    const speech = captureSpeech();
    localStorage.setItem(TTS_KEY, "1");
    const { container } = await mountAssistant({
      "POST /api/assistant/message": {
        body: { reply: "A long answer.", pending_action: null, tool_trace: [] },
      },
    });
    await say(container, "first");
    const before = speech.cancels();
    await say(container, "second");
    expect(speech.cancels()).toBeGreaterThan(before);
  });
});

// -- The repair pass, wired (FR-T13, r16) ----------------------------------

/** The JSON bodies posted to the assistant's message route, in order. */
function assistantMessageBodies(): Record<string, unknown>[] {
  const stub = globalThis.fetch as unknown as {
    mock: { calls: [RequestInfo | URL, RequestInit?][] };
  };
  return stub.mock.calls
    .filter(([input]) => String(input).includes("/api/assistant/message"))
    .map(([, init]) =>
      typeof init?.body === "string"
        ? (JSON.parse(init.body) as Record<string, unknown>)
        : {},
    );
}

describe("what the recognizer heard, repaired before it is sent", () => {
  beforeEach(() => {
    for (const fn of Object.values(recognition)) fn.mockClear();
  });

  it("sends the repaired utterance, not the raw transcription", async () => {
    // `voiceRepair.test.ts` proves the repair; this proves it is *reached*.
    // Without the wiring the module is a well-tested file that nothing calls,
    // and the utterance that arrives at the engine is the one with the
    // spaces in the project name.
    const { mic } = await mountAssistant();
    fireEvent.pointerDown(mic, { pointerId: 1 });
    await settle();
    // The default harness registers `alpha` and `beta` as projects, and the
    // repair is against what `project.list` returned — not a list in the
    // source, which is the difference between a validation pass and a guess.
    listenerFor("partialResults")?.({
      matches: ["can you work on issue twelve for alfa"],
    });
    await settle();
    fireEvent.pointerUp(mic, { pointerId: 1 });
    await settle();

    const bodies = assistantMessageBodies();
    expect(bodies).toHaveLength(1);
    expect(bodies[0]?.text).toBe("can you work on WI-12 for alpha");
  });

  it("shows what it changed", async () => {
    // A repair nobody can see is indistinguishable from a recognizer that
    // heard correctly — and when it is wrong, this line is the only thing
    // that explains why the answer is about the wrong item.
    const { mic, container } = await mountAssistant();
    fireEvent.pointerDown(mic, { pointerId: 1 });
    await settle();
    listenerFor("partialResults")?.({ matches: ["work on issue twelve"] });
    await settle();
    fireEvent.pointerUp(mic, { pointerId: 1 });
    await settle();

    const note = container.querySelector('[data-testid="voice-repair"]');
    expect(note?.textContent).toContain("WI-12");
  });

  it("says nothing when there was nothing to repair", async () => {
    const { mic, container } = await mountAssistant();
    fireEvent.pointerDown(mic, { pointerId: 1 });
    await settle();
    listenerFor("partialResults")?.({ matches: ["are there any notifications"] });
    await settle();
    fireEvent.pointerUp(mic, { pointerId: 1 });
    await settle();

    expect(assistantMessageBodies()[0]?.text).toBe("are there any notifications");
    expect(container.querySelector('[data-testid="voice-repair"]')).toBeNull();
  });
});

// -- Provider profiles (FR-T9, r16) ----------------------------------------

describe("choosing which backend answers", () => {
  beforeEach(() => {
    for (const fn of Object.values(recognition)) fn.mockClear();
  });

  it("offers no choice when the deployment configured one profile", async () => {
    // A select with one option is a control that asks a question with no
    // answer.
    const { container } = await mountAssistant({
      "GET /api/config": {
        body: {
          assistant_enabled: true,
          assistant_profiles: [
            { name: "default", model: "gpt-5.4-mini", default: true },
          ],
        },
      },
    });
    expect(container.querySelector('[data-testid="assistant-profile"]')).toBeNull();
  });

  it("sends the chosen profile with the message", async () => {
    const { container } = await mountAssistant({
      "GET /api/config": {
        body: {
          assistant_enabled: true,
          assistant_profiles: [
            { name: "clawbay", model: "gpt-5.4-mini", default: true },
            { name: "openrouter", model: "qwen/qwen3-coder", default: false },
          ],
        },
      },
      "POST /api/assistant/message": {
        body: { reply: "hi", pending_action: null, tool_trace: [] },
      },
    });
    const select = container.querySelector(
      '[data-testid="assistant-profile"]',
    ) as HTMLSelectElement;
    expect(select).not.toBeNull();

    fireEvent.change(select, { target: { value: "openrouter" } });
    await settle();
    await say(container, "hello");

    expect(assistantMessageBodies()[0]?.profile).toBe("openrouter");
  });

  it("names no profile when the default is left alone", async () => {
    // The request a client that never chose has always sent. A field that
    // appeared as `null` would be a request naming a profile called nothing.
    const { container } = await mountAssistant({
      "GET /api/config": {
        body: {
          assistant_enabled: true,
          assistant_profiles: [
            { name: "clawbay", model: "gpt-5.4-mini", default: true },
            { name: "openrouter", model: "qwen/qwen3-coder", default: false },
          ],
        },
      },
      "POST /api/assistant/message": {
        body: { reply: "hi", pending_action: null, tool_trace: [] },
      },
    });
    await say(container, "hello");
    expect(assistantMessageBodies()[0]).not.toHaveProperty("profile");
  });
});
