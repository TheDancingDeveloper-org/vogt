import { describe, expect, it } from "vitest";

import {
  historyMatchKey,
  historyResultUrl,
  historyUrl,
  readHistoryRoute,
} from "../historyRoute";

const RESULT = {
  session_id: "session-alpha",
  match_snippet: "build <mark>needle</mark> in alpha",
};

describe("qualified History routes", () => {
  it("round-trips the query, archived session and deterministic match", () => {
    const url = historyResultUrl("needle & context", RESULT);
    expect(url).toContain("q=needle+%26+context");

    const route = readHistoryRoute(url.slice(url.indexOf("?")));
    expect(route).toEqual({
      hasState: true,
      query: "needle & context",
      sessionId: "session-alpha",
      matchKey: historyMatchKey(RESULT),
      focusSearch: false,
    });
  });

  it("keeps unrelated query state when History updates its owned keys", () => {
    expect(historyUrl({ query: "needle" }, "?from=board&session=old&match=m12345678"))
      .toBe("/history?from=board&session=old&match=m12345678&q=needle");
  });

  it("fails unsafe session and match values closed without dropping the query", () => {
    expect(readHistoryRoute("?q=needle&session=../../etc&match=javascript:alert(1)"))
      .toMatchObject({
        hasState: true,
        query: "needle",
        sessionId: null,
        matchKey: null,
      });
  });

  it("uses the excerpt as part of the match identity", () => {
    expect(historyMatchKey(RESULT)).not.toBe(historyMatchKey({
      ...RESULT,
      match_snippet: "a different <mark>needle</mark>",
    }));
  });
});
