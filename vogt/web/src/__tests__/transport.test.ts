import { describe, it, expect, vi, afterEach } from "vitest";
import { fetchWithRetry, TransportError } from "../transport";
import { _resetConnectionHealth, isCircuitOpen } from "../connectionHealth";

function netError(): TypeError {
  // What a browser throws when the stream is reset before a response — the raw
  // string that used to leak into surfaces.
  return new TypeError("NetworkError when attempting to fetch resource.");
}

function abortError(): DOMException {
  return new DOMException("The operation was aborted.", "AbortError");
}

function okResponse(): Response {
  return new Response("{}", { status: 200 });
}

afterEach(() => {
  vi.restoreAllMocks();
  // The breaker is process-wide global state; keep each case independent.
  _resetConnectionHealth();
});

describe("fetchWithRetry (dropped-connection retry)", () => {
  it("retries a GET twice then throws a typed TransportError, not the raw TypeError", async () => {
    const fetchMock = vi.fn().mockRejectedValue(netError());
    vi.stubGlobal("fetch", fetchMock);

    const promise = fetchWithRetry("/x", { method: "GET" }, { backoffMs: 0 });
    await expect(promise).rejects.toBeInstanceOf(TransportError);
    // A written, actionable reason — not the browser implementation detail.
    await expect(
      fetchWithRetry("/x", { method: "GET" }, { backoffMs: 0 }),
    ).rejects.toThrow(/connection was interrupted/i);
    // first call: 1 initial + 2 retries = 3 attempts
    expect(fetchMock).toHaveBeenCalledTimes(6); // two invocations above, 3 each
  });

  it("returns the response as soon as a retry succeeds", async () => {
    const fetchMock = vi
      .fn()
      .mockRejectedValueOnce(netError())
      .mockResolvedValueOnce(okResponse());
    vi.stubGlobal("fetch", fetchMock);

    const res = await fetchWithRetry("/x", { method: "GET" }, { backoffMs: 0 });
    expect(res.status).toBe(200);
    expect(fetchMock).toHaveBeenCalledTimes(2);
  });

  it("never retries a POST — a blind retry could double-write", async () => {
    const fetchMock = vi.fn().mockRejectedValue(netError());
    vi.stubGlobal("fetch", fetchMock);

    await expect(
      fetchWithRetry("/x", { method: "POST" }, { backoffMs: 0 }),
    ).rejects.toBeInstanceOf(TransportError);
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });

  it("lets a deliberate AbortError pass through untouched (SSE teardown, cancel)", async () => {
    const fetchMock = vi.fn().mockRejectedValue(abortError());
    vi.stubGlobal("fetch", fetchMock);

    await expect(
      fetchWithRetry("/x", { method: "GET" }, { backoffMs: 0 }),
    ).rejects.toSatisfy((e: unknown) => (e as DOMException).name === "AbortError");
    // No retry on an abort.
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });

  it("does not retry once the caller's signal is already aborted", async () => {
    const controller = new AbortController();
    controller.abort();
    const fetchMock = vi.fn().mockRejectedValue(netError());
    vi.stubGlobal("fetch", fetchMock);

    await expect(
      fetchWithRetry(
        "/x",
        { method: "GET", signal: controller.signal },
        { backoffMs: 0 },
      ),
    ).rejects.toBeTruthy();
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });

  it("passes an HTTP error response through unchanged — it is not a transport failure", async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response("nope", { status: 500 }));
    vi.stubGlobal("fetch", fetchMock);

    const res = await fetchWithRetry("/x", { method: "GET" }, { backoffMs: 0 });
    expect(res.status).toBe(500);
    // A 5xx resolves; it must NOT be retried here (VogtUnavailable/ApiError own it).
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });

  it("defaults an init with no method to a retryable GET", async () => {
    const fetchMock = vi.fn().mockRejectedValue(netError());
    vi.stubGlobal("fetch", fetchMock);

    await expect(fetchWithRetry("/x", {}, { backoffMs: 0 })).rejects.toBeInstanceOf(
      TransportError,
    );
    expect(fetchMock).toHaveBeenCalledTimes(3);
  });

  it("aborts a hung attempt at its deadline and reports a recoverable transport error", async () => {
    vi.useFakeTimers();
    const fetchMock = vi.fn((_url: RequestInfo | URL, init?: RequestInit) =>
      new Promise<Response>((_, reject) => {
        init?.signal?.addEventListener("abort", () => reject(init.signal?.reason));
      }),
    );
    vi.stubGlobal("fetch", fetchMock);

    const expectation = expect(fetchWithRetry(
      "/x",
      { method: "GET" },
      { deadlineMs: 25, retries: 0, backoffMs: 0 },
    )).rejects.toBeInstanceOf(TransportError);
    await vi.advanceTimersByTimeAsync(25);
    await expectation;
    expect(fetchMock).toHaveBeenCalledTimes(1);
    vi.useRealTimers();
  });

  it("does not retry an attempt that exceeded its deadline", async () => {
    // A slow server and a dead socket look the same from here, and a blind
    // retry costs a slow server a whole read per attempt: on prod one badge
    // refresh was three 8 s reads, each abandoned by the browser and each
    // run to completion by the core. The deadline is the caller's signal.
    vi.useFakeTimers();
    const fetchMock = vi.fn((_url: RequestInfo | URL, init?: RequestInit) =>
      new Promise<Response>((_, reject) => {
        init?.signal?.addEventListener("abort", () => reject(init.signal?.reason));
      }),
    );
    vi.stubGlobal("fetch", fetchMock);

    const expectation = expect(fetchWithRetry(
      "/x",
      { method: "GET" },
      { deadlineMs: 25, backoffMs: 0 },
    )).rejects.toBeInstanceOf(TransportError);
    await vi.advanceTimersByTimeAsync(25);
    await expectation;
    expect(fetchMock).toHaveBeenCalledTimes(1);
    vi.useRealTimers();
  });

  it("aborts a retry backoff when the caller supersedes the read", async () => {
    vi.useFakeTimers();
    const controller = new AbortController();
    const fetchMock = vi.fn().mockRejectedValue(netError());
    vi.stubGlobal("fetch", fetchMock);

    const promise = fetchWithRetry(
      "/x",
      { method: "GET", signal: controller.signal },
      { backoffMs: 1_000 },
    );
    await Promise.resolve();
    controller.abort();
    await expect(promise).rejects.toBeTruthy();
    expect(fetchMock).toHaveBeenCalledTimes(1);
    vi.useRealTimers();
  });
});

