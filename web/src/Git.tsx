import {
  Component,
  For,
  Show,
  createEffect,
  createMemo,
  createResource,
  createSignal,
  onCleanup,
} from "solid-js";
import {
  api,
  type GitBranch,
  type GitLogEntry,
  type GitOpRequest,
  type GitOpResponse,
  type GitStatusEntry,
  type GitStatusKind,
  type GitStatusResp,
} from "./api";
import {
  languageFor,
  loadMonaco,
  type DiffEditor,
  type MonacoNamespace,
  type TextModel,
} from "./monaco";
import { readToolDraft, writeToolDraft } from "./toolDrafts";

interface Props {
  repo: string;
  confirmAction?: (title: string, body?: string) => Promise<boolean>;
}

interface GitDraft {
  selected: string | null;
  diffStaged: boolean;
  commitMessage: string;
  branchTarget: string;
  newBranch: string;
}

function gitDraftKey(repo: string): string {
  return `git:${repo}`;
}

function emptyGitDraft(): GitDraft {
  return {
    selected: null,
    diffStaged: false,
    commitMessage: "",
    branchTarget: "",
    newBranch: "",
  };
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

const EMPTY_BRANCH_INFO: GitBranch = {
  current: "",
  all: [],
};

function isNotGitRepoError(message: string): boolean {
  return (
    message.includes("not found") ||
    message.includes("not a git repository") ||
    message.includes("Stopping at filesystem boundary")
  );
}

function formatApiError(error: unknown): string {
  const message = error instanceof Error ? error.message : String(error);
  return message.replace(/^HTTP \d+:\s*/, "").trim() || message;
}

const DiffView: Component<{ repo: string; path: string; staged: boolean }> = (props) => {
  let host: HTMLDivElement | undefined;
  let editor: DiffEditor | null = null;
  let originalModel: TextModel | null = null;
  let modifiedModel: TextModel | null = null;
  let resizeObserver: ResizeObserver | null = null;
  const [err, setErr] = createSignal<string | null>(null);
  let disposed = false;
  let loadGeneration = 0;
  let monaco: MonacoNamespace | null = null;

  const disposeModels = () => {
    editor?.setModel(null);
    originalModel?.dispose();
    modifiedModel?.dispose();
    originalModel = null;
    modifiedModel = null;
  };

  const disposeDiff = () => {
    resizeObserver?.disconnect();
    resizeObserver = null;
    disposeModels();
    editor?.dispose();
    editor = null;
  };

  const ensureEditor = async (mountedHost: HTMLDivElement) => {
    if (editor) return editor;
    monaco ??= await loadMonaco();
    if (disposed) return null;
    editor = monaco.editor.createDiffEditor(mountedHost, {
      theme: "vs-dark",
      readOnly: true,
      renderSideBySide: true,
      automaticLayout: false,
      fontFamily:
        '"JetBrainsMono Nerd Font", "JetBrains Mono", ui-monospace, monospace',
      fontSize: 13,
      minimap: { enabled: false },
    });
    resizeObserver = new ResizeObserver(() => editor?.layout());
    resizeObserver.observe(mountedHost);
    return editor;
  };

  const loadDiff = async (repo: string, path: string, staged: boolean, generation: number) => {
    const mountedHost = host;
    if (!mountedHost) return;
    try {
      setErr(null);
      const [nextEditor, d] = await Promise.all([
        ensureEditor(mountedHost),
        api.gitDiff(repo, path, staged),
      ]);
      if (disposed || generation !== loadGeneration || !nextEditor || !monaco) return;
      const lang = languageFor(path);
      const original = monaco.editor.createModel(d.head, lang);
      const modified = monaco.editor.createModel(d.current, lang);
      if (disposed || generation !== loadGeneration) {
        original.dispose();
        modified.dispose();
        return;
      }
      disposeModels();
      originalModel = original;
      modifiedModel = modified;
      nextEditor.setModel({ original, modified });
    } catch (e) {
      if (!disposed && generation === loadGeneration) {
        disposeModels();
        setErr(formatApiError(e));
      }
    }
  };

  createEffect(() => {
    const repo = props.repo;
    const path = props.path;
    const staged = props.staged;
    loadGeneration += 1;
    void loadDiff(repo, path, staged, loadGeneration);
  });

  onCleanup(() => {
    disposed = true;
    loadGeneration += 1;
    disposeDiff();
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
  const restored = readToolDraft(gitDraftKey(props.repo), emptyGitDraft());
  const [gitError, setGitError] = createSignal<string | null>(null);
  const [actionError, setActionError] = createSignal<string | null>(null);
  const [actionInfo, setActionInfo] = createSignal<string | null>(null);
  const [selected, setSelected] = createSignal<string | null>(restored.selected);
  const [diffStaged, setDiffStaged] = createSignal(restored.diffStaged);
  const [commitMessage, setCommitMessage] = createSignal(restored.commitMessage);
  const [branchTarget, setBranchTarget] = createSignal(restored.branchTarget);
  const [newBranch, setNewBranch] = createSignal(restored.newBranch);
  const [busyAction, setBusyAction] = createSignal<string | null>(null);

  const snapshotDraft = (): GitDraft => ({
    selected: selected(),
    diffStaged: diffStaged(),
    commitMessage: commitMessage(),
    branchTarget: branchTarget(),
    newBranch: newBranch(),
  });

  const [status, { refetch: refetchStatus }] = createResource(
    () => props.repo,
    async (repo): Promise<GitStatusResp | null> => {
      try {
        const result = await api.gitStatus(repo);
        setGitError(null);
        return result;
      } catch (e) {
        const message = formatApiError(e);
        setGitError(isNotGitRepoError(message) ? "Not a git repository" : message);
        return null;
      }
    },
  );

  const [branches, { refetch: refetchBranches }] = createResource(
    () => props.repo,
    async (repo): Promise<GitBranch> => {
      try {
        return await api.gitBranch(repo);
      } catch {
        return EMPTY_BRANCH_INFO;
      }
    },
  );

  const [log, { refetch: refetchLog }] = createResource(
    () => props.repo,
    async (repo): Promise<GitLogEntry[]> => {
      try {
        return await api.gitLog(repo, 30);
      } catch {
        return [];
      }
    },
  );

  const notRepo = () =>
    status()?.is_repo === false || gitError() === "Not a git repository";

  const branchInfo = createMemo(() => branches() ?? EMPTY_BRANCH_INFO);

  const branchOptions = createMemo(() =>
    Array.from(
      new Set([branchInfo().current, ...branchInfo().all].filter((name): name is string => !!name)),
    ),
  );

  const grouped = createMemo(() => {
    const out: Record<GitStatusKind, GitStatusEntry[]> = {
      conflicted: [],
      staged: [],
      modified: [],
      renamed: [],
      deleted: [],
      untracked: [],
    };
    for (const entry of status()?.entries ?? []) {
      out[entry.kind].push(entry);
    }
    return out;
  });

  const selectedEntry = createMemo(() => {
    const path = selected();
    if (!path) return null;
    return status()?.entries.find((entry) => entry.path === path) ?? null;
  });

  const stagedCount = createMemo(
    () =>
      status()?.entries.filter((entry) => entry.index !== " " && entry.index !== "?").length ?? 0,
  );

  const canDiffStaged = createMemo(() => {
    const entry = selectedEntry();
    return !!entry && entry.index !== " " && entry.index !== "?";
  });

  const canStage = createMemo(() => {
    const entry = selectedEntry();
    return !!entry && (entry.kind === "untracked" || entry.worktree !== " ");
  });

  const canUnstage = createMemo(() => {
    const entry = selectedEntry();
    return !!entry && entry.index !== " " && entry.index !== "?";
  });

  let observedRepo = props.repo;
  createEffect(() => {
    const repo = props.repo;
    if (repo === observedRepo) return;
    writeToolDraft(gitDraftKey(observedRepo), snapshotDraft());
    observedRepo = repo;
    const next = readToolDraft(gitDraftKey(repo), emptyGitDraft());
    setSelected(next.selected);
    setDiffStaged(next.diffStaged);
    setCommitMessage(next.commitMessage);
    setBranchTarget(next.branchTarget);
    setNewBranch(next.newBranch);
    setActionError(null);
    setActionInfo(null);
  });

  onCleanup(() => {
    writeToolDraft(gitDraftKey(observedRepo), snapshotDraft());
  });

  createEffect(() => {
    const path = selected();
    if (!path) return;
    const currentStatus = status();
    if (!currentStatus) return;
    const present = currentStatus.entries.some((entry) => entry.path === path);
    if (!present) setSelected(null);
  });

  createEffect(() => {
    if (!canDiffStaged() && diffStaged()) {
      setDiffStaged(false);
    }
  });

  createEffect(() => {
    const loadedBranches = branches();
    if (!loadedBranches) return;
    const current = loadedBranches.current.trim();
    const target = branchTarget().trim();
    if (!current && loadedBranches.all.length === 0) {
      if (target) setBranchTarget("");
      return;
    }
    if (!target || (!branchOptions().includes(target) && target !== current)) {
      setBranchTarget(current || branchOptions()[0] || "");
    }
  });

  const refresh = async () => {
    setActionError(null);
    await Promise.all([refetchStatus(), refetchBranches(), refetchLog()]);
  };

  const runGitOp = async (request: GitOpRequest): Promise<GitOpResponse | null> => {
    setBusyAction(request.op);
    setActionError(null);
    setActionInfo(null);
    try {
      const response = await api.gitOp(request);
      await Promise.all([refetchStatus(), refetchBranches(), refetchLog()]);
      return response;
    } catch (e) {
      setActionError(formatApiError(e));
      return null;
    } finally {
      setBusyAction(null);
    }
  };

  const stageSelected = async () => {
    const entry = selectedEntry();
    if (!entry) return;
    const response = await runGitOp({ op: "stage", repo: props.repo, path: entry.path });
    if (!response) return;
    setActionInfo(`Staged ${entry.path}`);
  };

  const unstageSelected = async () => {
    const entry = selectedEntry();
    if (!entry) return;
    const response = await runGitOp({ op: "unstage", repo: props.repo, path: entry.path });
    if (!response) return;
    setActionInfo(`Unstaged ${entry.path}`);
  };

  const discardSelected = async () => {
    const entry = selectedEntry();
    if (!entry) return;
    if (!props.confirmAction) return;
    if (!await props.confirmAction(
      `Discard changes in "${entry.path}"?`,
      "Uncommitted working-tree changes in this file will be permanently lost.",
    )) return;
    const response = await runGitOp({ op: "discard", repo: props.repo, path: entry.path });
    if (!response) return;
    setActionInfo(`Discarded ${entry.path}`);
  };

  const commitChanges = async () => {
    const message = commitMessage().trim();
    if (!message) {
      setActionError("Commit message is required");
      return;
    }
    const response = await runGitOp({ op: "commit", repo: props.repo, message });
    if (!response) return;
    setCommitMessage("");
    setDiffStaged(false);
    const shortHash = response.commit?.slice(0, 7);
    setActionInfo(shortHash ? `Committed ${shortHash}` : "Created commit");
  };

  const checkoutBranch = async () => {
    const branch = branchTarget().trim();
    if (!branch) {
      setActionError("Select a branch to check out");
      return;
    }
    const response = await runGitOp({
      op: "checkout",
      repo: props.repo,
      branch,
      create: false,
    });
    if (!response) return;
    setBranchTarget(branch);
    setActionInfo(`Checked out ${branch}`);
  };

  const createBranch = async () => {
    const branch = newBranch().trim();
    if (!branch) {
      setActionError("Branch name is required");
      return;
    }
    const response = await runGitOp({
      op: "checkout",
      repo: props.repo,
      branch,
      create: true,
    });
    if (!response) return;
    setNewBranch("");
    setBranchTarget(branch);
    setActionInfo(`Created and checked out ${branch}`);
  };

  const busy = (op: string) => busyAction() === op;

  return (
    <div class="git-shell">
      <div class="git-toolbar">
        <span class="git-repo">
          {status()?.repo || props.repo || "(workspace root)"}
        </span>
        <span class="git-branch">
          {"\u2387"} {notRepo() ? "Not a git repository" : status()?.branch || gitError() || "?"}
          <Show when={status()?.ahead}>
            <span class="git-ab"> {"\u2191"}{status()!.ahead}</span>
          </Show>
          <Show when={status()?.behind}>
            <span class="git-ab"> {"\u2193"}{status()!.behind}</span>
          </Show>
        </span>
        <button onClick={() => void refresh()} disabled={busyAction() !== null}>
          {busyAction() === null ? "\u27f3 Refresh" : "Working..."}
        </button>
      </div>

      <Show when={actionError()}>
        <div class="git-banner git-banner-error">{actionError()}</div>
      </Show>
      <Show when={!actionError() && actionInfo()}>
        <div class="git-banner git-banner-info">{actionInfo()}</div>
      </Show>
      <Show when={gitError()}>
        <div
          class="empty"
          style={{
            color: gitError() === "Not a git repository" ? "var(--fg-muted)" : "#ff7b72",
          }}
        >
          {gitError()}
        </div>
      </Show>

      <div class="git-body">
        <div class="git-left">
          <div class="git-section-title">Workflow</div>
          <div class="git-controls">
            <div class="git-control-block">
              <div class="git-control-label">Branch</div>
              <div class="git-control-row">
                <select
                  value={branchTarget()}
                  disabled={notRepo() || busyAction() !== null || branchOptions().length === 0}
                  onInput={(event) => setBranchTarget(event.currentTarget.value)}
                >
                  <Show when={branchOptions().length > 0} fallback={<option value="">No branches</option>}>
                    <For each={branchOptions()}>
                      {(branch) => <option value={branch}>{branch}</option>}
                    </For>
                  </Show>
                </select>
                <button
                  onClick={() => void checkoutBranch()}
                  disabled={
                    notRepo() ||
                    busyAction() !== null ||
                    !branchTarget().trim() ||
                    branchTarget().trim() === branchInfo().current
                  }
                >
                  {busy("checkout") ? "Switching..." : "Checkout"}
                </button>
              </div>
              <div class="git-control-row">
                <input
                  type="text"
                  value={newBranch()}
                  placeholder="new branch"
                  disabled={notRepo() || busyAction() !== null}
                  onInput={(event) => setNewBranch(event.currentTarget.value)}
                  onKeyDown={(event) => {
                    if (event.key === "Enter") {
                      event.preventDefault();
                      void createBranch();
                    }
                  }}
                />
                <button
                  onClick={() => void createBranch()}
                  disabled={notRepo() || busyAction() !== null || !newBranch().trim()}
                >
                  {busy("checkout") ? "Creating..." : "Create"}
                </button>
              </div>
            </div>

            <div class="git-control-block">
              <div class="git-control-topline">
                <div class="git-control-label">Commit</div>
                <span class="meta">
                  {stagedCount()} staged file{stagedCount() === 1 ? "" : "s"}
                </span>
              </div>
              <textarea
                rows={3}
                value={commitMessage()}
                placeholder="Commit message"
                disabled={notRepo() || busyAction() !== null}
                onInput={(event) => setCommitMessage(event.currentTarget.value)}
                onKeyDown={(event) => {
                  if ((event.metaKey || event.ctrlKey) && event.key === "Enter") {
                    event.preventDefault();
                    void commitChanges();
                  }
                }}
              />
              <div class="git-control-row git-control-row-end">
                <button
                  onClick={() => void commitChanges()}
                  disabled={
                    notRepo() ||
                    busyAction() !== null ||
                    stagedCount() === 0 ||
                    !commitMessage().trim()
                  }
                >
                  {busy("commit") ? "Committing..." : "Commit staged"}
                </button>
              </div>
            </div>
          </div>

          <div class="git-section-title">Status</div>
          <Show
            when={!notRepo() && (status()?.entries.length ?? 0) > 0}
            fallback={
              <div class="meta" style={{ padding: "8px 10px" }}>
                {notRepo() ? "not a git repository" : "clean working tree"}
              </div>
            }
          >
            <For each={kindOrder}>
              {(kind) => (
                <Show when={grouped()[kind].length > 0}>
                  <div class="git-group">{kindLabel[kind]}</div>
                  <For each={grouped()[kind]}>
                    {(entry) => (
                      <div
                        class={`git-entry ${selected() === entry.path ? "selected" : ""}`}
                        onClick={() => setSelected(entry.path)}
                        title={`${entry.index}${entry.worktree} ${entry.path}`}
                      >
                        <span class={`git-badge git-${kind}`}>{kindBadge[kind]}</span>
                        <span class="git-path">{entry.path}</span>
                      </div>
                    )}
                  </For>
                </Show>
              )}
            </For>
          </Show>

          <div class="git-section-title">Recent commits</div>
          <For each={log() ?? []}>
            {(commit) => (
              <div class="git-commit" title={commit.hash}>
                <span class="git-commit-short">{commit.short}</span>
                <span class="git-commit-subject">{commit.subject}</span>
                <span class="git-commit-meta">
                  {commit.author} {"\u00b7"} {commit.date.split("T")[0]}
                </span>
              </div>
            )}
          </For>
        </div>

        <div class="git-right">
          <Show
            when={selectedEntry()}
            fallback={<div class="empty">Select a file on the left to view its diff.</div>}
          >
            {(entry) => (
              <>
                <div class="git-diff-toolbar">
                  <div class="git-diff-path">{entry().path}</div>
                  <div class="git-diff-actions">
                    <button
                      class={diffStaged() ? "" : "active"}
                      onClick={() => setDiffStaged(false)}
                    >
                      Worktree
                    </button>
                    <button
                      class={diffStaged() ? "active" : ""}
                      onClick={() => setDiffStaged(true)}
                      disabled={!canDiffStaged()}
                    >
                      Staged
                    </button>
                    <button
                      onClick={() => void stageSelected()}
                      disabled={!canStage() || busyAction() !== null}
                    >
                      {busy("stage") ? "Staging..." : "Stage"}
                    </button>
                    <button
                      onClick={() => void unstageSelected()}
                      disabled={!canUnstage() || busyAction() !== null}
                    >
                      {busy("unstage") ? "Unstaging..." : "Unstage"}
                    </button>
                    <button
                      onClick={() => void discardSelected()}
                      disabled={busyAction() !== null}
                    >
                      {busy("discard") ? "Discarding..." : "Discard"}
                    </button>
                  </div>
                </div>
                <DiffView repo={props.repo} path={entry().path} staged={diffStaged()} />
              </>
            )}
          </Show>
        </div>
      </div>
    </div>
  );
};

export default GitTab;
