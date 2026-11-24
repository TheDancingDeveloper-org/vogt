import { Route, Router } from "@solidjs/router";
import { fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vitest";
import { api } from "../api";
import FilePreview from "../FilePreview";
import { previewKind } from "../filePreview";

describe("previewKind", () => {
  it("treats html as rendered, images as images, and the rest as text", () => {
    expect(previewKind("notes/page.HTML")).toBe("html");
    expect(previewKind("notes/page.htm")).toBe("html");
    expect(previewKind("assets/logo.PNG")).toBe("image");
    expect(previewKind("src/main.rs")).toBe("text");
    expect(previewKind("Makefile")).toBe("text");
  });
});

describe("FilePreview", () => {
  afterEach(() => vi.restoreAllMocks());

  it("renders an html file in a sandbox with no privileges and toggles to source", async () => {
    vi.spyOn(api, "readFile").mockResolvedValue({
      path: "docs/page.html",
      size: 42,
      content: "<h1>Hello</h1><script>alert(1)</script>",
      content_base64: null,
      is_binary: false,
    });
    const created: string[] = [];
    const create = vi.spyOn(URL, "createObjectURL").mockImplementation((blob) => {
      created.push(blob instanceof Blob ? blob.type : "");
      return "blob:preview";
    });
    vi.spyOn(URL, "revokeObjectURL").mockImplementation(() => undefined);

    render(() => (
      <Router>
        <Route path="*" component={() => <FilePreview path="docs/page.html" />} />
      </Router>
    ));

    const frame = await screen.findByTitle("Rendered docs/page.html");
    expect(frame.tagName).toBe("IFRAME");
    expect(frame).toHaveAttribute("sandbox", "");
    expect(frame.getAttribute("sandbox")).not.toMatch(/allow-scripts|allow-same-origin/);
    expect(frame).toHaveAttribute("src", "blob:preview");
    expect(create).toHaveBeenCalled();
    expect(created).toEqual(["text/html"]);
    expect(screen.getByRole("button", { name: "Rendered" })).toHaveAttribute("aria-pressed", "true");

    await fireEvent.click(screen.getByRole("button", { name: "Source" }));
    expect(await screen.findByText(/<h1>Hello<\/h1>/)).toBeVisible();
    expect(screen.queryByTitle("Rendered docs/page.html")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Source" })).toHaveAttribute("aria-pressed", "true");
  });

  it("shows a text file as source with no iframe", async () => {
    vi.spyOn(api, "readFile").mockResolvedValue({
      path: "README.md",
      size: 5,
      content: "# Vogt",
      content_base64: null,
      is_binary: false,
    });
    render(() => (
      <Router>
        <Route path="*" component={() => <FilePreview path="README.md" />} />
      </Router>
    ));
    expect(await screen.findByText("# Vogt")).toBeVisible();
    expect(screen.queryByRole("group", { name: "HTML view" })).not.toBeInTheDocument();
    expect(document.querySelector("iframe")).toBeNull();
  });

  it("reports a read failure", async () => {
    vi.spyOn(api, "readFile").mockRejectedValue(new Error("HTTP 403: no"));
    const onError = vi.fn();
    render(() => (
      <Router>
        <Route path="*" component={() => <FilePreview path="secret.html" onError={onError} />} />
      </Router>
    ));
    expect(await screen.findByRole("alert")).toHaveTextContent("HTTP 403: no");
    await waitFor(() => expect(onError).toHaveBeenCalled());
  });
});
