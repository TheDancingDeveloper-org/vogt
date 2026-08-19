import {
  Component,
  For,
  Show,
  createEffect,
  createMemo,
  createSignal,
  onMount,
  type JSX,
} from "solid-js";
import { useLocation, useNavigate } from "@solidjs/router";
import { api, type AssistantPendingAction } from "./api";
import { sessionsStore, sessionsError, isConnected } from "./store";
import type { SessionSummary } from "./api";
import type { SessionTool } from "./routeModel";
import { pendingAction, setPendingAction } from "./pendingAction";
import SurfaceHeader from "./SurfaceHeader";
import WaitingSessionCard from "./WaitingSession";
import { createNarrow } from "./narrow";

interface Props {
  currentTool?: SessionTool | null;
  guiEnabled?: boolean;
  assistantEnabled?: boolean;
  children?: JSX.Element;
  hasActiveWorkspace?: boolean;
  onCreateSession?: () => void;
}

export const SessionTools: Component<Props> = (props) => (
  <nav class="sessions-tools" aria-label="Session tools">
    <span>Tools</span>
    <a href="#/sessions" aria-current={!props.currentTool ? "page" : undefined}>Overview</a>
    <a href="#/g" aria-current={props.currentTool === "git" ? "page" : undefined}>Git</a>
    <a href="#/history" aria-current={props.currentTool === "history" ? "page" : undefined}>History</a>
    <a href="#/tasks" aria-current={props.currentTool === "tasks" ? "page" : undefined}>Tasks</a>
    <Show when={props.guiEnabled}>
      <a href="#/gui" aria-current={props.currentTool === "gui" ? "page" : undefined}>GUI stream</a>
    </Show>
    <Show when={props.assistantEnabled}>
      <a href="#/assistant" aria-current={props.currentTool === "assistant" ? "page" : undefined}>Assistant</a>
    </Show>
  </nav>
);

function activityLabel(session: SessionSummary): string {
  if (session.exit_code !== null) return session.exit_code === 0 ? "exited" : "errored";
  return session.activity === "waiting-for-input" ? "waiting for input" : session.activity;
}

const ATTENTION_ORDER: Record<string, number> = {
  "waiting-for-input": 0,
  errored: 1,
  running: 2,
  idle: 3,
  exited: 4,
};

function attentionRank(session: SessionSummary): number {
  if (session.exit_code !== null) return session.exit_code === 0 ? 4 : 1;
  return ATTENTION_ORDER[session.activity] ?? 3;
}

function activitySince(session: SessionSummary): string {
  const instant = session.activity_changed_at || session.created_at;
  if (!instant) return "activity time unavailable";
  const elapsed = Date.now() - new Date(instant).getTime();
  if (!Number.isFinite(elapsed) || elapsed < 0) return instant;
  const minutes = Math.floor(elapsed / 60_000);
  if (minutes < 1) return "just now";
  if (minutes < 60) return `${minutes}m ago`;
  const hours = Math.floor(minutes / 60);
  return hours < 24 ? `${hours}h ago` : `${Math.floor(hours / 24)}d ago`;
}

function visibleTerminalInput(text: string): string {
  return text.replace(/[\x00-\x1f\x7f]/g, (char) => {
    if (char === "\n") return "\\n";
    if (char === "\r") return "\\r";
    if (char === "\t") return "\\t";
    return `\\x${char.charCodeAt(0).toString(16).padStart(2, "0")}`;
  });
}

