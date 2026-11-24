import { render } from "@solidjs/testing-library";
import { describe, expect, it } from "vitest";

import { SafeSnippet, snippetParts } from "../SafeSnippet";

describe("SafeSnippet", () => {
  it("renders hostile terminal output as text, never executable markup", () => {
    const { container } = render(() => (
      <SafeSnippet text={'<img src=x onerror="globalThis.__xss=1"> needle'} query="needle" />
    ));
    expect(container.querySelector("img")).toBeNull();
    expect(container.textContent).toContain('<img src=x onerror="globalThis.__xss=1">');
    expect(container.querySelector("mark")?.textContent).toBe("needle");
  });

  it("accepts legacy mark wrappers while treating all other tags as text", () => {
    const { container } = render(() => (
      <SafeSnippet text={'before <mark>needle</mark> <svg onload=1>'} />
    ));
    expect(container.querySelector("mark")?.textContent).toBe("needle");
    expect(container.querySelector("svg")).toBeNull();
    expect(container.textContent).toContain("<svg onload=1>");
  });

  it("does not create markup for encoded or malformed payloads", () => {
    const { container } = render(() => (
      <SafeSnippet text={'&lt;script&gt;alert(1)&lt;/script> <mark>needle'} query="needle" />
    ));
    expect(container.querySelector("script")).toBeNull();
    expect(container.textContent).toContain("&lt;script&gt;");
    expect(container.querySelectorAll("mark")).toHaveLength(1);
    expect(snippetParts("<mark>needle</mark>", "")).toEqual([
      { text: "needle", highlighted: true },
    ]);
  });
});
