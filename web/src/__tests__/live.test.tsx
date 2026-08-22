// FR-U10, all three of its clauses, on the surfaces that hold them.
//
// The requirement is one sentence with three promises in it: server-announced
// state updates live from the SSE stream without a manual refresh, a lost
// stream is indicated and reconciles on reconnect, and **a stale view shall
// never present itself as current**. `REQUIREMENTS.md` §6.2 recorded the
// first as absent on the drift inbox and the notification inbox, and the
// third as kept on the board and on none of the other four surfaces; §6.2a
// recorded that the board's own subscription — the one half that worked —
// was asserted at both ends and never across the join.
//
// So these tests drive the join. `liveStream` opens the client's real
// subscription against the harness's `/api/events`, a frame goes in on the
// engine's wire format, and what comes out is a surface re-reading through
// the real route table. Nothing between the two ends is mocked: `api.ts`
// parses the frame, `store.ts` routes it, `viewAge.tsx` delivers it, and the
// surface makes the ordinary call it would have made on Refresh.
//
// jsdom has no `EventSource`, which is the reason §6.2a gave for this being
// unasserted — and it turned out not to be the obstacle it looked like:
// `subscribeEvents` uses `fetch` and a `ReadableStream` precisely so it can
// send a bearer token, and the harness already owns `fetch`.

import { afterEach, describe, expect, it, vi } from "vitest";
import { fireEvent, waitFor } from "@solidjs/testing-library";
import AuditBrowser from "../AuditBrowser";
import Backlog from "../Backlog";
import Board from "../Board";
import Projects from "../Projects";
import WorkItemDetail from "../WorkItemDetail";
import { describeAge } from "../viewAge";
import { isConnected } from "../store";
import {
  driftProposal,
  fakeVogt,
  freshness,
  liveStream,
  mountAt,
  rankedEntry,
  refusal,
  settle,
  stopLiveStream,
  workItem,
} from "./harness";

afterEach(() => {
  // Module state in `store.ts`: a stream left open outlives the test that
  // opened it, and takes its reconnect timer with it.
  stopLiveStream();
});

const NO_CORE = "vogt-core is not configured for this front door";

// -- clause three, as a sentence --------------------------------------------
//
// The wording is the requirement here, so it is asserted where it is decided:
// `describeAge` is pure, and every badge on every surface is one call to it.

describe("FR-U10 — a stale view never presents itself as current", () => {
  const now = 1_770_000_000_000;

  it("does not claim an age it does not have", () => {
    expect(describeAge({ loadedAt: null, now })).toEqual({
      tone: "waiting",
      text: "Not loaded yet",
    });
  });

  it("calls a fresh polling view what it is, and never current", () => {
    const age = describeAge({ loadedAt: now - 4_000, now, poll: 20 });
    expect(age.tone).toBe("live");
    expect(age.text).toBe("Polling — updated 4s ago");
  });

  it("stops calling a polled view live once the poll has plainly not run", () => {
    // Three intervals is the board's own patience, kept in the move.
    const age = describeAge({ loadedAt: now - 61_000, now, poll: 20 });
    expect(age.tone).toBe("stale");
    expect(age.text).toBe("Stale — updated 1m ago");
  });

  it("tells a view with nothing behind it what would make it current", () => {
    // The backlog: no poll, no subscription, one Refresh button. A stale
    // badge that does not say what resolves it is a nag.
    const fresh = describeAge({ loadedAt: now - 3_000, now });
    expect(fresh.text).toBe("Updated 3s ago");
    const old = describeAge({ loadedAt: now - 600_000, now });
    expect(old.tone).toBe("stale");
    expect(old.text).toBe("Stale — updated 10m ago, press Refresh");
  });

  it("treats a quiet stream as a possibly dead one", () => {
    // The distinction the requirement turns on: a subscription with nothing
    // to say and a subscription that stopped arriving look identical from
    // here, so the older of the two is not called live.
    expect(describeAge({ loadedAt: now - 3_000, now, live: true }).text).toBe(
      "Live — updated 3s ago",
    );
    const old = describeAge({ loadedAt: now - 300_000, now, live: true });
    expect(old.tone).toBe("stale");
    expect(old.text).toBe("Stale — updated 5m ago, refresh to be sure");
  });

  it("promises a retry only where one is coming", () => {
    expect(describeAge({ loadedAt: now - 1_000, now, poll: 20, failed: true }).text).toBe(
      "Stale — updated 1s ago, retrying",
    );
    expect(describeAge({ loadedAt: now - 1_000, now, failed: true }).text).toBe(
      "Stale — updated 1s ago, press Refresh",
    );
  });

  it("dates the last answer when Vogt cannot be asked at all", () => {
    const age = describeAge({ loadedAt: now - 1_000, now, outage: NO_CORE });
    expect(age.tone).toBe("outage");
    expect(age.text).toContain("not current");
    expect(age.text).toContain("last answer");
  });
});

