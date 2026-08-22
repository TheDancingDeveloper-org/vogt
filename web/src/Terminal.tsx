import { Component, Show, createSignal, onCleanup, onMount } from "solid-js";
import { listSessions } from "./vogtApi";
import { Terminal as XTerm } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebLinksAddon } from "@xterm/addon-web-links";
import { SearchAddon, type ISearchResultChangeEvent } from "@xterm/addon-search";
import "@xterm/xterm/css/xterm.css";
import { openAttach } from "./api";
import { readClipboardText, writeClipboardText } from "./clipboard";
import { sessionsStore } from "./store";
import { getTheme, TERMINAL_THEME_EVENT } from "./terminalThemes";
import {
  formatTerminalInputLimit,
  MAX_TERMINAL_INPUT_BYTES,
  terminalInputTooLarge,
} from "./terminalInput";
import {
  loadTerminalCache,
  MAX_TERMINAL_CACHE_BYTES,
  saveTerminalCache,
} from "./terminalCache";
import {
  clampTerminalFontSize,
  DEFAULT_TERMINAL_FONT_SIZE,
  readTerminalFontSize,
  TERMINAL_FONT_SIZE_EVENT,
} from "./terminalFont";
import Dialog from "./Dialog";
import {
  formatQueuedBytes,
  formatReconnectStatus,
  ReconnectTracker,
} from "./terminalReconnect";
import { applyStickyMods } from "./terminalModifiers";

export interface TerminalActions {
  /** Copy the current xterm selection to the system clipboard. Returns true on success. */
  copy: () => Promise<boolean>;
  /** Read clipboard and inject as PTY stdin. */
  paste: () => Promise<void>;
  /** Tell xterm to select everything in the visible buffer. */
  selectAll: () => void;
  /** Optional higher-level action implemented by TerminalWorkspace. */
  focusComposer?: () => void;
  /** Jump to the next match of `query` in this pane's buffer (find bar). */
  findNext?: (query: string) => void;
  /** Jump to the previous match of `query` in this pane's buffer. */
  findPrevious?: (query: string) => void;
  /** Drop all search highlights in this pane. */
  clearSearch?: () => void;
}

// Search-match highlight colours. matchOverviewRuler and
// activeMatchColorOverviewRuler are required by the addon's typings; the ruler
// is off but the fields must be present for onDidChangeResults to fire.
const SEARCH_DECORATIONS = {
  matchBackground: "#5c4b00",
  matchOverviewRuler: "#d29922",
  activeMatchBackground: "#d29922",
  activeMatchColorOverviewRuler: "#f0f6fc",
} as const;

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
  /** Asked to open the workspace find bar (Ctrl/Cmd+Shift+F inside this pane). */
  onRequestFind?: () => void;
  /** Reports search match position/count so the find bar can show "i of n". */
  onSearchResults?: (info: ISearchResultChangeEvent) => void;
  /** The PTY program set its window title (OSC 0/2) — drives the tab label. */
  onTitle?: (title: string) => void;
  /** The PTY rang the bell (BEL) — drives the tab's activity dot. */
  onBell?: () => void;
}

// Upper bound on input buffered while the WS is reconnecting. Generous enough
// for anything typed or pasted by hand; anything bigger belongs in a file.
const MAX_PENDING_INPUT_BYTES = 4 * MAX_TERMINAL_INPUT_BYTES;

