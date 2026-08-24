// Clear all (#353): a batch action that archives every ACTIVE entry in view.
// It goes through the same archiveKeys path (per-entry refusals retained), and
// it honours the source filter — clearing while filtered to one source clears
// only that source's entries, and the button/confirm say so.

import { fireEvent, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, describe, expect, it } from "vitest";
import Inbox from "../Inbox";
import { INBOX_ENTRY, INBOX_RESULT, fakeVogt, mountAt, settle, stopLiveStream } from "./harness";

afterEach(() => {
  stopLiveStream();
  window.history.replaceState({}, "", "/");
});

/** A github-scoped inbox: two active entries and one already snoozed, so a
 *  clear-all can be shown to touch only the active ones. */
function githubInbox() {
  return {
    ...INBOX_RESULT,
    entries: [
      { ...INBOX_ENTRY, entry_key: "gh-1", source: "github", title: "Active one", triage_state: "active" },
      { ...INBOX_ENTRY, entry_key: "gh-2", source: "github", title: "Active two", triage_state: "active" },
      { ...INBOX_ENTRY, entry_key: "gh-3", source: "github", title: "Already snoozed", triage_state: "snoozed" },
    ],
  };
}

describe("inbox clear all (#353)", () => {
  it("archives every active entry, names the source, and honours the source filter", async () => {
    const vogt = fakeVogt({
      "GET /inbox": { body: githubInbox() },
      "POST /inbox/archive": { body: { entry: {} } },
    });
    const mounted = mountAt("/inbox", "/inbox?source=github", () => <Inbox />);
    await waitFor(() => expect(screen.getByText("Active one")).toBeInTheDocument());

    // The label names the source it is scoped to, so it never reads as a
    // whole-inbox sweep.
    const clear = screen.getByRole("button", { name: "Clear all github…" });
    fireEvent.click(clear);

    // The confirm step counts the active entries — the snoozed one is not one.
    await waitFor(() =>
      expect(screen.getByText(/Archive all 2 active github entries in view/)).toBeInTheDocument(),
    );

    fireEvent.input(screen.getByLabelText("Batch reason"), {
      target: { value: "sweeping github" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Confirm clear all" }));

    await waitFor(() => expect(vogt.matching("POST /inbox/archive").length).toBe(2));
    const archived = vogt.matching("POST /inbox/archive").map((call) => call.body?.entry_key);
    // Only the two active entries; the snoozed one is left alone.
    expect(new Set(archived)).toEqual(new Set(["gh-1", "gh-2"]));
    expect(vogt.matching("POST /inbox/archive").every((call) => call.body?.reason === "sweeping github")).toBe(true);

    // The source scope rode into the read that fed the clear.
    expect(vogt.matching("GET /inbox").every((call) => call.query.get("sources") === "github")).toBe(true);
    mounted.unmount();
  });

  it("retains a refused entry and counts it, as the shared archive path does", async () => {
    fakeVogt({
      "GET /inbox": { body: githubInbox() },
      "POST /inbox/archive": (call) =>
        call.body?.entry_key === "gh-2"
          ? { status: 409, body: { error: { code: "test.refused", message: "held" } } }
          : { body: { entry: {} } },
    });
    let reported: string | null = null;
    const mounted = mountAt("/inbox", "/inbox?source=github", () => (
      <Inbox onError={(message) => (reported = message)} />
    ));
    await waitFor(() => expect(screen.getByText("Active one")).toBeInTheDocument());

    fireEvent.click(screen.getByRole("button", { name: "Clear all github…" }));
    fireEvent.input(screen.getByLabelText("Batch reason"), { target: { value: "sweep" } });
    fireEvent.click(screen.getByRole("button", { name: "Confirm clear all" }));

    await waitFor(() => expect(reported).toMatch(/1 Inbox archive\(s\) were refused/));
    await settle();
    // The refused entry is retained as a selection so the reader can see it.
    expect(screen.getByText(/1 selected/)).toBeInTheDocument();
    mounted.unmount();
  });
});
