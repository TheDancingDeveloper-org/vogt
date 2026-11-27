import { Route, Router } from "@solidjs/router";
import { fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vitest";
import { api, ApiError, fileBase, isSessionHibernatedError } from "../api";
import FileTree from "../FileTree";

const hibernated = () =>
  new ApiError(409, JSON.stringify({ error: "hibernated", code: "session_hibernated" }));

describe("session-scoped Files (WI-4868)", () => {
  afterEach(() => {
    vi.restoreAllMocks();
    vi.useRealTimers();
  });

  it("builds the session route prefix and keeps the global one", () => {
    expect(fileBase("abc")).toBe("/api/sessions/abc");
    expect(fileBase(null)).toBe("/api");
    expect(fileBase()).toBe("/api");
  });

  it("recognises only 409 session_hibernated", () => {
    expect(isSessionHibernatedError(hibernated())).toBe(true);
    expect(isSessionHibernatedError(new ApiError(409, "{}"))).toBe(false);
    expect(isSessionHibernatedError(new ApiError(500, "nope"))).toBe(false);
  });

  it("lists the selected session's tree", async () => {
    const tree = vi.spyOn(api, "tree").mockResolvedValue([
      { name: "pod.txt", path: "pod.txt", is_dir: false },
    ]);
    vi.spyOn(api, "gitStatus").mockResolvedValue({
      repo: "", is_repo: false, branch: "", ahead: 0, behind: 0, entries: [],
    });
    render(() => (
      <Router>
        <Route path="*" component={() => <FileTree alwaysExpanded sessionId={() => "s1"} />} />
      </Router>
    ));
    expect(await screen.findByText("pod.txt")).toBeVisible();
    expect(tree).toHaveBeenCalledWith("", 0, undefined, "s1");
  });

  it("shows the hibernated state with a Wake button", async () => {
    vi.spyOn(api, "tree").mockRejectedValue(hibernated());
    vi.spyOn(api, "gitStatus").mockResolvedValue({
      repo: "", is_repo: false, branch: "", ahead: 0, behind: 0, entries: [],
    });
    const onWake = vi.fn();
    render(() => (
      <Router>
        <Route
          path="*"
          component={() => <FileTree alwaysExpanded sessionId={() => "s1"} onWake={onWake} />}
        />
      </Router>
    ));
    expect(await screen.findByText("Session is hibernated: wake to browse")).toBeVisible();
    fireEvent.click(screen.getByRole("button", { name: "Wake" }));
    expect(onWake).toHaveBeenCalledWith("s1");
  });

  it("re-reads the session tree every few seconds", async () => {
    const tree = vi.spyOn(api, "tree").mockResolvedValue([]);
    vi.spyOn(api, "gitStatus").mockResolvedValue({
      repo: "", is_repo: false, branch: "", ahead: 0, behind: 0, entries: [],
    });
    vi.useFakeTimers({ shouldAdvanceTime: true });
    render(() => (
      <Router>
        <Route path="*" component={() => <FileTree alwaysExpanded sessionId={() => "s1"} />} />
      </Router>
    ));
    await waitFor(() => expect(tree).toHaveBeenCalledTimes(1));
    await vi.advanceTimersByTimeAsync(4100);
    await waitFor(() => expect(tree.mock.calls.length).toBeGreaterThan(1));
  });
});
