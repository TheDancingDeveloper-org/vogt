import {
  Component,
  For,
  Show,
  createEffect,
  createMemo,
  createResource,
  createSignal,
  createUniqueId,
  onCleanup,
} from "solid-js";
import { useNavigate } from "@solidjs/router";
import {
  api,
  type FileSearchResult,
  type GitStatusEntry,
  type GitStatusKind,
  type TreeNode,
} from "./api";
import { openEditorTab, openTerminalTab } from "./tabs";
import { createSession } from "./store";
import { railSections, setRailSection } from "./railSections";
import {
  expandedPaths,
  fileTreeSearch,
  folderVersion,
  invalidateFolder,
  isExpanded,
  parentFolder,
  setExpanded,
  setFileTreeSearch,
} from "./fileTreeState";
import { workspaceVersion } from "./workspaceVersion";
import { onWake } from "./wakeCoordinator";

interface Props {
  /** Whether this mounted tree belongs to the currently visible surface. */
  active?: () => boolean;
  onOpen?: () => void;
  onCreatePresetHere?: (path: string) => void;
  promptPath?: (
    title: string,
    defaultValue?: string,
    placeholder?: string,
  ) => Promise<string | null>;
  confirmAction?: (title: string, body?: string) => Promise<boolean>;
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
  statusEntries: ReadonlyMap<string, FileStatus>;
}

// Only one node's actions picker is open at a time. The picker behaves like a
// menu, and two menus open at once in a recursive tree is two menus to dismiss.
// A module-level signal is the lightest coordination that survives the
// recursion: each node keeps its own `actionsOpen`, and this names which node
// currently owns an open picker so the others can close themselves.
const [openPickerNode, setOpenPickerNode] = createSignal<string | null>(null);

const FILE_STATUS: Record<GitStatusKind, { marker: string; label: string }> = {
  modified: { marker: "M", label: "Modified" },
  staged: { marker: "S", label: "Staged" },
  untracked: { marker: "?", label: "Untracked" },
  conflicted: { marker: "!", label: "Conflicted" },
  renamed: { marker: "R", label: "Renamed" },
  deleted: { marker: "D", label: "Deleted" },
};

export interface FileStatus {
  marker: string;
  label: string;
  kind: GitStatusKind;
}

/** Build once per Git response; each rendered node then does one map lookup. */
export function buildStatusMap(entries: GitStatusEntry[]): ReadonlyMap<string, FileStatus> {
  const result = new Map<string, FileStatus>();
  for (const entry of entries) {
    result.set(entry.path, { ...FILE_STATUS[entry.kind], kind: entry.kind });
  }
  return result;
}

