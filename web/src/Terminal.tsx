import { Component, Show, createSignal, onCleanup, onMount } from "solid-js";
import { Terminal as XTerm } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebLinksAddon } from "@xterm/addon-web-links";
import "@xterm/xterm/css/xterm.css";
import { openAttach } from "./api";
import { readClipboardText, writeClipboardText } from "./clipboard";
import { sessionsStore } from "./store";
import { getTheme, TERMINAL_THEME_EVENT } from "./terminalThemes";

const DEFAULT_FONT_SIZE = 13;
const MIN_FONT_SIZE = 9;
const MAX_FONT_SIZE = 24;
const FONT_SIZE_STORAGE_KEY = "mydevenv2.terminalFontSize.v1";
const FONT_SIZE_EVENT = "mydevenv2:terminal-font-size";

export interface TerminalActions {
  /** Copy the current xterm selection to the system clipboard. Returns true on success. */
  copy: () => Promise<boolean>;
  /** Read clipboard and inject as PTY stdin. */
  paste: () => Promise<void>;
  /** Tell xterm to select everything in the visible buffer. */
  selectAll: () => void;
  /** Optional higher-level action implemented by TerminalWorkspace. */
  focusComposer?: () => void;
}

interface Props {
  sessionId: string;
  /** Exposed so the parent can inject mobile-modkey input straight into the PTY. */
  registerSend?: (fn: ((data: string | ArrayBuffer) => void) | null) => void;
  /** Exposed so the parent (modkey row, etc.) can drive copy/paste. */
  registerActions?: (actions: TerminalActions | null) => void;
  /** Optional parent hook to reroute local keyboard/paste input before it hits this PTY. */
  interceptInput?: (data: string | ArrayBuffer) => boolean;
  /** Optional callback for user-facing notifications (copy success/failure). */
  onNotify?: (message: string, kind?: "info" | "error") => void;
}

function clampFontSize(value: number): number {
  return Math.min(MAX_FONT_SIZE, Math.max(MIN_FONT_SIZE, value));
}

function readTerminalFontSize(): number {
  try {
    const raw = localStorage.getItem(FONT_SIZE_STORAGE_KEY);
    if (!raw) return DEFAULT_FONT_SIZE;
    const parsed = Number(raw);
    return Number.isFinite(parsed)
      ? clampFontSize(parsed)
      : DEFAULT_FONT_SIZE;
  } catch {
    return DEFAULT_FONT_SIZE;
  }
}

function writeTerminalFontSize(fontSize: number) {
  const next = clampFontSize(Math.round(fontSize * 2) / 2);
  try {
    localStorage.setItem(FONT_SIZE_STORAGE_KEY, String(next));
  } catch {
    /* storage can be unavailable in private / locked-down contexts */
  }
  window.dispatchEvent(
    new CustomEvent(FONT_SIZE_EVENT, { detail: { fontSize: next } }),
  );
}

