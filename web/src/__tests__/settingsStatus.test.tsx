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

  it("shows the public product version and provenance in Settings", () => {
    render(() => (
      <Settings
        open={true}
        onClose={() => {}}
        publicConfig={{
          version: "0.2.2",
          product_version: "0.2.2",
          source_ref: "v0.2.2",
          source_sha: "a".repeat(40),
          release_url: "https://example.test/release",
          gui_stream_url: null,
        }}
      />
    ));
    const card = screen.getByLabelText("Product version");
    expect(card).toHaveTextContent("Vogt v0.2.2");
    expect(card).toHaveTextContent("v0.2.2@aaaaaaaaaaaa");
    expect(card.querySelector('a[href="https://example.test/release"]')).toBeTruthy();
  });
});
