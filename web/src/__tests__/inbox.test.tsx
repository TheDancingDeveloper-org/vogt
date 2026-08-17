import { fireEvent, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, describe, expect, it } from "vitest";
import Inbox from "../Inbox";
import {
  INBOX_ENTRY,
  INBOX_RESULT,
  fakeVogt,
  mountAt,
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

    fireEvent.keyDown(window, { key: "e" });
    expect(document.activeElement).toBe(screen.getByPlaceholderText("Why this triage decision?"));

    fireEvent.click(screen.getByLabelText(`Select ${INBOX_ENTRY.title}`));
    fireEvent.click(screen.getByRole("button", { name: "Archive selected" }));
    expect(vogt.matching("POST /inbox/archive")).toHaveLength(0);
    fireEvent.input(screen.getByLabelText("Batch reason"), { target: { value: "reviewed with the team" } });
    fireEvent.click(screen.getByRole("button", { name: "Archive selected" }));
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

    fireEvent.click(screen.getByRole("button", { name: "Reject proposed change" }));
    expect(vogt.matching("POST /drift/resolve")).toHaveLength(0);
    fireEvent.input(screen.getByPlaceholderText("Why this triage decision?"), { target: { value: "the declared state is intentional" } });
    fireEvent.click(screen.getByRole("button", { name: "Reject proposed change" }));
    await waitFor(() => expect(vogt.matching("POST /drift/resolve")).toHaveLength(1));
    expect(vogt.matching("POST /drift/resolve")[0]?.body).toEqual({
      id: "proposal-1",
      resolution: "rejected",
      reason: "the declared state is intentional",
    });
    mounted.unmount();
  });
});
