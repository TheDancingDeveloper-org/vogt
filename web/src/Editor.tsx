import { Component, createSignal, onCleanup, onMount, Show, For } from "solid-js";
import { api, ApiError } from "./api";
import {
  languageFor,
  loadMonaco,
  type StandaloneEditor,
  type TextModel,
} from "./monaco";
import { setEditorDirty, tabsStore } from "./tabs";
import { addRecentFile } from "./recentFiles";
import { getMinimapEnabled, setMinimapEnabled } from "./editorPrefs";
import { registerEditor } from "./editorRegistry";
import {
  discardEditorDraft,
  readEditorDraft,
  rememberEditorDraft,
} from "./editorDrafts";
import { bumpWorkspaceVersion } from "./workspaceVersion";
import { revealPathInGit } from "./gitReveal";

interface Props {
  tabId: string;
  path: string;
}

const Editor: Component<Props> = (props) => {
  let host: HTMLDivElement | undefined;
  let editor: StandaloneEditor | null = null;
  let model: TextModel | null = null;
  const [status, setStatus] = createSignal<"loading" | "ready" | "saving" | "error">(
    "loading",
  );
  const [error, setError] = createSignal<string | null>(null);
  const [savedAt, setSavedAt] = createSignal<number | null>(null);
  const [minimapOn, setMinimapOn] = createSignal(getMinimapEnabled());
  // A save was refused because the file changed on disk since we read it. The
  // reader chooses: Overwrite (force past the guard) or Reload (take disk).
  const [conflict, setConflict] = createSignal(false);
  // A restored draft did not match the file on disk when this tab remounted —
  // the reader is editing on top of content the disk has since moved past.
  const [draftDiffers, setDraftDiffers] = createSignal(false);

  const toggleMinimap = () => {
    const next = !minimapOn();
    setMinimapOn(next);
    setMinimapEnabled(next);
    editor?.updateOptions({ minimap: { enabled: next } });
  };
  let resizeObserver: ResizeObserver | null = null;
  // The content and its on-disk fingerprint the way we last saw it. `diskHash`
  // is sent back as `if_match` so the server refuses a save that would clobber
  // a change made underneath us (#237).
  let savedContent = "";
  let diskHash: string | undefined;
  let diskMtime: number | undefined;
  let disposed = false;
  let contentChangeDisposable: { dispose: () => void } | null = null;
  let unregisterEditor: () => void = () => undefined;

  const isDirty = () => {
    const tab = tabsStore.tabs.find((t) => t.id === props.tabId);
    return tab?.kind === "editor" ? Boolean(tab.dirty) : false;
  };

  const save = async (force = false) => {
    if (!editor) return;
    const content = editor.getValue();
    setStatus("saving");
    try {
      const res = await api.writeFile(
        props.path,
        content,
        false,
        // A forced overwrite drops the guard; a normal save carries the last
        // hash we read so a stale write is refused rather than silently winning.
        force ? undefined : diskHash,
      );
      savedContent = content;
      diskHash = res.hash ?? diskHash;
      diskMtime = res.mtime ?? diskMtime;
      rememberEditorDraft(props.tabId, {
        path: props.path,
        content,
        viewState: editor.saveViewState(),
      });
      setEditorDirty(props.tabId, false);
      setConflict(false);
      setDraftDiffers(false);
      setSavedAt(Date.now());
      setStatus("ready");
      setError(null);
      // Tell the file tree (and its Git markers) that the workspace moved.
      bumpWorkspaceVersion();
    } catch (e) {
      if (e instanceof ApiError && e.status === 409) {
        // The file changed on disk. Don't clobber it silently — surface the
        // choice inline and keep the buffer intact.
        setConflict(true);
        setError(null);
        setStatus("ready");
        return;
      }
      setError((e as Error).message);
      setStatus("error");
    }
  };

  const reloadFromDisk = async () => {
    setStatus("loading");
    try {
      const file = await api.readFile(props.path);
      if (disposed) return;
      if (file.is_binary) {
        setError("binary file (cannot edit)");
        setStatus("error");
        return;
      }
      const disk = file.content ?? "";
      savedContent = disk;
      diskHash = file.hash;
      diskMtime = file.mtime;
      if (editor && model) {
        const view = editor.saveViewState();
        model.setValue(disk);
        if (view) editor.restoreViewState(view);
      }
      discardEditorDraft(props.tabId);
      setEditorDirty(props.tabId, false);
      setConflict(false);
      setDraftDiffers(false);
      setSavedAt(null);
      setStatus("ready");
      setError(null);
    } catch (e) {
      if (disposed) return;
      setError((e as Error).message);
      setStatus("error");
    }
  };

  onMount(async () => {
    if (!host) return;
    // Track this file as recently opened
    addRecentFile(props.path);
    const mountedHost = host;
    try {
      const [monaco, file] = await Promise.all([
        loadMonaco(),
        api.readFile(props.path),
      ]);
      if (disposed) return;
      if (file.is_binary) {
        setError("binary file (cannot edit)");
        setStatus("error");
        return;
      }
      savedContent = file.content ?? "";
      diskHash = file.hash;
      diskMtime = file.mtime;
      const remembered = readEditorDraft(props.tabId, props.path);
      const initialContent = remembered?.content ?? savedContent;
      // A draft we are about to restore over content the disk has since changed
      // is not a silent win — say so, so the reader knows their draft is stale.
      if (remembered && remembered.content !== savedContent) {
        setDraftDiffers(true);
      }
      model = monaco.editor.createModel(
        initialContent,
        languageFor(props.path),
        monaco.Uri.parse(`inmemory://workspace/${props.path}`),
      );
      if (disposed) {
        model.dispose();
        model = null;
        return;
      }
      editor = monaco.editor.create(mountedHost, {
        model,
        theme: "vs-dark",
        automaticLayout: false,
        fontFamily:
          '"JetBrainsMono Nerd Font", "JetBrains Mono", ui-monospace, monospace',
        fontSize: 13,
        minimap: { enabled: getMinimapEnabled() },
        wordWrap: "on",
        scrollBeyondLastLine: false,
        renderWhitespace: "selection",
      });
      if (remembered?.viewState) editor.restoreViewState(remembered.viewState);
      setEditorDirty(props.tabId, initialContent !== savedContent);
      contentChangeDisposable = editor.onDidChangeModelContent(() => {
        if (!editor) return;
        const content = editor.getValue();
        if (content === savedContent) {
          discardEditorDraft(props.tabId);
          // Back in sync with disk — the stale-draft notice no longer applies.
          setDraftDiffers(false);
        } else {
          rememberEditorDraft(props.tabId, {
            path: props.path,
            content,
            viewState: editor.saveViewState(),
          });
        }
        setEditorDirty(props.tabId, content !== savedContent);
      });
      editor.addCommand(
        monaco.KeyMod.CtrlCmd | monaco.KeyCode.KeyS,
        () => void save(),
      );
      unregisterEditor = registerEditor(props.tabId, {
        path: props.path,
        getEditor: () => editor,
        getModel: () => model,
        save: () => save(),
      });
      resizeObserver = new ResizeObserver(() => editor?.layout());
      resizeObserver.observe(mountedHost);
      setStatus("ready");
    } catch (e) {
      if (disposed) return;
      setError((e as Error).message);
      setStatus("error");
    }
  });

  onCleanup(() => {
    disposed = true;
    if (editor) {
      rememberEditorDraft(props.tabId, {
        path: props.path,
        content: editor.getValue(),
        viewState: editor.saveViewState(),
      });
    }
    unregisterEditor();
    resizeObserver?.disconnect();
    contentChangeDisposable?.dispose();
    editor?.dispose();
    model?.dispose();
    resizeObserver = null;
    contentChangeDisposable = null;
    editor = null;
    model = null;
  });

  return (
    <div class="editor-shell">
      <div class="editor-toolbar">
        <div class="editor-breadcrumb">
          <For each={props.path.split("/").filter(Boolean)}>
            {(segment, idx) => (
              <>
                <Show when={idx() > 0}>
                  <span class="breadcrumb-separator">/</span>
                </Show>
                <span class="breadcrumb-segment">{segment}</span>
              </>
            )}
          </For>
        </div>
        <span class="editor-status">
          <Show when={status() === "loading"}>loading…</Show>
          <Show when={status() === "saving"}>saving…</Show>
          <Show when={status() === "error"}>
            <span style={{ color: "#ff7b72" }}>{error()}</span>
          </Show>
          <Show when={status() === "ready" && savedAt()}>
            <span style={{ color: "var(--fg-muted)" }}>
              saved {new Date(savedAt()!).toLocaleTimeString()}
            </span>
          </Show>
        </span>
        <button
          onClick={() => revealPathInGit(props.path)}
          title="Reveal this file in Git"
        >
          Reveal in Git
        </button>
        <button
          onClick={toggleMinimap}
          title="Toggle minimap"
          class={minimapOn() ? "active" : ""}
        >
          🗺
        </button>
        <button
          onClick={() => void reloadFromDisk()}
          disabled={status() === "loading" || status() === "saving"}
          title="Discard local changes and reload from disk"
        >
          Reload
        </button>
        <button
          onClick={() => void save()}
          disabled={status() === "loading" || status() === "saving" || !isDirty()}
        >
          Save
        </button>
      </div>
      <Show when={conflict()}>
        <div class="editor-banner editor-banner-conflict" role="alert">
          <span>File changed on disk since you opened it.</span>
          <span class="editor-banner-actions">
            <button type="button" onClick={() => void save(true)}>Overwrite</button>
            <button type="button" onClick={() => void reloadFromDisk()}>Reload</button>
          </span>
        </div>
      </Show>
      <Show when={draftDiffers() && !conflict()}>
        <div class="editor-banner editor-banner-draft" role="status">
          <span>Restored unsaved draft; disk differs.</span>
          <span class="editor-banner-actions">
            <button type="button" onClick={() => void reloadFromDisk()}>Reload disk</button>
          </span>
        </div>
      </Show>
      <div class="editor-host" ref={host} />
    </div>
  );
};

export default Editor;
