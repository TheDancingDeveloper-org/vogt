import { afterEach, describe, expect, it, vi } from "vitest";

import { api, setToken } from "../api";
import { listLabels, listProjects, transitionWork } from "../vogtApi";
import {
  cacheIdentity,
  cacheKey,
  cachedRead,
  cachedReadWithValidator,
  invalidate,
  STABLE_READ_POLICY,
} from "../swr";

afterEach(() => {
  invalidate();
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

const key = cacheKey(cacheIdentity("https://engine.test", "token-a"), "test");

async function flushMicrotasks(turns = 5): Promise<void> {
  for (let turn = 0; turn < turns; turn += 1) await Promise.resolve();
}

describe("typed client TTL/SWR cache (#419)", () => {
  it("returns a fresh value without repeating the loader", async () => {
    const loader = vi.fn().mockResolvedValue({ value: 1 });

    expect(await cachedRead(key, loader, STABLE_READ_POLICY)).toEqual({ value: 1 });
    expect(await cachedRead(key, loader, STABLE_READ_POLICY)).toEqual({ value: 1 });
    expect(loader).toHaveBeenCalledTimes(1);
  });

  it("serves stale data immediately and single-flights one background refresh", async () => {
    vi.useFakeTimers();
    let release: (value: string) => void = () => {};
    const loader = vi.fn()
      .mockResolvedValueOnce("old")
      .mockImplementationOnce(() => new Promise<string>((resolve) => { release = resolve; }));
    // Keep the retry assertion inside the stale window: after it expires a
    // read must wait for the network and is allowed to surface the outage.
    const policy = { ttlMs: 10, swrMs: 100 };

    expect(await cachedRead(key, loader, policy)).toBe("old");
    vi.advanceTimersByTime(11);

    expect(await cachedRead(key, loader, policy)).toBe("old");
    expect(await cachedRead(key, loader, policy)).toBe("old");
    await Promise.resolve();
    await Promise.resolve();
    expect(loader).toHaveBeenCalledTimes(2);
    release("new");
    await vi.waitFor(() => expect(cachedRead(key, loader, policy)).resolves.toBe("new"));
  });

  it("does not retry a failed SWR refresh without a bounded backoff", async () => {
    vi.useFakeTimers();
    const loader = vi.fn()
      .mockResolvedValueOnce("known")
      .mockRejectedValueOnce(new Error("offline"))
      .mockResolvedValueOnce("recovered");
    // Keep the retry assertion inside the stale window: after it expires a
    // read must wait for the network and is allowed to surface the outage.
    const policy = { ttlMs: 10, swrMs: 2_000 };

    await cachedRead(key, loader, policy);
    vi.advanceTimersByTime(11);
    expect(await cachedRead(key, loader, policy)).toBe("known");
    await flushMicrotasks();
    expect(loader).toHaveBeenCalledTimes(2);
    expect(await cachedRead(key, loader, policy)).toBe("known");
    expect(loader).toHaveBeenCalledTimes(2);

    vi.advanceTimersByTime(1_000);
    expect(await cachedRead(key, loader, policy)).toBe("known");
    await flushMicrotasks();
    expect(loader).toHaveBeenCalledTimes(3);
    expect(await cachedRead(key, loader, policy)).toBe("recovered");
  });

  it("never shares an entry between identities or parameter scopes", async () => {
    const loader = vi.fn()
      .mockResolvedValueOnce("a")
      .mockResolvedValueOnce("b")
      .mockResolvedValueOnce("project-b");
    const identityA = cacheIdentity("https://engine.test", "token-a");
    const identityB = cacheIdentity("https://engine.test", "token-b");

    expect(await cachedRead(cacheKey(identityA, "test"), loader, STABLE_READ_POLICY)).toBe("a");
    expect(await cachedRead(cacheKey(identityB, "test"), loader, STABLE_READ_POLICY)).toBe("b");
    expect(await cachedRead(cacheKey(identityB, "test", { project: "project-b" }), loader, STABLE_READ_POLICY)).toBe("project-b");
    expect(loader).toHaveBeenCalledTimes(3);
  });

  it("invalidates an in-flight result before it can repopulate the cache", async () => {
    let release: (value: string) => void = () => {};
    const loader = vi.fn()
      .mockImplementationOnce(() => new Promise<string>((resolve) => { release = resolve; }))
      .mockResolvedValueOnce("fresh");

    const pending = cachedRead(key, loader, STABLE_READ_POLICY);
    await Promise.resolve();
    await Promise.resolve();
    invalidate();
    release("old");
    await expect(pending).resolves.toBe("old");
    await cachedRead(key, loader, STABLE_READ_POLICY);
    expect(loader).toHaveBeenCalledTimes(2);
  });
});

describe("cache wiring", () => {
  it("caches public config while leaving the existing API return type intact", async () => {
    const fetchMock = vi.fn(async () =>
      new Response(JSON.stringify({ version: "cached" }), { status: 200 }),
    );
    vi.stubGlobal("fetch", fetchMock);

    await expect(api.publicConfig()).resolves.toEqual({ version: "cached" });
    await expect(api.publicConfig()).resolves.toEqual({ version: "cached" });
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });

  it("sends a stable-read validator and reuses the cached body on 304", async () => {
    setToken("token-a");
    let calls = 0;
    const fetchMock = vi.fn(async (_input: RequestInfo | URL, init?: RequestInit) => {
      calls += 1;
      if (calls === 1) {
        return new Response(JSON.stringify({ labels: [{ name: "bug" }] }), {
          status: 200,
          headers: { ETag: 'W/"7"' },
        });
      }
      expect(new Headers(init?.headers).get("If-None-Match")).toBe('W/"7"');
      return new Response(null, { status: 304, headers: { ETag: 'W/"7"' } });
    });
    vi.stubGlobal("fetch", fetchMock);
    const policy = { ttlMs: 0, swrMs: 0 };

    // The public helper uses the production policy; exercise the generic
    // validator path directly so the 304 behavior is deterministic here.
    const validatorKey = cacheKey(
      cacheIdentity("https://engine.test", "token-a"),
      "validator",
    );
    const loader = (etag: string | null) =>
      fetchMock("/validator", { headers: etag ? { "If-None-Match": etag } : {} }).then(
        async (response) => {
          if (response.status === 304) return "not-modified" as const;
          return {
            value: (await response.json()) as { labels: { name: string }[] },
            etag: response.headers.get("etag"),
          };
        },
      );
    await expect(cachedReadWithValidator(validatorKey, loader, policy)).resolves.toEqual({
      labels: [{ name: "bug" }],
    });
    // With a zero TTL/SWR window the second read must revalidate the retained
    // entry immediately, without deleting the validator first.
    await expect(cachedReadWithValidator(validatorKey, loader, policy)).resolves.toEqual({
      labels: [{ name: "bug" }],
    });
    expect(fetchMock).toHaveBeenCalledTimes(2);
  });

  it("invalidates metadata after a successful mutation and on identity change", async () => {
    setToken("token-a");
    const fetchMock = vi.fn(async (_input: RequestInfo | URL, init?: RequestInit) => {
      if (init?.method === "POST") return new Response("{}", { status: 200 });
      return new Response(JSON.stringify({ labels: [] }), { status: 200 });
    });
    vi.stubGlobal("fetch", fetchMock);

    await listLabels();
    await listLabels();
    expect(fetchMock).toHaveBeenCalledTimes(1);

    await transitionWork("WI-1", "done", "test mutation");
    await listLabels();
    expect(fetchMock).toHaveBeenCalledTimes(3);

    await listProjects();
    setToken("token-b");
    await listProjects();
    expect(fetchMock).toHaveBeenCalledTimes(5);
  });
});
