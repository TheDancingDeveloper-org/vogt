// WI-973: a grant a session asked for is decided by a person from the Inbox.
// The entry says what is asked for; Approve and Deny each take a reason and
// send exactly one `session.grant_decide`; a refusal stays on the entry.

import { fireEvent, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, describe, expect, it } from "vitest";
import Inbox from "../Inbox";
import { INBOX_ENTRY, INBOX_RESULT, fakeVogt, mountAt, refusal, stopLiveStream } from "./harness";

const GRANT_ENTRY = {
  ...INBOX_ENTRY,
  entry_key: "agent:grant:grt_1",
  source: "agent",
  kind: "session.grant_request",
  title: "Grant request: 100.109.218.11_SSH for session eng-work",
  summary:
    "agent:engine:eng-overseer asks for 100.109.218.11_SSH (project infra) as GRANT_100_109_218_11_SSH, one fetch, for 60 min once approved. Reason: emulator key",
  work_item_ref: null,
  session_id: "eng-worker",
  evidence_snapshot: null,
  proposed_change: null,
  action: { kind: "grant", grant_id: "grt_1", session_id: "eng-worker" },
};

const RESULT = { ...INBOX_RESULT, entries: [GRANT_ENTRY] };

describe("grant requests in the Inbox", () => {
  afterEach(() => {
    stopLiveStream();
    window.history.replaceState({}, "", "/");
  });

  it("approves a grant with a reason, through session.grant_decide", async () => {
    const vogt = fakeVogt({
      "GET /inbox": { body: RESULT },
      "POST /sessions/grants/decide": { body: { grant: { id: "grt_1", state: "approved" } } },
    });
    const mounted = mountAt("/inbox", "/inbox", () => <Inbox />);
    await waitFor(() => expect(screen.getByText(GRANT_ENTRY.title)).toBeInTheDocument());
    expect(screen.getByText(/as GRANT_100_109_218_11_SSH/)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Accept proposed change…" })).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Approve grant…" }));
    expect(vogt.matching("POST /sessions/grants/decide")).toHaveLength(0);
    fireEvent.input(screen.getByPlaceholderText("Why this triage decision?"), {
      target: { value: "the worker needs the emulator for WI-970" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Confirm approve grant" }));
    await waitFor(() => expect(vogt.matching("POST /sessions/grants/decide")).toHaveLength(1));
    expect(vogt.matching("POST /sessions/grants/decide")[0]?.body).toEqual({
      id: "grt_1",
      decision: "approve",
      reason: "the worker needs the emulator for WI-970",
    });
    mounted.unmount();
  });

  it("denies a grant, and keeps a refusal on the entry", async () => {
    const refused = "session.grant_decide: grant grt_1 is approved, not pending";
    const vogt = fakeVogt({
      "GET /inbox": { body: RESULT },
      "POST /sessions/grants/decide": refusal(409, refused),
    });
    const mounted = mountAt("/inbox", "/inbox", () => <Inbox />);
    await waitFor(() => expect(screen.getByText(GRANT_ENTRY.title)).toBeInTheDocument());

    fireEvent.click(screen.getByRole("button", { name: "Deny grant…" }));
    fireEvent.input(screen.getByPlaceholderText("Why this triage decision?"), {
      target: { value: "not for this task" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Confirm deny grant" }));
    await waitFor(() => expect(screen.getByRole("alert")).toHaveTextContent(refused));
    expect(vogt.matching("POST /sessions/grants/decide")[0]?.body).toMatchObject({
      decision: "deny",
    });
    mounted.unmount();
  });
});
