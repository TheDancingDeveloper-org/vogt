// Putting a recognizer's output back into the vocabulary the domain is made
// of (FR-T13, r16).
//
// FR-T5 adopted voice unproven and asked for "a validation pass against domain
// vocabulary (project names, 'backlog')". This is the half of that pass which
// is code rather than a person with a microphone: whatever the recognizer
// heard, the two things it reliably mangles are a work-item ref and a project
// slug, and both are the *subject* of the sentence. "Can you work on issue
// twelve for rust NZB D" is a perfectly good transcription and a useless one.
//
// Deliberately not a general-purpose corrector. It repairs two known shapes
// and leaves every other word alone, because a fuzzy pass over the whole
// utterance would eventually "fix" a word the user meant, and a wrong repair
// is worse than no repair: it is confidently wrong, and it is what gets sent.
//
// Every repair is reported rather than applied silently, so the composer can
// show what it is about to send. A repair the speaker can see is one they can
// stop.

/** One thing that was changed, so the UI can show the change and not just the result. */
export interface VoiceRepair {
  /** What the recognizer produced. */
  readonly heard: string;
  /** What it was replaced with. */
  readonly repaired: string;
  readonly kind: "work-item" | "project";
}

export interface VoiceRepairResult {
  readonly text: string;
  readonly repairs: readonly VoiceRepair[];
}

/**
 * Spoken numbers, up to the range a work-item ref plausibly reaches. A
 * recognizer asked for "issue twelve" may return the word rather than the
 * digits, and `WI-twelve` matches nothing.
 */
const NUMBER_WORDS: Record<string, number> = {
  zero: 0, one: 1, two: 2, three: 3, four: 4, five: 5, six: 6, seven: 7,
  eight: 8, nine: 9, ten: 10, eleven: 11, twelve: 12, thirteen: 13,
  fourteen: 14, fifteen: 15, sixteen: 16, seventeen: 17, eighteen: 18,
  nineteen: 19, twenty: 20, thirty: 30, forty: 40, fifty: 50, sixty: 60,
  seventy: 70, eighty: 80, ninety: 90,
};

/**
 * Every way a recognizer writes down "W I" before a number, plus the words a
 * speaker actually uses for one. `issue` and `ticket` are here because that
 * is what people say out loud; nobody says "work item dash seven".
 */
const WORK_ITEM_LEAD =
  "(?:w\\.?\\s*i\\.?|wi|work\\s*item|issue|ticket|item|bug)";

/** `WI-7`, however it arrived. */
const WORK_ITEM_RE = new RegExp(
  `\\b${WORK_ITEM_LEAD}[\\s.,\\-–—]*(?:number\\s+)?([0-9]+|[a-z]+(?:[\\s-][a-z]+)?)\\b`,
  "gi",
);

/** How far a heard word may be from a slug and still be taken for it. */
const MAX_SLUG_DISTANCE = 2;

/**
 * Turn "twelve" or "twenty one" into 12 / 21. Returns null for anything that
 * is not a number, which is the common case — `issue tracker` must not become
 * `WI-` anything.
 */
function spokenNumber(words: string): number | null {
  const parts = words.toLowerCase().split(/[\s-]+/).filter(Boolean);
  if (parts.length === 0 || parts.length > 2) return null;
  let total = 0;
  for (const part of parts) {
    const value = NUMBER_WORDS[part];
    if (value === undefined) return null;
    total += value;
  }
  return total;
}

/** Levenshtein distance, bounded — we only ever care about "within 2". */
function distance(a: string, b: string, limit: number): number {
  if (Math.abs(a.length - b.length) > limit) return limit + 1;
  let previous = Array.from({ length: b.length + 1 }, (_, i) => i);
  for (let i = 1; i <= a.length; i += 1) {
    const current = [i];
    let best = i;
    for (let j = 1; j <= b.length; j += 1) {
      const cost = a[i - 1] === b[j - 1] ? 0 : 1;
      const value = Math.min(
        (current[j - 1] ?? 0) + 1,
        (previous[j] ?? 0) + 1,
        (previous[j - 1] ?? 0) + cost,
      );
      current.push(value);
      if (value < best) best = value;
    }
    // Every remaining row can only grow, so a row whose best is already over
    // the limit cannot come back under it.
    if (best > limit) return limit + 1;
    previous = current;
  }
  return previous[b.length] ?? limit + 1;
}

/**
 * How a slug sounds when it is read out: `rustnzbd` becomes "rust nzb d", and
 * a recognizer writes that down as separate words. Comparing the *squashed*
 * forms is what lets those meet again.
 */
function squash(value: string): string {
  return value.toLowerCase().replace(/[^a-z0-9]/g, "");
}

