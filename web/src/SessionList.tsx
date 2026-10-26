// A running-sessions list, shared by the desktop Sessions overview and
// the phone Sessions body.
//
// The rows are the same `.session-row` grammar the desktop Places rail draws,
// minus the rail's per-row action menu: this list exists so idle and busy
// sessions are reachable from the surface itself, not to be a second control
// centre. Each row is a plain hash link to the session's terminal, so it needs
// no router of its own and a test can read where it points from the `href`.
//
// The list groups sessions the way the rail does (WI-1736): a filter, the
// Pinned strip, the state sections with their counts, workers nested under the
// oversight session that started them, and a child that needs a person lifted
// into Needs you. The grouping is `buildRailModel`, so the two surfaces cannot
// drift apart.

import { createRailKeys, createRailRowFields, type RailRowFields } from "./railRowFields";
import { For, Show, createEffect, createMemo, createSignal, on, type Component } from "solid-js";
import type { SessionSummary } from "./api";
import { createNow } from "./viewAge";
import { bookmarks } from "./bookmarks";
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
  railBadges,
  railFinished,
  type RailEntry,
  type RailHeadEntry,
} from "./sessionRail";
import { noteRailSessions, railStarters } from "./railStarters";
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
  const [query, setQuery] = createSignal("");
  const [folded, setFolded] = createSignal<ReadonlySet<string>>(new Set());

  // Who started whom is read once for every list on the screen (railStarters).
  createEffect(on(() => props.sessions.map((s) => s.id).join("\n"), noteRailSessions));

  const entries = createMemo<RailEntry[]>((previous) => {
    const omit = new Set(props.omit ?? []);
    const model = buildRailModel(
      props.sessions.filter((session) => !omit.has(session.id)),
      {
        now: now(),
        bookmarked: new Set(bookmarks()),
        starters: railStarters(),
        collapsed: folded(),
        query: query(),
        hiddenCount: sessionsStore.hiddenCount,
      },
    );
    return flattenRail(
      model,
      {
        open: (id) => (id === "pinned" ? railSections.pinned : railSections[id]),
        expanded: (id) => !folded().has(id),
      },
      previous,
    );
  }, []);

  // Keyed rows: see railRowFields.ts (WI-2328).
  const { byKey, keys } = createRailKeys(entries);

  const toggleFolded = (id: string) =>
    setFolded((current) => {
      const next = new Set(current);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });

  const sectionHead = (entry: RailHeadEntry) => (
    <li class="session-list-section">
      <button
        type="button"
        class="session-list-head"
        aria-expanded={entry.id === "pinned" ? railSections.pinned : railSections[entry.id]}
        onClick={() => {
          const open = entry.id === "pinned" ? railSections.pinned : railSections[entry.id];
          setRailSection(entry.id, !open);
          // Opening Older pages in what the engine hid, so its rows exist to show.
          if (!open && entry.id === "older" && !railSections.showHidden) setRailSection("showHidden", true);
        }}
      >
        <span aria-hidden="true">{(entry.id === "pinned" ? railSections.pinned : railSections[entry.id]) ? "▾" : "▸"}</span>
        <span>{entry.title}</span>
        <span class="session-list-count">{entry.count}</span>
        <Show when={entry.needs > 0}>
          <span class="session-list-needs" title={`${entry.needs} pinned ${entry.needs === 1 ? "session needs" : "sessions need"} you`}>
            {entry.needs} need you
          </span>
        </Show>
      </button>
    </li>
  );

  const sessionRow = (row: RailRowFields) => {
    const session = row.session();
    return (
    <li>
      <a
        class={`session-row ${activityClass(session)}${row.depth() === 1 ? " session-row--child" : ""}${row.section() === "pinned" && railFinished(session) ? " session-row--dimmed" : ""}${session.exit_code === null && (session.activity === "waiting-for-input" || session.activity === "awaiting-approval") ? " waiting" : ""}${sessionsStore.ready && !isConnected() ? " session-row--stale" : ""}`}
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
            <Show when={row.tally()}>
              <button
                type="button"
                class="session-caret"
                aria-expanded={row.expanded() === true}
                aria-label={`${row.expanded() ? "Fold" : "Show"} the sessions started by ${session.name}`}
                onClick={(event) => {
                  event.preventDefault();
                  event.stopPropagation();
                  toggleFolded(session.id);
                }}
                onKeyDown={(event) => event.stopPropagation()}
              >{row.expanded() ? "▾" : "▸"}</button>
            </Show>
            <Show when={isOversight(session)}>
              <span class="session-role-badge" title="Oversight session: supervises the others; pinned awake">oversight</span>
            </Show>
            <Show when={row.lifted()}>
              {(parent) => (
                <span class="session-lift" title={`Started by ${parent()}`}>↳ {parent()}</span>
              )}
            </Show>
            {session.name}
            <Show when={row.tally()}>
              {(tally) => (
                <span
                  class="session-tally"
                  title={`${tally().total} started by ${session.name}: ${tally().running} running, ${tally().needs} need you, ${tally().done} done`}
                >
                  {tally().running}▶ {tally().needs}? {tally().done}✓
                </span>
              )}
            </Show>
            {session.work_item ? <span class="session-work-chip">{session.work_item}</span> : null}
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
            <span class="session-badges">
              <For each={railBadges(session, now())}>
                {(badge) => (
                  <span class="session-badge" title={badge.label} aria-label={badge.label}>{badge.glyph}</span>
                )}
              </For>
            </span>
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
  };

  return (
    <Show when={props.sessions.length > 0}>
      <form
        role="search"
        class="rail-session-tools"
        onSubmit={(event) => event.preventDefault()}
      >
        <input
          type="search"
          class="rail-search"
          aria-label="Filter sessions"
          placeholder="Filter sessions"
          data-rail-search
          value={query()}
          onInput={(event) => setQuery(event.currentTarget.value)}
          onKeyDown={(event) => {
            if (event.key !== "Escape") return;
            setQuery("");
            event.currentTarget.blur();
          }}
        />
      </form>
      <ul class="session-list" aria-label={props.label ?? "Running sessions"}>
        <For each={keys()} fallback={<li class="empty">No sessions match.</li>}>
          {(key) => (key.startsWith("head:")
            ? <Show when={byKey().get(key)} keyed>{(entry) => entry.kind === "head" && sectionHead(entry)}</Show>
            : sessionRow(createRailRowFields(byKey, key)))}
        </For>
      </ul>
    </Show>
  );
};

export default SessionList;
