#!/usr/bin/env bash
# Deprecated alias, kept for one release (WI-887): `vogt-lsp mcp rust` serves the
# same purpose, finds the crate root per call and is registered with Codex by
# vogt-mcp-bootstrap. Remove after the next release.
set -euo pipefail
exec /usr/local/bin/vogt-lsp mcp rust
