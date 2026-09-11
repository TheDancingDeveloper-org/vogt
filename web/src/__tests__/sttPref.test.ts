import { afterEach, beforeEach, describe, expect, it } from "vitest";
import {
  getPreferServerStt,
  setPreferServerStt,
  sttVocabularyPrompt,
} from "../sttPref";

beforeEach(() => localStorage.clear());
afterEach(() => localStorage.clear());

describe("sttPref", () => {
  it("prefer-server defaults off and round-trips", () => {
    expect(getPreferServerStt()).toBe(false);
    setPreferServerStt(true);
    expect(getPreferServerStt()).toBe(true);
    setPreferServerStt(false);
    expect(getPreferServerStt()).toBe(false);
  });

  it("builds a vocabulary prompt from project slugs plus fixed terms", () => {
    const prompt = sttVocabularyPrompt(["komodo", "vogt", " ", "rustnzbd"]);
    expect(prompt).toContain("Project names: komodo, vogt, rustnzbd.");
    expect(prompt).toContain("shell");
    expect(prompt).not.toContain(",  ,"); // blank slug dropped
  });

  it("omits the project line when there are no slugs", () => {
    const prompt = sttVocabularyPrompt([]);
    expect(prompt).not.toContain("Project names");
    expect(prompt).toContain("Terms:");
  });
});
