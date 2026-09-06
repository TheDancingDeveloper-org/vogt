import { afterEach, describe, expect, it } from "vitest";
import { readClipboardText, writeClipboardText } from "../clipboard";

// The Android clipboard path now speaks to a WebMessageListener (#624): the PWA
// posts a JSON op and, for a read, awaits a correlated reply. These hold that
// contract; the origin/frame enforcement itself is native and lives in
// MainActivityTest.java.

interface FakeBridge {
  postMessage: (data: string) => void;
  onmessage: ((event: { data: string }) => void) | null;
  addEventListener: (
    type: "message",
    listener: (event: { data: string }) => void,
  ) => void;
}

interface TestWindow {
  Capacitor?: { isNativePlatform?: () => boolean };
  AndroidClipboard?: FakeBridge;
}

function asWindow(): TestWindow {
  return window as unknown as TestWindow;
}

/** A native bridge that records posts and answers a read as the app would. */
function installNativeClipboard(clipboardText: string): { posted: string[] } {
  const posted: string[] = [];
  let handler: ((event: { data: string }) => void) | null = null;
  const bridge: FakeBridge = {
    postMessage: (data: string) => {
      posted.push(data);
      const msg = JSON.parse(data);
      if (msg.op === "read") {
        // Reply on a later tick, exactly as the native side does.
        queueMicrotask(() =>
          handler?.({ data: JSON.stringify({ id: msg.id, text: clipboardText }) }),
        );
      }
    },
    onmessage: null,
    addEventListener: (_type, listener) => {
      handler = listener;
    },
  };
  asWindow().Capacitor = { isNativePlatform: () => true };
  asWindow().AndroidClipboard = bridge;
  return { posted };
}

afterEach(() => {
  delete asWindow().Capacitor;
  delete asWindow().AndroidClipboard;
});

describe("clipboard over the native WebMessageListener bridge (#624)", () => {
  it("posts a write op with the value", async () => {
    const { posted } = installNativeClipboard("");
    await writeClipboardText("hello");
    expect(posted).toEqual([JSON.stringify({ op: "write", value: "hello" })]);
  });

  it("posts a read op and resolves with the native reply", async () => {
    const { posted } = installNativeClipboard("clip-contents");
    const text = await readClipboardText();
    expect(text).toBe("clip-contents");
    expect(posted).toHaveLength(1);
    const sent = JSON.parse(posted[0] ?? "");
    expect(sent.op).toBe("read");
    expect(typeof sent.id).toBe("string");
  });

  it("correlates concurrent reads to their own replies", async () => {
    installNativeClipboard("same-clip");
    const [a, b] = await Promise.all([readClipboardText(), readClipboardText()]);
    expect(a).toBe("same-clip");
    expect(b).toBe("same-clip");
  });
});
