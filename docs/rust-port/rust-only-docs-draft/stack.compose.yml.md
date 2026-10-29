# deploy/stack.compose.yml — Rust-only edits

Apply at the swap (WI-2505).

## The VOGT_CORE_URL comment

Replace the comment and delete the `VOGT_CORE_IMPL` line added for the
overlap. After the swap there is nothing to select:

```yaml
      # One value both starts the core and tells the engine where to proxy.
      # Loopback means "the core is ours to run", so the entrypoint supervises
      # `vogt init && exec vogt serve` here; the engine proxies `/api/vogt`,
      # `/mcp` and `/ui-legacy` to the same address. Deriving one from the
      # other is what stops a front door confidently pointed at a port nothing
      # serves.
      VOGT_CORE_URL: "http://127.0.0.1:8000"
```

`vogt` in that command is the Rust core. Do not write `vogt-core`: that name
is the compatibility alias and is due to be removed.
