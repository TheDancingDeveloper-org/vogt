// The first-run install surface (#292).
//
// Not part of `vogtApi.ts` deliberately: that module's route table is checked
// against Vogt's operation registry, and these two paths are not operations —
// they are the unauthenticated bootstrap the core mounts beside its health
// probes, passed through the engine's front door untouched
// (`engine/server/src/vogt_core.rs`). The paths here must match the routes
// the engine serves; the Python suite's source scan holds that line.

import { getBase } from "./api";
import { fetchWithRetry } from "./transport";

export interface InstallStatus {
  install_mode: boolean;
}

export interface InstallBootstrapResult {
  actor: { id: string; identity_ref: string; display_name: string; kind: string };
  token: { id: string; name: string; scopes: string[] };
  /** Shown once. Not stored server-side, not recoverable. */
  secret: string;
  warning: string;
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
  try {
    const res = await fetchWithRetry(`${getBase()}/api/install/status`);
    if (!res.ok) return null;
    const body = (await res.json()) as Partial<InstallStatus>;
    if (typeof body.install_mode !== "boolean") return null;
    return { install_mode: body.install_mode };
  } catch {
    return null;
  }
}

/** Name the first operator and mint the first token — the core allows this
 * exactly once, and refuses with `install_closed` ever after. */
export async function bootstrapInstall(
  displayName: string,
): Promise<InstallBootstrapResult> {
  const res = await fetchWithRetry(`${getBase()}/api/install/bootstrap`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ display_name: displayName }),
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
