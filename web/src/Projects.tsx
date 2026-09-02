// Per-project pages, the dependency graph, the import form, and the drift
// inbox (FR-U7, FR-U18, FR-U21, FR-U11).
//
// Five rules this file exists to keep, in the order they are easy to lose:
//
//   1. **Evidence comes before the act.** FR-U18 is unusually specific: a
//      proposal shows both sides of its disagreement, with provenance and
//      age, *before* any act is possible. So the evidence panel is not a
//      disclosure — it is rendered open, for every proposal, and the resolve
//      controls sit below it and are refused outright when the proposal
//      carries no evidence to weigh. A list of "accept?" buttons is how an
//      estate quietly gets rewritten by whatever a collector saw last.
//   2. **Bulk accept does not exist.** `REQUIREMENTS.md` §3 defers it by
//      name. There is no select-all here, no multi-select, and nothing that
//      resolves more than one proposal per act — `resolveDrift` is singular
//      by signature and this file never loops over it.
//   3. **A write says who asked for it.** Resolving a proposal and importing
//      a repository both collect a reason the *user* typed, and neither can
//      submit without one (FR-W1, r6). Nothing here composes a reason,
//      prefills one, or lets a placeholder double as a value.
//   4. **Absence is stated, never rendered as emptiness.** Freshness on
//      every aggregate — the brief, the drift inbox and the dependency graph
//      each carry their own, because they are aggregates over different
//      sweeps — a trust state that is never blank, and an unreachable Vogt
//      that says so with the server's own reason rather than showing a
//      project with no drift (FR-U2, FR-U17, FR-U21).
//   5. **A view is a place.** The selected project, the sub-view and the
//      drift filters live in the URL, so a reload restores them and a link
//      carries them to somebody else (FR-U11).
//
// Everything reaches Vogt through `vogtApi.ts`; there is no fetch in this
// file and there must not be one — `tests/test_pwa.py` is what says so.

import {
  Component,
  For,
  Show,
  createEffect,
  createMemo,
  createResource,
  createSignal,
  onCleanup,
} from "solid-js";
import { useNavigate, useSearchParams } from "@solidjs/router";
import {
  VogtUnavailable,
  compliance,
  deps,
  importProject,
  listDrift,
  listForgeRepos,
  projectBrief,
  resolveDrift,
  type ComplianceResult,
  type DepRef,
  type ForgeRepoView,
  type DepsResult,
  type DriftListResult,
  type DriftProposal,
  type FreshnessSummary,
} from "./vogtApi";
import { openWorkItemTab } from "./tabs";
import SurfaceHeader from "./SurfaceHeader";
import { safeHref } from "./markdown";
import {
  ViewAgeBadge,
  createLoadStamp,
  createViewAge,
  honestyToneClass,
  onVogtLive,
} from "./viewAge";
import {
  filterAndSortProjects,
  type ProjectSort,
} from "./projectRegistry";
import { taxonomy } from "./taxonomyCache";

interface Props {
  onError?: (message: string) => void;
}

/** The URL keys this surface owns. Anything else in the query is left alone. */
const URL_KEYS = ["p", "view", "status", "kind"] as const;

type ViewName = "overview" | "deps" | "drift" | "import";

const VIEWS: readonly ViewName[] = ["overview", "deps", "drift", "import"];

/** `DriftListParams.status` is a closed set server-side. */
const DRIFT_STATUSES = ["open", "accepted", "rejected", "contested"] as const;

/** `DriftListParams.limit` is capped at 500; a page of 200 is the honest ask. */
const DRIFT_LIMIT = 200;

/** `ProjectListParams.limit` is capped at 200. */
const PROJECT_LIMIT = 200;

type Resolution = "accepted" | "rejected" | "contested";

const RESOLUTIONS: readonly { value: Resolution; label: string; note: string }[] = [
  {
    value: "accepted",
    label: "Accept",
    note: "agrees with the observed side, and writes the proposed change if the kind carries one",
  },
  {
    value: "rejected",
    label: "Reject",
    note: "agrees with the declared side; nothing is written to the estate",
  },
  {
    value: "contested",
    label: "Leave contested",
    note: "records that the disagreement is real and unresolved — a decision, not a deferral",
  },
];

// -- reading what the API actually sends ------------------------------------
//
// The shapes in `vogtApi.ts` are deliberately partial, and the project brief
// is returned as an open record. The readers below take a response as data
// and check each field at runtime, which is honest in a way a cast is not: a
// field the server stops sending becomes "not reported" here rather than
// `undefined` rendered as a blank.

function record(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" ? (value as Record<string, unknown>) : {};
}

function readString(source: unknown, key: string): string | null {
  const value = record(source)[key];
  return typeof value === "string" && value ? value : null;
}

