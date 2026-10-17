// A running-sessions list, shared by the desktop Sessions overview and
// the phone Sessions body.
//
// The rows are the same `.session-row` grammar the desktop Places rail draws,
// minus the rail's per-row action menu: this list exists so idle and busy
// sessions are reachable from the surface itself, not to be a second control
// centre. Each row is a plain hash link to the session's terminal, so it needs
// no router of its own and a test can read where it points from the `href`.
//
// The list is sectioned the way the rail is (WI-1736): Needs you, Running,
// Ready / idle, Done recently, and a collapsed Older. A section header folds
// its rows, using the same open state the rail keeps.

import { For, Show, createMemo, type Component } from "solid-js";
import type { SessionSummary } from "./api";
import { createNow } from "./viewAge";
import { isConnected, sessionsStore } from "./store";
import {
  activityClass,
  activityLabel,
  isOversight,
  sessionStateWord,
  permissionPostureLabel,
  sessionRuntimeHint,
} from "./sessionRowModel";
import {
  buildRailModel,
  flattenRail,
  type RailEntry,
  type RailHeadEntry,
} from "./sessionRail";
import { railSections, setRailSection } from "./railSections";

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
  const entries = createMemo<RailEntry[]>(() => {
    const omit = new Set(props.omit ?? []);
    const model = buildRailModel(
      props.sessions.filter((session) => !omit.has(session.id)),
      { now: now() },
    );
    return flattenRail(model, {
      open: (id) => id === "pinned" || railSections[id],
      expanded: () => true,
    });
  });

  const sectionHead = (entry: RailHeadEntry) => (
    <li class="session-list-section">
      <button
        type="button"
        class="session-list-head"
        aria-expanded={railSections[entry.id]}
        onClick={() => setRailSection(entry.id, !railSections[entry.id])}
      >
        <span aria-hidden="true">{railSections[entry.id] ? "▾" : "▸"}</span>
        <span>{entry.title}</span>
        <span class="session-list-count">{entry.count}</span>
      </button>
    </li>
  );

  const sessionRow = (session: SessionSummary) => (
    <li>
      <a
        class={`session-row ${activityClass(session)}${session.exit_code === null && (session.activity === "waiting-for-input" || session.activity === "awaiting-approval") ? " waiting" : ""}${sessionsStore.ready && !isConnected() ? " session-row--stale" : ""}`}
        href={`#/t/${session.id}`}
        aria-label={`${session.name}${isOversight(session) ? ", oversight" : ""}, ${activityLabel(session.activity, session.exit_code)}`}
        title={`${session.name}\ncwd: ${session.cwd}${sessionRuntimeHint(session) ? `\n${sessionRuntimeHint(session)}` : ""}`}
      >
        <span
          class={`activity-dot ${activityClass(session)}`}
          aria-hidden="true"
        />
        <div class="session-row-body">
          <span class="name">
            <Show when={isOversight(session)}>
              <span class="session-role-badge" title="Oversight session: supervises the others; pinned awake">oversight</span>
            </Show>
            {session.name}
            <Show when={session.work_item}>
              {(ref) => (
                <span class="session-work-chip">{ref()}</span>
              )}
            </Show>
            <Show when={permissionPostureLabel(session.permission_mode)}>
              {(label) => (
                <span
                  class={`session-posture${session.permission_mode === "bypass" ? " session-posture--bypass" : ""}`}
                  title={session.permission_mode === "bypass" ? "Started with no permission checks" : "Edits auto-accepted"}
                >
                  {label()}
                </span>
              )}
            </Show>
          </span>
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
  );

  return (
    <Show when={props.sessions.length > 0}>
      <ul class="session-list" aria-label={props.label ?? "Running sessions"}>
        <For each={entries()}>
          {(entry) => (entry.kind === "head" ? sectionHead(entry) : sessionRow(entry.session))}
        </For>
      </ul>
    </Show>
  );
};

export default SessionList;