function repairWorkItems(text: string, repairs: VoiceRepair[]): string {
  return text.replace(WORK_ITEM_RE, (match, raw: string) => {
    // "twenty one" is two words and so is "twelve for", and the pattern
    // cannot tell them apart — so the two-word reading is tried first and the
    // one-word reading is what "issue twelve for rustnzbd" falls back to.
    // Without the fallback the whole ref is left unrepaired whenever the
    // sentence continues, which is most of the time.
    let consumed = raw;
    let number = /^[0-9]+$/.test(raw) ? Number(raw) : spokenNumber(raw);
    if (number === null) {
      const first = raw.split(/[\s-]+/)[0] ?? "";
      const single = spokenNumber(first);
      if (single !== null) {
        number = single;
        consumed = first;
      }
    }
    if (number === null || !Number.isFinite(number)) return match;
    const repaired = `WI-${number}`;
    // Whatever the pattern matched beyond the number belongs to the sentence.
    const tail = consumed === raw ? "" : match.slice(match.lastIndexOf(consumed) + consumed.length);
    const heard = tail ? match.slice(0, match.length - tail.length) : match;
    if (heard === repaired) return match;
    repairs.push({ heard, repaired, kind: "work-item" });
    return repaired + tail;
  });
}

/**
 * Replace anything that is nearly a known slug with the slug.
 *
 * Windows of one to four consecutive words, longest first: a slug spoken as
 * several words has to be matched as several words, and matching greedily
 * stops "rust nzb d" from being repaired to `rustnzbd` twice over.
 */
function repairProjects(
  text: string,
  slugs: readonly string[],
  repairs: VoiceRepair[],
): string {
  if (slugs.length === 0) return text;
  const bySquashed = new Map<string, string>();
  for (const slug of slugs) bySquashed.set(squash(slug), slug);

  // Tokenize keeping the separators, so rebuilt text keeps its spacing.
  const tokens = text.split(/(\s+)/);
  const isWord = (index: number) => index % 2 === 0 && (tokens[index]?.length ?? 0) > 0;

  for (let start = 0; start < tokens.length; start += 2) {
    if (!isWord(start)) continue;
    for (let span = Math.min(4, (tokens.length - start + 1) / 2); span >= 1; span -= 1) {
      const end = start + (span - 1) * 2;
      if (end >= tokens.length || !isWord(end)) continue;
      const heard = tokens.slice(start, end + 1).join("");
      // Trailing punctuation belongs to the sentence, not to the name.
      const trailing = /[.,!?;:]+$/.exec(heard)?.[0] ?? "";
      const core = trailing ? heard.slice(0, -trailing.length) : heard;
      const squashed = squash(core);
      if (squashed.length < 3) continue;

      let match = bySquashed.get(squashed);
      if (match === undefined && span === 1) {
        // Fuzzy matching is for *one* word only. Across a window it repairs
        // sentences rather than names: "how is rust nzbd" squashes to within
        // two edits of `rustnzbd`, and the repair silently eats the verb.
        // A slug spoken as several words is a spacing problem, not a hearing
        // one — the letters are right — so multi-word windows must match
        // exactly.
        let best: { slug: string; distance: number } | null = null;
        for (const [candidate, slug] of bySquashed) {
          const d = distance(squashed, candidate, MAX_SLUG_DISTANCE);
          // Short names are a trap: within two edits of `web` sits half the
          // language. Require the distance to be a small share of the name.
          if (d > MAX_SLUG_DISTANCE || d >= candidate.length / 2) continue;
          if (best === null || d < best.distance) best = { slug, distance: d };
        }
        match = best?.slug;
      }
      if (match === undefined || match === core) continue;

      repairs.push({ heard: core, repaired: match, kind: "project" });
      tokens.splice(start, end - start + 1, match + trailing);
      break;
    }
  }
  return tokens.join("");
}

/**
 * Repair one utterance against the projects this instance actually has.
 *
 * `slugs` comes from `project.list` — the repair is against what exists, not
 * against a list in this file, which is the difference between a validation
 * pass and a guess.
 */
export function repairUtterance(
  text: string,
  slugs: readonly string[] = [],
): VoiceRepairResult {
  const repairs: VoiceRepair[] = [];
  const withItems = repairWorkItems(text, repairs);
  const withProjects = repairProjects(withItems, slugs, repairs);
  return { text: withProjects, repairs };
}

/** One line a person can read before the utterance is sent. */
export function describeRepairs(repairs: readonly VoiceRepair[]): string {
  if (repairs.length === 0) return "";
  return repairs.map((r) => `“${r.heard}” → ${r.repaired}`).join(", ");
}
