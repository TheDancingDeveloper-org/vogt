import { fireEvent, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, describe, expect, it } from "vitest";
import Sessions from "../Sessions";
import Assistant from "../Assistant";
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
    const mounted = mountAt("/sessions", "/sessions?approval=act-1", () => (
      <Sessions assistantEnabled />
    ));

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

  it("names pending terminal input and sorts its target first", async () => {
    const input = {
      kind: "send_input" as const,
      id: "act-input",
      session_id: "eng-waiting",
      session_name: "waiting-agent",
      text: "y\r",
      submit: true,
    };
    fakeVogt({}, {
      "GET /api/assistant/history": {
        body: { transcript: [], pending_action: input },
      },
    });
    const mounted = mountAt("/sessions", "/sessions?approval=act-input", () => (
      <Sessions assistantEnabled />
    ));

    await waitFor(() => expect(screen.getByRole("heading", { name: "Input · waiting-agent" })).toBeInTheDocument());
    expect(screen.getByText("y\\r")).toBeInTheDocument();
    mounted.unmount();
  });

  it("renders the workspace child and truthful audit attribution", async () => {
    fakeVogt({}, {
      "GET /api/assistant/history": { body: { transcript: [] } },
    });
    const mounted = mountAt("/sessions", "/sessions", () => (
      <Sessions assistantEnabled={false} hasActiveWorkspace currentTool="history">
        <div data-testid="machine-tool">tool workspace</div>
      </Sessions>
    ));

    await waitFor(() => expect(screen.getByTestId("machine-tool")).toBeVisible());
    expect(screen.getByRole("navigation", { name: "Session tools" })
      .querySelector('a[aria-current="page"]')?.textContent).toBe("History");
    expect(screen.getByText(/Direct session writes are audited to the session actor/))
      .toBeInTheDocument();
    expect(screen.getByText(/Assistant writes require on-screen approval/))
      .toBeInTheDocument();
    mounted.unmount();
  });

  it("renders one shared approval when Assistant is the active Sessions tool", async () => {
    fakeVogt({}, {
      "GET /api/assistant/history": { body: { transcript: [], pending_action: pending } },
      "GET /api/config": { body: { assistant_profiles: [] } },
    });
    const mounted = mountAt("/assistant", "/assistant", () => (
      <Sessions assistantEnabled hasActiveWorkspace currentTool="assistant">
        <Assistant
          assistantEnabled
          publicConfig={{ assistant_enabled: true, assistant_profiles: [] } as never}
          pendingHosted
          onError={() => undefined}
        />
      </Sessions>
    ));

    await waitFor(() => expect(screen.getAllByRole("region", { name: "Pending approval" }))
      .toHaveLength(1));
    expect(screen.getByRole("heading", { name: "work.transition · WI-7 → done" }))
      .toBeInTheDocument();
    mounted.unmount();
  });
});
