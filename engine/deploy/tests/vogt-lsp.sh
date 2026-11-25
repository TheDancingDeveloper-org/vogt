#!/usr/bin/env bash
# WI-4745: vogt-lsp resolves the project root within its bounds, picks the
# Python interpreter in order, strips secrets and Python env hints from the
# server's environment, hashes the jdtls data dir per root, passes the
# rust-analyzer flag and fails clearly on a missing binary. No network, no
# image: temp git repos and stub servers. Only fake values are used.
# VOGT_LSP_SCRIPT overrides the script under test (to show the test fails
# when a guarded line is removed).
set -euo pipefail

script="${VOGT_LSP_SCRIPT:-$(cd "$(dirname "$0")/.." && pwd)/vogt-lsp.sh}"
testdir="$(cd "$(mktemp -d)" && pwd -P)"
trap 'rm -rf "$testdir"' EXIT
fail() { echo "FAIL: $*" >&2; exit 1; }

export HOME="$testdir/home"
mkdir -p "$HOME"
unset VOGT_LSP_WORKSPACE_ROOT VOGT_LSP_DEBUG VIRTUAL_ENV PYTHONPATH

# Stub server: records cwd, args and the sorted names of its environment (and
# the value of the two variables the test needs to compare) into $OUT.
stub="$testdir/stub"
cat >"$stub" <<'STUB'
#!/usr/bin/env bash
{
  echo "cwd=$(pwd -P)"
  echo "args=$*"
  echo "interp=${VOGT_LSP_PYTHON_PATH:-}"
  echo "keep=${KEEP_ME:-}"
  while IFS= read -r n; do echo "name=$n"; done < <(compgen -e)
} >"$OUT"
STUB
chmod +x "$stub"
export OUT="$testdir/out"
export VOGT_LSP_PYTHON_BIN="$stub" VOGT_LSP_RUST_BIN="$stub" VOGT_LSP_JAVA_BIN="$stub"

run() { # run <dir> <lang>
  (cd "$1" && bash "$script" "$2")
}
field() { sed -n "s/^$1=//p" "$OUT" | head -n1; }
has_name() { grep -qx "name=$1" "$OUT"; }

mkrepo() {
  git init -q "$1"
  git -C "$1" -c user.email=t@example.com -c user.name=t commit -q --allow-empty -m init
}

# 1. root resolution --------------------------------------------------------
mkrepo "$testdir/r1"
mkdir -p "$testdir/r1/pkg/src/deep"
: >"$testdir/r1/pkg/pyproject.toml"
run "$testdir/r1/pkg/src/deep" python
[[ "$(field cwd)" == "$testdir/r1/pkg" ]] || fail "python root is '$(field cwd)'"

# no marker below the toplevel: stops at the git toplevel (.git marker)
mkdir -p "$testdir/r1/other/x"
run "$testdir/r1/other/x" python
[[ "$(field cwd)" == "$testdir/r1" ]] || fail "toplevel stop gave '$(field cwd)'"

# bounded by the git toplevel: a Cargo.toml above the repo is never found
: >"$testdir/Cargo.toml"
run "$testdir/r1/other/x" rust
[[ "$(field cwd)" == "$testdir/r1/other/x" ]] || fail "rust walked above the toplevel: '$(field cwd)'"
rm "$testdir/Cargo.toml"

# bounded by the workspace root: a marker above it is never used
mkdir -p "$testdir/ws/sub/leaf"
: >"$testdir/ws/Cargo.toml"
mkdir -p "$testdir/ws/inner/leaf"
: >"$testdir/ws/inner/../pom.xml"
(cd "$testdir/ws/inner/leaf" && VOGT_LSP_WORKSPACE_ROOT="$testdir/ws/inner" bash "$script" java)
[[ "$(field cwd)" == "$testdir/ws/inner/leaf" ]] || fail "java walked above workspace root: '$(field cwd)'"
(cd "$testdir/ws/sub/leaf" && VOGT_LSP_WORKSPACE_ROOT="$testdir/ws/sub" bash "$script" rust)
[[ "$(field cwd)" == "$testdir/ws/sub/leaf" ]] || fail "rust walked above workspace root: '$(field cwd)'"
# a workspace root that contains the marker lets the same walk find it
(cd "$testdir/ws/sub/leaf" && VOGT_LSP_WORKSPACE_ROOT="$testdir/ws" bash "$script" rust)
[[ "$(field cwd)" == "$testdir/ws" ]] || fail "workspace-root rust walk gave '$(field cwd)'"

# workspace root tighter than the git toplevel: a marker between them is ignored
mkrepo "$testdir/r2"
mkdir -p "$testdir/r2/a/b"
: >"$testdir/r2/Cargo.toml"
(cd "$testdir/r2/a/b" && VOGT_LSP_WORKSPACE_ROOT="$testdir/r2/a" bash "$script" rust)
[[ "$(field cwd)" == "$testdir/r2/a/b" ]] || fail "rust walked above workspace root inside a repo: '$(field cwd)'"
(cd "$testdir/r2/a/b" && bash "$script" rust)
[[ "$(field cwd)" == "$testdir/r2" ]] || fail "rust walk inside repo gave '$(field cwd)'"

# 2. python interpreter order ----------------------------------------------
fakepy() { mkdir -p "$(dirname "$1")"; printf '#!/bin/sh\n' >"$1"; chmod +x "$1"; }

