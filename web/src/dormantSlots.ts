import { MAX_DORMANT_SOCKETS } from "./terminalDormancy";

/**
 * The per-document cap on dormant sockets (WI-128). A hidden pane asks for a
 * slot; granted, it keeps its socket and buffers. Refused, it falls back to
 * today's park (socket closed). Slots go to the most recently active panes:
 * a pane that cannot get one evicts nothing itself — the pane that lost the
 * race simply parks, and the newest hidden panes hold the slots.
 */
const held = new Set<string>();

export function acquireDormantSlot(id: string): boolean {
  if (held.has(id)) return true;
  if (held.size >= MAX_DORMANT_SOCKETS) return false;
  held.add(id);
  return true;
}

export function releaseDormantSlot(id: string): void {
  held.delete(id);
}

export function dormantSlotsHeld(): number {
  return held.size;
}

/** Test-only: a fresh document between cases. */
export function resetDormantSlots(): void {
  held.clear();
}