// -- clause three, on the four surfaces that had no badge -------------------

describe("FR-U10 — every Vogt surface says how old it is", () => {
  it("the backlog says when it last got an answer", async () => {
    fakeVogt();
    const { container } = mountAt("/backlog", "/backlog", () => <Backlog />);

    await waitFor(() =>
      expect(container.querySelector(".vogt-backlog-age")?.textContent).toMatch(
        /Updated \d+s ago/,
      ),
    );
  });

  it("a backlog tab left open does not look like one loaded a second ago", async () => {
    // The clause in its own words: the read happens five minutes in the past
    // and the clock is put back before the badge's own tick, so what is being
    // asserted is that the view measures its age rather than assuming it.
    const base = Date.now();
    const clock = vi.spyOn(Date, "now").mockReturnValue(base - 300_000);

    fakeVogt();
    const { container } = mountAt("/backlog", "/backlog", () => <Backlog />);
    await waitFor(() =>
      expect(container.querySelector(".vogt-backlog-age")?.textContent).toMatch(
        /Updated \d+s ago/,
      ),
    );
    clock.mockRestore();

    await waitFor(
      () =>
        expect(container.querySelector(".vogt-backlog-age")?.textContent).toBe(
          "Stale — updated 5m ago, press Refresh",
        ),
      { timeout: 4_000 },
    );
  });

  it("the item page says how old it is, where a stale read is quietest", async () => {
    fakeVogt();
    const { container } = mountAt("/w/WI-1", "/w/WI-1", () => (
      <WorkItemDetail itemRef="WI-1" />
    ));

    // The item page subscribes now (#223), so it reads "Live" rather than
    // naming Refresh — the same honest badge the board and audit browser show.
    await waitFor(() =>
      expect(container.querySelector(".wid-age")?.textContent).toMatch(
        /Live — updated \d+s ago/,
      ),
    );
  });

  it("the audit browser says how old the view it is showing is", async () => {
    fakeVogt();
    const { container } = mountAt("/audit", "/audit", () => <AuditBrowser />);

    await waitFor(() =>
      expect(container.querySelector(".vab-age")?.textContent).toMatch(
        /Live — updated \d+s ago/,
      ),
    );
  });

  it("the project pages say how old the view they are showing is", async () => {
    fakeVogt();
    const { container } = mountAt("/projects", "/projects?view=drift", () => <Projects />);

    await waitFor(() =>
      expect(container.querySelector(".vogt-projects-age")?.textContent).toMatch(
        /Live — updated \d+s ago/,
      ),
    );
  });

  it("dates a failed refresh from the answer it still has, not from the failure", async () => {
    // The badge's whole job in one case: the view loaded five minutes ago,
    // a re-read has just failed, and what is on screen is still the
    // five-minute-old answer. Stamping the failure would reset the age to
    // zero and the view would look current at the exact moment it stopped
    // being able to become current.
    const base = Date.now();
    const clock = vi.spyOn(Date, "now").mockReturnValue(base - 300_000);

    const vogt = fakeVogt();
    const { container } = mountAt("/audit", "/audit", () => <AuditBrowser />);
    await waitFor(() =>
      expect(container.querySelector(".vab-age")?.textContent).toMatch(
        /Live — updated \d+s ago/,
      ),
    );
    clock.mockRestore();
    await liveStream(vogt);

    vogt.route("GET /audit", refusal(500, "audit.list: the index is corrupt"));
    vogt.stream.changed();

    await waitFor(
      () =>
        expect(container.querySelector(".vab-age")?.textContent).toBe(
          "Stale — updated 5m ago, retrying",
        ),
      { timeout: 4_000 },
    );
  });

  it("says Vogt is unreachable rather than putting an age on an answer it has not got", async () => {
    fakeVogt({ "GET /backlog": { status: 503, body: { error: { message: NO_CORE } } } });
    const { container } = mountAt("/backlog", "/backlog", () => <Backlog />);

    await waitFor(() =>
      expect(container.querySelector(".vogt-backlog-age")?.textContent).toBe(
        "Vogt unreachable",
      ),
    );
  });
});

