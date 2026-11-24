import { describe, expect, it } from "vitest";
import { TerminalSizeTracker } from "../terminalSize";

const DESK = { cols: 178, rows: 32 };
const PHONE = { cols: 66, rows: 40 };

describe("TerminalSizeTracker (WI-1089)", () => {
  it("draws at the fitted size and asks for it before the engine names a size", () => {
    const t = new TerminalSizeTracker();
    t.fit(PHONE);
    expect(t.target()).toEqual(PHONE);
    expect(t.request()).toEqual(PHONE);
    expect(t.foreign()).toBeNull();
  });

  it("draws a snapshot at the PTY's size, not its own", () => {
    const t = new TerminalSizeTracker();
    t.fit(PHONE);
    t.announce(DESK, true);
    // The stream is painted for 178 columns: drawing it at 66 is the ghost bug.
    expect(t.target()).toEqual(DESK);
    // A freshly opened pane still owns the size and asks for its own.
    expect(t.owner).toBe(true);
    expect(t.request()).toEqual(PHONE);
    t.announce(PHONE, false);
    expect(t.target()).toEqual(PHONE);
    expect(t.owner).toBe(true);
  });

  it("follows another viewer's resize and stops asking for its own", () => {
    const phone = new TerminalSizeTracker();
    phone.fit(PHONE);
    phone.announce(PHONE, true);
    expect(phone.request()).toBeNull(); // already the PTY's size

    phone.announce(DESK, false); // the desktop took the size
    expect(phone.owner).toBe(false);
    expect(phone.target()).toEqual(DESK);
    expect(phone.request()).toBeNull(); // an idle refit or reconnect takes nothing
    expect(phone.foreign()).toEqual(DESK);

    phone.claim(); // the person typed or tapped "fit" on the phone
    expect(phone.request()).toEqual(PHONE);
    phone.announce(PHONE, false);
    expect(phone.owner).toBe(true);
    expect(phone.foreign()).toBeNull();
  });

  it("treats a late echo of its own earlier request as its own", () => {
    const t = new TerminalSizeTracker();
    t.fit({ cols: 100, rows: 30 });
    t.announce(DESK, true);
    t.request();
    t.fit({ cols: 120, rows: 30 });
    t.request();
    t.announce({ cols: 100, rows: 30 }, false);
    expect(t.owner).toBe(true);
    t.announce({ cols: 120, rows: 30 }, false);
    expect(t.owner).toBe(true);
    expect(t.target()).toEqual({ cols: 120, rows: 30 });
  });

  it("yields on reattach when the size moved while it was away", () => {
    const desk = new TerminalSizeTracker();
    desk.fit(DESK);
    desk.announce(DESK, true);
    // Disconnected; the phone resized the PTY and the frame reached no one.
    desk.announce(PHONE, true);
    expect(desk.owner).toBe(false);
    expect(desk.request()).toBeNull();
  });

  it("reports a host resize, not the first fit or a same-size refit", () => {
    const t = new TerminalSizeTracker();
    expect(t.fit(PHONE)).toBe(false);
    expect(t.fit(PHONE)).toBe(false);
    expect(t.fit({ cols: 66, rows: 24 })).toBe(true);
  });
});
