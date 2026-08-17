import { Component, For, Show } from "solid-js";
import { useNavigate } from "@solidjs/router";
import { sessionsStore, sessionsError, isConnected } from "./store";
import type { SessionSummary } from "./api";

function activityLabel(session: SessionSummary): string {
  if (session.exit_code !== null) return session.exit_code === 0 ? "exited" : "errored";
  return session.activity === "waiting-for-input" ? "waiting for input" : session.activity;
}

const Sessions: Component = () => {
  const navigate = useNavigate();
  const sessions = () => sessionsStore.order.map((id) => sessionsStore.sessions[id]).filter((session): session is SessionSummary => Boolean(session));
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
