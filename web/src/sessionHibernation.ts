// Hibernating and waking sessions from the GUI (WI-912).
//
// The core's operations come first: they are audited, and waking a session
// Vogt started through the core is what gives the woken process a new Vogt
// token (the engine never stores one). The core accepts the engine's UUID for
// every session, linked or not, so the GUI never needs to know which kind it
// holds. Only when there is no core behind the front door (`VogtUnavailable`)
// does the GUI ask the engine directly — then there is no token to mint and
// nothing to audit it in.

import { api, type SessionRole } from "./api";
import {
  VogtUnavailable,
  hibernateSessionInVogt,
  keepSessionAwakeInVogt,
  setSessionRoleInVogt,
  wakeSessionInVogt,
} from "./vogtApi";

async function viaCore<T>(core: () => Promise<T>, engine: () => Promise<unknown>): Promise<void> {
  try {
    await core();
  } catch (error) {
    if (!(error instanceof VogtUnavailable)) throw error;
    await engine();
  }
}

export function wakeSession(id: string, reason = "woken from the GUI"): Promise<void> {
  return viaCore(
    () => wakeSessionInVogt(id, reason),
    () => api.wakeSession(id),
  );
}

export function hibernateSession(
  id: string,
  reason = "hibernated from the GUI to free its memory",
): Promise<void> {
  return viaCore(
    () => hibernateSessionInVogt(id, reason),
    () => api.hibernateSession(id, reason),
  );
}

export function setKeepAwake(
  id: string,
  keepAwake: boolean,
  reason = keepAwake ? "pinned awake from the GUI" : "unpinned from the GUI",
): Promise<void> {
  return viaCore(
    () => keepSessionAwakeInVogt(id, keepAwake, reason),
    () => api.keepSessionAwake(id, keepAwake),
  );
}

/** Nominate a session as oversight, or make it a worker again (WI-957).
 *  Oversight pins it awake too; the rail lists it first. */
export function setSessionRole(
  id: string,
  role: SessionRole,
  reason = role === "oversight" ? "nominated as oversight from the GUI" : "made a worker again from the GUI",
): Promise<void> {
  return viaCore(
    () => setSessionRoleInVogt(id, role, reason),
    () => api.setSessionRole(id, role),
  );
}
