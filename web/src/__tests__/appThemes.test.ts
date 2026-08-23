// App-theme store (#299): a selection persisted under a `vogt.` key with a
// one-time `mydevenv2.` fallback, "System" resolving through
// `prefers-color-scheme`, applied to <html data-theme> and broadcast on the
// app-theme channel so the coupled surfaces (terminal, Monaco, theme-color)
// can follow.

import { afterEach, describe, expect, it, vi } from "vitest";
import {
  APP_THEME_CHANGE_EVENT,
  APP_THEME_STORAGE_KEY,
  SYSTEM_SELECTION,
  applyAppTheme,
  getAppThemeSelection,
  prefersDark,
  resolveTheme,
  setAppTheme,
} from "../appThemes";

const KEY = "vogt.appTheme.v1";
const LEGACY_KEY = "mydevenv2.appTheme.v1";

/** Point `matchMedia` at a fixed OS preference for the duration of a test. */
function stubPrefersColorScheme(dark: boolean) {
  window.matchMedia = ((query: string) => ({
    matches: query.includes("dark") ? dark : !dark,
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
  document.documentElement.removeAttribute("data-theme");
});

describe("app-theme selection", () => {
  it("exposes the vogt-prefixed storage key", () => {
    expect(APP_THEME_STORAGE_KEY).toBe(KEY);
  });

  it("defaults to System when nothing is stored", () => {
    expect(getAppThemeSelection()).toBe(SYSTEM_SELECTION);
  });

  it("persists a chosen theme under the vogt key and reads it back", () => {
    setAppTheme("light");
    expect(localStorage.getItem(KEY)).toBe("light");
    expect(getAppThemeSelection()).toBe("light");
  });

  it("discards an unknown stored value and falls back to System", () => {
    localStorage.setItem(KEY, "not-a-theme");
    expect(getAppThemeSelection()).toBe(SYSTEM_SELECTION);
  });

  it("coerces an invalid setAppTheme argument to System", () => {
    setAppTheme("bogus");
    expect(localStorage.getItem(KEY)).toBe(SYSTEM_SELECTION);
    expect(getAppThemeSelection()).toBe(SYSTEM_SELECTION);
  });
});

describe("System follows prefers-color-scheme", () => {
  it("reports the OS preference through prefersDark()", () => {
    stubPrefersColorScheme(true);
    expect(prefersDark()).toBe(true);
    stubPrefersColorScheme(false);
    expect(prefersDark()).toBe(false);
  });

  it("resolves System to dark when the OS prefers dark", () => {
    stubPrefersColorScheme(true);
    expect(resolveTheme(SYSTEM_SELECTION)).toBe("dark");
  });

  it("resolves System to light when the OS prefers light", () => {
    stubPrefersColorScheme(false);
    expect(resolveTheme(SYSTEM_SELECTION)).toBe("light");
  });

  it("resolves a named theme to itself regardless of the OS", () => {
    stubPrefersColorScheme(true);
    expect(resolveTheme("light")).toBe("light");
    expect(resolveTheme("hc-dark")).toBe("hc-dark");
  });
});

describe("applying a theme", () => {
  it("sets data-theme on <html> to the resolved id", () => {
    stubPrefersColorScheme(false);
    applyAppTheme(SYSTEM_SELECTION);
    expect(document.documentElement.getAttribute("data-theme")).toBe("light");

    applyAppTheme("dim");
    expect(document.documentElement.getAttribute("data-theme")).toBe("dim");
  });

  it("broadcasts on the app-theme channel with the selection and resolved id", () => {
    const seen: { selection: string; themeId: string }[] = [];
    const handler = (e: Event) => seen.push((e as CustomEvent).detail);
    window.addEventListener(APP_THEME_CHANGE_EVENT, handler);
    stubPrefersColorScheme(true);
    setAppTheme("light");
    window.removeEventListener(APP_THEME_CHANGE_EVENT, handler);

    expect(seen).toHaveLength(1);
    expect(seen[0]).toEqual({ selection: "light", themeId: "light" });
  });

  it("re-fires the terminal-theme channel so open terminals re-read their preset", () => {
    const spy = vi.fn();
    window.addEventListener("vogt:terminal-theme", spy);
    setAppTheme("dark");
    window.removeEventListener("vogt:terminal-theme", spy);
    expect(spy).toHaveBeenCalled();
  });
});

describe("legacy key fallback", () => {
  it("adopts a legacy mydevenv2 selection once, under the vogt key", () => {
    localStorage.setItem(LEGACY_KEY, "hc-dark");
    expect(getAppThemeSelection()).toBe("hc-dark");
    // Adopted so the next read is direct.
    expect(localStorage.getItem(KEY)).toBe("hc-dark");
  });

  it("prefers the current key over a legacy one", () => {
    localStorage.setItem(KEY, "light");
    localStorage.setItem(LEGACY_KEY, "hc-dark");
    expect(getAppThemeSelection()).toBe("light");
  });

  it("ignores an invalid legacy value", () => {
    localStorage.setItem(LEGACY_KEY, "nope");
    expect(getAppThemeSelection()).toBe(SYSTEM_SELECTION);
  });
});
