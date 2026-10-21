#!/usr/bin/env bash
# Remove jdtls -data dirs whose project root is gone (WI-889).
#
# jdtls keeps an index per workspace under its -data dir, and worktrees come
# and go, so these pile up. Each data dir under the store records the root it
# serves in a `.vogt-root` file; a dir whose recorded root no longer exists is
# deleted. A dir with no `.vogt-root` is left alone: we cannot tell what it is.
#
# Usage: vogt-lsp-gc [--dry-run] [store-dir]
#        (default: $VOGT_LSP_STATE_DIR/jdtls, else $HOME/.cache/vogt-lsp/jdtls)
set -euo pipefail

dry=0
if [[ "${1:-}" == --dry-run ]]; then
    dry=1
    shift
fi
state="${VOGT_LSP_STATE_DIR:-${HOME:-}/.cache/vogt-lsp}"
store="${1:-$state/jdtls}"

[[ -d "$store" ]] || exit 0

for dir in "$store"/*/; do
    dir="${dir%/}"
    [[ -d "$dir" && ! -L "$dir" ]] || continue
    marker="$dir/.vogt-root"
    [[ -f "$marker" ]] || continue
    root="$(head -n1 "$marker")"
    if [[ -n "$root" && ! -d "$root" ]]; then
        echo "removing $dir (root $root is gone)"
        [[ "$dry" -eq 1 ]] || rm -rf -- "$dir"
    fi
done
