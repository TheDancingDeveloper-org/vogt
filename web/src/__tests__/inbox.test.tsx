import { fireEvent, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, describe, expect, it } from "vitest";
import Inbox from "../Inbox";
import {
  INBOX_ENTRY,
  INBOX_RESULT,
  fakeVogt,
  held,
  mountAt,
  refusal,
  unavailable,
} from "./harness";

describe("canonical Inbox interactions", () => {
  afterEach(() => {
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

  it("orders the attention answer before progressive coverage and batch operations", async () => {
    fakeVogt({ "GET /inbox": { body: INBOX_RESULT } });
    const mounted = mountAt("/inbox", "/inbox", () => <Inbox />);
    await waitFor(() => expect(screen.getByText(INBOX_ENTRY.title)).toBeInTheDocument());

    const stream = screen.getByLabelText("Attention stream");
    const coverage = screen.getByText("Coverage and provenance", { selector: "summary" }).parentElement!;
    const batch = screen.getByText(/Batch operations/).parentElement!;
    expect(stream.compareDocumentPosition(coverage) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(coverage.compareDocumentPosition(batch) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(coverage).not.toHaveAttribute("open");
    expect(batch).not.toHaveAttribute("open");
    mounted.unmount();
  });
});
