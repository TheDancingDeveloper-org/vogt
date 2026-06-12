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
  promptPath?: (
    title: string,
    defaultValue?: string,
    placeholder?: string,
  ) => Promise<string | null>;
  onError?: (message: string) => void;
}

interface NodeProps {
  node: TreeNode;
  onOpen?: () => void;
  onOpenFile: (path: string) => void;
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
      props.onOpenFile(props.node.path);
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
        <Show when={!props.node.is_dir}>
          <button
            class="tree-term-btn"
            title="Download"
            onClick={(e) => {
              e.stopPropagation();
              api.downloadFile(props.node.path).catch((err) =>
                console.error("download failed", err),
              );
            }}
          >
            ⬇
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
                onOpenFile={props.onOpenFile}
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

  const openFile = (path: string) => {
    openEditorTab(path);
    navigate(`/e/${encodeURIComponent(path)}`);
    props.onOpen?.();
  };

  const reportError = (message: string) => {
    if (props.onError) props.onError(message);
    else console.error(message);
  };

  const newFile = async () => {
    if (!props.promptPath) return;
    const raw = await props.promptPath(
      "New file",
      "",
      "relative/path/to/file.txt",
    );
    const path = raw?.trim().replace(/^\/+/, "") ?? "";
    if (!path) return;
    try {
      await api.writeFile(path, "", true);
      openFile(path);
      void refetch();
    } catch (e) {
      reportError(`new file failed: ${(e as Error).message}`);
    }
  };

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
      reportError(`open terminal here failed: ${(e as Error).message}`);
    }
  };

  return (
    <div class="file-tree">
      <div class="drawer-header">
        <span>Files</span>
        <span class="drawer-header-actions">
          <button
            style={{ "font-size": "11px", padding: "2px 6px" }}
            onClick={() => void newFile()}
            title="New file"
          >
            +
          </button>
          <button
            style={{ "font-size": "11px", padding: "2px 6px" }}
            onClick={() => refetch()}
            title="Refresh"
          >
            ⟳
          </button>
        </span>
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
              onOpenFile={openFile}
              onOpenTerminalHere={openTerminalHere}
            />
          )}
        </For>
      </div>
    </div>
  );
};

export default FileTree;
