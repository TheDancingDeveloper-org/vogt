import { describe, expect, it } from "vitest";
import { decodeOsc52 } from "../terminalClipboard";

// Base64 of the UTF-8 bytes, exactly as a terminal program encodes OSC 52.
const osc52b64 = (s: string) =>
  btoa(String.fromCharCode(...new TextEncoder().encode(s)));

describe("decodeOsc52", () => {
  it("decodes a system-clipboard (c) write", () => {
    expect(decodeOsc52(`c;${osc52b64("hello world")}`)).toBe("hello world");
  });

  it("round-trips UTF-8", () => {
    const s = "café — 日本語 — 🚀";
    expect(decodeOsc52(`c;${osc52b64(s)}`)).toBe(s);
  });

  it("accepts other selection specs (primary, sets)", () => {
    expect(decodeOsc52(`p;${osc52b64("x")}`)).toBe("x");
    expect(decodeOsc52(`cp;${osc52b64("y")}`)).toBe("y");
  });

  it("does NOT honour a read request — that would leak the clipboard", () => {
    expect(decodeOsc52("c;?")).toBeNull();
    expect(decodeOsc52("p;?")).toBeNull();
  });

  it("returns null for empty, missing-separator, or non-base64 payloads", () => {
    expect(decodeOsc52("c;")).toBeNull();
    expect(decodeOsc52("no-separator")).toBeNull();
    expect(decodeOsc52("c;@@@ not base64 @@@")).toBeNull();
  });
});
