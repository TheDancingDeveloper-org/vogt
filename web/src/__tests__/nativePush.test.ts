import { beforeEach, describe, expect, it, vi } from "vitest";

import { installRuntimeTransport } from "../runtimeTransport";

type Listener = (value: unknown) => void;
type PendingListener = {
  event: string;
  callback: Listener;
  resolve: (handle: { remove: () => Promise<void> }) => void;
};

const push = vi.hoisted(() => ({
  addListener: vi.fn(),
  createChannel: vi.fn(async () => {}),
  register: vi.fn(async () => {}),
  requestPermissions: vi.fn(async () => ({ receive: "granted" })),
}));

vi.mock("@capacitor/core", () => ({
  Capacitor: { getPlatform: () => "android" },
}));

vi.mock("@capacitor/push-notifications", () => ({
  PushNotifications: push,
}));

import { subscribeNativeFcm } from "../push";

function handle() {
  return { remove: vi.fn(async () => {}) };
}

describe("native FCM registration", () => {
  let pending: PendingListener[];
  let active: Map<string, Listener>;

  beforeEach(() => {
    pending = [];
    active = new Map();
    push.addListener.mockImplementation(
      (event: string, callback: Listener) => {
        if (event === "pushNotificationActionPerformed") {
          active.set(event, callback);
          return Promise.resolve(handle());
        }
        return new Promise((resolve) => {
          pending.push({ event, callback, resolve });
        });
      },
    );
    push.register.mockImplementation(async () => {
      // A real native bridge can emit synchronously from register(). Only an
      // installed listener may receive that callback.
      active.get("registration")?.({ value: "fcm-token" });
    });
    push.requestPermissions.mockResolvedValue({ receive: "granted" });
    push.createChannel.mockResolvedValue(undefined);
    localStorage.clear();
    Object.defineProperty(window, "Capacitor", {
      configurable: true,
      value: { isNativePlatform: () => true },
    });
    installRuntimeTransport({
      request: async () => new Response(JSON.stringify({ id: "sub-1" }), { status: 200 }),
      openSocket: () => {
        throw new Error("not used");
      },
    });
  });

  it("installs both FCM listeners before register can emit a fast token", async () => {
    const result = subscribeNativeFcm("Pixel");
    await vi.waitFor(() => expect(pending).toHaveLength(2));
    expect(push.register).not.toHaveBeenCalled();
    expect(pending.map(({ event }) => event)).toEqual(["registration", "registrationError"]);

    for (const listener of pending) {
      active.set(listener.event, listener.callback);
      listener.resolve(handle());
    }

    await expect(result).resolves.toEqual({ id: "sub-1" });
    expect(push.register).toHaveBeenCalledTimes(1);
  });

  it("does not call register while the error listener is still being installed", async () => {
    push.register.mockImplementation(async () => {
      active.get("registrationError")?.({ code: "unavailable" });
    });
    const result = subscribeNativeFcm();
    await vi.waitFor(() => expect(pending).toHaveLength(2));

    expect(push.register).not.toHaveBeenCalled();
    for (const listener of pending) {
      active.set(listener.event, listener.callback);
      listener.resolve(handle());
    }
    await expect(result).rejects.toThrow("FCM registration error");
    expect(push.register).toHaveBeenCalledTimes(1);
  });
});
