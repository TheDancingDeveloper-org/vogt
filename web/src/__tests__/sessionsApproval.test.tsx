import { fireEvent, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, describe, expect, it } from "vitest";
import Sessions from "../Sessions";
import { fakeVogt, mountAt } from "./harness";

const pending = {
  kind: "vogt_write" as const,
  id: "act-1",
  operation: "work.transition",
  target: "WI-7 → done",
  reason: "the reviewed fix is complete",
  payload: '{"ref":"WI-7","to_state":"done","reason":"the reviewed fix is complete"}',
};

describe("Sessions approval presentation", () => {
  afterEach(() => {
    window.history.replaceState({}, "", "/");
  });

  it("reviews a replacement reason before sending the held approval", async () => {
    const updated = { ...pending, reason: "the browser review confirmed the fix", payload: '{"ref":"WI-7","to_state":"done","reason":"the browser review confirmed the fix"}' };
    const engine = fakeVogt({}, {
      "GET /api/assistant/history": { body: { transcript: [], pending_action: pending } },
      "PATCH /api/assistant/actions/act-1": (call) => {
        expect(call.body).toEqual({ reason: "the browser review confirmed the fix" });
        return { body: updated };
      },
      "POST /api/assistant/actions/act-1": { body: { reply: null } },
    });
    const mounted = mountAt("/sessions", "/sessions?approval=act-1", () => <Sessions />);

    await waitFor(() => expect(screen.getByRole("heading", { name: "work.transition · WI-7 → done" })).toBeInTheDocument());
    expect(screen.getByText(pending.payload)).toBeInTheDocument();

    const reason = screen.getByLabelText("Pending Vogt write reason");
    fireEvent.input(reason, { target: { value: updated.reason } });
    expect(screen.getByRole("button", { name: "Approve on screen" })).toBeDisabled();
    expect(engine.engineCalls.filter((call) => call.method === "POST")).toHaveLength(0);

    fireEvent.click(screen.getByRole("button", { name: "Update reason for review" }));
    await waitFor(() => expect(engine.engineCalls.filter((call) => call.method === "PATCH")).toHaveLength(1));
    await waitFor(() => expect(screen.getByText(updated.payload)).toBeInTheDocument());

    fireEvent.click(screen.getByRole("button", { name: "Approve on screen" }));
    await waitFor(() => expect(engine.engineCalls.filter((call) => call.method === "POST")).toHaveLength(1));
    expect(engine.engineCalls.find((call) => call.method === "POST")?.body).toEqual({ approve: true });
    mounted.unmount();
  });
});
