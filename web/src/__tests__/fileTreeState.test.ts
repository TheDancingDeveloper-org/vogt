import { describe, expect, it, beforeEach } from "vitest";
import { createRoot } from "solid-js";
import {
  expandedPaths,
  fileTreeSearch,
  folderVersion,
  invalidateFolder,
  isExpanded,
  parentFolder,
  resetFileTreeState,
  setExpanded,
  setFileTreeSearch,
  setSidebarCollapsed,
  sidebarCollapsed,
} from "../fileTreeState";
import { bumpWorkspaceVersion, workspaceVersion } from "../workspaceVersion";

beforeEach(() => {
  localStorage.clear();
  resetFileTreeState();
});

describe("fileTreeState — persistent tree view state (#238)", () => {
  it("remembers folder expansion and survives a remount (re-read from storage)", () => {
    setExpanded("src", true);
    setExpanded("src/components", true);
    expect(isExpanded("src")).toBe(true);
    expect(isExpanded("src/components")).toBe(true);
    expect(isExpanded("docs")).toBe(false);

    // A remount clears the in-memory store and re-reads localStorage — the
    // expansion the reader had must come back rather than collapsing (#238).
    resetFileTreeState();
    expect(isExpanded("src")).toBe(true);
    expect(isExpanded("src/components")).toBe(true);
    expect(expandedPaths().sort()).toEqual(["src", "src/components"]);

    setExpanded("src", false);
    expect(isExpanded("src")).toBe(false);
    expect(expandedPaths()).toEqual(["src/components"]);
  });

  it("hoists the search query and the sidebar collapse across the module", () => {
    expect(fileTreeSearch()).toBe("");
    setFileTreeSearch("widget");
    expect(fileTreeSearch()).toBe("widget");

    expect(sidebarCollapsed()).toBe(false);
    setSidebarCollapsed(true);
    expect(sidebarCollapsed()).toBe(true);
    // The sidebar state is durable; the search query is intentionally not.
    resetFileTreeState();
    expect(sidebarCollapsed()).toBe(true);
    expect(fileTreeSearch()).toBe("");
  });

  it("derives a path's parent folder for targeted refetch", () => {
    expect(parentFolder("src/components/Button.tsx")).toBe("src/components");
    expect(parentFolder("README.md")).toBe("");
    expect(parentFolder("src/")).toBe("");
  });

  it("invalidates only the affected folder, not its siblings (#238)", () => {
    createRoot((dispose) => {
      // Track each folder's version the way a TreeNodeView effect would.
      const before = {
        root: folderVersion(""),
        src: folderVersion("src"),
        docs: folderVersion("docs"),
      };
      invalidateFolder("src");
      expect(folderVersion("src")).toBe(before.src + 1);
      // A sibling and the root are untouched: their loaded, expanded state
      // stands rather than collapsing on an unrelated op.
      expect(folderVersion("docs")).toBe(before.docs);
      expect(folderVersion("")).toBe(before.root);
      dispose();
    });
  });
});

describe("workspaceVersion — cross-surface invalidation (#238)", () => {
  it("advances monotonically on every bump", () => {
    createRoot((dispose) => {
      const start = workspaceVersion();
      bumpWorkspaceVersion();
      expect(workspaceVersion()).toBe(start + 1);
      bumpWorkspaceVersion();
      expect(workspaceVersion()).toBe(start + 2);
      dispose();
    });
  });
});
