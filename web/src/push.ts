// Browser-side push notifications wiring.
//
// Flow:
//   1. registerServiceWorker() once at app boot.
//   2. fetchPublicKey() — call /api/push/public-key to get the VAPID key.
//   3. subscribe() / unsubscribe() drive the PushManager and tell the server.
//
// Capacitor native FCM is layered on top of the same /api/push/subscribe
// endpoint — see mobile/ when scaffolded.

import { api, getBase, getToken } from "./api";
import { Capacitor } from "@capacitor/core";
import { isDemoMode, runtimeTransport } from "./runtimeTransport";

export interface PushPublicKey {
  vapid_public_key: string;
  fcm_enabled: boolean;
}

let serviceWorkerMessageHandlerRegistered = false;

export async function registerServiceWorker(): Promise<ServiceWorkerRegistration | null> {
  if (isDemoMode()) return null;
  if (isNativePlatform()) {
    void registerNativePushHandlers();
  }

  if (!("serviceWorker" in navigator)) return null;
  try {
    // sw.js is intentionally unversioned, so ask the browser to revalidate it
    // on every page load. This prevents old offline/push behavior surviving a
    // deployment indefinitely.
    const reg = await navigator.serviceWorker.register("/sw.js", {
      scope: "/",
      updateViaCache: "none",
    });
    await reg.update().catch(() => undefined);
    // Click messages from sw.js navigate the foreground tab.
    if (!serviceWorkerMessageHandlerRegistered) {
      serviceWorkerMessageHandlerRegistered = true;
      navigator.serviceWorker.addEventListener("message", (ev) => {
        if (ev.data?.type === "navigate" && typeof ev.data.url === "string") {
          navigateFromPushUrl(ev.data.url as string);
        }
      });
    }
    return reg;
  } catch {
    return null;
  }
}

export type PushPermissionState =
  | NotificationPermission
  | "prompt"
  | "prompt-with-rationale"
  | "unknown";

export async function pushPermission(): Promise<PushPermissionState> {
  if (isNativePlatform()) {
    try {
      const { PushNotifications } = await import("@capacitor/push-notifications");
      const perm = await PushNotifications.checkPermissions();
      return perm.receive;
    } catch {
      return "unknown";
    }
  }
  if (!("Notification" in window)) return "denied";
  return Notification.permission;
}

export async function requestPushPermission(): Promise<NotificationPermission> {
  if (isDemoMode()) return "denied";
  if (!("Notification" in window)) return "denied";
  if (Notification.permission === "default") {
    return Notification.requestPermission();
  }
  return Notification.permission;
}

export function isPushSupported(): boolean {
  if (isDemoMode()) return false;
  return (
    "serviceWorker" in navigator &&
    "PushManager" in window &&
    "Notification" in window
  );
}

export function isPushAvailable(): boolean {
  return isNativePlatform() || isPushSupported();
}

/**
 * iOS (and iPadOS 13+, which reports itself as a Mac) grants the Push API only
 * to a PWA that has been installed to the Home Screen. Detecting the device
 * lets Settings say "Add to Home Screen" instead of a dead-end "Not supported".
 */
export function isIOS(): boolean {
  const ua = navigator.userAgent || "";
  const iOSUA = /iPad|iPhone|iPod/.test(ua);
  const iPadOS =
    navigator.platform === "MacIntel" && (navigator.maxTouchPoints ?? 0) > 1;
  return iOSUA || iPadOS;
}

/** True when the PWA is running in an installed / standalone display mode. */
export function isStandalonePwa(): boolean {
  const legacyStandalone = (navigator as unknown as { standalone?: boolean })
    .standalone;
  const displayStandalone = Boolean(
    window.matchMedia?.("(display-mode: standalone)")?.matches,
  );
  return displayStandalone || Boolean(legacyStandalone);
}

export type PushSupportReason = "supported" | "ios-home-screen" | "unsupported";

/**
 * Why push is (un)available, so the UI can distinguish "this browser can never"
 * from "install this to the Home Screen and it can".
 */
export function pushSupportReason(): PushSupportReason {
  if (isNativePlatform() || isPushSupported()) return "supported";
  if (isIOS() && !isStandalonePwa()) return "ios-home-screen";
  return "unsupported";
}

/**
 * A human-readable device label derived from the platform — "Chrome · Android"
 * rather than a 60-character slice of the raw user-agent string, which read as
 * gibberish in the subscribed-devices list.
 */
