/**
 * How many parked panes may open an attach socket on load (WI-1413).
 *
 * Every retained terminal tab mounts, and each one used to open its socket
 * immediately — even a pane that was about to park — so a long-lived tab with
 * dozens of sessions opened dozens of sockets, PTY readers and subscribers in
 * one tick. Only this many parked panes in workspaces that are not on screen
 * attach on load; the rest attach the first time they are viewed. Which of
 * them get a slot is decided by mount order (`initialAttachSlots`), because a
 * pane mounts on its own and cannot see the others.
 */

/** Parked panes warmed on load, on top of whatever is currently visible. */
export const MAX_INITIAL_PARKED_ATTACHES = 3;
