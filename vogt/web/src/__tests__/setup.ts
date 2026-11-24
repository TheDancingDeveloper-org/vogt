// What jsdom does not provide, and every Vogt surface assumes.
//
// Nothing here fakes a Vogt response — that is `harness.tsx`'s job, per test.
// This file only makes the environment survive a mount: a surface that
// crashed on a missing `ResizeObserver` would fail every assertion for a
// reason that has nothing to do with the requirement under test.

import "@testing-library/jest-dom/vitest";
import { afterEach, beforeEach } from "vitest";
import { cleanup, configure } from "@solidjs/testing-library";
import { clearEditorDrafts } from "../editorDrafts";
import { clearToolDrafts } from "../toolDrafts";
import { clearPendingAction } from "../pendingAction";
import { resetRailSections } from "../railSections";
import { resetFileTreeState } from "../fileTreeState";
import { invalidate } from "../swr";
import { clearTaxonomyCache } from "../taxonomyCache";
import { resetAccountPreferences } from "../accountPrefs";
import { invalidateAssistantSnapshot } from "../assistantCache";

// waitFor/findBy default to 1 s. A loaded CI runner can take longer to load
// a lazily imported surface (shell.test "opens it for the same link when a
// key is configured" timed out at 1048 ms on PR #816), the same slow-runner
// class vitest.config.ts already gives headroom for. Local runs keep the
// fast default so a real hang still surfaces quickly.
// The PWA's tsconfig carries no Node types, so read CI through globalThis.
const onCi = Boolean(
  (globalThis as { process?: { env?: Record<string, string | undefined> } })
    .process?.env?.CI,
);
configure({ asyncUtilTimeout: onCi ? 10_000 : 1_000 });

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
// and the terminal's link back to its work item lives in one.
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
  cleanup();
  localStorage.clear();
  invalidate();
  clearEditorDrafts();
  clearToolDrafts();
  clearPendingAction();
  resetRailSections();
  resetFileTreeState();
  invalidateAssistantSnapshot();
  clearTaxonomyCache();
  resetAccountPreferences();
});
