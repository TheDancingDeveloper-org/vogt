import { describe, expect, it, vi } from "vitest";

import { handleNativeBack } from "../nativeBack";

const deps = (over: Partial<Parameters<typeof handleNativeBack>[0]>) => ({
  dialogOpen: () => false,
  dispatchEscape: () => false,
  canGoBack: true,
  goBack: vi.fn(),
  exitApp: vi.fn(),
  ...over,
});

describe("the phone's back button", () => {
  it("closes an open dialog and goes no further", () => {
    const d = deps({ dialogOpen: () => true, dispatchEscape: () => true });
    expect(handleNativeBack(d)).toBe("dialog");
    expect(d.goBack).not.toHaveBeenCalled();
    expect(d.exitApp).not.toHaveBeenCalled();
  });

  it("stops at a lighter menu that consumed Escape", () => {
    const d = deps({ dispatchEscape: () => true });
    expect(handleNativeBack(d)).toBe("escape");
    expect(d.goBack).not.toHaveBeenCalled();
  });

  it("walks the router's history when nothing is open", () => {
    const d = deps({});
    expect(handleNativeBack(d)).toBe("history");
    expect(d.goBack).toHaveBeenCalledTimes(1);
    expect(d.exitApp).not.toHaveBeenCalled();
  });

  it("leaves the app only when there is nothing left to go back to", () => {
    const d = deps({ canGoBack: false });
    expect(handleNativeBack(d)).toBe("exit");
    expect(d.exitApp).toHaveBeenCalledTimes(1);
  });
});
