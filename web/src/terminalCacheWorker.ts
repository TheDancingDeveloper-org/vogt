/**
 * IndexedDB writes for the terminal cache, off the pane's call stack (WI-2695).
 *
 * The main thread decides what to store (serialized screen or raw ring). The
 * put and the eight-session trim run in a worker when one is available.
 * Restore stays on the main thread via `loadTerminalCache`, reading the same
 * records.
 */

import { saveTerminalCache, type TerminalCachePayload } from "./terminalCache";

export interface TerminalCacheWrite {
  sessionId: string;
  outputPosition: number;
  payload: TerminalCachePayload;
}

export async function handleTerminalCacheWrite(message: TerminalCacheWrite): Promise<void> {
  await saveTerminalCache(message.sessionId, message.outputPosition, message.payload);
}
