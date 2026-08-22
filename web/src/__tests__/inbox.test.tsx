import { fireEvent, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, describe, expect, it } from "vitest";
import Inbox from "../Inbox";
import {
  INBOX_ENTRY,
  INBOX_RESULT,
  fakeVogt,
  held,
  liveStream,
  mountAt,
  refusal,
  settle,
  stopLiveStream,
  unavailable,
  type Handler,
} from "./harness";

/** An `/inbox` handler that pages: the first read carries a cursor, the read
 *  that returns with it gets the second page and no cursor. Records its own
 *  call count so a test can prove a live re-read re-read every loaded page. */
function pagedInbox(page2: Record<string, unknown>): { handler: Handler; calls: () => number } {
  let count = 0;
  const handler: Handler = (call) => {
    count += 1;
    if (call.query.get("cursor") === "page-2") {
      return { body: { ...INBOX_RESULT, entries: [page2], next_cursor: null } };
    }
    return { body: { ...INBOX_RESULT, entries: [INBOX_ENTRY], next_cursor: "page-2" } };
  };
  return { handler, calls: () => count };
}

const SECOND_PAGE = {
  ...INBOX_ENTRY,
  entry_key: "obs:second-page",
  title: "Second page entry",
  action: { kind: "observation" },
  work_item_ref: null,
  session_id: null,
  evidence_snapshot: null,
  proposed_change: null,
};

