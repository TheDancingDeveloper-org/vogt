// The assistant composer's ergonomics (#242): a failed send that can be
// retried, an in-flight send that can be stopped, a multi-line composer, a
// transcript that follows the reader rather than yanking them, and replies
// rendered as Markdown.
//
// These mount `Assistant.tsx` against a hand-rolled `fetch` so the abort test
// can hold a request open and reject it precisely when the signal fires — the
// one thing the shared Vogt harness cannot do, because its stub answers a
// call without ever seeing the `AbortSignal` on it.

import { render, fireEvent } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vitest";

// No native platform here, so the mic degrades to the browser path (absent in
// jsdom) and the composer is what these tests are about.
vi.mock("@capacitor/core", () => ({
  Capacitor: { isPluginAvailable: () => false },
}));

import Assistant, { nearBottom } from "../Assistant";

/** Let microtasks and the component's `queueMicrotask`/timers settle. */
async function settle(times = 3): Promise<void> {
  for (let i = 0; i < times; i += 1) {
    await Promise.resolve();
    await new Promise((resolve) => setTimeout(resolve, 0));
  }
}

type Responder = (init?: RequestInit) => Promise<Response> | Response;

interface FetchHarness {
  calls: { url: string; init?: RequestInit }[];
  /** Every recorded call to the assistant message endpoint, in order. */
  messageCalls(): { url: string; init?: RequestInit }[];
}

/** A `fetch` that dispatches on a URL substring, first match wins. */
function installFetch(routes: [string, Responder][]): FetchHarness {
  const calls: { url: string; init?: RequestInit }[] = [];
  const stub = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = String(input);
    calls.push({ url, init });
    const hit = routes.find(([key]) => url.includes(key));
    if (hit) return hit[1](init);
    return new Response("", { status: 404 });
  });
  vi.stubGlobal("fetch", stub);
  return {
    calls,
    messageCalls: () => calls.filter((c) => c.url.includes("/api/assistant/message")),
  };
}

const jsonOk = (body: unknown): Responder => () =>
  new Response(JSON.stringify(body), { status: 200 });

/** The mount defaults every test shares: an enabled, empty assistant. */
function baseRoutes(message: Responder): [string, Responder][] {
  return [
    ["/api/assistant/message", message],
    ["/api/assistant/history", jsonOk({ transcript: [], pending_action: null })],
    ["/api/config", jsonOk({ assistant_enabled: true })],
    ["/api/vogt/projects", jsonOk({ projects: [], total: 0 })],
  ];
}

async function mount(message: Responder) {
  const harness = installFetch(baseRoutes(message));
  const errors: string[] = [];
  const rendered = render(() => <Assistant onError={(m) => errors.push(m)} />);
  await settle();
  const composer = () =>
    rendered.container.querySelector<HTMLTextAreaElement>(".assistant-input")!;
  const type = (text: string) =>
    fireEvent.input(composer(), { target: { value: text } });
  const submit = async () => {
    fireEvent.submit(rendered.container.querySelector("form")!);
    await settle();
  };
  return { harness, errors, composer, type, submit, ...rendered };
}

afterEach(() => {
  vi.unstubAllGlobals();
});

// -- 1. a failed send is recoverable ---------------------------------------

describe("a send that does not reach the engine", () => {
  it("restores the message, marks the turn failed, and retries", async () => {
    // The engine is refusing at first; the retry finds it back.
    let up = false;
    const m = await mount((init) => {
      if (up) {
        return new Response(
          JSON.stringify({ reply: "back now", pending_action: null, tool_trace: [] }),
          { status: 200 },
        );
      }
      // Echo nothing, just fail.
      void init;
      return new Response("upstream down", { status: 502 });
    });

    m.type("what is on top?");
    await m.submit();

    // The message is back in the composer, not eaten by the failure.
    expect(m.composer().value).toBe("what is on top?");
    // The optimistic bubble is marked failed and offers a Retry.
    expect(m.container.querySelector('[data-testid="assistant-retry"]')).not.toBeNull();
    expect(m.container.querySelector(".assistant-row--failed")).not.toBeNull();
    // The failure was surfaced.
    expect(m.errors.length).toBeGreaterThan(0);
    expect(m.harness.messageCalls()).toHaveLength(1);

    // Retry, with the engine back.
    up = true;
    fireEvent.click(m.container.querySelector('[data-testid="assistant-retry"]')!);
    await settle();

    // A second attempt was made, the reply landed, and the failed row is gone.
    expect(m.harness.messageCalls()).toHaveLength(2);
    expect(m.container.textContent).toContain("back now");
    expect(m.container.querySelector(".assistant-row--failed")).toBeNull();
  });
});

