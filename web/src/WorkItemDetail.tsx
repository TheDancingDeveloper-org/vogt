// One work item, in full (FR-U5), with the sessions running for it (FR-U20).
//
// The surface exists to make four distinctions visible, because each of them
// is a place where a plausible-looking screen would be a lie:
//
//   1. **Unverified is not blank.** The legacy GUI's `trust()` helper renders
//      the absence of a trust state as `unverified`, never as an empty cell:
//      a blank says "no opinion", and the honest answer is "nobody has
//      verified this" (FR-U17, FR-D1).
//   2. **Not swept is not empty.** Freshness is rendered even when it is
//      fine, so that "nothing has looked yet" and "there is nothing" stop
//      looking alike — and a claim backed by a session that is *still
//      running* is marked provisional rather than fresh (FR-U17).
//   3. **Unasked is not stopped.** `activity` is null when the engine could
//      not be asked. That is not "idle" and it is certainly not "finished";
//      it is "we do not know", and it renders that way (FR-E2, FR-E9).
//   4. **A write says who asked for it.** Starting a session and commenting
//      are both writes, so both appear as forms that collect a reason the
//      user typed. A button could not (FR-W1, r6).
//
// Everything here goes through `vogtApi`, which is the only door to Vogt and
// the only file the route-table test reads.

import {
  For,
  Show,
  createEffect,
  createMemo,
  createResource,
  createSignal,
} from "solid-js";
import type { Component, JSX } from "solid-js";
import { api } from "./api";
import {
  VogtUnavailable,
  backlog,
  commentWork,
  getWork,
  listSessions,
  listWorkflows,
  startSession,
  stopSession,
  why,
  type FreshnessSummary,
  type SessionSummary,
  type WorkDetail,
  type WorkItem,
} from "./vogtApi";

interface Props {
  itemRef: string;
  onError?: (message: string) => void;
}

/**
 * The outcome of one read, kept rather than thrown.
 *
 * FR-U21 asks every surface for a designed absent state, which means a
 * failed read has to reach the render as a value: a panel that says why it
 * is empty is a different thing from a panel that is empty. `absent` marks
 * the outage case — Vogt could not be reached at all — which is reported
 * with the server's own words rather than as no data.
 */
type Loaded<T> =
  | { ok: true; value: T }
  | { ok: false; absent: boolean; message: string };

async function attempt<T>(read: () => Promise<T>): Promise<Loaded<T>> {
  try {
    return { ok: true, value: await read() };
  } catch (error) {
    if (error instanceof VogtUnavailable) {
      return {
        ok: false,
        absent: true,
        message: error.message || `Vogt answered ${error.status}`,
      };
    }
    return {
      ok: false,
      absent: false,
      message: error instanceof Error ? error.message : String(error),
    };
  }
}

// -- the shapes this surface reads beyond the client's partial interfaces ---
//
// `vogtApi.ts` types each response with what its first reader needed. The
// wire carries more: relations arrive with the related item's ref, title and
// state; comments arrive with their author; freshness arrives with the age
// and the per-collector detail the legacy GUI shows. Widening those
// interfaces is a decision for `vogtApi.ts`, which this branch does not own,
// so the extra fields are read here through narrow local views and every one
// of them has a fallback for the day it is not sent.

interface RelationView {
  kind: string;
  related_id: string;
  related_ref?: string;
  related_title?: string;
  related_state?: string;
}

interface CommentView {
  id: string;
  body: string;
  created_at: string;
  actor_display_name?: string;
}

interface FreshnessView extends FreshnessSummary {
  oldest_relevant_sweep?: string | null;
  age_seconds?: number | null;
  collectors?: Record<string, string>;
}

interface Contribution {
  input: string;
  detail: string;
  value: number;
  weight: number;
  contribution: number;
}

function relationsOf(item: WorkItem): RelationView[] {
  return (item.relations ?? []) as RelationView[];
}

function commentsOf(detail: WorkDetail): CommentView[] {
  return detail.comments as CommentView[];
}