function configureTerminalTextarea(textarea: HTMLTextAreaElement | undefined) {
  if (!textarea) return;
  textarea.setAttribute("autocorrect", "on");
  textarea.setAttribute("autocapitalize", "none");
  textarea.setAttribute("autocomplete", "on");
  textarea.setAttribute("inputmode", "text");
  textarea.setAttribute("enterkeyhint", "enter");
  textarea.spellcheck = true;
  textarea.setAttribute("spellcheck", "true");
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
  let fitFrame: number | null = null;
  let reconnectDelay = 500;
  let destroyed = false;
  let sessionGone = false;
  let visibilityHandler: (() => void) | null = null;
  let viewportHandler: (() => void) | null = null;
  let fontSizeHandler: ((event: Event) => void) | null = null;
  let themeHandler: (() => void) | null = null;
  let terminalDomCleanup: (() => void) | null = null;
  let pasteTextareaRef: HTMLTextAreaElement | undefined;
  const [showPasteModal, setShowPasteModal] = createSignal(false);
  const [statusText, setStatusText] = createSignal<string | null>("Connecting...");
  let pasteResolve: ((v: string | null) => void) | null = null;

  const promptPaste = (): Promise<string | null> =>
    new Promise((resolve) => {
      pasteResolve = resolve;
      setShowPasteModal(true);
    });

  const confirmPasteModal = () => {
    const text = pasteTextareaRef?.value ?? "";
    const resolve = pasteResolve;
    pasteResolve = null;
    setShowPasteModal(false);
    resolve?.(text || null);
  };

  const cancelPasteModal = () => {
    const resolve = pasteResolve;
    pasteResolve = null;
    setShowPasteModal(false);
    resolve?.(null);
  };

  const sendToPty = (data: string | ArrayBuffer) => {
    if (ws && ws.readyState === WebSocket.OPEN) {
      if (typeof data === "string") {
        ws.send(new TextEncoder().encode(data));
      } else {
        ws.send(data);
      }
    }
  };

  const dispatchInput = (data: string | ArrayBuffer) => {
    if (props.interceptInput?.(data)) return;
    sendToPty(data);
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

  const fitAndResize = () => {
    if (
      !term ||
      !fit ||
      !hostRef ||
      hostRef.clientWidth <= 0 ||
      hostRef.clientHeight <= 0
    ) {
      return;
    }
    try {
      fit.fit();
      sendResize();
    } catch {
      /* xterm can throw while its DOM is detaching or hidden */
    }
  };

  const scheduleFit = () => {
    if (fitFrame !== null) return;
    fitFrame = requestAnimationFrame(() => {
      fitFrame = null;
      fitAndResize();
    });
  };

  const applyFontSize = (fontSize: number) => {
    if (!term) return;
    term.options.fontSize = clampFontSize(fontSize);
    scheduleFit();
  };

  const publishFontSize = (fontSize: number) => {
    const next = clampFontSize(fontSize);
    applyFontSize(next);
    writeTerminalFontSize(next);
  };

  const estimateCellHeight = () => {
    if (!term) return DEFAULT_FONT_SIZE * 1.2;
    const lineHeight =
      typeof term.options.lineHeight === "number" ? term.options.lineHeight : 1;
    return Math.max(8, (term.options.fontSize ?? DEFAULT_FONT_SIZE) * lineHeight);
  };

  const installTouchGestures = () => {
    if (!hostRef) return () => {};

    let mode: "idle" | "scroll" | "pinch" = "idle";
    let startX = 0;
    let startY = 0;
    let lastY = 0;
    let lineRemainder = 0;
    let pinchStartDistance = 0;
    let pinchStartFontSize = readTerminalFontSize();

    const touchDistance = (touches: TouchList): number => {
      const a = touches[0];
      const b = touches[1];
      if (!a || !b) return 0;
      return Math.hypot(a.clientX - b.clientX, a.clientY - b.clientY);
    };

    const resetScroll = (touch: Touch) => {
      mode = "idle";
      startX = touch.clientX;
      startY = touch.clientY;
      lastY = touch.clientY;
      lineRemainder = 0;
    };

    const onTouchStart = (event: TouchEvent) => {
      if (event.touches.length === 1) {
        const touch = event.touches[0];
        if (touch) resetScroll(touch);
        return;
      }
      if (event.touches.length === 2) {
        mode = "pinch";
        pinchStartDistance = touchDistance(event.touches);
        pinchStartFontSize = term?.options.fontSize ?? readTerminalFontSize();
        event.preventDefault();
      }
    };

    const onTouchMove = (event: TouchEvent) => {
      if (!term) return;

      if (event.touches.length === 2) {
        const distance = touchDistance(event.touches);
        if (pinchStartDistance > 0 && distance > 0) {
          mode = "pinch";
          const scale = distance / pinchStartDistance;
          publishFontSize(pinchStartFontSize * scale);
        }
        event.preventDefault();
        return;
      }

      if (event.touches.length !== 1) return;
      const touch = event.touches[0];
      if (!touch) return;

      const totalX = touch.clientX - startX;
      const totalY = touch.clientY - startY;
      if (mode === "idle") {
        const absX = Math.abs(totalX);
        const absY = Math.abs(totalY);
        if (absY < 8 && absX < 8) return;
        if (absY < absX * 1.2) return;
        mode = "scroll";
      }

      if (mode !== "scroll") return;
      const dy = touch.clientY - lastY;
      lastY = touch.clientY;
      lineRemainder += -dy / estimateCellHeight();
      const wholeLines =
        lineRemainder > 0 ? Math.floor(lineRemainder) : Math.ceil(lineRemainder);
      if (wholeLines !== 0) {
        term.scrollLines(wholeLines);
        lineRemainder -= wholeLines;
      }
      event.preventDefault();
    };

    const onTouchEnd = (event: TouchEvent) => {
      if (event.touches.length === 1) {
        const touch = event.touches[0];
        if (touch) resetScroll(touch);
        return;
      }
      mode = "idle";
      lineRemainder = 0;
    };

    const onWheel = (event: WheelEvent) => {
      if (!term || !event.ctrlKey) return;
      event.preventDefault();
      const delta = event.deltaY > 0 ? -0.5 : 0.5;
      publishFontSize((term.options.fontSize ?? readTerminalFontSize()) + delta);
    };

    const listenerOptions: AddEventListenerOptions = { passive: false };
    hostRef.addEventListener("touchstart", onTouchStart, listenerOptions);
    hostRef.addEventListener("touchmove", onTouchMove, listenerOptions);
    hostRef.addEventListener("touchend", onTouchEnd, listenerOptions);
    hostRef.addEventListener("touchcancel", onTouchEnd, listenerOptions);
    hostRef.addEventListener("wheel", onWheel, listenerOptions);

    return () => {
      hostRef?.removeEventListener("touchstart", onTouchStart);
      hostRef?.removeEventListener("touchmove", onTouchMove);
      hostRef?.removeEventListener("touchend", onTouchEnd);
      hostRef?.removeEventListener("touchcancel", onTouchEnd);
      hostRef?.removeEventListener("wheel", onWheel);
    };
  };

  onMount(() => {
    if (!hostRef) return;
    const cleanupTouchGestures = installTouchGestures();

    term = new XTerm({
      fontFamily:
        '"JetBrainsMono Nerd Font", "JetBrains Mono", "Fira Code", ui-monospace, SFMono-Regular, Menlo, monospace',
      fontSize: readTerminalFontSize(),
      lineHeight: 1.2,
      cursorBlink: true,
      scrollback: 5000,
      allowProposedApi: true,
      scrollSensitivity: 2,
      smoothScrollDuration: 0,
      theme: getTheme(),
    });
    fit = new FitAddon();
    term.loadAddon(fit);
    term.loadAddon(new WebLinksAddon());
    term.open(hostRef);
    configureTerminalTextarea(term.textarea);
    fitAndResize();

    // Wire input: user keystrokes → PTY stdin.
    term.onData((data) => dispatchInput(data));

    // Intercept browser paste events (Ctrl+V / Edit→Paste) in the capture phase,
    // before xterm.js wraps them in bracketed-paste sequences (\x1b[200~...\x1b[201~).
    // Programs like infisical that don't implement bracketed-paste mode receive the
    // escape sequences as literal input, corrupting base64 tokens.
    const onPasteCapture = (e: ClipboardEvent) => {
      e.preventDefault();
      e.stopPropagation();
      const text = e.clipboardData?.getData("text/plain") ?? "";
      if (text) dispatchInput(text);
    };
    term.textarea?.addEventListener(
      "paste",
      onPasteCapture,
      true, // capture phase — runs before xterm's bubble-phase listener
    );

    // Clipboard plumbing.
    const copySelection = async (): Promise<boolean> => {
      const sel = term?.getSelection() ?? "";
      if (!sel) return false;

      try {
        await writeClipboardText(sel);
        return true;
      } catch (err) {
        console.warn("Clipboard write failed, trying fallback:", err);
      }

      // Fallback: use a hidden textarea + execCommand for older mobile WebViews
      // or when clipboard permissions are denied.
      try {
        const ta = document.createElement("textarea");
        ta.value = sel;
        ta.style.position = "fixed";
        ta.style.opacity = "0";
        ta.style.left = "-9999px";
        ta.setAttribute("readonly", "");
        document.body.appendChild(ta);

        // For iOS Safari
        ta.contentEditable = "true";
        ta.readOnly = false;

        const range = document.createRange();
        range.selectNodeContents(ta);
        const selection = window.getSelection();
        selection?.removeAllRanges();
        selection?.addRange(range);
        ta.setSelectionRange(0, ta.value.length);

        const success = document.execCommand("copy");
        document.body.removeChild(ta);
        return success;
      } catch (err) {
        console.error("All copy methods failed:", err);
        return false;
      }
    };
    const pasteFromClipboard = async () => {
      let text = "";
      try {
        text = await readClipboardText();
      } catch {
        // Fall back to a textarea modal if the browser/native bridge refused
        // the clipboard read.
        const v = await promptPaste();
        if (v == null) return;
        text = v;
      }
      if (text) dispatchInput(text);
    };

    // Custom key handler for Ctrl+Shift+C/V and Cmd+C/V semantics.
    // Returning false stops xterm from forwarding the keystroke to the PTY.
    term.attachCustomKeyEventHandler((e) => {
      if (e.type !== "keydown") return true;
      const meta = e.ctrlKey && e.shiftKey;
      const mac = navigator.platform.toLowerCase().includes("mac") && e.metaKey;
      if ((meta || mac) && (e.key === "c" || e.key === "C")) {
        if (term?.hasSelection()) {
          copySelection().then((success) => {
            if (success) {
              props.onNotify?.("Copied to clipboard", "info");
            } else {
              props.onNotify?.("Copy failed - check clipboard permissions", "error");
            }
          });
          return false;
        }
        // No selection: fall through so Ctrl+C still sends SIGINT.
        return true;
      }
      // Plain Ctrl+C: copy when there's a selection (Windows Terminal / VS Code
      // convention), otherwise let it through as SIGINT. Skip on Mac, where
      // Ctrl+C is always SIGINT and Cmd+C (handled above) is copy.
      if (
        e.ctrlKey &&
        !e.shiftKey &&
        !e.altKey &&
        !e.metaKey &&
        (e.key === "c" || e.key === "C") &&
        term?.hasSelection()
      ) {
        copySelection().then((success) => {
          if (success) {
            props.onNotify?.("Copied to clipboard", "info");
          }
        });
        term.clearSelection();
        return false;
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
    const onAuxClick = (e: MouseEvent) => {
      if (e.button === 1) {
        e.preventDefault();
        void pasteFromClipboard();
      }
    };
    hostRef.addEventListener("auxclick", onAuxClick);

    // Right-click: if there's a selection, copy it; otherwise paste. Bypasses
    // the browser context menu — most users on mobile/desktop just want one
    // of those two actions on a terminal.
    const onContextMenu = (e: MouseEvent) => {
      e.preventDefault();
      if (term?.hasSelection()) {
        copySelection().then((success) => {
          if (success) {
            props.onNotify?.("Copied to clipboard", "info");
          } else {
            props.onNotify?.("Copy failed - check clipboard permissions", "error");
          }
        });
      } else {
        void pasteFromClipboard();
      }
    };
    hostRef.addEventListener("contextmenu", onContextMenu);
    terminalDomCleanup = () => {
      term?.textarea?.removeEventListener("paste", onPasteCapture, true);
      hostRef?.removeEventListener("auxclick", onAuxClick);
      hostRef?.removeEventListener("contextmenu", onContextMenu);
      terminalDomCleanup = null;
    };

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
      scheduleFit();
    });
    resizeObserver.observe(hostRef);

    viewportHandler = () => scheduleFit();
    window.addEventListener("resize", viewportHandler);
    window.addEventListener("orientationchange", viewportHandler);
    window.addEventListener("mydevenv2:viewport-resize", viewportHandler);

    fontSizeHandler = (event: Event) => {
      const fontSize = (event as CustomEvent<{ fontSize?: number }>).detail
        ?.fontSize;
      if (typeof fontSize === "number") applyFontSize(fontSize);
    };
    window.addEventListener(FONT_SIZE_EVENT, fontSizeHandler);

    themeHandler = () => {
      if (term) term.options.theme = getTheme();
    };
    window.addEventListener(TERMINAL_THEME_EVENT, themeHandler);

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

    onCleanup(() => {
      cleanupTouchGestures();
    });
  });

  function markSessionGone() {
    if (!sessionGone) {
      sessionGone = true;
      setStatusText("Session unavailable");
      term?.write("\r\n\x1b[31m[session not found — server may have restarted]\x1b[0m\r\n");
    }
  }

  function isSessionGone(): boolean {
    return sessionsStore.ready && !sessionsStore.sessions[props.sessionId];
  }

  function scheduleReconnect(delayMs = reconnectDelay) {
    if (destroyed || reconnectTimer !== null) return;
    if (isSessionGone()) { markSessionGone(); return; }
    setStatusText(inSnapshot ? "Reconnecting terminal..." : "Reconnecting...");
    reconnectTimer = setTimeout(() => {
      reconnectTimer = null;
      if (!destroyed) connect();
    }, delayMs);
    reconnectDelay = Math.min(reconnectDelay * 2, 8_000);
  }

  function connect() {
    if (isSessionGone()) { markSessionGone(); return; }
    inSnapshot = true;
    setStatusText("Loading terminal...");
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
            setStatusText("Loading terminal...");
          } else if (ctrl.type === "snapshot-done") {
            inSnapshot = false;
            setStatusText(null);
            sendResize();
          } else if (ctrl.type === "lag") {
            term?.write("\r\n\x1b[31m[lag — reattaching]\x1b[0m\r\n");
            setStatusText("Reattaching terminal...");
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
      setStatusText(inSnapshot ? "Reconnecting terminal..." : "Reconnecting...");
      scheduleReconnect();
    });
    ws.addEventListener("error", () => {
      // Browser fires both error + close; close handler is enough.
    });
  }

  onCleanup(() => {
    destroyed = true;
    if (reconnectTimer !== null) { clearTimeout(reconnectTimer); reconnectTimer = null; }
    if (fitFrame !== null) {
      cancelAnimationFrame(fitFrame);
      fitFrame = null;
    }
    if (visibilityHandler) {
      document.removeEventListener("visibilitychange", visibilityHandler);
      visibilityHandler = null;
    }
    if (viewportHandler) {
      window.removeEventListener("resize", viewportHandler);
      window.removeEventListener("orientationchange", viewportHandler);
      window.removeEventListener("mydevenv2:viewport-resize", viewportHandler);
      viewportHandler = null;
    }
    if (fontSizeHandler) {
      window.removeEventListener(FONT_SIZE_EVENT, fontSizeHandler);
      fontSizeHandler = null;
    }
    if (themeHandler) {
      window.removeEventListener(TERMINAL_THEME_EVENT, themeHandler);
      themeHandler = null;
    }
    terminalDomCleanup?.();
    if (pasteResolve) {
      pasteResolve(null);
      pasteResolve = null;
    }
    props.registerSend?.(null);
    props.registerActions?.(null);
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

  return (
    <>
      <div class="terminal-shell">
        <div class="terminal-host" ref={hostRef} />
        <Show when={statusText()}>
          {(text) => <div class="terminal-status-overlay">{text()}</div>}
        </Show>
      </div>
      <Show when={showPasteModal()}>
        <div
          class="paste-modal-backdrop"
          onPointerDown={cancelPasteModal}
        >
          <div
            class="paste-modal"
            onPointerDown={(e) => e.stopPropagation()}
          >
            <div class="paste-modal-title">Paste text</div>
            <textarea
              ref={(el) => { pasteTextareaRef = el; el.focus(); }}
              class="paste-modal-textarea"
              rows={5}
              placeholder="Long-press here to paste from clipboard…"
              onKeyDown={(e) => {
                if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) confirmPasteModal();
                if (e.key === "Escape") cancelPasteModal();
              }}
            />
            <div class="paste-modal-actions">
              <button class="paste-modal-btn" onClick={cancelPasteModal}>Cancel</button>
              <button class="paste-modal-btn primary" onClick={confirmPasteModal}>Paste</button>
            </div>
          </div>
        </div>
      </Show>
    </>
  );
};

export default TerminalView;
