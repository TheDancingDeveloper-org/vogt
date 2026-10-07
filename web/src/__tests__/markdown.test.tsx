// The sanitising Markdown renderer.
//
// Two things are asserted here and the second is the one that matters: the
// allow-listed constructs become real nodes, and nothing an attacker writes
// becomes anything a browser will run. The renderer builds nodes and never
// injects HTML, so the injection assertions prove the property directly —
// there is no `<script>` node, and a `javascript:` href never reaches an
// anchor.

import { describe, expect, it } from "vitest";
import { render } from "@solidjs/testing-library";
import { renderMarkdown, safeHref, filePathRef, setFileViewOpener } from "../markdown";

function mount(source: string): HTMLElement {
  const { container } = render(() => <div>{renderMarkdown(source)}</div>);
  return container;
}

describe("renderMarkdown — the allow-list renders", () => {
  it("turns headings into real heading elements", () => {
    const c = mount("# Title\n\n## Subtitle");
    expect(c.querySelector("h1")?.textContent).toBe("Title");
    expect(c.querySelector("h2")?.textContent).toBe("Subtitle");
    // The literal `#` is gone, not shown.
    expect(c.textContent).not.toContain("#");
  });

  it("turns dashes into a list", () => {
    const c = mount("- one\n- two\n- three");
    const items = c.querySelectorAll("ul > li");
    expect(items).toHaveLength(3);
    expect(items[0]?.textContent).toBe("one");
    expect(c.textContent).not.toContain("- one");
  });

  it("turns a numbered list into an ordered list", () => {
    const c = mount("1. first\n2. second");
    expect(c.querySelectorAll("ol > li")).toHaveLength(2);
  });

  it("renders a link with a safe href and nothing else", () => {
    const c = mount("see [the docs](https://example.com/guide)");
    const link = c.querySelector("a");
    expect(link?.getAttribute("href")).toBe("https://example.com/guide");
    expect(link?.textContent).toBe("the docs");
    expect(link?.getAttribute("rel")).toContain("noopener");
  });

  it("renders inline code and fenced code blocks", () => {
    const c = mount("run `pnpm test` first\n\n```ts\nconst x = 1;\n```");
    expect(c.querySelector("p code")?.textContent).toBe("pnpm test");
    const pre = c.querySelector("pre code");
    expect(pre?.textContent).toBe("const x = 1;");
    // The backticks are structure, not text.
    expect(c.textContent).not.toContain("`");
    expect(c.textContent).not.toContain("```");
  });
});

describe("renderMarkdown — the injection is neutralised", () => {
  it("renders a raw <script> tag as inert text, never a node", () => {
    const c = mount("hello <script>alert(1)</script> world");
    // No script element was created — the tag is text.
    expect(c.querySelector("script")).toBeNull();
    expect(c.textContent).toContain("<script>alert(1)</script>");
  });

  it("refuses a javascript: href — no anchor carries it", () => {
    const c = mount("[click me](javascript:alert(1))");
    // The words survive; the link does not.
    expect(c.textContent).toContain("click me");
    const anchors = Array.from(c.querySelectorAll("a"));
    expect(anchors.every((a) => !(a.getAttribute("href") ?? "").includes("javascript"))).toBe(
      true,
    );
    // Rendered as the inert, blocked span instead.
    expect(c.querySelector(".md-link--blocked")?.textContent).toBe("click me");
  });

  it("refuses a data: href and an entity-disguised javascript: scheme", () => {
    const c = mount(
      "[a](data:text/html,<script>1</script>) and [b](&#106;avascript:alert(1))",
    );
    for (const a of Array.from(c.querySelectorAll("a"))) {
      const href = a.getAttribute("href") ?? "";
      expect(href).not.toContain("data:");
      expect(href.toLowerCase()).not.toContain("javascript");
    }
    // And still no script node anywhere.
    expect(c.querySelector("script")).toBeNull();
  });

  it("does not let a fenced block smuggle in HTML", () => {
    const c = mount("```html\n<script>alert(1)</script>\n```");
    expect(c.querySelector("script")).toBeNull();
    expect(c.querySelector("pre code")?.textContent).toBe("<script>alert(1)</script>");
  });
});