function configureTerminalTextarea(textarea: HTMLTextAreaElement | undefined) {
  if (!textarea) return;
  // Touch devices only: soft keyboards are unusable for prose without
  // autocorrect/suggestions. On desktop, keep xterm's defaults (everything
  // off) — enabling them routes hardware-keyboard input and paste through
  // IME composition, which xterm can garble into duplicated/"corrected"
  // characters.
  if (!(window.matchMedia?.("(pointer: coarse)").matches ?? false)) return;
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
 * remounting still creates a new WebSocket and replay. Sessions deliberately
 * retains terminal panes across route changes; other tool kinds unmount.
 */
const TerminalView: Component<Props> = (props) => {
  const [openedFor, setOpenedFor] = createSignal<string | null>(null);
  let hostRef: HTMLDivElement | undefined;
  let term: XTerm | null = null;
  let ws: WebSocket | null = null;
  let fit: FitAddon | null = null;
  let search: SearchAddon | null = null;
  let resizeObserver: ResizeObserver | null = null;
  let inSnapshot = true;
  let outputPosition: number | undefined;
  let snapshotEndPosition: number | undefined;
  let cacheChunks: Uint8Array[] = [];
  let cacheBytes = 0;
  let cacheTimer: ReturnType<typeof setTimeout> | null = null;
  let readyToConnect = false;
  let reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  let countdownTimer: ReturnType<typeof setInterval> | null = null;
  let reconnectAt = 0;
  let fitFrame: number | null = null;
  const reconnect = new ReconnectTracker();
  let destroyed = false;
  let sessionGone = false;
  let visibilityHandler: (() => void) | null = null;
  let viewportHandler: (() => void) | null = null;
  let fontSizeHandler: ((event: Event) => void) | null = null;
  let themeHandler: (() => void) | null = null;
  let terminalDomCleanup: (() => void) | null = null;
  let pendingInput: Uint8Array<ArrayBuffer>[] = [];
  let pendingInputBytes = 0;
  let pasteTextareaRef: HTMLTextAreaElement | undefined;
  const [showPasteModal, setShowPasteModal] = createSignal(false);
  const [statusText, setStatusText] = createSignal<string | null>("Connecting...");
  // The interactive reconnect overlay. Non-null only while we are between a
  // dropped socket and the next successful attach.
  const [reconnectView, setReconnectView] = createSignal<
    { attempt: number; nextInSec: number } | null
  >(null);
  const [queuedBytes, setQueuedBytes] = createSignal(0);
  // Touch copy chip (#236): on a coarse pointer there is no right-click and iOS
  // Safari fires no contextmenu on a long-press, so a live selection surfaces a
  // floating Copy chip instead. `runCopyChip` is wired to the real clipboard
  // path once xterm is mounted.
  const isCoarsePointer =
    window.matchMedia?.("(pointer: coarse)").matches ?? false;
  const [showCopyChip, setShowCopyChip] = createSignal(false);
  let runCopyChip: () => void = () => {};
  let pasteResolve: ((v: string | null) => void) | null = null;

  const syncQueuedBytes = () => setQueuedBytes(pendingInputBytes);

  const clearCountdown = () => {
    if (countdownTimer !== null) {
      clearInterval(countdownTimer);
      countdownTimer = null;
    }
  };

  const promptPaste = (): Promise<string | null> =>
    new Promise((resolve) => {
      pasteResolve = resolve;
      setShowPasteModal(true);
    });

  // Closing the paste modal leaves focus on the (now-gone) dialog; hand it back
  // to the PTY so the next keystroke lands in the terminal, not nowhere. Queued
  // so it runs after Dialog's own focus-restore teardown.
  const refocusTerminal = () => queueMicrotask(() => term?.focus());

  const confirmPasteModal = () => {
    const text = pasteTextareaRef?.value ?? "";
    const resolve = pasteResolve;
    pasteResolve = null;
    setShowPasteModal(false);
    resolve?.(text || null);
    refocusTerminal();
  };

  const cancelPasteModal = () => {
    const resolve = pasteResolve;
    pasteResolve = null;
    setShowPasteModal(false);
    resolve?.(null);
    refocusTerminal();
  };

  const sendToPty = (data: string | ArrayBuffer) => {
    if (terminalInputTooLarge(data)) {
      props.onNotify?.(
        `Paste not sent: terminal input is limited to ${formatTerminalInputLimit()}. Use a file upload or split it into smaller chunks.`,
        "error",
      );
      return;
    }
    const bytes: Uint8Array<ArrayBuffer> =
      typeof data === "string"
        ? new TextEncoder().encode(data)
        : new Uint8Array(data);
    if (ws && ws.readyState === WebSocket.OPEN) {
      ws.send(bytes);
      return;
    }
    // Reconnects are routine (mobile background/foreground, redeploys), so
    // input typed or pasted in that window must not vanish — queue it and
    // flush when the socket reopens.
    if (destroyed || sessionGone) return;
    if (pendingInputBytes + bytes.byteLength > MAX_PENDING_INPUT_BYTES) {
      props.onNotify?.("Terminal disconnected — input dropped", "error");
      return;
    }
    pendingInput.push(bytes);
    pendingInputBytes += bytes.byteLength;
    syncQueuedBytes();
  };

  const flushPendingInput = () => {
    if (!ws || ws.readyState !== WebSocket.OPEN) return;
    for (const chunk of pendingInput) ws.send(chunk);
    pendingInput = [];
    pendingInputBytes = 0;
    syncQueuedBytes();
  };

  const dropPendingInput = () => {
    if (pendingInputBytes > 0) {
      props.onNotify?.("Terminal reconnect failed — pending input dropped", "error");
    }
    pendingInput = [];
    pendingInputBytes = 0;
    syncQueuedBytes();
  };

  const appendToCache = (data: Uint8Array) => {
    if (data.byteLength === 0) return;
    cacheChunks.push(data.slice());
    cacheBytes += data.byteLength;
    while (cacheBytes > MAX_TERMINAL_CACHE_BYTES && cacheChunks.length > 0) {
      const first = cacheChunks[0];
      if (!first) break;
      const overflow = cacheBytes - MAX_TERMINAL_CACHE_BYTES;
      if (first.byteLength <= overflow) {
        cacheChunks.shift();
        cacheBytes -= first.byteLength;
      } else {
        cacheChunks[0] = first.slice(overflow);
        cacheBytes -= overflow;
      }
    }
  };

  const cachedBytes = (): Uint8Array => {
    const combined = new Uint8Array(cacheBytes);
    let offset = 0;
    for (const chunk of cacheChunks) {
      combined.set(chunk, offset);
      offset += chunk.byteLength;
    }
    cacheChunks = combined.byteLength > 0 ? [combined] : [];
    return combined;
  };

  const persistCache = () => {
    if (outputPosition === undefined) return;
    void saveTerminalCache(props.sessionId, outputPosition, cachedBytes());
  };

  const scheduleCachePersist = () => {
    if (cacheTimer !== null) return;
    cacheTimer = setTimeout(() => {
      cacheTimer = null;
      persistCache();
    }, 1_000);
  };

  const dispatchInput = (data: string | ArrayBuffer) => {
    // Sticky Ctrl/Alt from the phone modkey row lands here for soft-keyboard
    // characters: `Ctrl` armed + `r` typed → `^R` (#236).
    const next = applyStickyMods(data);
    if (props.interceptInput?.(next)) return;
    sendToPty(next);
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
    term.options.fontSize = clampTerminalFontSize(fontSize);
    scheduleFit();
  };

  const estimateCellHeight = () => {
    if (!term) return DEFAULT_TERMINAL_FONT_SIZE * 1.2;
    const lineHeight =
      typeof term.options.lineHeight === "number" ? term.options.lineHeight : 1;
    return Math.max(
      8,
      (term.options.fontSize ?? DEFAULT_TERMINAL_FONT_SIZE) * lineHeight,
    );
  };

  const installTouchGestures = () => {
    if (!hostRef) return () => {};

    let mode: "idle" | "scroll" = "idle";
    let startX = 0;
    let startY = 0;
    let lastY = 0;
    let lineRemainder = 0;

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
    };

    const onTouchMove = (event: TouchEvent) => {
      if (!term) return;

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

    const listenerOptions: AddEventListenerOptions = { passive: false };
    hostRef.addEventListener("touchstart", onTouchStart, listenerOptions);
    hostRef.addEventListener("touchmove", onTouchMove, listenerOptions);
    hostRef.addEventListener("touchend", onTouchEnd, listenerOptions);
    hostRef.addEventListener("touchcancel", onTouchEnd, listenerOptions);

    return () => {
      hostRef?.removeEventListener("touchstart", onTouchStart);
      hostRef?.removeEventListener("touchmove", onTouchMove);
      hostRef?.removeEventListener("touchend", onTouchEnd);
      hostRef?.removeEventListener("touchcancel", onTouchEnd);
    };
  };

  onMount(() => {
    // Ask Vogt whether this PTY belongs to a work item. One read, at mount,
    // and silence on any failure: the badge is worth having and worth
    // nothing at all if it costs the terminal.
    void listSessions({ include_stopped: true })
      .then((answer) => {
        const link = (answer.sessions ?? []).find(
          (row) => row.engine_session_id === props.sessionId,
        );
        setOpenedFor(link?.work_item ?? null);
      })
      .catch(() => setOpenedFor(null));

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
    search = new SearchAddon();
    term.loadAddon(search);
    search.onDidChangeResults((info) => props.onSearchResults?.(info));
    // A PTY program's window title (OSC 0/2) names the tab, and its bell (BEL)
    // lights the tab's activity dot — neither reached the tabs before.
    term.onTitleChange((title) => {
      const trimmed = title.trim();
      if (trimmed) props.onTitle?.(trimmed);
    });
    term.onBell(() => props.onBell?.());
    term.open(hostRef);
    configureTerminalTextarea(term.textarea);
    fitAndResize();

    // Wire input: user keystrokes → PTY stdin.
    term.onData((data) => dispatchInput(data));

    // Paste MUST go through term.paste(): it emits the bracketed-paste markers
    // (\x1b[200~...\x1b[201~) only when the foreground program enabled DECSET
    // 2004, and normalizes newlines to \r. Injecting clipboard text straight
    // into the PTY breaks every bracketed-paste-aware program — multi-line
    // pastes execute line-by-line in shells and submit early in TUIs (Claude
    // Code, opencode), and vim mangles indentation. A program that reads stdin
    // while a stale 2004 mode is left enabled (the old infisical token bug)
    // is a terminal-state leak recoverable with `reset`; don't fix it here by
    // stripping bracketing for everyone.

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
      if (text) term?.paste(text);
    };

    // Custom key handler for Ctrl+Shift+C/V and Cmd+C/V semantics.
    // Returning false stops xterm from forwarding the keystroke to the PTY —
    // but it does NOT cancel the browser default. Every combo handled here
    // must also preventDefault(), or the browser's own copy/paste fires on
    // top of ours (Ctrl+Shift+V pasted twice before this was added).
    term.attachCustomKeyEventHandler((e) => {
      if (e.type !== "keydown") return true;
      const meta = e.ctrlKey && e.shiftKey;
      const mac = navigator.platform.toLowerCase().includes("mac") && e.metaKey;
      // Ctrl/Cmd+Shift+F opens the workspace find bar. Handled here so it works
      // even when xterm holds keyboard focus.
      if ((e.ctrlKey || e.metaKey) && e.shiftKey && (e.key === "f" || e.key === "F")) {
        e.preventDefault();
        props.onRequestFind?.();
        return false;
      }
      if ((meta || mac) && (e.key === "c" || e.key === "C")) {
        if (term?.hasSelection()) {
          e.preventDefault();
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
        e.preventDefault();
        copySelection().then((success) => {
          if (success) {
            props.onNotify?.("Copied to clipboard", "info");
          }
        });
        term.clearSelection();
        return false;
      }
      if ((meta || mac) && (e.key === "v" || e.key === "V")) {
        e.preventDefault();
        void pasteFromClipboard();
        return false;
      }
      if ((meta || mac) && (e.key === "a" || e.key === "A")) {
        e.preventDefault();
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
      hostRef?.removeEventListener("auxclick", onAuxClick);
      hostRef?.removeEventListener("contextmenu", onContextMenu);
      terminalDomCleanup = null;
    };

    // Auto-copy on selection-end is nice on desktop but surprising on mobile
    // (long-press to select → release accidentally clobbers the clipboard).
    // We hold this back and rely on the explicit shortcuts / context menu.
    //
    // But a coarse pointer has neither: no right-click, and iOS Safari fires no
    // contextmenu on a long-press. So on touch we surface an explicit Copy chip
    // whenever there is a live selection — a tap copies, it does not auto-fire.
    runCopyChip = () => {
      setShowCopyChip(false);
      void copySelection().then((success) => {
        props.onNotify?.(
          success ? "Copied to clipboard" : "Copy failed - check clipboard permissions",
          success ? "info" : "error",
        );
      });
      term?.clearSelection();
    };
    if (isCoarsePointer) {
      term.onSelectionChange(() => {
        setShowCopyChip((term?.getSelection() ?? "").length > 0);
      });
    }

    const runSearch = (dir: "next" | "prev", query: string) => {
      if (!search) return;
      if (!query) {
        search.clearDecorations();
        return;
      }
      if (dir === "next") search.findNext(query, { decorations: SEARCH_DECORATIONS });
      else search.findPrevious(query, { decorations: SEARCH_DECORATIONS });
    };

    props.registerActions?.({
      copy: copySelection,
      paste: pasteFromClipboard,
      selectAll: () => term?.selectAll(),
      findNext: (query) => runSearch("next", query),
      findPrevious: (query) => runSearch("prev", query),
      clearSearch: () => search?.clearDecorations(),
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
    window.addEventListener(TERMINAL_FONT_SIZE_EVENT, fontSizeHandler);

    themeHandler = () => {
      if (term) term.options.theme = getTheme();
    };
    window.addEventListener(TERMINAL_THEME_EVENT, themeHandler);

    props.registerSend?.(sendToPty);

    // On mobile the OS kills the WebSocket when the app is backgrounded.
    // Reconnect immediately when the page becomes visible again.
    visibilityHandler = () => {
      if (document.visibilityState !== "visible") return;
      scheduleFit();
      term?.scrollToBottom();
      if (!readyToConnect) return;
      if (!ws || ws.readyState === WebSocket.CLOSED || ws.readyState === WebSocket.CLOSING) {
        if (reconnectTimer !== null) { clearTimeout(reconnectTimer); reconnectTimer = null; }
        clearCountdown();
        connect();
      }
    };
    document.addEventListener("visibilitychange", visibilityHandler);

    void loadTerminalCache(props.sessionId).then((cached) => {
      if (destroyed) return;
      if (!cached || cached.data.byteLength === 0) {
        readyToConnect = true;
        connect();
        return;
      }
      const bytes = new Uint8Array(cached.data);
      cacheChunks = [bytes];
      cacheBytes = bytes.byteLength;
      setStatusText("Restoring terminal...");
      term?.write(bytes, () => {
        if (destroyed) return;
        outputPosition = cached.outputPosition;
        term?.scrollToBottom();
        readyToConnect = true;
        connect();
      });
    });

    onCleanup(() => {
      cleanupTouchGestures();
    });
  });

  function markSessionGone() {
    if (!sessionGone) {
      sessionGone = true;
      dropPendingInput();
      clearCountdown();
      setReconnectView(null);
      setStatusText("Session unavailable");
      term?.write("\r\n\x1b[31m[session not found — server may have restarted]\x1b[0m\r\n");
    }
  }

  function isSessionGone(): boolean {
    return sessionsStore.ready && !sessionsStore.sessions[props.sessionId];
  }

  function scheduleReconnect(overrideDelayMs?: number) {
    if (destroyed || reconnectTimer !== null) return;
    if (isSessionGone()) { markSessionGone(); return; }
    const snap = reconnect.scheduleAttempt();
    const delay = overrideDelayMs ?? snap.delayMs;
    reconnectAt = Date.now() + delay;
    // The interactive overlay owns this window; hand it the attempt count and a
    // live countdown, and drop the plain status text so the two don't overlap.
    setStatusText(null);
    setReconnectView({ attempt: snap.attempt, nextInSec: Math.max(0, Math.ceil(delay / 1000)) });
    clearCountdown();
    countdownTimer = setInterval(() => {
      const remain = Math.max(0, Math.ceil((reconnectAt - Date.now()) / 1000));
      setReconnectView((cur) => (cur ? { ...cur, nextInSec: remain } : cur));
    }, 250);
    reconnectTimer = setTimeout(() => {
      reconnectTimer = null;
      clearCountdown();
      if (!destroyed) connect();
    }, delay);
  }

  function retryNow() {
    if (destroyed) return;
    if (isSessionGone()) { markSessionGone(); return; }
    if (reconnectTimer !== null) { clearTimeout(reconnectTimer); reconnectTimer = null; }
    clearCountdown();
    connect();
  }

  function connect() {
    if (isSessionGone()) { markSessionGone(); return; }
    inSnapshot = true;
    // We are actively attaching now, not waiting to retry — retire the overlay.
    clearCountdown();
    setReconnectView(null);
    setStatusText("Loading terminal...");
    ws = openAttach(props.sessionId, outputPosition);
    ws.addEventListener("open", () => {
      reconnect.recover();
      sendResize();
      flushPendingInput();
    });
    ws.addEventListener("message", (ev) => {
      if (typeof ev.data === "string") {
        try {
          const ctrl = JSON.parse(ev.data) as
            | {
                type: "snapshot-start";
                scrollback_bytes?: number;
                scrollback_pos?: number;
                reset?: boolean;
              }
            | { type: "snapshot-done" }
            | { type: "lag"; note?: string };
          if (ctrl.type === "snapshot-start") {
            if (ctrl.reset !== false) {
              term?.reset();
              cacheChunks = [];
              cacheBytes = 0;
            }
            if (typeof ctrl.scrollback_pos === "number") {
              snapshotEndPosition = ctrl.scrollback_pos;
              outputPosition = Math.max(
                0,
                ctrl.scrollback_pos - (ctrl.scrollback_bytes ?? 0),
              );
            }
            inSnapshot = true;
            setStatusText("Loading terminal...");
          } else if (ctrl.type === "snapshot-done") {
            inSnapshot = false;
            outputPosition = snapshotEndPosition ?? outputPosition;
            snapshotEndPosition = undefined;
            setStatusText(null);
            sendResize();
            term?.write("", () => term?.scrollToBottom());
            persistCache();
          } else if (ctrl.type === "lag") {
            term?.write("\r\n\x1b[31m[lag — reattaching]\x1b[0m\r\n");
            setStatusText("Reattaching terminal...");
            // Cancel any timer so the close event below doesn't double-schedule.
            if (reconnectTimer !== null) { clearTimeout(reconnectTimer); reconnectTimer = null; }
            ws?.close();
            scheduleReconnect(100);
          }
        } catch {
          /* ignore non-JSON text frames */
        }
        return;
      }
      const buf = new Uint8Array(ev.data as ArrayBuffer);
      outputPosition = (outputPosition ?? 0) + buf.byteLength;
      appendToCache(buf);
      if (!inSnapshot) scheduleCachePersist();
      term?.write(buf);
    });
    ws.addEventListener("close", () => {
      // Write the [disconnected] marker once at the start of the outage, not on
      // every retry, and never through appendToCache — it is a live hint, not
      // part of the replayable scrollback.
      if (!inSnapshot) {
        const { writeMarker } = reconnect.beginOutage();
        if (writeMarker) {
          term?.write("\r\n\x1b[33m[disconnected]\x1b[0m\r\n");
        }
      }
      scheduleReconnect();
    });
    ws.addEventListener("error", () => {
      // Browser fires both error + close; close handler is enough.
    });
  }

  onCleanup(() => {
    destroyed = true;
    pendingInput = [];
    pendingInputBytes = 0;
    if (reconnectTimer !== null) { clearTimeout(reconnectTimer); reconnectTimer = null; }
    clearCountdown();
    if (cacheTimer !== null) { clearTimeout(cacheTimer); cacheTimer = null; }
    persistCache();
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
      window.removeEventListener(TERMINAL_FONT_SIZE_EVENT, fontSizeHandler);
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
    search = null;
  });

  return (
    <>
      <div class="terminal-shell">
        {/*
          FR-U20's return leg. A terminal opened for a work item links back to
          it — the forward link (item → terminal) shipped with the work item
          surface, and a link that only goes one way leaves whoever is looking
          at the terminal to work out what it was for.

          Vogt is asked, not the engine: the engine knows this PTY and nothing
          about why it exists. An unreachable Vogt, or a session Vogt did not
          start, contributes nothing and says nothing — this is a badge, not a
          surface, and a terminal must keep working whatever Vogt is doing
          (FR-E9).
        */}
        <Show when={openedFor()}>
          {(ref) => (
            <a
              class="terminal-work-link"
              href={`#/w/${encodeURIComponent(ref())}`}
              title="The work item this session was opened for"
            >
              ✦ {ref()}
            </a>
          )}
        </Show>
        <div class="terminal-host" ref={hostRef} />
        <Show when={reconnectView()}>
          {(info) => (
            <div class="terminal-status-overlay terminal-reconnect-overlay" role="status">
              <span class="terminal-reconnect-line">
                {formatReconnectStatus(info().attempt, info().nextInSec)}
              </span>
              <Show when={queuedBytes() > 0}>
                <span class="terminal-reconnect-queued">
                  {formatQueuedBytes(queuedBytes())}
                </span>
              </Show>
              <button
                type="button"
                class="terminal-reconnect-retry"
                onClick={() => retryNow()}
              >
                Retry now
              </button>
            </div>
          )}
        </Show>
        <Show when={!reconnectView() && statusText()}>
          {(text) => <div class="terminal-status-overlay">{text()}</div>}
        </Show>
        <Show when={showCopyChip()}>
          <button
            type="button"
            class="terminal-copy-chip"
            onPointerDown={(event) => {
              // Grab the tap before the terminal's own pointer handling can
              // collapse the selection out from under us.
              event.preventDefault();
              runCopyChip();
            }}
          >
            Copy
          </button>
        </Show>
      </div>
      <Show when={showPasteModal()}>
        <Dialog
          labelledBy="paste-dialog-title"
          onClose={cancelPasteModal}
          backdropClass="paste-modal-backdrop"
          dialogClass="paste-modal"
        >
            <div id="paste-dialog-title" class="paste-modal-title">Paste text</div>
            <textarea
              ref={(el) => { pasteTextareaRef = el; }}
              data-dialog-initial-focus
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
        </Dialog>
      </Show>
    </>
  );
};

export default TerminalView;
