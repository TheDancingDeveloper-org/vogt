import {
  For,
  Show,
  type Component,
  createEffect,
  createSignal,
  onCleanup,
} from "solid-js";
import { api, type FileSearchResult } from "./api";
import Dialog from "./Dialog";
import { bumpWorkspaceVersion } from "./workspaceVersion";

export type FileWorkflow = "new" | "open";

interface Props {
  workflow: FileWorkflow;
  onClose: () => void;
  onOpenFile: (path: string) => void;
  onFileCreated?: (path: string) => void;
}

function relativeDirectory(value: string): string {
  return value.trim().replace(/^\/+|\/+$/g, "");
}

function filePath(directory: string, filename: string): string {
  const cleanDirectory = relativeDirectory(directory);
  return cleanDirectory ? `${cleanDirectory}/${filename}` : filename;
}

/** True when a file already reads back at `path`. A read that fails (a 404, or
 *  the network being down) is treated as "not there" — the create then either
 *  succeeds or surfaces the real write error, never a silent overwrite. */
async function fileAlreadyExists(path: string): Promise<boolean> {
  try {
    await api.readFile(path);
    return true;
  } catch {
    return false;
  }
}

const FileWorkflowDialog: Component<Props> = (props) => {
  const [destination, setDestination] = createSignal("");
  const [filename, setFilename] = createSignal("");
  const [query, setQuery] = createSignal("");
  const [results, setResults] = createSignal<FileSearchResult[]>([]);
  const [activeIndex, setActiveIndex] = createSignal(0);
  const [searching, setSearching] = createSignal(false);
  const [searched, setSearched] = createSignal(false);
  const [submitting, setSubmitting] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);

  createEffect(() => {
    if (props.workflow !== "open") return;
    const term = query().trim();
    setError(null);
    setSearched(false);
    if (!term) {
      setResults([]);
      setSearching(false);
      return;
    }

    let active = true;
    const controller = new AbortController();
    setSearching(true);
    const timer = window.setTimeout(() => {
      void api.searchFiles(term, "", 100, controller.signal).then(
        (files) => {
          if (!active) return;
          setResults(files);
          setActiveIndex(0);
          setSearching(false);
          setSearched(true);
        },
        (cause: unknown) => {
          if (!active) return;
          setResults([]);
          setSearching(false);
          setSearched(true);
          setError(`File search failed: ${(cause as Error).message}`);
        },
      );
    }, 160);

    onCleanup(() => {
      active = false;
      controller.abort();
      window.clearTimeout(timer);
    });
  });

  const openFile = (path: string) => {
    props.onClose();
    props.onOpenFile(path);
  };

  const createFile = async () => {
    const name = filename().trim();
    if (!name) {
      setError("Enter a filename.");
      return;
    }
    if (name === "." || name === ".." || /[\\/]/.test(name)) {
      setError("Filename must be one name. Put folders in Destination folder.");
      return;
    }

    const path = filePath(destination(), name);
    setSubmitting(true);
    setError(null);
    try {
      // The engine's PUT is last-writer-wins, so a create at an occupied path
      // would silently replace the file that lives there. Refuse instead: if
      // the path already reads back, stop and let the reader pick another name.
      // (`create_parents` stays true — the dialog promises to make folders.)
      if (await fileAlreadyExists(path)) {
        setError(`A file already exists at ${path}. Choose a different name to avoid overwriting it.`);
        return;
      }
      await api.writeFile(path, "", true);
      // The file tree (mounted or not) should reflect the new file without a
      // manual Refresh (#238).
      bumpWorkspaceVersion();
      props.onFileCreated?.(path);
      openFile(path);
    } catch (cause) {
      setError(`File creation failed: ${(cause as Error).message}`);
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <Dialog
      title={props.workflow === "new" ? "New file" : "Open file"}
      description={
        props.workflow === "new"
          ? "Choose a workspace destination and filename. Parent folders are created as needed."
          : "Search the workspace, then choose a file to open in the editor."
      }
      onClose={props.onClose}
      dialogClass="modal file-workflow-dialog"
    >
      <Show
        when={props.workflow === "new"}
        fallback={
          <>
            <label>
              Search workspace files
              <input
                type="search"
                data-dialog-initial-focus
                value={query()}
                placeholder="Filename or path"
                role="combobox"
                aria-expanded={results().length > 0}
                aria-controls="file-workflow-results"
                aria-activedescendant={
                  results().length > 0 ? `file-workflow-result-${activeIndex()}` : undefined
                }
                onInput={(event) => setQuery(event.currentTarget.value)}
                onKeyDown={(event) => {
                  // Arrow keys walk the matches; Enter opens the highlighted one
                  // without the reader ever leaving the search box.
                  const total = results().length;
                  if (total === 0) return;
                  if (event.key === "ArrowDown") {
                    event.preventDefault();
                    setActiveIndex((index) => (index + 1) % total);
                  } else if (event.key === "ArrowUp") {
                    event.preventDefault();
                    setActiveIndex((index) => (index - 1 + total) % total);
                  } else if (event.key === "Enter") {
                    const chosen = results()[activeIndex()];
                    if (chosen) {
                      event.preventDefault();
                      openFile(chosen.path);
                    }
                  }
                }}
              />
            </label>
            <div
              id="file-workflow-results"
              class="file-workflow-results"
              role="listbox"
              aria-label="Matching workspace files"
              aria-live="polite"
            >
              <Show when={searching()}>
                <div class="file-workflow-status">Searching workspace…</div>
              </Show>
              <Show when={!searching() && searched() && results().length === 0 && !error()}>
                <div class="file-workflow-status">No matching files</div>
              </Show>
              <For each={results()}>
                {(file, index) => (
                  <button
                    type="button"
                    id={`file-workflow-result-${index()}`}
                    class={`file-workflow-result${index() === activeIndex() ? " active" : ""}`}
                    role="option"
                    aria-selected={index() === activeIndex()}
                    aria-label={`${file.name} — ${file.path}`}
                    onPointerMove={() => setActiveIndex(index())}
                    onClick={() => openFile(file.path)}
                  >
                    <span>{file.name}</span>
                    <small>{file.path}</small>
                  </button>
                )}
              </For>
            </div>
          </>
        }
      >
        <form
          class="file-workflow-form"
          onSubmit={(event) => {
            event.preventDefault();
            void createFile();
          }}
        >
          <label>
            Destination folder
            <input
              type="text"
              data-dialog-initial-focus
              value={destination()}
              placeholder="Workspace root"
              onInput={(event) => setDestination(event.currentTarget.value)}
            />
          </label>
          <label>
            Filename
            <input
              type="text"
              value={filename()}
              placeholder="notes.md"
              onInput={(event) => setFilename(event.currentTarget.value)}
            />
          </label>
          <div class="file-workflow-destination">
            {filename().trim()
              ? `Create ${filePath(destination(), filename().trim())}`
              : "No file will be created until a filename is supplied."}
          </div>
          <div class="modal-actions">
            <button type="button" onClick={props.onClose}>Cancel</button>
            <button type="submit" disabled={submitting()}>
              {submitting() ? "Creating…" : "Create file"}
            </button>
          </div>
        </form>
      </Show>

      <Show when={error()}>
        <div class="file-workflow-error" role="alert">{error()}</div>
      </Show>
      <Show when={props.workflow === "open"}>
        <div class="modal-actions">
          <button type="button" onClick={props.onClose}>Cancel</button>
        </div>
      </Show>
    </Dialog>
  );
};

export default FileWorkflowDialog;
