#!/usr/bin/env node
/** Minimal static origin. It has no subprocess, PTY, proxy, or write route. */

import { createReadStream } from "node:fs";
import { stat } from "node:fs/promises";
import { createServer } from "node:http";
import { extname, resolve, sep } from "node:path";

const root = resolve("/app/dist");
const host = process.env.DEMO_BIND ?? "0.0.0.0";
const port = Number(process.env.DEMO_PORT ?? "8910");
const types = new Map([
  [".css", "text/css; charset=utf-8"], [".html", "text/html; charset=utf-8"],
  [".ico", "image/x-icon"], [".js", "text/javascript; charset=utf-8"],
  [".json", "application/json; charset=utf-8"], [".map", "application/json; charset=utf-8"],
  [".png", "image/png"], [".svg", "image/svg+xml"], [".webmanifest", "application/manifest+json"],
  [".woff", "font/woff"], [".woff2", "font/woff2"],
]);

function headers(res) {
  res.setHeader("X-Content-Type-Options", "nosniff");
  res.setHeader("Referrer-Policy", "no-referrer");
  res.setHeader("Permissions-Policy", "camera=(), microphone=(), geolocation=(), payment=()");
  res.setHeader("Cross-Origin-Resource-Policy", "same-origin");
  res.setHeader("Content-Security-Policy", "default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; font-src 'self' data:; connect-src 'self'; frame-src 'self'; object-src 'none'; base-uri 'self'; form-action 'self'; frame-ancestors 'self'");
}

function json(res, status, body) {
  headers(res); res.statusCode = status;
  res.setHeader("Content-Type", "application/json; charset=utf-8");
  res.setHeader("Cache-Control", "no-store");
  res.end(JSON.stringify(body));
}

createServer(async (req, res) => {
  const url = new URL(req.url ?? "/", "http://demo.invalid");
  if (url.pathname === "/healthz") return json(res, 200, { ok: true, mode: "static-demo" });
  if (url.pathname.startsWith("/api/") || url.pathname === "/mcp" || url.pathname.startsWith("/mcp/")) {
    return json(res, 404, { error: "The public demo has no server-side API." });
  }
  if (req.method !== "GET" && req.method !== "HEAD") return json(res, 405, { error: "read-only static origin" });

  let pathname;
  try { pathname = decodeURIComponent(url.pathname); } catch { return json(res, 400, { error: "bad path" }); }
  const relative = pathname === "/" ? "index.html" : pathname.replace(/^\/+/, "");
  const file = resolve(root, relative);
  if (file !== root && !file.startsWith(`${root}${sep}`)) return json(res, 404, { error: "not found" });
  let info;
  try { info = await stat(file); } catch { return json(res, 404, { error: "not found" }); }
  if (!info.isFile()) return json(res, 404, { error: "not found" });

  headers(res);
  res.setHeader("Content-Type", types.get(extname(file)) ?? "application/octet-stream");
  res.setHeader("Content-Length", String(info.size));
  const unversioned = new Set(["index.html", "sw.js", "demo-manifest.json", "demo-build.json"]);
  res.setHeader("Cache-Control", unversioned.has(relative) ? "no-store, must-revalidate" : "public, max-age=31536000, immutable");
  if (req.method === "HEAD") return res.end();
  createReadStream(file).pipe(res);
}).listen(port, host, () => process.stdout.write(`Vogt static demo listening on ${host}:${port}\n`));
