import {
  Component,
  For,
  Show,
  createMemo,
  createResource,
  createSignal,
  onCleanup,
} from "solid-js";
import { api, type GitStatusEntry, type GitStatusKind } from "./api";

interface Props {
  repo: string;
}

type MonacoNs = typeof import("monaco-editor");
type DiffEditor = import("monaco-editor").editor.IStandaloneDiffEditor;

let monacoP: Promise<MonacoNs> | null = null;
function loadMonaco(): Promise<MonacoNs> {
  if (!monacoP) {
    monacoP = (async () => {
      (self as unknown as { MonacoEnvironment?: object }).MonacoEnvironment ??= {
        getWorker: () => {
          const blob = new Blob(["self.onmessage=()=>{}"], {
            type: "text/javascript",
          });
          return new Worker(URL.createObjectURL(blob));
        },
      };
      return import("monaco-editor");
    })();
  }
  return monacoP;
}

const kindOrder: GitStatusKind[] = [
  "conflicted",
  "staged",
  "modified",
  "renamed",
  "deleted",
  "untracked",
];

const kindLabel: Record<GitStatusKind, string> = {
  conflicted: "Conflicts",
  staged: "Staged",
  modified: "Modified",
  renamed: "Renamed",
  deleted: "Deleted",
  untracked: "Untracked",
};

const kindBadge: Record<GitStatusKind, string> = {
  conflicted: "!",
  staged: "+",
  modified: "M",
  renamed: "R",
  deleted: "D",
  untracked: "?",
};

const DiffView: Component<{ repo: string; path: string }> = (props) => {
  let host: HTMLDivElement | undefined;
  let editor: DiffEditor | null = null;
  let resizeObserver: ResizeObserver | null = null;
  const [err, setErr] = createSignal<string | null>(null);

  const init = async () => {
    if (!host) return;
    try {
      const [monaco, d] = await Promise.all([
        loadMonaco(),
        api.gitDiff(props.repo, props.path, false),
      ]);
      const lang = props.path.endsWith(".rs")
        ? "rust"
        : props.path.endsWith(".ts") || props.path.endsWith(".tsx")
          ? "typescript"
          : props.path.endsWith(".md")
            ? "markdown"
            : "plaintext";
      const original = monaco.editor.createModel(d.head, lang);
      const modified = monaco.editor.createModel(d.current, lang);
      editor = monaco.editor.createDiffEditor(host, {
        theme: "vs-dark",
        readOnly: true,
        renderSideBySide: true,
        automaticLayout: false,
        fontFamily:
          '"JetBrainsMono Nerd Font", "JetBrains Mono", ui-monospace, monospace',
        fontSize: 13,
        minimap: { enabled: false },
      });
      editor.setModel({ original, modified });
      resizeObserver = new ResizeObserver(() => editor?.layout());
      resizeObserver.observe(host);
    } catch (e) {
      setErr((e as Error).message);
    }
  };

  // Refire when path changes
  createMemo(() => {
    props.path;
    editor?.dispose();
    editor = null;
    if (host) host.innerHTML = "";
    void init();
  });

  onCleanup(() => {
    resizeObserver?.disconnect();
    editor?.dispose();
    editor = null;
  });

  return (
    <div class="diff-shell">
      <Show when={err()}>
        <div class="empty" style={{ color: "#ff7b72" }}>
          {err()}
        </div>
      </Show>
      <div class="diff-host" ref={host} />
    </div>
  );
};

const GitTab: Component<Props> = (props) => {
  const [status, { refetch: refetchStatus }] = createResource(
    () => props.repo,
    (repo) => api.gitStatus(repo),
  );
  const [log, { refetch: refetchLog }] = createResource(
    () => props.repo,
    (repo) => api.gitLog(repo, 30),
  );
  const [selected, setSelected] = createSignal<string | null>(null);

  const grouped = createMemo(() => {
    const out: Record<GitStatusKind, GitStatusEntry[]> = {
      conflicted: [],
      staged: [],
      modified: [],
      renamed: [],
      deleted: [],
      untracked: [],
    };
    for (const e of status()?.entries ?? []) {
      out[e.kind].push(e);
    }
    return out;
  });

  const refresh = () => {
    void refetchStatus();
    void refetchLog();
  };

  return (
    <div class="git-shell">
      <div class="git-toolbar">
        <span class="git-repo">
          {status()?.repo || props.repo || "(workspace root)"}
        </span>
        <span class="git-branch">
          ⎇ {status()?.branch ?? "?"}
          <Show when={status()?.ahead}>
            <span class="git-ab"> ↑{status()!.ahead}</span>
          </Show>
          <Show when={status()?.behind}>
            <span class="git-ab"> ↓{status()!.behind}</span>
          </Show>
        </span>
        <button onClick={refresh}>⟳ Refresh</button>
      </div>
      <Show when={status.error}>
        <div class="empty" style={{ color: "#ff7b72" }}>
          {String(status.error)}
        </div>
      </Show>
      <div class="git-body">
        <div class="git-left">
          <div class="git-section-title">Status</div>
          <Show
            when={(status()?.entries.length ?? 0) > 0}
            fallback={
              <div class="meta" style={{ padding: "8px" }}>
                clean working tree
              </div>
            }
          >
            <For each={kindOrder}>
              {(k) => (
                <Show when={grouped()[k].length > 0}>
                  <div class="git-group">{kindLabel[k]}</div>
                  <For each={grouped()[k]}>
                    {(e) => (
                      <div
                        class={`git-entry ${selected() === e.path ? "selected" : ""}`}
                        onClick={() => setSelected(e.path)}
                        title={`${e.index}${e.worktree} ${e.path}`}
                      >
                        <span class={`git-badge git-${k}`}>{kindBadge[k]}</span>
                        <span class="git-path">{e.path}</span>
                      </div>
                    )}
                  </For>
                </Show>
              )}
            </For>
          </Show>

          <div class="git-section-title">Recent commits</div>
          <For each={log() ?? []}>
            {(c) => (
              <div class="git-commit" title={c.hash}>
                <span class="git-commit-short">{c.short}</span>
                <span class="git-commit-subject">{c.subject}</span>
                <span class="git-commit-meta">
                  {c.author} · {c.date.split("T")[0]}
                </span>
              </div>
            )}
          </For>
        </div>
        <div class="git-right">
          <Show
            when={selected()}
            fallback={
              <div class="empty">Select a file on the left to view its diff.</div>
            }
          >
            <DiffView repo={props.repo} path={selected()!} />
          </Show>
        </div>
      </div>
    </div>
  );
};

export default GitTab;