describe("fetchWithRetry (circuit breaker for a sustained outage, #681)", () => {
  it("stops retrying once a run of wire failures opens the breaker", async () => {
    const fetchMock = vi.fn().mockRejectedValue(netError());
    vi.stubGlobal("fetch", fetchMock);

    // Four consecutive wire failures cross the threshold and open the breaker.
    for (let i = 0; i < 4; i += 1) {
      await expect(
        fetchWithRetry("/x", { method: "GET" }, { backoffMs: 0 }),
      ).rejects.toBeInstanceOf(TransportError);
    }
    expect(isCircuitOpen()).toBe(true);
    // 4 calls x 3 attempts each, all while the breaker was still closed.
    expect(fetchMock).toHaveBeenCalledTimes(12);

    // A further GET now fires ONCE, not three times — the storm is capped.
    fetchMock.mockClear();
    await expect(
      fetchWithRetry("/x", { method: "GET" }, { backoffMs: 0 }),
    ).rejects.toBeInstanceOf(TransportError);
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });

  it("closes the breaker the moment a request succeeds, restoring retries", async () => {
    const fetchMock = vi.fn().mockRejectedValue(netError());
    vi.stubGlobal("fetch", fetchMock);
    for (let i = 0; i < 4; i += 1) {
      await fetchWithRetry("/x", { method: "GET" }, { backoffMs: 0 }).catch(() => {});
    }
    expect(isCircuitOpen()).toBe(true);

    // One good answer proves the wire is live again and reopens the floodgates.
    fetchMock.mockResolvedValueOnce(okResponse());
    const res = await fetchWithRetry("/x", { method: "GET" }, { backoffMs: 0 });
    expect(res.status).toBe(200);
    expect(isCircuitOpen()).toBe(false);

    // Retries are back for the next isolated blip.
    fetchMock.mockClear();
    fetchMock.mockRejectedValue(netError());
    await fetchWithRetry("/x", { method: "GET" }, { backoffMs: 0 }).catch(() => {});
    expect(fetchMock).toHaveBeenCalledTimes(3);
  });

  it("treats a 5xx answer as connectivity, so an erroring server never opens the breaker", async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response("x", { status: 503 }));
    vi.stubGlobal("fetch", fetchMock);
    for (let i = 0; i < 6; i += 1) {
      await fetchWithRetry("/x", { method: "GET" }, { backoffMs: 0 });
    }
    // The server answered every time; the wire was never the problem.
    expect(isCircuitOpen()).toBe(false);
  });
});