function contributionsOf(raw: Record<string, unknown> | undefined): Contribution[] {
  const rows = raw?.contributions;
  if (!Array.isArray(rows)) return [];
  return rows.map((row) => {
    const record = (row ?? {}) as Record<string, unknown>;
    return {
      input: String(record.input ?? "unnamed input"),
      detail: String(record.detail ?? ""),
      value: Number(record.value ?? 0),
      weight: Number(record.weight ?? 0),
      contribution: Number(record.contribution ?? 0),
    };
  });
}

function missingInputsOf(raw: Record<string, unknown> | undefined): [string, string][] {
  const missing = raw?.inputs_not_yet_available;
  if (!missing || typeof missing !== "object") return [];
  return Object.entries(missing as Record<string, unknown>).map(([name, note]) => [
    name,
    String(note),
  ]);
}

function scoreOf(raw: Record<string, unknown> | undefined): number | null {
  const total = raw?.total;
  return typeof total === "number" ? total : null;
}

// -- the three honest renderings -------------------------------------------

/**
 * Trust, by the legacy GUI's rule: never blank.
 *
 * `src/vogt/gui/static/app.js` reads `state || "unverified"` and this does
 * the same, because the two front ends disagreeing about what an unset trust
 * state means would be worse than either answer.
 */
function trustLabel(state: string | null | undefined): string {
  return state && state.trim() ? state : "unverified";
}

function describeAge(seconds: number | null | undefined): string {
  if (seconds === null || seconds === undefined) return "an unknown time";
  if (seconds < 90) return `${Math.round(seconds)}s`;
  if (seconds < 5400) return `${Math.round(seconds / 60)}m`;
  if (seconds < 172800) return `${Math.round(seconds / 3600)}h`;
  return `${Math.round(seconds / 86400)}d`;
}

/** The freshness sentence, in the legacy GUI's words. */
function freshnessText(state: FreshnessView | null): string {
  if (!state) return "freshness: not reported";
  const status = state.status || "never_swept";
  const parts: string[] = [];
  if (status === "never_swept") {
    parts.push(
      "nothing has been swept yet — this is 'not collected', not 'nothing found'",
    );
  } else {
    parts.push(`evidence is ${describeAge(state.age_seconds)} old at its oldest`);
    if (status === "partial") parts.push("at least one collector did not complete");
  }
  if (state.detail) parts.push(state.detail);
  return parts.join(" · ");
}

type Liveness = {
  /** The badge word. Never invented: "unknown" is a real answer here. */
  label: string;
  /** idle / running / waiting-for-input / errored / stopped / gone / unknown */
  tone: string;
  title: string;
};

/**
 * A session's activity, which is its liveness indicator (FR-U17, FR-U20).
 *
 * The order of these branches is the whole point. Vogt's own record — that
 * it stopped the session — is a fact it owns and outranks anything the
 * engine says. Below that, `activity` is the engine's live answer. Below
 * *that*, `alive === false` means the engine was asked and does not have the
 * session, while `activity == null` with `alive == null` means the engine
 * could not be asked at all — which must not render as "not running", since
 * the session may well be running perfectly.
 */
function liveness(session: SessionSummary, engineNote: string | null): Liveness {
  if (session.stopped_at) {
    return {
      label: "stopped",
      tone: "stopped",
      title: `Vogt stopped this session at ${formatWhen(session.stopped_at)}.`,
    };
  }
  if (session.activity) {
    return {
      label: session.activity,
      tone: session.activity,
      title: `The engine reports this session as ${session.activity}.`,
    };
  }
  if (session.alive === false) {
    return {
      label: "not running",
      tone: "gone",
      title:
        "The engine was asked and no longer has this session. Vogt never " +
        "recorded a stop for it.",
    };
  }
  return {
    label: "activity unknown",
    tone: "unknown",
    title:
      "The engine could not be asked" +
      (engineNote ? ` (${engineNote})` : "") +
      ", so this session's activity is unknown — which is not the same as it " +
      "having finished.",
  };
}

/**
 * Still running, for the freshness downgrade FR-U17's second clause asks for.
 *
 * `errored` counts as not running: the session is still there, but nothing is
 * being produced by it, so evidence behind it is not mid-flight. An engine
 * that could not be asked yields false here — an unknown is not a claim that
 * something is running, in either direction.
 */
