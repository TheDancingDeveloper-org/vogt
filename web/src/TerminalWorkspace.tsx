import {
  Component,
  For,
  Match,
  Show,
  Switch,
  createEffect,
  createMemo,
  createSignal,
  onCleanup,
  onMount,
} from "solid-js";
import Terminal, { type TerminalActions } from "./Terminal";
import type { ISearchResultChangeEvent } from "@xterm/addon-search";
import type { SessionSummary } from "./api";
import {
  createSession,
  deleteSession,
  killSession,
  sessionsStore,
  isConnected,
} from "./store";
import {
  formatTerminalInputLimit,
  terminalInputTooLarge,
} from "./terminalInput";
import {
  collectPanes,
  commitCreatedPane,
  containsSession,
  dropSessionIntoPane,
  findPane,
  firstPane,
  insertPane,
  makePane,
  MIN_SPLIT_FRACTION,
  normalizeSizes,
  normalizeTerminalLayout,
  paneIdFor,
  pruneTerminalLayout,
  removePane,
  resetDivider,
  resizeSplit,
  retargetPane,
  type PaneNode,
  type SavedTerminalLayout,
  type SplitDirection,
  type SplitNode,
  type TerminalLayoutNode,
} from "./terminalLayout";
import {
  directionForZone,
  dragCarriesSession,
  dropZoneForPoint,
  SESSION_DND_MIME,
  zoneInsertsBefore,
  type DropZone,
} from "./terminalDnd";
import Dialog from "./Dialog";
import { registerTerminalWorkspace } from "./paneComposeBus";
import {
  changeTerminalFontSize,
  MAX_TERMINAL_FONT_SIZE,
  MIN_TERMINAL_FONT_SIZE,
  readTerminalFontSize,
  resetTerminalFontSize,
  TERMINAL_FONT_SIZE_EVENT,
} from "./terminalFont";
import {
  getThemeName,
  setThemeName,
  THEMES,
  TERMINAL_THEME_EVENT,
} from "./terminalThemes";
import { autoSplitName } from "./terminalNaming";
import { terminalPaneParked } from "./tabLifecycle";
import {
  activityLabel,
  sessionStateWord,
  sortSessionsByAttention,
} from "./sessionRowModel";
import ModKeyRow from "./ModKeyRow";
import { createNarrow } from "./narrow";
import GoToButton from "./GoToButton";
import {
  adjacentMobilePagerIndex,
  beginMobilePagerGesture,
  moveMobilePagerGesture,
  settleMobilePagerIndex,
  type MobilePagerGesture,
} from "./mobilePager";

interface Props {
  tabId: string;
  sessionId: string;
  parked?: boolean;
  registerSend?: (fn: ((data: string | ArrayBuffer) => void) | null) => void;
  registerActions?: (actions: TerminalActions | null) => void;
  confirmClosePane?: (session: SessionSummary | null) => Promise<boolean>;
  onError?: (message: string) => void;
  onNotify?: (message: string, kind?: "info" | "error") => void;
  /** Restart an exited session in place (reuses the duplicate-session flow). */
  onRestartExited?: (session: SessionSummary) => void;
  /** Remove an exited session (reuses the close-session flow; no kill prompt). */
  onRemoveExited?: (session: SessionSummary) => void;
  /** The tab's own session set its window title (OSC 0/2) — relabels the tab. */
  onTitle?: (title: string) => void;
  /** A pane rang the bell (BEL) — lights the tab's activity dot. */
  onBell?: (sessionId: string) => void;
  /** Change the active mobile pager item and URL without inventing a second workspace. */
  onMobileSessionChange?: (sessionId: string) => void;
  /** The gesture's identity vanished before settlement; leave the pager safely. */
  onMobileSessionUnavailable?: () => void;
}

const STORAGE_KEY = "vogt.terminalLayouts.v1";

function readSavedLayouts(): Record<string, SavedTerminalLayout> {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    return raw
      ? (JSON.parse(raw) as Record<string, SavedTerminalLayout>)
      : {};
  } catch {
    return {};
  }
}

function readSavedLayout(tabId: string, sessionId: string): SavedTerminalLayout {
  const fallback = {
    root: makePane(sessionId),
    activePaneId: paneIdFor(sessionId),
    broadcast: false,
  };
  const saved = readSavedLayouts()[tabId];
  const root = normalizeTerminalLayout(saved?.root);
  if (!root || !containsSession(root, sessionId)) return fallback;
  const savedActive = saved?.activePaneId ?? fallback.activePaneId;
  return {
    root,
    activePaneId:
      findPane(root, savedActive)?.id ??
      firstPane(root)?.id ??
      fallback.activePaneId,
    broadcast: Boolean(saved?.broadcast),
  };
}

function writeSavedLayout(tabId: string, layout: SavedTerminalLayout) {
  try {
    const all = readSavedLayouts();
    all[tabId] = layout;
    localStorage.setItem(STORAGE_KEY, JSON.stringify(all));
  } catch {
    /* quota / private mode */
  }
}

function activityClass(session: SessionSummary | undefined): string {
  if (!session) return "idle";
  if (session.exit_code !== null) {
    return session.exit_code === 0 ? "done" : "errored";
  }
  return session.activity;
}

interface LayoutNodeProps {
  node: TerminalLayoutNode;
  activePaneId: string;
  parked: boolean;
  /** Whether to draw a per-pane header with its session dropdown (#212). Only
   *  meaningful in a split; a lone pane keeps its whole height for the shell. */
  withHeaders: boolean;
  sessions: SessionSummary[];
  onFocusPane: (paneId: string) => void;
  onRetargetPane: (paneId: string, sessionId: string) => void;
  /** Which pane (and edge) a session drag is currently hovering, for the
   *  drop-zone highlight. Null when no session is being dragged over a pane. */
  dropTarget: { paneId: string; zone: DropZone } | null;
  onPaneDragOver: (paneId: string, event: DragEvent) => void;
  onPaneDragLeave: (paneId: string, event: DragEvent) => void;
  onPaneDrop: (paneId: string, event: DragEvent) => void;
  interceptPaneInput: (paneId: string, data: string | ArrayBuffer) => boolean;
  registerPaneSend: (
    paneId: string,
    fn: ((data: string | ArrayBuffer) => void) | null,
  ) => void;
  registerPaneActions: (paneId: string, actions: TerminalActions | null) => void;
  onNotify?: (message: string, kind?: "info" | "error") => void;
  onRequestFind: () => void;
  onPaneSearchResults: (paneId: string, info: ISearchResultChangeEvent) => void;
  /** A pane's PTY set its window title — the workspace relabels the tab when
   *  the title belongs to the tab's own session. */
  onPaneTitle: (sessionId: string, title: string) => void;
  /** A pane's PTY rang the bell — the workspace lights the tab's activity. */
  onPaneBell: (sessionId: string) => void;
  /** This node's share of its parent split's main axis (#601). Undefined at the
   *  root, which fills the whole layout. */
  flexGrow?: number;
  /** Move `delta` (a fraction) across the divider at `dividerIndex` of a split. */
  onResizeSplit: (
    splitId: string,
    dividerIndex: number,
    delta: number,
    minFraction: number,
  ) => void;
  /** Equalise the two neighbours of a divider (double-click / Home/End). */
  onResetDivider: (splitId: string, dividerIndex: number) => void;
}

/**
 * A draggable divider between two split children (#601).
 *
 * Its neighbours and container are its own DOM siblings/parent, so it needs no
 * refs threaded down. The drag is imperative — it writes `flex-grow` straight
 * onto the two neighbour elements for a smooth 60fps update — and commits once,
 * on pointer-up, through `onResize`, at which point the reactive size re-asserts
 * the same value. Keyboard and double-click go straight to the model.
 */
