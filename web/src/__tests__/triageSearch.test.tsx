// Free-text search (#350) on the three triage surfaces. Board and Backlog
// take the term as a first-class filter that lives in the URL; the Inbox takes
// a display box beside its source pills.

import { fireEvent, waitFor } from "@solidjs/testing-library";
import { afterEach, describe, expect, it } from "vitest";
import Board from "../Board";
import Inbox from "../Inbox";
import {
  INBOX_ENTRY,
  INBOX_RESULT,
  fakeVogt,
  mountAt,
  settle,
  stopLiveStream,
  workItem,
} from "./harness";

afterEach(() => {
  stopLiveStream();
  window.history.replaceState({}, "", "/");
});

describe("board free-text search (#350)", () => {
  it("filters the loaded cards to the term and writes it to the URL", async () => {
    fakeVogt({
      "GET /work": {
        body: {
          items: [
            workItem({ ref: "WI-1", title: "Login page throws on submit", state: "open" }),
            workItem({ ref: "WI-2", title: "Export the weekly report", state: "open" }),
          ],
          total: 2,
        },
      },
    });
    const mounted = mountAt("/board", "/board", () => <Board />);
    const { container } = mounted;
    await waitFor(() => expect(container.querySelector("#board-card-WI-1")).toBeTruthy());
    expect(container.querySelector("#board-card-WI-2")).toBeTruthy();

    const search = container.querySelector<HTMLInputElement>('input[aria-label="Search work items"]');
    expect(search).toBeTruthy();
    fireEvent.input(search!, { target: { value: "login" } });
    await settle();

    // Only the matching card is drawn, and the term is in the URL so it
    // deep-links and saves into a lens.
    expect(container.querySelector("#board-card-WI-1")).toBeTruthy();
    expect(container.querySelector("#board-card-WI-2")).toBeNull();
    expect(mounted.url()).toContain("q=login");
    mounted.unmount();
  });

  it("restores a pasted ?q= term and applies it on load", async () => {
    fakeVogt({
      "GET /work": {
        body: {
          items: [
            workItem({ ref: "WI-1", title: "Login page throws on submit", state: "open" }),
            workItem({ ref: "WI-2", title: "Export the weekly report", state: "open" }),
          ],
          total: 2,
        },
      },
    });
    const mounted = mountAt("/board", "/board?q=export", () => <Board />);
    const { container } = mounted;
    await waitFor(() => expect(container.querySelector("#board-card-WI-2")).toBeTruthy());
    expect(container.querySelector("#board-card-WI-1")).toBeNull();
    mounted.unmount();
  });
});

describe("inbox free-text search (#350)", () => {
  const TWO_ENTRIES = {
    ...INBOX_RESULT,
    entries: [
      { ...INBOX_ENTRY, entry_key: "e-1", title: "Login page state mismatch" },
      { ...INBOX_ENTRY, entry_key: "e-2", title: "Export pipeline failed" },
    ],
  };

  it("narrows the drawn entries to the term without re-reading", async () => {
    const vogt = fakeVogt({ "GET /inbox": { body: TWO_ENTRIES } });
    const mounted = mountAt("/inbox", "/inbox", () => <Inbox />);
    const { container } = mounted;
    await waitFor(() =>
      expect(container.querySelector('[data-entry-key="e-1"]')).toBeTruthy(),
    );
    expect(container.querySelector('[data-entry-key="e-2"]')).toBeTruthy();
    const before = vogt.matching("GET /inbox").length;

    const search = container.querySelector<HTMLInputElement>(
      'input[aria-label="Search inbox entries"]',
    );
    expect(search).toBeTruthy();
    fireEvent.input(search!, { target: { value: "export" } });
    await settle();

    expect(container.querySelector('[data-entry-key="e-2"]')).toBeTruthy();
    expect(container.querySelector('[data-entry-key="e-1"]')).toBeNull();
    // A display filter: it re-reads nothing.
    expect(vogt.matching("GET /inbox").length).toBe(before);
    mounted.unmount();
  });
});