function isLive(session: SessionSummary): boolean {
  if (session.stopped_at) return false;
  if (session.activity) return session.activity !== "errored";
  return session.alive === true;
}

function formatWhen(value: string | null | undefined): string {
  if (!value) return "—";
  const at = new Date(value);
  return Number.isNaN(at.valueOf()) ? String(value) : at.toLocaleString();
}

// -- the form every write appears through ----------------------------------

/**
 * A write's form: its own fields, then the reason, then the button.
 *
 * Vogt refuses a write without a reason, so a control that could submit
 * without one could only fail at the user. The submit stays disabled until
 * the reason has been typed — the same rule quick-create keeps (FR-W1, r6).
 */
const ReasonForm: Component<{
  submitLabel: string;
  busyLabel: string;
  placeholder: string;
  /** Non-null when the write cannot be offered, and why (FR-U21). */
  blockedBy?: string | null;
  /** Whether the form's own fields are complete. */
  ready?: () => boolean;
  onSubmit: (reason: string) => Promise<void>;
  /** The surface's toast, so a refused write is not only visible in the form. */
  onFailure?: (message: string) => void;
  children?: JSX.Element;
}> = (props) => {
  const [reason, setReason] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [failure, setFailure] = createSignal<string | null>(null);

  const blocked = () => Boolean(props.blockedBy);
  const canSubmit = () =>
    !blocked() &&
    !busy() &&
    reason().trim().length > 0 &&
    (props.ready ? props.ready() : true);

  const submit = async (event: Event) => {
    event.preventDefault();
    if (!canSubmit()) return;
    setBusy(true);
    setFailure(null);
    try {
      await props.onSubmit(reason().trim());
      setReason("");
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      setFailure(message);
      props.onFailure?.(`${props.submitLabel} was refused: ${message}`);
    } finally {
      setBusy(false);
    }
  };

  return (
    <form class="wid-form" onSubmit={(event) => void submit(event)}>
      {props.children}
      <label class="wid-field">
        <span>Reason (audited)</span>
        <input
          type="text"
          placeholder={props.placeholder}
          value={reason()}
          disabled={blocked()}
          onInput={(event) => setReason(event.currentTarget.value)}
        />
      </label>
      <div class="wid-form-actions">
        <button type="submit" disabled={!canSubmit()}>
          {busy() ? props.busyLabel : props.submitLabel}
        </button>
        <Show
          when={props.blockedBy}
          fallback={
            <span class="wid-hint">
              Vogt records who asked and why; a write without a reason is refused.
            </span>
          }
        >
          {(note) => <span class="wid-blocked">{note()}</span>}
        </Show>
      </div>
      <Show when={failure()}>
        {(message) => <p class="wid-failure">{message()}</p>}
      </Show>
    </form>
  );
};

