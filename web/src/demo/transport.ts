import type { DemoManifest, RuntimeSocket, RuntimeTransport } from "../runtimeTransport";
import { DemoSocket } from "./socket";
import { DemoStore } from "./store";

export class DemoTransport implements RuntimeTransport {
  readonly store: DemoStore;

  constructor(readonly manifest: DemoManifest) {
    this.store = new DemoStore(manifest);
  }

  bootstrapPresentation(): void {
    localStorage.setItem("vogt.token", "public-demo-sentinel");
    localStorage.removeItem("vogt.base");
    this.store.seedPresentation();
  }

  reset(): void {
    this.store.reset();
  }

  request(input: RequestInfo | URL, init: RequestInit = {}): Promise<Response> {
    const raw = input instanceof Request ? input.url : String(input);
    const url = new URL(raw, window.location.origin);
    return this.store.request(url.pathname, (init.method ?? (input instanceof Request ? input.method : "GET")).toUpperCase(), url.searchParams, init);
  }

  openSocket(url: string): RuntimeSocket {
    const id = decodeURIComponent(new URL(url, window.location.origin).pathname.split("/").at(-2) ?? "");
    return new DemoSocket(this.store, id);
  }

  subscribe(listener: Parameters<DemoStore["subscribe"]>[0]): () => void {
    return this.store.subscribe(listener);
  }
}
