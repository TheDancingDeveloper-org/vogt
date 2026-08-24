#!/usr/bin/env node
/** Produce provenance without compiling a second PWA. */

import { createHash } from "node:crypto";
import { cp, mkdir, readFile, readdir, writeFile } from "node:fs/promises";
import { dirname, join, relative, resolve } from "node:path";

const root = resolve(import.meta.dirname, "..");
const dist = join(root, "dist");
const buildPath = join(dist, "demo-build.json");
const demo = process.argv.includes("--demo");
const zeros = "0".repeat(40);
const sourceSha = process.env.VOGT_SOURCE_SHA ?? process.env.GITHUB_SHA ?? zeros;
const sourceRef = process.env.VOGT_SOURCE_REF ?? process.env.GITHUB_REF_NAME ?? "local";

function validSha(value) {
  return /^[0-9a-f]{40}$/i.test(value) && value !== zeros;
}

async function filesBelow(path) {
  const files = [];
  for (const entry of await readdir(path, { withFileTypes: true })) {
    const child = join(path, entry.name);
    if (entry.isDirectory()) files.push(...await filesBelow(child));
    else files.push(child);
  }
  return files;
}

async function digest(path) {
  return createHash("sha256").update(await readFile(path)).digest("hex");
}

if (!demo) {
  const ignored = new Set([
    ".vite/manifest.json",
    "demo-build.json",
    "demo-gui.html",
    "demo-manifest.json",
  ]);
  const assets = {};
  for (const file of (await filesBelow(dist)).sort()) {
    const name = relative(dist, file).replaceAll("\\", "/");
    if (!ignored.has(name)) assets[name] = await digest(file);
  }
  await writeFile(buildPath, `${JSON.stringify({
    schema: 1,
    source_ref: sourceRef,
    source_sha: sourceSha,
    assets,
  }, null, 2)}\n`);
  process.stdout.write(`wrote demo-build.json for ${Object.keys(assets).length} PWA assets\n`);
  process.exit(0);
}

if (!validSha(sourceSha)) {
  throw new Error("demo augmentation requires VOGT_SOURCE_SHA to be a non-zero 40-character commit SHA");
}

const built = JSON.parse(await readFile(buildPath, "utf8"));
if (built.source_sha !== sourceSha || built.source_ref !== sourceRef) {
  throw new Error("demo provenance does not match the PWA build; rebuild once with the same source ref and SHA");
}
for (const [name, expected] of Object.entries(built.assets)) {
  if (await digest(join(dist, name)) !== expected) {
    throw new Error(`PWA asset changed after its provenance was recorded: ${name}`);
  }
}

await mkdir(dirname(join(dist, "demo-gui.html")), { recursive: true });
await cp(join(root, "src/demo/gui-stream.html"), join(dist, "demo-gui.html"));
await writeFile(join(dist, "demo-manifest.json"), `${JSON.stringify({
  schema: 1,
  enabled: true,
  source_ref: sourceRef,
  source_sha: sourceSha,
  scenario: "full-estate-v1",
}, null, 2)}\n`);
process.stdout.write(`added demo runtime files for ${sourceRef}@${sourceSha}\n`);