const WorkItemDetail: Component<Props> = (props) => {
  const [work, { refetch: refetchWork }] = createResource(
    () => props.itemRef,
    (ref) => attempt(() => getWork(ref)),
  );

  // Read separately from `work.get`, which returns the same sessions but
  // drops the `engine` note that says *why* it could not ask. That note is
  // the difference between "no sessions" and "we could not look".
  const [sessions, { refetch: refetchSessions }] = createResource(
    () => props.itemRef,
    (ref) => attempt(() => listSessions({ work_item: ref, include_stopped: true })),
  );

  const [evidence, { refetch: refetchEvidence }] = createResource(
    () => props.itemRef,
    (ref) => attempt(() => why(ref)),
  );

  const item = createMemo<WorkItem | null>(() => {
    const loaded = work();
    return loaded && loaded.ok ? loaded.value.item : null;
  });

  const comments = createMemo<CommentView[]>(() => {
    const loaded = work();
    return loaded && loaded.ok ? commentsOf(loaded.value) : [];
  });

  // Freshness for the evidence behind this item's ranking. `work.get`
  // carries none — freshness is a property of a sweep, and the ranked view
  // scoped to this item's project is the answer that reports the sweep the
  // ranking inputs above came from.
  const sweepScope = createMemo<{ slug: string | null } | null>(() => {
    const current = item();
    return current ? { slug: current.project_slug ?? null } : null;
  });

  const [sweep] = createResource(sweepScope, (scope) =>
    attempt(() =>
      backlog(scope.slug ? { project: scope.slug, limit: 1 } : { limit: 1 }),
    ),
  );

  const [workflows] = createResource(() => attempt(() => listWorkflows()));

  // The engine's own template list, so the start form offers the templates
  // that exist rather than asking for a name to be typed from memory.
  const [templates] = createResource(async () => {
    try {
      return (await api.publicConfig()).session_templates ?? [];
    } catch {
      return [];
    }
  });

  const refreshAll = () => {
    void refetchWork();
    void refetchSessions();
    void refetchEvidence();
  };

  createEffect(() => {
    const loaded = work();
    if (loaded && !loaded.ok) {
      props.onError?.(`${props.itemRef}: ${loaded.message}`);
    }
  });

  createEffect(() => {
    const loaded = sessions();
    if (loaded && !loaded.ok) {
      props.onError?.(`Sessions for ${props.itemRef}: ${loaded.message}`);
    }
  });

  const outage = createMemo(() => {
    const loaded = work();
    return loaded && !loaded.ok && loaded.absent ? loaded.message : null;
  });

  const failure = createMemo(() => {
    const loaded = work();
    return loaded && !loaded.ok && !loaded.absent ? loaded.message : null;
  });

  const sessionList = createMemo<SessionSummary[]>(() => {
    const loaded = sessions();
    return loaded && loaded.ok ? loaded.value.sessions : [];
  });

  /** What the engine said when it could not be asked, or null. */
  /**
   * What the engine said when it could not be asked, or null.
   *
   * Kept apart from a failure of the read itself. `engine` set means Vogt
   * answered and the session links below are true — it is their *activity*
   * that is unknown. A failed read means there is nothing below at all, and
   * saying "no sessions" then would be the lie this surface exists to avoid.
   */
  const engineNote = createMemo<string | null>(() => {
    const loaded = sessions();
    return loaded && loaded.ok ? (loaded.value.engine ?? null) : null;
  });

  const sessionsFailure = createMemo<string | null>(() => {
    const loaded = sessions();
    if (!loaded || loaded.ok) return null;
    return loaded.absent
      ? `Vogt cannot be reached: ${loaded.message}`
      : `The session list could not be read: ${loaded.message}`;
  });

  /** Session writes are engine writes: an engine that cannot be asked cannot start one. */
  const sessionWritesBlocked = createMemo<string | null>(() => {
    const reason = engineNote() ?? sessionsFailure() ?? outage();
    return reason ? `Session controls are disabled: ${reason}` : null;
  });

  const vogtWritesBlocked = createMemo<string | null>(() => {
    const absent = outage();
    return absent ? `Vogt cannot be reached: ${absent}` : null;
  });

  const liveCount = createMemo(() => sessionList().filter(isLive).length);

  const freshnessState = createMemo<FreshnessView | null>(() => {
    const loaded = sweep();
    if (!loaded || !loaded.ok) return null;
    return loaded.value.freshness as FreshnessView;
  });

  const sweepFailure = createMemo<string | null>(() => {
    const loaded = sweep();
    return loaded && !loaded.ok ? loaded.message : null;
  });

  const collectors = createMemo<[string, string][]>(() =>
    Object.entries(freshnessState()?.collectors ?? {}),
  );

  const freshnessTone = createMemo(() => {
    // FR-U17: a claim backed by a still-running session is provisional, not
    // fresh — whatever the sweep says about its own age.
    if (liveCount() > 0) return "provisional";
    return freshnessState()?.status || "unknown";
  });

  const evidenceRaw = createMemo<Record<string, unknown> | undefined>(() => {
    const loaded = evidence();
    return loaded && loaded.ok ? loaded.value : undefined;
  });

  const evidenceFailure = createMemo<string | null>(() => {
    const loaded = evidence();
    return loaded && !loaded.ok ? loaded.message : null;
  });

  const contributions = createMemo(() => contributionsOf(evidenceRaw()));
  const missingInputs = createMemo(() => missingInputsOf(evidenceRaw()));

  const workflowForKind = createMemo(() => {
    const current = item();
    const loaded = workflows();
    if (!current || !loaded || !loaded.ok) return null;
    return loaded.value.workflows.find((flow) => flow.kind === current.kind) ?? null;
  });

  const workflowStates = createMemo<string[]>(() => {
    const flow = workflowForKind();
    if (!flow) return [];
    return flow.states.map((state) =>
      typeof state === "string" ? state : state.name,
    );
  });

  const [commentBody, setCommentBody] = createSignal("");
  const [writeBack, setWriteBack] = createSignal<string | null>(null);
  const [template, setTemplate] = createSignal("");
  const [sessionName, setSessionName] = createSignal("");
  const [stopping, setStopping] = createSignal<string | null>(null);

  const submitComment = async (reason: string) => {
    const result = await commentWork(props.itemRef, commentBody().trim(), reason);
    setCommentBody("");
    // A write-back that failed upstream never fails the local write, which
    // is exactly why it has to be said out loud rather than assumed.
    const upstream = result?.write_back;
    setWriteBack(typeof upstream === "string" ? upstream : null);
    void refetchWork();
  };

  const submitStart = async (reason: string) => {
    await startSession({
      work_item: props.itemRef,
      template: template().trim() || undefined,
      name: sessionName().trim() || undefined,
      reason,
    });
    setTemplate("");
    setSessionName("");
    void refetchSessions();
    void refetchWork();
  };

  const submitStop = async (id: string, reason: string) => {
    await stopSession(id, reason);
    setStopping(null);
    void refetchSessions();
  };

  return (
    <div class="vogt-surface wid-view">
      <header class="wid-header">
        <div class="wid-heading">
          <span class="wid-ref">{props.itemRef}</span>
          <h2>{item()?.title ?? (work.loading ? "Loading…" : props.itemRef)}</h2>
        </div>
        <div class="wid-header-actions">
          <span
            class={`wid-trust wid-trust--${trustLabel(item()?.trust_state)}`}
            title={`trust: ${trustLabel(item()?.trust_state)}`}
          >
            {trustLabel(item()?.trust_state)}
          </span>
          <button type="button" onClick={refreshAll}>
            Refresh
          </button>
        </div>
      </header>

      <Show when={outage()}>
        {(message) => (
          <p class="wid-outage" role="alert">
            Vogt cannot be reached, so nothing below is this item's state:{" "}
            {message()}. This is an outage, not an empty work item.
          </p>
        )}
      </Show>

      <Show when={failure()}>
        {(message) => (
          <p class="wid-failure" role="alert">
            {props.itemRef} could not be read: {message()}
          </p>
        )}
      </Show>

      <Show when={item()}>
        {(current) => (
          <>
            <div class="wid-facts">
              <span class="wid-chip">{current().kind}</span>
              <span class="wid-chip wid-chip--state">{current().state}</span>
              <span class="wid-chip">{current().priority}</span>
              <Show when={current().effort}>
                {(effort) => <span class="wid-chip">effort {effort()}</span>}
              </Show>
              <span class="wid-chip">
                project: {current().project_slug ?? "unassigned"}
              </span>
              <span class="wid-chip">
                assignee: {current().assignee_identity_ref ?? "nobody"}
              </span>
              <span class="wid-chip">origin: {current().origin ?? "created"}</span>
              <span class="wid-chip">updated {formatWhen(current().updated_at)}</span>
            </div>

            <div class="wid-columns">
              <div class="wid-main">
                <section class="wid-panel">
                  <h3>Description</h3>
                  <Show
                    when={current().body?.trim()}
                    fallback={<p class="wid-absent">No description was written.</p>}
                  >
                    {(body) => <p class="wid-body">{body()}</p>}
                  </Show>
                </section>

                <section class="wid-panel">
                  <h3>State</h3>
                  <Show
                    when={workflowStates().length > 0}
                    fallback={
                      <p class="wid-absent">
                        The workflow for {current().kind} could not be read, so
                        only the current state is shown.
                      </p>
                    }
                  >
                    <div class="wid-rail">
                      <For each={workflowStates()}>
                        {(state) => (
                          <span
                            class={`wid-rail-state ${
                              state === current().state ? "is-current" : ""
                            }`}
                          >
                            {state}
                          </span>
                        )}
                      </For>
                    </div>
                  </Show>
                  <p class="wid-hint">
                    The transitions that produced this state are audited writes
                    and belong in the audit trail below, not in a second story
                    told by this panel.
                  </p>
                </section>

                <section class="wid-panel">
                  <div class="wid-panel-head">
                    <h3>Sessions</h3>
                    <span class="wid-hint">
                      {liveCount()} live · {sessionList().length} recorded
                    </span>
                  </div>

                  <Show when={engineNote()}>
                    {(note) => (
                      <p class="wid-engine-note" role="status">
                        The engine could not be asked: {note()} — Vogt's record
                        of what it started is below; their activity is unknown,
                        which is not the same as stopped.
                      </p>
                    )}
                  </Show>

                  <Show when={sessionsFailure()}>
                    {(message) => (
                      <p class="wid-failure" role="alert">
                        {message()} — nothing is listed below because nothing
                        could be read, not because nothing is running.
                      </p>
                    )}
                  </Show>

                  <Show
                    when={sessionList().length > 0}
                    fallback={
                      <Show when={!sessionsFailure()}>
                        <Show
                          when={!engineNote()}
                          fallback={
                            <p class="wid-absent">
                              No session links are recorded for {props.itemRef},
                              and the engine could not be asked about any it may
                              still be running.
                            </p>
                          }
                        >
                          <p class="wid-absent">
                            No session has been started for {props.itemRef}.
                          </p>
                        </Show>
                      </Show>
                    }
                  >
                    <ul class="wid-sessions">
                      <For each={sessionList()}>
                        {(session) => {
                          const state = createMemo(() =>
                            liveness(session, engineNote()),
                          );
                          return (
                            <li class="wid-session">
                              <div class="wid-session-head">
                                <span
                                  class={`wid-activity wid-activity--${state().tone}`}
                                  title={state().title}
                                >
                                  {state().label}
                                </span>
                                <span class="wid-mono">{session.id}</span>
                                {/* FR-U20: the control navigates to the
                                    terminal attached to this session. A
                                    session Vogt has stopped has no terminal
                                    to attach to, so it gets no control that
                                    would quietly do nothing. */}
                                <Show
                                  when={!session.stopped_at}
                                  fallback={
                                    <span class="wid-hint wid-open-terminal">
                                      terminal closed
                                    </span>
                                  }
                                >
                                  <a
                                    class="wid-open-terminal"
                                    href={`#/t/${encodeURIComponent(
                                      session.engine_session_id,
                                    )}`}
                                    title={`Open the terminal attached to ${session.engine_session_id}`}
                                  >
                                    Open terminal
                                  </a>
                                </Show>
                              </div>
                              <div class="wid-session-meta">
                                <span>{session.template ?? "default shell"}</span>
                                <span class="wid-mono">{session.cwd}</span>
                                <span>as {session.actor}</span>
                                <span>started {formatWhen(session.started_at)}</span>
                                <Show when={session.stopped_at}>
                                  {(at) => <span>stopped {formatWhen(at())}</span>}
                                </Show>
                              </div>
                              <p class="wid-session-reason">“{session.reason}”</p>
                              <Show when={!session.stopped_at}>
                                <Show
                                  when={stopping() === session.id}
                                  fallback={
                                    <button
                                      type="button"
                                      class="wid-inline-btn"
                                      onClick={() => setStopping(session.id)}
                                    >
                                      Stop session…
                                    </button>
                                  }
                                >
                                  <ReasonForm
                                    submitLabel="Stop session"
                                    busyLabel="Stopping…"
                                    placeholder="why this session should stop"
                                    blockedBy={sessionWritesBlocked()}
                                    onFailure={props.onError}
                                    onSubmit={(reason) =>
                                      submitStop(session.id, reason)
                                    }
                                  >
                                    <button
                                      type="button"
                                      class="wid-inline-btn"
                                      onClick={() => setStopping(null)}
                                    >
                                      Cancel
                                    </button>
                                  </ReasonForm>
                                </Show>
                              </Show>
                            </li>
                          );
                        }}
                      </For>
                    </ul>
                  </Show>

                  <div class="wid-start">
                    <h4>Start a session for {props.itemRef}</h4>
                    <p class="wid-hint">
                      The session opens in the project's registered tree with
                      this item's brief written for the agent to read.
                    </p>
                    <ReasonForm
                      submitLabel="Start session"
                      busyLabel="Starting…"
                      placeholder="why a session is being opened for this item"
                      blockedBy={sessionWritesBlocked()}
                      onFailure={props.onError}
                      onSubmit={submitStart}
                    >
                      <label class="wid-field">
                        <span>Template</span>
                        <select
                          value={template()}
                          disabled={Boolean(sessionWritesBlocked())}
                          onInput={(event) => setTemplate(event.currentTarget.value)}
                        >
                          <option value="">Default shell</option>
                          <For each={templates() ?? []}>
                            {(entry) => (
                              <option value={entry.name} title={entry.description}>
                                {entry.name}
                              </option>
                            )}
                          </For>
                        </select>
                      </label>
                      <label class="wid-field">
                        <span>Session name (optional)</span>
                        <input
                          type="text"
                          placeholder={`derived from ${props.itemRef}`}
                          value={sessionName()}
                          disabled={Boolean(sessionWritesBlocked())}
                          onInput={(event) =>
                            setSessionName(event.currentTarget.value)
                          }
                        />
                      </label>
                    </ReasonForm>
                  </div>
                </section>

                <section class="wid-panel">
                  <div class="wid-panel-head">
                    <h3>Collected evidence</h3>
                    <Show when={scoreOf(evidenceRaw()) !== null}>
                      <span class="wid-hint">
                        rank score {scoreOf(evidenceRaw())?.toFixed(3)}
                      </span>
                    </Show>
                  </div>

                  <p class={`wid-freshness wid-freshness--${freshnessTone()}`}>
                    <Show when={liveCount() > 0}>
                      <strong>provisional</strong> — a session is still running
                      for this item, so what is collected here is a snapshot
                      taken mid-flight ·{" "}
                    </Show>
                    {freshnessText(freshnessState())}
                    <Show when={sweepFailure()}>
                      {(message) => <> — the sweep could not be read: {message()}</>}
                    </Show>
                  </p>

                  <Show when={collectors().length > 0}>
                    <div class="wid-collectors">
                      <For each={collectors()}>
                        {([name, age]) => (
                          <span class="wid-chip">
                            {name}: {age}
                          </span>
                        )}
                      </For>
                    </div>
                  </Show>

                  <Show
                    when={evidence()?.ok}
                    fallback={
                      <Show
                        when={evidenceFailure()}
                        fallback={
                          <p class="wid-hint">Reading the ranking evidence…</p>
                        }
                      >
                        {(message) => (
                          <p class="wid-absent">
                            The ranking evidence for {props.itemRef} could not be
                            read: {message()}
                          </p>
                        )}
                      </Show>
                    }
                  >
                    <Show
                      when={contributions().length > 0}
                      fallback={
                        <p class="wid-absent">
                          No ranking input has fired for this item yet — that is
                          "nothing collected", not "nothing found".
                        </p>
                      }
                    >
                      <table class="wid-table">
                        <thead>
                          <tr>
                            <th>Input</th>
                            <th>What it saw</th>
                            <th>Value</th>
                            <th>Weight</th>
                            <th>Contribution</th>
                          </tr>
                        </thead>
                        <tbody>
                          <For each={contributions()}>
                            {(row) => (
                              <tr>
                                <td class="wid-mono">{row.input}</td>
                                <td>{row.detail}</td>
                                <td>{row.value.toFixed(2)}</td>
                                <td>{row.weight.toFixed(2)}</td>
                                <td>{row.contribution.toFixed(3)}</td>
                              </tr>
                            )}
                          </For>
                        </tbody>
                      </table>
                    </Show>

                    <Show when={missingInputs().length > 0}>
                      <div class="wid-missing">
                        <h4>Not collected</h4>
                        <For each={missingInputs()}>
                          {([name, note]) => (
                            <p class="wid-absent">
                              <span class="wid-mono">{name}</span> — {note}
                            </p>
                          )}
                        </For>
                      </div>
                    </Show>
                  </Show>
                </section>

                <section class="wid-panel">
                  <div class="wid-panel-head">
                    <h3>Comments</h3>
                    <span class="wid-hint">{comments().length} recorded</span>
                  </div>
                  <Show
                    when={comments().length > 0}
                    fallback={
                      <p class="wid-absent">
                        No comment has been written on {props.itemRef}.
                      </p>
                    }
                  >
                    <ul class="wid-comments">
                      <For each={comments()}>
                        {(comment) => (
                          <li class="wid-comment">
                            <div class="wid-comment-head">
                              <span>{comment.actor_display_name ?? "unattributed"}</span>
                              <span>{formatWhen(comment.created_at)}</span>
                            </div>
                            <p class="wid-body">{comment.body}</p>
                          </li>
                        )}
                      </For>
                    </ul>
                  </Show>

                  <Show when={writeBack()}>
                    {(status) => (
                      <p class="wid-hint">
                        Upstream write-back: {status()}
                        {status() === "failed"
                          ? " — the comment is recorded here regardless."
                          : ""}
                      </p>
                    )}
                  </Show>

                  <ReasonForm
                    submitLabel="Add comment"
                    busyLabel="Posting…"
                    placeholder="why this comment is being added"
                    blockedBy={vogtWritesBlocked()}
                    ready={() => commentBody().trim().length > 0}
                    onFailure={props.onError}
                    onSubmit={submitComment}
                  >
                    <label class="wid-field">
                      <span>Comment</span>
                      <textarea
                        rows={4}
                        value={commentBody()}
                        disabled={Boolean(vogtWritesBlocked())}
                        onInput={(event) => setCommentBody(event.currentTarget.value)}
                      />
                    </label>
                  </ReasonForm>
                </section>
              </div>

              <aside class="wid-aside">
                <section class="wid-panel">
                  <h3>Labels</h3>
                  <Show
                    when={(current().labels ?? []).length > 0}
                    fallback={<p class="wid-absent">No labels.</p>}
                  >
                    <div class="wid-facts">
                      <For each={current().labels ?? []}>
                        {(label) => <span class="wid-chip">{label}</span>}
                      </For>
                    </div>
                  </Show>
                </section>

                <section class="wid-panel">
                  <h3>Relations</h3>
                  <Show
                    when={relationsOf(current()).length > 0}
                    fallback={<p class="wid-absent">No relations.</p>}
                  >
                    <ul class="wid-relations">
                      <For each={relationsOf(current())}>
                        {(relation) => (
                          <li>
                            <span class="wid-chip">{relation.kind}</span>
                            <Show
                              when={relation.related_ref}
                              fallback={
                                <span class="wid-mono">{relation.related_id}</span>
                              }
                            >
                              {(ref) => (
                                <a href={`#/w/${encodeURIComponent(ref())}`}>
                                  {ref()}
                                  {relation.related_title
                                    ? ` — ${relation.related_title}`
                                    : ""}
                                </a>
                              )}
                            </Show>
                            <Show when={relation.related_state}>
                              {(state) => <span class="wid-chip">{state()}</span>}
                            </Show>
                          </li>
                        )}
                      </For>
                    </ul>
                  </Show>
                </section>

                <section class="wid-panel">
                  <h3>Audit trail</h3>
                  <p class="wid-absent">
                    Vogt audits every write to {props.itemRef} and{" "}
                    <span class="wid-mono">audit.list</span> can be filtered to
                    this item, but this client exposes no binding for it yet. No
                    records are shown rather than an empty list, which would read
                    as "nothing has ever been written here".
                  </p>
                </section>
              </aside>
            </div>
          </>
        )}
      </Show>

      <Show when={work.loading && !item()}>
        <p class="wid-hint">Loading {props.itemRef}…</p>
      </Show>
    </div>
  );
};

export default WorkItemDetail;
