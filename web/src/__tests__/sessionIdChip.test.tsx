import { afterEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, waitFor } from "@solidjs/testing-library";

import SessionIdChip, { elideMiddle } from "../SessionIdChip";

afterEach(() => vi.restoreAllMocks());

describe("terminal toolbar session id", () => {
  it("shows the ses_ id and copies it on click", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, "clipboard", { value: { writeText }, configurable: true });
    const { container } = render(() => (
      <SessionIdChip engineId="eng-uuid" lookup={async () => "ses_01ABCDEFGHJKMNPQRSTVWXYZ12"} />
    ));
    const btn = await waitFor(() => {
      const el = container.querySelector<HTMLButtonElement>(".session-id-chip");
      expect(el?.title).toContain("ses_01ABCDEFGHJKMNPQRSTVWXYZ12");
      return el!;
    });
    expect(btn.textContent).toContain("…");
    fireEvent.click(btn);
    await waitFor(() => expect(btn.textContent).toContain("Copied"));
    expect(writeText).toHaveBeenCalledWith("ses_01ABCDEFGHJKMNPQRSTVWXYZ12");
  });

  it("falls back to the engine UUID when unlinked", async () => {
    Object.defineProperty(navigator, "clipboard", { value: undefined, configurable: true });
    document.execCommand = vi.fn().mockReturnValue(true);
    const { container } = render(() => <SessionIdChip engineId="eng-1" lookup={async () => null} />);
    const btn = container.querySelector<HTMLButtonElement>(".session-id-chip")!;
    expect(btn.title).toContain("eng-1");
    fireEvent.click(btn);
    await waitFor(() => expect(btn.textContent).toContain("Copied"));
    expect(document.execCommand).toHaveBeenCalledWith("copy");
  });

  it("elides the middle only when long", () => {
    expect(elideMiddle("short")).toBe("short");
    expect(elideMiddle("a".repeat(40))).toMatch(/^a+…a+$/);
  });
});
