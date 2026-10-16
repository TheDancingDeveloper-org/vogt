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
// Run from the repository root or from `web/`:
//   node web/scripts/check-route-map.mjs

import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repo = resolve(here, "../..");

const manifest = JSON.parse(
  readFileSync(resolve(repo, "tests/parity/golden/registry.json"), "utf8"),
);
const client = readFileSync(resolve(repo, "web/src/vogtApi.ts"), "utf8");

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

if (problems.length > 0) {
  console.error(problems.join("\n"));
  process.exit(1);
}
console.log(
  `route map: ${entries.length} PWA routes match the Rust registry manifest`,
);
