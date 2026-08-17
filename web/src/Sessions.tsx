import { Component, For, Show, createSignal, onMount } from "solid-js";
import { useLocation, useNavigate } from "@solidjs/router";
import { api, type AssistantPendingAction } from "./api";
import { sessionsStore, sessionsError, isConnected } from "./store";
import type { SessionSummary } from "./api";

function activityLabel(session: SessionSummary): string {
  if (session.exit_code !== null) return session.exit_code === 0 ? "exited" : "errored";
  return session.activity === "waiting-for-input" ? "waiting for input" : session.activity;
}

const Sessions: Component = () => {
  const navigate = useNavigate();
  const location = useLocation();
  const [pending, setPending] = createSignal<AssistantPendingAction | null>(null);
  const [pendingError, setPendingError] = createSignal<string | null>(null);
  const [pendingBusy, setPendingBusy] = createSignal(false);
  const [reasonDraft, setReasonDraft] = createSignal("");
  const [reasonBusy, setReasonBusy] = createSignal(false);
  const sessions = () => sessionsStore.order.map((id) => sessionsStore.sessions[id]).filter((session): session is SessionSummary => Boolean(session));

  const readPending = async () => {
    try {
      const history = await api.assistantHistory();
      const action = history.pending_action ?? null;
      setPending(action);
      setReasonDraft(action?.kind === "vogt_write" ? action.reason : "");
      setPendingError(null);
    } catch {
      // Assistant routes are absent when the feature is not provisioned.
      setPending(null);
    }
  };

  const replaceReason = async (action: Extract<AssistantPendingAction, { kind: "vogt_write" }>) => {
    const reason = reasonDraft().trim();
    if (!reason || reasonBusy() || pendingBusy()) return;
    setReasonBusy(true);
    setPendingError(null);
    try {
      setPending(await api.assistantReplaceReason(action.id, reason));
      setReasonDraft(reason);
    } catch (error) {
      setPendingError(error instanceof Error ? error.message : String(error));
    } finally {
      setReasonBusy(false);
    }
  };
  onMount(() => void readPending());

  const resolvePending = async (approve: boolean) => {
    const action = pending();
    if (!action || pendingBusy()) return;
    setPendingBusy(true);
    try {
      await api.assistantAction(action.id, approve);
      await readPending();
    } catch (error) {
      setPendingError(error instanceof Error ? error.message : String(error));
    } finally {
      setPendingBusy(false);
    }
  };

  const approvalId = () => new URLSearchParams(location.search).get("approval");
  return (
    <section class="sessions-place" aria-label="Sessions">
      <header class="place-header">
        <div>
          <p class="place-kicker">Machine</p>
          <h1>Sessions</h1>
          <p>Terminal panes and machine tools live here. Select a session to attach.</p>
        </div>
        <span class={`connection-state ${isConnected() ? "connected" : "disconnected"}`}>
          {isConnected() ? "Connected" : "Engine unavailable"}
        </span>
      </header>
      <Show when={pending()} keyed>
        {(current) => {
          const isDeepLink = () => !approvalId() || approvalId() === current.id;
          return (
            <section class={`sessions-pending ${isDeepLink() ? "sessions-pending-current" : "sessions-pending-stale"}`} aria-label="Pending approval">
              <div>
                <p class="place-kicker">Approval required</p>
                <h2>{current.kind === "vogt_write" ? `${current.operation} · ${current.target}` : `Input · ${current.session_name}`}</h2>
                {current.kind === "vogt_write" ? (
                  <label class="sessions-pending-reason">
                    <span>Reason for the audited write</span>
                    <textarea
                      aria-label="Pending Vogt write reason"
                      rows={2}
                      value={reasonDraft()}
                      disabled={reasonBusy() || pendingBusy()}
                      onInput={(event) => setReasonDraft(event.currentTarget.value)}
                    />
                    <button
                      type="button"
                      disabled={reasonBusy() || pendingBusy() || !reasonDraft().trim() || reasonDraft().trim() === current.reason}
                      onClick={() => void replaceReason(current)}
                    >
                      {reasonBusy() ? "Updating reason…" : "Update reason for review"}
                    </button>
                  </label>
                ) : null}
                <details>
                  <summary>Review exact payload</summary>
                  <pre>{current.kind === "vogt_write" ? current.payload : current.text}</pre>
                </details>
              </div>
              <Show when={isDeepLink()} fallback={<p>This approval link is stale or points at another current action.</p>}>
                <div class="sessions-pending-actions">
                  <button type="button" disabled={pendingBusy()} onClick={() => void resolvePending(false)}>Deny</button>
                  <button
                    type="button"
                    disabled={pendingBusy() || reasonBusy() || (current.kind === "vogt_write" && reasonDraft().trim() !== current.reason)}
                    onClick={() => void resolvePending(true)}
                  >
                    Approve on screen
                  </button>
                </div>
              </Show>
            </section>
          );
        }}
      </Show>
      <Show when={pendingError()}>
        {(message) => <p class="sessions-outage" role="alert">Approval could not be completed: {message()}</p>}
      </Show>
      <Show when={sessionsError()}>
        {(message) => <p class="sessions-outage" role="alert">Sessions could not be read: {message()}</p>}
      </Show>
      <Show when={sessions().length > 0} fallback={<p class="sessions-empty">No live sessions are available.</p>}>
        <div class="sessions-place-list">
          <For each={sessions()}>
            {(session) => (
              <button type="button" class="session-place-row" onClick={() => navigate(`/t/${session.id}`)}>
                <span class={`activity-dot ${session.exit_code !== null ? (session.exit_code === 0 ? "done" : "errored") : session.activity}`} aria-hidden="true" />
                <span class="session-place-main">
                  <strong>{session.name}</strong>
                  <small>{session.cwd}</small>
                </span>
                <span class="session-place-status">{activityLabel(session)}</span>
              </button>
            )}
          </For>
        </div>
      </Show>
      <nav class="sessions-tools" aria-label="Session tools">
        <span>Tools</span>
        <button type="button" onClick={() => navigate("/g")}>Git</button>
        <button type="button" onClick={() => navigate("/history")}>History</button>
        <button type="button" onClick={() => navigate("/tasks")}>Tasks</button>
        <button type="button" onClick={() => navigate("/gui")}>GUI stream</button>
      </nav>
    </section>
  );
};

export default Sessions;
