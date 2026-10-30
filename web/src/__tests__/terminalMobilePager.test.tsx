import { fireEvent, render, waitFor } from "@solidjs/testing-library";
import { Show, createEffect, createSignal } from "solid-js";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { SessionSummary } from "../api";
import TerminalWorkspace from "../TerminalWorkspace";
import { refreshSessions } from "../store";
import { resetTerminalHandoffs } from "../terminalHandoff";
import { fakeVogt, settle } from "./harness";

// The mock stands in for xterm + its socket: a mount is a new terminal, and
// the first moment it is not a preview it "attaches" (openAttach).
const terminalLog = vi.hoisted(() => ({
  mounts: [] as string[],
  openAttach: [] as string[],
}));

vi.mock("../Terminal", () => ({
  default: (props: { sessionId: string; preview?: boolean }) => {
    terminalLog.mounts.push(props.sessionId);
    let attached = false;
    createEffect(() => {
      if (attached || props.preview) return;
      attached = true;
      terminalLog.openAttach.push(props.sessionId);
    });
    return (
      <div data-mock-terminal={props.sessionId}>
        <textarea data-mock-input={props.sessionId} />
        {props.sessionId}
      </div>
    );
  },
}));

function session(
  id: string,
  activity: SessionSummary["activity"],
  exitCode: number | null = null,
  changed = "2026-08-30T10:00:00Z",
): SessionSummary {
  return {
    id,
    name: `${id}-shell`,
    cwd: `/srv/${id}`,
    activity,
    activity_changed_at: changed,
    exit_code: exitCode,
    scrollback_bytes: 0,
    created_at: "2026-08-30T09:00:00Z",
  };
}

const WAITING = session("wait", "waiting-for-input");
const ALPHA = session("alpha", "running");
const BETA = session("beta", "idle");
const EXITED = session("old", "idle", 0);

async function seed(sessions: SessionSummary[]): Promise<void> {
  fakeVogt({}, { "GET /api/sessions": { body: sessions } });
  await refreshSessions();
  await settle();
}

beforeEach(() => {
  vi.stubGlobal(
    "matchMedia",
    ((query: string) => ({
      matches:
        query === "(max-width: 768px)" ||
        query === "(prefers-reduced-motion: reduce)",
      media: query,
      onchange: null,
      addListener() {},
      removeListener() {},
      addEventListener() {},
      removeEventListener() {},
      dispatchEvent: () => false,
    })) as typeof window.matchMedia,
  );
});

afterEach(() => {
  vi.unstubAllGlobals();
  resetTerminalHandoffs();
  terminalLog.mounts.length = 0;
  terminalLog.openAttach.length = 0;
});

