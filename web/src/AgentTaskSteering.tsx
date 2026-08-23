import { For, Show, createSignal } from "solid-js";
import type { AgentTaskGate, AgentTaskRun } from "./api";

interface Props {
  run: AgentTaskRun;
  /** Deliver a steer to the run's PTY at its next prompt boundary (#289). */
  onSteer: (text: string, interrupt: boolean) => void | Promise<void>;
  /** Answer an open gate by the index of the chosen option (#289). */
  onAnswerGate: (
    gateId: string,
    optionIndex: number,
  ) => void | Promise<void>;
  /** Disable the controls while a request is in flight. */
  busy?: boolean;
}

/**
 * The steer bar and approval-gate controls for one agent-task run (#289).
 *
 * Kept a pure presentational component — it takes a run and two callbacks and
 * renders no data-fetching of its own — so its two load-bearing behaviours are
 * testable in isolation: an *open* gate shows its question and one button per
 * option, and a *running* run shows a steer bar that hands its text (and the
 * interrupt flag) back to the parent. A resolved gate is shown as a one-line
 * audit note, because "how was this answered, and by whom" is the point of
 * modelling gates as first-class steps rather than transient prompts.
 */
export default function AgentTaskSteering(props: Props) {
  const [steerText, setSteerText] = createSignal("");
  const [interrupt, setInterrupt] = createSignal(false);

  const gates = (): AgentTaskGate[] => props.run.gates ?? [];
  const openGates = () => gates().filter((gate) => gate.state === "open");
  const resolvedGates = () =>
    gates().filter((gate) => gate.state !== "open");
  const isRunning = () => props.run.status === "running";

  const submitSteer = () => {
    const text = steerText().trim();
    // An interrupt with no text is a valid action — a bare cancel — so the
    // send is allowed when either the text is non-empty or interrupt is set.
    if (!text && !interrupt()) return;
    void props.onSteer(text, interrupt());
    setSteerText("");
    setInterrupt(false);
  };

  return (
    <div class="agent-task-steering" data-testid="agent-task-steering">
      <Show when={openGates().length > 0}>
        <div class="agent-task-gates" data-testid="agent-task-open-gates">
          <For each={openGates()}>
            {(gate) => (
              <div class="agent-task-gate" data-testid="agent-task-gate">
                <div class="agent-task-gate-question">{gate.question}</div>
                <div class="agent-task-gate-options">
                  <For each={gate.options}>
                    {(option, index) => (
                      <button
                        type="button"
                        class="agent-task-gate-option"
                        classList={{ approve: option.approve === true }}
                        disabled={props.busy === true}
                        onClick={() =>
                          void props.onAnswerGate(gate.id, index())
                        }
                      >
                        {option.label}
                      </button>
                    )}
                  </For>
                </div>
              </div>
            )}
          </For>
        </div>
      </Show>

      <Show when={resolvedGates().length > 0}>
        <ul class="agent-task-gate-history">
          <For each={resolvedGates()}>
            {(gate) => (
              <li
                class="agent-task-gate-resolved"
                classList={{ blocked: gate.state === "blocked" }}
              >
                <span class="agent-task-gate-resolved-question">
                  {gate.question}
                </span>
                <span class="agent-task-gate-resolved-outcome">
                  {gate.state === "answered"
                    ? `${gate.approved ? "Approved" : "Answered"} (${gate.option_label ?? "?"})` +
                      (gate.actor ? ` by ${gate.actor}` : "")
                    : `Blocked${gate.reason ? `: ${gate.reason}` : ""}`}
                </span>
              </li>
            )}
          </For>
        </ul>
      </Show>

      <Show when={isRunning()}>
        <form
          class="agent-task-steer-bar"
          data-testid="agent-task-steer-bar"
          onSubmit={(event) => {
            event.preventDefault();
            submitSteer();
          }}
        >
          <input
            type="text"
            class="agent-task-steer-input"
            placeholder="Steer this run…"
            aria-label="Steer this run"
            value={steerText()}
            disabled={props.busy === true}
            onInput={(event) => setSteerText(event.currentTarget.value)}
          />
          <label class="agent-task-steer-interrupt">
            <input
              type="checkbox"
              checked={interrupt()}
              disabled={props.busy === true}
              onChange={(event) => setInterrupt(event.currentTarget.checked)}
            />
            Interrupt first
          </label>
          <button
            type="submit"
            class="agent-task-steer-send"
            disabled={props.busy === true}
          >
            Steer
          </button>
        </form>
      </Show>
    </div>
  );
}
