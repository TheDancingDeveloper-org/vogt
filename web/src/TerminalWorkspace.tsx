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
} from "solid-js";
import Terminal, { type TerminalActions } from "./Terminal";
import Continuity from "./Continuity";
import type { ContinuationRecipe, SessionSummary } from "./api";
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

type SplitDirection = "row" | "column";

interface PaneNode {
  type: "pane";
  id: string;
  sessionId: string;
}

interface SplitNode {
  type: "split";
  id: string;
  direction: SplitDirection;
  children: TerminalLayoutNode[];
}

type TerminalLayoutNode = PaneNode | SplitNode;

interface SavedLayout {
  root: TerminalLayoutNode;
  activePaneId: string;
  broadcast?: boolean;
}

interface Props {
  tabId: string;
  sessionId: string;
  registerSend?: (fn: ((data: string | ArrayBuffer) => void) | null) => void;
  registerActions?: (actions: TerminalActions | null) => void;
  confirmClosePane?: (session: SessionSummary | null) => Promise<boolean>;
  onError?: (message: string) => void;
  onNotify?: (message: string, kind?: "info" | "error") => void;
  /** Focus an existing terminal by id (continuity reattach and post-recovery). */
  onFocusSession?: (sessionId: string) => void;
}

const STORAGE_KEY = "mydevenv2.terminalLayouts.v1";

function paneIdFor(sessionId: string): string {
  return `pane:${sessionId}`;
}

function makePane(sessionId: string): PaneNode {
  return { type: "pane", id: paneIdFor(sessionId), sessionId };
}

function newSplitId(): string {
  if (typeof crypto !== "undefined" && "randomUUID" in crypto) {
    return `split:${crypto.randomUUID()}`;
  }
  return `split:${Date.now()}:${Math.random().toString(36).slice(2)}`;
}

function normalizeNode(value: unknown): TerminalLayoutNode | null {
  if (!value || typeof value !== "object") return null;
  const obj = value as Record<string, unknown>;
  if (
    obj.type === "pane" &&
    typeof obj.id === "string" &&
    typeof obj.sessionId === "string"
  ) {
    return { type: "pane", id: obj.id, sessionId: obj.sessionId };
  }
  if (
    obj.type === "split" &&
    typeof obj.id === "string" &&
    (obj.direction === "row" || obj.direction === "column") &&
    Array.isArray(obj.children)
  ) {
    const children: TerminalLayoutNode[] = [];
    for (const child of obj.children) {
      const normalized = normalizeNode(child);
      if (normalized) children.push(normalized);
    }
    if (children.length === 0) return null;
    if (children.length === 1) return children[0] ?? null;
    return {
      type: "split",
      id: obj.id,
      direction: obj.direction,
      children,
    };
  }
  return null;
}

function readSavedLayouts(): Record<string, SavedLayout> {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    return raw ? (JSON.parse(raw) as Record<string, SavedLayout>) : {};
  } catch {
    return {};
  }
}

