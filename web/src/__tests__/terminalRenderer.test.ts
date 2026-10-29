import { afterEach, describe, expect, it } from "vitest";
import {
  TERMINAL_RENDERER_STORAGE_KEY,
  attachWebglRenderer,
  readTerminalRenderer,
  shouldUseWebglRenderer,
  writeTerminalRenderer,
  type WebglTerminal,
  type WebglTerminalAddon,
} from "../terminalRenderer";

afterEach(() => {
  localStorage.removeItem(TERMINAL_RENDERER_STORAGE_KEY);
});

describe("terminal renderer preference", () => {
  it("defaults to webgl and persists an explicit DOM choice", () => {
    expect(readTerminalRenderer()).toBe("webgl");
    writeTerminalRenderer("dom");
    expect(readTerminalRenderer()).toBe("dom");
    expect(shouldUseWebglRenderer("dom", true)).toBe(false);
    expect(shouldUseWebglRenderer("webgl", false)).toBe(false);
    expect(shouldUseWebglRenderer("webgl", true)).toBe(true);
  });

  it("does not load the addon when DOM is forced", async () => {
    let loaded = 0;
    const dispose = await attachWebglRenderer(
      { loadAddon() {} },
      {
        preference: "dom",
        webglAvailable: true,
        loadAddon: async () => {
          loaded += 1;
          return class {
            activate() {}
            dispose() {}
            onContextLoss() {}
          };
        },
      },
    );
    expect(dispose).toBeNull();
    expect(loaded).toBe(0);
  });

  it("disposes the addon and falls back when the context is lost", async () => {
    const reasons: string[] = [];
    let loss: (() => void) | null = null;
    let disposed = 0;
    const term: WebglTerminal = { loadAddon() {} };
    class Addon implements WebglTerminalAddon {
      activate() {}
      dispose() {
        disposed += 1;
      }
      onContextLoss(listener: () => void) {
        loss = listener;
      }
    }
    const dispose = await attachWebglRenderer(term, {
      preference: "webgl",
      webglAvailable: true,
      loadAddon: async () => Addon,
      onFallback: (reason) => reasons.push(reason),
    });
    expect(dispose).not.toBeNull();
    loss!();
    expect(reasons).toEqual(["context-loss"]);
    expect(disposed).toBe(1);
    dispose!();
    expect(disposed).toBe(1);
  });
});
