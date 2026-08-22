import { afterEach, describe, expect, it } from "vitest";
import {
  reconcilePush,
  saveButtonLabel,
  saveDisabled,
} from "../settingsModel";
import { defaultDeviceLabel } from "../push";
import type { PushSubscriptionEntry } from "../api";

function sub(id: string): PushSubscriptionEntry {
  return {
    id,
    label: null,
    created_at: "2026-08-22T00:00:00Z",
    kind: { kind: "web-push" },
    prefs: {
      waiting_for_input: true,
      errored: true,
      idle_stall: false,
      agent_task_started: false,
      agent_task_notify: true,
      drift: true,
      quiet_hours: {
        enabled: false,
        start_minute: 0,
        end_minute: 0,
        utc_offset_minutes: 0,
        digest: true,
      },
    },
    pending_digest_count: 0,
  };
}

describe("reconcilePush", () => {
  it("offers Re-enable when the server dropped a subscription the browser still holds", () => {
    const result = reconcilePush("device-1", [sub("device-2"), sub("device-3")], true);
    expect(result.serverDropped).toBe(true);
    expect(result.offerReEnable).toBe(true);
  });

  it("does not offer Re-enable when the server still lists the current subscription", () => {
    const result = reconcilePush("device-1", [sub("device-1"), sub("device-2")], true);
    expect(result.serverDropped).toBe(false);
    expect(result.offerReEnable).toBe(false);
  });

  it("does not offer Re-enable when the browser is not enabled", () => {
    // No local subscription id and disabled: nothing to reconcile.
    expect(reconcilePush(null, [sub("device-2")], false).offerReEnable).toBe(false);
    // Enabled but no id yet (still resolving) is not a dropped subscription.
    expect(reconcilePush(null, [sub("device-2")], true).offerReEnable).toBe(false);
  });

  it("treats an empty server list as a drop when the browser thinks it is subscribed", () => {
    expect(reconcilePush("device-1", [], true).offerReEnable).toBe(true);
  });
});

describe("saveButtonLabel", () => {
  const base = { token: false, base: false, layout: false, storage: false };

  it("promises validation and reload when the token changes", () => {
    expect(saveButtonLabel({ ...base, token: true })).toBe("Validate, save & reload");
  });

  it("promises validation and reload when the base URL changes", () => {
    expect(saveButtonLabel({ ...base, base: true })).toBe("Validate, save & reload");
  });

  it("promises a reload without validation when only the layout changes", () => {
    expect(saveButtonLabel({ ...base, layout: true })).toBe("Save & reload");
  });

  it("saves preferences in place when only storage limits change", () => {
    expect(saveButtonLabel({ ...base, storage: true })).toBe("Save preferences");
  });

  it("falls back to a plain save when nothing is dirty", () => {
    expect(saveButtonLabel(base)).toBe("Save settings");
  });

  it("prioritises the credential promise over layout/storage", () => {
    expect(saveButtonLabel({ token: true, base: false, layout: true, storage: true })).toBe(
      "Validate, save & reload",
    );
  });
});

describe("saveDisabled", () => {
  it("allows saving with the token field untouched even when blank", () => {
    // The regression #243 fixes: a blank-but-unchanged token blocked every save.
    expect(
      saveDisabled({ checking: false, tokenBlank: true, tokenChanged: false }),
    ).toBe(false);
  });

  it("blocks saving when the user blanks the token they are changing", () => {
    expect(
      saveDisabled({ checking: false, tokenBlank: true, tokenChanged: true }),
    ).toBe(true);
  });

  it("allows saving with an unchanged non-blank token", () => {
    expect(
      saveDisabled({ checking: false, tokenBlank: false, tokenChanged: false }),
    ).toBe(false);
  });

  it("blocks saving while a validation request is in flight", () => {
    expect(
      saveDisabled({ checking: true, tokenBlank: false, tokenChanged: true }),
    ).toBe(true);
  });
});

describe("defaultDeviceLabel", () => {
  const original = navigator.userAgent;
  const setUA = (value: string) => {
    Object.defineProperty(navigator, "userAgent", {
      value,
      configurable: true,
    });
  };

  afterEach(() => setUA(original));

  it("derives a Chrome · Android label rather than a raw user-agent slice", () => {
    setUA(
      "Mozilla/5.0 (Linux; Android 14; Pixel 9) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/128.0 Mobile Safari/537.36",
    );
    expect(defaultDeviceLabel()).toBe("Chrome · Android");
  });

  it("names the browser and OS for a desktop Firefox on Windows", () => {
    setUA("Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:129.0) Gecko/20100101 Firefox/129.0");
    expect(defaultDeviceLabel()).toBe("Firefox · Windows");
  });
});
