import { Component, For, Show, createEffect, createSignal, onCleanup } from "solid-js";
import { useLocation, useNavigate } from "@solidjs/router";
import {
  VogtUnavailable,
  archiveInbox,
  listInbox,
  restoreInbox,
  snoozeInbox,
  type InboxEntry,
  type InboxListResult,
  type InboxSourceCoverage,
} from "./vogtApi";
import { onVogtChanged } from "./store";

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
    <p class="inbox-view-age">Inbox response: {age(props.result.snapshot_at)}</p>
  </section>
);

interface EntryProps {
  entry: InboxEntry;
  seen: boolean;
  busy: boolean;
  onOpen: (entry: InboxEntry) => void;
  onTriage: (
    entry: InboxEntry,
    action: "archive" | "snooze" | "restore",
    reason: string,
    until?: string,
  ) => void;
}

const Entry: Component<EntryProps> = (props) => {
  const [reason, setReason] = createSignal("");
  const [until, setUntil] = createSignal((() => {
    const next = new Date(Date.now() + 24 * 60 * 60 * 1000);
    next.setMinutes(next.getMinutes() - next.getTimezoneOffset());
    return next.toISOString().slice(0, 16);
  })());
  const submit = (action: "archive" | "snooze" | "restore") => {
    props.onTriage(
      props.entry,
      action,
      reason(),
      action === "snooze" ? new Date(until()).toISOString() : undefined,
    );
  };
  return (
    <article class={`inbox-entry ${props.seen ? "inbox-entry-seen" : "inbox-entry-unseen"}`}>
      <div class="inbox-entry-mark" aria-label={props.seen ? "Seen" : "Unread"} />
      <div class="inbox-entry-body">
        <div class="inbox-entry-heading">
          <span class="inbox-source">{props.entry.source}</span>
          <h2>{props.entry.title}</h2>
        </div>
        <p class="inbox-entry-summary">{props.entry.summary ?? "No summary was provided by the server."}</p>
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
          <label>
            <span>Reason</span>
            <input
              value={reason()}
              onInput={(event) => setReason(event.currentTarget.value)}
              placeholder="Why this triage decision?"
            />
          </label>
          <Show when={props.entry.triage_state === "active"}>
            <button type="button" disabled={props.busy} onClick={() => submit("archive")}>Archive</button>
            <label>
              <span>Snooze until</span>
              <input type="datetime-local" value={until()} onInput={(event) => setUntil(event.currentTarget.value)} />
            </label>
            <button type="button" disabled={props.busy} onClick={() => submit("snooze")}>Snooze</button>
          </Show>
          <Show when={props.entry.triage_state !== "active"}>
            <button type="button" disabled={props.busy} onClick={() => submit("restore")}>Restore</button>
          </Show>
        </div>
      </div>
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
      const next = await listInbox({
        limit: 50,
        cursor: nextCursor ?? undefined,
        sources: source() || undefined,
        project: new URLSearchParams(location.search).get("project") ?? undefined,
        work_item: new URLSearchParams(location.search).get("work_item") ?? undefined,
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
    readSeen();
    void load();
  });
  onCleanup(onVogtChanged(() => { if (!loading()) void load(); }));

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
  ) => {
    if (!reason.trim()) {
      props.onError?.("A reason is required for every Inbox triage decision.");
      return;
    }
    setTriaging(entry.entry_key);
    try {
      if (action === "archive") await archiveInbox(entry.entry_key, reason);
      else if (action === "restore") await restoreInbox(entry.entry_key, reason);
      else if (until) await snoozeInbox(entry.entry_key, until, reason);
      await load();
    } catch (error) {
      props.onError?.(error instanceof Error ? error.message : String(error));
    } finally {
      setTriaging(null);
    }
  };

  return (
    <section class="vogt-surface inbox inbox-surface" aria-label="Inbox">
      <header class="place-header">
        <div>
          <p class="place-kicker">Work</p>
          <h1>Inbox</h1>
          <p>One server-ordered attention view. Provenance and coverage stay attached to every answer.</p>
        </div>
        <label class="inbox-filter">
          <span>Source</span>
          <select value={source()} onChange={(event) => applySource(event.currentTarget.value)}>
            <option value="">All sources</option>
            <For each={SOURCES}>{(value) => <option value={value}>{value}</option>}</For>
          </select>
        </label>
      </header>
      <Show when={result()}>{(answer) => <Coverage result={answer()} />}</Show>
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
        <p class="inbox-empty">No normalized entries were returned. Check the coverage cards above: a covered-empty source is different from a source that was not collected.</p>
      </Show>
      <div class="inbox-list" aria-live="polite">
        <For each={result()?.entries ?? []}>
          {(entry) => <Entry entry={entry} seen={seen().has(entry.entry_key)} busy={triaging() === entry.entry_key} onOpen={openEntry} onTriage={triage} />}
        </For>
      </div>
      <Show when={result()?.next_cursor}>
        <button type="button" class="inbox-load-more" disabled={loading()} onClick={() => void load(cursor())}>
          {loading() ? "Loading…" : "Load more"}
        </button>
      </Show>
    </section>
  );
};

export default Inbox;
