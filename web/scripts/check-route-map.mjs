// The PWA route map, checked against the Rust registry manifest.
//
// `tests/parity/golden/registry.json` is `vogt-core`'s `registry.dump`,
// recorded from the Rust core and compared to Python only while Python still
// exists (`engine/core/src/registry.rs`, `the_dump_matches_the_python_golden`).
// This script is the check that used to live in `tests/test_pwa.py` as an
// import of the Python registry: every entry in `web/src/vogtApi.ts`'s
// `ROUTES` names an HTTP operation, and the path it asks for is the path the
// registry serves. It reads the golden, not a Python process, so it survives
// the removal of the Python core.
//
// It also carries the rest of the PWA contract that has no Python in it:
// every write the client exports collects a reason, and every engine path the
// bundle names is one the engine's router serves and `docs/ENGINE.md` lists.
//
// Run from the repository root or from `web/`:
//   node web/scripts/check-route-map.mjs

import { readdirSync, readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repo = resolve(here, "../..");

const manifest = JSON.parse(
  readFileSync(resolve(repo, "tests/parity/golden/registry.json"), "utf8"),
);
const webSrc = resolve(repo, "web/src");

// A TypeScript file with its comments removed, the same reduction
// `tests/test_pwa.py` makes: a comment explaining a rule names what the rule
// forbids, and a commented-out route is not a route.
function source(path) {
  const text = readFileSync(path, "utf8").replace(/\/\*.*?\*\//gs, "");
  return text
    .split("\n")
    .map((line) => {
      const head = line.split("//")[0];
      const quotes = (head.match(/"/g) ?? []).length;
      const ticks = (head.match(/`/g) ?? []).length;
      return quotes % 2 || ticks % 2 ? line : head;
    })
    .join("\n");
}

const client = source(resolve(webSrc, "vogtApi.ts"));

const block = client.match(/export const ROUTES = \{(.*?)\n\} as const;/s);
if (!block) {
  throw new Error("the Vogt client's route table was not found");
}
const entries = [...block[1].matchAll(/"?([a-z][a-z._]*)"?:\s*"([^"]+)"/g)];
if (entries.length === 0) {
  throw new Error("the route table is empty, so this file proves nothing");
}

const byName = new Map(
  manifest.operations
    .filter((operation) => operation.transports.includes("http"))
    .map((operation) => [operation.name, operation]),
);

const problems = [];
for (const [, name, path] of entries) {
  const operation = byName.get(name);
  if (!operation) {
    problems.push(`${name} is named by the PWA and is not an HTTP operation`);
    continue;
  }
  if (operation.http_path !== path) {
    problems.push(
      `${name} is served at ${operation.http_path}, but the PWA asks for ${path}`,
    );
  }
}

// Every write carries a reason: no exported write can be called without one.
const mutating = new Set(
  entries.map(([, name]) => name).filter((name) => byName.get(name)?.mutating),
);
if (mutating.size === 0) {
  problems.push(
    "the PWA names no writes; the reason check would prove nothing",
  );
}
for (const match of client.matchAll(/call<[^>]*>\(\s*"([a-z][a-z._]*)"/g)) {
  if (!mutating.has(match[1])) continue;
  const start = client.lastIndexOf("export const", match.index);
  if (!client.slice(start, match.index + match[0].length).includes("reason")) {
    problems.push(`${match[1]} is a write the PWA can make without a reason`);
  }
}

// Every engine path the bundle names is served by the engine's router and
// written down in its wire contract. `/api/vogt` is the front door, covered
// by the route table above.
const normalise = (path) => {
  const interpolated = path.indexOf("${");
  if (interpolated !== -1) path = path.slice(0, interpolated);
  return path
    .replace(/\{[^}]*\}/g, "{}")
    .replace(/(?<=\/):[A-Za-z_][A-Za-z0-9_]*/g, "{}")
    .replace(/\/+$/, "");
};
const served = new Set(
  [
    ...readFileSync(resolve(repo, "engine/server/src/app.rs"), "utf8").matchAll(
      /\.route\(\s*"([^"]+)"/g,
    ),
  ].map((m) => normalise(m[1])),
);
const documented = new Set(
  [
    ...readFileSync(resolve(repo, "docs/ENGINE.md"), "utf8").matchAll(
      /`(?:[A-Z]+ )?(\/(?:api|healthz|readyz|mcp)[^`\s?]*)/g,
    ),
  ].map((m) => normalise(m[1])),
);
if (served.size === 0) problems.push("no routes found in the engine's router");
if (documented.size === 0) problems.push("no routes found in docs/ENGINE.md");
const resolves = (candidate, known) =>
  known.has(candidate) || [...known].some((path) => path.startsWith(candidate));
const literals = new Set();
for (const file of readdirSync(webSrc).sort()) {
  if (!/\.tsx?$/.test(file)) continue;
  for (const m of source(resolve(webSrc, file)).matchAll(
    /["'`](\/api\/[A-Za-z0-9/_.$\-{}]*)/g,
  )) {
    if (!m[1].startsWith("/api/vogt")) literals.add(m[1]);
  }
}
for (const literal of [...literals].sort()) {
  const candidate = normalise(literal);
  if (!resolves(candidate, served)) {
    problems.push(
      `the PWA calls ${literal}, which the engine's router does not serve`,
    );
  }
  if (!resolves(candidate, documented)) {
    problems.push(
      `the PWA calls ${literal}, which docs/ENGINE.md does not list`,
    );
  }
}

if (problems.length > 0) {
  console.error(problems.join("\n"));
  process.exit(1);
}
console.log(
  `route map: ${entries.length} PWA routes match the Rust registry manifest; ` +
    `${mutating.size} writes collect a reason; ` +
    `${literals.size} engine paths are served and documented`,
);