const DIVIDER_MIN_PANE_PX = 48;

const SplitDivider: Component<{
  direction: SplitDirection;
  onResize: (delta: number, minFraction: number) => void;
  onReset: () => void;
}> = (props) => {
  let el!: HTMLDivElement;
  const horizontal = () => props.direction === "row";
  const growOf = (node: HTMLElement | null) =>
    node ? Number.parseFloat(node.style.flexGrow || "1") || 1 : 1;

  let dragging = false;
  let startPos = 0;
  let containerPx = 0;
  let prevStart = 1;
  let nextStart = 1;
  let prevEl: HTMLElement | null = null;
  let nextEl: HTMLElement | null = null;

  const onPointerDown = (event: PointerEvent) => {
    if (event.button !== 0) return;
    const prev = el.previousElementSibling as HTMLElement | null;
    const next = el.nextElementSibling as HTMLElement | null;
    const container = el.parentElement;
    if (!prev || !next || !container) return;
    event.preventDefault();
    event.stopPropagation();
    prevEl = prev;
    nextEl = next;
    containerPx = horizontal() ? container.clientWidth : container.clientHeight;
    startPos = horizontal() ? event.clientX : event.clientY;
    prevStart = growOf(prev);
    nextStart = growOf(next);
    dragging = true;
    el.setPointerCapture(event.pointerId);
    el.classList.add("dragging");
  };

  const onPointerMove = (event: PointerEvent) => {
    if (!dragging || !prevEl || !nextEl || containerPx <= 0) return;
    const pos = horizontal() ? event.clientX : event.clientY;
    const pair = prevStart + nextStart;
    const min = Math.min(DIVIDER_MIN_PANE_PX / containerPx, pair / 2);
    const wanted = prevStart + (pos - startPos) / containerPx;
    const nextPrev = Math.min(pair - min, Math.max(min, wanted));
    prevEl.style.flexGrow = String(nextPrev);
    nextEl.style.flexGrow = String(pair - nextPrev);
  };

  const end = (event: PointerEvent) => {
    if (!dragging) return;
    dragging = false;
    el.classList.remove("dragging");
    try {
      el.releasePointerCapture(event.pointerId);
    } catch {
      /* not captured */
    }
    if (prevEl) {
      const min =
        containerPx > 0 ? DIVIDER_MIN_PANE_PX / containerPx : MIN_SPLIT_FRACTION;
      props.onResize(growOf(prevEl) - prevStart, min);
    }
    prevEl = nextEl = null;
  };

  const onKeyDown = (event: KeyboardEvent) => {
    const decrease = horizontal() ? "ArrowLeft" : "ArrowUp";
    const increase = horizontal() ? "ArrowRight" : "ArrowDown";
    if (event.key === decrease) {
      event.preventDefault();
      props.onResize(-0.02, MIN_SPLIT_FRACTION);
    } else if (event.key === increase) {
      event.preventDefault();
      props.onResize(0.02, MIN_SPLIT_FRACTION);
    } else if (event.key === "Home" || event.key === "End") {
      event.preventDefault();
      props.onReset();
    }
  };

  return (
    <div
      ref={el}
      class={`terminal-split-divider ${props.direction}`}
      role="separator"
      tabindex="0"
      aria-orientation={horizontal() ? "vertical" : "horizontal"}
      aria-label="Resize panes"
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={end}
      onPointerCancel={end}
      onDblClick={() => props.onReset()}
      onKeyDown={onKeyDown}
    />
  );
};

/**
 * One pane in the split tree.
 *
 * The terminal is keyed on its session id, so re-targeting this pane (the
 * header dropdown, or the command palette) tears the old attach down and mounts
 * a fresh one for the new session. Panes that did not change keep their object
 * identity through the layout update, so the `<For>` in `LayoutNodeView` leaves
 * their DOM, xterm instance and socket in place — only the retargeted pane
 * reloads (#600).
 */
const PaneView: Component<Omit<LayoutNodeProps, "node"> & { node: PaneNode }> = (
  props,
) => {
  const paneId = () => props.node.id;
  const paneSession = () =>
    props.sessions.find((s) => s.id === props.node.sessionId);
  let selectRef: HTMLSelectElement | undefined;
  // Keep the dropdown showing the session this pane actually holds. Setting
  // `value` on a <select> can be applied before its <For> options exist, which
  // drops the browser back to the first option and makes the control lie about
  // the pane — re-assert it once the options are rendered and whenever the
  // pane's session or the session list changes (#600 symptom 1).
  createEffect(() => {
    const want = props.node.sessionId;
    props.sessions.length;
    if (selectRef && selectRef.value !== want) selectRef.value = want;
  });
  return (
    <div
      class={`terminal-pane ${
        props.activePaneId === paneId() ? "active" : ""
      }`}
      style={{ "flex-grow": String(props.flexGrow ?? 1) }}
      onPointerDown={() => props.onFocusPane(paneId())}
      onDragOver={(event) => props.onPaneDragOver(paneId(), event)}
      onDragLeave={(event) => props.onPaneDragLeave(paneId(), event)}
      onDrop={(event) => props.onPaneDrop(paneId(), event)}
    >
      <Show when={props.dropTarget?.paneId === paneId()}>
        <div
          class={`terminal-pane-dropzone ${props.dropTarget?.zone ?? ""}`}
          aria-hidden="true"
        />
      </Show>
      <Show when={props.withHeaders}>
        <div class="terminal-pane-header">
          <span class={`activity-dot ${activityClass(paneSession())}`} />
          <label class="visually-hidden" for={`pane-session-${paneId()}`}>
            Session shown in this pane
          </label>
          <select
            ref={selectRef}
            id={`pane-session-${paneId()}`}
            class="terminal-pane-session"
            aria-label="Session shown in this pane"
            title="Show a different session in this pane"
            value={props.node.sessionId}
            onPointerDown={(event) => event.stopPropagation()}
            onChange={(event) => {
              const next = event.currentTarget.value;
              if (next && next !== props.node.sessionId) {
                props.onRetargetPane(paneId(), next);
              }
            }}
          >
            <For each={props.sessions}>
              {(session) => (
                <option
                  value={session.id}
                  selected={session.id === props.node.sessionId}
                >
                  {session.name}
                </option>
              )}
            </For>
          </select>
        </div>
      </Show>
      <Show when={props.node.sessionId} keyed>
        {(sessionId) => (
          <Terminal
            parked={terminalPaneParked(
              !props.parked,
              props.activePaneId === paneId(),
            )}
            interceptInput={(data) => props.interceptPaneInput(paneId(), data)}
            sessionId={sessionId}
            registerSend={(fn) => props.registerPaneSend(paneId(), fn)}
            registerActions={(actions) =>
              props.registerPaneActions(paneId(), actions)
            }
            onNotify={props.onNotify}
            onRequestFind={props.onRequestFind}
            onSearchResults={(info) =>
              props.onPaneSearchResults(paneId(), info)
            }
            onTitle={(title) => props.onPaneTitle(props.node.sessionId, title)}
            onBell={() => props.onPaneBell(props.node.sessionId)}
          />
        )}
      </Show>
    </div>
  );
};

