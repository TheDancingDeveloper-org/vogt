import { describe, expect, it, vi } from "vitest";

// WI-1413: every terminal mount read the full session list for its work-item
// badge, so warming ~70 terminals fired ~70 lists inside a second.
const listSessions = vi.fn(() => Promise.resolve({ sessions: [] }));
vi.mock("../vogtApi", () => ({ listSessions }));

describe("terminal work-item links", () => {
  it("shares one session.list read across terminals mounting together", async () => {
    const { workItemLinks } = await import("../Terminal");
    await Promise.all(Array.from({ length: 50 }, () => workItemLinks()));
    expect(listSessions).toHaveBeenCalledTimes(1);
  });
});
