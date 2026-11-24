import { For, type Component } from "solid-js";

interface SnippetPart {
  text: string;
  highlighted: boolean;
}

function escapeRegExp(value: string): string {
  return value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

function queryTerms(query: string): string[] {
  const seen = new Set<string>();
  const terms: string[] = [];
  for (const term of query.match(/[\p{L}\p{N}_-]+/gu) ?? []) {
    const normalized = term.toLocaleLowerCase();
    if (!seen.has(normalized)) {
      seen.add(normalized);
      terms.push(term);
    }
  }
  return terms;
}

/** Render an archive excerpt without treating terminal output as HTML. */
function parts(text: string, query: string): SnippetPart[] {
  const output: SnippetPart[] = [];
  let highlighted = false;
  const terms = queryTerms(query);
  const pattern = terms.length > 0
    ? new RegExp(`(${terms.map(escapeRegExp).join("|")})`, "giu")
    : null;

  // Keep support for literal <mark> wrappers from older engine responses
  // during a rolling upgrade; every other tag remains ordinary text.
  for (const chunk of text.split(/(<\/?mark>)/gi)) {
    if (/^<mark>$/i.test(chunk)) {
      highlighted = true;
      continue;
    }
    if (/^<\/mark>$/i.test(chunk)) {
      highlighted = false;
      continue;
    }
    if (!chunk) continue;
    if (highlighted || !pattern) {
      output.push({ text: chunk, highlighted });
      continue;
    }
    let cursor = 0;
    for (const match of chunk.matchAll(pattern)) {
      const index = match.index ?? 0;
      if (index > cursor) {
        output.push({ text: chunk.slice(cursor, index), highlighted: false });
      }
      output.push({ text: match[0], highlighted: true });
      cursor = index + match[0].length;
    }
    if (cursor < chunk.length) {
      output.push({ text: chunk.slice(cursor), highlighted: false });
    }
  }
  return output;
}

export const SafeSnippet: Component<{ text: string; query?: string }> = (props) => (
  <span>
    <For each={parts(props.text, props.query ?? "")}>
      {(part) => part.highlighted ? <mark>{part.text}</mark> : part.text}
    </For>
  </span>
);

export { parts as snippetParts };
