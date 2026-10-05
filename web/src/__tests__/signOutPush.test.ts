// WI-924: a deliberate sign-out ends the device's push subscription with the
// session, so a phone does not keep receiving notifications that open onto a
// login screen — and an unreachable server never keeps the reader signed in.

import { beforeEach, describe, expect, it, vi } from "vitest";

const calls = vi.hoisted(() => [] as string[]);
const state = vi.hoisted(() => ({ pushEnabled: true, unsubscribeFails: false }));

vi.mock("../push", () => ({
  currentPushEnabled: async () => state.pushEnabled,
  unsubscribePushNotifications: async () => {
    calls.push("unsubscribe-push");
    if (state.unsubscribeFails) throw new Error("engine away");
  },
}));
vi.mock("../vogtApi", () => ({
  logout: async () => {
    calls.push("revoke");
    return { revoked: true };
  },
}));
vi.mock("../api", () => ({
  getToken: () => "vogt_token",
  signOut: () => calls.push("sign-out"),
}));

import { signOutAndRevoke } from "../session";

describe("signing out", () => {
  beforeEach(() => {
    calls.length = 0;
    state.pushEnabled = true;
    state.unsubscribeFails = false;
  });

  it("unsubscribes push while the credential still works, then revokes, then clears", async () => {
    await signOutAndRevoke();
    expect(calls).toEqual(["unsubscribe-push", "revoke", "sign-out"]);
  });

  it("leaves push alone when it was never on", async () => {
    state.pushEnabled = false;
    await signOutAndRevoke();
    expect(calls).toEqual(["revoke", "sign-out"]);
  });

  it("still signs out when the push call fails", async () => {
    state.unsubscribeFails = true;
    await signOutAndRevoke();
    expect(calls).toEqual(["unsubscribe-push", "revoke", "sign-out"]);
  });
});
