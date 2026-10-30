/**
 * IndexedDB writes for the terminal cache, off the pane's call stack (WI-2695).
 *
 * The main thread decides what to store (serialized screen or raw ring). The
 * put and the eight-session trim run in a worker when one is available.
 * Restore stays on the main thread via `loadTerminalCache`, reading the same
 * records. Every write is acknowledged so the pane marks an offset persisted
 * only once it is stored.
 */

import {
  saveTerminalCache,
  type TerminalCachePayload,
  type TerminalCacheSaveResult,
} from "./terminalCache";

export interface TerminalCacheWrite {
  id: number;
  sessionId: string;
  outputPosition: number;
  payload: TerminalCachePayload;
}

export interface TerminalCacheAck {
  id: number;
  result: TerminalCacheSaveResult;
}

export async function handleTerminalCacheWrite(
  message: TerminalCacheWrite,
): Promise<TerminalCacheAck> {
  const result = await saveTerminalCache(
    message.sessionId,
    message.outputPosition,
    message.payload,
  );
  return { id: message.id, result };
}
