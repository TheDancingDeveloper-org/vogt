// What jsdom does not provide, and every Vogt surface assumes.
//
// Nothing here fakes a Vogt response — that is `harness.tsx`'s job, per test.
// This file only makes the environment survive a mount: a surface that
// crashed on a missing `ResizeObserver` would fail every assertion for a
// reason that has nothing to do with the requirement under test.

import "@testing-library/jest-dom/vitest";
import { afterEach, beforeEach } from "vitest";
import { cleanup } from "@solidjs/testing-library";
import { clearEditorDrafts } from "../editorDrafts";
import { clearToolDrafts } from "../toolDrafts";
import { clearPendingAction } from "../pendingAction";
import { resetRailSections } from "../railSections";
import { resetFileTreeState } from "../fileTreeState";

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

// xterm asks the window whether the reader prefers reduced motion before it
// draws anything, and jsdom has no `matchMedia` — the same shape of gap as
// the `ResizeObserver` above. Stubbed to "no preference" rather than left
// undefined, because a terminal that cannot mount cannot be tested at all,
// and FR-U20's return leg lives in one.
if (!window.matchMedia) {
  window.matchMedia = ((query: string) => ({
    matches: false,
    media: query,
    onchange: null,
    addListener() {},
    removeListener() {},
    addEventListener() {},
    removeEventListener() {},
    dispatchEvent: () => false,
  })) as unknown as typeof window.matchMedia;
}

afterEach(() => {
  localStorage.clear();
  clearEditorDrafts();
  clearToolDrafts();
  clearPendingAction();
  resetRailSections();
  resetFileTreeState();
});

// Unmount what the last test mounted.
//
// `@solidjs/testing-library` registers this itself — but only when `afterEach`
// is a global, and this suite deliberately does not run with Vitest's globals.
// So it never registered, and every surface a test file mounted stayed mounted
// and reactive for the rest of the file: a second board answering the first
// board's event, an old drift inbox refetching into the next test's call log.
// Harmless while the assertions were about one container; not harmless at all
// now that FR-U10's tests count the calls a surface makes.
afterEach(() => {
  cleanup();
});
