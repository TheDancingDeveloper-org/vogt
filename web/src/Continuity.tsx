import { Component, For, Show, createResource, createSignal } from "solid-js";
import {
  ApiError,
  api,
  type ContinuationRecipe,
  type SessionContinuity,
  type SessionSummary,
} from "./api";

/**
 * ContextKeeper continuity for one terminal: what it knows, and what it can do
 * about a session that has failed.
 *
 * Two rules this UI exists to honour:
 *
 * - **Render the recipe; do not reinvent it.** ContextKeeper decides which rung
 *   to prefer (reattach, resume, fork, bundle) from provider capabilities and
 *   session state. This shows what it offers, in its order, and creates the PTY
 *   from the recipe's own command, cwd, and env.
 * - **Never launch a bundle without showing it first.** Approval is a human
 *   deciding about one specific bundle, so the preview is the gate, not a
 *   confirmation dialog that says "are you sure".
 */

interface Props {
  session: SessionSummary;
  onClose: () => void;
  /** Create a terminal from a continuation recipe and focus it. */
  onLaunchRecipe: (recipe: ContinuationRecipe) => Promise<void>;
  /** Focus an existing terminal (the `reattach` rung, and post-launch). */
  onFocusTerminal: (mydevenv2SessionId: string) => void;
  onNotify?: (message: string, kind?: "info" | "error") => void;
}

const RUNG_LABELS: Record<string, string> = {
  reattach: "Continue here",
  resume: "Resume",
  fork: "Fork",
  bundle: "Recover with bundle",
};

function rungLabel(recipe: ContinuationRecipe): string {
  return RUNG_LABELS[recipe.kind] ?? recipe.kind;
}

function stateLabel(continuity: SessionContinuity | null | undefined): string {
  if (!continuity) return "Unprotected";
  if (continuity.state === "recovering") return "Recovery available";
  if (continuity.state === "protected") return "Protected";
  return "Unprotected";
}

function freshness(continuity: SessionContinuity): string {
  if (continuity.capture_status === "catching-up") {
    return "capture catching up on the backlog";
  }
  const lag = continuity.capture_lag_seconds;
  if (lag === null || lag === undefined) return "capture freshness unknown";
  if (lag < 60) return `captured ${Math.round(lag)}s ago`;
  return `captured ${Math.round(lag / 60)}m ago`;
}

/** ContextKeeper's own `detail` carries the reason a launch was refused. */
function upstreamDetail(error: unknown): Record<string, unknown> | null {
  if (!(error instanceof ApiError)) return null;
  try {
    const body = JSON.parse(error.body) as { detail?: unknown };
    return body.detail && typeof body.detail === "object"
      ? (body.detail as Record<string, unknown>)
      : null;
  } catch {
    return null;
  }
}

function errorMessage(error: unknown): string {
  const detail = upstreamDetail(error);
  const retryAfter = detail?.retry_after;
  if (typeof retryAfter === "string") {
    // An open circuit is a "not now", not a failure: say when, so the user
    // does not sit clicking a button that cannot work yet.
    return `Recovery launches are paused until ${new Date(retryAfter).toLocaleTimeString()} after repeated failures. The approval still stands.`;
  }
  if (typeof detail?.detail === "string") return detail.detail;
  if (error instanceof ApiError) return `HTTP ${error.status}`;
  return error instanceof Error ? error.message : String(error);
}

