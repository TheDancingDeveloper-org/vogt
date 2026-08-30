import { fireEvent, render, waitFor } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { SessionSummary } from "../api";
import TerminalWorkspace from "../TerminalWorkspace";
import { refreshSessions } from "../store";
import { fakeVogt, settle } from "./harness";

vi.mock("../Terminal", () => ({
  default: (props: { sessionId: string }) => (
    <div data-mock-terminal={props.sessionId}>{props.sessionId}</div>
  ),
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
      matches: query === "(max-width: 768px)",
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

afterEach(() => vi.unstubAllGlobals());

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
});
