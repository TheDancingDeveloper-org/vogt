import "fake-indexeddb/auto";
import { describe, expect, it, vi } from "vitest";

import { loadTerminalCache } from "../terminalCache";
// Importing the entry point installs its `onmessage` handler on `self`.
import "../terminalCache.worker";

const valid = {
  id: 7,
  sessionId: "msg-s1",
  outputPosition: 3,
  payload: { serialized: "screen" },
};

/** Deliver a message the way the browser does, with the given origin. */
function deliver(data: unknown, origin: string): void {
  const handler = self.onmessage as ((ev: MessageEvent) => void) | null;
  handler?.(new MessageEvent("message", { data, origin }));
}

describe("terminal cache worker message guard", () => {
  it("ignores a message from a foreign origin", async () => {
    const post = vi.spyOn(self, "postMessage").mockImplementation(() => undefined);
    deliver(valid, "https://evil.example");
    await new Promise((r) => setTimeout(r, 20));
    expect(post).not.toHaveBeenCalled();
    expect(await loadTerminalCache("msg-s1")).toBeNull();
  });

  it("handles a same-origin message and acknowledges it", async () => {
    const post = vi.spyOn(self, "postMessage").mockImplementation(() => undefined);
    deliver(valid, self.location.origin);
    await vi.waitFor(() => expect(post).toHaveBeenCalledWith({ id: 7, result: "written" }));
  });

  it("handles a dedicated-worker message, which carries an empty origin", async () => {
    const post = vi.spyOn(self, "postMessage").mockImplementation(() => undefined);
    deliver({ ...valid, id: 8, sessionId: "msg-s2" }, "");
    await vi.waitFor(() => expect(post).toHaveBeenCalledWith({ id: 8, result: "written" }));
  });

  it.each([
    ["null", null],
    ["a string", "terminal"],
    ["a missing sessionId", { ...valid, sessionId: undefined }],
    ["a non-numeric id", { ...valid, id: "7" }],
    ["a fractional outputPosition", { ...valid, outputPosition: 1.5 }],
    ["a missing payload", { ...valid, payload: undefined }],
    ["a payload that is not an object", { ...valid, payload: "screen" }],
    ["a non-string stream", { ...valid, stream: 42 }],
  ])("ignores a malformed message: %s", async (_label, data) => {
    const post = vi.spyOn(self, "postMessage").mockImplementation(() => undefined);
    deliver(data, self.location.origin);
    await new Promise((r) => setTimeout(r, 20));
    expect(post).not.toHaveBeenCalled();
  });
});
