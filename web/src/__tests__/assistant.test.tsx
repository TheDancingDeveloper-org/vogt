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

async function mountAssistant() {
  fakeVogt();
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
