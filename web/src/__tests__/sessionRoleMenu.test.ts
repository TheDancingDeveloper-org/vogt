import { describe, expect, it } from "vitest";
import type { SessionSummary } from "../api";
import { sessionRoleAction } from "../sessionRoleMenu";
import { sortSessionsOversightFirst } from "../sessionRowModel";

const session = (over: Partial<SessionSummary> = {}): SessionSummary => ({
  id: "s1",
  name: "worker",
  cwd: "/workspace",
  activity: "idle",
  exit_code: null,
  scrollback_bytes: 0,
  created_at: "2026-10-08T00:00:00Z",
  ...over,
}) as SessionSummary;

describe("sessionRoleAction (WI-1091)", () => {
  it("offers Make oversight on a worker", () => {
    expect(sessionRoleAction(session())).toMatchObject({
      role: "oversight",
      label: "Make oversight",
    });
    expect(sessionRoleAction(session({ role: "worker" }))?.label).toBe("Make oversight");
  });

  it("offers Remove oversight on an oversight session", () => {
    expect(sessionRoleAction(session({ role: "oversight", keep_awake: true }))).toMatchObject({
      role: "worker",
      label: "Remove oversight",
    });
  });

  it("keeps the row on a hibernated session, whose role the engine still holds", () => {
    expect(
      sessionRoleAction(session({ role: "oversight", activity: "hibernated" }))?.label,
    ).toBe("Remove oversight");
  });

  it("offers nothing on an exited session or none at all", () => {
    expect(sessionRoleAction(session({ exit_code: 0 }))).toBeNull();
    expect(sessionRoleAction(session({ role: "oversight", exit_code: 1 }))).toBeNull();
    expect(sessionRoleAction(null)).toBeNull();
    expect(sessionRoleAction(undefined)).toBeNull();
  });
});

describe("sortSessionsOversightFirst (WI-1091)", () => {
  it("floats oversight above attention order and drops it back once removed", () => {
    const waiting = session({ id: "w", name: "waiting", activity: "waiting-for-input" });
    const idle = session({ id: "o", name: "overseer", activity: "idle" });
    expect(sortSessionsOversightFirst([waiting, idle]).map((s) => s.id)).toEqual(["w", "o"]);
    const promoted = { ...idle, role: "oversight" as const };
    expect(sortSessionsOversightFirst([waiting, promoted]).map((s) => s.id)).toEqual(["o", "w"]);
  });
});
