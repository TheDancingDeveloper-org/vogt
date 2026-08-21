// The phone half of a voice conversation (FR-M6, VOICE_POC §3.6).
//
// The mobile shell is thin: the conversation lives in `Assistant.tsx`, and
// this module is the seam to the two native things a backgrounded conversation
// needs on Android —
//
//   1. a foreground service, held only while a conversation is active, so the
//      process (and the assistant socket, audio capture and TTS) survives
//      screen-off, released the moment the conversation ends; and
//   2. a listener for FCM push arrivals, so a message that lands *during* an
//      active conversation can be spoken as well as shown.
//
// Everything here is guarded behind a native-platform check and the presence
// of the `AndroidVoice` bridge (added in `MainActivity.java`, in the same shape
// as `AndroidClipboard`). On the desktop PWA — and in any browser — every
// export is a no-op, so a voice conversation on the desktop is unaffected.

interface CapacitorGlobal {
  isNativePlatform?: () => boolean;
}

/** The bridge MainActivity injects on Android. Absent everywhere else. */
interface AndroidVoiceBridge {
  startConversation?: () => void;
  endConversation?: () => void;
}

/** DOM event MainActivity dispatches when the notification ended the call. */
const VOICE_ENDED_EVENT = "vogt:voice-service-ended";

function isNativePlatform(): boolean {
  const w = window as unknown as { Capacitor?: CapacitorGlobal };
  return Boolean(w.Capacitor?.isNativePlatform?.());
}

function voiceBridge(): AndroidVoiceBridge | null {
  const w = window as unknown as { AndroidVoice?: AndroidVoiceBridge };
  return w.AndroidVoice ?? null;
}

/** Whether the held foreground service is reachable at all (native + bridge). */
export function voiceServiceAvailable(): boolean {
  return isNativePlatform() && voiceBridge() !== null;
}

/**
 * Begin holding the foreground service for an active conversation. No-op off
 * a native platform, or before the bridge exists. The device tester reads the
 * start/end timestamps this logs out of the WebView console to bound the
 * 30-minute battery + socket-survival measurement (FR-M6) — the number itself
 * is a device task, not this code's.
 */
export function startVoiceService(): void {
  const bridge = voiceBridge();
  if (!isNativePlatform() || !bridge?.startConversation) return;
  try {
    bridge.startConversation();
    console.info(`[voice] conversation service started at ${new Date().toISOString()}`);
  } catch (e) {
    // A failed start must not take the conversation down with it — the PWA
    // still works foregrounded, it just will not survive screen-off.
    console.warn(`[voice] could not start conversation service: ${String(e)}`);
  }
}

/** Release the foreground service. No-op off a native platform. Idempotent. */
export function stopVoiceService(): void {
  const bridge = voiceBridge();
  if (!isNativePlatform() || !bridge?.endConversation) return;
  try {
    bridge.endConversation();
    console.info(`[voice] conversation service stopped at ${new Date().toISOString()}`);
  } catch (e) {
    console.warn(`[voice] could not stop conversation service: ${String(e)}`);
  }
}

/**
 * Run `handler` when the notification's "End conversation" action stops the
 * service, so the PWA can close the conversation on its side too. Returns a
 * cleanup that removes the listener. No-op (a cleanup that removes nothing) off
 * a native platform.
 */
export function onVoiceServiceEnded(handler: () => void): () => void {
  if (!isNativePlatform()) return () => {};
  window.addEventListener(VOICE_ENDED_EVENT, handler);
  return () => window.removeEventListener(VOICE_ENDED_EVENT, handler);
}

/** The speakable text of an FCM message: its title and body, whichever exist. */
export function pushNotificationText(notification: {
  title?: string | null;
  body?: string | null;
}): string {
  const title = notification.title?.trim() ?? "";
  const body = notification.body?.trim() ?? "";
  if (title && body) return `${title}. ${body}`;
  return title || body;
}

/**
 * Register a foreground FCM listener whose `handler` receives the speakable
 * text of each arriving push (FR-M6 / FR-M2). The *decision to speak* — only
 * while a conversation is active — belongs to the caller: outside an active
 * conversation the push behaves exactly as FR-M2 already has it, shown and
 * handled, never spoken. Returns a cleanup that removes the listener.
 *
 * No-op off a native platform, and if the push plugin cannot be loaded.
 */
export async function registerPushSpeaker(
  handler: (text: string) => void,
): Promise<() => void> {
  if (!isNativePlatform()) return () => {};
  try {
    const { PushNotifications } = await import("@capacitor/push-notifications");
    const listener = await PushNotifications.addListener(
      "pushNotificationReceived",
      (notification) => {
        const text = pushNotificationText(notification);
        if (text) handler(text);
      },
    );
    return () => {
      void listener.remove();
    };
  } catch {
    // No plugin (desktop, or a stripped build): nothing to speak, nothing to
    // clean up.
    return () => {};
  }
}
