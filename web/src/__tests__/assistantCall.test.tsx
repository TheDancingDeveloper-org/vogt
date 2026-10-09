// The live call's wiring in the Assistant tab (WI-960): the Call control is
// offered only when the engine advertises calls, placing one opens the call
// socket, and a card's button during a call goes up that socket — a press,
// not a REST call, and never anything said.
import { fireEvent, render } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { fakeVogt, settle } from "./harness";

vi.mock("@capacitor/core", () => ({
  Capacitor: { isPluginAvailable: () => false, isNativePlatform: () => false },
}));

const capture = { stop: vi.fn() };
vi.mock("../callCapture", () => ({
  callCaptureSupported: () => true,
  startCapture: vi.fn(async () => capture),
}));

class FakeSocket {
  readyState = 0;
  binaryType = "blob";
  sent: string[] = [];
  private listeners: Record<string, ((event: unknown) => void)[]> = {};
  addEventListener(type: string, cb: (event: unknown) => void) {
    (this.listeners[type] ??= []).push(cb);
  }
  send(data: unknown) {
    if (typeof data === "string") this.sent.push(data);
  }
  close() {
    this.readyState = 3;
  }
  fire(type: string, event: unknown = {}) {
    for (const cb of this.listeners[type] ?? []) cb(event);
  }
  server(event: Record<string, unknown>) {
    this.fire("message", { data: JSON.stringify(event) });
  }
}
const sockets: FakeSocket[] = [];
vi.mock("../runtimeTransport", async (importOriginal) => {
  const real = await importOriginal<typeof import("../runtimeTransport")>();
  return {
    ...real,
    runtimeTransport: () => ({
      request: (input: RequestInfo | URL, init?: RequestInit) => fetch(input, init),
      openSocket: () => {
        const socket = new FakeSocket();
        sockets.push(socket);
        return socket;
      },
    }),
  };
});

import Assistant from "../Assistant";

class FakeAudioContext {
  state = "running";
  currentTime = 0;
  destination = {};
  resume = async () => {};
  suspend = async () => {};
  decodeAudioData = async () => ({ duration: 0.1 });
  createBufferSource = () => ({ connect() {}, start() {}, stop() {}, onended: null });
}

function engine(callEnabled: boolean) {
  return {
    "GET /api/assistant/history": { body: { transcript: [], pending_action: null } },
    "GET /api/config": {
      body: {
        assistant_enabled: true,
        assistant_profiles: [],
        assistant_stt_enabled: true,
        assistant_tts_enabled: true,
        assistant_call_enabled: callEnabled,
      },
    },
  };
}

function restCalls(substr: string) {
  const stub = globalThis.fetch as unknown as { mock: { calls: [RequestInfo | URL][] } };
  return stub.mock.calls.map(([input]) => String(input)).filter((url) => url.includes(substr));
}

describe("the Assistant's live call", () => {
  beforeEach(() => {
    sockets.length = 0;
    localStorage.clear();
    vi.stubGlobal("AudioContext", FakeAudioContext);
  });
  afterEach(() => vi.unstubAllGlobals());

  it("offers no Call control when the engine cannot place a call", async () => {
    fakeVogt({}, engine(false));
    const { container } = render(() => <Assistant onError={() => {}} />);
    await settle();
    expect(container.querySelector('[data-testid="assistant-call"]')).toBeNull();
  });

  it("places a call, shows its card, and approves it up the call socket", async () => {
    fakeVogt({}, engine(true));
    const errors: string[] = [];
    const { container } = render(() => <Assistant onError={(m) => errors.push(m)} />);
    await settle();
    const button = container.querySelector('[data-testid="assistant-call"]') as HTMLButtonElement;
    expect(button).toBeTruthy();
    expect(button.disabled).toBe(false);

    fireEvent.click(button);
    await settle();
    expect(sockets).toHaveLength(1);
    const socket = sockets[0] as FakeSocket;
    socket.readyState = 1;
    socket.fire("open");
    expect(JSON.parse(socket.sent[0] as string)).toMatchObject({ type: "auth" });
    socket.server({
      type: "session.created",
      protocol: 1,
      call_id: "c",
      sample_rate: 16000,
      end_of_turn_ms: 700,
      barge_in_ms: 500,
    });
    await settle();
    const panel = container.querySelector('[data-testid="assistant-call-panel"]') as HTMLElement;
    expect(panel.dataset.phase).toBe("listening");

    socket.server({
      type: "approval.pending",
      card: {
        kind: "send_input",
        id: "a1",
        session_id: "s1",
        session_name: "shell",
        text: "ls",
        submit: true,
      },
    });
    await settle();
    const approve = container.querySelector(".assistant-approve") as HTMLButtonElement;
    expect(approve).toBeTruthy();
    fireEvent.click(approve);
    await settle();
    expect(socket.sent.map((s) => JSON.parse(s))).toContainEqual({
      type: "action.resolve",
      id: "a1",
      approve: true,
    });
    expect(restCalls("/api/assistant/actions")).toHaveLength(0);

    fireEvent.click(container.querySelector('[data-testid="assistant-call-hangup"]') as HTMLElement);
    await settle();
    expect(capture.stop).toHaveBeenCalled();
    expect(container.querySelector('[data-testid="assistant-call-panel"]')).toBeNull();
    expect(errors).toEqual([]);
  });
});