const Sessions: Component<Props> = (props) => {
  const navigate = useNavigate();
  const location = useLocation();
  const [pendingError, setPendingError] = createSignal<string | null>(null);
  const [pendingBusy, setPendingBusy] = createSignal(false);
  const [reasonDraft, setReasonDraft] = createSignal("");
  const [reasonBusy, setReasonBusy] = createSignal(false);
  const sessions = createMemo(() => sessionsStore.order
    .map((id) => sessionsStore.sessions[id])
    .filter((session): session is SessionSummary => Boolean(session))
    .sort((left, right) => {
      const currentPending = pendingAction();
      const pendingSession = currentPending?.kind === "send_input"
        ? currentPending.session_id
        : null;
      const pendingDelta = Number(right.id === pendingSession)
        - Number(left.id === pendingSession);
      if (pendingDelta !== 0) return pendingDelta;
      const attentionDelta = attentionRank(left) - attentionRank(right);
      if (attentionDelta !== 0) return attentionDelta;
      return Date.parse(right.activity_changed_at || right.created_at)
        - Date.parse(left.activity_changed_at || left.created_at);
    }));

  const readPending = async () => {
    try {
      const history = await api.assistantHistory();
      const action = history.pending_action ?? null;
      setPendingAction(action);
      setPendingError(null);
    } catch {
      // Assistant routes are absent when the feature is not provisioned.
      setPendingAction(null);
    }
  };

  const replaceReason = async (action: Extract<AssistantPendingAction, { kind: "vogt_write" }>) => {
    const reason = reasonDraft().trim();
    if (!reason || reasonBusy() || pendingBusy()) return;
    setReasonBusy(true);
    setPendingError(null);
    try {
      setPendingAction(await api.assistantReplaceReason(action.id, reason));
      setReasonDraft(reason);
    } catch (error) {
      setPendingError(error instanceof Error ? error.message : String(error));
    } finally {
      setReasonBusy(false);
    }
  };
  onMount(() => void readPending());

  createEffect(() => {
    const action = pendingAction();
    setReasonDraft(action?.kind === "vogt_write" ? action.reason : "");
  });

  const resolvePending = async (approve: boolean) => {
    const action = pendingAction();
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

  // Stage 9: on a narrow client a waiting session is an attention card rather
  // than a row with a label on it. The rows keep every session, including
  // these; the cards are what puts the prompt and its two answers where a
  // thumb is, above the list rather than inside it.
  const narrow = createNarrow();
  // Including the ones that exited while waiting: somebody came to this
  // screen to answer that prompt, and a card that says the session is gone is
  // the answer to why they cannot (Stage 9's "refuse safely and explain").
  const waiting = createMemo(() =>
    sessions().filter((session) => session.activity === "waiting-for-input"),
  );
  return (
    <section
      class={`sessions-place ${props.hasActiveWorkspace ? "has-workspace" : ""}`}
      aria-label="Sessions"
    >
      <SurfaceHeader
        class="sessions-header"
        label="Sessions header"
        title={(
          <>
          <p class="place-kicker">Machine</p>
          <h1>Sessions</h1>
          </>
        )}
        honesty={(
          <div class="sessions-header-honesty" aria-live="polite">
            <p>
              {!sessionsStore.ready && !sessionsError()
                ? "Loading sessions — no answer yet"
                : sessionsError()
                  ? `Sessions unavailable — ${sessionsError()}`
                  : isConnected()
                    ? `${sessions().length} live · sorted by attention`
                    : `${sessions().length} from the last answer · stream disconnected; may be stale`}
            </p>
            <span class={`connection-state ${isConnected() ? "connected" : "disconnected"}`}>
              {isConnected() ? "Connected" : "Disconnected"}
            </span>
          </div>
        )}
        controls={(
          <SessionTools
            currentTool={props.currentTool}
            guiEnabled={props.guiEnabled}
            assistantEnabled={props.assistantEnabled}
          />
        )}
        action={props.onCreateSession ? (
          <button type="button" onClick={() => props.onCreateSession?.()}>
            + Session
          </button>
        ) : undefined}
      />
      <Show when={narrow() && waiting().length > 0}>
        <section class="sessions-waiting" aria-label="Sessions waiting for input">
          <For each={waiting()}>
            {(session) => (
              <WaitingSessionCard
                session={session}
                onOpen={(chosen) => navigate(`/t/${chosen.id}`)}
                onFailure={(message) => setPendingError(message)}
              />
            )}
          </For>
        </section>
      </Show>

      <div class="sessions-place-body">
        <aside class="sessions-place-sidebar" aria-label="Live sessions">
          <Show when={sessionsError()}>
            {(message) => <p class="sessions-outage" role="alert">Sessions could not be read: {message()}</p>}
          </Show>
          <Show when={sessions().length > 0} fallback={<p class="sessions-empty">No live sessions are available.</p>}>
            <div class="sessions-place-list">
              <For each={sessions()}>
                {(session) => (
                  <article class={`session-place-row ${session.activity === "waiting-for-input" ? "session-place-row--waiting" : ""}`}>
                    <span class={`activity-dot ${session.exit_code !== null ? (session.exit_code === 0 ? "done" : "errored") : session.activity}`} aria-hidden="true" />
                    <span class="session-place-main">
                      <strong>{session.name}</strong>
                      <span class="session-place-context">
                        <small>{session.cwd || "default workspace"}</small>
                        <small>{session.continuity ? `${session.continuity.provider} · ${session.continuity.state}` : "continuity unavailable"}</small>
                      </span>
                    </span>
                    <span class="session-place-status">
                      <strong>{activityLabel(session)}</strong>
                      <small>{activitySince(session)}</small>
                    </span>
                    <button type="button" class="session-place-open" onClick={() => navigate(`/t/${session.id}`)}>Open session</button>
                  </article>
                )}
              </For>
            </div>
          </Show>
        </aside>
        <div class="sessions-active-workspace">
          <Show when={pendingAction()} keyed>
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
                      <pre>{current.kind === "vogt_write" ? current.payload : visibleTerminalInput(current.text)}</pre>
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
          <Show when={!props.hasActiveWorkspace}>
            <div class="sessions-workspace-empty">
              <h2>Choose a session or tool</h2>
              <p>Terminals, files and machine tools stay inside this Sessions workspace.</p>
            </div>
          </Show>
          <div
            class="sessions-workspace-content"
            style={{ display: props.hasActiveWorkspace ? "flex" : "none" }}
          >
            {props.children}
          </div>
        </div>
      </div>
      <footer class="sessions-audit-note">
        Direct session writes are audited to the session actor. Assistant writes require on-screen approval and are audited to the approver.
      </footer>
    </section>
  );
};

export default Sessions;