const Continuity: Component<Props> = (props) => {
  const continuity = () => props.session.continuity ?? null;
  const registryId = () => continuity()?.session_id ?? "";

  const [plan] = createResource(
    () => registryId() || null,
    (id) => api.continuation(id),
  );
  const [work] = createResource(
    () => continuity()?.work_id ?? null,
    (id) => api.workSession(id),
  );

  const [preview, setPreview] = createSignal<{ bundleId: string; text: string } | null>(null);
  const [manualCommand, setManualCommand] = createSignal<string | null>(null);
  const [busy, setBusy] = createSignal<string | null>(null);
  const [error, setError] = createSignal<string | null>(null);

  const notify = (message: string, kind: "info" | "error" = "info") => {
    props.onNotify?.(message, kind);
  };

  const runRecipe = async (recipe: ContinuationRecipe) => {
    setError(null);
    if (recipe.kind === "reattach") {
      // The PTY is alive: attaching to it is the whole action. Starting
      // anything here would abandon the session it means to continue.
      props.onFocusTerminal(props.session.id);
      props.onClose();
      return;
    }
    if (recipe.requires_approval) {
      await loadPreview();
      return;
    }
    setBusy(recipe.kind);
    try {
      await props.onLaunchRecipe(recipe);
      props.onClose();
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setBusy(null);
    }
  };

  const loadPreview = async () => {
    setBusy("preview");
    setError(null);
    try {
      const result = await api.previewBundle(registryId());
      setPreview({ bundleId: result.bundle_id, text: result.bundle });
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setBusy(null);
    }
  };

  const approveAndLaunch = async () => {
    const current = preview();
    if (!current) return;
    setBusy("launch");
    setError(null);
    // One id per attempt, so a retried click replays the same operation
    // instead of starting a second replacement session.
    const requestId = `pwa-${crypto.randomUUID()}`;
    try {
      await api.approveBundle(registryId(), current.bundleId, `approve-${requestId}`);
      const launched = await api.launchRecovery(
        registryId(),
        current.bundleId,
        `launch-${requestId}`,
      );
      if (launched.status === "manual") {
        // No launcher available. The documented fallback is a command the
        // user runs themselves — showing it beats failing silently.
        setManualCommand(launched.copyable_command ?? null);
        return;
      }
      const child = launched.mydevenv2_session?.id;
      notify("Recovery launched");
      if (child) props.onFocusTerminal(child);
      props.onClose();
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setBusy(null);
    }
  };

  return (
    <div class="continuity-panel">
      <div class="continuity-header">
        <span class={`continuity-badge ${continuity()?.state ?? "unprotected"}`}>
          {stateLabel(continuity())}
        </span>
        <Show when={continuity()}>
          {(state) => (
            <span class="continuity-meta">
              {state().provider} · {state().native_session_id.slice(0, 8)} ·{" "}
              {state().event_count} events · {freshness(state())}
            </span>
          )}
        </Show>
        <button class="row-btn" onClick={() => props.onClose()} title="Close">
          ×
        </button>
      </div>

      <Show
        when={continuity()}
        fallback={
          <p class="continuity-empty">
            No agent session is bound to this terminal, or ContextKeeper is
            unavailable. The terminal is unaffected — it is simply not
            recoverable from a bundle.
          </p>
        }
      >
        <Show when={error()}>
          <p class="continuity-error">{error()}</p>
        </Show>

        <Show when={manualCommand()}>
          {(command) => (
            <div class="continuity-manual">
              <p>No launcher is available. Start the replacement session with:</p>
              <pre>{command()}</pre>
              <button
                onClick={() => {
                  void navigator.clipboard?.writeText(command());
                  notify("Command copied");
                }}
              >
                Copy command
              </button>
            </div>
          )}
        </Show>

        <Show when={preview()}>
          {(current) => (
            <div class="continuity-preview">
              <p class="continuity-preview-title">
                Recovery bundle {current().bundleId} — read it before approving.
                Approval applies to this bundle only.
              </p>
              <pre class="continuity-bundle">{current().text}</pre>
              <div class="continuity-actions">
                <button
                  disabled={busy() !== null}
                  onClick={() => void approveAndLaunch()}
                >
                  {busy() === "launch" ? "Launching…" : "Approve and launch"}
                </button>
                <button disabled={busy() !== null} onClick={() => setPreview(null)}>
                  Cancel
                </button>
              </div>
            </div>
          )}
        </Show>

        <Show when={plan()} fallback={<p class="continuity-empty">Loading continuation…</p>}>
          {(loaded) => (
            <div class="continuity-actions">
              <For each={[loaded().primary, ...loaded().alternatives]}>
                {(recipe, index) => (
                  <button
                    class={index() === 0 ? "primary" : ""}
                    disabled={busy() !== null}
                    title={recipe.reason}
                    onClick={() => void runRecipe(recipe)}
                  >
                    {index() === 0 ? "Continue" : rungLabel(recipe)}
                    <Show when={index() === 0}>
                      <span class="continuity-rung"> ({rungLabel(recipe)})</span>
                    </Show>
                  </button>
                )}
              </For>
            </div>
          )}
        </Show>

        <Show when={work()?.attempts?.length}>
          <div class="continuity-work">
            <span class="continuity-work-title">
              Earlier attempts in this work session
            </span>
            <For each={work()?.attempts ?? []}>
              {(attempt) => (
                <button
                  class="continuity-attempt"
                  disabled={!attempt.mydevenv2_session_id}
                  title={
                    attempt.mydevenv2_session_id
                      ? "Open the terminal that ran this attempt"
                      : "This attempt's terminal is gone; its capture is not"
                  }
                  onClick={() =>
                    attempt.mydevenv2_session_id &&
                    props.onFocusTerminal(attempt.mydevenv2_session_id)
                  }
                >
                  {attempt.lifecycle} · {attempt.native_session_id.slice(0, 8)} ·{" "}
                  {attempt.event_count ?? 0} events
                </button>
              )}
            </For>
          </div>
        </Show>
      </Show>
    </div>
  );
};

export default Continuity;