// -- 2. an in-flight send can be stopped -----------------------------------

describe("stopping a send in flight", () => {
  it("passes an AbortSignal and aborts the request cleanly", async () => {
    // A request that never answers on its own — only the signal ends it.
    const m = await mount(
      (init) =>
        new Promise<Response>((_resolve, reject) => {
          init?.signal?.addEventListener("abort", () =>
            reject(new DOMException("aborted", "AbortError")),
          );
        }),
    );

    m.type("summarise the backlog");
    await m.submit();

    // The request carried an AbortSignal, not yet aborted.
    const sent = m.harness.messageCalls()[0]?.init?.signal;
    expect(sent).toBeInstanceOf(AbortSignal);
    expect(sent?.aborted).toBe(false);

    // While busy, Stop replaces Send.
    const stopBtn = m.container.querySelector('[data-testid="assistant-stop"]');
    expect(stopBtn).not.toBeNull();

    fireEvent.click(stopBtn!);
    await settle();

    // The signal was aborted, the draft is back, the optimistic bubble was
    // dropped (a deliberate cancel is not a phantom), and nothing was toasted.
    expect(sent?.aborted).toBe(true);
    expect(m.composer().value).toBe("summarise the backlog");
    expect(m.container.querySelector(".assistant-row--user")).toBeNull();
    expect(m.errors).toHaveLength(0);
    // And the composer is back to offering Send.
    expect(m.container.querySelector('[data-testid="assistant-stop"]')).toBeNull();
  });
});

// -- 3. the composer is multi-line -----------------------------------------

describe("the multi-line composer", () => {
  it("sends on Enter and inserts a newline on Shift+Enter", async () => {
    const m = await mount(
      jsonOk({ reply: "ok", pending_action: null, tool_trace: [] }),
    );

    m.type("first line");
    // Shift+Enter must not send — it is the newline gesture.
    fireEvent.keyDown(m.composer(), { key: "Enter", shiftKey: true });
    await settle();
    expect(m.harness.messageCalls()).toHaveLength(0);

    // Plain Enter sends.
    fireEvent.keyDown(m.composer(), { key: "Enter" });
    await settle();
    expect(m.harness.messageCalls()).toHaveLength(1);
  });

  it("is a textarea, not a single-line input", async () => {
    const m = await mount(jsonOk({ reply: "ok", pending_action: null, tool_trace: [] }));
    expect(m.composer().tagName).toBe("TEXTAREA");
  });
});

// -- 4. the near-bottom scroll gate ----------------------------------------

describe("the scroll-to-end gate", () => {
  it("is true at the end and false when scrolled up", async () => {
    // At the end: the remaining distance is within the pad.
    expect(nearBottom(900, 100, 1000)).toBe(true);
    expect(nearBottom(820, 100, 1000)).toBe(true); // 80px up, inside the 120 pad
    // Scrolled up to re-read: well outside the pad.
    expect(nearBottom(0, 100, 1000)).toBe(false);
    expect(nearBottom(500, 100, 1000)).toBe(false);
    // An empty transcript with no overflow is trivially at the end.
    expect(nearBottom(0, 0, 0)).toBe(true);
  });
});

// -- 5. replies are Markdown -----------------------------------------------

describe("an assistant reply", () => {
  it("renders a fenced code block as a real <pre><code>", async () => {
    const m = await mount(
      jsonOk({
        reply: "Run this:\n\n```sh\ncargo test\n```\n",
        pending_action: null,
        tool_trace: [],
      }),
    );
    m.type("how do I test?");
    await m.submit();

    const pre = m.container.querySelector("pre.md-pre code");
    expect(pre).not.toBeNull();
    expect(pre?.textContent).toContain("cargo test");
    // A copy affordance rides along with the reply.
    expect(m.container.querySelector('[data-testid="assistant-copy"]')).not.toBeNull();
  });
});
