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
import { formatBytes } from "./sessionRowModel";
import {
  VogtUnavailable,
  answerSessionInVogt,
  sweepSessions,
  type SessionSweepResult,
  type SessionSweepRow,
} from "./vogtApi";

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
/** "claude · claude-opus-5-5 · effort high", from the resolved runtime and
 *  template; null when nothing is known. */
export function runtimeWord(session: SessionSweepRow["session"]): string | null {
  const r = session.running;
  const parts = [
    r?.agent ?? session.template ?? null,
    r?.model ?? null,
    r?.effort ? `effort ${r.effort}` : null,
  ].filter((part): part is string => Boolean(part));
  return parts.length ? parts.join(" · ") : null;
}

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
  const [heaviestFirst, setHeaviestFirst] = createSignal(false);
  const rows = () => {
    const all = result()?.rows ?? [];
    if (!heaviestFirst()) return all;
    return [...all].sort(
      (a, b) => (b.session.resources?.rss_bytes ?? -1) - (a.session.resources?.rss_bytes ?? -1),
    );
  };
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
        <div class="oversight-controls">
          <label>
            <input
              type="checkbox"
              checked={heaviestFirst()}
              onChange={(event) => setHeaviestFirst(event.currentTarget.checked)}
            />
            Heaviest first
          </label>
          <button type="button" onClick={() => void refresh()} disabled={loading()}>
            Refresh
          </button>
        </div>
      </header>
      <Show when={problem()}>
        {(message) => <p class="oversight-problem" role="status">{message()}</p>}
      </Show>
      <Show when={result() && result()!.rows.length === 0 && !problem()}>
        <p class="oversight-empty">No live or hibernated sessions.</p>
      </Show>
      <ol class="oversight-rows">
        <For each={rows()}>
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
                <Show when={row.session.permission_mode}>
                  {(mode) => (
                    <span
                      class={`oversight-posture${mode() === "bypass" ? " oversight-posture--bypass" : ""}`}
                      title={mode() === "bypass" ? "Started with no permission checks" : "Edits auto-accepted"}
                    >
                      {mode() === "bypass" ? "⚠ no permission checks" : mode()}
                    </span>
                  )}
                </Show>
                <Show when={runtimeWord(row.session)}>
                  {(word) => (
                    <span
                      class="oversight-runtime"
                      title={[
                        row.session.template ? `template ${row.session.template}` : null,
                        row.session.running?.model_basis ? `model from ${row.session.running.model_basis}` : null,
                        row.session.running?.effort_basis ? `effort from ${row.session.running.effort_basis}` : null,
                      ].filter(Boolean).join(" · ")}
                    >
                      {word()}
                    </span>
                  )}
                </Show>
                <Show when={row.session.resources}>
                  {(r) => (
                    <span
                      class={`oversight-resources${r().over_threshold ? " oversight-resources--over" : ""}`}
                      title={`${r().processes} processes`}
                    >
                      {r().over_threshold ? "⚠ " : ""}
                      {formatBytes(r().rss_bytes)} · {Math.round(r().cpu_pct)}% CPU
                    </span>
                  )}
                </Show>
                <span class="oversight-when">
                  {ago(row.session.last_output_at, now()) ?? ""}
                </span>
              </div>
              <p class="oversight-reason">{row.attention_reason}</p>
              <Show when={row.attention === "approval" && row.session.approval?.options?.length ? row.session.approval : null}>
                {(approval) => (
                  <div class="oversight-answers" role="group" aria-label={`Answer: ${approval().question}`}>
                    <For each={approval().options ?? []}>
                      {(option) => (
                        <button
                          type="button"
                          class={option.selected ? "oversight-answer oversight-answer--selected" : "oversight-answer"}
                          onClick={async () => {
                            try {
                              await answerSessionInVogt(row.session.engine_session_id, option.number, approval().question);
                              void refresh();
                            } catch (error) {
                              props.onError?.(`answer failed: ${(error as Error).message}`);
                              void refresh();
                            }
                          }}
                        >
                          {option.number}. {option.label}
                        </button>
                      )}
                    </For>
                  </div>
                )}
              </Show>
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
