import { fireEvent, render, screen } from "@solidjs/testing-library";
import { describe, expect, it } from "vitest";
import SurfaceHeader from "../SurfaceHeader";

/** Answer the shell's narrow query the way a phone would, for one test. */
function onAPhone(): () => void {
  const desktop = window.matchMedia;
  window.matchMedia = ((query: string) => ({
    matches: query.includes("max-width: 768px"),
    media: query,
    onchange: null,
    addListener() {},
    removeListener() {},
    addEventListener() {},
    removeEventListener() {},
    dispatchEvent: () => false,
  })) as unknown as typeof window.matchMedia;
  return () => {
    window.matchMedia = desktop;
  };
}

const ORDER = ["title", "honesty", "spacer", "controls", "action", "detail"];

describe("primary surface header grammar", () => {
  it.each([
    ["loading", "Loading — no answer yet"],
    ["data", "Live — updated 2s ago"],
    ["empty", "Covered and empty"],
    ["unavailable", "Unavailable — source did not answer"],
    ["partial", "Partial — one collector failed"],
  ])("keeps the %s honesty state before controls and action", (state, copy) => {
    const mounted = render(() => (
      <SurfaceHeader
        label={`${state} fixture`}
        title={<h1>A deliberately long primary surface title that must wrap</h1>}
        honesty={<p data-state={state}>{copy}</p>}
        controls={(
          <>
            <button type="button">First view</button>
            <button type="button">Second view</button>
            <button type="button">Refresh</button>
          </>
        )}
        action={<button type="button" disabled={state === "unavailable"}>Create</button>}
        detail={<p>Evidence detail stays attached to this answer.</p>}
      />
    ));

    const header = screen.getByRole("banner", { name: `${state} fixture` });
    expect([...header.children].map((child) => child.getAttribute("data-surface-header-slot")))
      .toEqual(ORDER);
    expect(header.querySelector('[data-surface-header-slot="honesty"]'))
      .toHaveTextContent(copy);
    expect(screen.getByRole("button", { name: "Create" }))
      .toHaveProperty("disabled", state === "unavailable");
    mounted.unmount();
  });

  it("omits optional regions without changing the remaining source order", () => {
    render(() => (
      <SurfaceHeader
        label="minimal fixture"
        title={<h1>Minimal</h1>}
        action={<button type="button">Primary action</button>}
      />
    ));

    const header = screen.getByRole("banner", { name: "minimal fixture" });
    expect([...header.children].map((child) => child.getAttribute("data-surface-header-slot")))
      .toEqual(["title", "spacer", "action"]);
  });

  it("retains enabled, disabled and pending primary-action semantics", () => {
    const { unmount } = render(() => (
      <SurfaceHeader
        label="pending fixture"
        title={<h1>Pending</h1>}
        honesty={<p aria-live="polite">Submitting</p>}
        action={<button type="button" disabled aria-busy="true">Creating…</button>}
      />
    ));
    expect(screen.getByRole("button", { name: "Creating…" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Creating…" })).toHaveAttribute("aria-busy", "true");
    unmount();

    render(() => (
      <SurfaceHeader
        label="enabled fixture"
        title={<h1>Ready</h1>}
        action={<button type="button">Create</button>}
      />
    ));
    expect(screen.getByRole("button", { name: "Create" })).toBeEnabled();
  });

  it("folds a narrow client's chrome behind one disclosure, and only when asked", () => {
    const desktop = onAPhone();
    try {
      const kept = render(() => (
        <SurfaceHeader
          label="not chrome"
          title={<h1>Inbox</h1>}
          controls={<button type="button">All sources</button>}
        />
      ));
      // A surface that did not ask keeps its controls where they were: the
      // Inbox's source pills are the surface, not chrome over it.
      expect(screen.queryByRole("button", { name: "View controls" })).toBeNull();
      expect(screen.getByRole("button", { name: "All sources" })).toBeVisible();
      kept.unmount();

      render(() => (
        <SurfaceHeader
          label="chrome"
          title={<h1>Board</h1>}
          collapseControls
          controls={<button type="button">Refresh now</button>}
          detail={<p>How this view stays current</p>}
        />
      ));

      const header = screen.getByRole("banner", { name: "chrome" });
      const slot = (name: string) =>
        header.querySelector<HTMLElement>(`[data-surface-header-slot="${name}"]`)!;
      const toggle = screen.getByRole("button", { name: "View controls" });

      expect(toggle).toHaveAttribute("aria-expanded", "false");
      expect(slot("controls").hidden).toBe(true);
      expect(slot("detail").hidden).toBe(true);
      // Folded, never removed: the reading order is what it always was.
      expect([...header.children].map((child) => child.getAttribute("data-surface-header-slot")))
        .toEqual(["title", "spacer", null, "controls", "detail"]);

      fireEvent.click(toggle);
      expect(slot("controls").hidden).toBe(false);
      expect(slot("detail").hidden).toBe(false);
      expect(screen.getByRole("button", { name: "Fewer controls" }))
        .toHaveAttribute("aria-expanded", "true");
    } finally {
      desktop();
    }
  });
});
