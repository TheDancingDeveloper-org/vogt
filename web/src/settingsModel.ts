// Pure Settings logic, kept out of the component so it can be unit-tested
// without mounting the whole 700-line dialog.
//
// Three things live here because all three used to be tangled into JSX and
// were impossible to assert on directly:
//   1. push subscription reconciliation against the server's list;
//   2. the Save button's label, derived from what is actually dirty;
//   3. whether Save may run at all.

import type { PushSubscriptionEntry } from "./api";

/** Which Settings fields differ from what is currently persisted. */
export interface SettingsDirty {
  token: boolean;
  base: boolean;
  layout: boolean;
  storage: boolean;
}

/**
 * The Save button used to hard-code "Validate, save & reload" even when
 * nothing about the connection had changed — so a reader could not tell
 * whether pressing it would reload the app or quietly save a retention limit.
 * Derive the promise from the dirty flags instead.
 *
 * Changing the token or base always revalidates and always reloads (the app's
 * connection identity moved). A layout change reloads without revalidating.
 * Everything else saves in place.
 */
export function saveButtonLabel(dirty: SettingsDirty): string {
  const validates = dirty.token || dirty.base;
  if (validates) return "Validate, save & reload";
  if (dirty.layout) return "Save & reload";
  if (dirty.storage) return "Save preferences";
  return "Save settings";
}

/** Inputs to the Save-disabled decision, named so the rule reads in one line. */
export interface SaveGate {
  checking: boolean;
  tokenBlank: boolean;
  tokenChanged: boolean;
}

/**
 * Save was disabled whenever the token field was blank — which meant a
 * same-origin deployment that needs no bearer token could never save a layout
 * or retention change, because there was no token to un-blank. A blank token
 * only blocks saving when the user is actively trying to set or change it; an
 * untouched blank field is a valid "keep using the current credential".
 */
export function saveDisabled(gate: SaveGate): boolean {
  if (gate.checking) return true;
  if (gate.tokenBlank && gate.tokenChanged) return true;
  return false;
}

/** What reconciliation concluded about this device against the server list. */
export interface PushReconciliation {
  /** The browser holds a subscription the server no longer lists. */
  serverDropped: boolean;
  /** Whether to surface a Re-enable affordance to re-register it. */
  offerReEnable: boolean;
}

/**
 * A push subscription can vanish server-side — a state-dir reset, a pruned
 * row, a re-keyed VAPID pair — while the browser's `PushManager` still holds a
 * live subscription and reports itself enabled. That device then silently
 * receives nothing. Compare the current subscription id against the server's
 * list and offer to re-register when the server has dropped it.
 */
export function reconcilePush(
  currentSubscriptionId: string | null,
  serverSubscriptions: PushSubscriptionEntry[],
  browserThinksEnabled: boolean,
): PushReconciliation {
  const dropped =
    browserThinksEnabled &&
    currentSubscriptionId != null &&
    !serverSubscriptions.some((sub) => sub.id === currentSubscriptionId);
  return { serverDropped: dropped, offerReEnable: dropped };
}
