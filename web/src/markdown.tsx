// A small, sanitising Markdown renderer.
//
// With the forge pivot, a work item's body, its comments and the board's card
// bodies are GitHub-flavoured Markdown rather than the plain text they used to
// be, so `#`, `-`, `[label](url)` and fenced ``` arrive literal and are read
// as noise. This turns that subset into real nodes.
//
// The one rule that matters is the security rule, and it is kept by
// construction rather than by scrubbing: every node here is *built*, never
// injected. There is no `innerHTML` anywhere, so a `<script>` in the source is
// a text node reading "<script>" and nothing a browser will run — SolidJS
// escapes text content and attribute values for us. The renderer supports only
// an allow-list of constructs (headings, lists, links, inline and fenced
// code, emphasis); anything it does not recognise falls through as escaped
// text. Link hrefs are the one place a string reaches an attribute, so they
// pass an allow-list of their own: http(s), mailto and relative references are
// kept, and everything else — `javascript:`, `data:`, `vbscript:`, an unknown
// scheme, or an entity-encoded disguise of one — is dropped and the link is
// rendered as inert text.

import type { JSX } from "solid-js";
import { Dynamic } from "solid-js/web";
import { openEditorTab } from "./tabs";

let openFileView: (path: string) => void = (path) => { openEditorTab(path); };
export function setFileViewOpener(opener: (path: string) => void): void { openFileView = opener; }


// -- href sanitising --------------------------------------------------------

/** Decode the numeric and named HTML entities an attacker would hide a scheme
 *  behind (`&#106;avascript:` → `javascript:`), so the scheme check below
 *  sees what the browser would eventually see. */
