import { MemoryRouter, Route, createMemoryHistory } from "@solidjs/router";
import { fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../monaco", () => ({
  languageFor: () => "typescript",
  loadMonaco: async () => ({
    editor: {
      createDiffEditor: () => ({
        dispose: vi.fn(),
        layout: vi.fn(),
        setModel: vi.fn(),
      }),
      createModel: () => ({ dispose: vi.fn() }),
    },
  }),
}));

import GitTab from "../Git";
import { api, type GitStatusResp } from "../api";
import { repositoryChoices } from "../gitRepositories";
import * as vogtApi from "../vogtApi";

const STATUS: GitStatusResp = {
  repo: "vogt",
  is_repo: true,
  branch: "main",
  ahead: 0,
  behind: 0,
  entries: [
    { path: "src/App.tsx", index: " ", worktree: "M", kind: "modified" },
  ],
};

const CLEAN_STATUS: GitStatusResp = { ...STATUS, entries: [] };

function renderGit(repo = "vogt") {
  const history = createMemoryHistory();
  history.set({ value: repo ? `/g/${repo}` : "/g" });
  const rendered = render(() => (
    <MemoryRouter history={history}>
      <Route path="*" component={() => <GitTab repo={repo} />} />
    </MemoryRouter>
  ));
  return { ...rendered, history };
}

async function refresh(): Promise<void> {
  const button = await screen.findByRole("button", { name: /Refresh/ });
  await waitFor(() => expect(button).toBeEnabled());
  await fireEvent.click(button);
}

beforeEach(() => {
  vi.spyOn(api, "gitStatus").mockResolvedValue(STATUS);
  vi.spyOn(api, "gitBranch").mockResolvedValue({ current: "main", all: ["main"] });
  vi.spyOn(api, "gitLog").mockResolvedValue([
    {
      hash: "1111111111111111111111111111111111111111",
      short: "1111111",
      author: "Ada",
      date: "2026-08-18T08:00:00Z",
      subject: "first answer",
    },
  ]);
});

describe("Git repository selection", () => {
  it("maps only registered projects inside the engine workspace", () => {
    expect(repositoryChoices([
      { slug: "estate", name: "Estate", root_path: "/workspace" },
      { slug: "vogt", name: "Vogt", root_path: "/workspace/apps/vogt/" },
      { slug: "outside", name: "Outside", root_path: "/srv/outside" },
      { slug: "escape", name: "Escape", root_path: "/workspace/../srv/escape" },
      { slug: "unknown", name: "Unknown", root_path: "" },
    ], "/workspace/")).toEqual([
      { slug: "estate", name: "Estate", root_path: "/workspace", repo: ".", unavailableReason: null },
      {
        slug: "vogt", name: "Vogt", root_path: "/workspace/apps/vogt/",
        repo: "apps/vogt", unavailableReason: null,
      },
      {
        slug: "outside", name: "Outside", root_path: "/srv/outside",
        repo: null, unavailableReason: "Outside this engine workspace",
      },
      {
        slug: "escape", name: "Escape", root_path: "/workspace/../srv/escape",
        repo: null, unavailableReason: "Outside this engine workspace",
      },
      {
        slug: "unknown", name: "Unknown", root_path: "",
        repo: null,
        unavailableReason: "Project or engine workspace path is unavailable",
      },
    ]);
  });

  it("shows registered choices on /g and makes the selected path addressable", async () => {
    vi.spyOn(vogtApi, "listProjects").mockResolvedValue({
      projects: [
        { slug: "vogt", name: "Vogt", root_path: "/workspace/apps/vogt" },
        { slug: "remote", name: "Remote", root_path: "/srv/remote" },
      ],
      total: 2,
    });
    vi.spyOn(api, "operationalStatus").mockResolvedValue({
      storage: { workspace_root: "/workspace", state_dir: "/state" },
    } as Awaited<ReturnType<typeof api.operationalStatus>>);

    const { history } = renderGit("");

    expect(await screen.findByRole("heading", { name: "Choose a repository" })).toBeVisible();
    expect(api.gitStatus).not.toHaveBeenCalled();
    const outside = await screen.findByRole("button", { name: /Remote/ });
    expect(outside).toBeDisabled();
    expect(outside).toHaveTextContent("Outside this engine workspace");

    await fireEvent.click(screen.getByRole("button", { name: /Vogt/ }));
    await waitFor(() => expect(history.get()).toBe("/g/apps%2Fvogt"));
  });
});

