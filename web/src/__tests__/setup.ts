// What jsdom does not provide, and every Vogt surface assumes.
//
// Nothing here fakes a Vogt response — that is `harness.tsx`'s job, per test.
// This file only makes the environment survive a mount: a surface that
// crashed on a missing `ResizeObserver` would fail every assertion for a
// reason that has nothing to do with the requirement under test.

import "@testing-library/jest-dom/vitest";
import { afterEach, beforeEach } from "vitest";

class StubResizeObserver implements ResizeObserver {
  observe(): void {}
  unobserve(): void {}
  disconnect(): void {}
}

// jsdom has no layout, so it refuses `scrollTo` loudly. `@solidjs/router`
// calls it on every navigation that does not pass `scroll: false`, which is a
// real thing the surfaces do and not something to route around — so the
// method exists and does nothing, rather than printing a paragraph per test.
if (typeof window !== "undefined") {
  window.scrollTo = () => {};
}

if (!("ResizeObserver" in globalThis)) {
  (globalThis as unknown as { ResizeObserver: typeof ResizeObserver }).ResizeObserver =
    StubResizeObserver as unknown as typeof ResizeObserver;
}

// `Backlog.tsx` and `Board.tsx` both keep per-client state in localStorage —
// saved filters, collapsed columns. jsdom shares one store across a file, so
// a test that saved a filter would hand it to the next one.
beforeEach(() => {
  localStorage.clear();
});

afterEach(() => {
  localStorage.clear();
});