describe("the narrow terminal pager", () => {
  it("keeps the routed identity selected when attention order changes", async () => {
    const firstAlpha = session("alpha", "idle", null, "2026-08-30T10:05:00Z");
    const firstBeta = session("beta", "idle", null, "2026-08-30T10:04:00Z");
    await seed([firstAlpha, firstBeta]);
    const rendered = render(() => (
      <TerminalWorkspace tabId="term:beta" sessionId="beta" />
    ));

    expect(
      rendered.container.querySelector('[aria-label="Show beta-shell"]')
        ?.getAttribute("aria-current"),
    ).toBe("true");

    // Beta moves ahead of alpha, but identity—not the old numeric index—is
    // the selection source for the bar, dot, and counter.
    await seed([
      session("alpha", "idle", null, "2026-08-30T10:05:00Z"),
      session("beta", "idle", null, "2026-08-30T10:06:00Z"),
    ]);
    await waitFor(() =>
      expect(
        rendered.container.querySelector('[aria-label="Show beta-shell"]')
          ?.getAttribute("aria-current"),
      ).toBe("true"),
    );
    expect(
      rendered.container.querySelector("[data-mobile-session-bar]")?.textContent,
    ).toContain("beta-shell");
  });

  it("supports arrow selection and preserves an actively edited composer", async () => {
    await seed([WAITING, ALPHA, BETA, EXITED]);
    const change = vi.fn();
    const rendered = render(() => (
      <TerminalWorkspace
        tabId="term:alpha"
        sessionId="alpha"
        onMobileSessionChange={change}
      />
    ));
    const bar = rendered.container.querySelector<HTMLElement>(
      "[data-mobile-session-bar]",
    )!;

    fireEvent.keyDown(bar, { key: "ArrowRight" });
    await settle();
    expect(change).toHaveBeenLastCalledWith("beta");
    expect(document.activeElement).toBe(bar);

    const composer = rendered.container.querySelector<HTMLTextAreaElement>(
      ".terminal-composer textarea",
    )!;
    composer.focus();
    fireEvent.click(
      rendered.container.querySelector('[aria-label="Show old-shell"]')!,
    );
    await settle();
    expect(change).toHaveBeenLastCalledWith("old");
    expect(document.activeElement).toBe(composer);
  });

  it("mounts only the current and adjacent destination while dragging", async () => {
    await seed([WAITING, ALPHA, BETA, EXITED]);
    const rendered = render(() => (
      <TerminalWorkspace tabId="term:alpha" sessionId="alpha" />
    ));
    const stage = rendered.container.querySelector<HTMLElement>(
      ".terminal-mobile-stage",
    )!;

    fireEvent.touchStart(stage, {
      touches: [{ clientX: 200, clientY: 100 }],
    });
    fireEvent.touchMove(stage, {
      touches: [{ clientX: 100, clientY: 104 }],
    });

    const mounted = [
      ...rendered.container.querySelectorAll<HTMLElement>("[data-mock-terminal]"),
    ].map((node) => node.dataset.mockTerminal);
    expect(mounted).toEqual(["alpha", "beta"]);
    expect(mounted).toHaveLength(2);
  });

  it("reports a destination that vanishes before the gesture settles", async () => {
    let snapshot = [WAITING, ALPHA, BETA, EXITED];
    fakeVogt({}, {
      "GET /api/sessions": () => ({ body: snapshot }),
    });
    await refreshSessions();
    await settle();
    const unavailable = vi.fn();
    const change = vi.fn();
    const rendered = render(() => (
      <TerminalWorkspace
        tabId="term:alpha"
        sessionId="alpha"
        onMobileSessionChange={change}
        onMobileSessionUnavailable={unavailable}
      />
    ));
    const stage = rendered.container.querySelector<HTMLElement>(
      ".terminal-mobile-stage",
    )!;

    fireEvent.touchStart(stage, {
      touches: [{ clientX: 200, clientY: 100 }],
    });
    fireEvent.touchMove(stage, {
      touches: [{ clientX: 100, clientY: 103 }],
    });
    expect(
      rendered.container.querySelector('[data-mock-terminal="beta"]'),
    ).not.toBeNull();

    snapshot = [WAITING, ALPHA, EXITED];
    await refreshSessions();
    await settle();
    fireEvent.touchEnd(stage, { touches: [] });

    await waitFor(() => expect(unavailable).toHaveBeenCalledOnce());
    expect(change).not.toHaveBeenCalled();
  });

  // Mimics App: a settled swipe routes to a new tab whose workspace mounts a
  // pane for the destination session.
  function renderRoutedPager(start: string) {
    const [current, setCurrent] = createSignal(start);
    const rendered = render(() => (
      <Show when={current()} keyed>
        {(id) => (
          <TerminalWorkspace
            tabId={`term:${id}`}
            sessionId={id}
            onMobileSessionChange={setCurrent}
          />
        )}
      </Show>
    ));
    const stage = () =>
      rendered.container.querySelector<HTMLElement>(".terminal-mobile-stage")!;
    return { rendered, current, stage };
  }

  function swipe(stage: HTMLElement, toX: number) {
    fireEvent.touchStart(stage, { touches: [{ clientX: 200, clientY: 100 }] });
    fireEvent.touchMove(stage, { touches: [{ clientX: toX, clientY: 102 }] });
    fireEvent.touchEnd(stage, { touches: [] });
  }

  it("hands the dragged terminal to the new tab: one attach per settled swipe", async () => {
    await seed([WAITING, ALPHA, BETA, EXITED]);
    const { rendered, current, stage } = renderRoutedPager("alpha");
    await settle();
    expect(terminalLog.openAttach).toEqual(["alpha"]);

    swipe(stage(), 100);
    await waitFor(() => expect(current()).toBe("beta"));
    await settle();

    // beta's terminal was mounted once (in the slide) and attached once.
    expect(terminalLog.mounts.filter((id) => id === "beta")).toHaveLength(1);
    expect(terminalLog.openAttach.filter((id) => id === "beta")).toHaveLength(1);
    expect(
      rendered.container.querySelectorAll('[data-mock-terminal="beta"]'),
    ).toHaveLength(1);
    expect(
      rendered.container.querySelector(".terminal-mobile-slide-current [data-mock-terminal]")
        ?.getAttribute("data-mock-terminal"),
    ).toBe("beta");
  });

  it("springs back under the settle distance without attaching", async () => {
    await seed([WAITING, ALPHA, BETA, EXITED]);
    const { current, stage } = renderRoutedPager("alpha");
    await settle();

    swipe(stage(), 170);
    await settle();
    await new Promise((resolve) => setTimeout(resolve, 10));

    expect(current()).toBe("alpha");
    expect(terminalLog.openAttach).toEqual(["alpha"]);
  });

  it("keeps the soft keyboard up across a swipe", async () => {
    await seed([WAITING, ALPHA, BETA, EXITED]);
    const { rendered, current, stage } = renderRoutedPager("alpha");
    await settle();
    rendered.container
      .querySelector<HTMLTextAreaElement>('[data-mock-input="alpha"]')!
      .focus();

    swipe(stage(), 100);
    await waitFor(() => expect(current()).toBe("beta"));
    await settle();

    expect(document.activeElement).toBe(
      rendered.container.querySelector('[data-mock-input="beta"]'),
    );
  });

  it("freezes the pager order while a finger is down", async () => {
    await seed([WAITING, ALPHA, BETA, EXITED]);
    const { rendered, stage } = renderRoutedPager("alpha");
    await settle();

    fireEvent.touchStart(stage(), { touches: [{ clientX: 200, clientY: 100 }] });
    // beta jumps ahead in attention order mid-gesture.
    await seed([WAITING, session("beta", "waiting-for-input", null, "2026-08-30T10:09:00Z"), ALPHA, EXITED]);
    fireEvent.touchMove(stage(), { touches: [{ clientX: 100, clientY: 102 }] });

    expect(
      rendered.container.querySelector(".terminal-mobile-slide-destination [data-mock-terminal]")
        ?.getAttribute("data-mock-terminal"),
    ).toBe("beta");
  });
});