function readNumber(source: unknown, key: string): number | null {
  const value = record(source)[key];
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

function readBoolean(source: unknown, key: string): boolean | null {
  const value = record(source)[key];
  return typeof value === "boolean" ? value : null;
}

function readStringMap(source: unknown, key: string): [string, string][] {
  const value = record(source)[key];
  if (!value || typeof value !== "object") return [];
  return Object.entries(value as Record<string, unknown>)
    .filter((entry): entry is [string, string] => typeof entry[1] === "string")
    .sort((a, b) => a[0].localeCompare(b[0]));
}

function readCounts(source: unknown, key: string): [string, number][] {
  const value = record(source)[key];
  if (!value || typeof value !== "object") return [];
  return Object.entries(value as Record<string, unknown>)
    .filter((entry): entry is [string, number] => typeof entry[1] === "number")
    .sort((a, b) => a[0].localeCompare(b[0]));
}

function readStringList(source: unknown, key: string): string[] {
  const value = record(source)[key];
  return Array.isArray(value) ? value.filter((item): item is string => typeof item === "string") : [];
}

/** Any JSON scalar, rendered as the text a reader can act on. */
function scalar(value: unknown): string {
  if (value === null) return "null";
  if (value === undefined) return "—";
  if (typeof value === "string") return value;
  if (typeof value === "number" || typeof value === "boolean") return String(value);
  return JSON.stringify(value);
}

// -- freshness, trust and age, which never render blank ---------------------

interface Described {
  status: string;
  text: string;
  collectors: [string, string][];
}

/**
 * How old the evidence behind one aggregate is (FR-U2, FR-U17).
 *
 * Rendered even when it is good news, for the reason the legacy GUI gives:
 * the value of the line is that an empty answer and a stale answer stop
 * looking alike. An inbox with no drift in it is reassuring only if
 * something has looked recently.
 *
 * Three of these appear on this surface rather than one, because the brief,
 * the drift inbox and the dependency graph aggregate over different sweeps
 * and a single banner would be claiming a freshness none of them has.
 */
function describeFreshness(freshness: FreshnessSummary | undefined | null): Described {
  if (!freshness || typeof freshness !== "object") {
    return { status: "unknown", text: "freshness: not reported", collectors: [] };
  }
  const status = readString(freshness, "status") ?? "never_swept";
  const detail = readString(freshness, "detail");
  const parts: string[] = [];
  if (status === "never_swept") {
    parts.push("nothing has been swept yet — this is 'not collected', not 'nothing found'");
  } else {
    parts.push(`evidence is ${describeAge(readNumber(freshness, "age_seconds"))} old at its oldest`);
    if (status === "partial") parts.push("at least one collector did not complete");
  }
  if (detail) parts.push(detail);
  return { status, text: parts.join(" · "), collectors: readStringMap(freshness, "collectors") };
}

function describeAge(seconds: number | null): string {
  if (seconds === null) return "an unknown time";
  if (seconds < 90) return `${seconds}s`;
  if (seconds < 5400) return `${Math.round(seconds / 60)}m`;
  if (seconds < 172800) return `${Math.round(seconds / 3600)}h`;
  return `${Math.round(seconds / 86400)}d`;
}

/** How long ago a timestamp was, or a stated inability to say. */
function ageOf(value: string | null | undefined): string {
  if (!value) return "age unknown";
  const at = new Date(value);
  if (Number.isNaN(at.valueOf())) return "age unknown";
  return `${describeAge(Math.max(0, Math.round((Date.now() - at.valueOf()) / 1000)))} ago`;
}

function formatWhen(value: string | null | undefined): string {
  if (!value) return "—";
  const at = new Date(value);
  return Number.isNaN(at.valueOf()) ? String(value) : at.toISOString().replace("T", " ").slice(0, 19);
}

/**
 * Trust, wherever a thing carries one (FR-U2, FR-U17).
 *
 * `unverified` rather than a blank, always: a blank says "no opinion", and
 * the honest answer is "nobody has verified this".
 */
function trustOf(value: unknown): string {
  return typeof value === "string" && value ? value : "unverified";
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

// -- results, tagged rather than thrown -------------------------------------
//
// A thrown resource error is a resource whose value cannot be read without
// rethrowing, and an outage is something this surface renders rather than
// escalates (FR-U21). So the fetchers return the failure as a value.

type Loaded<T> =
  | { ok: true; value: T }
  | { ok: false; unavailable: boolean; message: string };

async function attempt<T>(work: () => Promise<T>): Promise<Loaded<T>> {
  try {
    return { ok: true, value: await work() };
  } catch (error) {
    return {
      ok: false,
      unavailable: error instanceof VogtUnavailable,
      message: errorMessage(error),
    };
  }
}

// -- the two sides of a disagreement ----------------------------------------

interface Side {
  /** What this side is: the estate's record, or what a collector saw. */
  heading: string;
  /** The value in dispute, or a stated reason there is no single value. */
  value: string;
  /** Whether `value` is a real value or a statement that none is carried. */
  carried: boolean;
  /** Where it came from — store and field, or collector and subject. */
  provenance: string[];
  /** When it was true, if that is knowable. */
  observedAt: string | null;
  /** How this side is dated when it has no timestamp of its own. */
  ageNote: string | null;
}

interface Sides {
  declared: Side;
  observed: Side;
  /** What accepting would write, in words rather than as a JSON blob. */
  effect: string;
}

const NOT_CARRIED = "not carried on this proposal";

/**
 * Both halves of one proposal's disagreement, with provenance and age.
 *
 * Per kind, because the halves live in different places per kind and a
 * generic `from`/`to` renderer would silently show a blank for the three
 * kinds that do not have them. Where a half genuinely is not on the
 * proposal, this says so and names where the value does live — which is the
 * difference between an honest gap and an empty cell.
 *
 * Nothing here parses the server's `summary`: it is rendered verbatim above
 * the panel, and re-deriving its contents in the client would be a second
 * copy of a rule the server owns.
 */
function describeSides(proposal: DriftProposal): Sides {
  const change = record(proposal.proposed_change);
  const snapshot = record(proposal.evidence_snapshot);
  const payload = record(snapshot["payload"]);
  const collector = readString(snapshot, "collector");
  const subject = readString(snapshot, "subject_key");
  const digest = readString(snapshot, "content_digest");
  const observedAt = readString(snapshot, "observed_at");

  const seenBy: string[] = [];
  seenBy.push(collector ? `collector: ${collector}` : "collector: not recorded");
  if (subject) seenBy.push(`subject: ${subject}`);
  seenBy.push(
    digest
      ? `digest: ${digest.slice(0, 16)}`
      : "digest: none — this finding is an absence, not a document",
  );
  seenBy.push(
    proposal.evidence_observation_id
      ? `observation: ${proposal.evidence_observation_id}`
      : "observation: none retained — the snapshot below is the whole of it",
  );

  const declaredStore = (field: string): string[] => [
    "store: this instance's declared state",
    `field: ${field}`,
  ];

  switch (proposal.kind) {
    case "version_mismatch": {
      const from = change["from"];
      return {
        declared: {
          heading: "Declared here",
          value: from === null || from === undefined ? "no version declared" : scalar(from),
          carried: true,
          provenance: declaredStore(`project.current_version (${proposal.project_slug ?? proposal.subject_id})`),
          observedAt: null,
          ageNote: "declared state has no sweep age; it is true until somebody writes over it",
        },
        observed: {
          heading: "Observed by a collector",
          value: scalar(change["to"]),
          carried: true,
          provenance: seenBy,
          observedAt,
          ageNote: null,
        },
        effect: `Accepting writes current_version = ${scalar(change["to"])} on this project.`,
      };
    }
    case "forge_state_mismatch": {
      const ref = readString(change, "work_ref") ?? proposal.subject_id;
      return {
        declared: {
          heading: "Declared here",
          value: scalar(change["from"]),
          carried: change["from"] !== undefined,
          provenance: declaredStore(`work_item.state (${ref})`),
          observedAt: null,
          ageNote: "declared state has no sweep age; it is true until somebody writes over it",
        },
        observed: {
          heading: "Observed upstream",
          value: scalar(payload["state"] ?? NOT_CARRIED),
          carried: payload["state"] !== undefined,
          provenance: seenBy,
          observedAt,
          ageNote: null,
        },
        effect: `Accepting transitions ${ref} to ${scalar(change["to"])}.`,
      };
    }
    case "unresolved_dependency": {
      const manifest = readString(change, "manifest");
      return {
        declared: {
          heading: "Declared here",
          value: "no registered project matches this reference",
          carried: true,
          provenance: [
            "store: this instance's declared state",
            "field: the project registry, searched for a match",
            "note: a reference is only unresolved against the projects that exist",
          ],
          observedAt: null,
          ageNote: "true as of the registry now, not as of the sweep",
        },
        observed: {
          heading: "Observed in the tree",
          value: scalar(change["raw_target"]),
          carried: change["raw_target"] !== undefined,
          provenance: manifest ? [...seenBy, `manifest: ${manifest}`] : seenBy,
          observedAt,
          ageNote: null,
        },
        effect:
          "Accepting writes nothing. It records the judgement that the target is " +
          "not a project — which is usually a project nobody has registered yet.",
      };
    }
    case "vanished_upstream": {
      const ref = readString(change, "work_ref") ?? proposal.subject_id;
      return {
        declared: {
          heading: "Declared here",
          value: `${ref} is linked to ${readString(change, "subject_key") ?? subject ?? "an upstream object"}`,
          carried: true,
          provenance: declaredStore(`work_item link (${ref})`),
          observedAt: null,
          ageNote: "declared state has no sweep age; it is true until somebody writes over it",
        },
        observed: {
          heading: "Observed: an absence",
          value: "a completed sweep did not find it",
          carried: true,
          provenance: [
            ...seenBy,
            "this is absence inside provably swept scope, not 'not collected'",
          ],
          observedAt,
          ageNote: null,
        },
        effect:
          "Accepting writes nothing. It records the judgement that the upstream " +
          "object is gone — and a repo transfer or a permissions change looks " +
          "identical from here.",
      };
    }
    case "ci_red_vs_healthy": {
      const failing = readStringList(change, "failing");
      const revision = readString(change, "revision");
      return {
        declared: {
          heading: "Declared here",
          value: NOT_CARRIED,
          carried: false,
          provenance: [
            "store: this instance's declared state",
            `field: project.lifecycle_state (${proposal.project_slug ?? proposal.subject_id})`,
            "the summary above states it; the project's overview shows it live",
          ],
          observedAt: null,
          ageNote: null,
        },
        observed: {
          heading: "Observed by a collector",
          value: failing.length
            ? `${failing.length} failing check(s): ${failing.join(", ")}`
            : "failing checks not carried",
          carried: failing.length > 0,
          provenance: revision ? [...seenBy, `revision: ${revision}`] : seenBy,
          observedAt,
          ageNote: null,
        },
        effect:
          "Accepting writes nothing. A red build is a fact about the build, not " +
          "a decision about the project's lifecycle state.",
      };
    }
    case "update_automation_gap": {
      const missing = readStringList(change, "missing");
      return {
        declared: {
          heading: "Declared here",
          value: "nothing — Vogt declares no repository settings",
          carried: true,
          provenance: [
            "store: none",
            "this kind has no declared counterpart; the disagreement is with an expectation",
          ],
          observedAt: null,
          ageNote: null,
        },
        observed: {
          heading: "Observed on the repository",
          value: missing.length ? `off: ${missing.join(", ")}` : "no gap carried",
          carried: missing.length > 0,
          provenance: seenBy,
          observedAt,
          ageNote: null,
        },
        effect:
          "Accepting writes nothing here and changes nothing upstream. Turning a " +
          "toggle on is a change to the repository's settings.",
      };
    }
    case "referenced_issue_state_mismatch": {
      const ref = readString(change, "work_ref") ?? proposal.subject_id;
      const key = readString(change, "subject_key") ?? subject ?? "an upstream issue";
      return {
        declared: {
          heading: "Declared here",
          value: scalar(change["declared_state"] ?? NOT_CARRIED),
          carried: change["declared_state"] !== undefined,
          provenance: [
            ...declaredStore(`work_item.state (${ref})`),
            `the reference was read from ${ref}'s own title or body, not adopted as a link`,
          ],
          observedAt: null,
          ageNote: "declared state has no sweep age; it is true until somebody writes over it",
        },
        observed: {
          heading: "Observed upstream",
          value: scalar(payload["state"] ?? change["upstream_state"] ?? NOT_CARRIED),
          carried: payload["state"] !== undefined || change["upstream_state"] !== undefined,
          provenance: [...seenBy, `issue: ${key}`],
          observedAt,
          ageNote: null,
        },
        effect:
          "Accepting writes nothing. It records the judgement that the two " +
          "registers disagreed — closing the issue, or reopening the item, is " +
          "an act somebody takes deliberately.",
      };
    }
    default: {
      // A kind this build has not seen. Rendering the halves generically is
      // better than rendering nothing, and saying which half is a guess is
      // better than implying both were understood.
      const from = change["from"];
      const to = change["to"];
      return {
        declared: {
          heading: "Declared here",
          value: from === undefined ? NOT_CARRIED : scalar(from),
          carried: from !== undefined,
          provenance: [
            "store: this instance's declared state",
            `entity: ${scalar(change["entity"] ?? proposal.subject_kind)}`,
            "this GUI does not know this drift kind; the halves are read generically",
          ],
          observedAt: null,
          ageNote: null,
        },
        observed: {
          heading: "Observed by a collector",
          value: to === undefined ? NOT_CARRIED : scalar(to),
          carried: to !== undefined,
          provenance: seenBy,
          observedAt,
          ageNote: null,
        },
        effect:
          "This GUI does not know what accepting this kind writes. The raw " +
          "proposed change is below.",
      };
    }
  }
}

/**
 * Whether there is anything to weigh.
 *
 * A proposal with no snapshot cannot show both sides, and FR-U18 makes
 * showing them the precondition for acting. So the resolve controls are
 * refused rather than offered, with this as the stated reason.
 */
function evidenceIsShowable(proposal: DriftProposal): boolean {
  const snapshot = record(proposal.evidence_snapshot);
  return Boolean(readString(snapshot, "collector") || readString(snapshot, "observed_at"));
}

// -- one proposal, evidence first -------------------------------------------

const DriftCard: Component<{
  proposal: DriftProposal;
  gate: string | null;
  onResolved: (message: string) => void;
  onFailure: (message: string) => void;
  onOpenProject: (slug: string) => void;
  /** Whether this card is holding a reason somebody has started typing. */
  onDrafting?: (id: string, drafting: boolean) => void;
}> = (props) => {
  const [resolution, setResolution] = createSignal<Resolution>("accepted");
  const [reason, setReason] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [failure, setFailure] = createSignal<string | null>(null);

  // The inbox re-reads when Vogt announces a change, and a re-read replaces
  // every proposal object — which would take a half-written reason with it.
  // So a card says when it is holding one, and the live re-read waits.
  createEffect(() => {
    props.onDrafting?.(props.proposal.id, reason().trim().length > 0 || busy());
  });
  onCleanup(() => props.onDrafting?.(props.proposal.id, false));

  const sides = createMemo(() => describeSides(props.proposal));
  const showable = createMemo(() => evidenceIsShowable(props.proposal));
  const open = () => props.proposal.status === "open";
  const payload = createMemo(() =>
    Object.entries(record(record(props.proposal.evidence_snapshot)["payload"])).sort((a, b) =>
      a[0].localeCompare(b[0]),
    ),
  );
  const change = createMemo(() =>
    Object.entries(record(props.proposal.proposed_change)).sort((a, b) => a[0].localeCompare(b[0])),
  );

  const blockedBy = createMemo(() => {
    if (!open()) return `Already ${props.proposal.status}; a proposal is resolved once.`;
    if (!showable()) {
      return (
        "This proposal carries no evidence snapshot, so both sides of the " +
        "disagreement cannot be shown — and it cannot be resolved from here. " +
        "Resolve it with the CLI, where the operator can go and look."
      );
    }
    return null;
  });

  const canSubmit = () => !blockedBy() && !busy() && reason().trim().length > 0;

  const submit = async (event: Event) => {
    event.preventDefault();
    if (!canSubmit()) return;
    setBusy(true);
    setFailure(null);
    try {
      const result = await resolveDrift(props.proposal.id, resolution(), reason().trim());
      setReason("");
      props.onResolved(
        `${props.proposal.id} ${resolution()}` +
          (result.change_applied
            ? " — the proposed change was written."
            : " — nothing was written; the resolution is the record."),
      );
    } catch (error) {
      const message = errorMessage(error);
      setFailure(message);
      props.onFailure(`Resolving ${props.proposal.id} was refused: ${message}`);
    } finally {
      setBusy(false);
    }
  };

  const renderSide = (side: () => Side) => (
    <div class={`vogt-projects-side${side().carried ? "" : " vogt-projects-side--absent"}`}>
      <h5>{side().heading}</h5>
      <p class="vogt-projects-side-value">{side().value}</p>
      <ul class="vogt-projects-provenance">
        <For each={side().provenance}>{(line) => <li>{line}</li>}</For>
      </ul>
      <p class="vogt-projects-age">
        <Show
          when={side().observedAt}
          fallback={
            <span>{side().ageNote ?? "no timestamp of its own"}</span>
          }
        >
          {(at) => (
            <span>
              seen {ageOf(at())} · {formatWhen(at())}
            </span>
          )}
        </Show>
      </p>
    </div>
  );

  return (
    <article class={`vogt-projects-drift vogt-projects-drift--${props.proposal.status}`}>
      <header class="vogt-projects-drift-head">
        <span class="vogt-projects-kind">{props.proposal.kind}</span>
        <span class="vogt-projects-drift-status">{props.proposal.status}</span>
        <Show when={props.proposal.project_slug}>
          {(slug) => (
            <button
              type="button"
              class="vogt-projects-link"
              onClick={() => props.onOpenProject(slug())}
            >
              {slug()}
            </button>
          )}
        </Show>
        <span class="vogt-projects-muted">
          raised {ageOf(props.proposal.opened_at)} · {formatWhen(props.proposal.opened_at)}
        </span>
        <span class="vogt-projects-mono vogt-projects-muted">{props.proposal.id}</span>
      </header>

      <p class="vogt-projects-drift-summary">{props.proposal.summary}</p>

      {/* Superseded is a reading aid, not a resolution (FR-R6). The proposal
          is still open and still needs a person; what the flag says is that a
          sweep newer than the proposal stopped reproducing the condition, so
          this is worth reading before the ones without it. */}
      <Show when={props.proposal.superseded_at}>
        {(at) => (
          <p class="vogt-projects-superseded">
            <strong>Superseded by fresher evidence</strong> · marked {ageOf(at())} ·{" "}
            {formatWhen(at())}
            <br />
            {props.proposal.superseded_detail ??
              "a later sweep no longer reproduces the condition that raised this"}
            <br />
            Still open, and still yours to resolve — the evidence below is what it was
            raised on.
          </p>
        )}
      </Show>

      {/* The evidence, rendered open. Not a disclosure: FR-U18 makes seeing
          both sides the precondition for acting, and a panel the reader can
          skip is a panel most readers will. */}
      <div class="vogt-projects-sides">
        {renderSide(() => sides().declared)}
        {renderSide(() => sides().observed)}
      </div>

      <Show when={payload().length}>
        <details class="vogt-projects-raw">
          <summary>The collector's record, verbatim ({payload().length} field(s))</summary>
          <dl class="vogt-projects-kv">
            <For each={payload()}>
              {([key, value]) => (
                <div>
                  <dt>{key}</dt>
                  <dd class="vogt-projects-mono">{scalar(value)}</dd>
                </div>
              )}
            </For>
          </dl>
        </details>
      </Show>

      <p class="vogt-projects-effect">{sides().effect}</p>

      <Show when={change().length}>
        <details class="vogt-projects-raw">
          <summary>The proposed change, verbatim</summary>
          <dl class="vogt-projects-kv">
            <For each={change()}>
              {([key, value]) => (
                <div>
                  <dt>{key}</dt>
                  <dd class="vogt-projects-mono">{scalar(value)}</dd>
                </div>
              )}
            </For>
          </dl>
        </details>
      </Show>

      {/* The gate note is quoted from the API rather than restated here:
          two copies of a policy is one copy too many. */}
      <p class="vogt-projects-gate">
        <Show
          when={props.gate}
          fallback={
            <span>
              Auto-acceptable under the shipped low-risk policy — an agent may accept this
              without a human. Accepting it here still records the reason you type.
            </span>
          }
        >
          {(note) => <span>Human-gated: {note()}</span>}
        </Show>
      </p>

      <Show
        when={!blockedBy()}
        fallback={
          <p class="vogt-projects-blocked">
            {blockedBy()}
            <Show when={props.proposal.resolution_reason}>
              {(why) => (
                <span class="vogt-projects-muted">
                  {" "}
                  Reason given: “{why()}”
                  <Show when={props.proposal.resolved_by_identity_ref}>
                    {(who) => <> — {who()}</>}
                  </Show>
                </span>
              )}
            </Show>
          </p>
        }
      >
        {/* One proposal, one form, one act. There is no select-all and no
            batch submit anywhere on this surface: bulk drift resolution is
            deferred by name in `REQUIREMENTS.md` §3, because an acceptance
            is a declared-state write and carries its own reason. */}
        <form class="vogt-projects-resolve" onSubmit={(event) => void submit(event)}>
          <div class="vogt-projects-resolutions" role="radiogroup" aria-label="Resolution">
            <For each={RESOLUTIONS}>
              {(option) => (
                <label
                  class={`vogt-projects-resolution${
                    resolution() === option.value ? " on" : ""
                  }`}
                  title={option.note}
                >
                  <input
                    type="radio"
                    name={`resolution-${props.proposal.id}`}
                    value={option.value}
                    checked={resolution() === option.value}
                    onChange={() => setResolution(option.value)}
                  />
                  <span>{option.label}</span>
                  <span class="vogt-projects-muted">{option.note}</span>
                </label>
              )}
            </For>
          </div>
          <label class="vogt-projects-field">
            <span>Reason for this resolution (audited)</span>
            <input
              type="text"
              value={reason()}
              placeholder="Why is this the right answer?"
              onInput={(event) => setReason(event.currentTarget.value)}
            />
          </label>
          <div class="vogt-projects-resolve-actions">
            <button type="submit" disabled={!canSubmit()}>
              {busy()
                ? "Resolving…"
                : `${RESOLUTIONS.find((r) => r.value === resolution())?.label ?? "Resolve"} this proposal`}
            </button>
            <span class="vogt-projects-hint">
              Vogt records who asked and why; a resolution without a reason is refused.
            </span>
          </div>
          <Show when={failure()}>{(message) => <p class="vogt-projects-failure">{message()}</p>}</Show>
        </form>
      </Show>
    </article>
  );
};

// -- the URL, which is where a view lives (FR-U11) --------------------------

type Query = Partial<Record<(typeof URL_KEYS)[number], string | string[]>>;

function one(value: string | string[] | undefined): string {
  if (Array.isArray(value)) return value[0] ?? "";
  return value ?? "";
}

interface Place {
  project: string;
  view: ViewName;
  status: string;
  kind: string;
}

function placeFromQuery(query: Query): Place {
  const named = one(query.view);
  const view = VIEWS.find((candidate) => candidate === named);
  return {
    project: one(query.p),
    view: view ?? "overview",
    status: one(query.status) || "open",
    kind: one(query.kind),
  };
}

function queryFor(place: Place): Record<(typeof URL_KEYS)[number], string | null> {
  return {
    p: place.project || null,
    view: place.view === "overview" ? null : place.view,
    status: place.status === "open" ? null : place.status,
    kind: place.kind || null,
  };
}

/** The canonical text of this surface's slice of the query. Both encoders
 *  walk `URL_KEYS` in the same order, so two equal states always produce the
 *  same string — which is what lets one effect tell "the user moved" from
 *  "somebody handed us a different URL". */
function encodePlace(place: Place): string {
  const params = new URLSearchParams();
  const desired = queryFor(place);
  for (const key of URL_KEYS) {
    const value = desired[key];
    if (value !== null) params.append(key, value);
  }
  return params.toString();
}

function encodeQuery(query: Query): string {
  const params = new URLSearchParams();
  for (const key of URL_KEYS) {
    const value = query[key];
    if (value === undefined) continue;
    for (const item of Array.isArray(value) ? value : [value]) params.append(key, item);
  }
  return params.toString();
}

// -- the surface ------------------------------------------------------------

const Projects: Component<Props> = (props) => {
  const navigate = useNavigate();
  const [query, setQuery] = useSearchParams<Query>();

  // The URL is read once and written from the signal afterwards, for the
  // reason `Backlog.tsx` gives at length: activating a tab navigates to the
  // tab's bare path and drops the query, so a URL-as-truth surface would
  // forget which project was open every time the reader looked at a terminal.
  const [place, setPlace] = createSignal<Place>(placeFromQuery(query));
  const [reloadKey, setReloadKey] = createSignal(0);
  const refresh = () => setReloadKey((value) => value + 1);
  const [note, setNote] = createSignal<string | null>(null);

  let lastWritten = encodeQuery(query);

  createEffect(() => {
    const desired = encodePlace(place());
    const current = encodeQuery(query);
    if (desired === current) {
      lastWritten = current;
      return;
    }
    if (current !== lastWritten && current !== "") {
      setPlace(placeFromQuery(query));
      lastWritten = current;
      return;
    }
    setQuery(queryFor(place()), { replace: true });
    lastWritten = desired;
  });

  const move = <K extends keyof Place>(key: K, value: Place[K]) => {
    setPlace({ ...place(), [key]: value });
  };

  const openProject = (slug: string) => {
    setPlace({ ...place(), project: slug, view: "overview" });
    setNote(null);
  };

  /** The current project's slug, URL-encoded for a `?project=` cross-link. */
  const projectQuery = () => encodeURIComponent(place().project);

  // -- the estate list ------------------------------------------------------

  const [projects, { refetch: refetchProjects }] = createResource(
    () => reloadKey(),
    () => attempt(() => taxonomy.projects({ limit: PROJECT_LIMIT })),
  );

  const projectRows = createMemo(() => {
    const result = projects();
    if (!(result && result.ok)) return [];
    return result.value.projects as unknown as Record<string, unknown>[];
  });

  // The registry is a place to find a project, so it filters and sorts (#227).
  // Both are client-side: `project.list` already returns every registered
  // project, so this is presentation, not another query.
  const [projectSearch, setProjectSearch] = createSignal("");
  const [projectSort, setProjectSort] = createSignal<ProjectSort>("name");

  const shownProjectRows = createMemo(() =>
    filterAndSortProjects(projectRows(), projectSearch(), projectSort()),
  );

  const projectOutage = createMemo(() => {
    const result = projects();
    return result && !result.ok ? result : null;
  });

  // -- the brief (FR-U7) ----------------------------------------------------

  const briefKey = createMemo(() => {
    const current = place();
    return current.project ? { slug: current.project, reload: reloadKey() } : null;
  });

  const [brief] = createResource(briefKey, (key) => attempt(() => projectBrief(key.slug)));

  const briefValue = createMemo(() => {
    const result = brief();
    return result && result.ok ? (result.value as Record<string, unknown>) : null;
  });

  const briefOutage = createMemo(() => {
    const result = brief();
    return result && !result.ok ? result : null;
  });

  // Compliance is asked for separately because the brief carries only the
  // status and its date; the failing criteria — the part that says what to
  // fix — live on `compliance`, which also states the answer's own age and
  // never refreshes it implicitly.
  const [contract] = createResource(briefKey, (key) => attempt(() => compliance(key.slug)));

  const contractValue = createMemo<ComplianceResult | null>(() => {
    const result = contract();
    return result && result.ok ? result.value : null;
  });

  const contractFailure = createMemo(() => {
    const result = contract();
    return result && !result.ok ? result.message : null;
  });

  // -- the dependency graph (FR-U7) -----------------------------------------

  const depsKey = createMemo(() => {
    const current = place();
    return current.project && current.view === "deps"
      ? { slug: current.project, reload: reloadKey() }
      : null;
  });

  const [graph] = createResource(depsKey, (key) => attempt(() => deps(key.slug)));

  const graphValue = createMemo<DepsResult | null>(() => {
    const result = graph();
    return result && result.ok ? result.value : null;
  });

  const graphOutage = createMemo(() => {
    const result = graph();
    return result && !result.ok ? result : null;
  });

  // -- the drift inbox (FR-U18) ---------------------------------------------

  const driftKey = createMemo(() => {
    const current = place();
    if (current.view !== "drift") return null;
    return {
      project: current.project,
      status: current.status,
      kind: current.kind,
      reload: reloadKey(),
    };
  });

  const [drift, { refetch: refetchDrift }] = createResource(driftKey, (key) =>
    attempt(() =>
      listDrift({
        limit: DRIFT_LIMIT,
        status: key.status,
        kind: key.kind || undefined,
        project: key.project || undefined,
      }),
    ),
  );

  const driftValue = createMemo<DriftListResult | null>(() => {
    const result = drift();
    return result && result.ok ? result.value : null;
  });

  const driftOutage = createMemo(() => {
    const result = drift();
    return result && !result.ok ? result : null;
  });

  const proposals = createMemo<DriftProposal[]>(() => driftValue()?.proposals ?? []);

  const gates = createMemo(() => driftValue()?.human_gated ?? {});

  // -- drift arrives rather than being asked for (FR-U10) -------------------
  //
  // A drift proposal is raised by a sweep, not by anybody looking at this
  // page, and until now the only way to find out was to press Refresh — so
  // the inbox was as current as the last time somebody wondered whether it
  // was. Raising one publishes `drift.raised` onto vogt-core's event feed,
  // the front door republishes that onto the stream this client already has
  // open, and the read below is the same one the filter key makes.
  //
  // Guarded twice. Only the drift view re-reads: the brief and the dependency
  // graph are per-project aggregates over sweeps, and re-pulling four panels
  // on every announced work-item transition would be a poll wearing an
  // event's clothes. And a card holding a half-typed reason stops the
  // re-read outright — the answer replaces every proposal object, and losing
  // somebody's sentence to a background refresh is exactly the write FR-W1
  // makes them type.
  const [drafting, setDrafting] = createSignal<string[]>([]);

  const noteDrafting = (id: string, active: boolean) =>
    setDrafting((current) => {
      const held = current.includes(id);
      if (active === held) return current;
      return active ? [...current, id] : current.filter((one) => one !== id);
    });

  onVogtLive(() => void refetchDrift(), {
    when: () => place().view === "drift" && drafting().length === 0,
  });

  // -- how old this view is (FR-U10) ----------------------------------------
  //
  // Per view, because the panels are separate reads: the drift inbox's age is
  // not the brief's, and one badge covering both would be wrong about
  // whichever was older. Distinct again from the freshness lines inside the
  // panels, which say how old the *evidence* is — this one says how long ago
  // this tab last spoke to Vogt at all.
  const driftLoadedAt = createLoadStamp(drift, (result) => result.ok);
  const briefLoadedAt = createLoadStamp(brief, (result) => result.ok);
  const graphLoadedAt = createLoadStamp(graph, (result) => result.ok);
  const listLoadedAt = createLoadStamp(projects, (result) => result.ok);

  /** Which read the badge is about: the one this view is showing. */
  const shownRead = createMemo(() => {
    const view = place().view;
    if (view === "drift") return { at: driftLoadedAt(), failure: driftOutage() };
    if (view === "deps") return { at: graphLoadedAt(), failure: graphOutage() };
    if (view === "overview" && place().project) {
      return { at: briefLoadedAt(), failure: briefOutage() };
    }
    // The estate list, which is what "overview with no project" and the
    // import form are both looking at.
    return { at: listLoadedAt(), failure: projectOutage() };
  });

  const viewAge = createViewAge(() => {
    const shown = shownRead();
    return {
      loadedAt: shown.at,
      outage: shown.failure?.unavailable ? shown.failure.message : null,
      failed: Boolean(shown.failure),
      // Only the inbox re-reads on the stream, so only the inbox may claim it.
      live: place().view === "drift",
    };
  });

  /**
   * Slugs the inbox's project picker offers.
   *
   * The slug named in the URL is always one of them, even before the estate
   * list has answered: Solid re-applies a `<select>`'s value when the
   * expression's dependencies change, and a value with no matching `<option>`
   * is silently dropped by the browser — which would show "Every project"
   * while a filter was in force. Touching the list here makes the re-apply
   * happen once the options exist.
   */
  const driftProjectOptions = createMemo(() => {
    const seen = new Set<string>();
    for (const row of projectRows()) {
      const slug = readString(row, "slug");
      if (slug) seen.add(slug);
    }
    for (const proposal of proposals()) {
      if (proposal.project_slug) seen.add(proposal.project_slug);
    }
    if (place().project) seen.add(place().project);
    return [...seen].sort();
  });

  /** Kinds present in this answer, so the filter offers what exists rather
   *  than a hard-coded list this GUI would have to keep in step. */
  const driftKinds = createMemo(() => {
    const seen = new Set(proposals().map((proposal) => proposal.kind));
    if (place().kind) seen.add(place().kind);
    return [...seen].sort();
  });

  // -- the import form (FR-U3, FR-P6) ---------------------------------------

  const [repo, setRepo] = createSignal("");
  const [displayName, setDisplayName] = createSignal("");
  const [consolidate, setConsolidate] = createSignal(true);
  const [importReason, setImportReason] = createSignal("");
  const [importing, setImporting] = createSignal(false);
  const [imported, setImported] = createSignal<Record<string, unknown> | null>(null);
  const [importFailure, setImportFailure] = createSignal<string | null>(null);

  // -- the repo picker (#180, design #178 decision 5) -----------------------
  // Enumerates what the acting credential (a linked PAT, #179, else the file
  // token) can see, so a person can pick which to import. This lists; it never
  // crawls — the token is the scope. Select-all imports each as clone + full
  // sync, skipping what is already registered.
  const [pickerRepos, setPickerRepos] = createSignal<ForgeRepoView[]>([]);
  const [pickerHost, setPickerHost] = createSignal("github.com");
  const [pickerLogin, setPickerLogin] = createSignal<string | null>(null);
  const [pickerDetail, setPickerDetail] = createSignal<string | null>(null);
  const [pickerLoading, setPickerLoading] = createSignal(false);
  const [pickerError, setPickerError] = createSignal<string | null>(null);
  const [pickerOpen, setPickerOpen] = createSignal(false);
  const [selectedRepos, setSelectedRepos] = createSignal<Set<string>>(new Set());
  const [batchImporting, setBatchImporting] = createSignal(false);
  const [batchDone, setBatchDone] = createSignal<string | null>(null);
  const [batchError, setBatchError] = createSignal<string | null>(null);

  const importableRepos = createMemo(() =>
    pickerRepos().filter((entry) => !entry.already_registered),
  );
  const allSelected = createMemo(() => {
    const importable = importableRepos();
    return importable.length > 0 && importable.every((e) => selectedRepos().has(e.url));
  });

  const loadRepos = async () => {
    if (pickerLoading()) return;
    setPickerOpen(true);
    setPickerLoading(true);
    setPickerError(null);
    setBatchDone(null);
    try {
      const result = await listForgeRepos(pickerHost().trim() || "github.com");
      setPickerRepos(result.repos);
      setPickerLogin(result.login);
      setPickerDetail(result.detail);
      setSelectedRepos(new Set<string>());
    } catch (error) {
      setPickerError(errorMessage(error));
    } finally {
      setPickerLoading(false);
    }
  };

  const toggleRepo = (url: string) => {
    setSelectedRepos((current) => {
      const next = new Set<string>(current);
      if (next.has(url)) next.delete(url);
      else next.add(url);
      return next;
    });
  };

  const toggleSelectAll = () => {
    setSelectedRepos(
      allSelected()
        ? new Set<string>()
        : new Set<string>(importableRepos().map((e) => e.url)),
    );
  };

  const batchReady = createMemo(
    () => selectedRepos().size > 0 && importReason().trim().length > 0,
  );

  const importSelected = async () => {
    if (!batchReady() || batchImporting()) return;
    setBatchImporting(true);
    setBatchError(null);
    setBatchDone(null);
    const reason = importReason().trim();
    const chosen = importableRepos().filter((e) => selectedRepos().has(e.url));
    let done = 0;
    try {
      for (const entry of chosen) {
        // Each import is clone + full sync (the consolidate default), the same
        // path a single named import walks — the picker only chooses the names.
        await importProject({ repo: entry.url, consolidate: true, reason });
        done += 1;
      }
      setBatchDone(`Imported ${done} of ${chosen.length} selected repositories.`);
      setSelectedRepos(new Set<string>());
      void refetchProjects();
      await loadRepos();
    } catch (error) {
      const message = errorMessage(error);
      setBatchError(
        `Imported ${done} of ${chosen.length}; the next was refused: ${message}`,
      );
      props.onError?.(`An import was refused: ${message}`);
      void refetchProjects();
    } finally {
      setBatchImporting(false);
    }
  };

  const importReady = createMemo(
    () => repo().trim().length > 0 && importReason().trim().length > 0,
  );

  const submitImport = async (event: Event) => {
    event.preventDefault();
    if (!importReady() || importing()) return;
    setImporting(true);
    setImportFailure(null);
    setImported(null);
    try {
      const result = await importProject({
        repo: repo().trim(),
        name: displayName().trim() || undefined,
        consolidate: consolidate(),
        reason: importReason().trim(),
      });
      setImported(result);
      setRepo("");
      setDisplayName("");
      setImportReason("");
      void refetchProjects();
    } catch (error) {
      const message = errorMessage(error);
      setImportFailure(message);
      props.onError?.(`The import was refused: ${message}`);
    } finally {
      setImporting(false);
    }
  };

  // -- rendering helpers ----------------------------------------------------

  const freshnessLine = (described: () => Described, what: string) => (
    <p class={`vogt-projects-freshness ${described().status}`}>
      <span class="vogt-projects-freshness-what">{what}</span>
      {described().text}
      <Show when={described().collectors.length}>
        <span class="vogt-projects-collectors">
          <For each={described().collectors}>
            {([name, age]) => (
              <span class="vogt-projects-collector">
                {name}: {age}
              </span>
            )}
          </For>
        </span>
      </Show>
    </p>
  );

  const outagePanel = (
    failure: () => { unavailable: boolean; message: string },
    what: string,
  ) => (
    <div class="vogt-projects-outage" role="alert">
      <h3>{failure().unavailable ? "Vogt is not answering" : `${what} failed to load`}</h3>
      <p>{failure().message}</p>
      <p class="vogt-projects-muted">
        Nothing is shown because nothing was read. An estate with no {what.toLowerCase()} and
        an unreachable one are not the same answer.
      </p>
      <button type="button" onClick={refresh}>
        Try again
      </button>
    </div>
  );

  const edgeRow = (ref: DepRef, direction: "out" | "in") => {
    const other =
      direction === "out"
        ? (ref.to_project_slug ?? null)
        : (ref.from_project_slug ?? null);
    const resolved = direction === "out" ? Boolean(ref.to_project_id) : true;
    return (
      <li class={`vogt-projects-edge${resolved ? "" : " vogt-projects-edge--unresolved"}`}>
        <span class="vogt-projects-edge-node">
          <Show
            when={other}
            fallback={<span class="vogt-projects-mono">{ref.raw_target}</span>}
          >
            {(slug) => (
              <button
                type="button"
                class="vogt-projects-link"
                onClick={() => openProject(slug())}
              >
                {slug()}
              </button>
            )}
          </Show>
        </span>
        <span class="vogt-projects-edge-meta">
          <span class="vogt-projects-tag">{ref.ref_kind}</span>
          <Show when={ref.manifest}>
            {(manifest) => <span class="vogt-projects-mono">{manifest()}</span>}
          </Show>
          <span class="vogt-projects-muted">
            {resolved ? "in the estate" : "outside the estate — no registered project matches"}
          </span>
          <span class="vogt-projects-muted">seen {ageOf(ref.observed_at)}</span>
        </span>
      </li>
    );
  };

  return (
    <div class="vogt-surface vogt-projects">
      <SurfaceHeader
        class="vogt-projects-header"
        label="Projects header"
        title={(
          <>
            {/* The route says where it is as a heading and not only as a
                crumb: a crumb is a control, and a phone arriving on a
                secondary route needs the title (FR-U23, Stage 3). */}
            <h1 class="vogt-projects-title">{place().project || "Projects"}</h1>
            <nav class="vogt-projects-crumbs" aria-label="Where you are">
              <button
                type="button"
                class={`vogt-projects-crumb${place().project ? "" : " active"}`}
                onClick={() => setPlace({ ...place(), project: "", view: "overview" })}
              >
                Projects
              </button>
              <Show when={place().project}>
                {(slug) => (
                  <>
                    <span class="vogt-projects-muted">/</span>
                    <span class="vogt-projects-crumb active">{slug()}</span>
                  </>
                )}
              </Show>
            </nav>
          </>
        )}
        honestyClass={honestyToneClass(viewAge().tone)}
        honesty={(
          <div class="vogt-projects-honesty" aria-live="polite">
            <strong>
              <ViewAgeBadge
                age={viewAge()}
                class="vogt-projects-age"
                title="How long ago this view last got an answer from Vogt — not how old the evidence behind it is, which each panel says for itself"
              />
            </strong>
          </div>
        )}
        controls={(
          /* The views are the surface's own navigation, not chrome, so they
             are never folded on a phone: they are a scrollable segmented
             control instead, which keeps Import reachable at any width
             (#228). */
          <div
            class="vogt-projects-views surface-header-tabs"
            role="group"
            aria-label="Views"
          >
            <For each={VIEWS}>
              {(name) => {
                const perProject = name === "overview" || name === "deps";
                const disabled = () => !place().project && perProject;
                return (
                  <button
                    type="button"
                    aria-pressed={place().view === name}
                    class={`vogt-projects-viewtab${place().view === name ? " active" : ""}`}
                    disabled={disabled()}
                    title={
                      disabled()
                        ? "Pick a project first — this view is about one project"
                        : undefined
                    }
                    onClick={() => move("view", name)}
                  >
                    {name === "overview"
                      ? "Project"
                      : name === "deps"
                        ? "Dependencies"
                        : name === "drift"
                          ? "Drift inbox"
                          : "Import"}
                  </button>
                );
              }}
            </For>
          </div>
        )}
        action={(
          <button type="button" onClick={refresh}>
            Refresh
          </button>
        )}
      />

      <Show when={note()}>
        {(message) => (
          <p class="vogt-projects-note" role="status">
            {message()}
            <button type="button" onClick={() => setNote(null)}>
              Dismiss
            </button>
          </p>
        )}
      </Show>

      {/* -- the estate list ------------------------------------------------ */}
      <Show when={!place().project && place().view !== "drift" && place().view !== "import"}>
        <section class="vogt-projects-list" aria-label="Registered projects">
          <Show when={projectOutage()}>
            {(failure) => outagePanel(failure, "Projects")}
          </Show>
          <Show when={!projectOutage()}>
            <p class="vogt-projects-muted">
              <Show
                when={projectSearch().trim()}
                fallback={
                  <>
                    {projectRows().length} registered project(s). Every one was
                    registered explicitly — Vogt discovers nothing.
                  </>
                }
              >
                {shownProjectRows().length} of {projectRows().length} registered
                project(s) match “{projectSearch().trim()}”.
              </Show>
            </p>
            <Show when={projectRows().length > 0}>
              <div class="vogt-projects-listtools">
                <input
                  type="search"
                  class="vogt-projects-search"
                  placeholder="Filter by name or slug"
                  aria-label="Filter projects"
                  value={projectSearch()}
                  onInput={(event) => setProjectSearch(event.currentTarget.value)}
                />
                <label class="vogt-projects-sort">
                  <span>Sort</span>
                  <select
                    aria-label="Sort projects"
                    value={projectSort()}
                    onInput={(event) =>
                      setProjectSort(event.currentTarget.value as ProjectSort)
                    }
                  >
                    <option value="name">Name</option>
                    <option value="lifecycle">Lifecycle</option>
                    <option value="trust">Trust</option>
                  </select>
                </label>
              </div>
            </Show>
            <div class="vogt-projects-grid">
              <For
                each={shownProjectRows()}
                fallback={
                  <p class="vogt-projects-empty">
                    {projects.loading
                      ? "Asking Vogt for the estate…"
                      : projectSearch().trim()
                        ? "No registered project matches that filter."
                        : "No projects are registered. The import form registers one."}
                  </p>
                }
              >
                {(row) => (
                  <button
                    type="button"
                    class="vogt-projects-card"
                    onClick={() => openProject(readString(row, "slug") ?? "")}
                  >
                    <span class="vogt-projects-card-name">
                      {readString(row, "name") ?? readString(row, "slug") ?? "unnamed"}
                    </span>
                    <span class="vogt-projects-mono vogt-projects-muted">
                      {readString(row, "slug") ?? "—"}
                    </span>
                    <span class="vogt-projects-card-tags">
                      <span class="vogt-projects-tag">
                        {readString(row, "lifecycle_state") ?? "lifecycle not reported"}
                      </span>
                      <span
                        class={`vogt-projects-trust trust-${trustOf(row["trust_state"])}`}
                        title={`trust: ${trustOf(row["trust_state"])}`}
                      >
                        {trustOf(row["trust_state"])}
                      </span>
                      <span class="vogt-projects-tag">
                        write-back: {readString(row, "write_back") ?? "none"}
                      </span>
                    </span>
                    <span class="vogt-projects-muted">
                      {readString(row, "repo_url") ?? "no remote recorded"}
                    </span>
                  </button>
                )}
              </For>
            </div>
          </Show>
        </section>
      </Show>

      {/* -- one project (FR-U7) -------------------------------------------- */}
      <Show when={place().project && place().view === "overview"}>
        <Show when={briefOutage()}>{(failure) => outagePanel(failure, "This project")}</Show>
        <Show when={!briefOutage()}>
          <Show
            when={briefValue()}
            fallback={<p class="vogt-projects-empty">Asking Vogt for the brief…</p>}
          >
            {(data) => {
              const project = () => record(data()["project"]);
              const ci = () => record(data()["ci_status"]);
              const dependencies = () => record(data()["dependencies"]);
              const backlogRows = () => {
                const rows = data()["top_backlog"];
                return Array.isArray(rows) ? (rows as Record<string, unknown>[]) : [];
              };
              return (
                <section class="vogt-projects-project">
                  <header class="vogt-projects-project-head">
                    <h2>{readString(project(), "name") ?? place().project}</h2>
                    <span class="vogt-projects-tag">
                      {readString(project(), "lifecycle_state") ?? "lifecycle not reported"}
                    </span>
                    <span
                      class={`vogt-projects-trust trust-${trustOf(project()["trust_state"])}`}
                      title={`trust: ${trustOf(project()["trust_state"])}`}
                    >
                      {trustOf(project()["trust_state"])}
                    </span>
                    <span class="vogt-projects-mono vogt-projects-muted">
                      {readString(project(), "root_path") ?? "no path recorded"}
                    </span>
                    <Show when={safeHref(readString(project(), "repo_url"))}>
                      {(url) => (
                        <a href={url()} target="_blank" rel="noreferrer">
                          {url()}
                        </a>
                      )}
                    </Show>
                  </header>

                  {freshnessLine(
                    () => describeFreshness(data()["freshness"] as FreshnessSummary),
                    "brief:",
                  )}

                  <div class="vogt-projects-panels">
                    <section class="vogt-projects-panel">
                      <h3>Work</h3>
                      <dl class="vogt-projects-kv">
                        <div>
                          <dt>Open work</dt>
                          <dd>
                            <Show
                              when={readNumber(data(), "open_work") !== null}
                              fallback={<>not reported</>}
                            >
                              <a href={`#/board?project=${projectQuery()}`}>
                                {readNumber(data(), "open_work")}
                              </a>
                            </Show>
                          </dd>
                        </div>
                        <div>
                          <dt>Open bugs</dt>
                          <dd>
                            <Show
                              when={readNumber(data(), "open_bugs") !== null}
                              fallback={<>not reported</>}
                            >
                              <a href={`#/backlog?view=bugs&project=${projectQuery()}`}>
                                {readNumber(data(), "open_bugs")}
                              </a>
                            </Show>
                          </dd>
                        </div>
                        <div>
                          <dt>By state</dt>
                          <dd>
                            <Show
                              when={readCounts(data(), "by_state").length}
                              fallback={<span class="vogt-projects-muted">none</span>}
                            >
                              <For each={readCounts(data(), "by_state")}>
                                {([name, count]) => (
                                  <a
                                    class="vogt-projects-tag"
                                    href={`#/board?project=${projectQuery()}&state=${encodeURIComponent(name)}`}
                                  >
                                    {name}: {count}
                                  </a>
                                )}
                              </For>
                            </Show>
                          </dd>
                        </div>
                        <div>
                          <dt>By kind</dt>
                          <dd>
                            <Show
                              when={readCounts(data(), "by_kind").length}
                              fallback={<span class="vogt-projects-muted">none</span>}
                            >
                              <For each={readCounts(data(), "by_kind")}>
                                {([name, count]) => (
                                  <span class="vogt-projects-tag">
                                    {name}: {count}
                                  </span>
                                )}
                              </For>
                            </Show>
                          </dd>
                        </div>
                        <div>
                          <dt>Audit</dt>
                          <dd>
                            <a href={`#/audit?project=${projectQuery()}`}>
                              Every write to this project
                            </a>
                          </dd>
                        </div>
                      </dl>
                    </section>

                    {/* The version disagreement, shown as a disagreement — it
                        is the same shape as a drift proposal, and it is the
                        one `version_mismatch` is raised from. */}
                    <section class="vogt-projects-panel">
                      <h3>Version</h3>
                      <div class="vogt-projects-sides vogt-projects-sides--tight">
                        <div class="vogt-projects-side">
                          <h5>Declared here</h5>
                          <p class="vogt-projects-side-value">
                            {readString(data(), "declared_version") ?? "nothing declared"}
                          </p>
                        </div>
                        <div class="vogt-projects-side">
                          <h5>Observed by a collector</h5>
                          <p class="vogt-projects-side-value">
                            {readString(data(), "observed_version") ?? "not collected"}
                          </p>
                        </div>
                      </div>
                      <p class="vogt-projects-muted">
                        {readBoolean(data(), "version_matches") === null
                          ? "Whether they agree is not collected — one side is unknown, which is not the same as a mismatch."
                          : readBoolean(data(), "version_matches")
                            ? "They agree."
                            : "They disagree. The drift inbox carries the proposal, with the evidence."}
                      </p>
                    </section>

                    <section class="vogt-projects-panel">
                      <h3>CI</h3>
                      <p class={`vogt-projects-status vogt-projects-status--${readString(ci(), "status") ?? "not_collected"}`}>
                        {readString(ci(), "status") ?? "not_collected"}
                      </p>
                      <dl class="vogt-projects-kv">
                        <div>
                          <dt>Checks seen</dt>
                          <dd>{readNumber(ci(), "checks") ?? 0}</dd>
                        </div>
                        <div>
                          <dt>Failing</dt>
                          <dd>
                            <Show
                              when={readStringList(ci(), "failing").length}
                              fallback={<span class="vogt-projects-muted">none named</span>}
                            >
                              <For each={readStringList(ci(), "failing")}>
                                {(name) => <span class="vogt-projects-tag">{name}</span>}
                              </For>
                            </Show>
                          </dd>
                        </div>
                        <div>
                          <dt>Revision</dt>
                          <dd class="vogt-projects-mono">
                            {readString(ci(), "revision")?.slice(0, 12) ?? "—"}
                          </dd>
                        </div>
                      </dl>
                      <Show when={readString(ci(), "detail")}>
                        {(detail) => <p class="vogt-projects-muted">{detail()}</p>}
                      </Show>
                    </section>

                    <section class="vogt-projects-panel">
                      <h3>Contract and compliance</h3>
                      <p class={`vogt-projects-status vogt-projects-status--${readString(data(), "compliance_status") ?? "not_checked"}`}>
                        {readString(data(), "compliance_status") ?? "not_checked"}
                      </p>
                      {/* FR-G16: the contract is opt-in, so this status is
                          not a criticism — a project that adopted nothing is
                          not being measured, and the surface says which. */}
                      <Show when={readString(data(), "compliance_status") === "not_applicable"}>
                        <p class="vogt-projects-muted">
                          This project has not adopted the contract, so there is
                          nothing for it to comply with. `contract adopt` opts in.
                        </p>
                      </Show>
                      <Show when={contractValue()}>
                        {(result) => (
                          <>
                            <p class="vogt-projects-muted">
                              contract {result().contract_version || "version not recorded"} ·
                              checked {formatWhen(result().checked_at)} ·{" "}
                              {result().age_seconds === null || result().age_seconds === undefined
                                ? "age not reported"
                                : `${describeAge(result().age_seconds ?? null)} old`}{" "}
                              · never refreshed implicitly, so this is the last recorded answer
                            </p>
                            <Show
                              when={result().failing.length}
                              fallback={
                                <p class="vogt-projects-muted">
                                  No failing criteria are recorded in that answer.
                                </p>
                              }
                            >
                              <ul class="vogt-projects-criteria">
                                <For each={result().failing}>
                                  {(criterion) => (
                                    <li>
                                      <span class="vogt-projects-mono">{criterion.rule}</span>
                                      <span class="vogt-projects-muted">{criterion.target}</span>
                                      <span>{criterion.detail}</span>
                                    </li>
                                  )}
                                </For>
                              </ul>
                            </Show>
                            <Show when={result().detail}>
                              {(detail) => <p class="vogt-projects-muted">{detail()}</p>}
                            </Show>
                          </>
                        )}
                      </Show>
                      <Show when={contractFailure()}>
                        {(message) => (
                          <p class="vogt-projects-note">
                            The failing criteria could not be read: {message()}. The status above
                            is the brief's, and is not a claim that nothing is failing.
                          </p>
                        )}
                      </Show>
                      <p class="vogt-projects-muted">
                        Re-running the contract check is a write and is not offered here; it is
                        <span class="vogt-projects-mono"> vogt contract check</span>.
                      </p>
                    </section>

                    <section class="vogt-projects-panel">
                      <h3>Dependencies</h3>
                      <p class={`vogt-projects-status vogt-projects-status--${readString(dependencies(), "status") ?? "not_collected"}`}>
                        {readString(dependencies(), "status") ?? "not_collected"}
                      </p>
                      <dl class="vogt-projects-kv">
                        <div>
                          <dt>References out</dt>
                          <dd>{readNumber(dependencies(), "references_out") ?? 0}</dd>
                        </div>
                        <div>
                          <dt>Referenced by</dt>
                          <dd>{readNumber(dependencies(), "referenced_by") ?? 0}</dd>
                        </div>
                        <div>
                          <dt>Unresolved</dt>
                          <dd>{readNumber(dependencies(), "unresolved") ?? 0}</dd>
                        </div>
                      </dl>
                      <Show when={readString(dependencies(), "detail")}>
                        {(detail) => <p class="vogt-projects-muted">{detail()}</p>}
                      </Show>
                      <button type="button" onClick={() => move("view", "deps")}>
                        Open the dependency graph
                      </button>
                    </section>
                  </div>

                  <section class="vogt-projects-panel">
                    <h3>Top backlog</h3>
                    <Show
                      when={backlogRows().length}
                      fallback={
                        <p class="vogt-projects-muted">
                          Nothing is ranked for this project. The freshness line above says
                          whether anything has looked.
                        </p>
                      }
                    >
                      <ul class="vogt-projects-backlog">
                        <For each={backlogRows()}>
                          {(row) => {
                            const ref = readString(row, "ref") ?? "";
                            const declared = readString(row, "origin") === "declared";
                            return (
                              <li>
                                <Show
                                  when={declared && ref}
                                  fallback={<span class="vogt-projects-mono">{ref || "—"}</span>}
                                >
                                  <button
                                    type="button"
                                    class="vogt-projects-link vogt-projects-mono"
                                    onClick={() => {
                                      openWorkItemTab(ref);
                                      navigate(`/w/${encodeURIComponent(ref)}`);
                                    }}
                                  >
                                    {ref}
                                  </button>
                                </Show>
                                <span class="vogt-projects-backlog-title">
                                  {readString(row, "title") ?? "untitled"}
                                </span>
                                <span class="vogt-projects-tag">
                                  {readString(row, "kind") ?? "?"}
                                </span>
                                <span class="vogt-projects-tag">
                                  {readString(row, "state") ?? "?"}
                                </span>
                                <span
                                  class={`vogt-projects-trust trust-${trustOf(row["trust_state"])}`}
                                  title={`trust: ${trustOf(row["trust_state"])}`}
                                >
                                  {trustOf(row["trust_state"])}
                                </span>
                                <Show when={!declared}>
                                  <span class="vogt-projects-muted">observed, not adopted</span>
                                </Show>
                              </li>
                            );
                          }}
                        </For>
                      </ul>
                    </Show>
                  </section>

                  <p class="vogt-projects-muted">
                    <button
                      type="button"
                      class="vogt-projects-link"
                      onClick={() => move("view", "drift")}
                    >
                      Drift proposals for this project →
                    </button>
                  </p>
                </section>
              );
            }}
          </Show>
        </Show>
      </Show>

      {/* -- the dependency graph (FR-U7) ----------------------------------- */}
      <Show when={place().project && place().view === "deps"}>
        <section class="vogt-projects-deps" aria-label="Dependency graph">
          <Show when={graphOutage()}>{(failure) => outagePanel(failure, "The graph")}</Show>
          <Show when={!graphOutage()}>
            <Show
              when={graphValue()}
              fallback={<p class="vogt-projects-empty">Asking Vogt for the references…</p>}
            >
              {(data) => (
                <>
                  {freshnessLine(() => describeFreshness(data().freshness), "graph:")}
                  <p class="vogt-projects-muted">
                    {data().references_out.length} reference(s) out ·{" "}
                    {data().referenced_by.length} in · {data().unresolved} pointing outside the
                    estate
                  </p>
                  {/* One hop, drawn as a hub. Walking it is what makes it a
                      graph: a neighbour opens as the new centre. */}
                  <div class="vogt-projects-hub">
                    <div class="vogt-projects-hub-col">
                      <h4>Referenced by</h4>
                      <Show
                        when={data().referenced_by.length}
                        fallback={
                          <p class="vogt-projects-muted">
                            No registered project references this one.
                          </p>
                        }
                      >
                        <ul class="vogt-projects-edges">
                          <For each={data().referenced_by}>{(ref) => edgeRow(ref, "in")}</For>
                        </ul>
                      </Show>
                    </div>
                    <div class="vogt-projects-hub-centre">
                      <span class="vogt-projects-hub-node">{data().project}</span>
                    </div>
                    <div class="vogt-projects-hub-col">
                      <h4>References out</h4>
                      <Show
                        when={data().references_out.length}
                        fallback={
                          <p class="vogt-projects-muted">
                            No references were recorded from this project.
                          </p>
                        }
                      >
                        <ul class="vogt-projects-edges">
                          <For each={data().references_out}>{(ref) => edgeRow(ref, "out")}</For>
                        </ul>
                      </Show>
                    </div>
                  </div>
                  {/* What this view cannot show, said rather than left to be
                      inferred from a sparse picture. */}
                  <p class="vogt-projects-note">
                    This is the one-hop neighbourhood, not a transitive graph: Vogt answers
                    about one project at a time, so reaching further means opening a neighbour.
                    It records <em>which</em> projects reference which — no lockfile is parsed
                    and no package version is resolved, so there is no ecosystem, constraint or
                    resolved version to show. An empty graph over a{" "}
                    <span class="vogt-projects-mono">never_swept</span> freshness means "not
                    collected", not "depends on nothing".
                  </p>
                </>
              )}
            </Show>
          </Show>
        </section>
      </Show>

      {/* -- the drift inbox (FR-U18) --------------------------------------- */}
      <Show when={place().view === "drift"}>
        <section class="vogt-projects-inbox" aria-label="Drift inbox">
          <div class="vogt-projects-inbox-filters">
            <label class="vogt-projects-field">
              <span>Project</span>
              <select
                value={
                  driftProjectOptions().find((slug) => slug === place().project) ??
                  place().project
                }
                onInput={(event) => move("project", event.currentTarget.value)}
              >
                <option value="">Every project</option>
                <For each={driftProjectOptions()}>
                  {(slug) => <option value={slug}>{slug}</option>}
                </For>
              </select>
            </label>
            <label class="vogt-projects-field">
              <span>Status</span>
              <select
                value={place().status}
                onInput={(event) => move("status", event.currentTarget.value)}
              >
                <For each={DRIFT_STATUSES}>
                  {(status) => <option value={status}>{status}</option>}
                </For>
              </select>
            </label>
            <label class="vogt-projects-field">
              <span>Kind</span>
              <select
                value={place().kind}
                onInput={(event) => move("kind", event.currentTarget.value)}
              >
                <option value="">Every kind</option>
                <For each={driftKinds()}>{(kind) => <option value={kind}>{kind}</option>}</For>
              </select>
            </label>
          </div>

          <Show when={driftOutage()}>{(failure) => outagePanel(failure, "The drift inbox")}</Show>
          <Show when={!driftOutage()}>
            {freshnessLine(() => describeFreshness(driftValue()?.freshness), "inbox:")}
            <p class="vogt-projects-muted">
              {proposals().length} proposal(s). Each one is resolved on its own, with its own
              reason — there is no bulk accept, and there will not be one.
            </p>
            <Show
              when={proposals().length}
              fallback={
                <p class="vogt-projects-empty">
                  {drift.loading
                    ? "Asking Vogt for the proposals…"
                    : "No proposals match. Whether that is reassuring depends on the freshness line above."}
                </p>
              }
            >
              <For each={proposals()}>
                {(proposal) => (
                  <DriftCard
                    proposal={proposal}
                    gate={gates()[proposal.kind] ?? null}
                    onResolved={(message) => {
                      setNote(message);
                      void refetchDrift();
                    }}
                    onFailure={(message) => props.onError?.(message)}
                    onOpenProject={openProject}
                    onDrafting={noteDrafting}
                  />
                )}
              </For>
            </Show>
          </Show>
        </section>
      </Show>

      {/* -- the import form (FR-U3, FR-P6) --------------------------------- */}
      <Show when={place().view === "import"}>
        <section class="vogt-projects-import" aria-label="Import a repository">
          <h2>Import a repository</h2>
          {/* Name the repository, or pick from the ones your linked credential
              can see (#180, design #178 decision 5). The picker is an
              *enumeration* of what your token reaches, not the candidate crawl
              r3 removed (FR-G15): the token is the scope, and you still confirm
              what is imported. */}
          <p class="vogt-projects-note">
            Name the repository, or browse the ones your linked forge account can see. Vogt
            clones each into the import root, registers it, and reads what is already on the
            forge — it changes nothing upstream (FR-B3).
          </p>

          {/* -- the repo picker ------------------------------------------- */}
          <div class="vogt-projects-picker" aria-label="Repository picker">
            <label>
              <span>Forge host</span>
              <input
                type="text"
                value={pickerHost()}
                placeholder="github.com or repo.example.com"
                onInput={(event) => setPickerHost(event.currentTarget.value)}
              />
            </label>
            <div class="vogt-projects-resolve-actions">
              <button type="button" onClick={() => void loadRepos()} disabled={pickerLoading()}>
                {pickerLoading()
                  ? "Listing…"
                  : pickerOpen()
                    ? "Refresh my repositories"
                    : "Browse my repositories"}
              </button>
              <Show when={pickerLogin()}>
                {(login) => (
                  <span class="vogt-projects-hint">Listing as {login()}.</span>
                )}
              </Show>
            </div>
            <Show when={pickerError()}>
              {(message) => <p class="vogt-projects-failure">{message()}</p>}
            </Show>
            <Show when={pickerOpen() && !pickerLoading() && !pickerError()}>
              <Show
                when={pickerRepos().length > 0}
                fallback={
                  <p class="vogt-projects-note">
                    {pickerDetail() ??
                      "No repositories were listed — link a forge account first."}
                  </p>
                }
              >
                <label class="vogt-projects-check">
                  <input
                    type="checkbox"
                    checked={allSelected()}
                    disabled={importableRepos().length === 0}
                    onChange={() => toggleSelectAll()}
                  />
                  <span>Select all importable ({importableRepos().length})</span>
                </label>
                <ul class="vogt-projects-picker-list">
                  <For each={pickerRepos()}>
                    {(entry) => (
                      <li>
                        <label class="vogt-projects-check">
                          <input
                            type="checkbox"
                            checked={selectedRepos().has(entry.url)}
                            disabled={entry.already_registered}
                            onChange={() => toggleRepo(entry.url)}
                          />
                          <span>
                            {entry.owner}/{entry.name}{" "}
                            <span class="vogt-projects-hint">
                              {entry.visibility}
                              {entry.already_registered ? " · already imported" : ""}
                            </span>
                          </span>
                        </label>
                      </li>
                    )}
                  </For>
                </ul>
                <div class="vogt-projects-resolve-actions">
                  <button
                    type="button"
                    onClick={() => void importSelected()}
                    disabled={!batchReady() || batchImporting()}
                  >
                    {batchImporting()
                      ? "Importing…"
                      : `Import selected (${selectedRepos().size})`}
                  </button>
                  <span class="vogt-projects-hint">
                    Each is cloned and fully synced. A reason (below) is required — the audit
                    row records that a person imported these, and why.
                  </span>
                </div>
                <Show when={batchError()}>
                  {(message) => <p class="vogt-projects-failure">{message()}</p>}
                </Show>
                <Show when={batchDone()}>
                  {(message) => <p class="vogt-projects-imported">{message()}</p>}
                </Show>
              </Show>
            </Show>
          </div>
          <form class="vogt-projects-form" onSubmit={(event) => void submitImport(event)}>
            <label class="vogt-projects-field">
              <span>Repository</span>
              <input
                type="text"
                value={repo()}
                placeholder="owner/name, or any GitHub URL"
                onInput={(event) => setRepo(event.currentTarget.value)}
              />
            </label>
            <label class="vogt-projects-field">
              <span>Display name (optional)</span>
              <input
                type="text"
                value={displayName()}
                placeholder="Defaults to the repository's own name"
                onInput={(event) => setDisplayName(event.currentTarget.value)}
              />
            </label>
            <label class="vogt-projects-check">
              <input
                type="checkbox"
                checked={consolidate()}
                onChange={(event) => setConsolidate(event.currentTarget.checked)}
              />
              <span>
                Read the existing issues, PRs, labels and releases. Read-only, and on by
                default because a project that arrives empty looks like a project with no work.
              </span>
            </label>
            <label class="vogt-projects-field">
              <span>Reason (audited)</span>
              <input
                type="text"
                value={importReason()}
                placeholder="Why are you importing this?"
                onInput={(event) => setImportReason(event.currentTarget.value)}
              />
            </label>
            <div class="vogt-projects-resolve-actions">
              <button type="submit" disabled={!importReady() || importing()}>
                {importing() ? "Cloning…" : "Import"}
              </button>
              <span class="vogt-projects-hint">
                The audit row records that a person imported this repository, and why — so the
                reason has to be one you typed.
              </span>
            </div>
          </form>

          <Show when={importFailure()}>
            {(message) => <p class="vogt-projects-failure">{message()}</p>}
          </Show>

          <Show when={imported()}>
            {(result) => {
              const project = () => record(result()["project"]);
              const consolidated = () => record(result()["consolidated"]);
              return (
                <div class="vogt-projects-imported">
                  <p>
                    <strong>{readString(project(), "name") ?? "The project"}</strong> is
                    registered at{" "}
                    <span class="vogt-projects-mono">
                      {readString(result(), "root_path") ?? "an unreported path"}
                    </span>
                    .
                  </p>
                  <dl class="vogt-projects-kv">
                    <div>
                      <dt>Remote</dt>
                      <dd class="vogt-projects-mono">
                        {readString(result(), "remote") ?? "not reported"}
                      </dd>
                    </div>
                    <div>
                      <dt>Revision at import</dt>
                      <dd class="vogt-projects-mono">
                        {readString(result(), "revision")?.slice(0, 12) ?? "not reported"}
                      </dd>
                    </div>
                    <div>
                      <dt>Cloned</dt>
                      <dd>
                        {readBoolean(result(), "cloned") === false
                          ? "no — the destination already held a clone of the same remote, and was registered as it stood"
                          : "yes"}
                      </dd>
                    </div>
                    <Show when={Object.keys(consolidated()).length}>
                      <div>
                        <dt>Read from the forge</dt>
                        <dd>
                          {readNumber(consolidated(), "issues") ?? 0} issue(s),{" "}
                          {readNumber(consolidated(), "pull_requests") ?? 0} PR(s),{" "}
                          {readNumber(consolidated(), "labels") ?? 0} label(s),{" "}
                          {readNumber(consolidated(), "releases") ?? 0} release(s) ·{" "}
                          {readNumber(consolidated(), "mutations") ?? 0} upstream mutation(s)
                        </dd>
                      </div>
                    </Show>
                  </dl>
                  <Show when={readString(result(), "detail")}>
                    {(detail) => <p class="vogt-projects-muted">{detail()}</p>}
                  </Show>
                  <Show when={readString(project(), "slug")}>
                    {(slug) => (
                      <button type="button" onClick={() => openProject(slug())}>
                        Open {slug()}
                      </button>
                    )}
                  </Show>
                </div>
              );
            }}
          </Show>
        </section>
      </Show>
    </div>
  );
};

export default Projects;