describe("safeHref — the href allow-list", () => {
  it("keeps http, https, mailto and relative references", () => {
    expect(safeHref("https://x.test/a")).toBe("https://x.test/a");
    expect(safeHref("http://x.test")).toBe("http://x.test");
    expect(safeHref("mailto:a@x.test")).toBe("mailto:a@x.test");
    expect(safeHref("/local/path")).toBe("/local/path");
    expect(safeHref("#anchor")).toBe("#anchor");
    expect(safeHref("relative/page")).toBe("relative/page");
  });

  it("refuses javascript, data, vbscript and unknown schemes", () => {
    expect(safeHref("javascript:alert(1)")).toBeNull();
    expect(safeHref("data:text/html,x")).toBeNull();
    expect(safeHref("vbscript:msgbox(1)")).toBeNull();
    expect(safeHref("file:///etc/passwd")).toBeNull();
    // Entity-encoded and whitespace-split disguises are caught too.
    expect(safeHref("&#106;avascript:alert(1)")).toBeNull();
    expect(safeHref("java\tscript:alert(1)")).toBeNull();
    expect(safeHref("java\u0000script:alert(1)")).toBeNull();
    expect(safeHref("java\nscript:alert(1)")).toBeNull();
  });

  it("strips control characters, not hyphens, before judging the scheme", () => {
    // A hyphen is an ordinary character: `h-ttps:` is an unknown scheme.
    expect(safeHref("h-ttps://x.test")).toBeNull();
    expect(safeHref("my-page/sub-page")).toBe("my-page/sub-page");
  });
});

describe("file paths", () => {
  it("recognises real paths and refuses everything else", () => {
    expect(filePathRef("/home/sprooty/Working/notes.md")).toBe("/home/sprooty/Working/notes.md");
    expect(filePathRef("file:///home/sprooty/Working/notes.md")).toBe("/home/sprooty/Working/notes.md");
    expect(filePathRef("./docs/ARCHITECTURE.md")).toBe("docs/ARCHITECTURE.md");
    expect(filePathRef("src/vogt/app.py")).toBe("src/vogt/app.py");
    expect(filePathRef("/etc")).toBeNull();
    expect(filePathRef("https://example.com/a/b")).toBeNull();
    expect(filePathRef("just-a-word")).toBeNull();
  });

  it("makes a bare path clickable and opens the viewer", () => {
    const opened: string[] = [];
    setFileViewOpener((path: string) => opened.push(path));
    const c = render(() => <div>{renderMarkdown("see /home/sprooty/Working/notes.md please")}</div>);
    const link = c.container.querySelector("button.md-file-link") as HTMLButtonElement;
    expect(link.textContent).toBe("/home/sprooty/Working/notes.md");
    link.click();
    expect(opened).toEqual(["/home/sprooty/Working/notes.md"]);
  });

  it("keeps trailing punctuation after a bare path as text", () => {
    const c = render(() => <div>{renderMarkdown("read /home/sprooty/Working/notes.md.")}</div>);
    expect(c.container.querySelector("button.md-file-link")?.textContent).toBe(
      "/home/sprooty/Working/notes.md",
    );
    expect(c.container.textContent).toBe("read /home/sprooty/Working/notes.md.");
  });

  it("keeps an https link as a normal link", () => {
    const c = render(() => <div>{renderMarkdown("[docs](https://example.com/guide)")}</div>);
    expect(c.container.querySelector("a")?.getAttribute("href")).toBe("https://example.com/guide");
    expect(c.container.querySelector("button.md-file-link")).toBeNull();
  });
});
