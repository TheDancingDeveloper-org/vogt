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
//      running* is marked provisional rather than fresh (FR-U17). That rule
//      is about evidence, so it is kept where the evidence is: the "Observed
//      evidence" panel reads the observed store through `observations.list`
//      and badges every row provisional / settled / unverified.
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
  listObservations,
  listSessions,
  listWorkflows,
  startSession,
  stopSession,
  updateWork,
  why,
  type FreshnessSummary,
  type Observation,
  type SessionSummary,
  type WhyResult,
  type WorkDetail,
  type WorkItem,
} from "./vogtApi";
import { ViewAgeBadge, createLoadStamp, createViewAge } from "./viewAge";

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

/** Vogt's priority scale (`core/entities.py`: `Priority`). A closed set, so
 *  the editor offers it rather than deriving it from whatever this item
 *  happens to be. */
const PRIORITIES = ["p0", "p1", "p2", "p3", "p4"] as const;

function relationsOf(item: WorkItem): RelationView[] {
  return (item.relations ?? []) as RelationView[];
}

function commentsOf(detail: WorkDetail): CommentView[] {
  return detail.comments as CommentView[];
}

// These three read `why` directly now that the client types it. They were
// written defensively against `Record<string, unknown>` when it did not, and
// the defence was hiding the mismatch rather than surviving it.

function contributionsOf(raw: WhyResult | undefined): Contribution[] {
  return (raw?.contributions ?? []).map((row) => ({
    input: row.input,
    detail: row.detail ?? "",
    value: row.value,
    weight: row.weight,
    contribution: row.contribution,
  }));
}

function missingInputsOf(raw: WhyResult | undefined): [string, string][] {
  return Object.entries(raw?.inputs_not_yet_available ?? {});
}