// -- clause one: the join, on the board -------------------------------------

describe("FR-U10 — the board reloads on what the front door announced", () => {
  /** The board with its poll switched off, so a re-read can only be the
   *  stream. Anything else here would be asserting the poll. */
  function board() {
    return mountAt("/board", "/board?poll=off", () => <Board />);
  }

  it("re-reads when vogt-core says something changed, with no poll and no Refresh", async () => {
    const vogt = fakeVogt();
    const { container } = board();
    await waitFor(() => expect(vogt.matching("GET /work")).toHaveLength(1));
    await liveStream(vogt);

    // The board is paused: it has said so on screen, and the only thing that
    // can move it now is the announcement.
    expect(container.querySelector(".board-freshness")?.textContent).toMatch(/^Paused/);

    vogt.route("GET /work", {
      body: { items: [workItem({ state: "in_progress" })], total: 1 },
    });
    vogt.stream.changed();

    await waitFor(() => expect(vogt.matching("GET /work").length).toBeGreaterThan(1));
    await waitFor(() =>
      expect(container.querySelector('.board-cell[data-state="in progress"] .board-card')).toBeTruthy(),
    );
  });

  it("does not re-read under a reason somebody is typing", async () => {
    const vogt = fakeVogt();
    const { container } = board();
    await waitFor(() => expect(vogt.matching("GET /work")).toHaveLength(1));
    await liveStream(vogt);

    // Drop a card, which opens the composer FR-W1 requires. A refetch here
    // would swap the item list under an unsaved move.
    fireEvent.dragStart(container.querySelector("#board-card-WI-1")!);
    fireEvent.drop(container.querySelector('.board-cell[data-state="in progress"]')!);
    await waitFor(() =>
      expect(
        container.querySelector('.board-cell[data-state="in progress"] textarea'),
      ).toBeTruthy(),
    );

    vogt.stream.changed();
    await settle();

    expect(vogt.matching("GET /work")).toHaveLength(1);
    expect(
      container.querySelector('.board-cell[data-state="in progress"] textarea'),
    ).toBeTruthy();
  });
});

// -- clause one: the item page and the backlog (#223) -----------------------

describe("FR-U10 — the item page reconciles on what the front door announced", () => {
  function commentField(container: HTMLElement): HTMLTextAreaElement {
    const label = [...container.querySelectorAll("label.wid-field")].find(
      (node) => node.querySelector("span")?.textContent === "Comment",
    );
    return label!.querySelector("textarea")!;
  }

  it("re-reads the item and its sessions when vogt-core says something changed", async () => {
    const vogt = fakeVogt();
    mountAt("/w/WI-1", "/w/WI-1", () => <WorkItemDetail itemRef="WI-1" />);
    await waitFor(() => expect(vogt.matching("GET /work/get")).toHaveLength(1));
    await liveStream(vogt);

    // The item page used to sit here forever; now a transition somebody else
    // made reaches it, and its live-activity session badge with it.
    vogt.stream.changed();
    await waitFor(() =>
      expect(vogt.matching("GET /work/get").length).toBeGreaterThan(1),
    );
    await waitFor(() =>
      expect(vogt.matching("GET /sessions").length).toBeGreaterThan(1),
    );
  });

  it("does not re-read under a comment somebody is typing", async () => {
    const vogt = fakeVogt();
    const { container } = mountAt("/w/WI-1", "/w/WI-1", () => (
      <WorkItemDetail itemRef="WI-1" />
    ));
    await waitFor(() => expect(vogt.matching("GET /work/get")).toHaveLength(1));
    await liveStream(vogt);

    // A refetch here would swap the item — and the comment being written with
    // it — out from under the reader.
    fireEvent.input(commentField(container), {
      target: { value: "half a thought, not yet posted" },
    });
    const asked = vogt.matching("GET /work/get").length;

    vogt.stream.changed();
    await settle();

    expect(vogt.matching("GET /work/get")).toHaveLength(asked);
    expect(commentField(container).value).toBe("half a thought, not yet posted");
  });
});

