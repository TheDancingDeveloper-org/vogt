// #592, the DOM half: the terminal host claims vertical touch moves from the
// browser on the first move, and leaves horizontal ones alone for the pager.
// xterm's own gesture scroller is inert under jsdom (no touch device), so
// what is asserted here is our half of the contract, not the buffer moving.
import { describe, expect, it } from "vitest";
import { render, waitFor } from "@solidjs/testing-library";

import Terminal from "../Terminal";
import { fakeVogt } from "./harness";

function touch(type: string, target: Element, x: number, y: number): TouchEvent {
  const point = { clientX: x, clientY: y, identifier: 1, target } as unknown as Touch;
  const event = new Event(type, { bubbles: true, cancelable: true }) as TouchEvent;
  Object.defineProperty(event, "touches", { value: type === "touchend" ? [] : [point] });
  Object.defineProperty(event, "changedTouches", { value: [point] });
  Object.defineProperty(event, "targetTouches", { value: type === "touchend" ? [] : [point] });
  target.dispatchEvent(event);
  return event;
}

async function host(): Promise<HTMLElement> {
  fakeVogt({ "GET /sessions": { body: { sessions: [], engine: null } } });
  const { container } = render(() => <Terminal sessionId="eng-1" />);
  return waitFor(() => {
    const el = container.querySelector<HTMLElement>(".terminal-host");
    expect(el).toBeTruthy();
    expect(el!.querySelector(".xterm")).toBeTruthy();
    return el!;
  });
}

describe("#592 — a vertical swipe over the terminal is the page's, never the browser's", () => {
  it("prevents the default of the first vertical-leaning move", async () => {
    const el = await host();
    touch("touchstart", el, 100, 300);
    const first = touch("touchmove", el, 100, 303);
    expect(first.defaultPrevented).toBe(true);
    const later = touch("touchmove", el, 102, 260);
    expect(later.defaultPrevented).toBe(true);
    touch("touchend", el, 102, 260);
  });

  it("leaves a horizontal-leaning move for the session pager", async () => {
    const el = await host();
    touch("touchstart", el, 100, 300);
    expect(touch("touchmove", el, 105, 301).defaultPrevented).toBe(false);
    expect(touch("touchmove", el, 160, 304).defaultPrevented).toBe(false);
    touch("touchend", el, 160, 304);
  });
});