function scoreOf(raw: WhyResult | undefined): number | null {
  return raw?.total ?? null;
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

// -- answering a session that is waiting for input (FR-M1, FR-E2) ----------
//
// MERGE §14's M12 demo is one sentence: *receive a push that a session is
// waiting for input, open it, unblock it.* The first two parts existed —
// FR-M2 routes the notification and FR-U20 opens the terminal — and the third
// meant a keyboard over a PTY, which on a phone is the worst surface in the
// product for typing one character into.
//
// The rule this encodes, and the only reason it is safe to answer a prompt
// from a summary view: **nothing can be answered that has not been shown.**
// The control fetches the session's own scrollback and renders the tail of
// it first; the answer buttons do not exist until that text is on screen. A
// one-tap "y" against a prompt nobody read is not unblocking a session, it is
// approving something unseen — and this product's whole argument is that an
// act with no visible subject is worse than no act.

/** How much of the tail to show. Enough for a prompt and the lines that set
 *  it up, not so much that the answer scrolls off a phone. */
const TAIL_LINES = 12;

/** The tail of a session's scrollback, decoded and stripped of the escape
 *  sequences a terminal would have consumed. */
function tailOf(scrollbackBase64: string): string {
  let raw: string;
  try {
    const bytes = Uint8Array.from(atob(scrollbackBase64), (c) => c.charCodeAt(0));
    raw = new TextDecoder().decode(bytes);
  } catch {
    return "";
  }
  const plain = raw
    // CSI and OSC sequences: colour, cursor moves, title sets. A prompt
    // rendered with them intact is unreadable in a <pre>.
    // eslint-disable-next-line no-control-regex
    .replace(/\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)/g, "")
    // eslint-disable-next-line no-control-regex
    .replace(/\x1b\[[0-9;?]*[ -/]*[@-~]/g, "")
    .replace(/\r\n/g, "\n")
    .replace(/\r/g, "\n");
  const lines = plain.split("\n");
  while (lines.length && (lines[lines.length - 1] ?? "").trim() === "") lines.pop();
  return lines.slice(-TAIL_LINES).join("\n");
}

/** Does the tail end in something that reads like a yes/no question?
 *
 *  Only used to *offer* the two one-tap answers beside the free-text box —
 *  never to answer anything, and never to hide the prompt. A wrong guess
 *  here costs a button that is not useful, which is the correct direction
 *  for a guess about somebody else's prompt to fail in. */
function looksLikeYesNo(tail: string): boolean {
  const last = tail.trimEnd().slice(-200).toLowerCase();
  return /\(y\/n\)|\[y\/n\]|\(yes\/no\)|\by\/n\b|\?\s*\(y[/|]n\)/.test(last);
}

// -- observed evidence, and whether it has settled -------------------------
//
// `observations.list` returns the observed store itself — what a collector
// saw, before any ranking made anything of it. The item page reads it for one
// reason: FR-U17's second clause is a rule about *evidence*, and until this
// panel existed the rule had nowhere to be broken and nowhere to be kept.
//
// There is no work-item parameter on the operation, because an observation is
// filed under its own subject key (`session:01J…`). The link back to an item
// is the payload's `work_item`, which `session_outcomes.py` writes for both
// kinds it produces, so that is what this matches on.

/** How many observations one read asks for. Equal to `total` means the store
 *  had at least this many and the panel says the list is cut. */
const OBSERVED_LIMIT = 200;

type Settlement = {
  /** The badge word. `unverified` is a real answer, and never a blank. */
  label: string;
  /** provisional / settled / unverified */
  tone: string;
  title: string;
};

function payloadOf(observation: Observation): Record<string, unknown> {
  return observation.payload ?? {};
}

function payloadText(observation: Observation, key: string): string | null {
  const value = payloadOf(observation)[key];
  return typeof value === "string" && value.trim() ? value : null;
}

/**
 * Whether an observation is a settled fact or a snapshot taken mid-flight.
 *
 * FR-U17: *a claim backed by a still-running session is marked provisional,
 * not fresh.* `session_outcomes.py` writes that judgement into the evidence
 * as `provisional`, so this reads it rather than re-deriving it — a surface
 * deciding for itself what "still running" means is a second copy of a rule
 * the collector already keeps, and the two would eventually disagree.
 *
 * The third branch is the load-bearing one. An observation whose payload does
 * not carry the flag at all — an older sweep, another collector's kind — is
 * *not* settled and is not provisional: nobody said. That renders as
 * `unverified`, in the same words the trust badge uses, because a blank says
 * "no opinion" when the honest answer is "nobody checked".
 */
function settlement(observation: Observation): Settlement {
  const flag = payloadOf(observation).provisional;
  if (flag === true) {
    return {
      label: "provisional",
      tone: "provisional",
      title:
        "The session behind this had not finished when it was observed, so " +
        "this is a snapshot taken mid-flight and not a settled outcome. It " +
        "is provisional, not fresh.",
    };
  }
  if (flag === false) {
    return {
      label: "settled",
      tone: "settled",
      title:
        "The session behind this had finished when it was observed, so what " +
        "it says is what the run left behind.",
    };
  }
  return {
    label: "unverified",
    tone: "unverified",
    title:
      "This evidence does not say whether what produced it had finished, so " +
      "whether it has settled is unknown — which is not the same as settled.",
  };
}

/** What the row says about how the run ended, including when it says nothing. */
function exitText(observation: Observation): string {
  const code = payloadOf(observation).exit_code;
  if (typeof code === "number") return `exit ${code}`;
  if (payloadOf(observation).provisional === true) {
    return "no exit code — it had not exited";
  }
  return "no exit code recorded";
}

// -- the form every write appears through ----------------------------------

/**
 * A write's form: its own fields, then the reason, then the button.
 *
 * Vogt refuses a write without a reason, so a control that could submit
 * without one could only fail at the user. The submit stays disabled until
 * the reason has been typed — the same rule quick-create keeps (FR-W1, r6).
 */
/**
 * Answer a session that is waiting for input, without opening a terminal.
 *
 * Two properties, and the second is the one that makes this safe to offer on
 * a summary surface:
 *
 *   1. **It shows before it asks.** Opening the control reads the session's
 *      own scrollback and renders the tail. Until that text is on screen
 *      there is nothing to press: the answer field and the y/n shortcuts are
 *      inside the `Show` that waits for it.
 *   2. **A failed read is not an empty prompt.** If the engine cannot be
 *      asked, this says so in the engine's words and offers no way to answer
 *      — a blank box above a Send button would invite somebody to answer a
 *      question they were never shown, which is the failure the whole control
 *      is arranged against (FR-U21).
 *
 * `submit: true` appends the carriage return, so what is sent is the answer
 * *and* the return that commits it — the thing a person would do at the
 * terminal, in one act rather than two.
 */
const AnswerWaitingSession: Component<{
  engineSessionId: string;
  onDone: () => void;
  onFailure?: (message: string) => void;
}> = (props) => {
  const [open, setOpen] = createSignal(false);
  const [answer, setAnswer] = createSignal("");
  const [sending, setSending] = createSignal(false);

  const [tail, { refetch }] = createResource(open, async (isOpen) => {
    if (!isOpen) return null;
    return attempt(async () => {
      const detail = await api.getSession(props.engineSessionId);
      return tailOf(detail.scrollback_base64);
    });
  });

  const send = async (text: string) => {
    if (sending()) return;
    setSending(true);
    try {
      await api.sessionInput(props.engineSessionId, text, true);
      setAnswer("");
      // Re-read rather than assume: what the session did with the answer is
      // the only evidence that it took it, and "sent" is not "accepted".
      await refetch();
      props.onDone();
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      props.onFailure?.(`the session did not take that input: ${message}`);
    } finally {
      setSending(false);
    }
  };

  return (
    <div class="wid-answer">
      <Show
        when={open()}
        fallback={
          <button
            type="button"
            class="wid-inline-btn"
            onClick={() => setOpen(true)}
          >
            Answer…
          </button>
        }
      >
        <Show
          when={tail()}
          fallback={<p class="wid-hint">reading what it is asking…</p>}
        >
          {(loaded) => {
            const outcome = loaded();
            if (!outcome.ok) {
              return (
                <p class="wid-absent">
                  {outcome.message} — so there is nothing to answer here. Open
                  the terminal instead.
                </p>
              );
            }
            const text = outcome.value ?? "";
            return (
              <>
                <pre class="wid-answer-tail" data-testid="prompt-tail">
                  {text || "the session has produced no output to show"}
                </pre>
                <form
                  class="wid-answer-form"
                  onSubmit={(event) => {
                    event.preventDefault();
                    const typed = answer().trim();
                    if (typed) void send(typed);
                  }}
                >
                  <input
                    type="text"
                    value={answer()}
                    disabled={sending()}
                    placeholder="your answer"
                    aria-label="Answer this session"
                    onInput={(event) => setAnswer(event.currentTarget.value)}
                  />
                  <button type="submit" disabled={sending() || !answer().trim()}>
                    {sending() ? "sending…" : "Send"}
                  </button>
                  {/* Offered only when the prompt reads like a yes/no
                      question, and always beside the text it belongs to. A
                      guess that is wrong costs an unhelpful button; a guess
                      that hid the prompt would cost far more. */}
                  <Show when={looksLikeYesNo(text)}>
                    <button
                      type="button"
                      disabled={sending()}
                      onClick={() => void send("y")}
                    >
                      y
                    </button>
                    <button
                      type="button"
                      disabled={sending()}
                      onClick={() => void send("n")}
                    >
                      n
                    </button>
                  </Show>
                </form>
              </>
            );
          }}
        </Show>
      </Show>
    </div>
  );
};

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

  // -- inline edit (FR-U12) -------------------------------------------------
  //
  // FR-U12's subject is "a drag **or inline edit**", and there was no inline
  // edit anywhere: `updateWork` was exported by `vogtApi.ts` and called by
  // nothing. Title and priority are the honest minimum — the two fields a
  // reader looking at this page most often finds wrong — and the same three
  // rules govern them as govern the board's drag:
  //
  //   1. The edit renders optimistically, so the page reads as the change
  //      the moment it is submitted.
  //   2. The server's answer is authoritative and replaces it, field for
  //      field: `accepted` holds what Vogt sent back, never what was typed.
  //   3. A refusal discards the optimistic value outright and shows Vogt's
  //      own sentence beside the field. Nothing remembers the refused value —
  //      reopening the editor reads the server's state again, which is
  //      FR-U12's "never persist, cache, or re-derive".
  //
  // And it is a *view that collects a reason*: the editor is a `ReasonForm`
  // like every other write on this surface, so an edit with no typed reason
  // cannot submit (FR-W1, r6).

  /** Vogt's answer to the last accepted edit, kept only until the refetch
   *  lands so the page does not flicker back through the stale value. */
  const [accepted, setAccepted] = createSignal<{ ref: string; item: WorkItem } | null>(
    null,
  );
  /** What was submitted and is not yet answered. Discarded on refusal. */
  const [optimistic, setOptimistic] = createSignal<{
    ref: string;
    title: string;
    priority: string;
  } | null>(null);

  const serverItem = createMemo<WorkItem | null>(() => {
    const loaded = work();
    const read = loaded && loaded.ok ? loaded.value.item : null;
    const answer = accepted();
    // The accepted answer only stands in for the item it was an answer about,
    // and only until a read of that item comes back.
    if (answer && answer.ref === props.itemRef && !read) return answer.item;
    if (answer && read && answer.ref === props.itemRef) {
      return Date.parse(answer.item.updated_at) >= Date.parse(read.updated_at)
        ? answer.item
        : read;
    }
    return read;
  });

  const item = createMemo<WorkItem | null>(() => {
    const server = serverItem();
    if (!server) return null;
    const draft = optimistic();
    if (!draft || draft.ref !== props.itemRef) return server;
    return { ...server, title: draft.title, priority: draft.priority };
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

  // The observed store, scoped to this item's project. Asked for only once
  // the item has been read, because the project is the scope and an unscoped
  // read of every project's evidence would be a different question.
  const [observed, { refetch: refetchObserved }] = createResource(
    sweepScope,
    (scope) =>
      attempt(() =>
        listObservations(
          scope.slug
            ? { project: scope.slug, latest_only: true, limit: OBSERVED_LIMIT }
            : { latest_only: true, limit: OBSERVED_LIMIT },
        ),
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
    void refetchObserved();
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

  // -- how old this page is (FR-U10) ----------------------------------------
  //
  // The fifth surface, and the one where a stale read is quietest: an item
  // page shows a *single* item's state, so there is nothing on it that looks
  // wrong when it is an hour out of date. It reports its own age like the
  // rest now, and names the Refresh beside it, because that is what makes it
  // current — the page does not poll and does not subscribe.
  const loadedAt = createLoadStamp(work, (loaded) => loaded.ok);

  const viewAge = createViewAge(() => ({
    loadedAt: loadedAt(),
    outage: outage(),
    failed: Boolean(failure()),
  }));

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

  const evidenceRaw = createMemo<WhyResult | undefined>(() => {
    const loaded = evidence();
    return loaded && loaded.ok ? loaded.value : undefined;
  });

  const evidenceFailure = createMemo<string | null>(() => {
    const loaded = evidence();
    return loaded && !loaded.ok ? loaded.message : null;
  });

  const contributions = createMemo(() => contributionsOf(evidenceRaw()));
  const missingInputs = createMemo(() => missingInputsOf(evidenceRaw()));

  // -- the observed store, for this item (FR-U17) ---------------------------

  /** Every observation whose payload names this item, newest per subject. */
  const observedRows = createMemo<Observation[]>(() => {
    const loaded = observed();
    if (!loaded || !loaded.ok) return [];
    return loaded.value.observations.filter(
      (row) => payloadText(row, "work_item") === props.itemRef,
    );
  });

  const provisionalRows = createMemo(() =>
    observedRows().filter((row) => settlement(row).tone === "provisional"),
  );

  const unverifiedRows = createMemo(() =>
    observedRows().filter((row) => settlement(row).tone === "unverified"),
  );

  const observedFailure = createMemo<string | null>(() => {
    const loaded = observed();
    if (!loaded || loaded.ok) return null;
    return loaded.absent
      ? `Vogt cannot be reached: ${loaded.message}`
      : `The observed evidence could not be read: ${loaded.message}`;
  });

  /** True when the store had at least `OBSERVED_LIMIT` rows, so this list is
   *  cut rather than complete and must not be read as all there is. */
  const observedTruncated = createMemo(() => {
    const loaded = observed();
    return Boolean(loaded && loaded.ok && loaded.value.total >= OBSERVED_LIMIT);
  });

  /**
   * The panel's tone, by FR-U17's rule rather than by the clock.
   *
   * One provisional row makes the whole panel provisional: the reader is
   * being shown a set of claims and one of them is mid-flight, and averaging
   * that away would be the panel deciding the exception does not matter.
   */
  const observedTone = createMemo(() => {
    if (observedFailure()) return "unknown";
    if (provisionalRows().length > 0) return "provisional";
    if (observedRows().length === 0) return "never_swept";
    if (unverifiedRows().length > 0) return "partial";
    return "fresh";
  });

  /** The sentence under the heading. Never empty, in any of the five states. */
  const observedSummary = createMemo<string>(() => {
    if (observedFailure()) {
      return (
        "nothing is summarised here because nothing was read — that is a " +
        "failed read, not an item with no evidence behind it"
      );
    }
    if (!observed()) return "reading the observed store…";
    const rows = observedRows();
    if (rows.length === 0) {
      return (
        `nothing has been observed about ${props.itemRef} — that is "not ` +
        `collected", not "nothing found"`
      );
    }
    const live = provisionalRows().length;
    const unsure = unverifiedRows().length;
    const parts = [`${rows.length} observed`];
    parts.push(
      live > 0
        ? `${live} backed by a session that had not finished when it was ` +
          `observed, so ${live === 1 ? "it is a snapshot" : "they are snapshots"} ` +
          "taken mid-flight and not a settled outcome"
        : "every session behind this evidence had finished when it was observed",
    );
    if (unsure > 0) {
      parts.push(
        `${unsure} unverified — the evidence does not say whether what ` +
          "produced it had finished",
      );
    }
    if (observedTruncated()) {
      parts.push(
        `this is the newest ${OBSERVED_LIMIT} rows in the project and the ` +
          "store has at least that many, so it is a cut list rather than all " +
          "there is",
      );
    }
    return parts.join(" · ");
  });

  const workflowForKind = createMemo(() => {
    const current = item();
    const loaded = workflows();
    if (!current || !loaded || !loaded.ok) return null;
    return loaded.value.workflows.find((flow) => flow.kind === current.kind) ?? null;
  });

  const workflowStates = createMemo<string[]>(() => {
    const flow = workflowForKind();
    if (!flow) return [];
    return [...flow.states];
  });

  const [commentBody, setCommentBody] = createSignal("");
  const [writeBack, setWriteBack] = createSignal<string | null>(null);
  const [template, setTemplate] = createSignal("");
  const [sessionName, setSessionName] = createSignal("");
  const [stopping, setStopping] = createSignal<string | null>(null);

  // -- the inline editor's own state ---------------------------------------

  const [editing, setEditing] = createSignal(false);
  const [draftTitle, setDraftTitle] = createSignal("");
  const [draftPriority, setDraftPriority] = createSignal("p2");
  /** What the server refused, so the rollback can be *stated* and not merely
   *  performed. Holds no value the server rejected as a live field value. */
  const [rolledBack, setRolledBack] = createSignal<string | null>(null);

  const openEditor = () => {
    const current = serverItem();
    if (!current) return;
    // Always the server's state, never the last thing that was typed: a
    // refused value must not come back when the editor is reopened.
    setDraftTitle(current.title);
    setDraftPriority(current.priority);
    setRolledBack(null);
    setEditing(true);
  };

  const editChanged = () => {
    const current = serverItem();
    if (!current) return false;
    return draftTitle().trim() !== current.title || draftPriority() !== current.priority;
  };

  const editReady = () => draftTitle().trim().length > 0 && editChanged();

  const submitEdit = async (reason: string) => {
    const current = serverItem();
    if (!current || !editReady()) return;
    const ref = props.itemRef;
    const title = draftTitle().trim();
    const priority = draftPriority();
    setRolledBack(null);
    // Optimistic: the page reads as the change while Vogt is deciding.
    setOptimistic({ ref, title, priority });
    try {
      const answer = await updateWork({ ref, title, priority, reason });
      const updated = answer?.item;
      setOptimistic(null);
      // The server's answer, not what was typed. If Vogt normalised the
      // title, the page shows Vogt's version.
      if (updated) setAccepted({ ref, item: updated });
      setEditing(false);
      void refetchWork();
    } catch (error) {
      // FR-U12: the optimistic value is discarded here and remembered
      // nowhere. `ReasonForm` renders the server's own sentence; this says
      // what the item went back to, so the rollback is seen and not merely
      // true.
      setOptimistic(null);
      setRolledBack(`${ref} is unchanged: still “${current.title}” at ${current.priority}.`);
      throw error;
    }
  };

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
          <button
            type="button"
            class="wid-edit-open"
            disabled={!item() || Boolean(vogtWritesBlocked())}
            title={
              vogtWritesBlocked() ??
              "Change the title or priority, through a form that collects a reason"
            }
            onClick={() => (editing() ? setEditing(false) : openEditor())}
          >
            {editing() ? "Cancel edit" : "Edit"}
          </button>
          <ViewAgeBadge
            age={viewAge()}
            class="wid-age"
            title="How long ago this page last got an answer from Vogt — not how old the evidence behind the ranking is, which the evidence panel says for itself"
          />
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
              <Show when={optimistic()}>
                <span class="wid-chip wid-chip--unsaved">unsaved — Vogt is deciding</span>
              </Show>
            </div>

            {/* Inline edit (FR-U12), through a view that collects a reason
                (FR-W1, r6). Optimistic above, authoritative below. */}
            <Show when={editing()}>
              <section class="wid-panel wid-edit">
                <div class="wid-panel-head">
                  <h3>Edit this item</h3>
                  <span class="wid-hint">
                    The change renders straight away and is replaced by whatever
                    Vogt answers. A refusal puts it back.
                  </span>
                </div>
                <ReasonForm
                  submitLabel="Save"
                  busyLabel="Saving…"
                  placeholder="Why is this changing?"
                  blockedBy={vogtWritesBlocked()}
                  ready={editReady}
                  onSubmit={submitEdit}
                  onFailure={props.onError}
                >
                  <label class="wid-field">
                    <span>Title</span>
                    <input
                      type="text"
                      value={draftTitle()}
                      disabled={Boolean(vogtWritesBlocked())}
                      onInput={(event) => setDraftTitle(event.currentTarget.value)}
                    />
                  </label>
                  <label class="wid-field">
                    <span>Priority</span>
                    <select
                      value={draftPriority()}
                      disabled={Boolean(vogtWritesBlocked())}
                      onInput={(event) => setDraftPriority(event.currentTarget.value)}
                    >
                      <For each={PRIORITIES}>
                        {(priority) => <option value={priority}>{priority}</option>}
                      </For>
                    </select>
                  </label>
                  <Show when={rolledBack()}>
                    {(note) => (
                      <p class="wid-rolledback" role="status">
                        {note()}
                      </p>
                    )}
                  </Show>
                </ReasonForm>
              </section>
            </Show>

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
                              {/* FR-M1's "session start/approve", and MERGE
                                  §14's M12 demo: a session waiting for input
                                  is the one a push tells somebody about, and
                                  answering it should not require a keyboard
                                  over a PTY on a phone. Offered only while
                                  the engine actually reports it waiting —
                                  `activity` is null when the engine could not
                                  be asked, and a control that answers a
                                  question nobody established exists is the
                                  thing this file's third distinction is
                                  about. */}
                              <Show
                                when={
                                  !session.stopped_at &&
                                  session.activity === "waiting-for-input"
                                }
                              >
                                <AnswerWaitingSession
                                  engineSessionId={session.engine_session_id}
                                  onDone={() => void refetchSessions()}
                                  onFailure={(message) => props.onError?.(message)}
                                />
                              </Show>
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

                {/* The observed store itself (FR-O2, FR-U17). The panel above
                    is the *ranking's* view of this item — inputs, weights, a
                    score — which is a thing computed from evidence and not the
                    evidence. This is what was seen. It is separate because a
                    reader asking "what did anything actually observe about
                    this item?" was, until it existed, being answered with a
                    scoring table. */}
                <section class="wid-panel wid-observed">
                  <div class="wid-panel-head">
                    <h3>Observed evidence</h3>
                    <span class="wid-hint">
                      what collectors saw, before any ranking made anything of it
                    </span>
                  </div>

                  <p class={`wid-freshness wid-freshness--${observedTone()}`}>
                    <Show when={provisionalRows().length > 0}>
                      <strong>provisional</strong> —{" "}
                    </Show>
                    {observedSummary()}
                  </p>

                  <Show when={observedFailure()}>
                    {(message) => (
                      <p class="wid-failure" role="alert">
                        {message()} — nothing is listed below because nothing
                        could be read, not because nothing has been observed
                        about {props.itemRef}.
                      </p>
                    )}
                  </Show>

                  <Show when={!observedFailure() && observed()}>
                    <Show
                      when={observedRows().length > 0}
                      fallback={
                        <p class="wid-absent">
                          No collector has recorded anything about{" "}
                          {props.itemRef}. Evidence appears here once a sweep
                          has looked — an empty panel here is "nobody has
                          looked", not "there is nothing".
                        </p>
                      }
                    >
                      <ul class="wid-observations">
                        <For each={observedRows()}>
                          {(row) => {
                            const state = createMemo(() => settlement(row));
                            return (
                              <li
                                class={`wid-observation wid-observation--${state().tone}`}
                              >
                                <div class="wid-session-head">
                                  <span
                                    class={`wid-settlement wid-settlement--${state().tone}`}
                                    title={state().title}
                                  >
                                    {state().label}
                                  </span>
                                  <span class="wid-mono">{row.kind}</span>
                                  <span class="wid-hint">
                                    observed {formatWhen(row.observed_at)}
                                  </span>
                                </div>
                                <div class="wid-session-meta">
                                  <span class="wid-mono">{row.subject_key}</span>
                                  <span>by {row.collector}</span>
                                  <span>
                                    {payloadText(row, "state") ??
                                      "state not recorded"}
                                  </span>
                                  <span>{exitText(row)}</span>
                                </div>
                                <p class="wid-hint">{state().title}</p>
                              </li>
                            );
                          }}
                        </For>
                      </ul>
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
                  <p class="wid-note">
                    Every write to {props.itemRef} is audited with who, what
                    and why. The browser holds the filters, the paging and the
                    time range, so this links there rather than rendering a
                    second, thinner copy of it.
                  </p>
                  <a
                    class="wid-action"
                    href={`#/audit?ref=${encodeURIComponent(props.itemRef)}`}
                  >
                    Open the audit trail for {props.itemRef}
                  </a>
                  <p class="wid-absent">
                    Comments are audited against the comment, not the item, so
                    that view shows creates, transitions and updates — not the
                    comment history above it. Closing that gap is a server-side
                    change, not a filter this link is missing.
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