function readSavedLayout(tabId: string, sessionId: string): SavedLayout {
  const fallback = {
    root: makePane(sessionId),
    activePaneId: paneIdFor(sessionId),
    broadcast: false,
  };
  const saved = readSavedLayouts()[tabId];
  const root = normalizeNode(saved?.root);
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

function writeSavedLayout(tabId: string, layout: SavedLayout) {
  try {
    const all = readSavedLayouts();
    all[tabId] = layout;
    localStorage.setItem(STORAGE_KEY, JSON.stringify(all));
  } catch {
    /* quota / private mode */
  }
}

function containsSession(node: TerminalLayoutNode, sessionId: string): boolean {
  if (node.type === "pane") return node.sessionId === sessionId;
  return node.children.some((child) => containsSession(child, sessionId));
}

function findPane(node: TerminalLayoutNode, paneId: string): PaneNode | null {
  if (node.type === "pane") return node.id === paneId ? node : null;
  for (const child of node.children) {
    const found = findPane(child, paneId);
    if (found) return found;
  }
  return null;
}

function firstPane(node: TerminalLayoutNode): PaneNode | null {
  if (node.type === "pane") return node;
  for (const child of node.children) {
    const found = firstPane(child);
    if (found) return found;
  }
  return null;
}

function collectPanes(node: TerminalLayoutNode): PaneNode[] {
  if (node.type === "pane") return [node];
  return node.children.flatMap((child) => collectPanes(child));
}

function insertPane(
  node: TerminalLayoutNode,
  targetPaneId: string,
  direction: SplitDirection,
  nextPane: PaneNode,
): TerminalLayoutNode {
  if (node.type === "pane") {
    if (node.id !== targetPaneId) return node;
    return {
      type: "split",
      id: newSplitId(),
      direction,
      children: [node, nextPane],
    };
  }
  let changed = false;
  const children = node.children.map((child) => {
    const next = insertPane(child, targetPaneId, direction, nextPane);
    if (next !== child) changed = true;
    return next;
  });
  return changed ? { ...node, children } : node;
}

function removePane(
  node: TerminalLayoutNode,
  targetPaneId: string,
): TerminalLayoutNode | null {
  if (node.type === "pane") return node.id === targetPaneId ? null : node;
  let changed = false;
  const children: TerminalLayoutNode[] = [];
  for (const child of node.children) {
    const next = removePane(child, targetPaneId);
    if (next !== child) changed = true;
    if (next) children.push(next);
  }
  if (children.length === 0) return null;
  if (children.length === 1) return children[0] ?? null;
  return changed ? { ...node, children } : node;
}

function pruneMissingSessions(node: TerminalLayoutNode): TerminalLayoutNode | null {
  if (node.type === "pane") {
    return sessionsStore.sessions[node.sessionId] ? node : null;
  }
  let changed = false;
  const children: TerminalLayoutNode[] = [];
  for (const child of node.children) {
    const next = pruneMissingSessions(child);
    if (next) children.push(next);
    if (next !== child) changed = true;
  }
  if (children.length === 0) return null;
  if (children.length === 1) return children[0] ?? null;
  return changed ? { ...node, children } : node;
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
  <Switch>
    <Match when={props.node.type === "pane" ? props.node : null}>
      {(pane) => {
        const paneId = props.node.id;
        return (
          <div
            class={`terminal-pane ${
              props.activePaneId === paneId ? "active" : ""
            }`}
            onPointerDown={() => props.onFocusPane(paneId)}
          >
            <Terminal
              interceptInput={(data) => props.interceptPaneInput(paneId, data)}
              sessionId={pane().sessionId}
              registerSend={(fn) => props.registerPaneSend(paneId, fn)}
              registerActions={(actions) =>
                props.registerPaneActions(paneId, actions)
              }
              onNotify={props.onNotify}
            />
          </div>
        );
      }}
    </Match>
    <Match when={props.node.type === "split" ? props.node : null}>
      {(split) => (
        <div class={`terminal-split ${split().direction}`}>
          <For each={split().children}>
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
      )}
    </Match>
  </Switch>
);

const TerminalWorkspace: Component<Props> = (props) => {
  const initial = readSavedLayout(props.tabId, props.sessionId);
  const [root, setRoot] = createSignal<TerminalLayoutNode>(initial.root);
  const [activePaneId, setActivePaneId] = createSignal(initial.activePaneId);
  const [broadcast, setBroadcast] = createSignal(Boolean(initial.broadcast));
  const [busy, setBusy] = createSignal<SplitDirection | "close" | null>(null);
  const [error, setError] = createSignal<string | null>(null);
  const [continuityShown, setContinuityShown] = createSignal(false);
  const [draft, setDraft] = createSignal("");
  let composerRef: HTMLTextAreaElement | undefined;
  const paneSenders = new Map<string, (data: string | ArrayBuffer) => void>();
  const paneActions = new Map<string, TerminalActions>();

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
    const pruned = pruneMissingSessions(current);
    if (!pruned) return;
    if (pruned !== current) setRoot(pruned);
    if (!findPane(pruned, activePaneId())) {
      const first = firstPane(pruned);
      if (first) setActivePaneId(first.id);
    }
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
      setRoot((current) =>
        insertPane(current, pane.id, direction, nextPane),
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

  const continuityState = () => activeSession()?.continuity ?? null;

  /**
   * Create a terminal from a continuation recipe.
   *
   * The recipe's command, cwd, and env are used verbatim: ContextKeeper mints
   * the correlation identifiers that travel in `env`, and rewriting any of it
   * here would break the binding between the new PTY and the work it
   * continues.
   */
  const launchRecipe = async (recipe: ContinuationRecipe) => {
    const spec = recipe.mydevenv2;
    if (!spec) throw new Error(`the ${recipe.kind} rung starts nothing`);
    const session = await createSession(
      spec.name,
      spec.command,
      spec.cwd,
      spec.env,
    );
    props.onFocusSession?.(session.id);
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
          class={broadcastEnabled() ? "active" : ""}
          onClick={() => setBroadcast((value) => !value)}
          title="Send keyboard, paste, composer, and shortcut input to every pane in this workspace"
        >
          {broadcastEnabled() ? "Broadcast on" : "Broadcast off"}
        </button>
        <button
          class={continuityShown() ? "active" : ""}
          onClick={() => setContinuityShown((shown) => !shown)}
          title={
            continuityState()
              ? "ContextKeeper: protection state and recovery for this session"
              : "ContextKeeper: this terminal has no captured agent session bound to it"
          }
        >
          {continuityState()?.state === "recovering" ? "Recovery ◆" : "Continuity"}
        </button>
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
      <Show when={continuityShown() && activeSession()}>
        {(session) => (
          <Continuity
            session={session()}
            onClose={() => setContinuityShown(false)}
            onLaunchRecipe={launchRecipe}
            onFocusTerminal={(id) => props.onFocusSession?.(id)}
            onNotify={props.onNotify}
          />
        )}
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
      <div class="terminal-layout">
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
