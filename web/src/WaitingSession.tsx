// A session that is waiting for input, as a phone sees it (Stage 9, FR-M1).
//
// The rule this file exists to keep is the one the work item's answer control
// already keeps, restated here because a phone is where it is easiest to
// break: **it shows before it asks**. The card reads the session's own
// scrollback and renders the tail; the one-tap answers live inside the `Show`
// that waits for that text. A `y` pressed against a prompt nobody read is not
// unblocking a session, it is answering something unseen.
//
// The second rule is what these controls are *not*. `y + Enter` and `Ctrl-C`
// are keystrokes sent to a terminal. They are not Vogt approvals, they are
// not labelled as approvals, and they do not go through the approval path —
// which is why the card says so in words rather than leaving the distinction
// to whoever reads the code.

import {
  Show,
  createResource,
  createSignal,
  type Component,
} from "solid-js";
import { api, type SessionSummary } from "./api";
import { tailOf, TAIL_FETCH_BYTES } from "./terminalTail";
import { createNow } from "./viewAge";
import { sessionActivityAge } from "./sessionRowModel";

/** The byte a terminal receives when somebody presses Ctrl-C (ETX, 0x03).
 *  Written as an escape: a literal control byte in a source file is a byte
 *  the next person to touch this line cannot see. */
const INTERRUPT = "\u0003";

interface Props {
  session: SessionSummary;
  /** Open the session's own terminal, for anything these two acts cannot do. */
  onOpen: (session: SessionSummary) => void;
  /** The surface's feedback, so a refused write is not only visible here. */
  onFailure?: (message: string) => void;
  /** Disable terminal answers while the session stream is known to be down. */
  disabled?: boolean;
}

export const WaitingSessionCard: Component<Props> = (props) => {
  const [sending, setSending] = createSignal<string | null>(null);
  const now = createNow();

  /** A session that has exited has nothing to take input; say so, and offer
   *  nothing that would fail at the person who pressed it. */
  const live = () => props.session.exit_code === null;

  const [tail, { refetch }] = createResource(
    () => (live() ? props.session.id : null),
    async (id) => {
      try {
        const detail = await api.getSession(id, undefined, TAIL_FETCH_BYTES);
        return { ok: true as const, text: tailOf(detail.scrollback_base64) };
      } catch (error) {
        return {
          ok: false as const,
          message: error instanceof Error ? error.message : String(error),
        };
      }
    },
  );

  /** The tail as text, when the engine answered; null when it did not. */
  const answered = () => {
    const loaded = tail();
    return loaded && loaded.ok ? loaded.text : null;
  };

  /** The engine's own words for why the prompt cannot be shown. */
  const unreadable = () => {
    const loaded = tail();
    return loaded && !loaded.ok ? loaded.message : "";
  };

  const send = async (label: string, text: string, submit: boolean) => {
    if (sending() || props.disabled) return;
    setSending(label);
    try {
      await api.sessionInput(props.session.id, text, submit);
      // Re-read rather than assume: what the session did with the keystroke
      // is the only evidence it took it, and "sent" is not "accepted".
      await refetch();
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      props.onFailure?.(
        `${props.session.name} did not take that input: ${message}`,
      );
    } finally {
      setSending(null);
    }
  };

  return (
    <article class="session-waiting" aria-label={`${props.session.name} is waiting for input`}>
      <header class="session-waiting-head">
        <span class="activity-dot waiting-for-input" aria-hidden="true" />
        <span class="session-waiting-state">Waiting for input</span>
        <span class="session-waiting-age">
          {sessionActivityAge(props.session, now())}
        </span>
      </header>
      <div class="session-waiting-identity">
        <strong>{props.session.name}</strong>
        <span class="session-waiting-context">
          {props.session.cwd || "default workspace"}
        </span>
      </div>

      <Show
        when={live()}
        fallback={
          <p class="session-waiting-absent">
            This session has exited, so nothing here can take input. Open it to
            read what it last said.
          </p>
        }
      >
        <Show
          when={tail()}
          fallback={<p class="session-waiting-absent">reading what it is asking…</p>}
        >
          <Show
            when={answered()}
            fallback={
              <p class="session-waiting-absent">
                {unreadable()} — so there is nothing to answer here. Open the
                session instead.
              </p>
            }
          >
            {(shown) => (
                <>
                  <pre class="session-waiting-tail" data-testid="waiting-tail">
                    {shown() || "the session has produced no output to show"}
                  </pre>
                  <div
                    class="session-waiting-acts"
                    role="group"
                    aria-label={`Terminal input for ${props.session.name}`}
                  >
                    <button
                      type="button"
                      aria-label="Send y + Enter"
                      disabled={sending() !== null || props.disabled}
                      onClick={() => void send("y", "y", true)}
                    >
                      {sending() === "y" ? "sending…" : "Send y ⏎"}
                    </button>
                    <button
                      type="button"
                      aria-label="Send Ctrl-C"
                      disabled={sending() !== null || props.disabled}
                      onClick={() => void send("interrupt", INTERRUPT, false)}
                    >
                      {sending() === "interrupt" ? "sending…" : "Ctrl-C"}
                    </button>
                    <button class="session-waiting-open" type="button" onClick={() => props.onOpen(props.session)}>
                      Open session ›
                    </button>
                  </div>
                  <p class="session-waiting-note">
                    {props.disabled
                      ? "Input needs the live stream — reconnect to answer."
                      : "Sends keystrokes to the terminal — not a Vogt approval."}
                  </p>
                </>
            )}
          </Show>
        </Show>
      </Show>
    </article>
  );
};

export default WaitingSessionCard;
