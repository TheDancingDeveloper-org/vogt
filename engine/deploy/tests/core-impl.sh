#!/usr/bin/env bash
# WI-2660: VOGT_CORE_IMPL selects the supervised core. Default is python.
# This does not boot the image; it sources the selector the entrypoint uses
# (extracted by running the function definitions) and asserts the contract:
# unset and python name the Python console script, rust names vogt-core, and
# any other value fails closed.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
entry="$root/entrypoint.sh"

# Pull just the selector so the rest of the entrypoint (agent CLIs, exec) does
# not run. The function is delimited by its name and the next top-level def.
selector="$(awk '
  /^link_vogt_names\(\)/ {p=1}
  /^vogt_core_bin\(\)/ {p=1}
  p {print}
  p && /^}$/ {p=0}
' "$entry")"
# shellcheck disable=SC1090
eval "$selector"

fail() { echo "FAIL: $*" >&2; exit 1; }

export VOGT_PYTHON_BIN=/opt/vogt/fake-python-vogt
unset VOGT_CORE_IMPL || true
got="$(vogt_core_bin)" || fail "default impl should succeed"
[[ "$got" == /opt/vogt/fake-python-vogt ]] || fail "default got $got"

export VOGT_CORE_IMPL=python
got="$(vogt_core_bin)" || fail "python impl should succeed"
[[ "$got" == /opt/vogt/fake-python-vogt ]] || fail "python got $got"

export VOGT_CORE_IMPL=rust
got="$(vogt_core_bin)" || fail "rust impl should succeed"
[[ "$got" == vogt-core ]] || fail "rust got $got"

export VOGT_CORE_IMPL=Rust
if vogt_core_bin >/dev/null 2>/tmp/wi2660-impl.err; then
  fail "invalid impl should fail"
fi
grep -q "not python or rust" /tmp/wi2660-impl.err || fail "invalid impl message"

unset VOGT_PYTHON_BIN
export VOGT_CORE_IMPL=python
# The image default path may exist on a developer host. Hide it for this case.
if [[ -x /opt/vogt/.venv/bin/vogt ]]; then
  echo "core-impl: skip missing-python case (image venv is present)"
else
  if vogt_core_bin >/dev/null 2>/tmp/wi2660-impl.err; then
    fail "python without a console script should fail"
  fi
fi

# Operator names follow the same function. VOGT_BIN_DIR keeps the links out
# of /usr/local/bin; unset it and the entrypoint writes there.
bindir="$(mktemp -d)"
trap 'rm -rf "$bindir"' EXIT
export VOGT_BIN_DIR="$bindir"
export VOGT_PYTHON_BIN="$bindir/python-vogt"
export VOGT_PYTHON_MCP_BIN="$bindir/python-mcp"
touch "$VOGT_PYTHON_BIN" "$VOGT_PYTHON_MCP_BIN"
unset VOGT_CORE_IMPL || true
link_vogt_names || fail "default link"
[[ "$(readlink "$bindir/vogt")" == "$VOGT_PYTHON_BIN" ]] || fail "default link target"
[[ "$(readlink "$bindir/vogt-mcp-remote")" == "$VOGT_PYTHON_MCP_BIN" ]] || fail "default mcp target"
export VOGT_CORE_IMPL=rust
export VOGT_RUST_CORE_BIN="$bindir/rust-core"
link_vogt_names || fail "rust link"
[[ "$(readlink "$bindir/vogt")" == "$bindir/rust-core" ]] || fail "rust link target"
[[ "$(readlink "$bindir/vogt-mcp-remote")" == "$bindir/rust-core" ]] || fail "rust mcp target"

echo "core-impl: ok"
