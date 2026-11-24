import { fireEvent, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, describe, expect, it } from "vitest";
import Inbox from "../Inbox";
import {
  DEFAULT_INBOX_FILTER,
  filterFromQuery,
  filterParams,
  filterPath,
  parseFilter,
  serializeFilter,
  savedInboxFilter,
} from "../inboxFilter";
import { createPlaceMetrics } from "../placeMetrics";
import { INBOX_ENTRY, INBOX_RESULT, fakeVogt, mountAt, queryOf, settle, stopLiveStream } from "./harness";

const SAVED_EXTERNAL = {
  preferences: [
    {
      key: "inbox.filter",
      value: { sources: null, actor: "external", triage_states: ["active"] },
      version: 3,
      updated_at: "2026-08-17T10:00:00Z",
    },
  ],
};

describe("the Inbox filter model", () => {
  it("round-trips through the stored shape, the URL and the list parameters", () => {
    const filter = { source: "github", actor: "external", triage: "all" } as const;
    expect(parseFilter(serializeFilter(filter))).toEqual(filter);
    expect(filterFromQuery(filterPath(filter).split("?")[1]!)).toEqual(filter);
    expect(filterParams(filter)).toEqual({
      sources: "github",
      actor: "external",
      triage_states: ["active", "snoozed", "archived"],
    });
    // The default is stored as "nothing saved" and states nothing in the URL.
    expect(serializeFilter(DEFAULT_INBOX_FILTER)).toEqual({});
    expect(filterPath(DEFAULT_INBOX_FILTER)).toBe("/inbox");
    expect(filterFromQuery("")).toBeNull();
  });
});

describe("a saved Inbox filter persists until changed or cleared", () => {
  afterEach(() => {
    stopLiveStream();
    window.history.replaceState({}, "", "/");
  });

  it("restores the account's saved filter on a plain /inbox entry and says so", async () => {
    const vogt = fakeVogt({
      "GET /inbox": { body: INBOX_RESULT },
      "GET /preferences": { body: SAVED_EXTERNAL },
    });
    const mounted = mountAt("/inbox", "/inbox", () => <Inbox />);
    await waitFor(() =>
      expect(vogt.matching("GET /inbox").some((call) => call.query.get("actor") === "external")).toBe(true),
    );
    await waitFor(() => expect(screen.getByText(/Saved filter: external people only/)).toBeInTheDocument());
    expect(screen.getByLabelText("From")).toHaveValue("external");
    mounted.unmount();
  });

  it("lets an explicit link override the saved filter for that visit", async () => {
    const vogt = fakeVogt({
      "GET /inbox": { body: INBOX_RESULT },
      "GET /preferences": { body: SAVED_EXTERNAL },
    });
    const mounted = mountAt("/inbox", "/inbox?source=drift", () => <Inbox />);
    await waitFor(() => expect(screen.getByText(INBOX_ENTRY.title)).toBeInTheDocument());
    await settle();
    const reads = vogt.matching("GET /inbox");
    expect(reads.every((call) => call.query.get("sources") === "drift")).toBe(true);
    expect(reads.every((call) => call.query.get("actor") === null)).toBe(true);
    expect(screen.getByText(/Filter from link, not saved: drift/)).toBeInTheDocument();
    expect(vogt.matching("POST /preferences")).toHaveLength(0);
    mounted.unmount();
  });

  it("saves a changed filter to the account and clears it with one control", async () => {
    const vogt = fakeVogt({ "GET /inbox": { body: INBOX_RESULT } });
    const mounted = mountAt("/inbox", "/inbox", () => <Inbox />);
    await waitFor(() => expect(screen.getByText(INBOX_ENTRY.title)).toBeInTheDocument());

    fireEvent.change(screen.getByLabelText("From"), { target: { value: "external" } });
    await waitFor(() => expect(vogt.matching("POST /preferences")).toHaveLength(1));
    expect(vogt.matching("POST /preferences")[0]?.body).toMatchObject({
      key: "inbox.filter",
      value: { sources: null, actor: "external", triage_states: ["active"] },
    });
    await waitFor(() => expect(queryOf(mounted.url()).get("actor")).toBe("external"));
    await waitFor(() =>
      expect(vogt.matching("GET /inbox").at(-1)?.query.get("actor")).toBe("external"),
    );
    expect(savedInboxFilter.value().actor).toBe("external");

    fireEvent.click(screen.getByRole("button", { name: "Clear filters" }));
    await waitFor(() => expect(vogt.matching("POST /preferences")).toHaveLength(2));
    expect(vogt.matching("POST /preferences")[1]?.body).toMatchObject({ key: "inbox.filter", value: {} });
    await waitFor(() => expect(vogt.matching("GET /inbox").at(-1)?.query.get("actor")).toBeNull());
    expect(screen.queryByRole("button", { name: "Clear filters" })).not.toBeInTheDocument();
    mounted.unmount();
  });

  it("says how many entries the external filter hid because the author is unknown", async () => {
    fakeVogt({
      "GET /inbox": { body: { ...INBOX_RESULT, actor_unknown_hidden: 2 } },
      "GET /preferences": { body: SAVED_EXTERNAL },
    });
    const mounted = mountAt("/inbox", "/inbox", () => <Inbox />);
    await waitFor(() => expect(screen.getByText(/2 hidden: author unknown/)).toBeInTheDocument());
    mounted.unmount();
  });

  it("shows who caused an entry", async () => {
    fakeVogt({
      "GET /inbox": {
        body: {
          ...INBOX_RESULT,
          entries: [{ ...INBOX_ENTRY, source: "github", actor_login: "mallory", actor_kind: "human", actor_relation: "external" }],
        },
      },
    });
    const mounted = mountAt("/inbox", "/inbox", () => <Inbox />);
    await waitFor(() => expect(screen.getByText("From mallory · external person")).toBeInTheDocument());
    mounted.unmount();
  });
});

