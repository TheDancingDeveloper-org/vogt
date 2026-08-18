// FR-T13: the repair pass between a recognizer and the composer.
//
// These are the utterances from `docs/VOICE_POC.md` §2, in the forms a
// recognizer actually returns them. They are the half of FR-T5's promised
// validation pass that does not need a microphone — and the half that will
// still be true next week, since a person speaking into a device once proves
// only that it worked once.
import { describe, expect, it } from "vitest";

import { describeRepairs, repairUtterance } from "../voiceRepair";

const PROJECTS = ["rustnzbd", "rusttorrent", "vogt", "cadastre", "mydevenv2"];

describe("work item refs", () => {
  it("repairs the shapes a recognizer writes WI-7 as", () => {
    // Every one of these was a plausible transcription of the same sentence.
    for (const heard of [
      "what is W I 7",
      "what is WI 7",
      "what is wi-7",
      "what is issue 7",
      "what is item 7",
      "what is work item 7",
      "what is ticket 7",
    ]) {
      expect(repairUtterance(heard, PROJECTS).text).toBe("what is WI-7");
    }
  });

  it("repairs a number that arrived as a word", () => {
    // U3 spoken aloud. "issue twelve" is the natural way to say it and
    // `WI-twelve` matches nothing at the far end.
    expect(repairUtterance("can you work on issue twelve").text).toBe(
      "can you work on WI-12",
    );
    expect(repairUtterance("close issue twenty one").text).toBe("close WI-21");
  });

  it("leaves a phrase that only looks like a ref alone", () => {
    // The failure a greedier pattern would cause: an ordinary sentence
    // silently becomes a reference to an item that may well exist.
    for (const heard of [
      "open the issue tracker",
      "what is the bug about",
      "any issues at all",
    ]) {
      expect(repairUtterance(heard, PROJECTS).text).toBe(heard);
    }
  });

  it("says what it changed", () => {
    const { repairs } = repairUtterance("work on issue twelve");
    expect(repairs).toEqual([
      { heard: "issue twelve", repaired: "WI-12", kind: "work-item" },
    ]);
    expect(describeRepairs(repairs)).toContain("WI-12");
  });
});

describe("project slugs", () => {
  it("rejoins a slug the recognizer split into words", () => {
    // U2, and the reason FR-T5 said voice was unproven: `rustnzbd` is not a
    // word, so a recognizer returns the letters it heard.
    for (const heard of [
      "what open issues are there for rust nzbd",
      "what open issues are there for rust nzb d",
      "what open issues are there for Rust NZBD",
    ]) {
      expect(repairUtterance(heard, PROJECTS).text).toBe(
        "what open issues are there for rustnzbd",
      );
    }
  });

  it("repairs a slug that was heard slightly wrong", () => {
    expect(repairUtterance("anything open for rustnzbdee", PROJECTS).text).toBe(
      "anything open for rustnzbd",
    );
    expect(repairUtterance("what about cadaster", PROJECTS).text).toBe(
      "what about cadastre",
    );
  });

  it("does not choose between equally plausible projects", () => {
    const result = repairUtterance("what about alphx", ["alpha", "alphi"]);
    expect(result.text).toBe("what about alphx");
    expect(result.repairs).toEqual([]);
  });

  it("keeps the punctuation that belonged to the sentence", () => {
    expect(repairUtterance("how is rust nzbd?", PROJECTS).text).toBe(
      "how is rustnzbd?",
    );
  });

  it("does not invent a project out of an ordinary word", () => {
    // The wrong repair is the dangerous one: it is confidently wrong, and it
    // is what gets sent. A short slug is where that is most likely, so
    // closeness alone is not enough.
    for (const heard of [
      "what should I work on",
      "is the vote in yet",
      "read the last four lines",
      "got any coffee",
    ]) {
      expect(repairUtterance(heard, [...PROJECTS, "web", "api"]).text).toBe(heard);
    }
  });

  it("changes nothing when the instance has no projects", () => {
    // The repair is against what exists. With nothing to match, it must be
    // a no-op rather than a fuzzy match against a list in the source.
    const heard = "what open issues are there for rust nzbd";
    expect(repairUtterance(heard, []).text).toBe(heard);
  });

  it("leaves a slug that was already correct alone", () => {
    const { text, repairs } = repairUtterance(
      "what open issues are there for rustnzbd",
      PROJECTS,
    );
    expect(text).toBe("what open issues are there for rustnzbd");
    expect(repairs).toEqual([]);
  });
});

describe("the five utterances", () => {
  it("survives U1 through U5", () => {
    // U1 and U5 have nothing to repair, and that is the assertion: a pass
    // that only ever changes things is one that will eventually change the
    // wrong one.
    expect(repairUtterance("are there any notifications?", PROJECTS).text).toBe(
      "are there any notifications?",
    );
    expect(
      repairUtterance("what open issues are there for rust nzb d?", PROJECTS).text,
    ).toBe("what open issues are there for rustnzbd?");
    expect(
      repairUtterance("can you work on issue twelve for rust nzb d", PROJECTS).text,
    ).toBe("can you work on WI-12 for rustnzbd");
    expect(
      repairUtterance(
        "research the best place to buy risotto in wollongong using gpt 5.6 medium",
        PROJECTS,
      ).text,
    ).toBe(
      "research the best place to buy risotto in wollongong using gpt 5.6 medium",
    );
    expect(
      repairUtterance("what is the terminal for rust nzb d doing", PROJECTS).text,
    ).toBe("what is the terminal for rustnzbd doing");
  });
});
