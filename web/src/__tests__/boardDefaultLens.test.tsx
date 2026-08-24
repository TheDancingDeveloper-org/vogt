// Default lens (#352): a saved lens marked default is applied on a bare load
// and reflected in the URL/chips; an explicit (non-empty) URL wins; and the
// "Clear all" escape empties the view so the default cannot trap the reader.

import { fireEvent, waitFor } from "@solidjs/testing-library";
import { afterEach, describe, expect, it } from "vitest";
import Board from "../Board";
import Backlog from "../Backlog";
import { fakeVogt, mountAt, workItem, stopLiveStream } from "./harness";

const BOARD_SAVED = "vogt.boardFilters.v1";
const BOARD_DEFAULT = "vogt.boardDefaultFilter.v1";
const BACKLOG_SAVED = "vogt.vogtSavedFilters.v1";
const BACKLOG_DEFAULT = "vogt.backlogDefaultFilter.v1";

afterEach(() => {
  stopLiveStream();
  window.localStorage.clear();
  window.history.replaceState({}, "", "/");
});

function seedBoardDefault() {
  window.localStorage.setItem(
    BOARD_SAVED,
    JSON.stringify([{ name: "Mine", query: "project=core" }]),
  );
  window.localStorage.setItem(BOARD_DEFAULT, "Mine");
}

function boardVogt() {
  return fakeVogt({
    "GET /work": { body: { items: [workItem({ ref: "WI-1", state: "open" })], total: 1 } },
    "GET /projects": {
      body: { projects: [{ slug: "core", name: "Core" }, { slug: "vogt", name: "Vogt" }] },
    },
  });
}

describe("board default lens (#352)", () => {
  it("applies the default on a bare /board and reflects it in the URL and chips", async () => {
    seedBoardDefault();
    boardVogt();
    const mounted = mountAt("/board", "/board", () => <Board />);
    await waitFor(() => expect(mounted.url()).toContain("project=core"));
    // The applied default is a visible, removable chip — not a hidden narrow.
    const chip = [...mounted.container.querySelectorAll(".board-filter-chip")].find((node) =>
      (node.textContent ?? "").includes("Core"),
    );
    expect(chip).toBeTruthy();
    mounted.unmount();
  });

  it("lets an explicit pasted URL win over the default", async () => {
    seedBoardDefault();
    boardVogt();
    const mounted = mountAt("/board", "/board?project=vogt", () => <Board />);
    await waitFor(() => expect(mounted.url()).toContain("project=vogt"));
    // The default did not override the explicit link.
    expect(mounted.url()).not.toContain("project=core");
    mounted.unmount();
  });

  it("clears to the whole board even with a default set", async () => {
    seedBoardDefault();
    boardVogt();
    const mounted = mountAt("/board", "/board", () => <Board />);
    await waitFor(() => expect(mounted.url()).toContain("project=core"));

    const clear = [...mounted.container.querySelectorAll("button")].find(
      (node) => node.textContent?.trim() === "Clear all",
    );
    expect(clear).toBeTruthy();
    fireEvent.click(clear!);
    await waitFor(() => expect(mounted.url()).not.toContain("project=core"));
    mounted.unmount();
  });
});

describe("backlog default lens (#352 — shared mechanism)", () => {
  it("applies the default on a bare /backlog", async () => {
    window.localStorage.setItem(
      BACKLOG_SAVED,
      JSON.stringify([
        {
          name: "Mine",
          filter: {
            view: "backlog",
            project: "core",
            kinds: [],
            states: [],
            label: "",
            initiative: "",
            actor: "",
            q: "",
            exclude: { projects: [], kinds: [], states: [], labels: [] },
          },
        },
      ]),
    );
    window.localStorage.setItem(BACKLOG_DEFAULT, "Mine");
    fakeVogt();
    const mounted = mountAt("/backlog", "/backlog", () => <Backlog />);
    await waitFor(() => expect(mounted.url()).toContain("project=core"));
    mounted.unmount();
  });
});
