// WI-1224: the front door's shed 503 is "busy, ask again", not an outage.
//
// The engine refuses a request past its in-flight cap with a 503, a
// `Retry-After` and `error.code: "front_door_busy"`. The PWA used to read every
// 503 as `VogtUnavailable` and render "Vogt is down". These pin the split:
// a shed is retried after the wait it names (spread so a burst does not come
// back at once), surfaces as `VogtBusy` if it persists, and never touches the
// connection breaker; a real outage — a 502/504, a 503 without the code (even
// one carrying `Retry-After`), or no answer at all — is still an outage.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { ApiError } from "../api";
import {
  BUSY_CODE,
  BUSY_RETRIES,
  VogtBusy,
  VogtUnavailable,
  getWorkMany,
  listWork,
  retryAfterMs,
  transitionWork,
} from "../vogtApi";
import {
  _resetConnectionHealth,
  connectionHealthy,
  isCircuitOpen,
} from "../connectionHealth";

function shed(retryAfter: string | null = "2"): Response {
  const headers: Record<string, string> = { "Content-Type": "application/json" };
  if (retryAfter !== null) headers["Retry-After"] = retryAfter;
  return new Response(
    JSON.stringify({
      error: {
        code: BUSY_CODE,
        message: "this front door already has 16 requests open against vogt-core; retry shortly",
      },
    }),
    { status: 503, headers },
  );
}

function refusal(status: number, retryAfter?: string): Response {
  const headers: Record<string, string> = { "Content-Type": "application/json" };
  if (retryAfter) headers["Retry-After"] = retryAfter;
  return new Response(
    JSON.stringify({ error: { message: "vogt-core did not answer" } }),
    { status, headers },
  );
}

function ok(body: unknown = { items: [] }): Response {
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: { "Content-Type": "application/json" },
  });
}

beforeEach(() => {
  vi.useFakeTimers();
  // No jitter unless a case asks for it: the wait is exactly Retry-After.
  vi.spyOn(Math, "random").mockReturnValue(0);
});

afterEach(() => {
  vi.useRealTimers();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  _resetConnectionHealth();
});

describe("a shed 503", () => {
  it("is asked again after Retry-After and answers as if nothing happened", async () => {
    const fetchMock = vi.fn().mockResolvedValueOnce(shed("2")).mockResolvedValueOnce(ok());
    vi.stubGlobal("fetch", fetchMock);

    const answer = listWork();
    await vi.advanceTimersByTimeAsync(1_999);
    expect(fetchMock).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(1);
    await expect(answer).resolves.toEqual({ items: [] });
    expect(fetchMock).toHaveBeenCalledTimes(2);
  });

  it("spreads the retries of one burst over [wait, 2*wait)", async () => {
    vi.spyOn(Math, "random").mockReturnValue(0.5);
    const fetchMock = vi.fn().mockResolvedValueOnce(shed("2")).mockResolvedValueOnce(ok());
    vi.stubGlobal("fetch", fetchMock);

    const answer = listWork();
    await vi.advanceTimersByTimeAsync(2_999);
    expect(fetchMock).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(1);
    await answer;
    expect(fetchMock).toHaveBeenCalledTimes(2);
  });

  it("surfaces as VogtBusy, not an outage, once its retries are spent", async () => {
    const fetchMock = vi.fn().mockImplementation(() => Promise.resolve(shed("2")));
    vi.stubGlobal("fetch", fetchMock);

    const answer = listWork();
    const settled = expect(answer).rejects.toBeInstanceOf(VogtBusy);
    await vi.advanceTimersByTimeAsync(10_000);
    await settled;
    const error = await answer.catch((e: unknown) => e);
    expect(error).toBeInstanceOf(ApiError);
    expect(error).not.toBeInstanceOf(VogtUnavailable);
    expect((error as VogtBusy).status).toBe(503);
    expect((error as VogtBusy).retryAfterMs).toBe(2_000);
    expect(fetchMock).toHaveBeenCalledTimes(1 + BUSY_RETRIES);
  });

  it("never opens the connection breaker, however many arrive", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockImplementation(() => Promise.resolve(shed("1"))),
    );

    const reads = Array.from({ length: 8 }, () => listWork().catch((e: unknown) => e));
    await vi.advanceTimersByTimeAsync(10_000);
    for (const error of await Promise.all(reads)) expect(error).toBeInstanceOf(VogtBusy);
    expect(isCircuitOpen()).toBe(false);
    expect(connectionHealthy()).toBe(true);
  });

  it("is retried for a write too: the door shed it before the core saw it", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(shed("1"))
      .mockResolvedValueOnce(ok({ item: { ref: "WI-7" } }));
    vi.stubGlobal("fetch", fetchMock);

    const answer = transitionWork("WI-7", "in_progress", "test");
    await vi.advanceTimersByTimeAsync(1_000);
    await answer;
    expect(fetchMock).toHaveBeenCalledTimes(2);
    expect(fetchMock.mock.calls[1]?.[1]?.method).toBe("POST");
  });

  it("stops waiting when its caller gives up", async () => {
    vi.stubGlobal("fetch", vi.fn().mockImplementation(() => Promise.resolve(shed("2"))));
    const controller = new AbortController();

    const answer = getWorkMany(["WI-7"], controller.signal);
    const settled = expect(answer).rejects.toMatchObject({ name: "AbortError" });
    await vi.advanceTimersByTimeAsync(500);
    controller.abort();
    await settled;
  });
});

describe("a real outage", () => {
  it.each([502, 503, 504])("reads a bare %i as VogtUnavailable without retrying", async (status) => {
    const fetchMock = vi.fn().mockResolvedValue(refusal(status));
    vi.stubGlobal("fetch", fetchMock);

    await expect(listWork()).rejects.toBeInstanceOf(VogtUnavailable);
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });

  it("reads a 503 with Retry-After but no busy code as an outage", async () => {
    // The gate's "vogt-core could not be asked about this bearer" carries a
    // Retry-After too; only the code marks a shed.
    const fetchMock = vi.fn().mockResolvedValue(refusal(503, "2"));
    vi.stubGlobal("fetch", fetchMock);

    await expect(listWork()).rejects.toBeInstanceOf(VogtUnavailable);
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });

  it("still opens the breaker when nothing answers at all", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockRejectedValue(new TypeError("NetworkError when attempting to fetch resource.")),
    );

    for (let n = 0; n < 4; n += 1) {
      const read = listWork().catch((e: unknown) => e);
      await vi.advanceTimersByTimeAsync(1_000);
      await read;
    }
    expect(isCircuitOpen()).toBe(true);
    expect(connectionHealthy()).toBe(false);
  });
});

describe("retryAfterMs", () => {
  it("reads delta-seconds and HTTP dates, and bounds both", () => {
    expect(retryAfterMs("2")).toBe(2_000);
    expect(retryAfterMs("5")).toBe(5_000);
    expect(retryAfterMs("3600")).toBe(10_000);
    expect(retryAfterMs("0")).toBe(250);
    const now = Date.parse("2026-10-11T04:00:00Z");
    expect(retryAfterMs("Sun, 11 Oct 2026 04:00:04 GMT", now)).toBe(4_000);
  });

  it("falls back to two seconds when there is nothing usable", () => {
    expect(retryAfterMs(null)).toBe(2_000);
    expect(retryAfterMs("soon")).toBe(2_000);
  });
});
