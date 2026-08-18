import {
  Component,
  For,
  Show,
  createEffect,
  createResource,
  createSignal,
  onCleanup,
} from "solid-js";
import { useNavigate } from "@solidjs/router";
import { api, type FileSearchResult, type TreeNode } from "./api";
import { openEditorTab, openTerminalTab } from "./tabs";
import { createSession } from "./store";
import { getFileIcon, getFolderIcon } from "./fileIcons";

interface Props {
  onOpen?: () => void;
  onCreatePresetHere?: (path: string) => void;
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
  onCreatePresetHere?: (path: string) => void;
  onRenameMove: (node: TreeNode) => void;
  onDuplicate: (node: TreeNode) => void;
  onDelete: (node: TreeNode) => void;
  onUploadHere: (path: string) => void;
}

function joinPath(dir: string, name: string): string {
  const cleanDir = dir.replace(/^\/+|\/+$/g, "");
  const cleanName = name.replace(/^\/+/, "");
  return cleanDir ? `${cleanDir}/${cleanName}` : cleanName;
}

function duplicatePath(path: string): string {
  const idx = path.lastIndexOf("/");
  const dir = idx >= 0 ? path.slice(0, idx) : "";
  const base = idx >= 0 ? path.slice(idx + 1) : path;
  const dot = base.lastIndexOf(".");
  const name = dot > 0 ? base.slice(0, dot) : base;
  const ext = dot > 0 ? base.slice(dot) : "";
  return joinPath(dir, `${name}-copy${ext}`);
}

