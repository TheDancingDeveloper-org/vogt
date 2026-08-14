// What the audit browser *asks Vogt for* (FR-U19, FR-S6, FR-U11, FR-U21).
//
// Every assertion in this file is about the query the server receives, and
// that is deliberate: a filter applied to rows the client already has and a
// filter applied in the store render identically. Both show the reader a
// shorter list under the same heading. They differ entirely in what they can
// see — the first can only ever narrow the newest few hundred records, and a
// reader who asked "what happened in alpha last March" gets an empty list that
// means "not in the last 500 writes" while reading as "nothing happened".
//
// So these tests look at `RecordedCall.query`. A test that asserted the
// rendered rows would have passed against the surface as it was before this
// change, which is the definition of a test that proves nothing.
//
// `tests/test_audit_query.py` is the other half: it fixes the semantics of the
// parameters on the server (`since` inclusive, `until` exclusive, `total`
// counting the narrowing rather than the page). Nothing here re-asserts those.
// What is asserted here is that the surface uses them, and uses them the way
// they mean — the tiling presets below are the one place the two files touch.

import { afterAll, afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { fireEvent, waitFor } from "@solidjs/testing-library";
import AuditBrowser from "../AuditBrowser";
import {
  auditRecord,
  fakeVogt,
  liveStream,
  mountAt,
  refusal,
  settle,
  stopLiveStream,
  type FakeVogt,
  type RecordedCall,
} from "./harness";

/**
 * Somewhere that is not UTC, for the reason `test_audit_query.py::off_utc`
 * gives: the controls are `datetime-local` and what the server takes is an
 * instant, so a build that shipped the typed text straight to the wire is
 * invisible on a host already in UTC — which most CI runners are. Put back
 * afterwards, because this runner shares a process across test files.
 */
beforeAll(() => {
  vi.stubEnv("TZ", "America/New_York");
});

afterAll(() => {
  vi.unstubAllEnvs();
});

afterEach(() => {
  stopLiveStream();
});

function audit(url = "/audit") {
  return mountAt("/audit", url, () => <AuditBrowser />);
}

/** The last `audit.list` the surface made, once it has made `count` of them. */
async function asked(vogt: FakeVogt, count = 1): Promise<RecordedCall> {
  await waitFor(() => expect(vogt.matching("GET /audit")).toHaveLength(count));
  return vogt.matching("GET /audit").at(-1)!;
}

/** A front door that answers the audit log with `records` and a stated total. */
function log(records: Record<string, unknown>[], total = records.length) {
  return { "GET /audit": { body: { records, total } } };
}

function labelled(container: HTMLElement, label: string): HTMLElement {
  const found = [...container.querySelectorAll<HTMLElement>("button")].find(
    (button) => button.textContent?.trim() === label,
  );
  if (!found) throw new Error(`no "${label}" button on this surface`);
  return found;
}

/** Wait until the answer is on screen, not merely until it was asked for.
 *
 *  The pager is disabled while the page has no total to reason about, so a
 *  click sent between the request and the answer lands on a dead button. */
async function rendered(container: HTMLElement): Promise<void> {
  await waitFor(() =>
    expect(container.querySelectorAll(".vab-record").length).toBeGreaterThan(0),
  );
}

// -- the narrowing is the store's, not this client's -------------------------

describe("FR-U19 — the project filter is a parameter", () => {
  it("names the project in the query rather than resolving its items first", async () => {
    const vogt = fakeVogt(log([auditRecord()]));
    audit("/audit?project=alpha");

    const call = await asked(vogt);
    expect(call.query.get("project")).toBe("alpha");
    // And the mechanism it replaced is gone: the old surface listed up to 500
    // of the project's work items to decide which loaded rows to keep, which
    // is why comments, sessions and suppressions were never in a project's
    // trail — they carry their own ids and matched nothing.
    expect(vogt.matching("GET /work")).toHaveLength(0);
  });

  it("renders a write the old client-side scope could never have matched", async () => {
    // A suppression is scoped to a project by a foreign key and is not a work
    // item, so no set of work-item ids contains it. The server's filter
    // reaches it; a pass over loaded rows never could.
    const vogt = fakeVogt(
      log([auditRecord({ entity_kind: "suppression", entity_id: "01JSUPPRESS" })]),
    );
    const { container } = audit("/audit?project=alpha");

    await waitFor(() =>
      expect(container.querySelectorAll(".vab-record")).toHaveLength(1),
    );
    expect(container.textContent).toContain("01JSUPPRESS");
    void vogt;
  });

  it("keeps the picker usable when the project list could not be read", async () => {
    // The slug is what the server takes, so a failed facet read costs the
    // reader a dropdown and not the filter — unlike the actor filter below,
    // which needs an id this page can only get from the list.
    const vogt = fakeVogt({
      ...log([auditRecord()]),
      "GET /projects": refusal(500, "project.list: the index is corrupt"),
    });
    audit("/audit?project=alpha");

    const call = await asked(vogt);
    expect(call.query.get("project")).toBe("alpha");
  });
});

describe("FR-U19 — the time range is two instants on the wire", () => {
  it("sends the typed wall clock as the instant it names here", async () => {
    const vogt = fakeVogt(log([]));
    audit("/audit?from=2026-08-01T09:30&to=2026-08-08T09:30");

    const call = await asked(vogt);
    // Eastern daylight time in August, so 09:30 local is 13:30Z. The point of
    // the assertion is the offset: a build that appended a `Z` to the typed
    // text would send 09:30Z and silently move every boundary four hours.
    expect(call.query.get("since")).toBe("2026-08-01T13:30:00.000Z");
    expect(call.query.get("until")).toBe("2026-08-08T13:30:00.000Z");
  });

  it("does not second-guess which records the answer contains", async () => {
    // The server decided. A row outside the range the reader typed is either
    // a server bug or a clock this client cannot read, and hiding it here
    // would delete evidence to tidy a filter.
    const vogt = fakeVogt(log([auditRecord({ at: "2019-01-01T00:00:00Z" })]));
    const { container } = audit("/audit?from=2026-08-01T00:00&to=2026-08-08T00:00");

    await waitFor(() =>
      expect(container.querySelectorAll(".vab-record")).toHaveLength(1),
    );
    void vogt;
  });

  it("offers shortcuts that tile the log instead of overlapping it", async () => {
    // The client half of `test_consecutive_windows_tile_the_log_without_gap_
    // or_overlap`. `until` is exclusive, so Yesterday ending exactly where
    // Today begins means the write made at midnight is in one of them and not
    // in both — which is what lets a reader add up two days without counting
    // it twice.
    const vogt = fakeVogt(log([]));
    const { container } = audit();
    await asked(vogt);

    fireEvent.click(labelled(container, "Yesterday"));
    const yesterday = await asked(vogt, 2);

    fireEvent.click(labelled(container, "Today"));
    const today = await asked(vogt, 3);

    expect(yesterday.query.get("until")).toBe(today.query.get("since"));
    expect(yesterday.query.get("since")).not.toBe(today.query.get("since"));
    // Today has no upper bound: pinning one to "now" would make the range mean
    // something different a second later.
    expect(today.query.get("until")).toBeNull();
  });
});

// -- paging, which is what reaches the rest of the log -----------------------

describe("FR-U19 — the browser can read past the newest records", () => {
  it("asks for a page at an offset rather than a bigger window", async () => {
    const vogt = fakeVogt(log([auditRecord()], 120));
    const { container } = audit("/audit?size=25");

    const first = await asked(vogt);
    expect(first.query.get("limit")).toBe("25");
    expect(first.query.get("offset")).toBeNull();
    await rendered(container);

    fireEvent.click(labelled(container, "Older"));

    const second = await asked(vogt, 2);
    expect(second.query.get("offset")).toBe("25");
    expect(second.query.get("limit")).toBe("25");
  });

  it("reaches record 1501, which no window could show", async () => {
    // The gap this change closes, stated as the number that used to be
    // unreachable: `limit` is capped at 500 server-side, so before there was
    // an offset the newest 500 writes were the whole of what this surface
    // could ever see.
    const vogt = fakeVogt(log([auditRecord()], 4000));
    audit("/audit?size=500&page=3");

    const call = await asked(vogt);
    expect(call.query.get("limit")).toBe("500");
    expect(call.query.get("offset")).toBe("1500");
  });

  it("says how much of how much, from the total rather than the page", async () => {
    const vogt = fakeVogt(
      log([auditRecord(), auditRecord({ id: "01JAUDIT2" })], 3482),
    );
    const { container } = audit("/audit?size=25");

    await waitFor(() => expect(container.textContent).toContain("3482 matching"));
    expect(container.textContent).toContain("records 1–2");
    void vogt;
  });

  it("clamps a link to a page the narrowing does not have", async () => {
    const vogt = fakeVogt(log([auditRecord()], 30));
    const view = audit("/audit?size=25&page=9");

    // The first read is the page the link named; the second is the last page
    // that exists. A blank screen under a full pager is a reader concluding
    // there is nothing there.
    const clamped = await asked(vogt, 2);
    expect(clamped.query.get("offset")).toBe("25");
    await waitFor(() => expect(view.url()).toContain("page=1"));
  });

  it("offers Older only while there is an older page", async () => {
    const vogt = fakeVogt(log([auditRecord()], 20));
    const { container } = audit("/audit?size=25");
    await asked(vogt);
    await rendered(container);

    expect(labelled(container, "Older")).toBeDisabled();
    expect(container.textContent).toContain(
      "every record matching these filters is on this page",
    );
  });
});

// -- a filter that cannot be pushed is not quietly widened -------------------

describe("FR-U21 — a query Vogt refuses is not an empty log", () => {
  it("renders the server's own sentence for a combination it will not answer", async () => {
    const vogt = fakeVogt({
      "GET /audit": refusal(404, "audit.list: no such project 'nope'"),
    });
    const { container } = audit("/audit?project=nope&from=2026-08-01T00:00");

    await waitFor(() => expect(container.querySelector(".vab-outage")).toBeTruthy());
    expect(container.textContent).toContain("audit.list: no such project 'nope'");
    expect(container.textContent).not.toContain(
      "no audited write matches this query",
    );
    void vogt;
  });

  it("makes no query at all when an actor cannot be turned into an id", async () => {
    // The old surface fell back to matching the loaded rows by identity. With
    // a page and a total that describe the query the server answered, that
    // fallback would report "records 1–3 of 4000 matching" over three rows
    // picked out of a query that never mentioned the actor.
    const vogt = fakeVogt({
      ...log([auditRecord()], 4000),
      "GET /actors": refusal(500, "actor.list: the index is corrupt"),
    });
    const { container } = audit("/audit?actor=local:ana");

    await waitFor(() => expect(container.querySelector(".vab-outage")).toBeTruthy());
    expect(container.textContent).toContain("could not be pushed to Vogt");
    expect(vogt.matching("GET /audit")).toHaveLength(0);
  });

  it("makes no query at all when a bound in the link will not parse", async () => {
    const vogt = fakeVogt(log([auditRecord()], 4000));
    const { container } = audit("/audit?from=whenever");

    await waitFor(() => expect(container.querySelector(".vab-outage")).toBeTruthy());
    expect(container.textContent).toContain("From bound could not be read");
    await settle();
    expect(vogt.matching("GET /audit")).toHaveLength(0);
  });
});

// -- the link is the query (FR-U11), and stays it across a live re-read ------

describe("FR-U11 — a pasted link is the query it names", () => {
  it("restores every narrowing and asks Vogt for exactly it", async () => {
    const vogt = fakeVogt(log([auditRecord()], 900));
    audit(
      "/audit?project=alpha&op=work.comment&from=2026-08-01T09:30" +
        "&to=2026-08-08T09:30&size=100&page=2",
    );

    const call = await asked(vogt);
    expect(Object.fromEntries(call.query)).toMatchObject({
      project: "alpha",
      operation: "work.comment",
      since: "2026-08-01T13:30:00.000Z",
      until: "2026-08-08T13:30:00.000Z",
      limit: "100",
      offset: "200",
    });
  });

  it("puts a range chosen here into the URL, so the view can be sent", async () => {
    const vogt = fakeVogt(log([]));
    const view = audit();
    await asked(vogt);

    fireEvent.click(labelled(view.container, "Last 7 days"));

    await waitFor(() => expect(view.url()).toContain("from="));
    expect(view.url()).not.toContain("to=");
  });
});

describe("FR-U10 — a live re-read asks the same question again", () => {
  it("keeps the project, the range and the page across the nudge", async () => {
    const vogt = fakeVogt(log([auditRecord()], 900));
    audit("/audit?project=alpha&from=2026-08-01T09:30&size=25&page=4");
    const before = await asked(vogt);
    await liveStream(vogt);

    vogt.stream.changed();

    const after = await asked(vogt, 2);
    expect(after.query.get("project")).toBe("alpha");
    expect(after.query.get("since")).toBe(before.query.get("since"));
    expect(after.query.get("offset")).toBe("100");
  });
});