describe("the Inbox badge honours the saved filter", () => {
  it("reports the filter the core counted under", async () => {
    fakeVogt({
      "GET /place/metrics": {
        body: {
          inbox_active: 2,
          inbox_active_unfiltered: 9,
          inbox_filter: { sources: ["github"], actor: "external", triage_states: ["active"] },
          projects_total: 1,
          work_total: 1,
          backlog_total_considered: 1,
          drift_present: false,
          revision: 1,
          generated_at: "2026-08-19T00:00:00Z",
        },
      },
    });
    const state = createPlaceMetrics();
    await state.refresh();
    expect(state.metrics.inbox).toEqual({ value: 2, state: "ready" });
    expect(state.inboxFilter()).toBe("external people only · github");
    state.dispose();
  });

  it("asks an older core's list for the same filter", async () => {
    localStorage.setItem(
      "vogt.pref.v1:inbox.filter",
      JSON.stringify({ sources: null, actor: "external", triage_states: ["active", "snoozed"] }),
    );
    savedInboxFilter.reset();
    const vogt = fakeVogt({
      "GET /place/metrics": { status: 404, body: { error: { message: "no such operation" } } },
      "GET /inbox": {
        body: { entries: [], snapshot_at: "2026-08-18T00:00:00Z", coverage: {}, counts: { active: 2, snoozed: 1, archived: 0 } },
      },
      "GET /projects": { body: { projects: [], total: 0 } },
      "GET /work": { body: { items: [], total: 0 } },
      "GET /backlog": { body: { items: [], total_considered: 0, freshness: { status: "fresh", collectors: {} } } },
      "GET /drift": { body: { proposals: [] } },
    });
    const state = createPlaceMetrics();
    await state.refresh();
    const read = vogt.matching("GET /inbox")[0]!;
    expect(read.query.get("actor")).toBe("external");
    expect(read.query.getAll("triage_states")).toEqual(["active", "snoozed", "archived"]);
    // "all" states: the count sums every state the filter selects.
    expect(state.metrics.inbox).toEqual({ value: 3, state: "ready" });
    expect(state.inboxFilter()).toBe("external people only · all states");
    state.dispose();
  });
});