describe("canonical Inbox interactions", () => {
  afterEach(() => {
    stopLiveStream();
    window.history.replaceState({}, "", "/");
  });

  it("restores a source filter from the URL and shows evidence before triage", async () => {
    const vogt = fakeVogt({ "GET /inbox": { body: INBOX_RESULT } });
    const mounted = mountAt("/inbox", "/inbox?source=drift", () => <Inbox />);
    await waitFor(() => expect(screen.getByText(INBOX_ENTRY.title)).toBeInTheDocument());

    expect(screen.getByLabelText("Source")).toHaveValue("drift");
    expect(screen.getByRole("region", { name: "Drift evidence" })).toHaveTextContent("observed_state");
    expect(vogt.matching("GET /inbox")[0]?.query.get("sources")).toBe("drift");
    mounted.unmount();
  });

  it("uses the same typed reason boundary for keyboard focus and batch archive", async () => {
    const vogt = fakeVogt({
      "GET /inbox": { body: INBOX_RESULT },
      "POST /inbox/archive": { body: { entry: { ...INBOX_ENTRY, triage_state: "archived" } } },
    });
    const mounted = mountAt("/inbox", "/inbox", () => <Inbox />);
    await waitFor(() => expect(screen.getByText(INBOX_ENTRY.title)).toBeInTheDocument());

    expect(screen.queryByPlaceholderText("Why this triage decision?")).not.toBeInTheDocument();
    fireEvent.keyDown(window, { key: "e" });
    await waitFor(() => expect(document.activeElement).toBe(
      screen.getByPlaceholderText("Why this triage decision?"),
    ));

    fireEvent.click(screen.getByLabelText(`Select ${INBOX_ENTRY.title}`));
    fireEvent.click(screen.getByRole("button", { name: "Archive selected…" }));
    expect(vogt.matching("POST /inbox/archive")).toHaveLength(0);
    fireEvent.input(screen.getByLabelText("Batch reason"), { target: { value: "reviewed with the team" } });
    fireEvent.click(
      screen.getByLabelText("Batch reason")
        .closest(".inbox-batch-composer")!
        .querySelector<HTMLButtonElement>('button[type="submit"]')!,
    );
    await waitFor(() => expect(vogt.matching("POST /inbox/archive")).toHaveLength(1));
    expect(vogt.matching("POST /inbox/archive")[0]?.body).toMatchObject({ reason: "reviewed with the team" });
    mounted.unmount();
  });

  it("keeps drift evidence ahead of the singular resolve action", async () => {
    const vogt = fakeVogt({
      "GET /inbox": { body: INBOX_RESULT },
      "POST /drift/resolve": { body: { proposal: {}, change_applied: false } },
    });
    const mounted = mountAt("/inbox", "/inbox", () => <Inbox />);
    await waitFor(() => expect(screen.getByText(INBOX_ENTRY.title)).toBeInTheDocument());

    expect(screen.queryByPlaceholderText("Why this triage decision?")).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Reject proposed change…" }));
    expect(vogt.matching("POST /drift/resolve")).toHaveLength(0);
    fireEvent.input(screen.getByPlaceholderText("Why this triage decision?"), { target: { value: "the declared state is intentional" } });
    fireEvent.click(screen.getByRole("button", { name: "Confirm reject" }));
    await waitFor(() => expect(vogt.matching("POST /drift/resolve")).toHaveLength(1));
    expect(vogt.matching("POST /drift/resolve")[0]?.body).toEqual({
      id: "proposal-1",
      resolution: "rejected",
      reason: "the declared state is intentional",
    });
    mounted.unmount();
  });

  it("keeps a refused reason composer in place for correction", async () => {
    const refused = "drift.resolve: the proposal changed while it was open";
    fakeVogt({
      "GET /inbox": { body: INBOX_RESULT },
      "POST /drift/resolve": refusal(409, refused),
    });
    const mounted = mountAt("/inbox", "/inbox", () => <Inbox />);
    await waitFor(() => expect(screen.getByText(INBOX_ENTRY.title)).toBeInTheDocument());

    fireEvent.click(screen.getByRole("button", { name: "Reject proposed change…" }));
    const reason = screen.getByPlaceholderText("Why this triage decision?");
    fireEvent.input(reason, { target: { value: "the declared state is intentional" } });
    fireEvent.click(screen.getByRole("button", { name: "Confirm reject" }));

    await waitFor(() => expect(screen.getByRole("alert")).toHaveTextContent(refused));
    expect(reason).toHaveValue("the declared state is intentional");
    expect(screen.getByRole("button", { name: "Confirm reject" })).toBeInTheDocument();
    mounted.unmount();
  });

  it("keeps loading, unavailable, and covered-empty answers distinct", async () => {
    const pending = held();
    const vogt = fakeVogt({ "GET /inbox": pending.handler });
    const mounted = mountAt("/inbox", "/inbox", () => <Inbox />);

    await pending.asked;
    expect(screen.getByText("Loading Inbox — no answer yet.")).toBeInTheDocument();
    expect(screen.queryByText(/No normalized entries/)).not.toBeInTheDocument();
    pending.answer(unavailable("core did not answer"));
    await waitFor(() => expect(screen.getByRole("alert")).toHaveTextContent("core did not answer"));
    expect(screen.queryByText(/No normalized entries/)).not.toBeInTheDocument();

    vogt.route("GET /inbox", {
      body: {
        ...INBOX_RESULT,
        entries: [],
        coverage: {
          ...INBOX_RESULT.coverage,
          drift: { status: "current", count: 0, detail: "covered" },
          github: { status: "failed", count: 0, detail: "rate limited" },
        },
      },
    });
    fireEvent.click(screen.getByRole("button", { name: "Retry Inbox" }));
    await waitFor(() => expect(screen.getByText(/No normalized entries/)).toBeInTheDocument());
    expect(screen.getByText("Nothing needs attention in the covered source.")).toBeInTheDocument();
    expect(screen.getByText("Collection failed: rate limited")).toBeInTheDocument();
    mounted.unmount();
  });

  it("orders the answer first, keeps coverage after it and closed, and has no batch bar until a selection", async () => {
    fakeVogt({ "GET /inbox": { body: INBOX_RESULT } });
    const mounted = mountAt("/inbox", "/inbox", () => <Inbox />);
    await waitFor(() => expect(screen.getByText(INBOX_ENTRY.title)).toBeInTheDocument());

    const stream = screen.getByLabelText("Attention stream");
    const coverage = screen.getByText("Coverage and provenance", { selector: "summary" }).parentElement!;
    // The stream is the first answer; coverage stays after it and closed.
    expect(stream.compareDocumentPosition(coverage) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(coverage).not.toHaveAttribute("open");
    // With nothing selected and nothing read, the batch bar earns no space.
    expect(screen.queryByLabelText("Batch Inbox actions")).not.toBeInTheDocument();
    mounted.unmount();
  });

  it("raises the batch bar above the list the moment an entry is selected", async () => {
    fakeVogt({ "GET /inbox": { body: INBOX_RESULT } });
    const mounted = mountAt("/inbox", "/inbox", () => <Inbox />);
    await waitFor(() => expect(screen.getByText(INBOX_ENTRY.title)).toBeInTheDocument());

    expect(screen.queryByLabelText("Batch Inbox actions")).not.toBeInTheDocument();
    fireEvent.click(screen.getByLabelText(`Select ${INBOX_ENTRY.title}`));

    const batch = screen.getByLabelText("Batch Inbox actions");
    const stream = screen.getByLabelText("Attention stream");
    // Sticky at the top means: before the list in document order, not buried
    // beneath it.
    expect(batch.compareDocumentPosition(stream) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(batch).toHaveTextContent("1 selected");
    mounted.unmount();
  });

  // #218 — a live nudge must not wipe a composer or a loaded second page.
  it("holds a live re-read under an open composer and keeps the loaded second page", async () => {
    const paged = pagedInbox(SECOND_PAGE);
    const vogt = fakeVogt({
      "GET /inbox": paged.handler,
      "POST /drift/resolve": { body: { proposal: {}, change_applied: false } },
    });
    const mounted = mountAt("/inbox", "/inbox", () => <Inbox />);
    await waitFor(() => expect(screen.getByText(INBOX_ENTRY.title)).toBeInTheDocument());

    fireEvent.click(screen.getByRole("button", { name: "Load more" }));
    await waitFor(() => expect(screen.getByText("Second page entry")).toBeInTheDocument());

    await liveStream(vogt);
    const before = vogt.matching("GET /inbox").length;

    // Open a reason composer on the drift entry and start typing into it.
    fireEvent.click(screen.getByRole("button", { name: "Reject proposed change…" }));
    const reason = screen.getByPlaceholderText("Why this triage decision?");
    fireEvent.input(reason, { target: { value: "still deciding" } });

    // A nudge arrives while the composer is open.
    vogt.stream.changed();
    await settle();

    // Guard held: no re-read happened, so the composer and both pages survive.
    expect(vogt.matching("GET /inbox").length).toBe(before);
    expect(screen.getByPlaceholderText("Why this triage decision?")).toHaveValue("still deciding");
    expect(screen.getByText("Second page entry")).toBeInTheDocument();
    expect(screen.getByText(INBOX_ENTRY.title)).toBeInTheDocument();
    mounted.unmount();
  });

  // #218 — a permitted live re-read merges by entry_key and re-reads to depth.
  it("merges a live re-read by entry_key, keeping row identity and the loaded depth", async () => {
    const paged = pagedInbox(SECOND_PAGE);
    const vogt = fakeVogt({ "GET /inbox": paged.handler });
    const mounted = mountAt("/inbox", "/inbox", () => <Inbox />);
    await waitFor(() => expect(screen.getByText(INBOX_ENTRY.title)).toBeInTheDocument());

    fireEvent.click(screen.getByRole("button", { name: "Load more" }));
    await waitFor(() => expect(screen.getByText("Second page entry")).toBeInTheDocument());

    await liveStream(vogt);
    const before = vogt.matching("GET /inbox").length;
    const firstRow = document.querySelector(`[data-entry-key="${CSS.escape(INBOX_ENTRY.entry_key)}"]`);

    vogt.stream.changed();

    // Both loaded pages are re-read — the depth is preserved, not collapsed.
    await waitFor(() => expect(vogt.matching("GET /inbox").length).toBe(before + 2));
    // The unchanged first row keeps its very DOM node: merge reused the object.
    expect(document.querySelector(`[data-entry-key="${CSS.escape(INBOX_ENTRY.entry_key)}"]`)).toBe(firstRow);
    expect(screen.getByText("Second page entry")).toBeInTheDocument();
    mounted.unmount();
  });

  // #220 — "Open entry" honours whether there is anything in-app to open.
  it("disables Open entry when neither a work item nor a session backs it", async () => {
    const orphan = { ...INBOX_ENTRY, work_item_ref: null, session_id: null };
    fakeVogt({ "GET /inbox": { body: { ...INBOX_RESULT, entries: [orphan] } } });
    const mounted = mountAt("/inbox", "/inbox", () => <Inbox />);
    await waitFor(() => expect(screen.getByText(INBOX_ENTRY.title)).toBeInTheDocument());

    const open = screen.getByRole("button", { name: "Open entry" });
    expect(open).toBeDisabled();
    expect(open.getAttribute("title")).toContain("Nothing to open in-app");
    mounted.unmount();
  });

  it("enables Open entry when a work item backs it", async () => {
    fakeVogt({ "GET /inbox": { body: INBOX_RESULT } });
    const mounted = mountAt("/inbox", "/inbox", () => <Inbox />);
    await waitFor(() => expect(screen.getByText(INBOX_ENTRY.title)).toBeInTheDocument());

    expect(screen.getByRole("button", { name: "Open entry" })).toBeEnabled();
    mounted.unmount();
  });

  // #220 — evidence opens for the selected entry and for drift-with-a-change,
  // and stays folded otherwise.
  it("opens evidence for drift-with-a-change and for the selected entry only", async () => {
    const plain = {
      ...INBOX_ENTRY,
      entry_key: "ci:build-42",
      source: "ci",
      title: "CI build log",
      action: { kind: "observation" },
      proposed_change: null,
      evidence_snapshot: { log: "the last hundred lines" },
    };
    fakeVogt({ "GET /inbox": { body: { ...INBOX_RESULT, entries: [INBOX_ENTRY, plain] } } });
    const mounted = mountAt("/inbox", "/inbox", () => <Inbox />);
    await waitFor(() => expect(screen.getByText("CI build log")).toBeInTheDocument());

    const driftEvidence = document.querySelector(
      `[data-entry-key="${CSS.escape(INBOX_ENTRY.entry_key)}"] .inbox-evidence`,
    );
    const plainEvidence = document.querySelector('[data-entry-key="ci:build-42"] .inbox-evidence');
    // The drift entry carries a proposed change, so its evidence starts open.
    expect(driftEvidence).toHaveAttribute("open");
    // The plain, unselected entry keeps its evidence folded.
    expect(plainEvidence).not.toHaveAttribute("open");

    // Selecting the plain entry opens its evidence.
    fireEvent.click(screen.getByLabelText("Select CI build log"));
    expect(plainEvidence).toHaveAttribute("open");
    mounted.unmount();
  });
});