describe("Git read truth", () => {
  it("keeps status content stale after failure and retries locally", async () => {
    vi.mocked(api.gitStatus)
      .mockResolvedValueOnce(STATUS)
      .mockRejectedValueOnce(new Error("status offline"))
      .mockResolvedValueOnce(CLEAN_STATUS);
    renderGit();

    expect(await screen.findByText("src/App.tsx")).toBeVisible();
    await refresh();
    expect(await screen.findByText(/Status is stale: status offline/)).toBeVisible();
    expect(screen.getByText("src/App.tsx")).toBeVisible();

    await fireEvent.click(screen.getByRole("button", { name: "Retry status" }));
    await waitFor(() => expect(screen.queryByText(/status offline/)).not.toBeInTheDocument());
    expect(screen.getByText("clean working tree")).toBeVisible();
  });

  it("keeps branch content stale after failure and retries locally", async () => {
    vi.mocked(api.gitBranch)
      .mockResolvedValueOnce({ current: "main", all: ["main"] })
      .mockRejectedValueOnce(new Error("branch offline"))
      .mockResolvedValueOnce({ current: "dev", all: ["dev"] });
    renderGit();

    expect(await screen.findByRole("option", { name: "main" })).toBeVisible();
    await refresh();
    expect(await screen.findByText(/Branches are stale: branch offline/)).toBeVisible();
    expect(screen.getByRole("option", { name: "main" })).toBeVisible();

    await fireEvent.click(screen.getByRole("button", { name: "Retry branches" }));
    expect(await screen.findByRole("option", { name: "dev" })).toBeVisible();
    expect(screen.queryByText(/branch offline/)).not.toBeInTheDocument();
  });

  it("keeps commit history stale after failure and retries locally", async () => {
    vi.mocked(api.gitLog)
      .mockResolvedValueOnce([
        { hash: "1", short: "1", author: "Ada", date: "2026-08-18T08:00:00Z", subject: "first answer" },
      ])
      .mockRejectedValueOnce(new Error("log offline"))
      .mockResolvedValueOnce([
        { hash: "2", short: "2", author: "Lin", date: "2026-08-18T09:00:00Z", subject: "recovered answer" },
      ]);
    renderGit();

    expect(await screen.findByText("first answer")).toBeVisible();
    await refresh();
    expect(await screen.findByText(/Commit history is stale: log offline/)).toBeVisible();
    expect(screen.getByText("first answer")).toBeVisible();

    await fireEvent.click(screen.getByRole("button", { name: "Retry history" }));
    expect(await screen.findByText("recovered answer")).toBeVisible();
    expect(screen.queryByText("first answer")).not.toBeInTheDocument();
  });

  it("keeps a loaded diff stale after failure and recovers through its Retry", async () => {
    vi.spyOn(api, "gitDiff")
      .mockResolvedValueOnce({ path: "src/App.tsx", head: "old", current: "new" })
      .mockRejectedValueOnce(new Error("diff offline"))
      .mockResolvedValueOnce({ path: "src/App.tsx", head: "old", current: "recovered" });
    renderGit();

    await fireEvent.click((await screen.findByText("src/App.tsx")).closest("button")!);
    await waitFor(() => expect(api.gitDiff).toHaveBeenCalledTimes(1));
    await refresh();
    expect(await screen.findByText(/Diff is stale: diff offline/)).toBeVisible();
    expect(document.querySelector(".diff-shell.stale")).toBeTruthy();
    await fireEvent.click(screen.getByRole("button", { name: "Retry diff" }));
    await waitFor(() => expect(screen.queryByText(/diff offline/)).not.toBeInTheDocument());
    expect(api.gitDiff).toHaveBeenCalledTimes(3);
  });

  it("names successful zero-result states instead of presenting an outage", async () => {
    vi.mocked(api.gitStatus).mockResolvedValue(CLEAN_STATUS);
    vi.mocked(api.gitBranch).mockResolvedValue({ current: "", all: [] });
    vi.mocked(api.gitLog).mockResolvedValue([]);
    renderGit();

    expect(await screen.findByText("clean working tree")).toBeVisible();
    expect(screen.getByRole("option", { name: "No branches" })).toBeVisible();
    expect(screen.getByText("No commits yet.")).toBeVisible();
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });

  it("keeps a selected non-repository distinct from a missing path", async () => {
    vi.mocked(api.gitStatus).mockResolvedValue({
      repo: "plain",
      is_repo: false,
      branch: "",
      ahead: 0,
      behind: 0,
      entries: [],
    });
    renderGit("plain");
    expect(await screen.findByText("Not a Git repository")).toBeVisible();
    expect(screen.queryByText(/Status could not be read/)).not.toBeInTheDocument();
  });

  it("renders a missing path as an error rather than a clean or non-repository answer", async () => {
    vi.mocked(api.gitStatus).mockRejectedValue(new Error("path not found"));
    renderGit("missing");

    expect(await screen.findByText(/Status could not be read: path not found/)).toBeVisible();
    expect(screen.getByRole("button", { name: "Retry status" })).toBeVisible();
    expect(screen.getByText("status unavailable")).toBeVisible();
    expect(screen.queryByText("clean working tree")).not.toBeInTheDocument();
    expect(screen.queryByText("Not a Git repository")).not.toBeInTheDocument();
  });
});

