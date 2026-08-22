import { fireEvent, render, screen } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import ModKeyRow from "../ModKeyRow";
import {
  altArmed,
  applyStickyMods,
  armAlt,
  armCtrl,
  ctrlArmed,
  ctrlByte,
  disarmMods,
  STICKY_MOD_TIMEOUT_MS,
} from "../terminalModifiers";

// The sticky-modifier store is a module singleton (one soft keyboard, one
// focused terminal); reset it around every test so state cannot leak.
beforeEach(() => {
  disarmMods();
});
afterEach(() => {
  disarmMods();
  vi.useRealTimers();
});

describe("sticky Ctrl chording (#236)", () => {
  it("turns an armed Ctrl + the next printable char into the control byte, once", () => {
    armCtrl();
    expect(ctrlArmed()).toBe(true);

    // Ctrl+r → ^R (0x12), and the arm is spent.
    expect(applyStickyMods("r")).toBe("\x12");
    expect(ctrlArmed()).toBe(false);

    // The very next char is plain again: the chord fired exactly once.
    expect(applyStickyMods("r")).toBe("r");
  });

  it("chords regardless of case and maps the classic Ctrl punctuation", () => {
    armCtrl();
    expect(applyStickyMods("C")).toBe("\x03"); // Ctrl+C
    armCtrl();
    expect(applyStickyMods("[")).toBe("\x1b"); // Ctrl+[ = Esc
    expect(ctrlByte("a")).toBe("\x01");
    expect(ctrlByte("z")).toBe("\x1a");
  });

  it("leaves multi-byte sequences and control bytes untouched and stays armed", () => {
    armCtrl();
    // An arrow key (ESC [ D) is not a chord target and must not spend the arm.
    expect(applyStickyMods("\x1b[D")).toBe("\x1b[D");
    expect(ctrlArmed()).toBe(true);
    // A subsequent real letter still chords.
    expect(applyStickyMods("d")).toBe("\x04");
  });

  it("disarms itself after the timeout without being used", () => {
    vi.useFakeTimers();
    armCtrl();
    expect(ctrlArmed()).toBe(true);
    vi.advanceTimersByTime(STICKY_MOD_TIMEOUT_MS);
    expect(ctrlArmed()).toBe(false);
    // A char typed after the timeout is plain.
    expect(applyStickyMods("r")).toBe("r");
  });

  it("toggles off when Ctrl is tapped twice", () => {
    armCtrl();
    expect(ctrlArmed()).toBe(true);
    armCtrl();
    expect(ctrlArmed()).toBe(false);
  });
});

describe("sticky Alt / ESC prefix (#236)", () => {
  it("prefixes the next printable char with ESC", () => {
    armAlt();
    expect(altArmed()).toBe(true);
    expect(applyStickyMods("f")).toBe("\x1bf"); // Alt+f → ESC f
    expect(altArmed()).toBe(false);
    expect(applyStickyMods("f")).toBe("f");
  });

  it("combines with Ctrl as ESC + control byte", () => {
    armCtrl();
    armAlt();
    expect(applyStickyMods("f")).toBe("\x1b\x06"); // Alt+Ctrl+f
    expect(ctrlArmed()).toBe(false);
    expect(altArmed()).toBe(false);
  });

  it("times out independently", () => {
    vi.useFakeTimers();
    armAlt();
    vi.advanceTimersByTime(STICKY_MOD_TIMEOUT_MS);
    expect(altArmed()).toBe(false);
  });
});

describe("ModKeyRow modifier buttons", () => {
  it("emits Home/End/PgUp/PgDn escape sequences and arms Ctrl/Alt", () => {
    const sent: string[] = [];
    render(() => <ModKeyRow send={(d) => sent.push(d)} />);

    const press = (label: string) =>
      fireEvent.click(screen.getByRole("button", { name: label }));

    press("Home");
    press("End");
    press("PgUp");
    press("PgDn");
    expect(sent).toEqual(["\x1b[H", "\x1b[F", "\x1b[5~", "\x1b[6~"]);

    // The sticky keys arm the shared store rather than sending bytes.
    press("Ctrl");
    expect(ctrlArmed()).toBe(true);
    press("Alt");
    expect(altArmed()).toBe(true);
    // No extra bytes were sent by arming.
    expect(sent).toHaveLength(4);
  });

  it("chords a modkey button's own printable key through the armed Ctrl", () => {
    const sent: string[] = [];
    render(() => <ModKeyRow send={(d) => sent.push(d)} />);
    const press = (label: string) =>
      fireEvent.click(screen.getByRole("button", { name: label }));

    press("Ctrl");
    press("/");
    // Ctrl+/ has no control mapping, so it falls back to the literal slash and
    // still spends the arm.
    expect(sent).toEqual(["/"]);
    expect(ctrlArmed()).toBe(false);
  });
});
