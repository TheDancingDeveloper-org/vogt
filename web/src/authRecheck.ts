// A single 401 is not a sign-out.
//
// A proxy, a tunnel edge, or a stream that dies during an outage can answer
// 401 once and then accept the same credential. Boot already treats one 401
// from `/api/auth/check` as the only confirmed refusal; mid-session reads,
// the event stream, and another tab used to skip that and wipe the token.
//
// So a 401 is a request to look again, once, shared across every caller.
// Only a 401 from that re-check ends the session. A network error, a
// timeout, or any other status keeps it: the transport is the caller's to
// render, and "offline" is still not "signed out".

import { getBase, getToken, type AuthRejection } from "./api";
import { fetchWithRetry } from "./transport";

/** Short enough that a real refusal reaches the gate quickly, long enough
 *  that a blip does not look like one. The brief names 3s. */
export const AUTH_RECHECK_DEADLINE_MS = 3_000;

export type RecheckOutcome = "confirmed" | "kept";

export interface AuthRecheckDeps {
  /** The authenticated ping. Defaults to `GET /api/auth/check`. */
  probe?: (signal: AbortSignal) => Promise<Response>;
  now?: () => number;
}

let inflight: Promise<RecheckOutcome> | null = null;

/** Forget a finished re-check so the next 401 looks again. Tests use this. */
export function resetAuthRecheck(): void {
  inflight = null;
}

async function defaultProbe(signal: AbortSignal): Promise<Response> {
  const token = getToken();
  return fetchWithRetry(
    `${getBase()}/api/auth/check`,
    {
      method: "GET",
      headers: token
        ? { Authorization: `Bearer ${token}`, Accept: "application/json" }
        : { Accept: "application/json" },
      signal,
      cache: "no-store",
    },
    { deadlineMs: AUTH_RECHECK_DEADLINE_MS, retries: 0 },
  );
}

/**
 * One shared look at the credential. Concurrent callers — a panel read and
 * the event stream, or two tabs' listeners in one process — await the same
 * probe. A 401 is `confirmed`; everything else, including a thrown transport
 * error and a missing token (nothing left to confirm), is `kept`.
 *
 * The token check lives inside the try so the `.finally` always clears the
 * shared promise. An early return before that assignment used to pin a
 * stale `kept` until reload, and a later real 401 then never signed out.
 */
export function recheckAuth(deps: AuthRecheckDeps = {}): Promise<RecheckOutcome> {
  if (inflight) return inflight;
  const probe = deps.probe ?? defaultProbe;
  // The body must not run until `inflight` points at this promise. An async
  // function starts immediately, and a synchronous return (no token) runs
  // its `finally` before the caller can assign the promise — which left a
  // finished `kept` in place until reload, so a later real 401 never signed
  // out. The deferred start is what makes the clear see the right promise.
  let run!: Promise<RecheckOutcome>;
  run = new Promise<RecheckOutcome>((resolve) => {
    queueMicrotask(() => {
      void (async () => {
        const controller = new AbortController();
        const timer = setTimeout(
          () => controller.abort(new DOMException("auth re-check deadline", "TimeoutError")),
          AUTH_RECHECK_DEADLINE_MS,
        );
        try {
          if (!getToken()) {
            resolve("kept");
            return;
          }
          const res = await probe(controller.signal);
          resolve(res.status === 401 ? "confirmed" : "kept");
        } catch {
          resolve("kept");
        } finally {
          clearTimeout(timer);
          if (inflight === run) inflight = null;
        }
      })();
    });
  });
  inflight = run;
  return run;
}

/**
 * Decide what a published refusal means.
 *
 * A deliberate sign-out (`status` 0) is already confirmed: the reader asked
 * to leave, and there is nothing to re-check. A 401 is provisional until
 * `/api/auth/check` agrees. Any other status is not a refusal at all.
 *
 * `fromStream` marks a 401 that came only from the event stream. It still
 * joins the shared re-check — a stream is often the first caller — but it
 * never signs out on its own report. The session ends only when that
 * re-check itself returns 401. A deliberate sign-out is not a stream event.
 */
export async function resolveAuthRejection(
  rejection: AuthRejection,
  options: { fromStream?: boolean; confirmed?: boolean } = {},
  deps: AuthRecheckDeps = {},
): Promise<AuthRejection | null> {
  // Already confirmed by a re-check (the event stream publishes only then).
  // Do not probe again: the shell must sign out on this one fact.
  if (options.confirmed && rejection.status === 401) {
    return { ...rejection, confirmed: true };
  }
  if (rejection.status === 0) return options.fromStream ? null : rejection;
  if (rejection.status !== 401) return null;
  const outcome = await recheckAuth(deps);
  if (outcome !== "confirmed") return null;
  return { status: 401, detail: rejection.detail };
}
