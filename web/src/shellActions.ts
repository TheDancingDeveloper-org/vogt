// Shell-level actions a surface may offer without owning them (phone uplift).
//
// The command palette belongs to the shell in `App.tsx`; a surface header,
// the terminal's phone header and the assistant's head each want a "Go to…"
// control in their own top row so a phone does not spend a full row on one
// button above every screen. They ask the shell through this context rather
// than each importing a signal, so a surface rendered outside the shell (a
// unit test, a storybook-style harness) simply renders no such control.

import { createContext, useContext } from "solid-js";

export interface ShellActions {
  /** Open the command palette ("Go to…"). */
  openCommandPalette: () => void;
}

export const ShellActionsContext = createContext<ShellActions>();

/** The shell's actions, or `undefined` outside the shell. */
export function useShellActions(): ShellActions | undefined {
  return useContext(ShellActionsContext);
}
