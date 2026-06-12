import {
  Component,
  For,
  Match,
  Show,
  Switch,
  createEffect,
  createMemo,
  createSignal,
} from "solid-js";
import Terminal, { type TerminalActions } from "./Terminal";
import type { SessionSummary } from "./api";
import {
  createSession,
  deleteSession,
  killSession,
  sessionsStore,
} from "./store";

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
}

interface Props {
  tabId: string;
  sessionId: string;
  registerSend?: (fn: (data: string | ArrayBuffer) => void) => void;
  registerActions?: (actions: TerminalActions) => void;
  confirmClosePane?: (session: SessionSummary | null) => Promise<boolean>;
  onError?: (message: string) => void;
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
  return {
    ...node,
    children: node.children.map((child) =>
      insertPane(child, targetPaneId, direction, nextPane),
    ),
  };
}

function removePane(
  node: TerminalLayoutNode,
  targetPaneId: string,
): TerminalLayoutNode | null {
  if (node.type === "pane") return node.id === targetPaneId ? null : node;
  const children = node.children
    .map((child) => removePane(child, targetPaneId))
    .filter((child): child is TerminalLayoutNode => Boolean(child));
  if (children.length === 0) return null;
  if (children.length === 1) return children[0] ?? null;
  return { ...node, children };
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
  registerPaneSend: (
    paneId: string,
    fn: (data: string | ArrayBuffer) => void,
  ) => void;
  registerPaneActions: (paneId: string, actions: TerminalActions) => void;
}

const LayoutNodeView: Component<LayoutNodeProps> = (props) => (
  <Switch>
    <Match when={props.node.type === "pane" ? props.node : null}>
      {(pane) => (
        <div
          class={`terminal-pane ${
            props.activePaneId === pane().id ? "active" : ""
          }`}
          onPointerDown={() => props.onFocusPane(pane().id)}
        >
          <Terminal
            sessionId={pane().sessionId}
            registerSend={(fn) => props.registerPaneSend(pane().id, fn)}
            registerActions={(actions) =>
              props.registerPaneActions(pane().id, actions)
            }
          />
        </div>
      )}
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
                registerPaneSend={props.registerPaneSend}
                registerPaneActions={props.registerPaneActions}
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
  const [busy, setBusy] = createSignal<SplitDirection | "close" | null>(null);
  const [error, setError] = createSignal<string | null>(null);
  const paneSenders = new Map<string, (data: string | ArrayBuffer) => void>();
  const paneActions = new Map<string, TerminalActions>();

  const panes = createMemo(() => collectPanes(root()));
  const activePane = createMemo(
    () => findPane(root(), activePaneId()) ?? firstPane(root()),
  );
  const activeSession = createMemo(() => {
    const pane = activePane();
    return pane ? sessionsStore.sessions[pane.sessionId] : undefined;
  });
  const canCloseActivePane = createMemo(() => {
    const pane = activePane();
    return Boolean(
      pane && panes().length > 1 && pane.sessionId !== props.sessionId,
    );
  });

  const sendToActive = (data: string | ArrayBuffer) => {
    paneSenders.get(activePaneId())?.(data);
  };

  const workspaceActions: TerminalActions = {
    copy: async () => {
      await paneActions.get(activePaneId())?.copy();
    },
    paste: async () => {
      await paneActions.get(activePaneId())?.paste();
    },
    selectAll: () => {
      paneActions.get(activePaneId())?.selectAll();
    },
  };

  createEffect(() => {
    props.registerSend?.(sendToActive);
    props.registerActions?.(workspaceActions);
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
      const session = await createSession(
        `${base}-${suffix}-${Date.now() % 1000}`,
        undefined,
        source?.cwd || undefined,
      );
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
      <div class="terminal-layout">
        <LayoutNodeView
          node={root()}
          activePaneId={activePaneId()}
          onFocusPane={setActivePaneId}
          registerPaneSend={(paneId, fn) => paneSenders.set(paneId, fn)}
          registerPaneActions={(paneId, actions) =>
            paneActions.set(paneId, actions)
          }
        />
      </div>
    </div>
  );
};

export default TerminalWorkspace;
