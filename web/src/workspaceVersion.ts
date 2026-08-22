// A single monotonic counter that says "something in the workspace changed":
// a file was saved, a Git op ran, a file was created. Surfaces that render a
// view of the workspace (the file tree and its Git markers) read it and
// refetch when it moves, so an edit made in one tab is reflected in the tree
// without the reader hitting Refresh (#238).
//
// It is deliberately thin — like `api.ServerEvent`'s `vogt-changed`, it says
// there is something to re-read, not what. Module-singleton state so a bump
// from any surface is seen by every mounted reader.
import { createSignal } from "solid-js";

const [version, setVersion] = createSignal(0);

/** Read the current workspace version. Track it in a resource/effect to
 *  refetch whenever the workspace changes underfoot. */
export const workspaceVersion = version;

/** Announce that the workspace changed on disk (a save, a Git op, a create). */
export function bumpWorkspaceVersion(): void {
  setVersion((n) => n + 1);
}
