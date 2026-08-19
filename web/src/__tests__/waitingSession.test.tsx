// The phone's waiting-session card (Stage 9, FR-M1).
//
// One rule carries the file: the card shows the session's prompt before it
// offers a way to answer it. Everything else here is about what the two acts
// send — the exact bytes a person at that terminal would have typed — and
// about what happens when the prompt cannot be read at all, which is the case
// a one-tap "y" must never survive.

import { fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vitest";
import { api } from "../api";
import WaitingSessionCard from "../WaitingSession";

const SESSION = {
  id: "eng-1",
  name: "alpha-build",
  cwd: "/workspace/vogt",
  activity: "waiting-for-input",
  exit_code: null,
  scrollback_bytes: 2048,
  created_at: "2026-08-18T08:00:00Z",
} as unknown as Parameters<typeof WaitingSessionCard>[0]["session"];

/** What the engine returns for `getSession`, with the prompt in scrollback. */
function detail(text: string): unknown {
  return { ...SESSION, scrollback_base64: btoa(text) };
}

afterEach(() => vi.restoreAllMocks());

describe("a session waiting for input, on a phone", () => {
  it("shows the prompt, then sends exactly the bytes each act names", async () => {
    vi.spyOn(api, "getSession").mockResolvedValue(
      detail("running migration 0011\nApply this migration? (y/n) ") as never,
    );
    const input = vi.spyOn(api, "sessionInput").mockResolvedValue({ ok: true } as never);

    render(() => <WaitingSessionCard session={SESSION} onOpen={() => undefined} />);

    await waitFor(() =>
      expect(screen.getByTestId("waiting-tail")).toHaveTextContent(
        "Apply this migration? (y/n)",
      ),
    );

    fireEvent.click(screen.getByRole("button", { name: "Send y + Enter" }));
    await waitFor(() => expect(input).toHaveBeenCalledTimes(1));
    // `submit: true` is the carriage return: the answer *and* the key that
    // commits it, in one act rather than two.
    expect(input).toHaveBeenNthCalledWith(1, "eng-1", "y", true);

    // The card re-reads after a send, and the acts stay disabled until it
    // has: what the session did with the keystroke is the evidence it took it.
    const interrupt = screen.getByRole("button", { name: "Send Ctrl-C" });
    await waitFor(() => expect(interrupt).toBeEnabled());
    fireEvent.click(interrupt);
    await waitFor(() => expect(input).toHaveBeenCalledTimes(2));
    expect(input).toHaveBeenNthCalledWith(2, "eng-1", "\u0003", false);

    // Neither act is an approval, and the card says which it is.
    expect(screen.getByText(/not Vogt approvals/)).toBeVisible();
  });

  it("offers nothing to press when the prompt cannot be read", async () => {
    vi.spyOn(api, "getSession").mockRejectedValue(new Error("engine is not reachable"));
    const input = vi.spyOn(api, "sessionInput");

    render(() => <WaitingSessionCard session={SESSION} onOpen={() => undefined} />);

    await waitFor(() => expect(screen.getByText(/engine is not reachable/)).toBeVisible());
    expect(screen.queryByRole("button", { name: "Send y + Enter" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Send Ctrl-C" })).toBeNull();
    expect(input).not.toHaveBeenCalled();
  });

  it("refuses a session that has exited, and says so instead", async () => {
    const read = vi.spyOn(api, "getSession");
    render(() => (
      <WaitingSessionCard
        session={{ ...SESSION, exit_code: 0 } as typeof SESSION}
        onOpen={() => undefined}
      />
    ));

    expect(screen.getByText(/has exited/)).toBeVisible();
    expect(screen.queryByRole("button", { name: "Send y + Enter" })).toBeNull();
    // Nothing is even read: there is no scrollback worth asking for.
    expect(read).not.toHaveBeenCalled();
  });
});
