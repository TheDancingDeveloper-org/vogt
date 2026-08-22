// #247, bullet 11: a configured GUI stream URL is a link the reader can open,
// not bare text.
import { render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vitest";
import GuiTab from "../Gui";
import { api } from "../api";

afterEach(() => vi.restoreAllMocks());

describe("#247 — the GUI stream URL is a link", () => {
  it("renders the configured stream URL as an anchor", async () => {
    vi.spyOn(api, "guiProcesses").mockResolvedValue([]);
    const url = "https://gui.example.test/stream";
    render(() => <GuiTab streamUrl={url} />);

    const link = await waitFor(() => {
      const anchor = screen.getByRole("link", { name: url });
      expect(anchor).toBeTruthy();
      return anchor as HTMLAnchorElement;
    });
    expect(link.getAttribute("href")).toBe(url);
  });
});
