import type { RuntimeSocket } from "../runtimeTransport";
import type { DemoStore } from "./store";

const encoder = new TextEncoder();

function asText(data: string | ArrayBufferLike | Blob | ArrayBufferView): string | null {
  if (typeof data === "string") return data;
  if (data instanceof Blob) return null;
  if (ArrayBuffer.isView(data)) {
    return new TextDecoder().decode(new Uint8Array(data.buffer, data.byteOffset, data.byteLength));
  }
  return new TextDecoder().decode(new Uint8Array(data));
}

/** Browser-only PTY transcript. It implements only the socket surface xterm uses. */
export class DemoSocket extends EventTarget implements RuntimeSocket {
  readyState: number = WebSocket.CONNECTING;
  binaryType: BinaryType = "arraybuffer";
  private input = "";
  private timers: number[] = [];

  constructor(private readonly store: DemoStore, private readonly sessionId: string) {
    super();
    queueMicrotask(() => this.open());
  }

  private emitMessage(data: string | ArrayBuffer): void {
    if (this.readyState !== WebSocket.OPEN) return;
    this.dispatchEvent(new MessageEvent("message", { data }));
  }

  private output(text: string): void {
    this.emitMessage(encoder.encode(text).buffer);
  }

  private open(): void {
    if (this.readyState !== WebSocket.CONNECTING) return;
    this.readyState = WebSocket.OPEN;
    this.dispatchEvent(new Event("open"));
    const transcript = this.store.terminalTranscript(this.sessionId);
    const bytes = encoder.encode(transcript);
    this.emitMessage(JSON.stringify({
      type: "snapshot-start",
      scrollback_bytes: bytes.byteLength,
      scrollback_pos: bytes.byteLength,
      reset: true,
    }));
    if (bytes.byteLength) this.emitMessage(bytes.buffer);
    this.emitMessage(JSON.stringify({ type: "snapshot-done" }));
    for (const [delay, line] of this.store.liveTerminalFrames(this.sessionId)) {
      this.timers.push(window.setTimeout(() => this.output(line), delay));
    }
  }

  send(data: string | ArrayBufferLike | Blob | ArrayBufferView): void {
    if (this.readyState !== WebSocket.OPEN) return;
    const text = asText(data);
    if (text === null) return;
    if (text.startsWith("{") && text.endsWith("}")) {
      try {
        const control = JSON.parse(text) as { type?: string };
        if (["auth", "resize", "ping"].includes(control.type ?? "")) return;
      } catch {
        // It was terminal input that happened to look a little like JSON.
      }
    }
    this.input += text.replace(/\n/g, "\r");
    const commands = this.input.split("\r");
    this.input = commands.pop() ?? "";
    for (const command of commands) {
      this.output(`\r\n\x1b[38;5;111mvisitor@vogt-demo\x1b[0m:\x1b[38;5;110m~/Working/orbit\x1b[0m$ ${command}\r\n`);
      this.output(this.store.cannedTerminalResponse(command));
    }
  }

  close(_code?: number, _reason?: string): void {
    if (this.readyState === WebSocket.CLOSED) return;
    this.readyState = WebSocket.CLOSING;
    for (const timer of this.timers) window.clearTimeout(timer);
    this.timers = [];
    this.readyState = WebSocket.CLOSED;
    this.dispatchEvent(new CloseEvent("close", { code: 1000, reason: "demo socket closed" }));
  }

  override addEventListener(type: "open" | "close" | "error", listener: (event: Event) => void, options?: AddEventListenerOptions | boolean): void;
  override addEventListener(type: "message", listener: (event: MessageEvent) => void, options?: AddEventListenerOptions | boolean): void;
  override addEventListener(
    type: "open" | "close" | "error" | "message",
    listener: ((event: Event) => void) | ((event: MessageEvent) => void),
    options?: AddEventListenerOptions | boolean,
  ): void {
    super.addEventListener(type, listener as EventListener, options);
  }
}