describe("FR-U10 — the backlog reconciles on tab return but not on every nudge", () => {
  it("reloads the ranked list when the tab comes back to the front", async () => {
    const vogt = fakeVogt({
      "GET /backlog": { body: { items: [rankedEntry()], freshness: freshness() } },
    });
    mountAt("/backlog", "/backlog", () => <Backlog />);
    await waitFor(() => expect(vogt.matching("GET /backlog")).toHaveLength(1));
    await liveStream(vogt);

    // A stream nudge still does not re-rank the estate under the reader.
    vogt.stream.changed();
    await settle();
    expect(vogt.matching("GET /backlog")).toHaveLength(1);

    // But a tab left in the background and brought back is the moment its
    // answer is furthest from current, so that one case reconciles.
    document.dispatchEvent(new Event("visibilitychange"));
    await waitFor(() =>
      expect(vogt.matching("GET /backlog").length).toBeGreaterThan(1),
    );
  });
});

// -- clause one: drift arrivals ---------------------------------------------

describe("FR-U10 — drift arrivals reach the inbox without being asked for", () => {
  function inbox() {
    return mountAt("/projects", "/projects?view=drift", () => <Projects />);
  }

  it("shows a proposal raised by a sweep somebody else ran", async () => {
    const vogt = fakeVogt();
    const { container } = inbox();
    await waitFor(() => expect(vogt.matching("GET /drift")).toHaveLength(1));
    await liveStream(vogt);
    expect(container.textContent).not.toContain("the repository says 1.3.0");

    // What the core publishes when `drift.detect` raises one, republished by
    // the front door onto the stream this client already has open.
    vogt.route("GET /drift", {
      body: { proposals: [driftProposal()], freshness: freshness() },
    });
    vogt.stream.changed({
      kind: "drift.raised",
      entity_kind: "drift_proposal",
      entity_id: "dft_01",
    });

    await waitFor(() =>
      expect(container.textContent).toContain("the repository says 1.3.0"),
    );
    expect(vogt.matching("GET /drift").length).toBeGreaterThan(1);
  });

  it("does not throw away a reason somebody is part-way through typing", async () => {
    const vogt = fakeVogt({
      "GET /drift": { body: { proposals: [driftProposal()], freshness: freshness() } },
    });
    const { container } = inbox();
    await waitFor(() =>
      expect(container.querySelector(".vogt-projects-resolve")).toBeTruthy(),
    );
    await liveStream(vogt);

    const reason = container.querySelector<HTMLInputElement>(
      ".vogt-projects-resolve input[type=text]",
    )!;
    fireEvent.input(reason, { target: { value: "the observed version is the real one" } });
    const asked = vogt.matching("GET /drift").length;

    vogt.stream.changed({ kind: "drift.raised", entity_kind: "drift_proposal" });
    await settle();

    // FR-W1's reason is the user's sentence, and a background re-read
    // replaces every proposal object on this page.
    expect(vogt.matching("GET /drift")).toHaveLength(asked);
    expect(
      container.querySelector<HTMLInputElement>(
        ".vogt-projects-resolve input[type=text]",
      )?.value,
    ).toBe("the observed version is the real one");
  });

  it("leaves the other panels alone: an announcement is not a page reload", async () => {
    const vogt = fakeVogt();
    const { container } = mountAt("/projects", "/projects?p=alpha", () => <Projects />);
    await waitFor(() => expect(vogt.matching("GET /projects/brief")).toHaveLength(1));
    await liveStream(vogt);

    vogt.stream.changed();
    await settle();

    // The brief is an aggregate over sweeps; re-pulling it on every announced
    // transition would be a poll wearing an event's clothes.
    expect(vogt.matching("GET /projects/brief")).toHaveLength(1);
    expect(container.querySelector(".vogt-projects-age")).toBeTruthy();
  });
});

// -- clause one: notification counts ----------------------------------------

