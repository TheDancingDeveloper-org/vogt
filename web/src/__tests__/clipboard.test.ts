import { afterEach, describe, expect, it, vi } from "vitest";
import { writeClipboardText } from "../clipboard";

// #354: on a non-secure LAN/IP origin (or with permission denied / no focus)
// `navigator.clipboard.writeText` rejects. The browser copy path must then fall
// back to a hidden-textarea + `execCommand("copy")` before reporting failure,
// and the reported reason must be the actionable async-API one.

function stubClipboard(writeText: () => Promise<void>) {
  Object.defineProperty(navigator, "clipboard", {
    value: { writeText },
    configurable: true,
  });
}

afterEach(() => {
  // Leave navigator clean for the next test; the property is configurable.
  Object.defineProperty(navigator, "clipboard", {
    value: undefined,
    configurable: true,
  });
});

describe("clipboard write fallback (#354)", () => {
  it("uses the execCommand path when the async Clipboard API rejects", async () => {
    const writeText = vi.fn().mockRejectedValue(new Error("permission denied"));
    stubClipboard(writeText);
    const execCommand = vi.fn(() => true);
    document.execCommand = execCommand;

    await expect(writeClipboardText("hello")).resolves.toBeUndefined();

    expect(writeText).toHaveBeenCalledWith("hello");
    expect(execCommand).toHaveBeenCalledWith("copy");
    // The textarea is cleaned up after the copy — nothing left in the DOM.
    expect(document.querySelector("textarea")).toBeNull();
  });

  it("surfaces the async-API reason when both routes fail", async () => {
    stubClipboard(vi.fn().mockRejectedValue(new Error("not allowed: insecure origin")));
    document.execCommand = vi.fn(() => false);

    await expect(writeClipboardText("hello")).rejects.toThrow(
      /not allowed: insecure origin/,
    );
  });

  it("does not touch execCommand when the async API succeeds", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    stubClipboard(writeText);
    const execCommand = vi.fn(() => true);
    document.execCommand = execCommand;

    await writeClipboardText("hello");

    expect(writeText).toHaveBeenCalledWith("hello");
    expect(execCommand).not.toHaveBeenCalled();
  });
});