/**
 * Render the split tree.
 *
 * Split children go through `<For>`, which is keyed by object identity, and the
 * layout transforms in `terminalLayout.ts` preserve the reference of every pane
 * they do not touch. So a layout change — a re-target, a split, a close —
 * remounts only the panes that actually changed and leaves the rest (their
 * xterm, scrollback and socket) exactly as they were. Keying the whole subtree
 * on the node object, as this used to, tore every pane down on any edit (#600
 * symptom 2, and the "all panes reload" half of #599/#601).
 */
const LayoutNodeView: Component<LayoutNodeProps> = (props) => {
  const split = () => props.node as SplitNode;
  // Fractions for this split's children (equal when a legacy/fresh split has
  // none). A drag changes only this array, so the memo — and the flex-grow it
  // drives — updates without any pane remounting (#601).
  const sizes = createMemo(() =>
    props.node.type === "split"
      ? normalizeSizes(split().children.length, split().sizes)
      : [],
  );
  return (
    <Switch>
      <Match when={props.node.type === "split"}>
        <div
          class={`terminal-split ${split().direction}`}
          style={{ "flex-grow": String(props.flexGrow ?? 1) }}
        >
          <For each={split().children}>
            {(child, index) => (
              <>
                <Show when={index() > 0}>
                  <SplitDivider
                    direction={split().direction}
                    onResize={(delta, minFraction) =>
                      props.onResizeSplit(
                        split().id,
                        index() - 1,
                        delta,
                        minFraction,
                      )
                    }
                    onReset={() => props.onResetDivider(split().id, index() - 1)}
                  />
                </Show>
                <LayoutNodeView
                  {...props}
                  node={child}
                  flexGrow={sizes()[index()] ?? 1}
                />
              </>
            )}
          </For>
        </div>
      </Match>
      <Match when={props.node.type === "pane"}>
        <PaneView {...props} node={props.node as PaneNode} />
      </Match>
    </Switch>
  );
};

