// @vitest-environment jsdom
// Pins the textarea wiring of the IME guard (WI-2707): each reset trigger,
// the re-attach and programmatic-paste paths, and the 229 keydown rule.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { type CompositionInput, wireCompositionInput } from "../terminalComposition";

let textarea: HTMLTextAreaElement;
let sent: string[];
let clock: number;
let input: CompositionInput;

beforeEach(() => {
  textarea = document.createElement("textarea");
  document.body.appendChild(textarea);
  sent = [];
  clock = 0;
  input = wireCompositionInput(textarea, (d) => sent.push(d), () => clock);
});

afterEach(() => {
  input.dispose();
  textarea.remove();
  vi.restoreAllMocks();
});

const compose = () => textarea.dispatchEvent(new CompositionEvent("compositionstart"));
const commit = (data: string) =>
  textarea.dispatchEvent(new CompositionEvent("compositionend", { data }));

describe("wireCompositionInput", () => {
  it("drops onData while composing", () => {
    compose();
    input.onData("a");
    expect(sent).toEqual([]);
  });

  it("sends the committed text once and swallows xterm's echo", () => {
    compose();
    commit("hello");
    input.onData("hello");
    expect(sent).toEqual(["hello"]);
  });

  it("blur resets a stale composition", () => {
    compose();
    textarea.dispatchEvent(new FocusEvent("blur"));
    input.onData("x");
    expect(sent).toEqual(["x"]);
  });

  it("visibilitychange to hidden resets a stale composition", () => {
    compose();
    vi.spyOn(document, "visibilityState", "get").mockReturnValue("hidden");
    document.dispatchEvent(new Event("visibilitychange"));
    input.onData("x");
    expect(sent).toEqual(["x"]);
  });

  it("visibilitychange to visible leaves the composition alone", () => {
    compose();
    vi.spyOn(document, "visibilityState", "get").mockReturnValue("visible");
    document.dispatchEvent(new Event("visibilitychange"));
    input.onData("x");
    expect(sent).toEqual([]);
  });

  it("a paste event resets a stale composition", () => {
    compose();
    textarea.dispatchEvent(new Event("paste"));
    input.onData("x");
    expect(sent).toEqual(["x"]);
  });

  it("dispose removes the visibility listener", () => {
    input.dispose();
    compose();
    vi.spyOn(document, "visibilityState", "get").mockReturnValue("hidden");
    document.dispatchEvent(new Event("visibilitychange"));
    input.onData("x");
    expect(sent).toEqual([]);
  });

  it("re-attach reset clears a stale composition", () => {
    compose();
    input.reset();
    input.onData("x");
    expect(sent).toEqual(["x"]);
  });

  it("programmatic paste resets, then hands the text to the sink", () => {
    compose();
    const sink = vi.fn();
    input.paste("clip", sink);
    expect(sink).toHaveBeenCalledWith("clip");
    input.onData("x");
    expect(sent).toEqual(["x"]);
  });

  it("programmatic paste of empty text skips the sink but still resets", () => {
    compose();
    const sink = vi.fn();
    input.paste("", sink);
    expect(sink).not.toHaveBeenCalled();
    input.onData("x");
    expect(sent).toEqual(["x"]);
  });

  it("an IME keydown (keyCode 229) keeps the echo armed", () => {
    compose();
    commit("a");
    textarea.dispatchEvent(new KeyboardEvent("keydown", { key: "Unidentified", keyCode: 229 }));
    input.onData("a");
    expect(sent).toEqual(["a"]);
  });

  it("a real keydown clears the echo so a fast repeat lands", () => {
    compose();
    commit("a");
    textarea.dispatchEvent(new KeyboardEvent("keydown", { key: "a", keyCode: 65 }));
    input.onData("a");
    expect(sent).toEqual(["a", "a"]);
  });
});