export function defaultDeviceLabel(): string {
  const ua = navigator.userAgent || "";
  const browser = /Edg\//.test(ua)
    ? "Edge"
    : /OPR\/|Opera/.test(ua)
      ? "Opera"
      : /Firefox\//.test(ua)
        ? "Firefox"
        : /Chrome\//.test(ua)
          ? "Chrome"
          : /Safari\//.test(ua)
            ? "Safari"
            : "Browser";
  const os = /Android/.test(ua)
    ? "Android"
    : /iPhone|iPad|iPod/.test(ua)
      ? "iOS"
      : /Windows/.test(ua)
        ? "Windows"
        : /Mac OS X|Macintosh/.test(ua)
          ? "macOS"
          : /Linux/.test(ua)
            ? "Linux"
            : "device";
  return `${browser} · ${os}`;
}

async function getRegistration(): Promise<ServiceWorkerRegistration | null> {
  if (!("serviceWorker" in navigator)) return null;
  const reg = await navigator.serviceWorker.ready.catch(() => null);
  return reg;
}

export async function currentSubscription(): Promise<PushSubscription | null> {
  const reg = await getRegistration();
  if (!reg) return null;
  return reg.pushManager.getSubscription();
}

const WEB_SUB_ID_KEY = "vogt.push.webSubId";
const NATIVE_SUB_ID_KEY = "vogt.push.nativeSubId";

export async function currentPushEnabled(): Promise<boolean> {
  if (isNativePlatform()) {
    return Boolean(localStorage.getItem(NATIVE_SUB_ID_KEY));
  }
  return (await currentSubscription()) !== null;
}

async function currentWebSubscriptionId(): Promise<string | null> {
  const sub = await currentSubscription();
  if (!sub) return null;
  const idBytes = await crypto.subtle.digest(
    "SHA-256",
    new TextEncoder().encode(sub.endpoint),
  );
  return Array.from(new Uint8Array(idBytes))
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");
}

export async function currentPushSubscriptionId(): Promise<string | null> {
  if (isNativePlatform()) {
    return localStorage.getItem(NATIVE_SUB_ID_KEY);
  }
  return currentWebSubscriptionId();
}

function urlBase64ToUint8Array(base64String: string): Uint8Array {
  const padding = "=".repeat((4 - (base64String.length % 4)) % 4);
  const b64 = (base64String + padding).replace(/-/g, "+").replace(/_/g, "/");
  const raw = atob(b64);
  const out = new Uint8Array(raw.length);
  for (let i = 0; i < raw.length; i++) out[i] = raw.charCodeAt(i);
  return out;
}

function arrayBufferToBase64Url(buf: ArrayBuffer): string {
  const bytes = new Uint8Array(buf);
  let bin = "";
  for (let i = 0; i < bytes.byteLength; i++) bin += String.fromCharCode(bytes[i]!);
  return btoa(bin).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

async function fetchPublicKey(): Promise<PushPublicKey> {
  const r = await runtimeTransport().request(`${getBase()}/api/push/public-key`);
  if (!r.ok) throw new Error(`public-key fetch: ${r.status}`);
  return r.json();
}

export async function subscribePush(label?: string): Promise<{
  id: string;
}> {
  if (!isPushSupported()) throw new Error("push not supported in this browser");
  const perm = await requestPushPermission();
  if (perm !== "granted") throw new Error(`notification permission ${perm}`);

  const reg = await getRegistration();
  if (!reg) throw new Error("service worker not registered");

  let sub = await reg.pushManager.getSubscription();
  if (!sub) {
    const { vapid_public_key } = await fetchPublicKey();
    if (!vapid_public_key) throw new Error("server VAPID key not configured");
    // Some DOM lib versions type applicationServerKey as BufferSource keyed on
    // strict ArrayBuffer; the runtime accepts any TypedArray. Pass `.buffer`.
    sub = await reg.pushManager.subscribe({
      userVisibleOnly: true,
      applicationServerKey: urlBase64ToUint8Array(vapid_public_key).buffer as ArrayBuffer,
    });
  }

  const json = sub.toJSON();
  const body = {
    kind: "web-push" as const,
    endpoint: json.endpoint!,
    p256dh: json.keys?.p256dh ?? arrayBufferToBase64Url(sub.getKey("p256dh")!),
    auth: json.keys?.auth ?? arrayBufferToBase64Url(sub.getKey("auth")!),
    label,
  };
  const tok = getToken();
  const r = await runtimeTransport().request(`${getBase()}/api/push/subscribe`, {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      ...(tok ? { Authorization: `Bearer ${tok}` } : {}),
    },
    body: JSON.stringify(body),
  });
  if (!r.ok) throw new Error(`subscribe: ${r.status} ${await r.text()}`);
  const registered = (await r.json()) as { id: string };
  localStorage.setItem(WEB_SUB_ID_KEY, registered.id);
  return registered;
}