export function statusForPath(
  entries: ReadonlyMap<string, FileStatus>,
  path: string,
): FileStatus | null {
  return entries.get(path) ?? null;
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
  // Expansion lives in a module store keyed by path, so an expanded folder
  // survives a tab switch and a workspace remount instead of collapsing (#238).
  const open = () => isExpanded(props.node.path);
  const [actionsOpen, setActionsOpen] = createSignal(false);
  const [kids, setKids] = createSignal<TreeNode[] | null>(
    props.node.children ?? null,
  );
  const [loading, setLoading] = createSignal(false);
  const status = () => statusForPath(props.statusEntries, props.node.path);

  const [loadError, setLoadError] = createSignal<string | null>(null);

  const loadKids = async () => {
    setLoading(true);
    setLoadError(null);
    try {
      const tree = await api.tree(props.node.path, 0);
      setKids(tree);
    } catch (cause) {
      // A failed expand must not become an unhandled rejection (the effect
      // calls this with `void`): report it in place and leave the folder open
      // with a notice rather than throwing out of the render tree (#247).
      setLoadError(`Could not open this folder: ${(cause as Error).message}`);
      setKids([]);
    } finally {
      setLoading(false);
    }
  };

  // Load (and reload) an expanded folder's children. Tracks this folder's own
  // invalidation counter, so a file op that touches this folder refetches
  // exactly it — the rest of the tree keeps its loaded, expanded state (#238).
  createEffect(() => {
    if (!props.node.is_dir || !open()) return;
    folderVersion(props.node.path);
    void loadKids();
  });
  let rowRef: HTMLDivElement | undefined;
  let actionsRef: HTMLDivElement | undefined;

  const openActions = () => {
    // Claim ownership before opening: were these swapped, the coordination
    // effect could observe `actionsOpen` true while the owner is still another
    // node and close this picker in the same tick it opened.
    setOpenPickerNode(props.node.path);
    setActionsOpen(true);
  };

  const closeActions = () => {
    setActionsOpen(false);
    if (openPickerNode() === props.node.path) setOpenPickerNode(null);
  };

  // Another node opened its picker: close ours. This is the single-open-at-a-time
  // rule, expressed once for the whole tree.
  createEffect(() => {
    if (actionsOpen() && openPickerNode() !== props.node.path) setActionsOpen(false);
  });

  // While the picker is open it dismisses like a menu: a click anywhere outside
  // its row, or Escape, closes it. The listeners exist only while it is open,
  // and `onCleanup` here fires when the effect re-runs (the picker closed) or
  // the node unmounts.
  createEffect(() => {
    if (!actionsOpen()) return;
    const onDocClick = (event: MouseEvent) => {
      const target = event.target as Node | null;
      if (target && (rowRef?.contains(target) || actionsRef?.contains(target))) {
        return;
      }
      closeActions();
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") closeActions();
    };
    document.addEventListener("click", onDocClick);
    document.addEventListener("keydown", onKeyDown);
    onCleanup(() => {
      document.removeEventListener("click", onDocClick);
      document.removeEventListener("keydown", onKeyDown);
    });
  });

  const toggle = () => {
    if (!props.node.is_dir) {
      props.onOpenFile(props.node.path);
      return;
    }
    const next = !open();
    setExpanded(props.node.path, next);
    // Collapsing a folder must take its picker with it: an actions menu left
    // open over a now-hidden folder is orphaned on screen (#186). The children
    // are (re)fetched by the expansion effect above, not here.
    if (!next) closeActions();
  };

  return (
    <div>
      <div class="tree-row" title={props.node.path} ref={rowRef}>
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
          <span class="tree-label">{props.node.name}</span>
          <Show when={status()}>
            {(value) => (
              <span
                class={`tree-status tree-status-${value().kind}`}
                aria-label={`${value().label} file`}
                title={value().label}
              >
                {value().marker}
              </span>
            )}
          </Show>
        </button>
        <button
          type="button"
          class="tree-actions-toggle"
          aria-label={`Actions for ${props.node.path}`}
          aria-expanded={actionsOpen()}
          onClick={() => (actionsOpen() ? closeActions() : openActions())}
        >
          <span aria-hidden="true">⋯</span>
        </button>
      </div>
      <Show when={actionsOpen()}>
        <div class="tree-actions" aria-label={`Actions for ${props.node.path}`} ref={actionsRef}>
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
          <Show when={loadError()}>
            <div class="meta" role="alert" style={{ padding: "2px 8px", color: "var(--activity-errored)" }}>
              {loadError()}
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
                statusEntries={props.statusEntries}
              />
            )}
          </For>
        </div>
      </Show>
    </div>
  );
};

