// An approval tap must not lose its card. The card stays up, marked sending,
// until the request succeeds. A tap that fails because the connection dropped
// is queued and sent again when the event stream is back. Any other refusal
// restores the card so the approval can be made again.
//
// The stream's connection state is a signal the test can flip, so the queue's
// "send when reconnected" path runs against the real component.
import { fireEvent, render, waitFor } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vitest";

vi.mock("@capacitor/core", () => ({
  Capacitor: { isPluginAvailable: () => false },
}));

// The stream's connected state, held in a signal the tests can flip.
const link = vi.hoisted(() => ({ setConnected: (_value: boolean) => {} }));
vi.mock("../store", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../store")>();
  const { createSignal } = await import("solid-js");
  const [connected, setConnected] = createSignal(true);
  link.setConnected = setConnected;
  return { ...actual, isConnected: connected };
});

import Assistant, { TAP_RETRY_MS } from "../Assistant";

async function settle(times = 3): Promise<void> {
  for (let i = 0; i < times; i += 1) {
    await Promise.resolve();
    await new Promise((resolve) => setTimeout(resolve, 0));
  }
}

type Responder = (init?: RequestInit) => Promise<Response> | Response;

const APPROVAL_ID = "22222222-2222-4222-8222-222222222222";

const pendingCard = {
  reply: null,
  pending_action: {
    kind: "send_input",
    id: APPROVAL_ID,
    session_id: "11111111-1111-4111-8111-111111111111",
    session_name: "deploy-agent",
    text: "y",
    submit: true,
  },
  tool_trace: ["prepared terminal input"],
};

const json = (body: unknown, status = 200): Responder => () =>
  new Response(JSON.stringify(body), { status });

/** Mount with a pending approval card already on screen. */
async function mountWithCard(action: Responder) {
  const calls: string[] = [];
  const routes: [string, Responder][] = [
    ["/api/assistant/actions/", (init) => {
      calls.push(init?.method ?? "GET");
      return action(init);
    }],
    ["/api/assistant/message", json(pendingCard)],
    ["/api/assistant/history", json({ transcript: [], pending_action: null })],
    ["/api/config", json({ assistant_enabled: true })],
    ["/api/vogt/projects", json({ projects: [], total: 0 })],
  ];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input);
      const hit = routes.find(([key]) => url.includes(key));
      if (hit) return hit[1](init);
      return new Response("", { status: 404 });
    }),
  );
  const errors: string[] = [];
  const rendered = render(() => <Assistant onError={(m) => errors.push(m)} />);
  await settle();
  const composer = rendered.container.querySelector<HTMLTextAreaElement>(".assistant-input")!;
  fireEvent.input(composer, { target: { value: "answer yes" } });
  fireEvent.submit(rendered.container.querySelector("form")!);
  await settle();
  const card = () => rendered.container.querySelector<HTMLElement>(".assistant-pending-card");
  const approve = () =>
    rendered.container.querySelector<HTMLButtonElement>(".assistant-approve")!;
  return { ...rendered, calls, errors, card, approve };
}

afterEach(() => {
  link.setConnected(true);
  vi.unstubAllGlobals();
});

describe("an approval tap that fails on a dropped connection", () => {
  it("keeps the card, queues the tap, and sends it when the stream is back", async () => {
    let up = false;
    const m = await mountWithCard(() => {
      if (up) return json({ reply: "Done.", pending_action: null, tool_trace: [] })();
      // A transport-level failure: fetch rejects before any response arrives.
      throw new TypeError("NetworkError when attempting to fetch resource.");
    });
    expect(m.card()).not.toBeNull();

    link.setConnected(false);
    fireEvent.click(m.approve());
    await settle(6);

    // The card is still there, held, and the tap cannot be sent again.
    expect(m.card()).not.toBeNull();
    expect(m.card()!.textContent).toContain("queued");
    expect(m.approve().disabled).toBe(true);
    expect(m.calls).toHaveLength(1);

    // The stream comes back: the queued tap goes out once and the card clears.
    up = true;
    link.setConnected(true);
    await waitFor(() => expect(m.card()).toBeNull());
    expect(m.calls).toHaveLength(2);
    expect(m.container.textContent).toContain("Done.");
    expect(m.errors).toEqual([]);
  });

  it("retries a tap that failed while the stream stayed up", async () => {
    let attempts = 0;
    const m = await mountWithCard(() => {
      attempts += 1;
      if (attempts === 1) throw new TypeError("NetworkError when attempting to fetch resource.");
      return json({ reply: "Done.", pending_action: null, tool_trace: [] })();
    });

    fireEvent.click(m.approve());
    await settle(6);
    expect(m.card()!.textContent).toContain("queued");

    // Still connected, so the retry is on a timer rather than a reconnect.
    await waitFor(() => expect(m.card()).toBeNull(), { timeout: TAP_RETRY_MS + 2_000 });
    expect(m.calls).toHaveLength(2);
  });

  it("sends the tap once while the first request is still in flight", async () => {
    let release: (() => void) | undefined;
    const m = await mountWithCard(
      () =>
        new Promise<Response>((resolve) => {
          release = () => resolve(new Response(JSON.stringify({ reply: "Done.", pending_action: null, tool_trace: [] })));
        }),
    );

    fireEvent.click(m.approve());
    await settle();
    // The card is held as sending, and a second tap does nothing.
    expect(m.card()!.textContent).toContain("Sending your answer");
    fireEvent.click(m.approve());
    await settle();
    expect(m.calls).toHaveLength(1);

    release?.();
    await waitFor(() => expect(m.card()).toBeNull());
    expect(m.calls).toHaveLength(1);
  });
});

describe("an approval tap the engine refuses", () => {
  it("restores the card and reports the refusal", async () => {
    const m = await mountWithCard(json({ error: "action expired" }, 409));

    fireEvent.click(m.approve());
    await settle(6);

    // Not a connection fault, so nothing is queued: the card comes back with
    // its approval buttons, and the refusal is surfaced.
    expect(m.card()).not.toBeNull();
    expect(m.card()!.textContent).not.toContain("queued");
    expect(m.card()!.textContent).not.toContain("Sending");
    expect(m.approve().disabled).toBe(false);
    expect(m.errors.some((e) => e.startsWith("assistant action:"))).toBe(true);
    expect(m.calls).toHaveLength(1);
  });
});
