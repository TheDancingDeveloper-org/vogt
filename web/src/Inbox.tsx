import { Component, For, Show, createEffect, createSignal, onCleanup, onMount } from "solid-js";
import { useLocation, useNavigate } from "@solidjs/router";
import {
  VogtUnavailable,
  adoptSubject,
  archiveInbox,
  listInbox,
  resolveInboxDrift,
  restoreInbox,
  snoozeInbox,
  suppressSubject,
  type InboxEntry,
  type InboxListResult,
  type InboxSourceCoverage,
} from "./vogtApi";
import { onVogtChanged } from "./store";
import Dialog from "./Dialog";
import SurfaceHeader from "./SurfaceHeader";

interface Props {
  onError?: (message: string) => void;
}

const SOURCES = ["github", "drift", "ci", "agent"] as const;

function age(value: string | null | undefined): string {
  if (!value) return "age unavailable";
  const seconds = Math.max(0, Math.round((Date.now() - Date.parse(value)) / 1000));
  if (seconds < 60) return `${seconds}s ago`;
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m ago`;
  if (seconds < 86400) return `${Math.floor(seconds / 3600)}h ago`;
  return `${Math.floor(seconds / 86400)}d ago`;
}

function coverageCopy(source: string, entry: InboxSourceCoverage | undefined): string {
  if (!entry) return "Not collected: this source is not present in the Inbox response.";
  if (entry.status === "unconfigured" || entry.status === "unswept") {
    return `Not collected: ${entry.detail ?? `${source} has no current coverage.`}`;
  }
  if (entry.status === "failed") return `Collection failed: ${entry.detail ?? "the source did not answer."}`;
  if (entry.count === 0) return "Nothing needs attention in the covered source.";
  return `${entry.count} attention item(s), coverage ${age(entry.observed_at)}`;
}

const Coverage: Component<{ result: InboxListResult }> = (props) => (
  <section class="inbox-coverage" aria-label="Inbox source coverage">
    <h2>Coverage and provenance</h2>
    <p class="inbox-scope">
      {props.result.instance_scope ?? "Inbox is scoped to this Vogt instance."}
    </p>
    <div class="inbox-coverage-grid">
      <For each={SOURCES}>
        {(source) => {
          const item = () => props.result.coverage[source];
          return (
            <div class={`inbox-coverage-item inbox-coverage-${item()?.status ?? "missing"}`}>
              <strong>{source}</strong>
              <span>{coverageCopy(source, item())}</span>
            </div>
          );
        }}
      </For>
      <div class="inbox-coverage-item inbox-coverage-agent">
        <strong>engine</strong>
        <span>{props.result.engine_available === false ? "Not available: live session attention cannot be read." : "Live session coverage is reported by the server."}</span>
      </div>
    </div>
    <p class="inbox-view-age">
      Inbox response: {age(props.result.snapshot_at)}
      <Show when={props.result.high_water}>
        {(water) => <span> · source high-water: {Object.values(water()).filter(Boolean).length}/4 reported</span>}
      </Show>
    </p>
  </section>
);

interface EntryProps {
  entry: InboxEntry;
  seen: boolean;
  selected: boolean;
  busy: boolean;
  phone: boolean;
  onSelect: (entry: InboxEntry) => void;
  onOpen: (entry: InboxEntry) => void;
  onTriage: (
    entry: InboxEntry,
    action: "archive" | "snooze" | "restore",
    reason: string,
    until?: string,
  ) => Promise<string | null>;
  onAction: (
    entry: InboxEntry,
    action: "adopt" | "suppress" | "accept" | "reject",
    reason: string,
  ) => Promise<string | null>;
}

type EntryAction = "archive" | "snooze" | "restore" | "adopt" | "suppress" | "accept" | "reject";

const Entry: Component<EntryProps> = (props) => {
  const [composing, setComposing] = createSignal<EntryAction | null>(null);
  const [reason, setReason] = createSignal("");
  const [refusal, setRefusal] = createSignal<string | null>(null);
  const [sheetOpen, setSheetOpen] = createSignal(false);
  const [until, setUntil] = createSignal((() => {
    const next = new Date(Date.now() + 24 * 60 * 60 * 1000);
    next.setMinutes(next.getMinutes() - next.getTimezoneOffset());
    return next.toISOString().slice(0, 16);
  })());
  const begin = (action: EntryAction) => {
    setComposing(action);
    setRefusal(null);
    queueMicrotask(() => {
      document.getElementById(`inbox-reason-${encodeURIComponent(props.entry.entry_key)}`)?.focus();
    });
  };
  const cancel = () => {
    setComposing(null);
    setReason("");
    setRefusal(null);
  };
  const closeSheet = () => {
    setSheetOpen(false);
    cancel();
    if (window.history.state?.vogtInboxActionSheet) window.history.back();
  };
  const openSheet = () => {
    window.history.pushState(
      { ...(window.history.state ?? {}), vogtInboxActionSheet: true },
      "",
      window.location.href,
    );
    setSheetOpen(true);
  };
  onMount(() => {
    const onPopState = () => {
      if (sheetOpen()) {
        setSheetOpen(false);
        cancel();
      }
    };
    window.addEventListener("popstate", onPopState);
    onCleanup(() => window.removeEventListener("popstate", onPopState));
  });
  const submit = async (action: "archive" | "snooze" | "restore") => {
    const error = await props.onTriage(
      props.entry,
      action,
      reason(),
      action === "snooze" ? new Date(until()).toISOString() : undefined,
    );
    if (error) setRefusal(error);
    else if (props.phone) closeSheet();
    else cancel();
  };
  const submitAction = async (action: "adopt" | "suppress" | "accept" | "reject") => {
    const error = await props.onAction(props.entry, action, reason());
    if (error) setRefusal(error);
    else if (props.phone) closeSheet();
    else cancel();
  };
  return (
    <article
      class={`inbox-entry ${props.seen ? "inbox-entry-seen" : "inbox-entry-unseen"}`}
      data-entry-key={props.entry.entry_key}
      tabIndex={0}
    >
      <div class="inbox-entry-mark" aria-label={props.seen ? "Seen" : "Unread"} />
      <div class="inbox-entry-body">
        <div class="inbox-entry-heading">
          {/* The box is small; the thing a thumb has to hit is not. The label
              is what makes the 44px target actually toggle the box. */}
          <label class="vogt-tickbox">
            <input
              type="checkbox"
              aria-label={`Select ${props.entry.title}`}
              checked={props.selected}
              onChange={() => props.onSelect(props.entry)}
            />
          </label>
          <span class="inbox-source">{props.entry.source}</span>
          <h2>{props.entry.title}</h2>
        </div>
        <p class="inbox-entry-summary">{props.entry.summary ?? "No summary was provided by the server."}</p>
        <Show when={props.entry.evidence_snapshot || props.entry.proposed_change}>
          <section class="inbox-evidence" aria-label="Drift evidence">
            <strong>Evidence before action</strong>
            <Show when={props.entry.evidence_snapshot}>
              {(evidence) => <pre>{JSON.stringify(evidence(), null, 2)}</pre>}
            </Show>
            <Show when={props.entry.proposed_change}>
              {(change) => <pre>Proposed change: {JSON.stringify(change(), null, 2)}</pre>}
            </Show>
          </section>
        </Show>
        <div class="inbox-entry-meta">
          <span>Occurred {age(props.entry.occurred_at)}</span>
          <Show when={props.entry.observed_at}><span>Observed {age(props.entry.observed_at)}</span></Show>
          <Show when={props.entry.project_slug}><span>Project: {props.entry.project_slug}</span></Show>
          <Show when={props.entry.work_item_ref}><span>Work item: {props.entry.work_item_ref}</span></Show>
          <span>Source: {props.entry.source_subject_key ?? "server-normalized entry"}</span>
          <Show when={props.entry.trust_state}><span>Trust: {props.entry.trust_state}</span></Show>
          <span>State: {props.entry.triage_state}</span>
        </div>
        <div class="inbox-entry-actions">
          <button type="button" class="inbox-open" onClick={() => props.onOpen(props.entry)}>
            Open entry
          </button>
          <Show when={!props.phone}>
            <Show when={props.entry.triage_state === "active"}>
              <button type="button" disabled={props.busy} onClick={() => begin("archive")}>Archive…</button>
              <button type="button" disabled={props.busy} onClick={() => begin("snooze")}>Snooze…</button>
              <Show when={props.entry.action?.kind === "observation"}>
                <button type="button" disabled={props.busy} onClick={() => begin("adopt")}>Adopt as work item…</button>
                <button type="button" disabled={props.busy} onClick={() => begin("suppress")}>Suppress source…</button>
              </Show>
              <Show when={props.entry.action?.kind === "drift" && props.entry.evidence_snapshot && props.entry.proposed_change}>
                <button type="button" disabled={props.busy} onClick={() => begin("accept")}>Accept proposed change…</button>
                <button type="button" disabled={props.busy} onClick={() => begin("reject")}>Reject proposed change…</button>
              </Show>
            </Show>
            <Show when={props.entry.triage_state !== "active"}>
              <button type="button" disabled={props.busy} onClick={() => begin("restore")}>Restore…</button>
            </Show>
          </Show>
          <Show when={props.phone}>
            <button type="button" class="inbox-actions-trigger" disabled={props.busy} onClick={openSheet}>
              Inbox actions
            </button>
          </Show>
        </div>
        <Show when={props.phone ? null : composing()}>
          {(chosen) => (
            <form
              class="inbox-entry-composer"
              aria-label={`${chosen()} ${props.entry.title}`}
              onSubmit={(event) => {
                event.preventDefault();
                const action = chosen();
                if (action === "archive" || action === "snooze" || action === "restore") {
                  void submit(action);
                } else {
                  void submitAction(action);
                }
              }}
            >
              <strong>{chosen().replaceAll("_", " ")}</strong>
              <label>
                <span>Reason</span>
                <input
                  id={`inbox-reason-${encodeURIComponent(props.entry.entry_key)}`}
                  value={reason()}
                  onInput={(event) => setReason(event.currentTarget.value)}
                  placeholder="Why this triage decision?"
                />
              </label>
              <Show when={chosen() === "snooze"}>
                <label>
                  <span>Snooze until</span>
                  <input type="datetime-local" value={until()} onInput={(event) => setUntil(event.currentTarget.value)} />
                </label>
              </Show>
              <div class="inbox-entry-composer-actions">
                <button type="submit" disabled={props.busy || !reason().trim()}>
                  {props.busy ? "Submitting…" : `Confirm ${chosen()}`}
                </button>
                <button type="button" disabled={props.busy} onClick={cancel}>Cancel</button>
              </div>
              <Show when={refusal()}>{(message) => <p class="inbox-refusal" role="alert">{message()}</p>}</Show>
            </form>
          )}
        </Show>
      </div>
      <Show when={props.phone && sheetOpen()}>
        <Dialog
          label={`Actions for ${props.entry.title}`}
          dialogClass="inbox-action-sheet"
          backdropClass="inbox-action-sheet-backdrop"
          onClose={closeSheet}
        >
          <button type="button" class="inbox-sheet-close" onClick={closeSheet}>Close</button>
          <div class="inbox-sheet-actions">
            <Show when={props.entry.triage_state === "active"}>
              <button type="button" disabled={props.busy} onClick={() => begin("archive")}>Archive…</button>
              <button type="button" disabled={props.busy} onClick={() => begin("snooze")}>Snooze…</button>
              <Show when={props.entry.action?.kind === "observation"}>
                <button type="button" disabled={props.busy} onClick={() => begin("adopt")}>Adopt as work item…</button>
                <button type="button" disabled={props.busy} onClick={() => begin("suppress")}>Suppress source…</button>
              </Show>
              <Show when={props.entry.action?.kind === "drift" && props.entry.evidence_snapshot && props.entry.proposed_change}>
                <button type="button" disabled={props.busy} onClick={() => begin("accept")}>Accept proposed change…</button>
                <button type="button" disabled={props.busy} onClick={() => begin("reject")}>Reject proposed change…</button>
              </Show>
            </Show>
            <Show when={props.entry.triage_state !== "active"}>
              <button type="button" disabled={props.busy} onClick={() => begin("restore")}>Restore…</button>
            </Show>
          </div>
          <Show when={composing()}>
            {(chosen) => (
              <form
                class="inbox-entry-composer"
                aria-label={`${chosen()} ${props.entry.title}`}
                onSubmit={(event) => {
                  event.preventDefault();
                  const action = chosen();
                  if (action === "archive" || action === "snooze" || action === "restore") void submit(action);
                  else void submitAction(action);
                }}
              >
                <strong>{chosen().replaceAll("_", " ")}</strong>
                <label>
                  <span>Reason</span>
                  <input
                    id={`inbox-reason-${encodeURIComponent(props.entry.entry_key)}`}
                    value={reason()}
                    onInput={(event) => setReason(event.currentTarget.value)}
                    placeholder="Why this triage decision?"
                  />
                </label>
                <Show when={chosen() === "snooze"}>
                  <label>
                    <span>Snooze until</span>
                    <input type="datetime-local" value={until()} onInput={(event) => setUntil(event.currentTarget.value)} />
                  </label>
                </Show>
                <div class="inbox-entry-composer-actions">
                  <button type="submit" disabled={props.busy || !reason().trim()}>
                    {props.busy ? "Submitting…" : `Confirm ${chosen()}`}
                  </button>
                  <button type="button" disabled={props.busy} onClick={cancel}>Cancel</button>
                </div>
                <Show when={refusal()}>{(message) => <p class="inbox-refusal" role="alert">{message()}</p>}</Show>
              </form>
            )}
          </Show>
        </Dialog>
      </Show>
    </article>
  );
};

const Inbox: Component<Props> = (props) => {
  const navigate = useNavigate();
  const location = useLocation();
  const [result, setResult] = createSignal<InboxListResult | null>(null);
  const [failure, setFailure] = createSignal<Error | null>(null);
  const [loading, setLoading] = createSignal(false);
  const [cursor, setCursor] = createSignal<string | null>(null);
  const [seen, setSeen] = createSignal<Set<string>>(new Set<string>());
  const [source, setSource] = createSignal("");
  const [triaging, setTriaging] = createSignal<string | null>(null);
  const [selected, setSelected] = createSignal<Set<string>>(new Set<string>());
  const [batchReason, setBatchReason] = createSignal("");
  const [batchAction, setBatchAction] = createSignal<"selected" | "read" | null>(null);
  const [batchBusy, setBatchBusy] = createSignal(false);
  const [focusedIndex, setFocusedIndex] = createSignal(0);
  const [phone, setPhone] = createSignal(false);

  const seenKey = "mydevenv2.inbox.seen.v1";
  const readSeen = () => {
    try {
      const parsed = JSON.parse(localStorage.getItem(seenKey) ?? "[]") as unknown;
      if (Array.isArray(parsed)) {
        const keys = parsed.filter((key): key is string => typeof key === "string");
        setSeen(new Set<string>(keys));
      }
    } catch {
      setSeen(new Set<string>());
    }
  };
  const writeSeen = (key: string) => {
    const next = new Set<string>(seen());
    next.add(key);
    setSeen(next);
    try { localStorage.setItem(seenKey, JSON.stringify([...next])); } catch { /* presentation state is best effort */ }
  };

  const load = async (nextCursor: string | null = null) => {
    setLoading(true);
    setFailure(null);
    try {
      const query = new URLSearchParams(location.search);
      const selectedSource = query.get("source") ?? source();
      const next = await listInbox({
        limit: 50,
        cursor: nextCursor ?? undefined,
        sources: selectedSource || undefined,
        project: query.get("project") ?? undefined,
        work_item: query.get("work_item") ?? undefined,
      });
      setResult(nextCursor ? { ...next, entries: [...(result()?.entries ?? []), ...next.entries] } : next);
      setCursor(next.next_cursor ?? null);
    } catch (error) {
      setFailure(error instanceof Error ? error : new Error(String(error)));
      setResult(null);
      if (!(error instanceof VogtUnavailable)) props.onError?.(String(error));
    } finally {
      setLoading(false);
    }
  };

  const applySource = (next: string) => {
    setSource(next);
    navigate(next ? `/inbox?source=${encodeURIComponent(next)}` : "/inbox", { replace: true });
  };

  createEffect(() => {
    location.pathname;
    location.search;
    setSource(new URLSearchParams(location.search).get("source") ?? "");
    readSeen();
    void load();
  });
  onCleanup(onVogtChanged(() => { if (!loading()) void load(); }));

  const entries = () => result()?.entries ?? [];
  const toggleSelected = (entry: InboxEntry) => {
    setSelected((current) => {
      const next = new Set(current);
      if (next.has(entry.entry_key)) next.delete(entry.entry_key);
      else next.add(entry.entry_key);
      return next;
    });
  };

  const archiveKeys = async (keys: string[]) => {
    const reason = batchReason().trim();
    if (!keys.length) return;
    if (!reason) {
      props.onError?.("A reason is required for the batch Inbox decision.");
      return;
    }
    setBatchBusy(true);
    const failed: string[] = [];
    try {
      for (const key of keys) {
        try {
          await archiveInbox(key, reason);
        } catch {
          failed.push(key);
        }
      }
      setSelected(new Set(failed));
      if (!failed.length) {
        setBatchReason("");
        setBatchAction(null);
      }
      if (failed.length) props.onError?.(`${failed.length} Inbox archive(s) were refused; the selection was retained.`);
      await load();
    } finally {
      setBatchBusy(false);
    }
  };

  const archiveSelected = () => archiveKeys([...selected()]);

  const archiveRead = async () => {
    const readKeys = entries()
      .filter((entry) => seen().has(entry.entry_key) && entry.triage_state === "active")
      .map((entry) => entry.entry_key);
    if (readKeys.length) await archiveKeys(readKeys);
  };

  const focusEntry = (index: number) => {
    const bounded = Math.max(0, Math.min(index, entries().length - 1));
    setFocusedIndex(bounded);
    const entry = entries()[bounded];
    if (entry) {
      document.querySelector<HTMLElement>(`[data-entry-key="${CSS.escape(entry.entry_key)}"]`)?.focus();
    }
  };

  const focusReason = (index: number, action: "archive" | "snooze" | "resolve") => {
    const entry = entries()[Math.max(0, Math.min(index, entries().length - 1))];
    if (!entry) return;
    const labels = action === "archive"
      ? ["Archive…"]
      : action === "snooze"
        ? ["Snooze…"]
        : entry.action?.kind === "drift"
          ? ["Reject proposed change…", "Accept proposed change…"]
          : entry.triage_state === "active"
            ? ["Suppress source…", "Adopt as work item…"]
            : ["Restore…"];
    const root = document.querySelector<HTMLElement>(
      `[data-entry-key="${CSS.escape(entry.entry_key)}"]`,
    );
    const button = [...(root?.querySelectorAll<HTMLButtonElement>(
      ".inbox-entry-actions button",
    ) ?? [])].find((candidate) => labels.includes(candidate.textContent?.trim() ?? ""));
    button?.click();
  };

  const onKeyDown = (event: KeyboardEvent) => {
    if (event.target instanceof HTMLInputElement || event.target instanceof HTMLSelectElement) return;
    const key = event.key.toLowerCase();
    if (key === "j" || key === "arrowdown") {
      event.preventDefault();
      focusEntry(focusedIndex() + 1);
    } else if (key === "k" || key === "arrowup") {
      event.preventDefault();
      focusEntry(focusedIndex() - 1);
    } else if (key === "e" || key === "s" || key === "r") {
      event.preventDefault();
      focusReason(focusedIndex(), key === "s" ? "snooze" : key === "r" ? "resolve" : "archive");
    }
  };
  onMount(() => {
    const query = window.matchMedia("(max-width: 768px)");
    const updatePhone = () => setPhone(query.matches);
    updatePhone();
    query.addEventListener("change", updatePhone);
    window.addEventListener("keydown", onKeyDown);
    onCleanup(() => {
      query.removeEventListener("change", updatePhone);
      window.removeEventListener("keydown", onKeyDown);
    });
  });

  const openEntry = (entry: InboxEntry) => {
    writeSeen(entry.entry_key);
    if (entry.work_item_ref) navigate(`/w/${encodeURIComponent(entry.work_item_ref)}`);
    else if (entry.session_id) navigate(`/t/${encodeURIComponent(entry.session_id)}`);
  };

  const triage = async (
    entry: InboxEntry,
    action: "archive" | "snooze" | "restore",
    reason: string,
    until?: string,
  ): Promise<string | null> => {
    if (!reason.trim()) {
      return "A reason is required for every Inbox triage decision.";
    }
    setTriaging(entry.entry_key);
    try {
      if (action === "archive") await archiveInbox(entry.entry_key, reason);
      else if (action === "restore") await restoreInbox(entry.entry_key, reason);
      else if (until) await snoozeInbox(entry.entry_key, until, reason);
      await load();
      return null;
    } catch (error) {
      return error instanceof Error ? error.message : String(error);
    } finally {
      setTriaging(null);
    }
  };

  const action = async (
    entry: InboxEntry,
    kind: "adopt" | "suppress" | "accept" | "reject",
    reason: string,
  ): Promise<string | null> => {
    if (!reason.trim()) {
      return "A reason is required for every Inbox decision.";
    }
    setTriaging(entry.entry_key);
    try {
      const target = entry.action?.subject_key ?? entry.source_subject_key;
      if (!target) throw new Error("Inbox entry has no adoptable source subject.");
      if (kind === "adopt") await adoptSubject(target, reason);
      else if (kind === "suppress") await suppressSubject(target, reason);
      else if (entry.action?.kind === "drift" && entry.action.drift_id) {
        await resolveInboxDrift(entry.action.drift_id, kind === "accept" ? "accepted" : "rejected", reason);
      }
      await load();
      return null;
    } catch (error) {
      return error instanceof Error ? error.message : String(error);
    } finally {
      setTriaging(null);
    }
  };

  return (
    <section class="vogt-surface inbox inbox-surface" aria-label="Inbox">
      <SurfaceHeader
        class="inbox-header"
        label="Inbox header"
        title={(
          <>
          <p class="place-kicker">Work</p>
          <h1>Inbox</h1>
          </>
        )}
        honestyClass={failure() ? "surface-header-honesty--outage" : loading() && !result() ? "surface-header-honesty--partial" : "surface-header-honesty--fresh"}
        honesty={(
          <p class="inbox-header-honesty" aria-live="polite">
            {failure()
              ? "Inbox unavailable — no normalized attention answer was read."
              : loading() && !result()
                ? "Loading Inbox — no answer yet."
                : result()
                  ? `Response ${age(result()!.snapshot_at)}; source coverage remains attached below.`
                  : "No Inbox answer has been read yet."}
          </p>
        )}
        controls={(
          <Show when={phone()} fallback={(
            <label class="inbox-filter">
              <span>Source</span>
              <select value={source()} onChange={(event) => applySource(event.currentTarget.value)}>
                <option value="">All sources</option>
                <For each={SOURCES}>{(value) => <option value={value}>{value}</option>}</For>
              </select>
            </label>
          )}>
            <div class="inbox-filter-pills" role="group" aria-label="Source filter">
              <For each={["", ...SOURCES]}>
                {(value) => (
                  <button
                    type="button"
                    class={source() === value ? "active" : ""}
                    aria-pressed={source() === value}
                    onClick={() => applySource(value)}
                  >
                    {value || "All sources"}
                  </button>
                )}
              </For>
            </div>
          </Show>
        )}
        detail={(
          <details class="surface-header-disclosure">
            <summary>About this view</summary>
            <p>One server-ordered attention view. Provenance and coverage stay attached to every answer.</p>
          </details>
        )}
      />
      <Show when={failure()}>
        {(error) => (
          <div class="inbox-outage" role="alert">
            <h2>Inbox is unavailable</h2>
            <p>{error() instanceof VogtUnavailable ? error().message : `The normalized Inbox contract could not be read: ${error().message}`}</p>
            <p>Nothing is shown because no normalized attention rows were read. This surface does not merge legacy notification, drift, or event reads.</p>
            <button type="button" onClick={() => void load()}>Retry Inbox</button>
          </div>
        )}
      </Show>
      <Show when={!failure() && result() && result()!.entries.length === 0}>
        <p class="inbox-empty">No normalized entries were returned. Open coverage and provenance below: a covered-empty source is different from a source that was not collected.</p>
      </Show>
      <div class="inbox-list" aria-label="Attention stream" aria-live="polite">
        <For each={entries()}>
          {(entry, index) => <Entry entry={entry} seen={seen().has(entry.entry_key)} selected={selected().has(entry.entry_key)} busy={triaging() === entry.entry_key} phone={phone()} onSelect={toggleSelected} onOpen={(value) => { setFocusedIndex(index()); openEntry(value); }} onTriage={triage} onAction={action} />}
        </For>
      </div>
      <Show when={result()?.next_cursor}>
        <button type="button" class="inbox-load-more" disabled={loading()} onClick={() => void load(cursor())}>
          {loading() ? "Loading…" : "Load more"}
        </button>
      </Show>
      <Show when={result()}>
        {(answer) => (
          <details class="inbox-support" open={answer().entries.length === 0}>
            <summary>Coverage and provenance</summary>
            <Coverage result={answer()} />
          </details>
        )}
      </Show>
      <details class="inbox-support" open={selected().size > 0 || batchAction() !== null}>
        <summary>Batch operations · {selected().size} selected</summary>
        <div class="inbox-batch" aria-label="Batch Inbox actions">
          <span>{selected().size} selected</span>
          <button
            type="button"
            disabled={batchBusy() || selected().size === 0}
            onClick={() => setBatchAction("selected")}
          >
            Archive selected…
          </button>
          <button
            type="button"
            disabled={batchBusy() || !entries().some((entry) => seen().has(entry.entry_key) && entry.triage_state === "active")}
            onClick={() => setBatchAction("read")}
          >
            Archive read…
          </button>
          <Show when={batchAction()}>
            <form
              class="inbox-batch-composer"
              onSubmit={(event) => {
                event.preventDefault();
                if (batchAction() === "selected") void archiveSelected();
                else void archiveRead();
              }}
            >
              <label>
                <span>Reason</span>
                <input
                  aria-label="Batch reason"
                  value={batchReason()}
                  onInput={(event) => setBatchReason(event.currentTarget.value)}
                  placeholder="Why archive these entries?"
                />
              </label>
              <button type="submit" disabled={batchBusy() || !batchReason().trim()}>
                {batchBusy() ? "Archiving…" : "Confirm archive"}
              </button>
              <button type="button" disabled={batchBusy()} onClick={() => setBatchAction(null)}>Cancel</button>
            </form>
          </Show>
        </div>
      </details>
    </section>
  );
};

export default Inbox;
