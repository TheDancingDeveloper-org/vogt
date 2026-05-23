import { Component, createEffect, onCleanup, onMount } from "solid-js";
import { Terminal as XTerm } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebLinksAddon } from "@xterm/addon-web-links";
import "@xterm/xterm/css/xterm.css";
import { openAttach } from "./api";

interface Props {
  sessionId: string;
  /** Exposed so the parent can inject mobile-modkey input straight into the PTY. */
  registerSend?: (fn: (data: string | ArrayBuffer) => void) => void;
}

/**
 * One xterm.js Terminal attached to a single WS session.
 *
 * Reattach is cheap because the server replays the scrollback snapshot, but
 * remounting this component still causes a fresh subscribe so prefer keeping
 * it alive (e.g. by toggling `display: none` on tab switch) rather than
 * tearing down. For Phase 2 we render only the active tab — good enough.
 */
const TerminalView: Component<Props> = (props) => {
  let hostRef: HTMLDivElement | undefined;
  let term: XTerm | null = null;
  let ws: WebSocket | null = null;
  let fit: FitAddon | null = null;
  let resizeObserver: ResizeObserver | null = null;
  let inSnapshot = true;

  const sendToPty = (data: string | ArrayBuffer) => {
    if (ws && ws.readyState === WebSocket.OPEN) {
      if (typeof data === "string") {
        ws.send(new TextEncoder().encode(data));
      } else {
        ws.send(data);
      }
    }
  };

  const sendResize = () => {
    if (!term || !ws || ws.readyState !== WebSocket.OPEN) return;
    ws.send(
      JSON.stringify({
        type: "resize",
        cols: term.cols,
        rows: term.rows,
      }),
    );
  };

  onMount(() => {
    if (!hostRef) return;

    term = new XTerm({
      fontFamily:
        '"JetBrainsMono Nerd Font", "JetBrains Mono", "Fira Code", ui-monospace, SFMono-Regular, Menlo, monospace',
      fontSize: 13,
      lineHeight: 1.2,
      cursorBlink: true,
      scrollback: 5000,
      allowProposedApi: true,
      theme: {
        background: "#000000",
        foreground: "#c9d1d9",
        cursor: "#58a6ff",
        selectionBackground: "#1f6feb55",
        black: "#484f58",
        red: "#ff7b72",
        green: "#7ee787",
        yellow: "#d29922",
        blue: "#58a6ff",
        magenta: "#bc8cff",
        cyan: "#39c5cf",
        white: "#c9d1d9",
        brightBlack: "#6e7681",
        brightRed: "#ffa198",
        brightGreen: "#56d364",
        brightYellow: "#e3b341",
        brightBlue: "#79c0ff",
        brightMagenta: "#d2a8ff",
        brightCyan: "#56d4dd",
        brightWhite: "#f0f6fc",
      },
    });
    fit = new FitAddon();
    term.loadAddon(fit);
    term.loadAddon(new WebLinksAddon());
    term.open(hostRef);
    fit.fit();

    // Wire input: user keystrokes → PTY stdin.
    term.onData((data) => sendToPty(data));

    // Resize plumbing
    resizeObserver = new ResizeObserver(() => {
      try {
        fit?.fit();
        sendResize();
      } catch {
        /* no-op during teardown */
      }
    });
    resizeObserver.observe(hostRef);

    props.registerSend?.(sendToPty);

    connect();
  });

  function connect() {
    inSnapshot = true;
    ws = openAttach(props.sessionId);
    ws.addEventListener("open", () => {
      // Force a fresh resize as soon as we're attached so the server's PTY
      // tracks the visible dimensions, not the 80x24 default.
      sendResize();
    });
    ws.addEventListener("message", (ev) => {
      if (typeof ev.data === "string") {
        try {
          const ctrl = JSON.parse(ev.data) as
            | { type: "snapshot-start" }
            | { type: "snapshot-done" }
            | { type: "lag"; note?: string };
          if (ctrl.type === "snapshot-start") {
            // Clear screen + scrollback before replaying. Otherwise the
            // snapshot prints on top of a stale buffer.
            term?.reset();
            inSnapshot = true;
          } else if (ctrl.type === "snapshot-done") {
            inSnapshot = false;
            sendResize();
          } else if (ctrl.type === "lag") {
            // Server kicked us for being too slow — reattach.
            term?.write(
              "\r\n\x1b[31m[lag — reattaching]\x1b[0m\r\n",
            );
            ws?.close();
            setTimeout(connect, 100);
          }
        } catch {
          /* ignore non-JSON text frames */
        }
        return;
      }
      // Binary frame: PTY output. ev.data is an ArrayBuffer.
      const buf = new Uint8Array(ev.data as ArrayBuffer);
      term?.write(buf);
    });
    ws.addEventListener("close", () => {
      if (!inSnapshot) {
        term?.write("\r\n\x1b[33m[disconnected]\x1b[0m\r\n");
      }
    });
    ws.addEventListener("error", () => {
      // Browser fires both error + close; close handler is enough.
    });
  }

  // If the sessionId prop changes (e.g. parent reuses the component across
  // tabs), reconnect cleanly. In Phase 2 each tab keys its own Terminal so
  // this is defensive.
  createEffect(() => {
    const id = props.sessionId;
    if (ws && id) {
      // Only reconnect when id genuinely changes after first mount.
      // No-op here; tracked for completeness.
    }
  });

  onCleanup(() => {
    resizeObserver?.disconnect();
    try {
      ws?.close();
    } catch {
      /* ignore */
    }
    term?.dispose();
    term = null;
    ws = null;
    fit = null;
  });

  return <div class="terminal-host" ref={hostRef} />;
};

export default TerminalView;