export async function unsubscribePush(): Promise<void> {
  const reg = await getRegistration();
  if (!reg) return;
  const sub = await reg.pushManager.getSubscription();
  if (!sub) return;
  const id = (await currentWebSubscriptionId()) ?? localStorage.getItem(WEB_SUB_ID_KEY);
  await sub.unsubscribe();

  const tok = getToken();
  if (id) {
    await runtimeTransport().request(`${getBase()}/api/push/unsubscribe`, {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        ...(tok ? { Authorization: `Bearer ${tok}` } : {}),
      },
      body: JSON.stringify({ id }),
    });
  }
  localStorage.removeItem(WEB_SUB_ID_KEY);
}

export async function subscribePushNotifications(label?: string): Promise<{ id: string }> {
  return isNativePlatform() ? subscribeNativeFcm(label) : subscribePush(label);
}

export async function unsubscribePushNotifications(): Promise<void> {
  if (isNativePlatform()) {
    await unsubscribeNativeFcm();
    return;
  }
  await unsubscribePush();
}

export async function pushSelfTest(): Promise<{ ok: number; fail: number; queued: number }> {
  return api.pushTest();
}

// ---------------------------------------------------------------------------
// Capacitor / native FCM integration.
//
// When the PWA is loaded inside the Android Capacitor wrap, the browser
// PushManager path is replaced with the native FCM plugin. The plugin
// returns a device token which we POST as `kind: "fcm"` — the server
// handles dispatch via FCM HTTP v1.
// ---------------------------------------------------------------------------

interface CapacitorGlobal {
  isNativePlatform?: () => boolean;
}

export function isNativePlatform(): boolean {
  const w = window as unknown as { Capacitor?: CapacitorGlobal };
  return Boolean(w.Capacitor?.isNativePlatform?.());
}

function navigateFromPushUrl(url: string) {
  // Only same-origin/relative navigation from a push payload (#522). The URL
  // is server-supplied; feeding a scheme (`javascript:`, or an off-origin
  // `http(s):`) or a protocol-relative `//host` into the raw `location.href`
  // sink would be an open-redirect / script sink the moment any notification
  // URL is derived from work-item content. Relative paths only.
  if (url.startsWith("/#")) {
    location.hash = url.slice(1);
  } else if (url.startsWith("/") && !url.startsWith("//")) {
    location.hash = `#${url}`;
  } else {
    console.warn("ignoring push navigation to a non-relative URL");
  }
}

// Keep the setup promise, rather than only a boolean, because Capacitor's
// `addListener` is asynchronous. A subscription that starts while the boot
// registration is still installing its action listener must wait for that
// setup to finish before it installs its own FCM listeners.
let nativeHandlersSetup: Promise<void> | null = null;
let nativeChannelCreated = false;

async function ensureNativeNotificationChannel() {
  if (nativeChannelCreated) return;
  if (Capacitor.getPlatform() !== "android") {
    nativeChannelCreated = true;
    return;
  }
  const { PushNotifications } = await import("@capacitor/push-notifications");
  await PushNotifications.createChannel({
    // Keep the historical id: changing it would strand user settings on an
    // obsolete channel. Android permits its visible name/description to be
    // refreshed in place when this channel is created again during upgrade.
    id: "mydevenv2-alerts",
    name: "Vogt alerts",
    description: "Session, work, and assistant alerts from Vogt",
    importance: 5,
    visibility: 1,
    sound: "default",
    lights: true,
    vibration: true,
  });
  nativeChannelCreated = true;
}

function registerNativePushHandlers(): Promise<void> {
  if (nativeHandlersSetup) return nativeHandlersSetup;

  const setup = (async () => {
    const { PushNotifications } = await import("@capacitor/push-notifications");
    // Create the channel on every native boot as well as during subscription.
    // This covers APKs that already have a stored FCM token from before the
    // channel was introduced.
    await ensureNativeNotificationChannel();
    await PushNotifications.addListener("pushNotificationActionPerformed", (action) => {
      const url = action.notification.data?.url;
      if (typeof url === "string" && url.length > 0) {
        navigateFromPushUrl(url);
      }
    });
  })();
  // The action listener is best-effort, as it was before this became
  // awaitable. A failed setup may be retried by a later subscription, while
  // concurrent callers still share the same in-flight attempt.
  nativeHandlersSetup = setup.catch(() => {
    nativeHandlersSetup = null;
  });
  return nativeHandlersSetup;
}

