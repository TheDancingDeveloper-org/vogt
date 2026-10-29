# docs/DEPLOYMENT.md — Rust-only edits

Apply at the swap (WI-2505). Three edits.

## 1. Section 1, the topology diagram

Replace the `vogt-core serve` stanza:

```
vogt serve   (:8000, loopback inside the container, never published)
  ├── /api/...        REST (OpenAPI at /openapi.json)
  ├── /mcp            MCP streamable HTTP transport
  └── /health/live, /health/ready, /version
```

And the image sentence above it. Replace "carries the Python core, the Rust
session engine, the Solid PWA and the `claude` and `codex` agent CLIs" with
"carries the Rust core, the Rust session engine, the Solid PWA and the
`claude` and `codex` agent CLIs".

## 2. Section 5, "Migrations run at boot"

Replace the paragraph with:

> **Migrations run at boot.** The entrypoint runs `vogt init` before every
> `vogt serve`. `init` is idempotent: it creates the instance on a new volume,
> brings an existing one forward to this build's schema, and leaves the audit
> history alone after that. Until the schemas match, `/health/ready` answers
> 503 naming the store and both schema numbers. Migrations are forward-only and
> run under a lock. Read [`SCHEMA.md`](../../SCHEMA.md) before a major version.

## 3. Section 5, "The stack image ships both cores"

Delete the section. Replace it with:

> **One core.** The entrypoint runs `vogt`, the Rust core, on loopback and
> links the operator names `vogt` and `vogt-mcp-remote` in `/opt/vogt/bin` to
> it. For one release `vogt-core` remains on `PATH` as a second name for the
> same binary; after that it is removed. `init` (with `--check`) and `serve`
> behave as before, and `vogt` carries the registry's CLI: `status`,
> `token issue`, `user create`, `backup`, `restore`, `migrate`,
> `project import`, `project register`, and the `work`, `session`, `forge`,
> `export`, `import`, `clone` commands, with the same global `--json` and
> `--data-dir`. Run them in the container that holds the store. Commands not
> yet ported answer with a "not available" error rather than an empty success.
