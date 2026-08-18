import { render, screen } from "@solidjs/testing-library";
import { describe, expect, it } from "vitest";
import SurfaceHeader from "../SurfaceHeader";

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
});
