// A running-sessions list, shared by the desktop Sessions overview (#233) and
// the phone Sessions body (#231's reachability half).
//
// The rows are the same `.session-row` grammar the desktop Places rail draws,
// minus the rail's per-row action menu: this list exists so idle and busy
// sessions are reachable from the surface itself, not to be a second control
// centre. Each row is a plain hash link to the session's terminal, so it needs
// no router of its own and a test can read where it points from the `href`.

import { For, Show, createMemo, type Component } from "solid-js";
import type { SessionSummary } from "./api";
import { createNow } from "./viewAge";
import { isConnected, sessionsStore } from "./store";
import {
  activityClass,
  activityLabel,
  sessionStateWord,
  sortSessionsByAttention,
} from "./sessionRowModel";

interface Props {
  sessions: SessionSummary[];
  /** Session ids to leave out — e.g. the waiting sessions already shown above
   *  as attention cards, or the one already open in the terminal. */
  omit?: string[];
  /** Accessible name for the list; the surfaces phrase it for their context. */
  label?: string;
}

export const SessionList: Component<Props> = (props) => {
  const now = createNow(30_000);
  const rows = createMemo(() => {
    const omit = new Set(props.omit ?? []);
    return sortSessionsByAttention(
      props.sessions.filter((session) => !omit.has(session.id)),
    );
  });

  return (
    <Show when={rows().length > 0}>
      <ul class="session-list" aria-label={props.label ?? "Running sessions"}>
        <For each={rows()}>
          {(session) => (
            <li>
              <a
                class={`session-row ${activityClass(session)}${session.exit_code === null && session.activity === "waiting-for-input" ? " waiting" : ""}${sessionsStore.ready && !isConnected() ? " session-row--stale" : ""}`}
                href={`#/t/${session.id}`}
                aria-label={`${session.name}, ${activityLabel(session.activity, session.exit_code)}`}
                title={`${session.name}\ncwd: ${session.cwd}`}
              >
                <span
                  class={`activity-dot ${activityClass(session)}`}
                  aria-hidden="true"
                />
                <div class="session-row-body">
                  <span class="name">{session.name}</span>
                  <span class="session-row-meta">
                    <span
                      class={`state state--${activityClass(session)}`}
                    >
                      {sessionStateWord(session, now(), sessionsStore.ready && !isConnected() ? sessionsStore.lastAnswerAt : null)}
                    </span>
                    <Show when={session.cwd}>
                      <span class="cwd"> · {session.cwd}</span>
                    </Show>
                  </span>
                </div>
                <span class="session-row-chevron" aria-hidden="true">›</span>
              </a>
            </li>
          )}
        </For>
      </ul>
    </Show>
  );
};

export default SessionList;
