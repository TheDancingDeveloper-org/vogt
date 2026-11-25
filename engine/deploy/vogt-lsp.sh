#!/usr/bin/env bash
# vogt-lsp <python|rust|java>: launch a shared language server for the project
# containing the current directory (WI-4745, phase 0d of
# docs/design/shared-language-servers.md).
#
# - Resolves the project root: the nearest language marker walking up from the
#   cwd, never above the git toplevel nor VOGT_LSP_WORKSPACE_ROOT.
# - Python: picks an interpreter (project config, .venv, main checkout's venv,
#   `uv python find`) and exports it as VOGT_LSP_PYTHON_PATH for the client.
# - Strips credentials and Python environment hints from the server's env.
# - Execs the server at its pinned path, or VOGT_LSP_<LANG>_BIN (tests, stubs).
#
# VOGT_LSP_DEBUG=1 prints the resolved root and interpreter to stderr. It never
# prints environment values.
set -euo pipefail

usage() {
  echo "usage: vogt-lsp <python|rust|java>" >&2
  exit 64
}

[[ $# -eq 1 ]] || usage
lang="$1"
case "${lang}" in
  python | rust | java) ;;
  *) usage ;;
esac

debug() {
  if [[ "${VOGT_LSP_DEBUG:-}" == 1 ]]; then
    echo "vogt-lsp: $*" >&2
  fi
}

# Secret denylist. KEEP IN SYNC with `is_secret_env` / `sanitized_child_env` in
# engine/server/src/pty.rs (the `is_secret_env` function): ENGINE_*,
# INFISICAL_*, TAILSCALE_AUTH_KEY, and any name containing TOKEN, SECRET,
# PASSWORD or API_KEY (case-insensitive). This is the only copy in this script.
is_secret_env() {
  local k="${1^^}"
  [[ "${k}" == ENGINE_* || "${k}" == INFISICAL_* || "${k}" == TAILSCALE_AUTH_KEY \
    || "${k}" == *TOKEN* || "${k}" == *SECRET* || "${k}" == *PASSWORD* \
    || "${k}" == *API_KEY* ]]
}

sanitize_env() {
  local name
  # Names only; values are never read into the shell's output.
  while IFS= read -r name; do
    if is_secret_env "${name}" || [[ "${name}" == UV_* ]]; then
      unset "${name}"
    fi
  done < <(compgen -e)
  unset VIRTUAL_ENV PYTHONPATH
}

# --- root resolution -------------------------------------------------------

start="$(pwd -P)"

# Upper bound: the git toplevel and/or the workspace root, whichever is
# tighter. With neither, the cwd itself is the bound (no walking). A cwd outside
# the workspace root is never walked.
bound=""
if top="$(git -C "${start}" rev-parse --show-toplevel 2>/dev/null)"; then
  bound="$(cd "${top}" && pwd -P)"
fi
if [[ -n "${VOGT_LSP_WORKSPACE_ROOT:-}" ]]; then
  ws="$(cd "${VOGT_LSP_WORKSPACE_ROOT}" 2>/dev/null && pwd -P)" || ws=""
  if [[ -n "${ws}" ]]; then
    if [[ "${start}" != "${ws}" && "${start}" != "${ws}"/* ]]; then
      bound="${start}"
    elif [[ -z "${bound}" || "${bound}" == "${ws}"/* ]]; then
      : # the git toplevel is already inside the workspace root (or no toplevel
      # and the workspace root bounds below)
      [[ -n "${bound}" ]] || bound="${ws}"
    else
      bound="${ws}"
    fi
  fi
fi
[[ -n "${bound}" ]] || bound="${start}"

case "${lang}" in
  python) markers=(pyproject.toml setup.cfg .git) ;;
  rust) markers=(Cargo.toml) ;;
  java) markers=(pom.xml build.gradle .git) ;;
esac

root=""
probe="${start}"
while :; do
  for m in "${markers[@]}"; do
    if [[ -e "${probe}/${m}" ]]; then
      root="${probe}"
      break 2
    fi
  done
  [[ "${probe}" == "${bound}" || "${probe}" == "/" ]] && break
  probe="$(dirname "${probe}")"
done
[[ -n "${root}" ]] || root="${start}"
debug "root=${root}"

# --- python interpreter ----------------------------------------------------

# Extract pythonPath from pyrightconfig.json or pyproject [tool.basedpyright].
config_python() {
  local v=""
  if [[ -f "${root}/pyrightconfig.json" ]]; then
    v="$(sed -n 's/.*"pythonPath"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' \
      "${root}/pyrightconfig.json" | head -n1)"
  fi
  if [[ -z "${v}" && -f "${root}/pyproject.toml" ]]; then
    v="$(sed -n '/^\[tool\.basedpyright\]/,/^\[/{s/^[[:space:]]*pythonPath[[:space:]]*=[[:space:]]*["'"'"']\([^"'"'"']*\)["'"'"'].*/\1/p}' \
      "${root}/pyproject.toml" | head -n1)"
  fi
  [[ -n "${v}" ]] || return 1
  [[ "${v}" == /* ]] || v="${root}/${v}"
  printf '%s\n' "${v}"
}

resolve_python() {
  local c common main
  if c="$(config_python)" && [[ -x "${c}" ]]; then
    printf '%s\n' "${c}"
    return 0
  fi
  if [[ -x "${root}/.venv/bin/python" ]]; then
    printf '%s\n' "${root}/.venv/bin/python"
    return 0
  fi
  if common="$(git -C "${root}" rev-parse --path-format=absolute --git-common-dir 2>/dev/null)"; then
    main="$(dirname "${common}")"
    if [[ "${main}" != "${root}" && -x "${main}/.venv/bin/python" ]]; then
      printf '%s\n' "${main}/.venv/bin/python"
      return 0
    fi
  fi
  if command -v uv >/dev/null 2>&1 && c="$(uv python find 2>/dev/null)" && [[ -n "${c}" ]]; then
    printf '%s\n' "${c}"
    return 0
  fi
  return 1
}

# --- server binary ---------------------------------------------------------

case "${lang}" in
  python) bin="${VOGT_LSP_PYTHON_BIN:-/opt/basedpyright/bin/basedpyright-langserver}" ;;
  rust) bin="${VOGT_LSP_RUST_BIN:-/opt/rust-analyzer/bin/rust-analyzer}" ;;
  java) bin="${VOGT_LSP_JAVA_BIN:-/opt/jdtls/bin/jdtls}" ;;
esac
if [[ ! -x "${bin}" ]]; then
  echo "vogt-lsp: ${lang} language server not found or not executable at ${bin} (override with VOGT_LSP_${lang^^}_BIN)" >&2
  exit 127
fi

args=()
case "${lang}" in
  python)
    args=(--stdio)
    if interp="$(resolve_python)"; then
      debug "interpreter=${interp}"
      export VOGT_LSP_PYTHON_PATH="${interp}"
    else
      debug "interpreter=none"
      unset VOGT_LSP_PYTHON_PATH
    fi
    ;;
  rust)
    args=(--config cargo.targetDir=true)
    ;;
  java)
    hash="$(printf '%s' "${root}" | sha256sum | cut -d' ' -f1)"
    data="${HOME}/.cache/vogt-lsp/jdtls/${hash}"
    mkdir -p "${data}"
    debug "jdtls data=${data}"
    args=(-data "${data}")
    ;;
esac

sanitize_env
cd "${root}"
exec "${bin}" "${args[@]}"
