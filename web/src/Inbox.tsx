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
import {
  ViewAgeBadge,
  createLoadStamp,
  createViewAge,
  honestyToneClass,
  onVogtLive,
} from "./viewAge";
import Dialog from "./Dialog";
import SurfaceHeader from "./SurfaceHeader";

/** A datetime-local value (no timezone) for `now + days`. */
function localDatetimeIn(days: number): string {
  const next = new Date(Date.now() + days * 24 * 60 * 60 * 1000);
  next.setMinutes(next.getMinutes() - next.getTimezoneOffset());
  return next.toISOString().slice(0, 16);
}

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
  /** Report whether this entry currently holds an unsaved composer open, so a
   *  live re-read never swaps the rows out from under a half-typed reason. */
  onComposerChange: (key: string, open: boolean) => void;
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
  const [until, setUntil] = createSignal(localDatetimeIn(1));

  // A composer or an open action sheet is an unsaved edit: while either is
  // open this entry is a reason not to re-read the list under it. The parent
  // guards its live subscription on the sum of these across every entry.
  createEffect(() => {
    props.onComposerChange(props.entry.entry_key, composing() !== null || sheetOpen());
  });
  onCleanup(() => props.onComposerChange(props.entry.entry_key, false));

  // Evidence is dense, so it is folded away except where a reader is most
  // likely to want it open already: the entry they have selected, and a drift
  // entry that carries a change it is proposing they accept or reject.
  const hasProposedDrift = () =>
    Boolean(props.entry.proposed_change) &&
    (props.entry.action?.kind === "drift" || props.entry.source === "drift");
  const evidenceOpen = () => props.selected || hasProposedDrift();

  // "Open entry" navigates in-app; with neither a work item nor a session
  // behind it there is nowhere to go, so the button says so rather than
  // pretending it does nothing on purpose.
  const canOpen = () => Boolean(props.entry.work_item_ref || props.entry.session_id);

  const setSnoozePreset = (days: number) => setUntil(localDatetimeIn(days));

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
          <details class="inbox-evidence" open={evidenceOpen()}>
            <summary>Evidence</summary>
            <section aria-label="Drift evidence">
              <Show when={props.entry.evidence_snapshot}>
                {(evidence) => <pre>{JSON.stringify(evidence(), null, 2)}</pre>}
              </Show>
              <Show when={props.entry.proposed_change}>
                {(change) => <pre>Proposed change: {JSON.stringify(change(), null, 2)}</pre>}
              </Show>
            </section>
          </details>
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
          <button
            type="button"
            class="inbox-open"
            disabled={!canOpen()}
            title={canOpen() ? undefined : "Nothing to open in-app: this entry has no linked work item or session."}
            onClick={() => props.onOpen(props.entry)}
          >
            Open entry
          </button>
          <Show when={props.entry.source_url}>
            {(href) => (
              <a
                class="inbox-open-source"
                href={href()}
                target="_blank"
                rel="noopener noreferrer"
              >
                Open on {props.entry.source}
              </a>
            )}
          </Show>
          <Show when={!props.phone}>
            <Show when={props.entry.triage_state === "active"}>
              <Show when={props.entry.action?.kind === "observation"}>
                <button type="button" disabled={props.busy} onClick={() => begin("adopt")}>Adopt as work item…</button>
                <button type="button" disabled={props.busy} onClick={() => begin("suppress")}>Suppress source…</button>
              </Show>
              <Show when={props.entry.action?.kind === "drift" && props.entry.evidence_snapshot && props.entry.proposed_change}>
                <button type="button" disabled={props.busy} onClick={() => begin("accept")}>Accept proposed change…</button>
                <button type="button" disabled={props.busy} onClick={() => begin("reject")}>Reject proposed change…</button>
              </Show>
              {/* The routine triage lives in a quieter, compact second row so the
                  entry reads first and acts second. */}
              <div class="inbox-entry-actions-secondary">
                <button type="button" disabled={props.busy} onClick={() => begin("archive")}>Archive…</button>
                <button type="button" disabled={props.busy} onClick={() => begin("snooze")}>Snooze…</button>
              </div>
            </Show>
            <Show when={props.entry.triage_state !== "active"}>
              <div class="inbox-entry-actions-secondary">
                <button type="button" disabled={props.busy} onClick={() => begin("restore")}>Restore…</button>
              </div>
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
                <div class="inbox-snooze-presets" role="group" aria-label="Snooze presets">
                  <button type="button" disabled={props.busy} onClick={() => setSnoozePreset(1)}>Tomorrow</button>
                  <button type="button" disabled={props.busy} onClick={() => setSnoozePreset(7)}>Next week</button>
                </div>
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
  const [batchAction, setBatchAction] = createSignal<"selected" | "read" | "all" | null>(null);
  const [batchBusy, setBatchBusy] = createSignal(false);
  // #350: free-text over the loaded entries, alongside the source pills. A
  // display filter, deliberately: it narrows what is drawn, not what the batch
  // actions below reach for — "clear all" clears the source, not the search.
  const [search, setSearch] = createSignal("");
  const [focusedIndex, setFocusedIndex] = createSignal(0);
  const [phone, setPhone] = createSignal(false);
  // How many pages the reader has loaded, so a live re-read reconciles what is
  // on screen rather than collapsing a paged view back to its first page.
  const [pages, setPages] = createSignal(1);
  // The entries whose composer or action sheet is open right now. A live
  // re-read is held while any is, so it never throws away a half-typed reason.
  const [composerKeys, setComposerKeys] = createSignal<Set<string>>(new Set<string>());
  const anyComposerOpen = () => composerKeys().size > 0;
  const onComposerChange = (key: string, open: boolean) => {
    setComposerKeys((current) => {
      if (open === current.has(key)) return current;
      const next = new Set(current);
      if (open) next.add(key);
      else next.delete(key);
      return next;
    });
  };

  // How old this view is, in the shared badge every Vogt surface now wears.
  // The stamp only moves on an answer we believe (FR-U10), and the Inbox
  // listens on the stream, so it is a live view that must still say when it
  // last actually heard back.
  const loadedAt = createLoadStamp(result);
  const freshness = createViewAge(() => ({
    loadedAt: loadedAt(),
    failed: Boolean(failure()),
    live: true,
  }));

  const seenKey = "vogt.inbox.seen.v1";
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

  const fetchPage = (pageCursor: string | null) => {
    const query = new URLSearchParams(location.search);
    const selectedSource = query.get("source") ?? source();
    return listInbox({
      limit: 50,
      cursor: pageCursor ?? undefined,
      sources: selectedSource || undefined,
      project: query.get("project") ?? undefined,
      work_item: query.get("work_item") ?? undefined,
    });
  };

  const handleFailure = (error: unknown) => {
    setFailure(error instanceof Error ? error : new Error(String(error)));
    setResult(null);
    setPages(1);
    setCursor(null);
    if (!(error instanceof VogtUnavailable)) props.onError?.(String(error));
  };

  /** A fresh read of the first page: the URL-changed and retry path, which is
   *  allowed to replace what is on screen because the reader asked it to. */
  const load = async () => {
    setLoading(true);
    setFailure(null);
    try {
      const next = await fetchPage(null);
      setResult(next);
      setCursor(next.next_cursor ?? null);
      setPages(1);
    } catch (error) {
      handleFailure(error);
    } finally {
      setLoading(false);
    }
  };

  /** Deepen the view by one page, appending rather than replacing. */
  const loadMore = async () => {
    const at = cursor();
    if (!at || loading()) return;
    setLoading(true);
    try {
      const next = await fetchPage(at);
      setResult((current) =>
        current ? { ...next, entries: [...current.entries, ...next.entries] } : next,
      );
      setCursor(next.next_cursor ?? null);
      setPages((n) => n + 1);
    } catch (error) {
      handleFailure(error);
    } finally {
      setLoading(false);
    }
  };

  /** Reuse the previous object for an entry whose content is unchanged, so a
   *  live re-read keeps its `<For>` identity — and the composing/selection
   *  signals that hang off that identity — while a changed entry re-renders. */
  const mergeEntries = (prev: InboxEntry[], next: InboxEntry[]): InboxEntry[] => {
    const byKey = new Map(prev.map((entry) => [entry.entry_key, entry] as const));
    return next.map((entry) => {
      const old = byKey.get(entry.entry_key);
      return old && JSON.stringify(old) === JSON.stringify(entry) ? old : entry;
    });
  };

  /** Re-read every page currently loaded and merge by `entry_key`. Preserves
   *  the paged depth, keeps unchanged rows' identity, and — unlike `load` —
   *  never wipes the answer on a failed background read (FR-U10). This is the
   *  live path and the after-a-write path both. */
  const reload = async () => {
    if (loading()) return;
    const depth = pages();
    setLoading(true);
    try {
      const firstPage = await fetchPage(null);
      let collected = [...firstPage.entries];
      let at = firstPage.next_cursor ?? null;
      for (let read = 1; read < depth && at; read += 1) {
        const page = await fetchPage(at);
        collected = [...collected, ...page.entries];
        at = page.next_cursor ?? null;
      }
      setResult((current) => ({
        ...firstPage,
        entries: mergeEntries(current?.entries ?? [], collected),
        next_cursor: at,
      }));
      setCursor(at);
      setFailure(null);
    } catch (error) {
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
  // Live, like the board: re-read on what the front door announced, but not
  // under a reason somebody is composing, and not while a batch decision is
  // half-made. A hidden tab is skipped and reconciled on return — that is
  // `onVogtLive`'s own second half.
  onVogtLive(() => void reload(), {
    when: () => !anyComposerOpen() && batchAction() === null,
  });

  const entries = () => result()?.entries ?? [];
  // #350: what the list draws, after the free-text box. Kept separate from
  // `entries()` so the batch actions keep working on the whole loaded set.
  const visibleEntries = () => {
    const needle = search().trim().toLowerCase();
    if (!needle) return entries();
    return entries().filter((entry) =>
      `${entry.title} ${entry.summary ?? ""}`.toLowerCase().includes(needle),
    );
  };
  const hasReadEntries = () =>
    entries().some((entry) => seen().has(entry.entry_key) && entry.triage_state === "active");
  // The active entries currently loaded — already scoped to the chosen source,
  // because the server filtered them (#353). "Clear all" archives exactly these.
  const activeEntries = () => entries().filter((entry) => entry.triage_state === "active");
  const hasActiveEntries = () => activeEntries().length > 0;
  // The batch bar earns its sticky place when there is something to batch: a
  // selection, a decision half-made, or read entries the reader could sweep.
  // "Clear all" is not in this set: it lives in its own always-available
  // control so the selection bar stays hidden until the reader engages it.
  const batchable = () =>
    Boolean(result()) && (selected().size > 0 || batchAction() !== null || hasReadEntries());
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
      await reload();
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

  // #353: clear every active entry currently in the inbox. It goes through the
  // same `archiveKeys` path, so a per-entry refusal keeps its current
  // behaviour — the refused entries are retained and counted in the toast. The
  // source filter is honoured for free: `entries()` is already the server's
  // source-scoped answer, so a filtered inbox clears only that source.
  const archiveAll = async () => {
    const keys = activeEntries().map((entry) => entry.entry_key);
    if (keys.length) await archiveKeys(keys);
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
      await reload();
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
      await reload();
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
        title={<h1>Inbox</h1>}
        honestyClass={
          failure()
            ? "surface-header-honesty--outage"
            : result()
              ? honestyToneClass(freshness().tone)
              : "surface-header-honesty--never"
        }
        honesty={(
          <p class="inbox-header-honesty" aria-live="polite">
            <Show
              when={!failure()}
              fallback="Inbox unavailable — no normalized attention answer was read."
            >
              <Show
                when={result()}
                fallback={loading() ? "Loading Inbox — no answer yet." : "No Inbox answer has been read yet."}
              >
                <strong><ViewAgeBadge age={freshness()} class="inbox-age" /></strong>
                {" "}· source coverage remains attached below.
              </Show>
            </Show>
          </p>
        )}
        controls={(
          <>
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
            {/* #350: a free-text box beside the source pills, over the loaded
                entries' title and summary. */}
            <label class="inbox-filter inbox-search">
              <span>Search</span>
              <input
                type="search"
                aria-label="Search inbox entries"
                placeholder="Match title or summary"
                value={search()}
                onInput={(event) => setSearch(event.currentTarget.value)}
              />
            </label>
          </>
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
      {/* Batch decisions ride at the top and stay put: with a selection made,
          the bar the reader is acting through follows the list down rather
          than sitting buried under it. */}
      <Show when={batchable()}>
        <div
          class="inbox-batch inbox-batch-bar"
          classList={{ "inbox-batch-bar--selected": selected().size > 0 }}
          aria-label="Batch Inbox actions"
        >
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
            disabled={batchBusy() || !hasReadEntries()}
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
                else if (batchAction() === "read") void archiveRead();
                else void archiveAll();
              }}
            >
              <Show when={batchAction() === "all"}>
                <p class="inbox-batch-confirm" role="alert">
                  {source()
                    ? `Archive all ${activeEntries().length} active ${source()} entr${
                        activeEntries().length === 1 ? "y" : "ies"
                      } in view? Archiving is restorable.`
                    : `Archive all ${activeEntries().length} active entr${
                        activeEntries().length === 1 ? "y" : "ies"
                      } in view? Archiving is restorable.`}
                </p>
              </Show>
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
                {batchBusy()
                  ? "Archiving…"
                  : batchAction() === "all"
                    ? "Confirm clear all"
                    : "Confirm archive"}
              </button>
              <button type="button" disabled={batchBusy()} onClick={() => setBatchAction(null)}>Cancel</button>
            </form>
          </Show>
        </div>
      </Show>
      {/* #353: clear every active entry in view. Its own always-available
          control, so the selection bar above stays hidden until engaged. The
          label names the source when one filters, so it is never mistaken for
          a whole-inbox sweep; clicking opens the confirm composer above. */}
      <Show when={hasActiveEntries() && batchAction() !== "all"}>
        <div class="inbox-clearall" aria-label="Clear the inbox">
          <button
            type="button"
            class="inbox-clear-all"
            disabled={batchBusy()}
            onClick={() => setBatchAction("all")}
          >
            {source() ? `Clear all ${source()}…` : "Clear all…"}
          </button>
        </div>
      </Show>
      <p class="board-keys inbox-keys" aria-hidden="true">
        Keyboard: <kbd>j</kbd>/<kbd>k</kbd> move · <kbd>e</kbd> archive ·{" "}
        <kbd>s</kbd> snooze · <kbd>r</kbd> resolve
      </p>
      <Show when={search().trim() && visibleEntries().length === 0 && entries().length > 0}>
        <p class="inbox-empty">
          No loaded entries match “{search().trim()}”. The search covers the entries
          on screen; load more or clear it to see the rest.
        </p>
      </Show>
      <div class="inbox-list" aria-label="Attention stream" aria-live="polite">
        <For each={visibleEntries()}>
          {(entry, index) => <Entry entry={entry} seen={seen().has(entry.entry_key)} selected={selected().has(entry.entry_key)} busy={triaging() === entry.entry_key} phone={phone()} onSelect={toggleSelected} onOpen={(value) => { setFocusedIndex(index()); openEntry(value); }} onComposerChange={onComposerChange} onTriage={triage} onAction={action} />}
        </For>
      </div>
      <Show when={result()?.next_cursor}>
        <button type="button" class="inbox-load-more" disabled={loading()} onClick={() => void loadMore()}>
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
    </section>
  );
};

export default Inbox;
