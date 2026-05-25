import { Component, createEffect, onCleanup, onMount } from "solid-js";
import { Terminal as XTerm } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebLinksAddon } from "@xterm/addon-web-links";
import "@xterm/xterm/css/xterm.css";
import { openAttach } from "./api";

export interface TerminalActions {
  /** Copy the current xterm selection to the system clipboard. */
  copy: () => Promise<void>;
  /** Read clipboard and inject as PTY stdin. */
  paste: () => Promise<void>;
  /** Tell xterm to select everything in the visible buffer. */
  selectAll: () => void;
}

interface Props {
  sessionId: string;
  /** Exposed so the parent can inject mobile-modkey input straight into the PTY. */
  registerSend?: (fn: (data: string | ArrayBuffer) => void) => void;
  /** Exposed so the parent (modkey row, etc.) can drive copy/paste. */
  registerActions?: (actions: TerminalActions) => void;
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
  let reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  let reconnectDelay = 500;
  let destroyed = false;
  let visibilityHandler: (() => void) | null = null;

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

    // Clipboard plumbing.
    const copySelection = async () => {
      const sel = term?.getSelection() ?? "";
      if (!sel) return;
      try {
        await navigator.clipboard.writeText(sel);
      } catch {
        // Fallback: use a hidden textarea + execCommand for older mobile WebViews.
        const ta = document.createElement("textarea");
        ta.value = sel;
        ta.style.position = "fixed";
        ta.style.opacity = "0";
        document.body.appendChild(ta);
        ta.select();
        try {
          document.execCommand("copy");
        } catch {
          /* nothing else to try */
        }
        document.body.removeChild(ta);
      }
    };
    const pasteFromClipboard = async () => {
      let text = "";
      try {
        text = await navigator.clipboard.readText();
      } catch {
        // No permission / unsupported (iOS Safari without user gesture, etc.).
        // Prompt the user as a last resort — preserves the feature on locked-
        // down WebViews.
        const v = window.prompt("Paste:");
        if (v == null) return;
        text = v;
      }
      if (text) sendToPty(text);
    };

    // Custom key handler for Ctrl+Shift+C/V and Cmd+C/V semantics.
    // Returning false stops xterm from forwarding the keystroke to the PTY.
    term.attachCustomKeyEventHandler((e) => {
      if (e.type !== "keydown") return true;
      const meta = e.ctrlKey && e.shiftKey;
      const mac = navigator.platform.toLowerCase().includes("mac") && e.metaKey;
      if ((meta || mac) && (e.key === "c" || e.key === "C")) {
        if (term?.hasSelection()) {
          void copySelection();
          return false;
        }
        // No selection: fall through so Ctrl+C still sends SIGINT.
        return true;
      }
      if ((meta || mac) && (e.key === "v" || e.key === "V")) {
        void pasteFromClipboard();
        return false;
      }
      if ((meta || mac) && (e.key === "a" || e.key === "A")) {
        term?.selectAll();
        return false;
      }
      return true;
    });

    // Middle-click paste (matches xterm-on-Linux convention).
    hostRef.addEventListener("auxclick", (e) => {
      if ((e as MouseEvent).button === 1) {
        e.preventDefault();
        void pasteFromClipboard();
      }
    });

    // Right-click: if there's a selection, copy it; otherwise paste. Bypasses
    // the browser context menu — most users on mobile/desktop just want one
    // of those two actions on a terminal.
    hostRef.addEventListener("contextmenu", (e) => {
      e.preventDefault();
      if (term?.hasSelection()) {
        void copySelection();
      } else {
        void pasteFromClipboard();
      }
    });

    // Auto-copy on selection-end is nice on desktop but surprising on mobile
    // (long-press to select → release accidentally clobbers the clipboard).
    // We hold this back and rely on the explicit shortcuts / context menu.

    props.registerActions?.({
      copy: copySelection,
      paste: pasteFromClipboard,
      selectAll: () => term?.selectAll(),
    });

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

    // On mobile the OS kills the WebSocket when the app is backgrounded.
    // Reconnect immediately when the page becomes visible again.
    visibilityHandler = () => {
      if (document.visibilityState !== "visible") return;
      if (!ws || ws.readyState === WebSocket.CLOSED || ws.readyState === WebSocket.CLOSING) {
        if (reconnectTimer !== null) { clearTimeout(reconnectTimer); reconnectTimer = null; }
        reconnectDelay = 500;
        connect();
      }
    };
    document.addEventListener("visibilitychange", visibilityHandler);

    connect();
  });

  function scheduleReconnect(delayMs = reconnectDelay) {
    if (destroyed || reconnectTimer !== null) return;
    reconnectTimer = setTimeout(() => {
      reconnectTimer = null;
      if (!destroyed) connect();
    }, delayMs);
    reconnectDelay = Math.min(reconnectDelay * 2, 8_000);
  }

  function connect() {
    inSnapshot = true;
    ws = openAttach(props.sessionId);
    ws.addEventListener("open", () => {
      reconnectDelay = 500;
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
            term?.reset();
            inSnapshot = true;
          } else if (ctrl.type === "snapshot-done") {
            inSnapshot = false;
            sendResize();
          } else if (ctrl.type === "lag") {
            term?.write("\r\n\x1b[31m[lag — reattaching]\x1b[0m\r\n");
            // Cancel any timer so the close event below doesn't double-schedule.
            if (reconnectTimer !== null) { clearTimeout(reconnectTimer); reconnectTimer = null; }
            reconnectDelay = 500;
            ws?.close();
            scheduleReconnect(100);
          }
        } catch {
          /* ignore non-JSON text frames */
        }
        return;
      }
      const buf = new Uint8Array(ev.data as ArrayBuffer);
      term?.write(buf);
    });
    ws.addEventListener("close", () => {
      if (!inSnapshot) {
        term?.write("\r\n\x1b[33m[disconnected]\x1b[0m\r\n");
      }
      scheduleReconnect();
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
    destroyed = true;
    if (reconnectTimer !== null) { clearTimeout(reconnectTimer); reconnectTimer = null; }
    if (visibilityHandler) {
      document.removeEventListener("visibilitychange", visibilityHandler);
      visibilityHandler = null;
    }
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