const TreeNodeView: Component<NodeProps> = (props) => {
  const [open, setOpen] = createSignal(false);
  const [actionsOpen, setActionsOpen] = createSignal(false);
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
        const tree = await api.tree(props.node.path, 0);
        setKids(tree);
      } finally {
        setLoading(false);
      }
    }
  };

  return (
    <div>
      <div class="tree-row" title={props.node.path}>
        <button
          type="button"
          class="tree-main"
          onClick={() => void toggle()}
          aria-label={`${props.node.is_dir ? (open() ? "Collapse" : "Expand") : "Open"} ${props.node.path}`}
          aria-expanded={props.node.is_dir ? open() : undefined}
        >
          <span class="tree-disclosure" aria-hidden="true">
            {props.node.is_dir ? (open() ? "▾" : "▸") : " "}
          </span>
          <span class="tree-icon" aria-hidden="true">
            {props.node.is_dir ? getFolderIcon(open()) : getFileIcon(props.node.path)}
          </span>
          <span class="tree-label">{props.node.name}</span>
        </button>
        <button
          type="button"
          class="tree-actions-toggle"
          aria-label={`Actions for ${props.node.path}`}
          aria-expanded={actionsOpen()}
          onClick={() => setActionsOpen((value) => !value)}
        >
          Actions
        </button>
      </div>
      <Show when={actionsOpen()}>
        <div class="tree-actions" aria-label={`Actions for ${props.node.path}`}>
          <Show when={props.node.is_dir}>
            <Show when={props.onCreatePresetHere}>
              <button type="button" onClick={() => props.onCreatePresetHere?.(props.node.path)}>Create preset</button>
            </Show>
            <button type="button" onClick={() => props.onUploadHere(props.node.path)}>Upload here</button>
            <button type="button" onClick={() => props.onOpenTerminalHere(props.node.path)}>Open terminal</button>
          </Show>
          <button type="button" onClick={() => props.onRenameMove(props.node)}>Rename / move</button>
          <button type="button" onClick={() => props.onDuplicate(props.node)}>Duplicate</button>
          <Show when={!props.node.is_dir}>
            <button type="button" onClick={() => void api.downloadFile(props.node.path)}>Download</button>
          </Show>
          <button type="button" class="danger" onClick={() => props.onDelete(props.node)}>Delete</button>
        </div>
      </Show>
      <Show when={open() && props.node.is_dir}>
        <div class="tree-children">
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
                onCreatePresetHere={props.onCreatePresetHere}
                onRenameMove={props.onRenameMove}
                onDuplicate={props.onDuplicate}
                onDelete={props.onDelete}
                onUploadHere={props.onUploadHere}
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
  const [searchQuery, setSearchQuery] = createSignal("");
  const [debouncedSearchQuery, setDebouncedSearchQuery] = createSignal("");
  const [uploadTarget, setUploadTarget] = createSignal<string>("");
  const [fileSearchResults] = createResource(
    debouncedSearchQuery,
    async (query): Promise<FileSearchResult[]> => {
      const trimmed = query.trim();
      if (!trimmed) return [];
      return await api.searchFiles(trimmed);
    },
  );
  const navigate = useNavigate();
  let uploadInputRef: HTMLInputElement | undefined;

  const reportError = (message: string) => {
    if (props.onError) props.onError(message);
    else console.error(message);
  };

  const refreshTree = () => {
    void refetch();
  };

  createEffect(() => {
    const query = searchQuery();
    const timer = window.setTimeout(() => setDebouncedSearchQuery(query), 160);
    onCleanup(() => window.clearTimeout(timer));
  });

  const openFile = (path: string) => {
    openEditorTab(path);
    navigate(`/e/${encodeURIComponent(path)}`);
    props.onOpen?.();
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
      refreshTree();
    } catch (e) {
      reportError(`new file failed: ${(e as Error).message}`);
    }
  };

  const newFolder = async () => {
    if (!props.promptPath) return;
    const raw = await props.promptPath(
      "New folder",
      "",
      "relative/path/to/folder",
    );
    const path = raw?.trim().replace(/^\/+/, "") ?? "";
    if (!path) return;
    try {
      await api.fileOp({ op: "mkdir", path, parents: true });
      refreshTree();
    } catch (e) {
      reportError(`mkdir failed: ${(e as Error).message}`);
    }
  };

  const openTerminalHere = async (path: string) => {
    const name = `sh ${path.split("/").pop() || "/"}`;
    try {
      const session = await createSession(name, undefined, path);
      openTerminalTab(session.id, session.name);
      navigate(`/t/${session.id}`);
      props.onOpen?.();
    } catch (e) {
      reportError(`open terminal here failed: ${(e as Error).message}`);
    }
  };

  const renameMoveNode = async (node: TreeNode) => {
    if (!props.promptPath) return;
    const target = await props.promptPath(
      `Rename or move ${node.name}`,
      node.path,
      "relative/path/to/new-name",
    );
    const nextPath = target?.trim().replace(/^\/+/, "") ?? "";
    if (!nextPath || nextPath === node.path) return;
    try {
      const result = await api.fileOp({
        op: "move",
        from: node.path,
        to: nextPath,
        create_parents: true,
      });
      refreshTree();
      if (!node.is_dir && result.path) {
        openFile(result.path);
      }
    } catch (e) {
      reportError(`move failed: ${(e as Error).message}`);
    }
  };

  const duplicateNode = async (node: TreeNode) => {
    if (!props.promptPath) return;
    const target = await props.promptPath(
      `Duplicate ${node.name}`,
      duplicatePath(node.path),
      "relative/path/to/copy",
    );
    const nextPath = target?.trim().replace(/^\/+/, "") ?? "";
    if (!nextPath) return;
    try {
      const result = await api.fileOp({
        op: "duplicate",
        from: node.path,
        to: nextPath,
        create_parents: true,
      });
      refreshTree();
      if (!node.is_dir && result.path) {
        openFile(result.path);
      }
    } catch (e) {
      reportError(`duplicate failed: ${(e as Error).message}`);
    }
  };

  const deleteNode = async (node: TreeNode) => {
    const ok = window.confirm(
      node.is_dir
        ? `Delete folder "${node.path}" and all contents?`
        : `Delete file "${node.path}"?`,
    );
    if (!ok) return;
    try {
      await api.fileOp({
        op: "delete",
        path: node.path,
        recursive: node.is_dir,
      });
      refreshTree();
    } catch (e) {
      reportError(`delete failed: ${(e as Error).message}`);
    }
  };

  const triggerUpload = (targetDir = "") => {
    setUploadTarget(targetDir);
    uploadInputRef?.click();
  };

  const uploadFiles = async (files: FileList | null) => {
    if (!files || files.length === 0) return;
    try {
      for (const file of Array.from(files)) {
        const buffer = await file.arrayBuffer();
        const bytes = new Uint8Array(buffer);
        let binary = "";
        for (const byte of bytes) binary += String.fromCharCode(byte);
        const b64 = btoa(binary);
        const dest = joinPath(uploadTarget(), file.name);
        await api.writeFileBase64(dest, b64, true);
      }
      refreshTree();
    } catch (e) {
      reportError(`upload failed: ${(e as Error).message}`);
    } finally {
      if (uploadInputRef) uploadInputRef.value = "";
      setUploadTarget("");
    }
  };
  const searchActive = () => searchQuery().trim().length > 0;

  return (
    <div class="file-tree">
      <input
        ref={uploadInputRef}
        type="file"
        multiple
        style={{ display: "none" }}
        onChange={(e) => void uploadFiles(e.currentTarget.files)}
      />
      <div class="file-tree-header">
        <input
          type="search"
          class="file-tree-search"
          placeholder="Search files..."
          value={searchQuery()}
          onInput={(e) => setSearchQuery(e.currentTarget.value)}
        />
      </div>
      <div class="places-section-header">
        <span>Files</span>
        <span class="places-section-header-actions">
          <button
            onClick={() => void newFile()}
            title="New file"
            aria-label="New file"
          >
            New file
          </button>
          <button
            onClick={() => void newFolder()}
            title="New folder"
            aria-label="New folder"
          >
            New folder
          </button>
          <button
            onClick={() => triggerUpload("")}
            title="Upload files"
            aria-label="Upload files"
          >
            Upload
          </button>
          <button
            onClick={refreshTree}
            title="Refresh"
            aria-label="Refresh files"
          >
            Refresh
          </button>
        </span>
      </div>
      <Show when={tree.error}>
        <div style={{ padding: "8px 10px", color: "#ff7b72", "font-size": "12px" }}>
          {String(tree.error)}
        </div>
      </Show>
      <div class="tree-scroll">
        <Show
          when={searchActive()}
          fallback={
            <For each={tree() ?? []}>
              {(node) => (
                <TreeNodeView
                  node={node}
                  onOpen={props.onOpen}
                  onOpenFile={openFile}
                  onOpenTerminalHere={openTerminalHere}
                  onCreatePresetHere={props.onCreatePresetHere}
                  onRenameMove={(entry) => void renameMoveNode(entry)}
                  onDuplicate={(entry) => void duplicateNode(entry)}
                  onDelete={(entry) => void deleteNode(entry)}
                  onUploadHere={triggerUpload}
                />
              )}
            </For>
          }
        >
          <Show
            when={!fileSearchResults.loading}
            fallback={<div class="tree-search-meta">Searching workspace…</div>}
          >
            <Show
              when={(fileSearchResults()?.length ?? 0) > 0}
              fallback={<div class="tree-search-meta">No matching files</div>}
            >
              <div class="tree-search-meta">
                {(fileSearchResults()?.length ?? 0).toString()} file match(es)
              </div>
              <For each={fileSearchResults() ?? []}>
                {(file) => (
                  <button
                    class="tree-search-result"
                    onClick={() => openFile(file.path)}
                    title={file.path}
                  >
                    <span class="tree-icon">{getFileIcon(file.path)}</span>
                    <span class="tree-search-main">
                      <span class="tree-search-name">{file.name}</span>
                      <span class="tree-search-path">{file.path}</span>
                    </span>
                  </button>
                )}
              </For>
            </Show>
          </Show>
        </Show>
      </div>
    </div>
  );
};

export default FileTree;
