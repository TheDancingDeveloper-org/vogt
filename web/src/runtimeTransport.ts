/** Runtime I/O boundary shared by the live client and the public demo. */

export interface RuntimeSocket {
  readonly readyState: number;
  binaryType: BinaryType;
  send(data: string | ArrayBufferLike | Blob | ArrayBufferView): void;
  close(code?: number, reason?: string): void;
  addEventListener(type: "open" | "close" | "error", listener: (event: Event) => void, options?: AddEventListenerOptions | boolean): void;
  addEventListener(type: "message", listener: (event: MessageEvent) => void, options?: AddEventListenerOptions | boolean): void;
}

export interface RuntimeTransport {
  request(input: RequestInfo | URL, init?: RequestInit): Promise<Response>;
  openSocket(url: string): RuntimeSocket;
}

export interface DemoManifest {
  schema: 1;
  enabled: true;
  source_ref: string;
  source_sha: string;
  scenario: string;
}

interface DemoBuildManifest {
  schema: 1;
  source_ref: string;
  source_sha: string;
  assets: Record<string, string>;
}

const networkTransport: RuntimeTransport = {
  request: (input, init) => fetch(input, init),
  openSocket: (url) => new WebSocket(url),
};

let activeTransport: RuntimeTransport = networkTransport;
let activeDemoManifest: DemoManifest | null = null;

export function runtimeTransport(): RuntimeTransport {
  return activeTransport;
}

export function installRuntimeTransport(transport: RuntimeTransport): void {
  activeTransport = transport;
}

export function isDemoMode(): boolean {
  return activeDemoManifest !== null;
}

export function demoManifest(): DemoManifest | null {
  return activeDemoManifest;
}

function validManifest(value: unknown): value is DemoManifest {
  if (!value || typeof value !== "object") return false;
  const row = value as Record<string, unknown>;
  return row.schema === 1 && row.enabled === true
    && typeof row.source_ref === "string" && row.source_ref.length > 0
    && typeof row.source_sha === "string" && /^[0-9a-f]{40}$/i.test(row.source_sha)
    && row.source_sha !== "0".repeat(40)
    && typeof row.scenario === "string" && row.scenario.length > 0;
}

function validBuildManifest(value: unknown, runtime: DemoManifest): value is DemoBuildManifest {
  if (!value || typeof value !== "object") return false;
  const row = value as Record<string, unknown>;
  const assets = row.assets;
  return row.schema === 1 && row.source_ref === runtime.source_ref
    && row.source_sha === runtime.source_sha && assets !== null
    && typeof assets === "object" && Object.keys(assets as object).length > 0
    && Object.entries(assets as Record<string, unknown>).every(
      ([name, digest]) => name.length > 0 && typeof digest === "string" && /^[0-9a-f]{64}$/i.test(digest),
    );
}

/**
 * Probe the same origin before the application (and its storage-backed module
 * singletons) mounts. A missing or malformed manifest always means the normal
 * network client; demo mode is never inferred from a host name or query flag.
 */
export async function initializeRuntimeTransport(): Promise<DemoManifest | null> {
  try {
    const response = await networkTransport.request("/demo-manifest.json", {
      cache: "no-store",
      headers: { Accept: "application/json" },
    });
    if (!response.ok) return null;
    const candidate: unknown = await response.json();
    if (!validManifest(candidate)) return null;
    const provenanceResponse = await networkTransport.request("/demo-build.json", {
      cache: "no-store",
      headers: { Accept: "application/json" },
    });
    if (!provenanceResponse.ok) return null;
    const provenance: unknown = await provenanceResponse.json();
    if (!validBuildManifest(provenance, candidate)) return null;
    const { DemoTransport } = await import("./demo/transport");
    const demo = new DemoTransport(candidate);
    activeTransport = demo;
    activeDemoManifest = candidate;
    document.documentElement.dataset.demo = "true";
    demo.bootstrapPresentation();
    return candidate;
  } catch {
    return null;
  }
}

export function resetDemoData(): void {
  if (!activeDemoManifest) return;
  const resettable = activeTransport as RuntimeTransport & { reset?: () => void };
  resettable.reset?.();
  window.location.reload();
}
