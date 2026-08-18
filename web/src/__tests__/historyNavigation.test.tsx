import { describe, expect, it } from "vitest";
import { fireEvent, waitFor } from "@solidjs/testing-library";

import History from "../History";
import { historyMatchKey, historyResultUrl } from "../historyRoute";
import { fakeVogt, mountAt } from "./harness";

const alpha = {
  id: "session-alpha",
  name: "alpha archive",
  created_at: "2026-08-18T10:00:00Z",
  ended_at: "2026-08-18T10:01:00Z",
  exit_code: 0,
  cwd: "/workspace/alpha",
  command: "pnpm test",
  scrollback_bytes: 80,
};

const beta = {
  ...alpha,
  id: "session-beta",
  name: "beta archive",
  cwd: "/workspace/beta",
};

const alphaResult = {
  session_id: alpha.id,
  session_name: alpha.name,
  created_at: alpha.created_at,
  match_snippet: "alpha says <mark>needle</mark>",
  rank: -2,
};

const betaResult = {
  ...alphaResult,
  session_id: beta.id,
  session_name: beta.name,
  match_snippet: "beta says <mark>needle</mark>",
  rank: -1,
};

function historyFixture() {
  return fakeVogt({}, {
    "GET /api/history/sessions": { body: [alpha, beta] },
    "GET /api/history/search": { body: [alphaResult, betaResult] },
    "GET /api/history/session-alpha": { body: alpha },
    "GET /api/history/session-beta": { body: beta },
    "GET /api/history/session-alpha/log": {
      body: { session_id: alpha.id, text: "alpha says needle", bytes: 17, total_bytes: 17, truncated: false },
    },
    "GET /api/history/session-beta/log": {
      body: { session_id: beta.id, text: "beta says needle", bytes: 16, total_bytes: 16, truncated: false },
    },
  });
}

describe("History result navigation", () => {
  it("restores a qualified shared URL and marks its exact excerpt", async () => {
    const vogt = historyFixture();
    const url = historyResultUrl("needle", betaResult);
    const view = mountAt("/history", url, () => <History />);

    await waitFor(() => expect(
      view.container.querySelector<HTMLInputElement>('input[placeholder="Needle inside scrollback"]')?.value,
    ).toBe("needle"));
    await waitFor(() => expect(view.container.querySelector(".history-detail h3")?.textContent)
      .toBe("beta archive"));

    const qualified = view.container.querySelector<HTMLElement>(
      `[data-history-match="${historyMatchKey(betaResult)}"]`,
    );
    expect(qualified).toHaveAttribute("aria-current", "true");
    expect(qualified?.innerHTML).toContain("<mark>needle</mark>");
    expect(vogt.engineCalls.find((call) => call.path === "/api/history/search")?.query.get("q"))
      .toBe("needle");
  });

  it("gives each distinct result its own URL and updates the detail", async () => {
    historyFixture();
    const view = mountAt("/history", historyResultUrl("needle", alphaResult), () => <History />);

    await waitFor(() => expect(view.container.querySelector(".history-detail h3")?.textContent)
      .toBe("alpha archive"));
    const betaButton = [...view.container.querySelectorAll<HTMLButtonElement>(".history-search-result")]
      .find((button) => button.textContent?.includes("beta archive"));
    expect(betaButton).toBeTruthy();
    fireEvent.click(betaButton!);

    await waitFor(() => expect(view.url()).toBe(historyResultUrl("needle", betaResult)));
    await waitFor(() => expect(view.container.querySelector(".history-detail h3")?.textContent)
      .toBe("beta archive"));
  });

  it("keeps a stale shared excerpt unqualified without losing its session", async () => {
    historyFixture();
    const staleUrl = "/history?q=needle&session=session-beta&match=m00000000";
    const view = mountAt("/history", staleUrl, () => <History />);

    await waitFor(() => expect(view.container.querySelector(".history-detail h3")?.textContent)
      .toBe("beta archive"));
    await waitFor(() => expect(view.container.querySelector(".history-match-panel")).not.toBeNull());
    expect(view.url()).toBe(staleUrl);
    expect(view.container.querySelector('[data-history-match][aria-current="true"]')).toBeNull();
  });
});
