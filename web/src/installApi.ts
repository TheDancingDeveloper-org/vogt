// The first-run install surface.
//
// Not part of `vogtApi.ts` deliberately: that module's route table is checked
// against Vogt's operation registry, and these two paths are not operations —
// they are the unauthenticated bootstrap the core mounts beside its health
// probes, passed through the engine's front door untouched
// (`engine/server/src/vogt_core.rs`). The paths here must match the routes
// the engine serves; the Python suite's source scan holds that line.

import { getBase } from "./api";
import { fetchWithRetry } from "./transport";
import { DEADLINE_MS } from "./deadlines";
import { isDemoMode } from "./runtimeTransport";

/** Set by the identity wizard once the bootstrap has minted the first token,
 *  cleared when the setup steps (`#/setup`) finish — the shell reads it to
 *  bring a fresh operator straight to the remaining steps after their first
 *  sign-in. Lives here rather than in `SetupSteps.tsx` so `App.tsx` can read
 *  it without eagerly loading the lazily-split setup surface. */
export const SETUP_PENDING_KEY = "vogt.setup.pending";

export interface InstallStatus {
  install_mode: boolean;
}

export interface InstallBootstrapResult {
  actor: { id: string; identity_ref: string; display_name: string; kind: string };
  token: { id: string; name: string; scopes: string[]; kind?: string };
  /** The session bearer when a password was set; otherwise an admin API
   *  token shown once and not recoverable. */
  secret: string;
  warning: string;
  /** The login name created, when the bootstrap set a password. */
  username?: string | null;
}

export interface BootstrapLogin {
  username: string;
  password: string;
}

/**
 * Whether this instance is still in first-run install mode.
 *
 * `null` means "could not tell": the deployment is unreachable, predates the
 * install surface (404), or answered with something other than the status
 * shape. The caller treats that exactly like a closed install mode — the
 * login gate — because a wizard that appears on a guess would appear wrongly.
 */
export async function fetchInstallStatus(): Promise<InstallStatus | null> {
  if (isDemoMode()) return { install_mode: false };
  try {
    const res = await fetchWithRetry(`${getBase()}/api/install/status`, {}, {
      deadlineMs: DEADLINE_MS.metadata,
    });
    if (!res.ok) return null;
    const body = (await res.json()) as Partial<InstallStatus>;
    if (typeof body.install_mode !== "boolean") return null;
    return { install_mode: body.install_mode };
  } catch {
    return null;
  }
}

/** Name the first operator and mint the first credential — the core allows
 * this exactly once, and refuses with `install_closed` ever after. With a
 * `login`, the operator gets a password and the returned secret is a
 * browser session; without one, an admin API token shown once. */
export async function bootstrapInstall(
  displayName: string,
  login?: BootstrapLogin,
): Promise<InstallBootstrapResult> {
  const body: Record<string, string> = { display_name: displayName };
  if (login) {
    body.username = login.username;
    body.password = login.password;
  }
  const res = await fetchWithRetry(`${getBase()}/api/install/bootstrap`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
  if (!res.ok) {
    let message = `the server answered ${res.status}`;
    try {
      const body = (await res.json()) as { error?: { message?: string } };
      if (body.error?.message) message = body.error.message;
    } catch {
      /* the status alone will have to do */
    }
    throw new Error(message);
  }
  return (await res.json()) as InstallBootstrapResult;
}
