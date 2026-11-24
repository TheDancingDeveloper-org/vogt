// Hibernating and waking from the GUI goes through the core first — it is
// audited, and only the core can mint a woken session a new token — and
// falls back to the engine's own route only when there is no core.

import { beforeEach, describe, expect, it, vi } from "vitest";

const core = vi.hoisted(() => ({
  wake: vi.fn(),
  hibernate: vi.fn(),
  keepAwake: vi.fn(),
  setRole: vi.fn(),
  bindWork: vi.fn(),
}));
const engine = vi.hoisted(() => ({
  wakeSession: vi.fn(),
  hibernateSession: vi.fn(),
  keepSessionAwake: vi.fn(),
  setSessionRole: vi.fn(),
  setSessionWorkItem: vi.fn(),
}));

vi.mock("../vogtApi", () => {
  class VogtUnavailable extends Error {
    constructor(
      public readonly status: number,
      message: string,
    ) {
      super(message);
    }
  }
  return {
    VogtUnavailable,
    wakeSessionInVogt: core.wake,
    hibernateSessionInVogt: core.hibernate,
    keepSessionAwakeInVogt: core.keepAwake,
    setSessionRoleInVogt: core.setRole,
    bindSessionWorkInVogt: core.bindWork,
  };
});
vi.mock("../api", () => ({ api: engine }));

import { VogtUnavailable } from "../vogtApi";
import {
  bindSessionWork,
  hibernateSession,
  setKeepAwake,
  setSessionRole,
  wakeSession,
} from "../sessionHibernation";

describe("session hibernation from the GUI", () => {
  beforeEach(() => {
    for (const fn of [...Object.values(core), ...Object.values(engine)]) fn.mockReset();
  });

  it("wakes through the core, by the engine id, and not the engine too", async () => {
    core.wake.mockResolvedValue({ session: {} });
    await wakeSession("uuid-1");
    expect(core.wake).toHaveBeenCalledWith("uuid-1", "woken from the GUI");
    expect(engine.wakeSession).not.toHaveBeenCalled();
  });

  it("falls back to the engine only when there is no core", async () => {
    core.hibernate.mockRejectedValue(new VogtUnavailable(503, "no core"));
    engine.hibernateSession.mockResolvedValue({});
    await hibernateSession("uuid-2");
    expect(engine.hibernateSession).toHaveBeenCalledWith(
      "uuid-2",
      "hibernated from the GUI to free its memory",
    );
  });

  it("does not paper over a refusal from the core", async () => {
    core.keepAwake.mockRejectedValue(new Error("the engine does not know a conversation"));
    await expect(setKeepAwake("uuid-3", true)).rejects.toThrow("conversation");
    expect(engine.keepSessionAwake).not.toHaveBeenCalled();
  });

  it("nominates oversight through the core, falling back to the engine (WI-957)", async () => {
    core.setRole.mockResolvedValue({ session: {} });
    await setSessionRole("uuid-4", "oversight");
    expect(core.setRole).toHaveBeenCalledWith("uuid-4", "oversight", "nominated as oversight from the GUI");
    expect(engine.setSessionRole).not.toHaveBeenCalled();

    core.setRole.mockRejectedValue(new VogtUnavailable(503, "no core"));
    engine.setSessionRole.mockResolvedValue({});
    await setSessionRole("uuid-4", "worker");
    expect(engine.setSessionRole).toHaveBeenCalledWith("uuid-4", "worker");
  });

  it("binds and unbinds through the core, falling back to the engine label (WI-998)", async () => {
    core.bindWork.mockResolvedValue({ session: {}, engine_label: "written" });
    await bindSessionWork("uuid-5", "WI-7");
    expect(core.bindWork).toHaveBeenCalledWith("uuid-5", "WI-7", "bound to WI-7 from the GUI");
    expect(engine.setSessionWorkItem).not.toHaveBeenCalled();

    core.bindWork.mockRejectedValue(new VogtUnavailable(503, "no core"));
    engine.setSessionWorkItem.mockResolvedValue({});
    await bindSessionWork("uuid-5", null);
    expect(engine.setSessionWorkItem).toHaveBeenCalledWith("uuid-5", null);
  });
});
