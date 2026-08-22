// #247, bullet 8: the preset editor confirms before a destructive delete
// through the shared accessible confirm, says where presets are stored, and
// does not throw away a half-typed preset on Escape.
import { fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import TemplateEditor from "../TemplateEditor";
import { addCustomTemplate, getCustomTemplates } from "../customTemplates";
import type { SessionTemplate } from "../api";

const PRESET: SessionTemplate = {
  name: "Rust shell",
  description: "cargo test loop",
  command: ["cargo", "test"],
  cwd: null,
  env: [],
  default_name: null,
  match_repo_names: [],
  match_path_prefixes: [],
  tags: [],
};

beforeEach(() => {
  localStorage.clear();
  addCustomTemplate(PRESET);
});
afterEach(() => {
  localStorage.clear();
  vi.restoreAllMocks();
});

describe("#247 — preset editor polish", () => {
  it("says presets are stored in this browser", async () => {
    render(() => <TemplateEditor open={true} onClose={() => {}} />);
    expect(await screen.findByText(/stored in this browser/i)).toBeVisible();
  });

  it("asks the shared confirm before delete and honours a No", async () => {
    const confirmAction = vi.fn().mockResolvedValue(false);
    render(() => (
      <TemplateEditor open={true} onClose={() => {}} confirmAction={confirmAction} />
    ));

    await fireEvent.click(await screen.findByRole("button", { name: "Delete" }));
    await waitFor(() => expect(confirmAction).toHaveBeenCalledTimes(1));
    // No means the preset stays.
    expect(getCustomTemplates()).toHaveLength(1);
    expect(screen.getByText("Rust shell")).toBeVisible();
  });

  it("deletes when the shared confirm returns Yes", async () => {
    const confirmAction = vi.fn().mockResolvedValue(true);
    render(() => (
      <TemplateEditor open={true} onClose={() => {}} confirmAction={confirmAction} />
    ));

    await fireEvent.click(await screen.findByRole("button", { name: "Delete" }));
    await waitFor(() => expect(getCustomTemplates()).toHaveLength(0));
  });

  it("does not close the dialog on Escape while a preset is being edited", async () => {
    const onClose = vi.fn();
    render(() => <TemplateEditor open={true} onClose={onClose} />);

    // Enter the form for a new preset, then press Escape.
    await fireEvent.click(await screen.findByRole("button", { name: "+ New Preset" }));
    expect(screen.getByPlaceholderText("Rust service shell")).toBeVisible();
    await fireEvent.keyDown(document, { key: "Escape" });
    // The half-typed form is not discarded by Escape.
    expect(onClose).not.toHaveBeenCalled();
  });
});
