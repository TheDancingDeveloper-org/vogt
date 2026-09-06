// The phone half of a voice conversation (FR-M6, VOICE_POC §3.6, #192).
//
// The service lifecycle and speak-the-push wiring live behind a native-platform
// guard so the desktop PWA is untouched. These tests hold that line: off a
// native platform every export is inert, and on one the `AndroidVoice` bridge
// is driven exactly as `MainActivity.java` exposes it. Device validation — the
// battery number and the spoken U1–U5 pass — is a person's job, not this
// file's.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// Hoisted so the module factory below can see them.
const { addListener } = vi.hoisted(() => ({ addListener: vi.fn() }));

vi.mock("@capacitor/push-notifications", () => ({
  PushNotifications: { addListener },
}));

import {
  onVoiceServiceEnded,
  pushNotificationText,
  registerPushSpeaker,
  startVoiceService,
  stopVoiceService,
  voiceServiceAvailable,
} from "../voiceService";

interface TestWindow {
  Capacitor?: { isNativePlatform?: () => boolean };
  AndroidVoice?: { postMessage?: (data: string) => void };
}

function asWindow(): TestWindow {
  return window as unknown as TestWindow;
}

// The bridge is a WebMessageListener now (#624): a single postMessage taking a
// JSON op string, not a pair of per-method functions.
const voicePost = vi.fn();

function goNative() {
  asWindow().Capacitor = { isNativePlatform: () => true };
  asWindow().AndroidVoice = { postMessage: voicePost };
}

afterEach(() => {
  delete asWindow().Capacitor;
  delete asWindow().AndroidVoice;
});

describe("voiceService off a native platform", () => {
  beforeEach(() => {
    voicePost.mockClear();
    addListener.mockClear();
  });

  it("reports itself unavailable and every lever is a no-op", async () => {
    expect(voiceServiceAvailable()).toBe(false);
    startVoiceService();
    stopVoiceService();
    expect(voicePost).not.toHaveBeenCalled();
  });

  it("registers no push listener and returns a cleanup that removes nothing", async () => {
    const cleanup = await registerPushSpeaker(() => {});
    expect(addListener).not.toHaveBeenCalled();
    expect(() => cleanup()).not.toThrow();
  });

  it("does not attach the end-of-conversation DOM listener", () => {
    const spy = vi.spyOn(window, "addEventListener");
    const cleanup = onVoiceServiceEnded(() => {});
    expect(spy).not.toHaveBeenCalledWith(
      "vogt:voice-service-ended",
      expect.anything(),
    );
    cleanup();
    spy.mockRestore();
  });
});

describe("voiceService on the native platform", () => {
  beforeEach(() => {
    voicePost.mockClear();
    addListener.mockClear();
    goNative();
  });

  it("holds and releases the foreground service through the bridge", () => {
    expect(voiceServiceAvailable()).toBe(true);
    startVoiceService();
    expect(voicePost).toHaveBeenNthCalledWith(1, JSON.stringify({ op: "start" }));
    stopVoiceService();
    expect(voicePost).toHaveBeenNthCalledWith(2, JSON.stringify({ op: "end" }));
  });

  it("routes the notification's End action to the handler and cleans up", () => {
    const handler = vi.fn();
    const cleanup = onVoiceServiceEnded(handler);
    window.dispatchEvent(new CustomEvent("vogt:voice-service-ended"));
    expect(handler).toHaveBeenCalledTimes(1);
    cleanup();
    window.dispatchEvent(new CustomEvent("vogt:voice-service-ended"));
    expect(handler).toHaveBeenCalledTimes(1);
  });

  it("speaks the text of a push that arrives while a conversation is active", async () => {
    let received: ((n: { title?: string; body?: string }) => void) | undefined;
    addListener.mockImplementation(
      (_event: string, cb: (n: { title?: string; body?: string }) => void) => {
        received = cb;
        return Promise.resolve({ remove: vi.fn(() => Promise.resolve()) });
      },
    );
    const spoken: string[] = [];
    const cleanup = await registerPushSpeaker((text) => spoken.push(text));
    expect(addListener).toHaveBeenCalledWith(
      "pushNotificationReceived",
      expect.any(Function),
    );
    received?.({ title: "Session waiting", body: "answer the prompt" });
    expect(spoken).toEqual(["Session waiting. answer the prompt"]);
    await cleanup();
  });
});

describe("pushNotificationText", () => {
  it("joins title and body, and tolerates either being absent", () => {
    expect(pushNotificationText({ title: "A", body: "B" })).toBe("A. B");
    expect(pushNotificationText({ title: "A", body: null })).toBe("A");
    expect(pushNotificationText({ title: null, body: "B" })).toBe("B");
    expect(pushNotificationText({ title: "", body: "" })).toBe("");
  });
});
