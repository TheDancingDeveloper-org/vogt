import {
  Component,
  For,
  Show,
  createEffect,
  createMemo,
  createSignal,
  onCleanup,
  onMount,
  type JSX,
} from "solid-js";
import { useLocation, useNavigate } from "@solidjs/router";
import { api, type AssistantPendingAction, type SessionTemplate } from "./api";
import { sessionsStore, sessionsError, isConnected } from "./store";
import type { SessionSummary } from "./api";
import type { SessionTool } from "./routeModel";
import { pendingAction, setPendingAction } from "./pendingAction";
import { isDemoMode } from "./runtimeTransport";
import {
  deferAssistantHydration,
  invalidateAssistantSnapshot,
  readAssistantSnapshot,
} from "./assistantCache";
import SurfaceHeader from "./SurfaceHeader";
import WaitingSessionCard from "./WaitingSession";
import SessionList from "./SessionList";
import { sortSessionsByAttention } from "./sessionRowModel";
import { createNarrow } from "./narrow";

interface Props {
  currentTool?: SessionTool | null;
  guiEnabled?: boolean;
  assistantEnabled?: boolean;
  children?: JSX.Element;
  hasActiveWorkspace?: boolean;
  /** Create a session. `promptForName` (Shift held) asks for a name first. */
  onCreateSession?: (promptForName?: boolean) => void;
  /** Presets the overview offers when there are no sessions to list (#233). */
  sessionTemplates?: SessionTemplate[];
  /** Launch one of those presets straight into its terminal. */
  onLaunchTemplate?: (template: SessionTemplate) => void;
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
  const sessions = createMemo(() => {
    // Attention order is the shared spine (the rail orders the same way); a
    // stable partition then floats the session awaiting this reader's input to
    // the very top without disturbing the attention order beneath it.
    const currentPending = pendingAction();
    const pendingSession = currentPending?.kind === "send_input"
      ? currentPending.session_id
      : null;
    return sortSessionsByAttention(
      sessionsStore.order
        .map((id) => sessionsStore.sessions[id])
        .filter((session): session is SessionSummary => Boolean(session)),
    ).sort((left, right) =>
      Number(right.id === pendingSession) - Number(left.id === pendingSession),
    );
  });

  const readPending = async () => {
    try {
      const snapshot = await readAssistantSnapshot(props.assistantEnabled === true);
      if (!snapshot) return;
      setPendingAction(snapshot.pendingAction);
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
      invalidateAssistantSnapshot();
      setReasonDraft(reason);
    } catch (error) {
      setPendingError(error instanceof Error ? error.message : String(error));
    } finally {
      setReasonBusy(false);
    }
  };
  onMount(() => {
    // Sessions is mounted around the terminal workspace. Only an explicit
    // approval deep-link needs the pending card; ordinary shell and terminal
    // mounts do not wake the assistant at all.
    if (props.assistantEnabled !== true) return;
    const explicitApproval = new URLSearchParams(location.search).has("approval");
    if (!explicitApproval && !isDemoMode()) return;
    const cancel = deferAssistantHydration(() => void readPending());
    onCleanup(cancel);
  });

  createEffect(() => {
    const action = pendingAction();
    setReasonDraft(action?.kind === "vogt_write" ? action.reason : "");
  });

  const resolvePending = async (approve: boolean) => {
    const action = pendingAction();
    if (!action || pendingBusy()) return;
    setPendingBusy(true);
    try {
      const reply = await api.assistantAction(action.id, approve);
      setPendingAction(reply.pending_action ?? null);
      invalidateAssistantSnapshot();
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
  // On a narrow shell, once a terminal or a machine tool owns the screen the
  // Sessions surface header is chrome over it: the two-line honesty folds and
  // the whole header shrinks to a title and "+ Session" so the terminal is not
  // pushed off-screen (#232). On the overview, and on any desk, the header is
  // itself and stays open.
  const collapsed = () => narrow() && Boolean(props.hasActiveWorkspace);
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
        class={`sessions-header${collapsed() ? " sessions-header--compact" : ""}`}
        collapseHonesty={collapsed()}
        label="Sessions header"
        title={(
          <>
          <p class="place-kicker">Machine</p>
          <h1>Sessions</h1>
          </>
        )}
        honestyClass={
          !sessionsStore.ready && !sessionsError()
            ? "surface-header-honesty--never"
            : sessionsError()
              ? "surface-header-honesty--outage"
              : isConnected()
                ? "surface-header-honesty--fresh"
                : "surface-header-honesty--stale"
        }
        honesty={(
          <div class="sessions-header-honesty" aria-live="polite">
            <p>
              {!sessionsStore.ready && !sessionsError()
                ? "Loading sessions — no answer yet"
                : sessionsError()
                  ? `Sessions unavailable — ${sessionsError()}`
                  : isConnected()
                    ? <><strong>{sessions().length} live</strong> · sorted by attention</>
                    : <><strong>{sessions().length} from the last answer</strong> · stream disconnected; may be stale</>}
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
          <button
            type="button"
            onClick={(event) => props.onCreateSession?.(event.shiftKey)}
            title="New session (hold Shift to name it)"
          >
            + Session
          </button>
        ) : undefined}
      />
      <Show when={narrow() && !props.hasActiveWorkspace && waiting().length > 0}>
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

      {/* The Live Sessions sub-panel was removed (#167): the places rail
          already lists running sessions, and its collapse/resize machinery
          was upkeep for a redundant secondary view. The body is now just the
          active workspace. */}
      <div class="sessions-place-body">
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
            <div class="sessions-overview" aria-label="Sessions overview">
              <Show
                when={sessions().length > 0}
                fallback={(
                  <div class="sessions-workspace-empty">
                    <h2>No sessions yet</h2>
                    <p>
                      Start a terminal session to run work on this machine, or
                      launch one of the presets below.
                    </p>
                    <Show when={props.onCreateSession}>
                      <button
                        type="button"
                        class="sessions-start"
                        onClick={(event) => props.onCreateSession?.(event.shiftKey)}
                      >
                        Start a session
                      </button>
                    </Show>
                    <Show when={(props.sessionTemplates?.length ?? 0) > 0}>
                      <ul class="sessions-template-list" aria-label="Session presets">
                        <For each={props.sessionTemplates}>
                          {(template) => (
                            <li>
                              <button
                                type="button"
                                onClick={() => props.onLaunchTemplate?.(template)}
                              >
                                <span class="sessions-template-name">{template.name}</span>
                                <Show when={template.description}>
                                  <span class="sessions-template-desc">{template.description}</span>
                                </Show>
                              </button>
                            </li>
                          )}
                        </For>
                      </ul>
                    </Show>
                  </div>
                )}
              >
                <div class="sessions-overview-list">
                  <h2 class="sessions-overview-heading">Running sessions</h2>
                  {/* On a narrow shell the waiting sessions are already the
                      attention cards above, so the list carries the rest;
                      on a desk there are no cards and it carries them all. */}
                  <SessionList
                    sessions={sessions()}
                    omit={narrow() ? waiting().map((session) => session.id) : []}
                    label="Running sessions"
                  />
                </div>
              </Show>
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