function decodeEntities(value: string): string {
  return value
    .replace(/&#x([0-9a-f]+);?/gi, (_m, hex) =>
      String.fromCodePoint(parseInt(hex, 16)),
    )
    .replace(/&#(\d+);?/g, (_m, dec) => String.fromCodePoint(parseInt(dec, 10)))
    .replace(/&colon;/gi, ":")
    .replace(/&tab;/gi, "\t")
    .replace(/&newline;/gi, "\n");
}

/**
 * The href allow-list. Returns a safe href, or `null` when the reference must
 * not become a link at all.
 *
 * The decision is made on a *probe* — the reference with entities decoded and
 * all control/whitespace removed and lower-cased — but the value returned is
 * the original, so a legitimate URL keeps its exact form. http(s), mailto and
 * references with no scheme (relative paths, anchors, queries) are allowed;
 * everything with any other scheme is refused.
 */
export function filePathRef(raw: string): string | null {
  let text = raw.trim().replace(/^[`'"]+|[`'"]+$/g, "");
  if (!text || /\s/.test(text)) return null;
  if (/^file:\/\//i.test(text)) {
    try { text = decodeURIComponent(new URL(text).pathname); } catch { return null; }
  }
  if (/^[a-z][a-z0-9+.-]*:/i.test(text)) return null;
  text = text.replace(/[),.;:]+$/, "");
  if (text.startsWith("/")) return text.split("/").filter(Boolean).length >= 2 ? text : null;
  if (text.startsWith("./")) return text.slice(2);
  if (/^[\w.-]+(?:\/[\w.-]+)+$/.test(text)) return text;
  return null;
}

export function safeHref(raw: string | null | undefined): string | null {
  if (!raw) return null;
  const url = raw.trim();
  if (!url) return null;
  const probe = decodeEntities(url)
    .replace(/[\u0000-\u0020\u007f]+/g, "")
    .toLowerCase();
  if (/^(https?:|mailto:)/.test(probe)) return url;
  // No scheme before the first path separator means a relative reference.
  const head = probe.split(/[/?#]/, 1)[0] ?? probe;
  if (!head.includes(":")) return url;
  return null;
}

// -- inline spans -----------------------------------------------------------

/**
 * Parse the inline constructs inside one run of text into nodes.
 *
 * Text that matches nothing is pushed as a plain string, which SolidJS renders
 * as an escaped text node — that is what makes raw HTML in the source inert.
 */
export function renderInline(text: string): JSX.Element[] {
  const nodes: JSX.Element[] = [];
  let buffer = "";
  let i = 0;

  const flush = () => {
    if (buffer) {
      nodes.push(buffer);
      buffer = "";
    }
  };

  while (i < text.length) {
    const rest = text.slice(i);
    const char = text[i];

    // Inline code: verbatim, no nested parsing.
    if (char === "`") {
      const end = text.indexOf("`", i + 1);
      if (end !== -1) {
        flush();
        nodes.push(<code class="md-code">{text.slice(i + 1, end)}</code>);
        i = end + 1;
        continue;
      }
    }

    // Link: [label](href). The label is parsed for inline constructs; the
    // href passes the allow-list or the whole thing renders as inert text.
    if (char === "[") {
      const match = /^\[([^\]]*)\]\(([^)\s]*)\)/.exec(rest);
      if (match) {
        flush();
        const target = match[2] ?? "";
        const file = filePathRef(target);
        const href = file ? null : safeHref(target);
        const label = renderInline(match[1] ?? "");
        if (file) { nodes.push(<FileLink path={file}>{label}</FileLink>); }
        else if (href) {
          nodes.push(
            <a
              class="md-link"
              href={href}
              target="_blank"
              rel="noopener noreferrer nofollow"
            >
              {label}
            </a>,
          );
        } else {
          // Refused scheme: keep the words, drop the link.
          nodes.push(<span class="md-link md-link--blocked">{label}</span>);
        }
        i += match[0].length;
        continue;
      }
    }

    // Bold, before italic so `**` is not read as two `*`.
    if (char === "*" && text[i + 1] === "*") {
      const end = text.indexOf("**", i + 2);
      if (end !== -1 && end > i + 2) {
        flush();
        nodes.push(<strong>{renderInline(text.slice(i + 2, end))}</strong>);
        i = end + 2;
        continue;
      }
    }

    // Italic: *…* or _…_.
    if (char === "*" || char === "_") {
      const end = text.indexOf(char, i + 1);
      if (end !== -1 && end > i + 1) {
        flush();
        nodes.push(<em>{renderInline(text.slice(i + 1, end))}</em>);
        i = end + 1;
        continue;
      }
    }

    buffer += char;
    i += 1;
  }

  flush();
  return linkify(nodes);
}

const BARE_PATH = /(?:file:\/\/\S+|\/(?:[\w.@+-]+\/)+[\w.@+-]+|[\w.-]+(?:\/[\w.-]+)+)/g;

function linkify(nodes: JSX.Element[]): JSX.Element[] {
  const out: JSX.Element[] = [];
  for (const node of nodes) {
    if (typeof node !== "string") { out.push(node); continue; }
    let cursor = 0;
    for (const found of node.matchAll(BARE_PATH)) {
      const ref = filePathRef(found[0]);
      if (!ref) continue;
      const at = found.index ?? 0;
      if (at > cursor) out.push(node.slice(cursor, at));
      // Trailing punctuation is prose, not path: keep it as text after the link.
      const shown = found[0].replace(/[),.;:]+$/, "");
      out.push(<FileLink path={ref}>{shown}</FileLink>);
      cursor = at + shown.length;
    }
    if (cursor < node.length) out.push(node.slice(cursor));
  }
  return out;
}

function FileLink(props: { path: string; children: JSX.Element }): JSX.Element {
  return (
    <button type="button" class="md-link md-file-link" title={`Open ${props.path}`}
      onClick={() => openFileView(props.path)}>{props.children}</button>
  );
}

// -- blocks -----------------------------------------------------------------

const FENCE = /^```/;
const HEADING = /^(#{1,6})\s+(.*)$/;
const UL_ITEM = /^[-*+]\s+/;
const OL_ITEM = /^\d+\.\s+/;
const TASK = /^\[([ xX])\]\s+(.*)$/;

function sanitiseLang(lang: string): string {
  return lang.replace(/[^a-z0-9+#-]/gi, "").toLowerCase();
}

function listItem(text: string): JSX.Element {
  const task = TASK.exec(text);
  if (task) {
    return (
      <li class="md-li md-task">
        <input type="checkbox" checked={(task[1] ?? "").toLowerCase() === "x"} disabled />{" "}
        {renderInline(task[2] ?? "")}
      </li>
    );
  }
  return <li class="md-li">{renderInline(text)}</li>;
}

/** True when the source carries any construct this renderer would transform —
 *  used by callers that want to keep a plain path for text with no markup. */
export function hasMarkup(source: string): boolean {
  return source
    .split("\n")
    .some((line) => {
      const t = line.trim();
      return (
        FENCE.test(t) ||
        HEADING.test(t) ||
        UL_ITEM.test(t) ||
        OL_ITEM.test(t) ||
        /`[^`]+`/.test(t) ||
        /\[[^\]]*\]\([^)\s]*\)/.test(t) ||
        /\*\*[^*]+\*\*/.test(t)
      );
    });
}

