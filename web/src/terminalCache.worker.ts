import { handleTerminalCacheWrite } from "./terminalCacheWorker";
import { isOwnMessage, isTerminalCacheWrite } from "./terminalCacheWorkerGuard";

self.onmessage = (ev: MessageEvent<unknown>) => {
  if (!isOwnMessage(ev.origin, self.location.origin) || !isTerminalCacheWrite(ev.data)) return;
  const message = ev.data;
  void handleTerminalCacheWrite(message).then((ack) => self.postMessage(ack));
};
