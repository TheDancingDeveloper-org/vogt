import { describe, expect, it, vi } from "vitest";

// WI-1413: every terminal mount read the full session list for its work-item
// badge, so warming ~70 terminals fired ~70 lists inside a second.
const listSessions = vi.fn((_params: unknown) => Promise.resolve({ sessions: [{ engine_session_id: "eng-1", work_item: "WI-1" }] }));
vi.mock("../vogtApi", () => ({ listSessions }));

describe("terminal work-item links", () => {
  it("shares one session.list read across terminals mounting together", async () => {
    const { workItemLinkFor } = await import("../Terminal");
    await Promise.all(Array.from({ length: 50 }, () => workItemLinkFor("eng-1")));
    expect(listSessions).toHaveBeenCalledTimes(1);
  });

  it("asks for live sessions only, not every stopped one (WI-4532)", async () => {
    const { workItemLinkFor, forgetWorkItemLinks } = await import("../Terminal");
    forgetWorkItemLinks();
    listSessions.mockClear();
    await workItemLinkFor("eng-1");
    expect(listSessions).toHaveBeenCalledTimes(1);
    expect(listSessions.mock.calls[0]![0]).not.toHaveProperty("include_stopped");
  });

  it("reads two columns off the declared table, not whole full rows (WI-4543)", async () => {
    const { workItemLinkFor, forgetWorkItemLinks } = await import("../Terminal");
    forgetWorkItemLinks();
    listSessions.mockClear();
    expect(await workItemLinkFor("eng-1")).toBe("WI-1");
    expect(listSessions.mock.calls[0]![0]).toMatchObject({
      mode: "full",
      starters_only: true,
      fields: ["engine_session_id", "work_item"],
    });
  });
});
