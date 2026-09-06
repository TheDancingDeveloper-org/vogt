interface CapacitorGlobal {
  isNativePlatform?: () => boolean;
}

// The Android side exposes the clipboard through a WebMessageListener bound to
// the trusted origin and the main frame (#624), not a synchronous
// JavaScript-interface method. The injected object takes a JSON string and can
// reply with one; a read is a request/reply correlated by id.
interface WebMessageBridge {
  postMessage: (data: string) => void;
  onmessage: ((event: { data: string }) => void) | null;
  addEventListener?: (
    type: "message",
    listener: (event: { data: string }) => void,
  ) => void;
}

interface ClipboardChannel {
  bridge: WebMessageBridge;
  pending: Map<string, (text: string) => void>;
  seq: number;
}

let clipboardChannel: ClipboardChannel | null = null;

function isNativePlatform(): boolean {
  const w = window as unknown as { Capacitor?: CapacitorGlobal };
  return Boolean(w.Capacitor?.isNativePlatform?.());
}

// Resolve (and, once, wire up) the reply channel for the injected bridge. The
// single message listener resolves pending reads by id; a stale channel (a new
// bridge object after a reload) is replaced so we never listen on a dead one.
function androidClipboardChannel(): ClipboardChannel | null {
  const w = window as unknown as { AndroidClipboard?: WebMessageBridge };
  const bridge = w.AndroidClipboard;
  if (!bridge || typeof bridge.postMessage !== "function") return null;
  if (clipboardChannel?.bridge === bridge) return clipboardChannel;

  const channel: ClipboardChannel = { bridge, pending: new Map(), seq: 0 };
  const onMessage = (event: { data: string }) => {
    let msg: { id?: string; text?: string };
    try {
      msg = JSON.parse(event.data);
    } catch {
      return;
    }
    if (msg.id == null) return;
    const resolve = channel.pending.get(msg.id);
    if (resolve) {
      channel.pending.delete(msg.id);
      resolve(msg.text ?? "");
    }
  };
  if (typeof bridge.addEventListener === "function") {
    bridge.addEventListener("message", onMessage);
  } else {
    bridge.onmessage = onMessage;
  }
  clipboardChannel = channel;
  return channel;
}

async function writeViaNavigator(text: string): Promise<void> {
  if (!navigator.clipboard?.writeText) {
    throw new Error("Clipboard API unavailable");
  }
  await navigator.clipboard.writeText(text);
}

async function readViaNavigator(): Promise<string> {
  if (!navigator.clipboard?.readText) {
    throw new Error("Clipboard API unavailable");
  }
  return navigator.clipboard.readText();
}

async function writeViaAndroidBridge(text: string): Promise<void> {
  const channel = androidClipboardChannel();
  if (!channel) {
    throw new Error("Android clipboard bridge unavailable");
  }
  channel.bridge.postMessage(JSON.stringify({ op: "write", value: text }));
}

async function readViaAndroidBridge(): Promise<string> {
  const channel = androidClipboardChannel();
  if (!channel) {
    throw new Error("Android clipboard bridge unavailable");
  }
  const id = String(++channel.seq);
  return new Promise<string>((resolve, reject) => {
    // The native side replies almost immediately; a bounded wait keeps a
    // dropped reply (an unconfigured or torn-down bridge) from hanging a read.
    const timer = setTimeout(() => {
      channel.pending.delete(id);
      reject(new Error("Android clipboard read timed out"));
    }, 3000);
    channel.pending.set(id, (value) => {
      clearTimeout(timer);
      resolve(value);
    });
    channel.bridge.postMessage(JSON.stringify({ op: "read", id }));
  });
}

// Legacy fallback for the browser copy path: a hidden textarea + the
// synchronous `execCommand("copy")`. This is the only copy route that survives
// a non-secure LAN/IP origin, a missing focus, or a denied permission — all
// cases where the async Clipboard API rejects. Kept out of the native path,
// which has its own Android bridge.
function writeViaExecCommand(text: string): void {
  if (typeof document === "undefined" || !document.execCommand) {
    throw new Error("execCommand copy unavailable");
  }
  const ta = document.createElement("textarea");
  ta.value = text;
  // Off-screen but still focusable/selectable: `display:none` or an empty
  // value makes execCommand copy nothing.
  ta.style.position = "fixed";
  ta.style.top = "0";
  ta.style.left = "-9999px";
  ta.style.opacity = "0";
  ta.setAttribute("readonly", "");
  document.body.appendChild(ta);
  try {
    // iOS Safari only selects a contentEditable, non-readonly field.
    ta.contentEditable = "true";
    ta.readOnly = false;
    const range = document.createRange();
    range.selectNodeContents(ta);
    const selection = window.getSelection();
    selection?.removeAllRanges();
    selection?.addRange(range);
    ta.setSelectionRange(0, text.length);
    if (!document.execCommand("copy")) {
      throw new Error("execCommand('copy') returned false");
    }
  } finally {
    ta.remove();
  }
}

export async function writeClipboardText(text: string): Promise<void> {
  if (isNativePlatform() && androidClipboardChannel()) {
    await writeViaAndroidBridge(text);
    return;
  }
  // Browser: the async Clipboard API is the happy path, but it rejects on a
  // non-secure LAN/IP origin, without focus, or when permission is denied.
  // Fall back to the legacy execCommand route before giving up.
  try {
    await writeViaNavigator(text);
  } catch (err) {
    try {
      writeViaExecCommand(text);
    } catch (fallbackErr) {
      // Surface the ORIGINAL async-API reason — it names the actionable cause
      // (permissions / non-secure context); the execCommand symptom is noise.
      const primary = err instanceof Error ? err.message : String(err);
      const secondary =
        fallbackErr instanceof Error ? fallbackErr.message : String(fallbackErr);
      throw new Error(
        `${primary} (clipboard fallback also failed: ${secondary})`,
      );
    }
  }
}

export async function readClipboardText(): Promise<string> {
  if (isNativePlatform() && androidClipboardChannel()) {
    return readViaAndroidBridge();
  }
  return readViaNavigator();
}
