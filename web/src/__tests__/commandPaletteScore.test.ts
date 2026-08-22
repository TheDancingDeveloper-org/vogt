// The palette's ranking (#230). The old filter was an unscored subsequence
// over label OR description in source order, so a description hit could beat a
// name hit and a session literally named for the query never floated to the
// top. These assert the tier order, that a label always beats a description,
// and the two orderings the issue calls out by name.

import { describe, expect, it } from "vitest";
import {
  rankCommands,
  scoreCommand,
  textMatchTier,
  TIER_EXACT,
  TIER_PREFIX,
  TIER_WORD_START,
  TIER_SUBSTRING,
  TIER_SUBSEQUENCE,
  type Scorable,
} from "../commandPaletteScore";

describe("textMatchTier", () => {
  it("ranks exact > prefix > word-start > substring > subsequence", () => {
    expect(textMatchTier("open inbox", "Open Inbox")).toBe(TIER_EXACT);
    expect(textMatchTier("open", "Open Inbox")).toBe(TIER_PREFIX);
    expect(textMatchTier("inbox", "Open Inbox")).toBe(TIER_WORD_START);
    expect(textMatchTier("pen", "Open Inbox")).toBe(TIER_SUBSTRING);
    expect(textMatchTier("oinx", "Open Inbox")).toBe(TIER_SUBSEQUENCE);
    expect(textMatchTier("zzz", "Open Inbox")).toBe(0);
  });

  it("is case-insensitive and treats an empty pattern as no match", () => {
    expect(textMatchTier("OPEN", "open inbox")).toBe(TIER_PREFIX);
    expect(textMatchTier("", "anything")).toBe(0);
  });

  it("finds a word-start after a dash, not only after a space", () => {
    expect(textMatchTier("attention", "needs-attention")).toBe(TIER_WORD_START);
  });
});

describe("scoreCommand — label beats description", () => {
  it("scores even the weakest label match above the strongest description match", () => {
    // A label subsequence (the weakest label tier) must still outrank a
    // description that matches exactly.
    const labelOnly: Scorable = { label: "Open Inbox", description: "" };
    const descExact: Scorable = { label: "Something else", description: "oinx" };
    expect(scoreCommand("oinx", labelOnly)).toBeGreaterThan(
      scoreCommand("oinx", descExact),
    );
  });

  it("falls back to the description when the label does not match", () => {
    const cmd: Scorable = { label: "Resolve Drift...", description: "the drift inbox" };
    expect(scoreCommand("inbox", cmd)).toBeGreaterThan(0);
    // but below any label match band
    expect(scoreCommand("inbox", cmd)).toBeLessThan(
      scoreCommand("inbox", { label: "Open Inbox" }),
    );
  });
});

describe("rankCommands", () => {
  it("surfaces Open Inbox for the query 'inbox' ahead of a description-only hit", () => {
    const commands: Scorable[] = [
      { label: "Resolve Drift...", description: "Opens the drift inbox" },
      { label: "Open Inbox", description: "Attention items" },
    ];
    const ranked = rankCommands("inbox", commands);
    expect(ranked[0]!.label).toBe("Open Inbox");
  });

  it("ranks the session named needs-attention first for the query 'needs'", () => {
    // Sessions are listed before work items, so on any tie sessions win; here
    // the session even wins on tier — a prefix beats the work item's substring.
    const commands: Array<Scorable & { id: string }> = [
      { id: "session-x", label: "needs-attention", description: "Jump to session" },
      { id: "work-1", label: "WI-9 — triage what needs a decision", description: "bug" },
    ];
    const ranked = rankCommands("needs", commands);
    expect(ranked[0]!.id).toBe("session-x");
  });

  it("keeps ties in the order given (sessions before work items)", () => {
    // Both match 'deploy' at a word start — an equal score. The session,
    // listed first, must stay first.
    const commands: Array<Scorable & { id: string }> = [
      { id: "session-deploy", label: "prod-deploy" },
      { id: "work-deploy", label: "WI-3 — deploy the shell" },
    ];
    const ranked = rankCommands("deploy", commands);
    expect(ranked.map((c) => (c as { id: string }).id)).toEqual([
      "session-deploy",
      "work-deploy",
    ]);
  });

  it("returns the whole list unchanged for an empty query", () => {
    const commands: Scorable[] = [{ label: "a" }, { label: "b" }];
    expect(rankCommands("", commands)).toHaveLength(2);
  });

  it("escapes regex metacharacters in the query", () => {
    // A '+' in the query must be matched literally, not as a quantifier.
    expect(rankCommands("c++", [{ label: "build c++ target" }])).toHaveLength(1);
    expect(rankCommands("(", [{ label: "no paren here" }])).toHaveLength(0);
  });
});
