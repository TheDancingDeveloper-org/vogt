#!/usr/bin/env bash
set -euo pipefail

workspace_root="${MYDEVENV2_RUST_ANALYZER_WORKSPACE:-}"

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

cd "${workspace_root}"
exec rust-analyzer-mcp
