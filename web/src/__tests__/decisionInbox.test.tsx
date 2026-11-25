// WI-1339: a decision an agent asked is answered by a person from the Inbox.
// The card shows the question, the context folded away, one button per option
// with the recommended one first and marked, and an optional note. A click
// sends exactly one `decision.answer`; the row leaves; a refusal stays.

import { fireEvent, screen, waitFor, within } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vitest";
import Inbox from "../Inbox";
import { INBOX_ENTRY, INBOX_RESULT, fakeVogt, mountAt, refusal, stopLiveStream } from "./harness";

const DECISION_ENTRY = {
  ...INBOX_ENTRY,
  entry_key: "agent:decision:dec_1",
  source: "agent",
  kind: "decision.pending",
  title: "Decision: Which branch should land first?",
  summary: "agent:engine:eng-worker asks: Which branch should land first? Options: a | b (recommended).",
  work_item_ref: null,
  session_id: "eng-worker",
  proposed_change: null,
  action: { kind: "decision", decision_id: "dec_1", session_id: "eng-worker" },
  evidence_snapshot: {
    decision_id: "dec_1",
    asker: "agent:engine:eng-worker",
    asker_session: "eng-worker",
    question: "Which branch should land first?",
    // The asker listed the recommended option last; the card puts it first.
    options: [
      { label: "a", description: "the small one", recommended: false },
      { label: "b", description: "the big one", recommended: true },
    ],
    recommended: "b",
    context: "Both touch Inbox.tsx.",
    expires_at: "2026-10-22T00:00:00Z",
  },
};

const RESULT = { ...INBOX_RESULT, entries: [DECISION_ENTRY] };
const WHOAMI = { body: { identity_ref: "person:sprooty", kind: "human" } };

describe("decisions in the Inbox", () => {
  afterEach(() => {
    stopLiveStream();
    window.history.replaceState({}, "", "/");
  });

  it("renders the options with the recommended one first and marked", async () => {
    fakeVogt({ "GET /inbox": { body: RESULT }, "GET /auth/whoami": WHOAMI });
    const mounted = mountAt("/inbox", "/inbox", () => <Inbox />);
    await waitFor(() => expect(screen.getByText("Which branch should land first?")).toBeInTheDocument());
    const options = within(screen.getByRole("group", { name: "Options" })).getAllByRole("button");
    expect(options.map((b) => b.textContent)).toEqual(["b (recommended)", "a"]);
    expect(screen.getByText("Context")).toBeInTheDocument();
    expect(screen.getByText("Both touch Inbox.tsx.")).toBeInTheDocument();
    // Not the asker: no Withdraw.
    expect(screen.queryByRole("button", { name: "Withdraw" })).not.toBeInTheDocument();
    mounted.unmount();
  });

  it("posts the label and note, removes the row and names the label", async () => {
    const vogt = fakeVogt({
      "GET /inbox": { body: RESULT },
      "GET /auth/whoami": WHOAMI,
      "POST /decisions/answer": { body: { decision: { id: "dec_1", state: "answered" } } },
    });
    const notice = vi.fn();
    const mounted = mountAt("/inbox", "/inbox", () => <Inbox onNotice={notice} />);
    await waitFor(() => expect(screen.getByText("Which branch should land first?")).toBeInTheDocument());
    // After the answer the core no longer lists the row.
    vogt.route("GET /inbox", { body: { ...INBOX_RESULT, entries: [] } });

    fireEvent.input(screen.getByPlaceholderText("Anything the asker should know"), {
      target: { value: "rebase after" },
    });
    fireEvent.click(screen.getByRole("button", { name: "a" }));
    await waitFor(() => expect(vogt.matching("POST /decisions/answer")).toHaveLength(1));
    expect(vogt.matching("POST /decisions/answer")[0]?.body).toMatchObject({
      id: "dec_1",
      label: "a",
      note: "rebase after",
    });
    await waitFor(() => expect(screen.queryByText("Which branch should land first?")).not.toBeInTheDocument());
    expect(notice).toHaveBeenCalledWith("Answered: a");
    mounted.unmount();
  });

  it("shows the server's person_required message and keeps the row", async () => {
    const refused = "decision.answer: a person must answer a decision";
    fakeVogt({
      "GET /inbox": { body: RESULT },
      "GET /auth/whoami": WHOAMI,
      "POST /decisions/answer": refusal(403, refused),
    });
    const mounted = mountAt("/inbox", "/inbox", () => <Inbox />);
    await waitFor(() => expect(screen.getByText("Which branch should land first?")).toBeInTheDocument());
    fireEvent.click(screen.getByRole("button", { name: /^b\b/ }));
    await waitFor(() => expect(screen.getByRole("alert")).toHaveTextContent(refused));
    expect(screen.getByText("Which branch should land first?")).toBeInTheDocument();
    mounted.unmount();
  });

  it("offers Withdraw to the asker", async () => {
    const vogt = fakeVogt({
      "GET /inbox": { body: RESULT },
      "GET /auth/whoami": { body: { identity_ref: "agent:engine:eng-worker" } },
      "POST /decisions/withdraw": { body: { decision: { id: "dec_1", state: "withdrawn" } } },
    });
    const mounted = mountAt("/inbox", "/inbox", () => <Inbox />);
    await waitFor(() => expect(screen.getByRole("button", { name: "Withdraw" })).toBeInTheDocument());
    fireEvent.click(screen.getByRole("button", { name: "Withdraw" }));
    await waitFor(() => expect(vogt.matching("POST /decisions/withdraw")).toHaveLength(1));
    expect(vogt.matching("POST /decisions/withdraw")[0]?.body).toMatchObject({ id: "dec_1" });
    mounted.unmount();
  });
});
