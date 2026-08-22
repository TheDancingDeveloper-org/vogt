// Cross-link from the editor to Git: "Reveal in Git" on a file stashes its
// path here and jumps to the Git repository picker. When a repository that
// tracks the path is opened, the Git tab consumes the pending path and selects
// the file, so a reveal lands the reader on that file's diff (#238).
//
// The path is kept, not the repository, because the editor does not know which
// registered project a file belongs to — the Git tab, which reads status per
// repo, is the surface that can match it.
import { createSignal } from "solid-js";

const [pendingRevealPath, setPendingRevealPath] = createSignal<string | null>(null);

export { pendingRevealPath };

/** Ask Git to reveal `path`: remember it and route to the Git picker. */
export function revealPathInGit(path: string): void {
  setPendingRevealPath(path);
  // Hash router: the repository picker. Choosing a repo that tracks the path
  // lets the Git tab consume it below.
  window.location.hash = "#/g";
}

/** Read and clear the pending reveal path (the Git tab, once it has a match). */
export function consumePendingReveal(): string | null {
  const path = pendingRevealPath();
  if (path !== null) setPendingRevealPath(null);
  return path;
}