// Subscribe via Capacitor's native FCM plugin. Resolves with the server-side
// subscription id once the token is registered AND posted to /api/push/subscribe.
export async function subscribeNativeFcm(label?: string): Promise<{ id: string }> {
  if (!isNativePlatform()) {
    throw new Error("not running inside Capacitor");
  }
  await registerNativePushHandlers();

  // Dynamic import — the plugin is only needed inside the native wrap.
  const { PushNotifications } = await import("@capacitor/push-notifications");

  const perm = await PushNotifications.requestPermissions();
  if (perm.receive !== "granted") {
    throw new Error(`native notification permission ${perm.receive}`);
  }
  await ensureNativeNotificationChannel();

  // Wait once for the `registration` event to fire with our FCM token. Attach
  // listeners before register() so a fast native callback cannot be missed.
  let registrationHandle: { remove: () => Promise<void> } | undefined;
  let errorHandle: { remove: () => Promise<void> } | undefined;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let settled = false;
  let resolveToken: (token: string) => void = () => {};
  let rejectToken: (error: Error) => void = () => {};
  const tokenPromise = new Promise<string>((resolve, reject) => {
    resolveToken = resolve;
    rejectToken = reject;
  });
  const cleanup = () => {
    if (timer !== undefined) clearTimeout(timer);
    void registrationHandle?.remove();
    void errorHandle?.remove();
  };
  const settle = (fn: () => void) => {
    if (settled) return;
    settled = true;
    cleanup();
    fn();
  };
  // Do not call `register()` until both listener-registration promises have
  // resolved. Capacitor can deliver a fast token/error callback as soon as
  // register starts; firing it before these promises resolve loses that
  // callback and leaves the subscription waiting until its timeout.
  const listenersReady = Promise.all([
    // Start both registrations before awaiting either one. The important
    // ordering guarantee is that `register()` waits for both handles; doing
    // this concurrently also avoids making the second listener depend on an
    // implementation detail of the first one.
    PushNotifications.addListener("registration", (t) =>
      settle(() => resolveToken(t.value)),
    ).then((handle) => {
      registrationHandle = handle;
    }),
    PushNotifications.addListener("registrationError", (err) =>
      settle(() => rejectToken(new Error(`FCM registration error: ${JSON.stringify(err)}`))),
    ).then((handle) => {
      errorHandle = handle;
    }),
  ]).catch((err: unknown) => {
    settle(() => rejectToken(new Error(`FCM listener error: ${JSON.stringify(err)}`)));
    throw err;
  });
  timer = setTimeout(
    () => settle(() => rejectToken(new Error("FCM registration timed out"))),
    15_000,
  );
  try {
    await listenersReady;
    await PushNotifications.register();
  } catch (e) {
    cleanup();
    throw e;
  }
  const token = await tokenPromise;

  const tok = getToken();
  const r = await runtimeTransport().request(`${getBase()}/api/push/subscribe`, {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      ...(tok ? { Authorization: `Bearer ${tok}` } : {}),
    },
    body: JSON.stringify({ kind: "fcm", token, label }),
  });
  if (!r.ok) throw new Error(`subscribe: ${r.status} ${await r.text()}`);
  const registered = (await r.json()) as { id: string };
  localStorage.setItem(NATIVE_SUB_ID_KEY, registered.id);
  return registered;
}

export async function unsubscribeNativeFcm(): Promise<void> {
  if (!isNativePlatform()) {
    throw new Error("not running inside Capacitor");
  }

  const id = localStorage.getItem(NATIVE_SUB_ID_KEY);
  const { PushNotifications } = await import("@capacitor/push-notifications");

  try {
    await PushNotifications.unregister();
  } finally {
    if (id) {
      const tok = getToken();
      await runtimeTransport().request(`${getBase()}/api/push/unsubscribe`, {
        method: "POST",
        headers: {
          "Content-Type": "application/json",
          ...(tok ? { Authorization: `Bearer ${tok}` } : {}),
        },
        body: JSON.stringify({ id }),
      });
    }
    localStorage.removeItem(NATIVE_SUB_ID_KEY);
    localStorage.removeItem(WEB_SUB_ID_KEY);
  }
}
