// The session menu's role row (WI-1091): "Make oversight" on a worker,
// "Remove oversight" on an oversight session. One source for the rail's row
// menu and the terminal's "⋯" menu (the phone header's too), so both offer
// the same thing and the unit test pins it without mounting either.
//
// The row calls `session.set_role` through the core, which only a person may
// do (an agent is refused there, WI-957). Removing oversight also lifts the
// pin oversight set, so the session falls back to the ordinary idle policy.

import type { SessionSummary } from "./api";
import { isOversight } from "./sessionRowModel";

export interface SessionRoleAction {
  /** The role the row sets. */
  role: "oversight" | "worker";
  label: "Make oversight" | "Remove oversight";
  title: string;
}

/** The role row for `session`, or null where a role change means nothing. */
export function sessionRoleAction(
  session: Pick<SessionSummary, "role" | "exit_code"> | null | undefined,
): SessionRoleAction | null {
  // An exited session supervises nothing; a hibernated one keeps its role in
  // the engine's record, so it may still be nominated or demoted.
  if (!session || session.exit_code !== null) return null;
  if (isOversight(session)) {
    return {
      role: "worker",
      label: "Remove oversight",
      title:
        "Make it an ordinary session again: listed with the others, and no longer pinned awake",
    };
  }
  return {
    role: "oversight",
    label: "Make oversight",
    title: "An oversight session supervises the others: it is pinned awake and listed first",
  };
}
