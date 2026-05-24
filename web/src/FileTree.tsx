import {
  Component,
  For,
  Show,
  createResource,
  createSignal,
} from "solid-js";
import { useNavigate } from "@solidjs/router";
import { api, type TreeNode } from "./api";
import { openEditorTab, openTerminalTab } from "./tabs";
import { createSession } from "./store";

interface Props {
  /** Called after opening an editor tab so the drawer can auto-close on mobile. */
  onOpen?: () => void;
}

interface NodeProps {
  node: TreeNode;
  onOpen?: () => void;
  onOpenTerminalHere: (path: string) => void;
}

const TreeNodeView: Component<NodeProps> = (props) => {
  const [open, setOpen] = createSignal(false);
  const [kids, setKids] = createSignal<TreeNode[] | null>(
    props.node.children ?? null,
  );
  const [loading, setLoading] = createSignal(false);

  const toggle = async () => {
    if (!props.node.is_dir) {
      // File: open in editor
      openEditorTab(props.node.path);
      props.onOpen?.();
      return;
    }
    const next = !open();
    setOpen(next);
    if (next && (kids() === null || kids()?.length === 0)) {
      setLoading(true);
      try {
        const t = await api.tree(props.node.path, 0);
        setKids(t);
      } finally {
        setLoading(false);
      }
    }
  };

  return (
    <div>
      <div
        class="tree-row"
        style={{ cursor: "pointer" }}
        onClick={toggle}
        title={props.node.path}
      >
        <span style={{ width: "14px", display: "inline-block", "text-align": "center" }}>
          {props.node.is_dir ? (open() ? "▾" : "▸") : " "}
        </span>
        <span style={{ "margin-left": "2px", flex: 1, "min-width": 0, overflow: "hidden", "text-overflow": "ellipsis" }}>
          {props.node.is_dir ? "📁" : "📄"} {props.node.name}
        </span>
        <Show when={props.node.is_dir}>
          <button
            class="tree-term-btn"
            title="Open terminal here"
            onClick={(e) => {
              e.stopPropagation();
              props.onOpenTerminalHere(props.node.path);
            }}
          >
            &gt;_
          </button>
        </Show>
      </div>
      <Show when={open() && props.node.is_dir}>
        <div style={{ "padding-left": "14px" }}>
          <Show when={loading()}>
            <div class="meta" style={{ padding: "2px 8px", color: "var(--fg-muted)" }}>
              loading…
            </div>
          </Show>
          <For each={kids() ?? []}>
            {(child) => (
              <TreeNodeView
                node={child}
                onOpen={props.onOpen}
                onOpenTerminalHere={props.onOpenTerminalHere}
              />
            )}
          </For>
        </div>
      </Show>
    </div>
  );
};

const FileTree: Component<Props> = (props) => {
  const [tree, { refetch }] = createResource(() => api.tree("", 0));
  const navigate = useNavigate();

  const openTerminalHere = async (path: string) => {
    // Server resolves a relative path against workspace_root, so we pass the
    // path verbatim. "" → workspace_root (default cwd).
    const name = `sh ${path.split("/").pop() || "/"}`;
    try {
      const s = await createSession(name, undefined, path);
      openTerminalTab(s.id, s.name);
      navigate(`/t/${s.id}`);
      props.onOpen?.();
    } catch (e) {
      console.error("open terminal here failed", e);
      alert(`open terminal here failed: ${(e as Error).message}`);
    }
  };

  return (
    <div class="file-tree">
      <div class="drawer-header">
        <span>Files</span>
        <button
          style={{ "font-size": "11px", padding: "2px 6px" }}
          onClick={() => refetch()}
          title="Refresh"
        >
          ⟳
        </button>
      </div>
      <Show when={tree.error}>
        <div style={{ padding: "8px 10px", color: "#ff7b72", "font-size": "12px" }}>
          {String(tree.error)}
        </div>
      </Show>
      <div class="tree-scroll">
        <For each={tree() ?? []}>
          {(n) => (
            <TreeNodeView
              node={n}
              onOpen={props.onOpen}
              onOpenTerminalHere={openTerminalHere}
            />
          )}
        </For>
      </div>
    </div>
  );
};

export default FileTree;
