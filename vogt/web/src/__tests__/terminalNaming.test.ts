import { describe, expect, it } from "vitest";
import {
  autoSessionName,
  autoSplitName,
  baseNameFromCwd,
  uniqueSessionName,
  uniqueSplitName,
} from "../terminalNaming";

describe("cwd-derived session naming", () => {
  it("takes the basename of the cwd", () => {
    expect(baseNameFromCwd("/home/me/work/vogt")).toBe("vogt");
    expect(baseNameFromCwd("/home/me/work/vogt/")).toBe("vogt");
    expect(baseNameFromCwd("vogt")).toBe("vogt");
  });

  it("falls back to a stable placeholder for an empty or root cwd", () => {
    expect(baseNameFromCwd(undefined)).toBe("shell");
    expect(baseNameFromCwd(null)).toBe("shell");
    expect(baseNameFromCwd("")).toBe("shell");
    expect(baseNameFromCwd("/")).toBe("shell");
  });

  it("dedupes same-directory shells as vogt, vogt-2, vogt-3", () => {
    expect(uniqueSessionName("vogt", [])).toBe("vogt");
    expect(uniqueSessionName("vogt", ["vogt"])).toBe("vogt-2");
    expect(uniqueSessionName("vogt", ["vogt", "vogt-2"])).toBe("vogt-3");
    // Gaps are skipped over, not filled.
    expect(uniqueSessionName("vogt", ["vogt", "vogt-2", "vogt-4"])).toBe("vogt-3");
  });

  it("auto-names a new session from cwd + existing names in one call", () => {
    expect(autoSessionName("/srv/vogt", ["vogt"])).toBe("vogt-2");
    expect(autoSessionName(undefined, [])).toBe("shell");
  });

  it("names splits as children with the ▸N marker", () => {
    expect(uniqueSplitName("vogt", [])).toBe("vogt ▸2");
    expect(uniqueSplitName("vogt", ["vogt ▸2"])).toBe("vogt ▸3");
    expect(autoSplitName("/srv/vogt", ["vogt", "vogt ▸2"])).toBe("vogt ▸3");
  });
});
