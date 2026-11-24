import { handleTerminalCacheWrite, type TerminalCacheWrite } from "./terminalCacheWorker";

self.onmessage = (ev: MessageEvent<TerminalCacheWrite>) => {
  void handleTerminalCacheWrite(ev.data).then((ack) => self.postMessage(ack));
};
