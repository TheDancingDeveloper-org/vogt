import {
  Component,
  For,
  Show,
  createEffect,
  createMemo,
  createSignal,
  onCleanup,
} from "solid-js";
import { useNavigate } from "@solidjs/router";
import {
  api,
  type GitBranch,
  type GitOpRequest,
  type GitOpResponse,
  type GitStatusEntry,
  type GitStatusKind,
} from "./api";
import {
  languageFor,
  loadLanguage,
  loadMonaco,
  monacoThemeForApp,
  syncMonacoTheme,
  type DiffEditor,
  type MonacoNamespace,
  type TextModel,
} from "./monaco";
import { readToolDraft, writeToolDraft } from "./toolDrafts";
import { shouldRenderSideBySide } from "./diffLayout";
import { taxonomy } from "./taxonomyCache";
import { repositoryChoices } from "./gitRepositories";
import { createRetainedRead } from "./retainedRead";
import { openEditorTab } from "./tabs";
import { bumpWorkspaceVersion } from "./workspaceVersion";
import { consumePendingReveal, pendingRevealPath } from "./gitReveal";

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

function formatApiError(error: unknown): string {
  const message = error instanceof Error ? error.message : String(error);
  return message.replace(/^HTTP \d+:\s*/, "").trim() || message;
}

const DiffView: Component<{
  repo: string;
  path: string;
  staged: boolean;
  refreshKey: number;
}> = (props) => {
  let host: HTMLDivElement | undefined;
  let editor: DiffEditor | null = null;
  let originalModel: TextModel | null = null;
  let modifiedModel: TextModel | null = null;
  let resizeObserver: ResizeObserver | null = null;
  const [err, setErr] = createSignal<string | null>(null);
  const [loading, setLoading] = createSignal(false);
  const [reloadKey, setReloadKey] = createSignal(0);
  let disposed = false;
  let loadGeneration = 0;
  let monaco: MonacoNamespace | null = null;
  let displayedKey = "";

  const disposeModels = () => {
    editor?.setModel(null);
    originalModel?.dispose();
    modifiedModel?.dispose();
    originalModel = null;
    modifiedModel = null;
    displayedKey = "";
  };

  const disposeDiff = () => {
    resizeObserver?.disconnect();
    resizeObserver = null;
    disposeModels();
    editor?.dispose();
    editor = null;
  };

  // Side-by-side needs two full code columns; on a narrow (phone) host the
  // diff renders inline instead, and flips back as the host grows (#240).
  const applyDiffLayout = () => {
    if (!editor || !host) return;
    editor.updateOptions({ renderSideBySide: shouldRenderSideBySide(host.clientWidth) });
    editor.layout();
  };

  const ensureEditor = async (mountedHost: HTMLDivElement) => {
    if (editor) return editor;
    monaco ??= await loadMonaco();
    if (disposed) return null;
    syncMonacoTheme(monaco);
    editor = monaco.editor.createDiffEditor(mountedHost, {
      theme: monacoThemeForApp(),
      readOnly: true,
      renderSideBySide: shouldRenderSideBySide(mountedHost.clientWidth),
      automaticLayout: false,
      fontFamily:
        '"JetBrainsMono Nerd Font", "JetBrains Mono", ui-monospace, monospace',
      fontSize: 13,
      minimap: { enabled: false },
    });
    resizeObserver = new ResizeObserver(() => applyDiffLayout());
    resizeObserver.observe(mountedHost);
    return editor;
  };

  const loadDiff = async (repo: string, path: string, staged: boolean, generation: number) => {
    const mountedHost = host;
    if (!mountedHost) return;
    const key = `${repo}\0${path}\0${staged}`;
    if (displayedKey !== key) {
      disposeModels();
      setErr(null);
    }
    try {
      setLoading(true);
      const lang = languageFor(path);
      const [nextEditor, d] = await Promise.all([
        ensureEditor(mountedHost),
        api.gitDiff(repo, path, staged),
        loadLanguage(lang),
      ]);
      if (disposed || generation !== loadGeneration || !nextEditor || !monaco) return;
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
      displayedKey = key;
      setErr(null);
    } catch (e) {
      if (!disposed && generation === loadGeneration) {
        if (displayedKey !== key) disposeModels();
        setErr(formatApiError(e));
      }
    } finally {
      if (!disposed && generation === loadGeneration) setLoading(false);
    }
  };

  createEffect(() => {
    const repo = props.repo;
    const path = props.path;
    const staged = props.staged;
    props.refreshKey;
    reloadKey();
    loadGeneration += 1;
    void loadDiff(repo, path, staged, loadGeneration);
  });

  onCleanup(() => {
    disposed = true;
    loadGeneration += 1;
    disposeDiff();
  });

  return (
    <div class={`diff-shell ${err() && displayedKey ? "stale" : ""}`}>
      <Show when={err()}>
        <div class="git-panel-error" role="alert">
          <span>Diff {displayedKey ? "is stale" : "could not be read"}: {err()}</span>
          <button type="button" disabled={loading()} onClick={() => setReloadKey((value) => value + 1)}>
            Retry diff
          </button>
        </div>
      </Show>
      <div class="diff-host" ref={host} />
    </div>
  );
};

