// #247, bullet 10: Settings' transient status messages are announced — the
// divs carry role="status" so a screen reader hears "Saved profile …" without
// the reader hunting for it.
import { fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import Settings from "../Settings";
import { setToken } from "../api";

beforeEach(() => {
  localStorage.clear();
  setToken("settings-test-token");
});
afterEach(() => {
  localStorage.clear();
  setToken("");
});

describe("#247 — Settings status divs are announced", () => {
  it("marks the saved-profile confirmation with role=status", async () => {
    render(() => <Settings open={true} onClose={() => {}} />);

    await fireEvent.input(
      await screen.findByPlaceholderText("Profile name"),
      { target: { value: "work" } },
    );
    await fireEvent.click(screen.getByRole("button", { name: "Save profile" }));

    const status = await waitFor(() => {
      const node = screen
        .getAllByRole("status")
        .find((el) => el.textContent?.includes('Saved profile "work"'));
      expect(node).toBeTruthy();
      return node!;
    });
    expect(status).toHaveTextContent('Saved profile "work"');
  });
});