describe("#247 — branch-control polish", () => {
  it("gives Create-branch a busy key distinct from Checkout", async () => {
    vi.mocked(api.gitBranch).mockResolvedValue({
      current: "main",
      all: ["main", "feature"],
    });
    // A create in flight that never settles, so both buttons can be inspected
    // mid-operation.
    let release!: () => void;
    vi.spyOn(api, "gitOp").mockReturnValue(
      new Promise(() => {
        release = () => {};
      }) as never,
    );
    void release;
    const { container } = renderGit();
    await waitFor(() =>
      expect(
        container.querySelector('input[placeholder="new branch"]'),
      ).toBeTruthy(),
    );

    const newBranch = container.querySelector<HTMLInputElement>('input[placeholder="new branch"]')!;
    await fireEvent.input(newBranch, { target: { value: "spike" } });
    const createButton = [...container.querySelectorAll("button")].find(
      (b) => b.textContent === "Create",
    )!;
    await fireEvent.click(createButton);

    // The Create button reflects its own operation; the Checkout button, which
    // runs the same git "checkout" op, does not borrow the spinner.
    await waitFor(() =>
      expect(
        [...container.querySelectorAll("button")].some((b) => b.textContent === "Creating..."),
      ).toBe(true),
    );
    const checkoutButton = [...container.querySelectorAll("button")].find(
      (b) => b.textContent === "Checkout" || b.textContent === "Switching...",
    );
    expect(checkoutButton?.textContent).toBe("Checkout");
  });

  it("styles the empty-repo breadcrumb as a kicker, not a monospace path strip", async () => {
    vi.spyOn(vogtApi, "listProjects").mockResolvedValue({ projects: [], total: 0 });
    vi.spyOn(api, "operationalStatus").mockResolvedValue({
      storage: { workspace_root: "/workspace", state_dir: "/state" },
    } as Awaited<ReturnType<typeof api.operationalStatus>>);
    const { container } = renderGit("");

    const breadcrumb = container.querySelector(".git-repo")!;
    expect(breadcrumb.textContent).toBe("Choose a registered project");
    expect(breadcrumb.classList.contains("git-repo--placeholder")).toBe(true);
  });
});
