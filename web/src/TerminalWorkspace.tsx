import {
  Component,
  For,
  Show,
  createEffect,
  createMemo,
  createSignal,
  onCleanup,
} from "solid-js";
import Terminal, { type TerminalActions } from "./Terminal";
import type { SessionSummary } from "./api";
import {
  createSession,
  deleteSession,
  killSession,
  sessionsStore,
} from "./store";
import {
  formatTerminalInputLimit,
  terminalInputTooLarge,
} from "./terminalInput";
import {
  collectPanes,
  commitCreatedPane,
  containsSession,
  findPane,
  firstPane,
  insertPane,
  makePane,
  normalizeTerminalLayout,
  paneIdFor,
  pruneTerminalLayout,
  removePane,
  type SavedTerminalLayout,
  type SplitDirection,
  type TerminalLayoutNode,
} from "./terminalLayout";
import { changeTerminalFontSize } from "./terminalFont";

interface Props {
  tabId: string;
  sessionId: string;
  registerSend?: (fn: ((data: string | ArrayBuffer) => void) | null) => void;
  registerActions?: (actions: TerminalActions | null) => void;
  confirmClosePane?: (session: SessionSummary | null) => Promise<boolean>;
  onError?: (message: string) => void;
  onNotify?: (message: string, kind?: "info" | "error") => void;
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
  onFocusPane: (paneId: string) => void;
  interceptPaneInput: (paneId: string, data: string | ArrayBuffer) => boolean;
  registerPaneSend: (
    paneId: string,
    fn: ((data: string | ArrayBuffer) => void) | null,
  ) => void;
  registerPaneActions: (paneId: string, actions: TerminalActions | null) => void;
  onNotify?: (message: string, kind?: "info" | "error") => void;
}

const LayoutNodeView: Component<LayoutNodeProps> = (props) => (
  <Show when={props.node} keyed>
    {(node) =>
      node.type === "pane" ? (
        (() => {
          const paneId = node.id;
        return (
          <div
            class={`terminal-pane ${
              props.activePaneId === paneId ? "active" : ""
            }`}
            onPointerDown={() => props.onFocusPane(paneId)}
          >
            <Terminal
              interceptInput={(data) => props.interceptPaneInput(paneId, data)}
              sessionId={node.sessionId}
              registerSend={(fn) => props.registerPaneSend(paneId, fn)}
              registerActions={(actions) =>
                props.registerPaneActions(paneId, actions)
              }
              onNotify={props.onNotify}
            />
          </div>
        );
        })()
      ) : (
        <div class={`terminal-split ${node.direction}`}>
          <For each={node.children}>
            {(child) => (
              <LayoutNodeView
                node={child}
                activePaneId={props.activePaneId}
                onFocusPane={props.onFocusPane}
                interceptPaneInput={props.interceptPaneInput}
                registerPaneSend={props.registerPaneSend}
                registerPaneActions={props.registerPaneActions}
                onNotify={props.onNotify}
              />
            )}
          </For>
        </div>
      )
    }
  </Show>
);

const TerminalWorkspace: Component<Props> = (props) => {
  const initial = readSavedLayout(props.tabId, props.sessionId);
  const [root, setRoot] = createSignal<TerminalLayoutNode>(initial.root);
  const [activePaneId, setActivePaneId] = createSignal(initial.activePaneId);
  const [broadcast, setBroadcast] = createSignal(Boolean(initial.broadcast));
  // Maximise/solo: show only the active pane at full size, without closing the
  // others (#185). Only meaningful with a split; a single pane already fills.
  const [soloed, setSoloed] = createSignal(false);
  const [busy, setBusy] = createSignal<SplitDirection | "close" | null>(null);
  const [error, setError] = createSignal<string | null>(null);
  const [draft, setDraft] = createSignal("");
  let composerRef: HTMLTextAreaElement | undefined;
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
      const base = source?.name || "shell";
      const suffix = direction === "row" ? "right" : "down";
      const name = `${base}-${suffix}-${Date.now() % 1000}`;
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

  const closeActivePane = async () => {
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
      reportError("close pane failed", err);
    } finally {
      setBusy(null);
    }
  };

  return (
    <div class="terminal-workspace">
      <div class="terminal-workspace-toolbar">
        <span class={`activity-dot ${activityClass(activeSession())}`} />
        <span class="terminal-workspace-title">
          {activeSession()?.name ?? activePane()?.sessionId.slice(0, 8) ?? "terminal"}
        </span>
        <Show when={activeSession()?.cwd}>
          <span class="terminal-workspace-cwd">{activeSession()?.cwd}</span>
        </Show>
        <Show when={error()}>
          <span class="terminal-workspace-error">{error()}</span>
        </Show>
        <button
          type="button"
          onClick={() => changeTerminalFontSize(-1)}
          title="Decrease terminal font size (browser zoom still scales the whole app)"
          aria-label="Decrease terminal font size"
        >
          A−
        </button>
        <button
          type="button"
          onClick={() => changeTerminalFontSize(1)}
          title="Increase terminal font size (browser zoom still scales the whole app)"
          aria-label="Increase terminal font size"
        >
          A+
        </button>
        <button
          class={broadcastEnabled() ? "active" : ""}
          onClick={() => setBroadcast((value) => !value)}
          title="Send keyboard, paste, composer, and shortcut input to every pane in this workspace"
        >
          {broadcastEnabled() ? "Broadcast on" : "Broadcast off"}
        </button>
        <Show when={panes().length > 1}>
          <button
            class={soloEnabled() ? "active" : ""}
            onClick={() => setSoloed((value) => !value)}
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
          onClick={() => void splitActive("row")}
          disabled={busy() !== null}
          title="Split right"
        >
          Split right
        </button>
        <button
          onClick={() => void splitActive("column")}
          disabled={busy() !== null}
          title="Split down"
        >
          Split down
        </button>
        <button
          onClick={() => void closeActivePane()}
          disabled={!canCloseActivePane() || busy() !== null}
          title={
            activePane()?.sessionId === props.sessionId
              ? "Root pane stays with this tab"
              : "Kill and close active pane"
          }
        >
          Close pane
        </button>
      </div>
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
      <div class={`terminal-layout ${soloEnabled() ? "soloed" : ""}`}>
        <LayoutNodeView
          node={root()}
          activePaneId={activePaneId()}
          onFocusPane={setActivePaneId}
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
        />
      </div>
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
          type="button"
          onClick={() => sendDraft(false)}
          disabled={!draft()}
          title="Send text without Enter"
        >
          Insert
        </button>
        <button type="submit" title="Send text and Enter">
          Enter
        </button>
      </form>
    </div>
  );
};

export default TerminalWorkspace;