const FileTree: Component<Props> = (props) => {
  const isActive = () => props.active?.() ?? true;
  const [tree, { refetch }] = createResource(
    () => (isActive() ? "active" : undefined),
    () => api.tree("", 0),
  );
  const [gitStatus, { refetch: refetchGitStatus }] = createResource(
    () => (isActive() ? "active" : undefined),
    async () => {
      try {
        const status = await api.gitStatus("");
        return status.is_repo === false ? [] : status.entries;
      } catch {
        // File browsing is independent of Git. A non-repository workspace or a
        // local Git read failure removes optional markers, never the file tree.
        return [];
      }
    },
  );
  const statusMap = createMemo(() => buildStatusMap(gitStatus() ?? []));
  // Search query is hoisted to the module store so it survives a tab switch.
  const searchQuery = fileTreeSearch;
  const setSearchQuery = setFileTreeSearch;
  const [debouncedSearchQuery, setDebouncedSearchQuery] = createSignal(fileTreeSearch());
  const [uploadTarget, setUploadTarget] = createSignal<string>("");
  const [moreActionsOpen, setMoreActionsOpen] = createSignal(false);
  const moreActionsId = `file-tree-more-actions-${createUniqueId()}`;
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

  // A full manual refresh (the ↻ button): the root, Git markers, and every
  // currently-expanded folder's children.
  const refreshTree = () => {
    void refetch();
    void refetchGitStatus();
    for (const path of expandedPaths()) invalidateFolder(path);
  };

  // Refetch after a file op affecting `folder`: only that parent, so the rest
  // of the tree keeps its expanded state (#238). The root level is served by
  // the `tree` resource, not a per-folder node, so it refetches directly.
  const refreshFolder = (folder: string) => {
    if (folder === "") void refetch();
    else invalidateFolder(folder);
    void refetchGitStatus();
  };

  // A save, a Git op or a file created elsewhere bumps the workspace version;
  // reflect it in the root and the Git markers without a manual refresh. The
  // first run is the initial mount, already covered by the resources' own fetch.
  let firstVersion = true;
  createEffect(() => {
    workspaceVersion();
    if (!isActive()) return;
    if (firstVersion) {
      firstVersion = false;
      return;
    }
    void refetch();
    void refetchGitStatus();
  });

  // Coming back to the tab is a moment the tree may be stale (an agent wrote
  // files, a terminal ran git). The shared wake coordinator combines focus
  // and visibility into one debounced wake (#410).
  const revalidate = () => {
    void refetch();
    void refetchGitStatus();
  };
  onCleanup(onWake(() => {
    if (isActive()) revalidate();
  }));

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
      refreshFolder(parentFolder(path));
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
      refreshFolder(parentFolder(path));
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
      // Both ends move: the source folder loses the entry, the destination
      // folder gains it.
      refreshFolder(parentFolder(node.path));
      refreshFolder(parentFolder(nextPath));
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
      refreshFolder(parentFolder(nextPath));
      if (!node.is_dir && result.path) {
        openFile(result.path);
      }
    } catch (e) {
      reportError(`duplicate failed: ${(e as Error).message}`);
    }
  };

  const deleteNode = async (node: TreeNode) => {
    if (!props.confirmAction) return;
    const ok = await props.confirmAction(
      node.is_dir ? `Delete folder "${node.path}"?` : `Delete file "${node.path}"?`,
      node.is_dir
        ? "The folder and every file beneath it will be permanently deleted."
        : "The file will be permanently deleted.",
    );
    if (!ok) return;
    try {
      await api.fileOp({
        op: "delete",
        path: node.path,
        recursive: node.is_dir,
      });
      refreshFolder(parentFolder(node.path));
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
    const targetDir = uploadTarget();
    try {
      for (const file of Array.from(files)) {
        const buffer = await file.arrayBuffer();
        const bytes = new Uint8Array(buffer);
        let binary = "";
        for (const byte of bytes) binary += String.fromCharCode(byte);
        const b64 = btoa(binary);
        const dest = joinPath(targetDir, file.name);
        await api.writeFileBase64(dest, b64, true);
      }
      refreshFolder(targetDir);
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
        <h2 class="places-section-header" aria-label="Files">
          <button
            type="button"
            class="places-section-toggle"
            aria-expanded={railSections.files}
            onClick={() => setRailSection("files", !railSections.files)}
          >
            <span class="places-section-caret" aria-hidden="true">{railSections.files ? "▾" : "▸"}</span>
            <span>Files</span>
          </button>
          <span class="places-section-header-actions">
            <button
              type="button"
              class="file-tree-primary-action"
              onClick={() => void newFile()}
              title="New file"
              aria-label="New file"
            >
              <span aria-hidden="true">+</span>
            </button>
            <button
              type="button"
              class="file-tree-primary-action"
              onClick={refreshTree}
              title="Refresh files"
              aria-label="Refresh files"
            >
              <span aria-hidden="true">↻</span>
            </button>
            <button
              type="button"
              class="file-tree-more-toggle"
              onClick={() => setMoreActionsOpen((open) => !open)}
              title="More file actions"
              aria-label="More file actions"
              aria-expanded={moreActionsOpen()}
              aria-controls={moreActionsId}
            >
              <span aria-hidden="true">⋯</span>
            </button>
          </span>
        </h2>
        <Show when={moreActionsOpen()}>
          <div
            id={moreActionsId}
            class="file-tree-more-actions"
            aria-label="More file actions"
          >
            <button type="button" onClick={() => { setMoreActionsOpen(false); void newFolder(); }}>
              New folder
            </button>
            <button type="button" onClick={() => { setMoreActionsOpen(false); triggerUpload(""); }}>
              Upload files
            </button>
          </div>
        </Show>
        <input
          type="search"
          class="file-tree-search"
          aria-label="Search files"
          placeholder="Search files…"
          value={searchQuery()}
          onInput={(e) => setSearchQuery(e.currentTarget.value)}
          hidden={!railSections.files}
        />
      </div>
      <Show when={tree.error}>
        <div style={{ padding: "8px 10px", color: "var(--danger)", "font-size": "12px" }}>
          {String(tree.error)}
        </div>
      </Show>
      <div class="tree-scroll" hidden={!railSections.files}>
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
                  statusEntries={statusMap()}
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
                    <span class="tree-search-main">
                      <span class="tree-search-name">{file.name}</span>
                      <span class="tree-search-path">{file.path}</span>
                    </span>
                    <Show when={statusForPath(statusMap(), file.path)}>
                      {(value) => (
                        <span
                          class={`tree-status tree-status-${value().kind}`}
                          aria-label={`${value().label} file`}
                          title={value().label}
                        >
                          {value().marker}
                        </span>
                      )}
                    </Show>
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
