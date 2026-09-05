// #590 part 3: Settings → Operational visibility shows the runtime-pinned
// agent CLIs the engine reports, says when npm has a newer one, and moves the
// pin through the engine's route with the version the operator typed.
import { fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { setToken } from "../api";
import Settings from "../Settings";
import { fakeVogt } from "./harness";

const REPORT = {
  root: "/opt/vogt/agent-clis",
  installer_present: true,
  tools: [
    {
      tool: "claude-code",
      package: "@anthropic-ai/claude-code",
      binary: "claude",
      env_var: "VOGT_CLAUDE_CODE_VERSION",
      baked_version: "2.1.258",
      active_version: "2.1.258",
      source: "image",
      installed_versions: [],
      upstream_latest: "2.1.261",
      update_available: true,
    },
    {
      tool: "codex",
      package: "@openai/codex",
      binary: "codex",
      env_var: "VOGT_CODEX_VERSION",
      baked_version: "0.149.1",
      active_version: "0.149.1",
      source: "image",
      installed_versions: [],
      upstream_latest: "0.149.1",
      update_available: false,
    },
  ],
};

beforeEach(() => {
  localStorage.clear();
  setToken("settings-test-token");
});
afterEach(() => {
  localStorage.clear();
  setToken("");
});

describe("#590 — Settings shows and moves the runtime agent CLI pin", () => {
  it("lists each tool with its active, image and upstream versions, hinting at a newer one", async () => {
    const vogt = fakeVogt({}, { "GET /api/agent-clis": { body: REPORT } });
    render(() => <Settings open={true} onClose={() => {}} />);

    const block = await screen.findByLabelText("Agent CLIs");
    const claude = block.querySelector('[data-agent-cli="claude-code"]')!;
    expect(claude).toHaveTextContent("2.1.258");
    expect(claude).toHaveTextContent("image 2.1.258");
    expect(claude).toHaveTextContent("npm latest 2.1.261");
    expect(claude.querySelector(".agent-cli-update-hint")).toHaveTextContent("newer version available");
    const codex = block.querySelector('[data-agent-cli="codex"]')!;
    expect(codex.querySelector(".agent-cli-update-hint")).toBeNull();
    // Asked with upstream, so the hint is npm's answer and not a guess.
    const asked = vogt.engineCalls.find((c) => c.path === "/api/agent-clis");
    expect(asked?.query.get("upstream")).toBe("true");
  });

  it("moves the pin through the engine with the typed version, defaulting to npm's latest", async () => {
    const moved = {
      ...REPORT,
      tools: [
        { ...REPORT.tools[0], active_version: "2.1.261", source: "runtime", installed_versions: ["2.1.261"], update_available: false },
        REPORT.tools[1],
      ],
    };
    const vogt = fakeVogt(
      {},
      {
        "GET /api/agent-clis": { body: REPORT },
        "POST /api/agent-clis/claude-code": { body: moved },
      },
    );
    render(() => <Settings open={true} onClose={() => {}} />);
    const block = await screen.findByLabelText("Agent CLIs");
    const claude = block.querySelector('[data-agent-cli="claude-code"]')!;
    const update = Array.from(claude.querySelectorAll("button")).find((b) => b.textContent === "Update")!;
    fireEvent.click(update);

    await waitFor(() => {
      const post = vogt.engineCalls.find((c) => c.method === "POST");
      expect(post?.path).toBe("/api/agent-clis/claude-code");
      expect(post?.body).toEqual({ version: "2.1.261" });
    });
    // The rows are re-rendered from the new report; ask the DOM again.
    await waitFor(() => {
      const after = block.querySelector('[data-agent-cli="claude-code"]')!;
      expect(after).toHaveTextContent("2.1.261");
      expect(after).toHaveTextContent("runtime");
    });
    const said = screen.getAllByRole("status").map((el) => el.textContent ?? "");
    expect(said.some((text) => /claude-code is now 2\.1\.261/.test(text))).toBe(true);
  });

  it("says nothing about agent CLIs against an engine that predates them", async () => {
    fakeVogt({}, {});
    render(() => <Settings open={true} onClose={() => {}} />);
    await screen.findByText("Operational visibility");
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(screen.queryByLabelText("Agent CLIs")).toBeNull();
  });
});
