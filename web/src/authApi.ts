// The password login.
//
// Not part of `vogtApi.ts` deliberately, for the reason `installApi.ts` is
// not: that module's route table is checked against Vogt's operation
// registry, and the login is not an operation — it is the unauthenticated
// door the core mounts beside its install surface, passed through the
// engine's front door untouched (`engine/server/src/vogt_core.rs`). What it
// returns is a *session*: a bearer like any other, minted for this person,
// expiring on its own, and revoked by `auth.logout` (which *is* an
// operation, reached through `vogtApi.ts`).

import { ApiError } from "./api";
import { fetchWithRetry } from "./transport";
import { DEADLINE_MS } from "./deadlines";

export interface LoginResult {
  actor: { id: string; identity_ref: string; display_name: string; kind: string };
  token: { id: string; name: string; scopes: string[]; kind?: string; expires_at?: string | null };
  /** The session bearer. Stored where a pasted token lives; never shown. */
  secret: string;
}

/**
 * Exchange a username and password for a session bearer against `base`.
 *
 * Throws `ApiError` with the server's status: 401 for a wrong username or
 * password (the core says which no more than we do), 429 when the username
 * has failed too often and must wait, anything else for an outage. Never
 * `refused()`: this is about a *candidate* credential, not the session.
 */
export async function loginWithPassword(
  username: string,
  password: string,
  base: string,
  sessionName = "browser session",
): Promise<LoginResult> {
  const candidateBase = base.trim().replace(/\/+$/, "");
  const res = await fetchWithRetry(
    `${candidateBase}/api/auth/login`,
    {
      method: "POST",
      headers: { "Content-Type": "application/json", Accept: "application/json" },
      body: JSON.stringify({ username: username.trim(), password, session_name: sessionName }),
    },
    { deadlineMs: DEADLINE_MS.auth, retries: 0 },
  );
  const text = await res.text();
  if (!res.ok) {
    let message = text;
    try {
      const parsed = JSON.parse(text) as { error?: { message?: string } | string };
      if (typeof parsed.error === "string") message = parsed.error;
      else if (parsed.error?.message) message = parsed.error.message;
    } catch {
      /* the status alone will have to do */
    }
    throw new ApiError(res.status, message);
  }
  return JSON.parse(text) as LoginResult;
}