const GitTab: Component<Props> = (props) => {
  const navigate = useNavigate();
  const restored = readToolDraft(gitDraftKey(props.repo), emptyGitDraft());
  const [actionError, setActionError] = createSignal<string | null>(null);
  const [actionInfo, setActionInfo] = createSignal<string | null>(null);
  const [selected, setSelected] = createSignal<string | null>(restored.selected);
  const [diffStaged, setDiffStaged] = createSignal(restored.diffStaged);
  const [commitMessage, setCommitMessage] = createSignal(restored.commitMessage);
  const [branchTarget, setBranchTarget] = createSignal(restored.branchTarget);
  const [newBranch, setNewBranch] = createSignal(restored.newBranch);
  const [busyAction, setBusyAction] = createSignal<string | null>(null);
  const [diffRefreshKey, setDiffRefreshKey] = createSignal(0);

  const snapshotDraft = (): GitDraft => ({
    selected: selected(),
    diffStaged: diffStaged(),
    commitMessage: commitMessage(),
    branchTarget: branchTarget(),
    newBranch: newBranch(),
  });

  const statusRead = createRetainedRead(
    () => props.repo || false,
    (repo) => api.gitStatus(repo),
    (error) => formatApiError(error),
  );
  const branchesRead = createRetainedRead(
    () => props.repo || false,
    (repo) => api.gitBranch(repo),
    (error) => formatApiError(error),
  );
  const logRead = createRetainedRead(
    () => props.repo || false,
    (repo) => api.gitLog(repo, 30),
    (error) => formatApiError(error),
  );
  const projectsRead = createRetainedRead(
    () => !props.repo,
    async () => {
      const projects = [];
      let offset = 0;
      let total = 0;
      do {
        const page = await taxonomy.projects({ limit: 500, offset });
        projects.push(...page.projects);
        total = page.total;
        offset += page.projects.length;
        if (page.projects.length === 0) break;
      } while (projects.length < total);
      return { projects, total };
    },
    (error) => formatApiError(error),
  );
  const workspaceRead = createRetainedRead(
    () => !props.repo,
    () => api.operationalStatus(),
    (error) => formatApiError(error),
  );

  const status = statusRead.data;
  const branches = branchesRead.data;
  const log = logRead.data;

  const choices = createMemo(() => {
    const projects = projectsRead.data()?.projects;
    const workspace = workspaceRead.data()?.storage.workspace_root;
    return projects && workspace ? repositoryChoices(projects, workspace) : [];
  });

  const notRepo = () => status()?.is_repo === false;
  const readsLoading = () =>
    statusRead.loading() || branchesRead.loading() || logRead.loading();

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
    await Promise.all([
      statusRead.retry(),
      branchesRead.retry(),
      logRead.retry(),
    ]);
    setDiffRefreshKey((value) => value + 1);
  };

  // The busy key drives which control shows its spinner and disables. It is
  // usually the git op, but Checkout and Create-branch are the *same* git op
  // ("checkout" with/without --create), so they pass distinct keys to keep one
  // from spinning the other's button (#247).
  const runGitOp = async (
    request: GitOpRequest,
    busyKey: string = request.op,
  ): Promise<GitOpResponse | null> => {
    setBusyAction(busyKey);
    setActionError(null);
    setActionInfo(null);
    try {
      const response = await api.gitOp(request);
      await Promise.all([
        statusRead.retry(),
        branchesRead.retry(),
        logRead.retry(),
      ]);
      setDiffRefreshKey((value) => value + 1);
      // A stage/commit/checkout can change the working tree the file tree
      // renders (and its Git markers) — reflect it there too (#238).
      bumpWorkspaceVersion();
      return response;
    } catch (e) {
      setActionError(formatApiError(e));
      return null;
    } finally {
      setBusyAction(null);
    }
  };

  // Success banners are transient: clear an `actionInfo` after a few seconds so
  // it does not linger as a stale claim after the state it described moved on
  // (#239). An error stays until the next action replaces it.
  createEffect(() => {
    if (!actionInfo()) return;
    const timer = window.setTimeout(() => setActionInfo(null), 4000);
    onCleanup(() => window.clearTimeout(timer));
  });

  // "Reveal in Git" from the editor stashes a path and routes here. Once this
  // repo's status is loaded, claim the path if it tracks it — selecting the
  // file so the reader lands on its diff. A path this repo does not track is
  // left pending for whichever repo does (#238).
  createEffect(() => {
    const pending = pendingRevealPath();
    if (!pending) return;
    const entries = status()?.entries;
    if (!entries) return;
    if (entries.some((entry) => entry.path === pending)) {
      setSelected(pending);
      consumePendingReveal();
    }
  });

  // Whether an arbitrary entry (not just the selected one) can be staged or
  // unstaged — for the per-row controls (#239).
  const entryStageable = (entry: GitStatusEntry) =>
    entry.kind === "untracked" || entry.worktree !== " ";
  const entryUnstageable = (entry: GitStatusEntry) =>
    entry.index !== " " && entry.index !== "?";

  const stageEntry = async (entry: GitStatusEntry) => {
    const response = await runGitOp({ op: "stage", repo: props.repo, path: entry.path });
    if (!response) return;
    // After staging, the file's changes live on the index — show that side of
    // the diff instead of leaving the reader on the now-empty Worktree view.
    setSelected(entry.path);
    setDiffStaged(true);
    setActionInfo(`Staged ${entry.path}`);
  };

  const unstageEntry = async (entry: GitStatusEntry) => {
    const response = await runGitOp({ op: "unstage", repo: props.repo, path: entry.path });
    if (!response) return;
    setSelected(entry.path);
    setDiffStaged(false);
    setActionInfo(`Unstaged ${entry.path}`);
  };

  const stageSelected = async () => {
    const entry = selectedEntry();
    if (!entry) return;
    await stageEntry(entry);
  };

  const unstageSelected = async () => {
    const entry = selectedEntry();
    if (!entry) return;
    await unstageEntry(entry);
  };

  const stageableEntries = createMemo(
    () => status()?.entries.filter(entryStageable) ?? [],
  );

  // Stage every currently-unstaged change in one go, refetching once at the end
  // rather than after each file (#239).
  const stageAll = async () => {
    const targets = stageableEntries();
    if (targets.length === 0) return;
    setBusyAction("stage");
    setActionError(null);
    setActionInfo(null);
    try {
      for (const entry of targets) {
        await api.gitOp({ op: "stage", repo: props.repo, path: entry.path });
      }
      await Promise.all([
        statusRead.retry(),
        branchesRead.retry(),
        logRead.retry(),
      ]);
      setDiffRefreshKey((value) => value + 1);
      bumpWorkspaceVersion();
      setDiffStaged(true);
      setActionInfo(
        `Staged ${targets.length} file${targets.length === 1 ? "" : "s"}`,
      );
    } catch (e) {
      setActionError(formatApiError(e));
    } finally {
      setBusyAction(null);
    }
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
    const response = await runGitOp(
      {
        op: "checkout",
        repo: props.repo,
        branch,
        create: true,
      },
      "create-branch",
    );
    if (!response) return;
    setNewBranch("");
    setBranchTarget(branch);
    setActionInfo(`Created and checked out ${branch}`);
  };

  const busy = (op: string) => busyAction() === op;

  const openRepository = (repo: string) => {
    void navigate(`/g/${encodeURIComponent(repo)}`);
  };

  return (
    <div class="git-shell">
      <div class="git-toolbar">
        <span class={`git-repo${status()?.repo || props.repo ? "" : " git-repo--placeholder"}`}>
          {status()?.repo || props.repo || "Choose a registered project"}
        </span>
        <Show when={props.repo}>
          <span class="git-branch">
            {"\u2387"} {notRepo()
              ? "Not a Git repository"
              : status()?.branch || (statusRead.loading() ? "Loading…" : "Unknown")}
            <Show when={status()?.ahead}>
              <span class="git-ab"> {"\u2191"}{status()!.ahead}</span>
            </Show>
            <Show when={status()?.behind}>
              <span class="git-ab"> {"\u2193"}{status()!.behind}</span>
            </Show>
          </span>
          <button
            onClick={() => void refresh()}
            disabled={busyAction() !== null || readsLoading()}
          >
            {busyAction() !== null
              ? "Working..."
              : readsLoading()
                ? "Refreshing..."
                : "\u27f3 Refresh"}
          </button>
        </Show>
      </div>

      <Show when={actionError()}>
        <div class="git-banner git-banner-error">{actionError()}</div>
      </Show>
      <Show when={!actionError() && actionInfo()}>
        <div class="git-banner git-banner-info">{actionInfo()}</div>
      </Show>
      <Show when={!props.repo}>
        <section class="git-repository-picker" aria-labelledby="git-repository-picker-title">
          <div>
            <p class="place-kicker">Registered scope</p>
            <h2 id="git-repository-picker-title">Choose a repository</h2>
            <p>
              Git operates only on projects already registered with Vogt. No
              workspace crawling or repository discovery is performed.
            </p>
          </div>
          <Show when={projectsRead.error()}>
            {(message) => (
              <div class="git-panel-error" role="alert">
                <span>
                  Registered projects {projectsRead.stale() ? "are stale" : "could not be read"}: {message()}
                </span>
                <button
                  type="button"
                  disabled={projectsRead.loading()}
                  onClick={() => void projectsRead.retry()}
                >
                  Retry projects
                </button>
              </div>
            )}
          </Show>
          <Show when={workspaceRead.error()}>
            {(message) => (
              <div class="git-panel-error" role="alert">
                <span>
                  Engine workspace {workspaceRead.stale() ? "is stale" : "could not be read"}: {message()}
                </span>
                <button
                  type="button"
                  disabled={workspaceRead.loading()}
                  onClick={() => void workspaceRead.retry()}
                >
                  Retry engine
                </button>
              </div>
            )}
          </Show>
          <Show when={projectsRead.loading() || workspaceRead.loading()}>
            <p class="meta">Loading registered projects and engine scope…</p>
          </Show>
          <Show when={projectsRead.data() && workspaceRead.data()}>
            <Show
              when={choices().length > 0}
              fallback={<p class="git-picker-empty">No projects are registered.</p>}
            >
              <div class="git-repository-choices">
                <For each={choices()}>
                  {(choice) => (
                    <button
                      type="button"
                      disabled={!choice.repo}
                      onClick={() => choice.repo && openRepository(choice.repo)}
                    >
                      <span>
                        <strong>{choice.name}</strong>
                        <small>{choice.slug}</small>
                      </span>
                      <code>{choice.repo ?? choice.root_path}</code>
                      <Show when={choice.unavailableReason}>
                        <small>{choice.unavailableReason}</small>
                      </Show>
                    </button>
                  )}
                </For>
              </div>
            </Show>
          </Show>
        </section>
      </Show>

      <Show when={props.repo}>
        <div class="git-body">
          <div class="git-left">
            <section
              class={`git-status-panel ${statusRead.stale() ? "stale" : ""}`}
              aria-label="Repository status"
            >
              <Show when={statusRead.error()}>
                {(message) => (
                  <div class="git-panel-error" role="alert">
                    <span>Status {statusRead.stale() ? "is stale" : "could not be read"}: {message()}</span>
                    <button
                      type="button"
                      disabled={statusRead.loading()}
                      onClick={() => void statusRead.retry()}
                    >
                      Retry status
                    </button>
                  </div>
                )}
              </Show>
              <Show when={statusRead.loading() && !status()}>
                <p class="git-panel-loading">Loading repository status…</p>
              </Show>
              <Show when={status()?.is_repo === false}>
                <div class="git-non-repository">
                  <strong>Not a Git repository</strong>
                  <span>
                    The selected registered project exists, but Git does not
                    identify it as a repository.
                  </span>
                  <button type="button" onClick={() => void navigate("/g")}>Choose another project</button>
                </div>
              </Show>
            </section>
            <section
              class={`git-branches-panel ${branchesRead.stale() ? "stale" : ""}`}
              aria-label="Repository workflow"
            >
              <div class="git-section-title">Workflow</div>
              <Show when={branchesRead.error()}>
                {(message) => (
                  <div class="git-panel-error" role="alert">
                    <span>
                      Branches {branchesRead.stale() ? "are stale" : "could not be read"}: {message()}
                    </span>
                    <button
                      type="button"
                      disabled={branchesRead.loading()}
                      onClick={() => void branchesRead.retry()}
                    >
                      Retry branches
                    </button>
                  </div>
                )}
              </Show>
              <div class="git-controls">
                <div class="git-control-block">
                  <div class="git-control-label">Branch</div>
                  <div class="git-control-row">
                    <select
                      value={branchTarget()}
                      disabled={notRepo() || busyAction() !== null || branchOptions().length === 0}
                      onInput={(event) => setBranchTarget(event.currentTarget.value)}
                    >
                    <Show
                      when={branchOptions().length > 0}
                      fallback={
                        <option value="">
                          {branchesRead.error() && !branches()
                            ? "Branch data unavailable"
                            : "No branches"}
                        </option>
                      }
                    >
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
                      {busy("create-branch") ? "Creating..." : "Create"}
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
                  <Show when={!notRepo() && stagedCount() === 0}>
                    <p class="meta git-commit-hint">
                      Stage a file to enable committing — use a row's + or Stage all.
                    </p>
                  </Show>
                </div>
              </div>
            </section>

            <section
              class={`git-status-panel ${statusRead.stale() ? "stale" : ""}`}
              aria-label="Working tree status"
            >
              <div class="git-section-title git-status-heading">
                <span>Status</span>
                <button
                  type="button"
                  class="git-stage-all"
                  onClick={() => void stageAll()}
                  disabled={notRepo() || busyAction() !== null || stageableEntries().length === 0}
                  title="Stage every unstaged change"
                >
                  {busy("stage") ? "Staging..." : "Stage all"}
                </button>
              </div>
              <Show
                when={!notRepo() && (status()?.entries.length ?? 0) > 0}
                fallback={
                  <div class="meta" style={{ padding: "8px 10px" }}>
                    {statusRead.loading() && !status()
                      ? "loading status…"
                      : statusRead.error() && !status()
                        ? "status unavailable"
                        : notRepo()
                          ? "selected path is not a Git repository"
                          : "clean working tree"}
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
                            class={`git-entry-row ${selected() === entry.path ? "selected" : ""}`}
                          >
                            <button
                              type="button"
                              class={`git-entry ${selected() === entry.path ? "selected" : ""}`}
                              onClick={() => setSelected(entry.path)}
                              title={`${entry.index}${entry.worktree} ${entry.path}`}
                            >
                              <span class={`git-badge git-${kind}`}>{kindBadge[kind]}</span>
                              <span class="git-path">{entry.path}</span>
                            </button>
                            <div class="git-entry-actions">
                              <Show when={entryStageable(entry)}>
                                <button
                                  type="button"
                                  class="git-entry-stage"
                                  aria-label={`Stage ${entry.path}`}
                                  title={`Stage ${entry.path}`}
                                  onClick={() => void stageEntry(entry)}
                                  disabled={busyAction() !== null}
                                >
                                  +
                                </button>
                              </Show>
                              <Show when={entryUnstageable(entry)}>
                                <button
                                  type="button"
                                  class="git-entry-unstage"
                                  aria-label={`Unstage ${entry.path}`}
                                  title={`Unstage ${entry.path}`}
                                  onClick={() => void unstageEntry(entry)}
                                  disabled={busyAction() !== null}
                                >
                                  {"−"}
                                </button>
                              </Show>
                            </div>
                          </div>
                        )}
                      </For>
                    </Show>
                  )}
                </For>
              </Show>
            </section>

            <section class={`git-log-panel ${logRead.stale() ? "stale" : ""}`} aria-label="Commit history">
              <div class="git-section-title">Recent commits</div>
              <Show when={logRead.error()}>
                {(message) => (
                  <div class="git-panel-error" role="alert">
                    <span>
                      Commit history {logRead.stale() ? "is stale" : "could not be read"}: {message()}
                    </span>
                    <button
                      type="button"
                      disabled={logRead.loading()}
                      onClick={() => void logRead.retry()}
                    >
                      Retry history
                    </button>
                  </div>
                )}
              </Show>
              <Show when={logRead.loading() && !log()}>
                <p class="git-panel-loading">Loading commit history…</p>
              </Show>
              <Show when={log() && log()!.length === 0}>
                <p class="git-panel-empty">No commits yet.</p>
              </Show>
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
            </section>
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
                      onClick={() => {
                        openEditorTab(entry().path);
                        void navigate(`/e/${encodeURIComponent(entry().path)}`);
                      }}
                      title="Open this file in the editor"
                    >
                      Open file
                    </button>
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
                  <DiffView
                    repo={props.repo}
                    path={entry().path}
                    staged={diffStaged()}
                    refreshKey={diffRefreshKey()}
                  />
                </>
              )}
            </Show>
          </div>
        </div>
      </Show>
    </div>
  );
};

export default GitTab;
