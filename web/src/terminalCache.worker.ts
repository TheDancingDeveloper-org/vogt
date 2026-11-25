import { handleTerminalCacheWrite, type TerminalCacheWrite } from "./terminalCacheWorker";

/**
 * A message from the page that owns this dedicated worker. A dedicated worker
 * only receives messages from its owning page, and those carry an empty
 * `origin` (or the page's own origin). Anything else is not ours and is ignored.
 */
function isOwnMessage(ev: MessageEvent): boolean {
  return ev.origin === "" || ev.origin === self.location.origin;
}

/** Accept only the shape `postTerminalCacheWrite` sends; ignore the rest. */
function isTerminalCacheWrite(data: unknown): data is TerminalCacheWrite {
  if (typeof data !== "object" || data === null) return false;
  const m = data as Record<string, unknown>;
  const payload = m.payload;
  return (
    typeof m.id === "number" &&
    typeof m.sessionId === "string" &&
    typeof m.outputPosition === "number" &&
    Number.isSafeInteger(m.outputPosition) &&
    typeof payload === "object" &&
    payload !== null &&
    (m.stream === undefined || typeof m.stream === "string") &&
    (m.tag === undefined || typeof m.tag === "string")
  );
}

self.onmessage = (ev: MessageEvent<unknown>) => {
  if (!isOwnMessage(ev) || !isTerminalCacheWrite(ev.data)) return;
  const message = ev.data;
  void handleTerminalCacheWrite(message).then((ack) => self.postMessage(ack));
};
