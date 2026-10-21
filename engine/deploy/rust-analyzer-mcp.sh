#!/usr/bin/env bash
set -euo pipefail

workspace_root="${VOGT_ENGINE_RUST_ANALYZER_WORKSPACE:-}"

if [[ -z "${workspace_root}" ]]; then
  probe_dir="${PWD}"
  while [[ "${probe_dir}" != "/" ]]; do
    if [[ -f "${probe_dir}/Cargo.toml" ]]; then
      workspace_root="${probe_dir}"
      break
    fi
    probe_dir="$(dirname "${probe_dir}")"
  done
fi

if [[ -z "${workspace_root}" ]]; then
  workspace_root="${PWD}"
fi


# Build scripts and proc macros run only for a trusted root (WI-889).
guard="$(command -v vogt-lsp-guard || echo "$(dirname "$0")/lsp-guard.sh")"
exec "${guard}" --root "${workspace_root}" -- rust-analyzer-mcp
