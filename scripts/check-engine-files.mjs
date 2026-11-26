// Engine-spec, image and workflow checks with no Python (WI-4610).
// Also ports tests/test_engine_parity.py (WI-4663) against a golden.
// Ports the assertions of tests/test_check_engine_spec.py (the real-tree
// half), tests/test_dev_images_workflow.py and tests/test_engine_image_core.py.
// Run from anywhere: node scripts/check-engine-files.mjs
import { readFileSync, existsSync, readdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const read = (p) => readFileSync(join(root, p), "utf8");
const failures = [];
const check = (ok, msg) => { if (!ok) failures.push(msg); };
const count = (text, needle) => text.split(needle).length - 1;
const codeLines = (text) => text.split("\n").filter((l) => !l.trimStart().startsWith("#"));

if (!existsSync(join(root, "engine/server/src/app.rs"))) {
  console.log("engine files check skipped: engine/ absent");
  process.exit(0);
}

// --- engine OpenAPI spec vs routes (scripts/check_engine_spec.py) ---
{
  const app = read("engine/server/src/app.rs");
  const routes = new Set([...app.matchAll(/\.route\(\s*"(\/[^"]*)"/g)].map((m) => m[1]));
  const spec = read("docs/engine-openapi.yaml");
  const idx = spec.indexOf("\npaths:\n");
  const paths = new Set();
  if (idx >= 0) {
    const block = spec.slice(idx + 8).split(/\n(?=[A-Za-z])/)[0];
    for (const m of ("\n" + block).matchAll(/^  (\/[^:\s]*):\s*$/gm)) paths.add(m[1]);
  }
  for (const r of [...routes].sort())
    if (r.startsWith("/api/sessions") && !paths.has(r))
      failures.push(`${r} is an engine route with no path in the spec`);
  for (const p of [...paths].sort())
    if (!routes.has(p)) failures.push(`${p} is in the spec but is not an engine route`);
}

// --- dev image workflows (tests/test_dev_images_workflow.py) ---
{
  const df = read("engine/Dockerfile.pod");
  const args = Object.fromEntries([...df.matchAll(/^ARG\s+([A-Z0-9_]+)=(.*)$/gm)].map((m) => [m[1], m[2]]));
  for (const n of ["UBUNTU_IMAGE", "RUST_IMAGE"])
    check((args[n] ?? "").includes("@sha256:"), `${n} is not pinned by digest`);
  for (const [, ref] of df.matchAll(/^FROM\s+(\S+)/gm)) {
    const m = ref.match(/^\$\{([A-Z0-9_]+)\}$/);
    let image;
    if (m) image = args[m[1]] ?? "";
    else if (ref.includes("/")) image = ref;
    else { check(["base", "rust"].includes(ref), `FROM names an unknown stage: ${ref}`); continue; }
    check(image.includes("@sha256:"), `FROM ${image} is not pinned by digest`);
    check(!image.endsWith(":latest"), `FROM ${image} uses :latest`);
  }
  check(df.includes("\nCOPY . ."), "pod Dockerfile no longer COPY . .");
  const base = read(".github/workflows/pod-base.yml");
  check(base.includes("git ls-tree -r HEAD -- engine"), "pod-base tag does not hash git ls-tree of engine");
  check(!base.includes("sha256sum engine/Dockerfile.pod"), "pod-base tag hashes only the Dockerfile");

  const w = read(".github/workflows/rust2-images.yml");
  check(w.includes("workflow_dispatch:"), "rust2-images: no workflow_dispatch");
  check(w.includes('branches: ["graft/batch-*"]'), "rust2-images: not triggered by graft/batch-*");
  check(!w.includes("branches: [rust-2]"), "rust2-images: watches the rust-2 branch");
  check(w.includes("repo.indexarr.net/indexarr/vogt-dev-pod"), "rust2-images: no pod image");
  check(w.includes("repo.indexarr.net/indexarr/vogt-dev-engine"), "rust2-images: no engine image");
  check(count(w, "push-by-digest=true") === 2, "rust2-images: push-by-digest count != 2");
  check(count(w, "provenance: true") === 2, "rust2-images: provenance count != 2");
  check(count(w, "sbom: true") === 2, "rust2-images: sbom count != 2");
  check(w.includes("steps.push.outputs.digest"), "rust2-images: digest output missing");
  check(w.includes("Graft-Changeset: *"), "rust2-images: Graft-Changeset trailer missing");
  check(w.includes("needs.source.outputs.changeset"), "rust2-images: changeset output unused");
  check(!w.includes("VOGT_SOURCE_SHA=${{ github.sha }}"), "rust2-images: builds the squashed github sha");
  const pod = "${{ env.POD_IMAGE }}@${{ needs.pod.outputs.digest }}";
  check(w.includes(`POD_BASE_IMAGE=${pod}`), "engine image not built on the run's pod digest (POD_BASE_IMAGE)");
  check(w.includes(`ESTATE_IMAGE=${pod}`), "engine image not built on the run's pod digest (ESTATE_IMAGE)");
  check(w.includes("${ENGINE_IMAGE}@${ENGINE_DIGEST}"), "run does not report the engine image digest");
  check(w.includes("${POD_IMAGE}@${POD_DIGEST}"), "run does not report the pod image digest");
}

// --- engine image (tests/test_engine_image_core.py) ---
{
  const text = read("engine/Dockerfile");
  const code = codeLines(text).join("\n");
  check(text.includes("-p vogt-engine-server -p vogt-core"), "server-build does not compile vogt-core");
  check(text.includes("COPY --from=server-build /usr/local/bin/vogt-core /usr/local/bin/vogt-core"),
    "runtime does not copy vogt-core");
  const cargo = read("engine/Cargo.toml");
  const members = [...(cargo.match(/members\s*=\s*\[([\s\S]*?)\]/)?.[1] ?? "").matchAll(/"([^"]+)"/g)].map((m) => m[1]);
  check(members.length > 0, "no workspace members parsed from engine/Cargo.toml");
  const parts = read(".github/workflows/rust-core.yml").split("- name: server-build stage loads\n");
  check(parts.length > 1, "rust-core.yml has no 'server-build stage loads' step");
  const stage = (parts[1] ?? "").split("\n\n")[0];
  for (const m of members) for (const e of ["Cargo.toml", "src"])
    check(stage.includes(`copy engine/${m}/${e} ${m}/${e}`), `server-build check does not copy engine/${m}/${e}`);
  for (const r of ["UV_IMAGE", "CORE_IMAGE", "AS core", "AS uv"])
    check(!code.includes(r), `engine/Dockerfile still refers to ${r}`);
  check(!/COPY --from=core\b/.test(code), "engine/Dockerfile copies from a published core");
  check(code.includes("AS python-core"), "engine/Dockerfile has no python-core stage");
  check(code.includes("COPY --from=python-core /opt/vogt /opt/vogt"), "python-core not copied");
  check(!code.includes("ln -sfn /usr/local/bin/vogt-core /usr/local/bin/vogt"), "build-time link of vogt to rust");
  check(!code.includes("ln -sfn /usr/local/bin/vogt-core /usr/local/bin/vogt-mcp-remote"), "build-time link of vogt-mcp-remote to rust");
  check(code.includes("ghcr.io/thedancingdeveloper-org/vogt-base/python:3.13-slim@"), "python base not digest pinned");
  check(text.includes("install -d -o ${SPROOTY_UID} -g ${SPROOTY_GID} -m 0755 /opt/vogt/bin"), "/opt/vogt/bin not created");
  const pl = text.split("\n").filter((l) => l.startsWith("ENV PATH="));
  check(pl.length > 0, "engine/Dockerfile sets no PATH");
  check(pl.length > 0 && pl.at(-1).startsWith('ENV PATH="/opt/vogt/bin:'), "/opt/vogt/bin is not first on PATH");

  const lines = codeLines(read("engine/deploy/entrypoint.sh"));
  const ep = lines.join("\n");
  check(!lines.some((l) => /(?<![\w-])vogt (init|serve)\b/.test(l)), "entrypoint calls bare vogt init/serve");
  for (const s of [
    '"$bin" init', '"$bin" serve --host "$host" --port "$port"',
    'local impl="${VOGT_CORE_IMPL:-python}"', 'cli="${VOGT_PYTHON_BIN:-/opt/vogt/.venv/bin/vogt}"',
    'mcp="${VOGT_PYTHON_MCP_BIN:-/opt/vogt/.venv/bin/vogt-mcp-remote}"',
    'local dest="${VOGT_BIN_DIR:-/opt/vogt/bin}"', 'ln -sfn "$cli" "$dest/vogt"',
    'ln -sfn "$mcp" "$dest/vogt-mcp-remote"', "link_vogt_names",
  ]) check(ep.includes(s), `entrypoint.sh lacks: ${s}`);
  check(!ep.includes('local dest="${VOGT_BIN_DIR:-/usr/local/bin}"'), "entrypoint links into /usr/local/bin");

  const wf = join(root, ".github/workflows");
  for (const f of readdirSync(wf).filter((f) => f.endsWith(".yml")).sort())
    read(`.github/workflows/${f}`).split("\n").forEach((l, i) => {
      if (l.includes("engine/Dockerfile") && l.includes("CORE_IMAGE"))
        failures.push(`CORE_IMAGE passed to engine/Dockerfile at ${f}:${i + 1}`);
    });
}

// --- engine routes vs MCP (tests/test_engine_parity.py, WI-4663) ---
// Reads the golden dumped from ENGINE_COUNTERPARTS, ENGINE_ONLY and the MCP
// registry (regenerate: uv run python tests/test_engine_parity.py --regenerate).
{
  const golden = JSON.parse(read("tests/fixtures/engine_parity.json"));
  const counterparts = golden.engine_counterparts;
  const only = golden.engine_only;
  const mcp = new Set(golden.mcp_operations);
  const engineSrc = "engine/server/src";
  const consts = {};
  for (const dir of [engineSrc, "engine/session-agent/src"])
    for (const f of readdirSync(join(root, dir)).filter((f) => f.endsWith(".rs")))
      for (const m of read(`${dir}/${f}`).matchAll(/pub const ([A-Z_]+): &str = "(\/[^"]*)"/g)) consts[m[1]] = m[2];
  const probe = read(`${engineSrc}/vogt_core.rs`).match(/pub const PROBE_PATHS: \[&str; \d+\] = \[([\s\S]*?)\];/);
  if (!probe) failures.push("PROBE_PATHS was not found in vogt_core.rs");
  const probePaths = probe ? [...probe[1].matchAll(/"([^"]+)"/g)].map((m) => m[1]) : [];

  // Every `METHOD /path` the router serves: each `.route(` call read to its
  // matching parenthesis, a constant path resolved, the probe loop expanded.
  const text = read(`${engineSrc}/app.rs`);
  const routes = new Set();
  let start = 0, found;
  while ((found = text.indexOf(".route(", start)) !== -1) {
    const cursor = found + 7;
    let depth = 1, end = cursor;
    while (depth) { const c = text[end++]; depth += c === "(" ? 1 : c === ")" ? -1 : 0; }
    const call = text.slice(cursor, end - 1);
    start = end;
    let paths;
    const lit = call.match(/^\s*"([^"]+)"/);
    if (lit) paths = [lit[1]];
    else {
      const name = call.match(/^\s*([\w:]+)/);
      if (!name) { failures.push(`unreadable route call: ${call.slice(0, 60)}`); continue; }
      const ident = name[1].split("::").pop();
      if (ident === "path") paths = probePaths;
      else if (ident in consts) paths = [consts[ident]];
      else { failures.push(`route constant ${ident} not found`); continue; }
    }
    const methods = [...call.matchAll(/\b(get|post|put|patch|delete|any)\(/g)].map((m) => m[1].toUpperCase());
    if (!methods.length) failures.push(`no handler method in route call: ${call.slice(0, 60)}`);
    for (const m of methods) for (const p of paths) routes.add(`${m} ${p}`);
  }
  check(routes.size > 50, "the router read implausibly few routes");

  const cKeys = Object.keys(counterparts), oKeys = Object.keys(only);
  const listed = new Set([...cKeys, ...oKeys]);
  const unaccounted = [...routes].filter((r) => !listed.has(r)).sort();
  check(!unaccounted.length, `engine routes ${JSON.stringify(unaccounted)} have no MCP counterpart and no reason: add the core operation that proxies each to ENGINE_COUNTERPARTS, or say why there is none in ENGINE_ONLY (src/vogt/registry/engine_routes.py), then regenerate tests/fixtures/engine_parity.json`);
  const stale = [...listed].filter((r) => !routes.has(r)).sort();
  check(!stale.length, `entries name routes the engine no longer serves: ${JSON.stringify(stale)}`);
  const both = cKeys.filter((r) => r in only).sort();
  check(!both.length, `routes listed as both counterpart and exclusion: ${JSON.stringify(both)}`);
  const wrong = cKeys.filter((r) => !mcp.has(counterparts[r])).sort();
  check(!wrong.length, `counterparts that are not MCP operations: ${JSON.stringify(wrong.map((r) => [r, counterparts[r]]))}`);
  for (const [r, reason] of Object.entries(only))
    check(reason.trim().split(/\s+/).length >= 8, `${r} needs a real justification`);

  // The PWA's own calls, named directly.
  const webSrc = join(root, "web/src");
  if (existsSync(webSrc)) {
    const norm = (p) => {
      const i = p.indexOf("${");
      if (i !== -1) p = p.slice(0, i);
      return p.replace(/\{[^}]*\}/g, "{}").replace(/\/+$/, "");
    };
    const accounted = [...listed].map((e) => norm(e.split(" ").slice(1).join(" ")));
    const literals = new Set();
    const walk = (dir) => {
      for (const e of readdirSync(dir, { withFileTypes: true }).sort((a, b) => a.name.localeCompare(b.name))) {
        if (e.isDirectory()) { if (e.name !== "__tests__") walk(join(dir, e.name)); }
        else if (/\.tsx?$/.test(e.name))
          for (const m of readFileSync(join(dir, e.name), "utf8").matchAll(/["'`](\/api\/[A-Za-z0-9/_.$\-{}]*)/g)) literals.add(m[1]);
      }
    };
    walk(webSrc);
    const unresolved = [...literals].filter((l) => !l.startsWith("/api/vogt")
      && !accounted.some((k) => k === norm(l) || k.startsWith(norm(l)))).sort();
    check(!unresolved.length, `the PWA calls ${JSON.stringify(unresolved)}, which no entry accounts for`);
  }
}

if (failures.length) {
  console.log(`engine files check: ${failures.length} problem(s)`);
  for (const f of failures) console.log(`  ${f}`);
  process.exit(1);
}
console.log("engine files check passed");