/**
 * Render Markdown source to a fragment of block-level nodes.
 *
 * Text with no markup renders as a single paragraph identical to the plain
 * path, so it is always safe to route everything through here.
 */
export function renderMarkdown(source: string): JSX.Element {
  const lines = source.replace(/\r\n?/g, "\n").split("\n");
  const at = (n: number): string => lines[n] ?? "";
  const blocks: JSX.Element[] = [];
  let i = 0;

  while (i < lines.length) {
    const trimmed = at(i).trim();

    if (trimmed === "") {
      i += 1;
      continue;
    }

    // Fenced code block.
    if (FENCE.test(trimmed)) {
      const lang = sanitiseLang(trimmed.slice(3).trim());
      const body: string[] = [];
      i += 1;
      while (i < lines.length && !FENCE.test(at(i).trim())) {
        body.push(at(i));
        i += 1;
      }
      if (i < lines.length) i += 1; // consume the closing fence
      blocks.push(
        <pre class="md-pre">
          <code class={lang ? `md-code md-lang-${lang}` : "md-code"}>
            {body.join("\n")}
          </code>
        </pre>,
      );
      continue;
    }

    // Heading.
    const heading = HEADING.exec(trimmed);
    if (heading) {
      const level = (heading[1] ?? "#").length;
      const content = renderInline((heading[2] ?? "").replace(/\s+#+\s*$/, ""));
      blocks.push(
        <Dynamic component={`h${level}`} class={`md-h md-h${level}`}>
          {content}
        </Dynamic>,
      );
      i += 1;
      continue;
    }

    // Unordered list.
    if (UL_ITEM.test(trimmed)) {
      const items: JSX.Element[] = [];
      while (i < lines.length && UL_ITEM.test(at(i).trim())) {
        items.push(listItem(at(i).trim().replace(UL_ITEM, "")));
        i += 1;
      }
      blocks.push(<ul class="md-ul">{items}</ul>);
      continue;
    }

    // Ordered list.
    if (OL_ITEM.test(trimmed)) {
      const items: JSX.Element[] = [];
      while (i < lines.length && OL_ITEM.test(at(i).trim())) {
        items.push(listItem(at(i).trim().replace(OL_ITEM, "")));
        i += 1;
      }
      blocks.push(<ol class="md-ol">{items}</ol>);
      continue;
    }

    // Paragraph: consecutive lines until a blank line or a block starts.
    const para: string[] = [];
    while (i < lines.length) {
      const t = at(i).trim();
      if (
        t === "" ||
        FENCE.test(t) ||
        HEADING.test(t) ||
        UL_ITEM.test(t) ||
        OL_ITEM.test(t)
      ) {
        break;
      }
      para.push(t);
      i += 1;
    }
    blocks.push(<p class="md-p">{renderInline(para.join("\n"))}</p>);
  }

  return <>{blocks}</>;
}