describe("FR-U10 — the notification count is re-read, not remembered", () => {
  function notifications(unread: number, total = unread) {
    return {
      body: {
        notifications: [],
        total,
        unread,
        by_reason: {},
        freshness: freshness(),
      },
    };
  }

  it("moves the unread count when a sweep lands", async () => {
    const vogt = fakeVogt({ "GET /notifications": notifications(0, 0) });
    const { container } = mountAt("/audit", "/audit?view=inbox", () => <AuditBrowser />);
    await waitFor(() => expect(container.textContent).toContain("0 unread"));
    await liveStream(vogt);

    // A notification is collected during a sweep and a sweep publishes
    // `sweep.completed` onto the core's feed — which is the only moment this
    // count can move, and now the only moment it needs to.
    vogt.route("GET /notifications", notifications(3));
    vogt.stream.changed({ kind: "sweep.completed", entity_kind: "sweep" });

    await waitFor(() => expect(container.textContent).toContain("3 unread"));
  });

  it("keeps the filter, the page and the window across a live re-read", async () => {
    const vogt = fakeVogt({ "GET /notifications": notifications(2) });
    mountAt("/audit", "/audit?view=inbox&unread=1&nreason=mention", () => <AuditBrowser />);
    await waitFor(() => expect(vogt.matching("GET /notifications")).toHaveLength(1));
    await liveStream(vogt);

    vogt.stream.changed({ kind: "sweep.completed", entity_kind: "sweep" });
    await waitFor(() =>
      expect(vogt.matching("GET /notifications").length).toBeGreaterThan(1),
    );

    const asked = vogt.matching("GET /notifications").at(-1)!;
    expect(asked.query.get("unread_only")).toBe("true");
    expect(asked.query.get("reason")).toBe("mention");
  });

  it("re-reads the audit log too, which is the record of what was announced", async () => {
    const vogt = fakeVogt();
    mountAt("/audit", "/audit", () => <AuditBrowser />);
    await waitFor(() => expect(vogt.matching("GET /audit")).toHaveLength(1));
    await liveStream(vogt);

    vogt.stream.changed();
    await waitFor(() => expect(vogt.matching("GET /audit").length).toBeGreaterThan(1));
  });
});

// -- clause two: a lost stream ----------------------------------------------

describe("FR-U10 — a lost stream is indicated and reconciles on reconnect", () => {
  it("notices an ended stream, reopens it, and catches up on the next change", async () => {
    const vogt = fakeVogt();
    const { container } = mountAt("/board", "/board?poll=off", () => <Board />);
    await waitFor(() => expect(vogt.matching("GET /work")).toHaveLength(1));
    await liveStream(vogt);
    expect(isConnected()).toBe(true);

    // Not an error — an end, which is what a restarted engine or a proxy's
    // idle timeout looks like from here. It used to leave the client
    // believing it was connected forever, with every surface quiet and
    // nothing on screen saying so.
    vogt.stream.drop();
    await waitFor(() => expect(isConnected()).toBe(false));

    await waitFor(() => expect(vogt.stream.opens()).toBeGreaterThan(1), {
      timeout: 5_000,
    });
    await vogt.stream.opened();
    await settle();

    vogt.route("GET /work", {
      body: { items: [workItem({ state: "in_progress" })], total: 1 },
    });
    vogt.stream.changed();

    await waitFor(() =>
      expect(
        container.querySelector('.board-cell[data-state="in progress"] .board-card'),
      ).toBeTruthy(),
    );
    expect(isConnected()).toBe(true);
  }, 20_000);
});

// -- the ranked views, which are deliberately not live -----------------------

describe("FR-U10 — the backlog says what it is rather than what it is not", () => {
  it("does not re-rank the estate under the reader on every announcement", async () => {
    const vogt = fakeVogt({
      "GET /backlog": { body: { items: [rankedEntry()], freshness: freshness() } },
    });
    const { container } = mountAt("/backlog", "/backlog", () => <Backlog />);
    await waitFor(() => expect(vogt.matching("GET /backlog")).toHaveLength(1));
    await liveStream(vogt);

    vogt.stream.changed();
    await settle();

    // The decision, asserted so it stays a decision: the badge tells the
    // reader the age of what they are looking at and names Refresh, and
    // nothing re-orders the list they are reading down.
    expect(vogt.matching("GET /backlog")).toHaveLength(1);
    expect(container.querySelector(".vogt-backlog-age")?.textContent).not.toContain(
      "Live",
    );
  });
});