const TerminalWorkspace: Component<Props> = (props) => {
  const initial = readSavedLayout(props.tabId, props.sessionId);
  const [root, setRoot] = createSignal<TerminalLayoutNode>(initial.root);
  const [activePaneId, setActivePaneId] = createSignal(initial.activePaneId);
  const [broadcast, setBroadcast] = createSignal(Boolean(initial.broadcast));
  // The pane (and edge) a rail session drag is hovering, for the drop-zone
  // highlight; cleared on drop or drag-leave.
  const [dropTarget, setDropTarget] = createSignal<{
    paneId: string;
    zone: DropZone;
  } | null>(null);
  // Maximise/solo: show only the active pane at full size, without closing the
  // others (#185). Only meaningful with a split; a single pane already fills.
  const [soloed, setSoloed] = createSignal(false);
  const [busy, setBusy] = createSignal<SplitDirection | "close" | null>(null);
  // When set, the "Split with…" picker is open for this direction (#212): the
  // operator chooses a fresh session or composes one that already exists.
  const [splitPickerDir, setSplitPickerDir] = createSignal<SplitDirection | null>(
    null,
  );
  const [error, setError] = createSignal<string | null>(null);
  const [draft, setDraft] = createSignal("");
  // Find bar (#234): opened by Ctrl/Cmd+Shift+F or the toolbar button, drives
  // the active pane's search addon.
  const [findOpen, setFindOpen] = createSignal(false);
  const [findQuery, setFindQuery] = createSignal("");
  const [findResults, setFindResults] = createSignal<ISearchResultChangeEvent | null>(null);
  // A toolbar readout of the terminal-only font size, kept in step with the
  // A−/A+ controls and any other pane that changed it.
  const [fontSize, setFontSize] = createSignal(readTerminalFontSize());
  const [themeName, setThemeNameSignal] = createSignal(getThemeName());
  // Toolbar overflow (#236): on a phone, nine text buttons forced a hidden
  // horizontal scroll strip. Below 768px the pane-management actions
  // (Broadcast/Maximise/Split/Close) collapse into a single `···` menu.
  const isNarrow = createNarrow();
  const [overflowOpen, setOverflowOpen] = createSignal(false);
  const [mobileSwipeOffset, setMobileSwipeOffset] = createSignal(0);
  const [mobileDestinationId, setMobileDestinationId] = createSignal<string | null>(null);
  const [mobileDestinationDirection, setMobileDestinationDirection] = createSignal<-1 | 1>(1);
  const [mobileDragging, setMobileDragging] = createSignal(false);
  const [mobileSettling, setMobileSettling] = createSignal(false);
  let mobileGesture: MobilePagerGesture | null = null;
  let mobileForceHorizontal = false;
  let mobileSettleTimer: ReturnType<typeof setTimeout> | undefined;
  let composerRef: HTMLTextAreaElement | undefined;
  let findInputRef: HTMLInputElement | undefined;
  let overflowRef: HTMLDivElement | undefined;
  let mobileOverflowRef: HTMLDivElement | undefined;
  let mobileSessionBarRef: HTMLDivElement | undefined;
  let mobileStageRef: HTMLDivElement | undefined;

  onMount(() => {
    const onFont = (event: Event) => {
      const next = (event as CustomEvent<{ fontSize?: number }>).detail?.fontSize;
      if (typeof next === "number") setFontSize(next);
    };
    const onTheme = (event: Event) => {
      const next = (event as CustomEvent<{ name?: string }>).detail?.name;
      if (typeof next === "string") setThemeNameSignal(next);
    };
    // Close the overflow menu on any tap outside it.
    const onDocPointer = (event: PointerEvent) => {
      if (!overflowOpen()) return;
      const target = event.target as Node | null;
      if (target && (!overflowRef || !overflowRef.contains(target)) && (!mobileOverflowRef || !mobileOverflowRef.contains(target))) {
        setOverflowOpen(false);
      }
    };
    window.addEventListener(TERMINAL_FONT_SIZE_EVENT, onFont);
    window.addEventListener(TERMINAL_THEME_EVENT, onTheme);
    document.addEventListener("pointerdown", onDocPointer);
    onCleanup(() => {
      window.removeEventListener(TERMINAL_FONT_SIZE_EVENT, onFont);
      window.removeEventListener(TERMINAL_THEME_EVENT, onTheme);
      document.removeEventListener("pointerdown", onDocPointer);
    });
  });

  onCleanup(() => {
    if (mobileSettleTimer !== undefined) clearTimeout(mobileSettleTimer);
  });

  const openFind = () => {
    setFindOpen(true);
    queueMicrotask(() => {
      findInputRef?.focus();
      findInputRef?.select();
    });
  };

  const runFind = (dir: "next" | "prev") => {
    const query = findQuery();
    const actions = paneActions.get(activePaneId());
    if (!actions) return;
    if (dir === "next") actions.findNext?.(query);
    else actions.findPrevious?.(query);
  };

  const closeFind = () => {
    setFindOpen(false);
    setFindResults(null);
    paneActions.get(activePaneId())?.clearSearch?.();
  };

  const onPaneSearchResults = (paneId: string, info: ISearchResultChangeEvent) => {
    // The find bar only ever drives the active pane, so only its results matter.
    if (paneId === activePaneId()) setFindResults(info);
  };
  const paneSenders = new Map<string, (data: string | ArrayBuffer) => void>();
  const paneActions = new Map<string, TerminalActions>();
  let disposed = false;

  const panes = createMemo(() => collectPanes(root()));
  const activePane = createMemo(
    () => findPane(root(), activePaneId()) ?? firstPane(root()),
  );
  const paneSummaries = createMemo(() =>
    panes().map((pane) => ({
      pane,
      session: sessionsStore.sessions[pane.sessionId],
    })),
  );
  const activeSession = createMemo(() => {
    const pane = activePane();
    return pane ? sessionsStore.sessions[pane.sessionId] : undefined;
  });
  const broadcastEnabled = createMemo(() => broadcast() && panes().length > 1);
  const soloEnabled = createMemo(() => soloed() && panes().length > 1);
  const canCloseActivePane = createMemo(() => {
    const pane = activePane();
    return Boolean(
      pane && panes().length > 1 && pane.sessionId !== props.sessionId,
    );
  });
  // Every session, in the order the rail shows them — the choices a pane's
  // header dropdown offers and the pool the split picker draws from (#212).
  const allSessions = createMemo(() =>
    sessionsStore.order
      .map((id) => sessionsStore.sessions[id])
      .filter((session): session is SessionSummary => Boolean(session)),
  );
  const mobileSessions = createMemo(() => sortSessionsByAttention(allSessions()));
  const mobileIndex = createMemo(() => {
    const index = mobileSessions().findIndex((session) => session.id === props.sessionId);
    return index >= 0 ? index : 0;
  });
  const mobileSelected = createMemo(() => mobileSessions()[mobileIndex()] ?? activeSession());
  const mobileDestination = createMemo(() => {
    const id = mobileDestinationId();
    return id ? mobileSessions().find((session) => session.id === id) : undefined;
  });
  const focusMobileSessionBar = () => {
    const editing = document.activeElement?.closest(".terminal-composer") !== null;
    if (!editing) {
      queueMicrotask(() =>
        document.querySelector<HTMLElement>("[data-mobile-session-bar]")?.focus(),
      );
    }
  };
  const changeMobileSessionById = (sessionId: string) => {
    const target = mobileSessions().find((session) => session.id === sessionId);
    if (!target || target.id === props.sessionId) return;
    props.onMobileSessionChange?.(target.id);
    focusMobileSessionBar();
  };
  const changeMobileSession = (index: number) => {
    const target = mobileSessions()[index];
    if (target) changeMobileSessionById(target.id);
  };
  const mobileTransitionMs = () =>
    window.matchMedia?.("(prefers-reduced-motion: reduce)").matches ? 0 : 280;
  const clearMobileGesture = (delay = mobileTransitionMs()) => {
    mobileGesture = null;
    setMobileDragging(false);
    setMobileSettling(delay > 0);
    setMobileSwipeOffset(0);
    if (mobileSettleTimer !== undefined) clearTimeout(mobileSettleTimer);
    mobileSettleTimer = setTimeout(() => {
      setMobileSettling(false);
      setMobileDestinationId(null);
    }, delay);
  };
  const onMobileTouchStart = (event: TouchEvent, forceHorizontal = false) => {
    if (mobileSettling()) return;
    const touch = event.touches[0];
    if (!touch) return;
    mobileGesture = beginMobilePagerGesture(touch.clientX, touch.clientY);
    mobileForceHorizontal = forceHorizontal;
    setMobileSwipeOffset(0);
  };
  const onMobileTouchMove = (event: TouchEvent) => {
    const touch = event.touches[0];
    if (!mobileGesture || !touch) return;
    const move = moveMobilePagerGesture(
      mobileGesture,
      touch.clientX,
      touch.clientY,
      mobileIndex(),
      mobileSessions().length,
      mobileForceHorizontal,
    );
    mobileGesture = move.gesture;
    if (!move.claimed) return;
    event.preventDefault();
    setMobileDragging(true);
    setMobileSwipeOffset(move.offset);
    const destinationIndex = adjacentMobilePagerIndex(
      mobileIndex(),
      mobileSessions().length,
      move.offset,
    );
    const destination = destinationIndex === null
      ? undefined
      : mobileSessions()[destinationIndex];
    setMobileDestinationId(destination?.id ?? null);
    if (destinationIndex !== null) {
      setMobileDestinationDirection(destinationIndex > mobileIndex() ? 1 : -1);
    }
  };
  const onMobileTouchEnd = () => {
    const offset = mobileSwipeOffset();
    const settledIndex = settleMobilePagerIndex(
      mobileIndex(),
      mobileSessions().length,
      offset,
    );
    // The destination identity was chosen while the finger was down. Keep it
    // even if attention ordering changes before release; looking up the new
    // occupant of the old numeric index would make a live reorder swipe to a
    // different session than the one visible under the finger.
    const destinationId = settledIndex === null
      ? null
      : mobileDestinationId();
    mobileGesture = null;
    setMobileDragging(false);
    if (!destinationId) {
      clearMobileGesture();
      return;
    }

    const direction = mobileDestinationDirection();
    const distance = mobileStageRef?.getBoundingClientRect().width
      ?? window.innerWidth;
    const delay = mobileTransitionMs();
    setMobileDestinationDirection(direction);
    setMobileDestinationId(destinationId);
    setMobileSettling(true);
    setMobileSwipeOffset(direction > 0 ? -distance : distance);
    if (mobileSettleTimer !== undefined) clearTimeout(mobileSettleTimer);
    mobileSettleTimer = setTimeout(() => {
      // The attention order may have changed while the finger was down. The
      // settled identity wins only if it still exists in the latest snapshot.
      const stillExists = mobileSessions().some(
        (session) => session.id === destinationId,
      );
      setMobileSettling(false);
      setMobileDestinationId(null);
      setMobileSwipeOffset(0);
      if (stillExists) changeMobileSessionById(destinationId);
      else if (props.onMobileSessionUnavailable) {
        props.onMobileSessionUnavailable();
      } else {
        props.onNotify?.("That session is no longer available.", "info");
      }
    }, delay);
  };
  const onMobileTouchCancel = () => clearMobileGesture();
  const onMobilePagerKeyDown = (event: KeyboardEvent) => {
    if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return;
    event.preventDefault();
    changeMobileSession(mobileIndex() + (event.key === "ArrowRight" ? 1 : -1));
  };
  // Sessions not already bound to a pane here: the ones worth composing in,
  // since a session is never shown in two panes at once.
  const eligibleSessions = createMemo(() => {
    const shown = new Set(panes().map((pane) => pane.sessionId));
    return allSessions().filter((session) => !shown.has(session.id));
  });

  const sendToTargets = (data: string | ArrayBuffer, originPaneId?: string) => {
    const targetPaneIds = broadcastEnabled()
      ? panes().map((pane) => pane.id)
      : [originPaneId ?? activePaneId()];
    const seen = new Set<string>();
    for (const paneId of targetPaneIds) {
      if (seen.has(paneId)) continue;
      seen.add(paneId);
      paneSenders.get(paneId)?.(data);
    }
  };

  const interceptPaneInput = (paneId: string, data: string | ArrayBuffer) => {
    if (!broadcastEnabled()) return false;
    if (paneId !== activePaneId()) return false;
    sendToTargets(data, paneId);
    return true;
  };

  const sendToActive = (data: string | ArrayBuffer) => {
    sendToTargets(data);
  };

  const sendDraft = (submit: boolean) => {
    const text = draft();
    if (!text && !submit) return;
    if (text && terminalInputTooLarge(text)) {
      const message = `Input not sent: the terminal limit is ${formatTerminalInputLimit()}. Shorten it or move the content through a file.`;
      setError(message);
      props.onError?.(message);
      return;
    }
    if (text) sendToActive(text);
    if (submit) sendToActive("\r");
    setDraft("");
  };

  const insertDraftNewline = (textarea: HTMLTextAreaElement) => {
    const value = draft();
    const start = textarea.selectionStart ?? value.length;
    const end = textarea.selectionEnd ?? start;
    const next = `${value.slice(0, start)}\n${value.slice(end)}`;
    setDraft(next);
    queueMicrotask(() => {
      textarea.selectionStart = start + 1;
      textarea.selectionEnd = start + 1;
    });
  };

  const focusComposer = () => {
    const el = composerRef;
    if (!el) return;
    el.focus();
    const pos = el.value.length;
    el.selectionStart = pos;
    el.selectionEnd = pos;
  };

  const workspaceActions: TerminalActions = {
    copy: async () => {
      const result = await paneActions.get(activePaneId())?.copy();
      return result ?? false;
    },
    paste: async () => {
      await paneActions.get(activePaneId())?.paste();
    },
    selectAll: () => {
      paneActions.get(activePaneId())?.selectAll();
    },
    focusComposer,
  };

  createEffect(() => {
    props.registerSend?.(sendToActive);
    props.registerActions?.(workspaceActions);
  });

  onCleanup(() => {
    disposed = true;
    paneSenders.clear();
    paneActions.clear();
    props.registerSend?.(null);
    props.registerActions?.(null);
  });

  createEffect(() => {
    const livePaneIds = new Set(panes().map((pane) => pane.id));
    for (const id of paneSenders.keys()) {
      if (!livePaneIds.has(id)) paneSenders.delete(id);
    }
    for (const id of paneActions.keys()) {
      if (!livePaneIds.has(id)) paneActions.delete(id);
    }
  });

  createEffect(() => {
    const currentRoot = root();
    const currentActive = activePaneId();
    writeSavedLayout(props.tabId, {
      root: currentRoot,
      activePaneId: findPane(currentRoot, currentActive)?.id ?? firstPane(currentRoot)?.id ?? currentActive,
      broadcast: broadcast(),
    });
  });

  createEffect(() => {
    if (!sessionsStore.ready) return;
    const current = root();
    const pruned = pruneTerminalLayout(
      current,
      (sessionId) => Boolean(sessionsStore.sessions[sessionId]),
    );
    if (!pruned) return;
    if (pruned !== current) setRoot(pruned);
    if (!findPane(pruned, activePaneId())) {
      const first = firstPane(pruned);
      if (first) setActivePaneId(first.id);
    }
  });

  // Closing a split back to a single pane returns to the ordinary full-size
  // layout: solo is a multi-pane state and has nothing left to maximise.
  createEffect(() => {
    if (panes().length <= 1 && soloed()) setSoloed(false);
  });

  const reportError = (prefix: string, err: unknown) => {
    const detail = err instanceof Error ? err.message : String(err);
    const message = `${prefix}: ${detail}`;
    setError(message);
    props.onError?.(message);
  };

  const splitActive = async (direction: SplitDirection) => {
    const pane = activePane();
    if (!pane || busy()) return;
    setBusy(direction);
    setError(null);
    try {
      const source = sessionsStore.sessions[pane.sessionId];
      // Splits read as children of the session they came from: `vogt ▸2`,
      // `vogt ▸3`, deduped against every live session name.
      const existingNames = Object.values(sessionsStore.sessions).map((s) => s.name);
      const name = autoSplitName(source?.cwd, existingNames);
      let session: SessionSummary;
      try {
        session = await createSession(name, undefined, source?.cwd || undefined);
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        if (!message.includes("escapes workspace_root")) throw err;
        session = await createSession(name);
        props.onNotify?.("Split opened at the default cwd", "info");
      }
      const nextPane = makePane(session.id);
      await commitCreatedPane(
        session.id,
        () => {
          if (disposed || !sessionsStore.sessions[pane.sessionId]) return false;
          let inserted = false;
          setRoot((current) => {
            const next = insertPane(current, pane.id, direction, nextPane);
            inserted = next !== null;
            return next ?? current;
          });
          return inserted;
        },
        async (sessionId) => {
          try {
            await killSession(sessionId);
          } catch {
            /* a failed or already-exited PTY must still be deleted */
          }
          await deleteSession(sessionId);
        },
      );
      setActivePaneId(nextPane.id);
    } catch (err) {
      reportError("split failed", err);
    } finally {
      setBusy(null);
    }
  };

  // Open the "Split with…" picker, unless nothing exists to compose — then a
  // split can only mean a fresh session, so skip the extra tap and make one.
  const requestSplit = (direction: SplitDirection) => {
    if (busy()) return;
    if (eligibleSessions().length === 0) {
      void splitActive(direction);
      return;
    }
    setSplitPickerDir(direction);
  };

  // Compose a session that already exists into a new pane. Creates no PTY, so
  // no session is spawned and none is ever shown in two panes at once (#212).
  const splitWithExisting = (direction: SplitDirection, sessionId: string) => {
    const pane = activePane();
    if (!pane || busy()) return;
    if (containsSession(root(), sessionId)) return;
    setError(null);
    const nextPane = makePane(sessionId);
    let inserted = false;
    setRoot((current) => {
      const next = insertPane(current, pane.id, direction, nextPane);
      inserted = next !== null;
      return next ?? current;
    });
    if (inserted) setActivePaneId(nextPane.id);
  };

  // Drag a session from the places rail onto a pane to mirror it into a split
  // (#355). No PTY is created — the session already exists; the server's attach
  // fans out from a snapshot, so the same shell renders in both panes, each on
  // its own WebSocket. A session already on screen is not duplicated: its pane
  // is focused instead (the duplicate guard lives in `dropSessionIntoPane`).
  const dropSessionOnPane = (
    targetPaneId: string,
    sessionId: string,
    zone: DropZone,
  ) => {
    // Ignore payloads for sessions this client doesn't know — a stale drag or a
    // foreign source. `splitWithExisting`/`retargetPane` accept only live ids.
    if (!sessionId || !sessionsStore.sessions[sessionId]) return;
    const outcome = dropSessionIntoPane(
      root(),
      targetPaneId,
      sessionId,
      directionForZone(zone),
      zoneInsertsBefore(zone),
    );
    if (!outcome) return;
    setError(null);
    setRoot(outcome.root);
    setActivePaneId(outcome.activePaneId);
  };

  const onPaneDragOver = (paneId: string, event: DragEvent) => {
    if (!dragCarriesSession(event.dataTransfer?.types)) return;
    // preventDefault marks the pane a valid drop target and lets the drop fire.
    event.preventDefault();
    if (event.dataTransfer) event.dataTransfer.dropEffect = "copy";
    const rect = (event.currentTarget as HTMLElement).getBoundingClientRect();
    const zone = dropZoneForPoint(rect, event.clientX, event.clientY);
    const current = dropTarget();
    if (!current || current.paneId !== paneId || current.zone !== zone) {
      setDropTarget({ paneId, zone });
    }
  };

  const onPaneDragLeave = (paneId: string, event: DragEvent) => {
    // dragleave also fires crossing into a child element — only clear when the
    // pointer actually left this pane's box, else the highlight flickers.
    const related = event.relatedTarget as Node | null;
    const host = event.currentTarget as HTMLElement;
    if (related && host.contains(related)) return;
    if (dropTarget()?.paneId === paneId) setDropTarget(null);
  };

  const onPaneDrop = (paneId: string, event: DragEvent) => {
    if (!dragCarriesSession(event.dataTransfer?.types)) return;
    event.preventDefault();
    const sessionId =
      event.dataTransfer?.getData(SESSION_DND_MIME) ||
      event.dataTransfer?.getData("text/plain") ||
      "";
    const current = dropTarget();
    const zone = current?.paneId === paneId ? current.zone : "right";
    setDropTarget(null);
    dropSessionOnPane(paneId, sessionId, zone);
  };

  // Point a pane at another session without disturbing the layout; a clash with
  // a session already on screen swaps the two. No PTY is created (#212).
  const retargetPaneTo = (paneId: string, sessionId: string) => {
    const result = retargetPane(root(), paneId, sessionId);
    if (!result) return;
    setError(null);
    setRoot(result.root);
    setActivePaneId(result.activePaneId);
  };

  // Divider drag/keyboard: adjust one split's child fractions. Pure transforms
  // that touch only the split's spine, so no pane remounts and the new ratios
  // ride along into the saved layout via the persistence effect (#601).
  const resizeSplitBy = (
    splitId: string,
    dividerIndex: number,
    delta: number,
    minFraction: number,
  ) => {
    setRoot((current) =>
      resizeSplit(current, splitId, dividerIndex, delta, minFraction),
    );
  };
  const resetSplitDivider = (splitId: string, dividerIndex: number) => {
    setRoot((current) => resetDivider(current, splitId, dividerIndex));
  };

  // Detach the active pane: drop it from the layout but leave its session
  // running and listed. This never kills or deletes — that is `killActivePane`
  // below, a separate, confirmed act (#212).
  const detachActivePane = () => {
    const pane = activePane();
    if (!pane || !canCloseActivePane() || busy()) return;
    setError(null);
    let nextRoot: TerminalLayoutNode | null = null;
    setRoot((current) => {
      nextRoot = removePane(current, pane.id);
      return nextRoot ?? makePane(props.sessionId);
    });
    const nextPane = nextRoot ? firstPane(nextRoot) : null;
    setActivePaneId(nextPane?.id ?? paneIdFor(props.sessionId));
  };

  const killActivePane = async () => {
    const pane = activePane();
    if (!pane || !canCloseActivePane() || busy()) return;
    const session = sessionsStore.sessions[pane.sessionId] ?? null;
    const ok = props.confirmClosePane
      ? await props.confirmClosePane(session)
      : true;
    if (!ok) return;
    setBusy("close");
    setError(null);
    try {
      try {
        await killSession(pane.sessionId);
      } catch {
        /* session may already be dead */
      }
      await deleteSession(pane.sessionId);
      let nextRoot: TerminalLayoutNode | null = null;
      setRoot((current) => {
        nextRoot = removePane(current, pane.id);
        return nextRoot ?? makePane(props.sessionId);
      });
      const nextPane = nextRoot ? firstPane(nextRoot) : null;
      setActivePaneId(nextPane?.id ?? paneIdFor(props.sessionId));
    } catch (err) {
      reportError("kill pane failed", err);
    } finally {
      setBusy(null);
    }
  };

  // Let the command palette and the rail's session menu compose a session into
  // *this* workspace while it is the active tab (#212).
  createEffect(() => {
    const unregister = registerTerminalWorkspace({
      tabId: props.tabId,
      shownSessionIds: () => panes().map((pane) => pane.sessionId),
      splitWithSession: (direction, sessionId) =>
        splitWithExisting(direction, sessionId),
      showSessionInActivePane: (sessionId) =>
        retargetPaneTo(activePaneId(), sessionId),
      focusSession: (sessionId) => {
        if (!containsSession(root(), sessionId)) return false;
        setActivePaneId(paneIdFor(sessionId));
        return true;
      },
    });
    onCleanup(unregister);
  });

  // The pane-management actions. Rendered inline on a wide toolbar and inside
  // the `···` menu on a phone, so the button logic lives in exactly one place.
  const overflowActions = (mobile = false) => (
    <>
      <Show when={mobile}>
        <div class="terminal-mobile-display-controls" role="group" aria-label="Terminal display">
          <button
            type="button"
            onClick={() => changeTerminalFontSize(-1)}
            disabled={fontSize() <= MIN_TERMINAL_FONT_SIZE}
            aria-label="Decrease terminal font size"
          >
            A−
          </button>
          <button
            type="button"
            onClick={() => resetTerminalFontSize()}
            aria-label={`Terminal font size ${fontSize()}, click to reset`}
          >
            {fontSize()}
          </button>
          <button
            type="button"
            onClick={() => changeTerminalFontSize(1)}
            disabled={fontSize() >= MAX_TERMINAL_FONT_SIZE}
            aria-label="Increase terminal font size"
          >
            A+
          </button>
        </div>
        <label class="terminal-mobile-theme-label">
          <span>Terminal theme</span>
          <select
            aria-label="Terminal color theme"
            value={themeName()}
            onChange={(event) => setThemeName(event.currentTarget.value)}
          >
            <For each={Object.keys(THEMES)}>
              {(name) => <option value={name}>{name}</option>}
            </For>
          </select>
        </label>
        <button
          type="button"
          onClick={() => {
            sendDraft(false);
            setOverflowOpen(false);
          }}
          disabled={!draft()}
          title="Send the composer text without Enter"
        >
          Insert draft without Enter
        </button>
      </Show>
      <button
        class={broadcastEnabled() ? "active" : ""}
        onClick={() => {
          setBroadcast((value) => !value);
          setOverflowOpen(false);
        }}
        title="Send keyboard, paste, composer, and shortcut input to every pane in this workspace"
      >
        {broadcastEnabled() ? "Broadcast on" : "Broadcast off"}
      </button>
      <Show when={panes().length > 1}>
        <button
          class={soloEnabled() ? "active" : ""}
          onClick={() => {
            setSoloed((value) => !value);
            setOverflowOpen(false);
          }}
          title={
            soloEnabled()
              ? "Show every pane in this workspace again"
              : "Maximise the active pane; the others keep running, hidden"
          }
        >
          {soloEnabled() ? "Restore split" : "Maximise"}
        </button>
      </Show>
      <button
        onClick={() => {
          setOverflowOpen(false);
          requestSplit("row");
        }}
        disabled={busy() !== null}
        title="Split right (choose a new or existing session)"
      >
        Split right
      </button>
      <button
        onClick={() => {
          setOverflowOpen(false);
          requestSplit("column");
        }}
        disabled={busy() !== null}
        title="Split down (choose a new or existing session)"
      >
        Split down
      </button>
      <button
        onClick={() => {
          setOverflowOpen(false);
          detachActivePane();
        }}
        disabled={!canCloseActivePane() || busy() !== null}
        title={
          activePane()?.sessionId === props.sessionId
            ? "Root pane stays with this tab"
            : "Detach the active pane; its session keeps running and returns to the tab list"
        }
      >
        Close pane
      </button>
      <button
        class="danger"
        onClick={() => {
          setOverflowOpen(false);
          void killActivePane();
        }}
        disabled={!canCloseActivePane() || busy() !== null}
        title={
          activePane()?.sessionId === props.sessionId
            ? "Root pane stays with this tab"
            : "Kill the active session and close its pane"
        }
      >
        Kill pane
      </button>
    </>
  );

  const renderWorkspaceLayout = () => (
    <LayoutNodeView
      node={root()}
      activePaneId={activePaneId()}
      parked={Boolean(props.parked)}
      withHeaders={panes().length > 1}
      sessions={allSessions()}
      onFocusPane={setActivePaneId}
      onRetargetPane={retargetPaneTo}
      dropTarget={dropTarget()}
      onPaneDragOver={onPaneDragOver}
      onPaneDragLeave={onPaneDragLeave}
      onPaneDrop={onPaneDrop}
      interceptPaneInput={interceptPaneInput}
      registerPaneSend={(paneId, fn) => {
        if (fn) paneSenders.set(paneId, fn);
        else paneSenders.delete(paneId);
      }}
      registerPaneActions={(paneId, actions) => {
        if (actions) paneActions.set(paneId, actions);
        else paneActions.delete(paneId);
      }}
      onNotify={props.onNotify}
      onRequestFind={openFind}
      onPaneSearchResults={onPaneSearchResults}
      onPaneTitle={(sessionId, title) => {
        // Only the tab's own session names the tab; a split showing another
        // session's shell must not rewrite the label out from under it.
        if (sessionId === props.sessionId) props.onTitle?.(title);
      }}
      onPaneBell={(sessionId) => props.onBell?.(sessionId)}
      onResizeSplit={resizeSplitBy}
      onResetDivider={resetSplitDivider}
    />
  );

  return (
    <div class="terminal-workspace">
      <Show when={isNarrow()}>
      <div
        class="terminal-mobile-header"
        ref={mobileSessionBarRef}
        data-mobile-session-bar
        tabIndex={0}
        onKeyDown={onMobilePagerKeyDown}
        onTouchStart={(event) => onMobileTouchStart(event, true)}
        onTouchMove={onMobileTouchMove}
        onTouchEnd={onMobileTouchEnd}
        onTouchCancel={onMobileTouchCancel}
      >
        <a class="terminal-mobile-back" href="#/sessions" aria-label="Back to Sessions">
          <svg viewBox="0 0 24 24" aria-hidden="true">
            <path d="M15 5l-7 7 7 7" />
          </svg>
        </a>
        <span class={`activity-dot ${activityClass(mobileSelected())}`} aria-hidden="true" />
        <div class="terminal-mobile-session">
          <strong>{mobileSelected()?.name ?? "terminal"}</strong>
          <span>
            {mobileSelected() ? sessionStateWord(mobileSelected()!, Date.now(), sessionsStore.ready && !isConnected() ? sessionsStore.lastAnswerAt : null) : "unknown"}
            <Show when={mobileSelected()?.cwd}>
              <small> · {mobileSelected()?.cwd}</small>
            </Show>
          </span>
        </div>
        <GoToButton class="terminal-mobile-icon" />
        <button type="button" class="terminal-mobile-icon" onClick={openFind} aria-label="Find in terminal" title="Find in terminal">
          <svg viewBox="0 0 24 24" aria-hidden="true">
            <circle cx="10.5" cy="10.5" r="5.5" />
            <path d="m15 15 4 4" />
          </svg>
        </button>
        <button type="button" class="terminal-mobile-icon" onClick={() => setOverflowOpen((value) => !value)} aria-label="More terminal actions" title="More terminal actions">
          <svg viewBox="0 0 24 24" aria-hidden="true">
            <circle cx="5" cy="12" r="1.5" />
            <circle cx="12" cy="12" r="1.5" />
            <circle cx="19" cy="12" r="1.5" />
          </svg>
        </button>
        <Show when={overflowOpen()}>
          <div class="terminal-mobile-overflow" ref={mobileOverflowRef} role="menu">
            {overflowActions(true)}
          </div>
        </Show>
      </div>
      <Show when={error()}>
        <div class="terminal-workspace-error terminal-mobile-error" role="alert">
          {error()}
        </div>
      </Show>
      <Show when={mobileSessions().length > 1}>
      <div
        class="terminal-mobile-pager"
        role="region"
        aria-label="Session pager"
        onTouchStart={(event) => onMobileTouchStart(event, true)}
        onTouchMove={onMobileTouchMove}
        onTouchEnd={onMobileTouchEnd}
        onTouchCancel={onMobileTouchCancel}
      >
        <div class="terminal-mobile-pager-strip">
          <For each={mobileSessions()}>
            {(session, index) => (
              <button
                type="button"
                class={`terminal-mobile-dot ${activityClass(session)} ${index() === mobileIndex() ? "active" : ""}`}
                aria-label={`Show ${session.name}`}
                aria-current={index() === mobileIndex() ? "true" : undefined}
                onClick={() => changeMobileSession(index())}
              >
                <span class="terminal-mobile-dot-mark" aria-hidden="true" />
              </button>
            )}
          </For>
        </div>
        <span class="terminal-mobile-counter" aria-hidden="true">
          {mobileIndex() + 1} / {mobileSessions().length}
        </span>
      </div>
      </Show>
      <span class="visually-hidden" aria-live="polite" aria-atomic="true">
        {mobileSelected()
          ? `${mobileSelected()!.name}, ${activityLabel(mobileSelected()!.activity, mobileSelected()!.exit_code)}`
          : "No sessions"}
      </span>
      </Show>
      <div class="terminal-workspace-toolbar">
        <span class={`activity-dot ${activityClass(activeSession())}`} />
        <span class="terminal-workspace-title">
          {activeSession()?.name ?? activePane()?.sessionId.slice(0, 8) ?? "terminal"}
        </span>
        <Show when={activeSession()?.cwd}>
          <span class="terminal-workspace-cwd">{activeSession()?.cwd}</span>
        </Show>
        <Show when={!isNarrow() && error()}>
          <span class="terminal-workspace-error">{error()}</span>
        </Show>
        <span class="terminal-font-readout" role="group" aria-label="Terminal font size">
          <button
            type="button"
            onClick={() => changeTerminalFontSize(-1)}
            disabled={fontSize() <= MIN_TERMINAL_FONT_SIZE}
            title="Decrease terminal font size (browser zoom still scales the whole app)"
            aria-label="Decrease terminal font size"
          >
            A−
          </button>
          <button
            type="button"
            class="terminal-font-value"
            onClick={() => resetTerminalFontSize()}
            title="Reset terminal font size to the default"
            aria-label={`Terminal font size ${fontSize()}, click to reset`}
          >
            {fontSize()}
          </button>
          <button
            type="button"
            onClick={() => changeTerminalFontSize(1)}
            disabled={fontSize() >= MAX_TERMINAL_FONT_SIZE}
            title="Increase terminal font size (browser zoom still scales the whole app)"
            aria-label="Increase terminal font size"
          >
            A+
          </button>
        </span>
        <select
          class="terminal-theme-select"
          aria-label="Terminal color theme"
          title="Terminal color theme"
          value={themeName()}
          onChange={(event) => setThemeName(event.currentTarget.value)}
        >
          <For each={Object.keys(THEMES)}>
            {(name) => <option value={name}>{name}</option>}
          </For>
        </select>
        <button
          type="button"
          class={findOpen() ? "active" : ""}
          onClick={() => (findOpen() ? closeFind() : openFind())}
          title="Search the terminal buffer (Ctrl/Cmd+Shift+F)"
          aria-label="Find in terminal"
        >
          Find
        </button>
        <Show when={!isNarrow()}>{overflowActions()}</Show>
        <Show when={isNarrow()}>
          <div class="terminal-toolbar-overflow" ref={overflowRef}>
            <button
              type="button"
              class={`terminal-toolbar-overflow-toggle ${overflowOpen() ? "active" : ""}`}
              aria-haspopup="menu"
              aria-expanded={overflowOpen()}
              aria-label="More terminal actions"
              title="More actions"
              onClick={() => setOverflowOpen((value) => !value)}
            >
              ···
            </button>
            <Show when={overflowOpen()}>
              <div class="terminal-toolbar-overflow-menu" role="menu">
                {overflowActions()}
              </div>
            </Show>
          </div>
        </Show>
      </div>
      <Show when={findOpen()}>
        <div class="terminal-find-bar" role="search">
          <input
            ref={findInputRef}
            class="terminal-find-input"
            type="text"
            placeholder="Find in terminal…"
            aria-label="Find in terminal"
            value={findQuery()}
            onInput={(event) => {
              setFindQuery(event.currentTarget.value);
              runFind("next");
            }}
            onKeyDown={(event) => {
              if (event.key === "Enter") {
                event.preventDefault();
                runFind(event.shiftKey ? "prev" : "next");
              } else if (event.key === "Escape") {
                event.preventDefault();
                closeFind();
              }
            }}
          />
          <span class="terminal-find-count" aria-live="polite">
            {findResults() && findResults()!.resultCount > 0
              ? `${findResults()!.resultIndex + 1} of ${findResults()!.resultCount}`
              : findQuery()
                ? "No matches"
                : ""}
          </span>
          <button type="button" onClick={() => runFind("prev")} aria-label="Previous match" title="Previous match">
            ↑
          </button>
          <button type="button" onClick={() => runFind("next")} aria-label="Next match" title="Next match">
            ↓
          </button>
          <button type="button" onClick={() => closeFind()} aria-label="Close find" title="Close find (Esc)">
            Esc
          </button>
        </div>
      </Show>
      <Show when={activeSession()?.exit_code != null}>
        <div class="terminal-exited-banner" role="status">
          <span class="terminal-exited-label">
            Exited (code {activeSession()!.exit_code})
          </span>
          <span class="terminal-exited-actions">
            <button
              type="button"
              onClick={() => {
                const session = activeSession();
                if (session) props.onRestartExited?.(session);
              }}
              title="Open a fresh shell in the same working directory"
            >
              Restart here
            </button>
            <button
              type="button"
              class="danger"
              onClick={() => {
                const session = activeSession();
                if (session) props.onRemoveExited?.(session);
              }}
              title="Remove this exited session (scrollback is already archived)"
            >
              Remove
            </button>
          </span>
        </div>
      </Show>
      <Show when={panes().length > 1}>
        <div class="terminal-workspace-roster">
          <span class={`terminal-workspace-roster-badge ${broadcastEnabled() ? "active" : ""}`}>
            {broadcastEnabled() ? "Input fan-out" : "Active pane only"}
          </span>
          <For each={paneSummaries()}>
            {({ pane, session }) => (
              <button
                class={`terminal-pane-chip ${activePaneId() === pane.id ? "active" : ""}`}
                onClick={() => setActivePaneId(pane.id)}
                title={session?.cwd || session?.name || pane.sessionId}
              >
                <span class={`activity-dot ${activityClass(session)}`} />
                <span>{session?.name ?? pane.sessionId.slice(0, 8)}</span>
              </button>
            )}
          </For>
        </div>
      </Show>
      <Show
        when={isNarrow() && panes().length === 1}
        fallback={
          <div class={`terminal-layout ${soloEnabled() ? "soloed" : ""}`}>
            {renderWorkspaceLayout()}
          </div>
        }
      >
        <div
          ref={mobileStageRef}
          class={`terminal-layout terminal-mobile-stage ${mobileDragging() ? "dragging" : ""} ${mobileSettling() ? "settling" : ""}`}
          onTouchStart={(event) => onMobileTouchStart(event)}
          onTouchMove={onMobileTouchMove}
          onTouchEnd={onMobileTouchEnd}
          onTouchCancel={onMobileTouchCancel}
        >
          <div
            class="terminal-mobile-slide terminal-mobile-slide-current"
            style={{ transform: `translate3d(${mobileSwipeOffset()}px, 0, 0)` }}
          >
            {renderWorkspaceLayout()}
          </div>
          <Show when={mobileDestination()} keyed>
            {(destination) => (
              <div
                class="terminal-mobile-slide terminal-mobile-slide-destination"
                aria-hidden={!mobileDragging() && !mobileSettling()}
                style={{
                  transform: `translate3d(calc(${mobileDestinationDirection() * 100}% + ${mobileSwipeOffset()}px), 0, 0)`,
                }}
              >
                <div class="terminal-pane active">
                  <Terminal
                    sessionId={destination.id}
                    onNotify={props.onNotify}
                    onRequestFind={openFind}
                    onBell={() => props.onBell?.(destination.id)}
                  />
                </div>
              </div>
            )}
          </Show>
        </div>
      </Show>
      <div class="terminal-mobile-input-dock">
        <ModKeyRow
          send={(data) => sendToActive(data)}
          onCopy={() => void workspaceActions.copy()}
          onPaste={() => void workspaceActions.paste()}
          onSelectAll={() => workspaceActions.selectAll()}
          onFocusComposer={focusComposer}
        />
        <form
          class="terminal-composer"
          onSubmit={(event) => {
            event.preventDefault();
            sendDraft(true);
          }}
        >
        <textarea
          ref={composerRef}
          value={draft()}
          onInput={(event) => setDraft(event.currentTarget.value)}
          onKeyDown={(event) => {
            if (event.key !== "Enter") return;
            if (event.ctrlKey || event.metaKey) {
              event.preventDefault();
              insertDraftNewline(event.currentTarget);
              return;
            }
            event.preventDefault();
            sendDraft(true);
          }}
          placeholder="Command"
          autocomplete="on"
          autocorrect="on"
          autocapitalize="none"
          spellcheck={true}
          enterkeyhint="send"
          rows={2}
        />
        <button
          class="terminal-composer-line"
          type="button"
          onClick={() => {
            if (composerRef) {
              insertDraftNewline(composerRef);
              focusComposer();
            }
          }}
          title="Insert newline"
        >
          Line
        </button>
        <button
          class="terminal-composer-insert"
          type="button"
          onClick={() => sendDraft(false)}
          disabled={!draft()}
          title="Send text without Enter"
        >
          Insert
        </button>
        <button class="terminal-composer-send" type="submit" title="Send text and Enter" aria-label="Send text and Enter">
          <Show when={isNarrow()} fallback="Enter">
            <svg viewBox="0 0 24 24" aria-hidden="true">
              <path d="m5 12 14-7-4 14-3-5-7-2Z" />
              <path d="m12 14 7-9" />
            </svg>
          </Show>
        </button>
        </form>
      </div>
      <Show when={splitPickerDir()} keyed>
        {(direction) => (
          <Dialog
            label={direction === "row" ? "Split right with…" : "Split down with…"}
            onClose={() => setSplitPickerDir(null)}
            dialogClass="split-picker"
            dismissOnBackdrop
          >
            <div class="split-picker-body">
              <p class="split-picker-title">
                {direction === "row" ? "Split right" : "Split down"}
              </p>
              <div class="split-picker-list" role="listbox" aria-label="Session to show in the new pane">
                <button
                  type="button"
                  class="split-picker-option"
                  data-dialog-initial-focus
                  onClick={() => {
                    setSplitPickerDir(null);
                    void splitActive(direction);
                  }}
                >
                  <span class="split-picker-option-name">New session (current cwd)</span>
                  <span class="split-picker-option-meta">Spawns a fresh shell</span>
                </button>
                <For each={eligibleSessions()}>
                  {(session) => (
                    <button
                      type="button"
                      class="split-picker-option"
                      onClick={() => {
                        setSplitPickerDir(null);
                        splitWithExisting(direction, session.id);
                      }}
                    >
                      <span class={`activity-dot ${activityClass(session)}`} />
                      <span class="split-picker-option-name">{session.name}</span>
                      <Show when={session.cwd}>
                        <span class="split-picker-option-meta">{session.cwd}</span>
                      </Show>
                    </button>
                  )}
                </For>
              </div>
            </div>
          </Dialog>
        )}
      </Show>
    </div>
  );
};

export default TerminalWorkspace;