# bin dir with a fake uv, so case 4 needs no real uv
mkdir -p "$testdir/bin"
cat >"$testdir/bin/uv" <<UV
#!/bin/sh
[ "\$1 \$2" = "python find" ] && echo "$testdir/uv-python" && exit 0
exit 1
UV
chmod +x "$testdir/bin/uv"
fakepy "$testdir/uv-python"

mkrepo "$testdir/main"
git -C "$testdir/main" worktree add -q "$testdir/wt" -b wt
: >"$testdir/wt/pyproject.toml"
export PATH="$testdir/bin:$PATH"

# 4. uv python find (nothing else present)
run "$testdir/wt" python
[[ "$(field interp)" == "$testdir/uv-python" ]] || fail "uv case gave '$(field interp)'"

# 3. main checkout's venv via git-common-dir
fakepy "$testdir/main/.venv/bin/python"
run "$testdir/wt" python
[[ "$(field interp)" == "$testdir/main/.venv/bin/python" ]] || fail "main venv case gave '$(field interp)'"

# 2. .venv in the root wins over the main checkout's
fakepy "$testdir/wt/.venv/bin/python"
run "$testdir/wt" python
[[ "$(field interp)" == "$testdir/wt/.venv/bin/python" ]] || fail ".venv case gave '$(field interp)'"

# 1. project config wins over .venv (pyproject, then pyrightconfig.json)
fakepy "$testdir/cfg/py"
cat >"$testdir/wt/pyproject.toml" <<TOML
[project]
name = "x"
[tool.basedpyright]
pythonPath = "$testdir/cfg/py"
TOML
run "$testdir/wt" python
[[ "$(field interp)" == "$testdir/cfg/py" ]] || fail "pyproject config gave '$(field interp)'"
: >"$testdir/wt/pyproject.toml"
fakepy "$testdir/wt/rel/py"
printf '{ "pythonPath": "rel/py" }\n' >"$testdir/wt/pyrightconfig.json"
run "$testdir/wt" python
[[ "$(field interp)" == "$testdir/wt/rel/py" ]] || fail "pyrightconfig gave '$(field interp)'"

# debug mode prints the interpreter to stderr; normal mode stays quiet
err="$(cd "$testdir/wt" && VOGT_LSP_DEBUG=1 bash "$script" python 2>&1 >/dev/null)"
[[ "$err" == *"interpreter=$testdir/wt/rel/py"* ]] || fail "debug did not print interpreter"
err="$(cd "$testdir/wt" && bash "$script" python 2>&1 >/dev/null)"
[[ -z "$err" ]] || fail "non-debug run wrote to stderr"

# 5. environment stripping --------------------------------------------------
(
  cd "$testdir/wt"
  export VIRTUAL_ENV=/fake/venv PYTHONPATH=/fake/pp UV_PROJECT_ENVIRONMENT=/fake UV_CACHE_DIR=/fake
  export ENGINE_TOKEN=fake1 ENGINE_FOO=fake2 INFISICAL_CLIENT_ID=fake3 TAILSCALE_AUTH_KEY=fake4
  export GH_TOKEN=fake5 MY_SECRET=fake6 DB_PASSWORD=fake7 SOME_API_KEY=fake8 lower_token=fake9
  export KEEP_ME=normal
  bash "$script" python
)
for n in VIRTUAL_ENV PYTHONPATH UV_PROJECT_ENVIRONMENT UV_CACHE_DIR ENGINE_TOKEN ENGINE_FOO \
  INFISICAL_CLIENT_ID TAILSCALE_AUTH_KEY GH_TOKEN MY_SECRET DB_PASSWORD SOME_API_KEY lower_token; do
  ! has_name "$n" || fail "$n reached the server environment"
done
[[ "$(field keep)" == normal ]] || fail "normal variable was dropped"
has_name PATH || fail "PATH was dropped"

# 6. jdtls data dir: stable per root, different across roots ----------------
mkdir -p "$testdir/j1" "$testdir/j2"
git init -q "$testdir/j1"
git init -q "$testdir/j2"
run "$testdir/j1" java
a1="$(field args)"
run "$testdir/j1" java
[[ "$(field args)" == "$a1" ]] || fail "jdtls args not stable"
want="$HOME/.cache/vogt-lsp/jdtls/$(printf '%s' "$testdir/j1" | sha256sum | cut -d' ' -f1)"
[[ "$a1" == "-data $want" ]] || fail "jdtls args '$a1', want '-data $want'"
[[ -d "$want" ]] || fail "jdtls data dir not created"
run "$testdir/j2" java
[[ "$(field args)" != "$a1" ]] || fail "jdtls data dir shared across roots"

# 7. rust-analyzer flag ------------------------------------------------------
mkdir -p "$testdir/rs"
: >"$testdir/rs/Cargo.toml"
run "$testdir/rs" rust
[[ "$(field args)" == *"cargo.targetDir=true"* ]] || fail "rust flag missing: '$(field args)'"

# 8. missing binary and bad usage -------------------------------------------
rc=0
err="$(cd "$testdir/rs" && VOGT_LSP_RUST_BIN="$testdir/nope" bash "$script" rust 2>&1)" || rc=$?
[[ "$rc" != 0 && "$err" == *"not found"*"$testdir/nope"* ]] || fail "missing binary rc=$rc err='$err'"
rc=0
bash "$script" cobol >/dev/null 2>&1 || rc=$?
[[ "$rc" != 0 ]] || fail "unknown language accepted"

echo "vogt-lsp: ok"
