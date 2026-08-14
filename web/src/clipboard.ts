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

export async function writeClipboardText(text: string): Promise<void> {
  if (isNativePlatform() && androidClipboardBridge()) {
    await writeViaAndroidBridge(text);
    return;
  }
  await writeViaNavigator(text);
}

export async function readClipboardText(): Promise<string> {
  if (isNativePlatform() && androidClipboardBridge()) {
    return readViaAndroidBridge();
  }
  return readViaNavigator();
}
