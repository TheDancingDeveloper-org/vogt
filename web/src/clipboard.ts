interface CapacitorGlobal {
  isNativePlatform?: () => boolean;
}

interface AndroidClipboardBridge {
  readText?: () => string;
  writeText?: (value: string) => void;
}

function isNativePlatform(): boolean {
  const w = window as unknown as { Capacitor?: CapacitorGlobal };
  return Boolean(w.Capacitor?.isNativePlatform?.());
}

function androidClipboardBridge(): AndroidClipboardBridge | null {
  const w = window as unknown as { AndroidClipboard?: AndroidClipboardBridge };
  return w.AndroidClipboard ?? null;
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
  const bridge = androidClipboardBridge();
  if (!bridge?.writeText) {
    throw new Error("Android clipboard bridge unavailable");
  }
  bridge.writeText(text);
}

async function readViaAndroidBridge(): Promise<string> {
  const bridge = androidClipboardBridge();
  if (!bridge?.readText) {
    throw new Error("Android clipboard bridge unavailable");
  }
  return bridge.readText() ?? "";
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
  if (isNativePlatform() && androidClipboardBridge()) {
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
  if (isNativePlatform() && androidClipboardBridge()) {
    return readViaAndroidBridge();
  }
  return readViaNavigator();
}
