// Client diagnostics: batched, clipped, switchable, never throwing.
import { afterEach, describe, expect, it, vi } from "vitest";

import { diag, diagEnabled, flush, resetDiagForTests } from "../diag";
import { api } from "../api";

describe("client diagnostics", () => {
  afterEach(() => {
    resetDiagForTests();
    vi.restoreAllMocks();
  });

  it("batches events and posts them as one request", async () => {
    const spy = vi.spyOn(api, "clientLog").mockResolvedValue(undefined);
    diag("tts.blob", { type: "audio/wav", size: 1234 });
    diag("tts.play.ok", { duration: 10.5 });
    expect(spy).not.toHaveBeenCalled(); // not yet — batched
    flush();
    expect(spy).toHaveBeenCalledTimes(1);
    const batch = spy.mock.calls[0]?.[0] ?? [];
    expect(batch.map((e) => e.event)).toEqual(["tts.blob", "tts.play.ok"]);
    expect(batch[0]?.fields).toEqual({ type: "audio/wav", size: 1234 });
  });

  it("flushes on its own once the batch fills", () => {
    const spy = vi.spyOn(api, "clientLog").mockResolvedValue(undefined);
    for (let i = 0; i < 20; i += 1) diag("e", { i });
    expect(spy).toHaveBeenCalledTimes(1);
  });

  it("clips long values so a stray object cannot bloat a batch", () => {
    const spy = vi.spyOn(api, "clientLog").mockResolvedValue(undefined);
    diag("x", { big: "a".repeat(500), obj: { deep: "y".repeat(500) }, nil: null });
    flush();
    const fields = spy.mock.calls[0]?.[0]?.[0]?.fields ?? {};
    expect(String(fields.big).length).toBeLessThanOrEqual(201);
    expect(String(fields.obj).length).toBeLessThanOrEqual(201);
    expect(fields).not.toHaveProperty("nil");
  });

  it("is on by default and off when the flag says so", () => {
    expect(diagEnabled()).toBe(true);
    localStorage.setItem("vogt.diag", "0");
    expect(diagEnabled()).toBe(false);
    const spy = vi.spyOn(api, "clientLog").mockResolvedValue(undefined);
    diag("ignored");
    flush();
    expect(spy).not.toHaveBeenCalled();
  });

  it("never throws when the post fails", async () => {
    vi.spyOn(api, "clientLog").mockRejectedValue(new Error("offline"));
    diag("x");
    expect(() => flush()).not.toThrow();
    await Promise.resolve();
  });
});
