// The oversight board (WI-915): every live and hibernated session in one
// table, most urgent first, for whoever is driving several at once. Backed by
// vogt-core's `session.sweep`, so the GUI and an agent driving over MCP read
// the same table in the same order with the same reasons.
//
// Refreshed on a short timer while visible, on any session event, and on
// demand. Absent core or engine is said, never rendered as "nothing running".

import { Component, For, Show, createEffect, createSignal, onCleanup, onMount, on } from "solid-js";
import { sessionsStore } from "./store";
import { SafeSnippet } from "./SafeSnippet";
import { VogtUnavailable, sweepSessions, type SessionSweepResult, type SessionSweepRow } from "./vogtApi";

interface Props {
  onError?: (message: string) => void;
  onOpenSession?: (sessionId: string, label: string) => void;
}

const REFRESH_MS = 10_000;

const ATTENTION_LABEL: Record<SessionSweepRow["attention"], string> = {
  approval: "Approval",
  blocked: "Blocked",
  waiting: "Waiting",
  stalled: "Stalled",
  running: "Running",
  idle: "Idle",
  hibernated: "Hibernated",
  exited: "Exited",
  unknown: "Unknown",
};

/** "3 min ago", "just now", or null when the time is absent or unreadable. */
export function ago(iso: string | null | undefined, now: number): string | null {
  if (!iso) return null;
  const at = Date.parse(iso);
  if (Number.isNaN(at)) return null;
  const minutes = Math.floor((now - at) / 60_000);
  if (minutes < 1) return "just now";
  if (minutes < 60) return `${minutes} min ago`;
  const hours = Math.floor(minutes / 60);
  return hours < 48 ? `${hours} h ago` : `${Math.floor(hours / 24)} d ago`;
}

const Oversight: Component<Props> = (props) => {
  const [result, setResult] = createSignal<SessionSweepResult | null>(null);
  const [problem, setProblem] = createSignal<string | null>(null);
  const [loading, setLoading] = createSignal(false);
  const [now, setNow] = createSignal(Date.now());
  let inFlight: AbortController | null = null;

  const refresh = async () => {
    inFlight?.abort();
    const controller = new AbortController();
    inFlight = controller;
    setLoading(true);
    try {
      const next = await sweepSessions({ screen_lines: 6 }, controller.signal);
      if (controller.signal.aborted) return;
      setResult(next);
      setProblem(next.engine ? `Sessions unavailable — ${next.engine}` : null);
      setNow(Date.now());
    } catch (error) {
      if (controller.signal.aborted) return;
      setProblem(
        error instanceof VogtUnavailable
          ? "Oversight needs vogt-core, which is not answering behind this front door."
          : `Sweep failed — ${(error as Error).message}`,
      );
    } finally {
      if (inFlight === controller) {
        inFlight = null;
        setLoading(false);
      }
    }
  };

  onMount(() => {
    void refresh();
    const timer = setInterval(() => {
      if (document.visibilityState === "visible") void refresh();
    }, REFRESH_MS);
    onCleanup(() => {
      clearInterval(timer);
      inFlight?.abort();
    });
  });

  // Any change the event stream reports — an activity flip, a new or
  // hibernated session — is worth a fresh sweep; coalesced to one per second.
  let pending: ReturnType<typeof setTimeout> | null = null;
  createEffect(
    on(
      () => sessionsStore.order.map((id) => sessionsStore.sessions[id]?.activity).join(","),
      () => {
        if (pending !== null) return;
        pending = setTimeout(() => {
          pending = null;
          void refresh();
        }, 1_000);
      },
      { defer: true },
    ),
  );
  onCleanup(() => {
    if (pending !== null) clearTimeout(pending);
  });

  const name = (row: SessionSweepRow) =>
    sessionsStore.sessions[row.session.engine_session_id]?.name ?? row.session.id;

  return (
    <section class="oversight" aria-label="Session oversight">
      <header class="oversight-header">
        <div>
          <h2>Oversight</h2>
          <p class="oversight-summary" aria-live="polite">
            <Show when={result()} fallback={loading() ? "Sweeping sessions…" : "No answer yet"}>
              {(r) => (
                <>
                  <strong>{r().counts.needs_you ?? 0} need you</strong> · {r().counts.total ?? 0} sessions ·
                  swept {ago(r().swept_at, now()) ?? "just now"}
                </>
              )}
            </Show>
          </p>
        </div>
        <button type="button" onClick={() => void refresh()} disabled={loading()}>
          Refresh
        </button>
      </header>
      <Show when={problem()}>
        {(message) => <p class="oversight-problem" role="status">{message()}</p>}
      </Show>
      <Show when={result() && result()!.rows.length === 0 && !problem()}>
        <p class="oversight-empty">No live or hibernated sessions.</p>
      </Show>
      <ol class="oversight-rows">
        <For each={result()?.rows ?? []}>
          {(row) => (
            <li class={`oversight-row oversight-row--${row.attention}`}>
              <div class="oversight-row-head">
                <span class={`oversight-badge oversight-badge--${row.attention}`}>
                  {ATTENTION_LABEL[row.attention] ?? row.attention}
                </span>
                <button
                  type="button"
                  class="oversight-name"
                  onClick={() => props.onOpenSession?.(row.session.engine_session_id, name(row))}
                >
                  {name(row)}
                </button>
                <Show when={row.session.work_item}>
                  {(ref) => <a class="oversight-ref" href={`#/w/${ref()}`}>{ref()}</a>}
                </Show>
                <span class="oversight-when">
                  {ago(row.session.last_output_at, now()) ?? ""}
                </span>
              </div>
              <p class="oversight-reason">{row.attention_reason}</p>
              <Show when={row.session.last_reply_excerpt}>
                {(excerpt) => (
                  <p class="oversight-excerpt" title="The agent's latest reply">
                    <SafeSnippet text={excerpt()} />
                  </p>
                )}
              </Show>
              <Show when={row.screen_tail.length > 0}>
                <pre class="oversight-screen" aria-label={`Last lines of ${name(row)}`}>
                  {row.screen_tail.join("\n")}
                </pre>
              </Show>
            </li>
          )}
        </For>
      </ol>
    </section>
  );
};

export default Oversight;
