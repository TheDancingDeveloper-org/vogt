#!/usr/bin/env bash
# The CI gates a worker skips by running only its touched tests.
#
# Batches 131-134 each landed and then turned the line red on a gate nobody
# ran locally: cargo deny, the disposition ledger, the engine/MCP route
# parity test, ruff, mypy, or the committed parity golden. This script runs
# those gates in the order the workflows run them and stops at the first
# failure with one line and a log path.
#
#     scripts/pre-land-check.sh              # quick: default, no workspace test
#     scripts/pre-land-check.sh --rust       # add fmt, clippy and nextest
#     scripts/pre-land-check.sh --self-test  # prove a failed gate exits non-zero
#
# The commands are copied from `.github/workflows/rust-core.yml` and
# `.github/workflows/ci.yml`; keep them in step by hand. The parity gate runs
# the committed-golden checks, not CI's record-twice-and-diff determinism
# probe: that probe proves the recorder, not the change, and costs minutes.
#
# A graft working copy has no `.git`, so `scripts/select_parity_golden.sh`
# cannot read its history. This script then picks the newest committed golden
# by `meta.json`'s `recorded_at`, prints that choice, and never tries a
# git-dependent test. A normal checkout uses the selector unchanged.
#
# Environment:
#   CARGO_BUILD_JOBS           cargo parallelism (default 4)
#   PRE_LAND_NICE              nice level for every gate (default 10)
#   PRE_LAND_PARITY_GOLDEN     use this golden directory instead of selecting
#   PRE_LAND_BASE              git base for --rust's changed-file scope
#   PRE_LAND_RUST_PACKAGES     space-separated crates for --rust (when the
#                              committed draft diff cannot be read)
#   PRE_LAND_FORCE_FAIL=<id>   make one gate fail (used by --self-test)

set -uo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "$0")" && pwd) || exit 1
ROOT=$(cd -- "$SCRIPT_DIR/.." && pwd) || exit 1

MODE=quick
SELF_TEST=0
while [ $# -gt 0 ]; do
    case "$1" in
    --quick)
        MODE=quick
        ;;
    --rust)
        MODE=rust
        ;;
    --self-test)
        SELF_TEST=1
        ;;
    -h | --help)
        sed -n '2,30p' "$0" | sed 's/^# \{0,1\}//'
        exit 0
        ;;
    *)
        echo "pre-land: unknown argument: $1 (expected --quick, --rust or --self-test)" >&2
        exit 64
        ;;
    esac
    shift
done

export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-4}"
NICE_LEVEL="${PRE_LAND_NICE:-10}"
NICE=(nice -n "$NICE_LEVEL")
LOG_DIR=$(mktemp -d "${TMPDIR:-/tmp}/vogt-pre-land.XXXXXX") || {
    echo "pre-land: cannot create a log directory" >&2
    exit 1
}

if [ "$SELF_TEST" -eq 1 ]; then
    output=$(PRE_LAND_FORCE_FAIL=cargo-deny "$0" --quick 2>&1)
    rc=$?
    if [ "$rc" -eq 0 ]; then
        echo "self-test: a forced cargo-deny failure exited 0" >&2
        exit 1
    fi
    if ! printf '%s\n' "$output" | grep -q 'FAIL cargo-deny'; then
        echo "self-test: the failure was not reported as FAIL cargo-deny:" >&2
        printf '%s\n' "$output" >&2
        exit 1
    fi
    echo "self-test: ok (forced cargo-deny failure exited $rc)"
    exit 0
fi

# A one-line reason from a captured log: an error-shaped line when there is
# one, otherwise the last line that is not blank.
gate_reason() {
    local log="$1"
    local reason
    reason=$(grep -E '^(error:|Error:|FAILED|Traceback|KeyError|[A-Za-z]+Error|.*: error:)' "$log" | tail -n 1)
    if [ -z "$reason" ]; then
        reason=$(grep -v '^[[:space:]]*$' "$log" | tail -n 1)
    fi
    printf '%s' "${reason:0:240}"
}

run_gate() {
    local gate="$1"
    shift
    if [ "${PRE_LAND_FORCE_FAIL:-}" = "$gate" ]; then
        echo "pre-land: $gate: forced failure (PRE_LAND_FORCE_FAIL)" >&2
        echo "pre-land: FAIL $gate: forced failure" >&2
        exit 1
    fi
    printf 'pre-land: %-14s ... ' "$gate"
    local log="$LOG_DIR/$gate.log"
    "$@" >"$log" 2>&1
    local rc=$?
    if [ "$rc" -eq 0 ]; then
        echo "ok"
        return 0
    fi
    echo "FAILED"
    local reason
    reason=$(gate_reason "$log")
    echo "pre-land: FAIL $gate (exit $rc): ${reason:-no output}" >&2
    echo "pre-land: log: $log" >&2
    exit 1
}

cargo_deny_gate() {
    cd "$ROOT/engine" || return 1
    "${NICE[@]}" cargo deny --all-features check
}

disposition_gate() {
    cd "$ROOT/engine" || return 1
    # The CI step is `cargo test -p vogt-core`; this is its one test, without
    # building every integration target of the package.
    "${NICE[@]}" cargo test -p vogt-core --bin vogt-core every_python_test_has_one_disposition
}

rust_core_image_gate() {
    cd "$ROOT" || return 1
    "${NICE[@]}" python3 scripts/check_rust_core_dockerfile.py
}

engine_parity_gate() {
    cd "$ROOT" || return 1
    uv sync --locked || return 1
    "${NICE[@]}" uv run --no-sync pytest tests/test_engine_parity.py --no-cov -q
}

ruff_check_gate() {
    cd "$ROOT" || return 1
    "${NICE[@]}" uv run --no-sync ruff check .
}

ruff_format_gate() {
    cd "$ROOT" || return 1
    "${NICE[@]}" uv run --no-sync ruff format --check .
}

mypy_gate() {
    cd "$ROOT" || return 1
    "${NICE[@]}" uv run --no-sync mypy
}

golden_for_parity() {
    if [ -n "${PRE_LAND_PARITY_GOLDEN:-}" ]; then
        printf '%s\n' "$PRE_LAND_PARITY_GOLDEN"
        return 0
    fi
    if [ -d "$ROOT/.git" ] && command -v git >/dev/null 2>&1; then
        local selected
        if selected=$(cd "$ROOT" && scripts/select_parity_golden.sh 2>/dev/null); then
            printf '%s\n' "$selected"
            return 0
        fi
        echo "pre-land: no git-selected golden; falling back to recorded_at" >&2
    elif [ -d "$ROOT/.graft" ]; then
        echo "pre-land: graft working copy has no .git; choosing the newest golden by recorded_at" >&2
    fi
    python3 - "$ROOT/tests/parity/golden" <<'PY'
import json
import pathlib
import sys

root = pathlib.Path(sys.argv[1])
best = None
for directory in root.iterdir():
    meta = directory / "meta.json"
    if not meta.is_file():
        continue
    try:
        recorded_at = json.loads(meta.read_text("utf-8"))["recorded_at"]
    except (KeyError, OSError, ValueError):
        continue
    if best is None or recorded_at > best[0]:
        best = (recorded_at, directory)
if best is None:
    sys.exit(1)
print(best[1])
PY
}

parity_gate() {
    cd "$ROOT" || return 1
    local golden
    golden=$(golden_for_parity) || {
        echo "no parity golden found" >&2
        return 1
    }
    echo "using golden $golden" >&2
    export VOGT_PARITY_PYTHON_BIN="$ROOT/.venv/bin/vogt"
    "${NICE[@]}" uv run --no-sync python scripts/parity.py check --impl python --golden "$golden"
    "${NICE[@]}" uv run --no-sync python scripts/parity.py check --impl python --transport http --golden "$golden"
    "${NICE[@]}" uv run --no-sync python scripts/parity.py check --impl python --transport mcp --golden "$golden"
    "${NICE[@]}" uv run --no-sync python scripts/parity.py check --impl python --golden "$golden" --only actor
    "${NICE[@]}" uv run --no-sync python scripts/parity.py check --impl python --golden "$golden" --only schema
    "${NICE[@]}" uv run --no-sync python scripts/parity.py selftest --golden "$golden"
    "${NICE[@]}" uv run --no-sync python scripts/parity.py errors --golden "$golden" --check
    "${NICE[@]}" uv run --no-sync python scripts/parity.py contract --impl python

    if [ ! -x "$ROOT/engine/target/debug/vogt-core" ]; then
        ( cd "$ROOT/engine" && "${NICE[@]}" cargo build --locked -p vogt-core ) || return 1
    fi
    export VOGT_PARITY_RUST_BIN="$ROOT/engine/target/debug/vogt-core"
    "${NICE[@]}" uv run --no-sync python scripts/parity.py check --impl rust --transport http --golden "$golden"
    "${NICE[@]}" uv run --no-sync python scripts/parity.py check --impl rust --transport mcp --golden "$golden"
    "${NICE[@]}" uv run --no-sync python scripts/parity.py check --impl rust --golden "$golden" --only dump
    "${NICE[@]}" uv run --no-sync python scripts/parity.py check --impl rust --golden "$golden" --only schema
    "${NICE[@]}" uv run --no-sync python scripts/parity.py crossopen --writer python --reader rust
    "${NICE[@]}" uv run --no-sync python scripts/parity.py contract --impl rust
}

changed_engine_files() {
    if [ -d "$ROOT/.graft" ] && command -v graft >/dev/null 2>&1; then
        local head base
        head=$(cd "$ROOT" && graft status --json 2>/dev/null | python3 -c \
            'import json,sys; print(json.load(sys.stdin).get("head") or "")' 2>/dev/null)
        if [ -n "$head" ]; then
            base=$(sed -n 's/^base = //p' "$ROOT/.graft/state" 2>/dev/null)
            if [ -n "$base" ]; then
                ( cd "$ROOT" && graft diff --name-only "$base" "$head" 2>/dev/null )
                return
            fi
        fi
        ( cd "$ROOT" && graft diff --name-only 2>/dev/null )
        return
    fi
    if [ -d "$ROOT/.git" ]; then
        local base="${PRE_LAND_BASE:-}"
        if [ -z "$base" ]; then
            base=$(git -C "$ROOT" merge-base HEAD origin/integrate/vogt-next 2>/dev/null ||
                git -C "$ROOT" merge-base HEAD origin/main 2>/dev/null ||
                git -C "$ROOT" rev-parse HEAD~1 2>/dev/null || true)
        fi
        if [ -n "$base" ]; then
            git -C "$ROOT" diff --name-only "$base"
            return
        fi
    fi
    return 1
}

rust_packages() {
    local files="$1"
    local unknown=0
    local found=()
    local file
    while IFS= read -r file; do
        case "$file" in
        engine/core/*) found+=("vogt-core") ;;
        engine/contract/*) found+=("vogt-engine-contract") ;;
        engine/server/*) found+=("vogt-engine-server") ;;
        engine/session-agent/*) found+=("vogt-session-agent") ;;
        engine/*) unknown=1 ;;
        esac
    done <<<"$files"
    if [ "$unknown" -eq 1 ] || [ "${#found[@]}" -eq 0 ]; then
        printf '%s' "--workspace"
        return
    fi
    printf '%s\n' "${found[@]}" | sort -u | sed 's/^/-p /' | tr '\n' ' '
}

rust_gate() {
    cd "$ROOT/engine" || return 1
    "${NICE[@]}" cargo fmt --all -- --check || return 1
    local files scope
    if [ -n "${PRE_LAND_RUST_PACKAGES:-}" ]; then
        scope=""
        # shellcheck disable=SC2086  # a space-separated package list on purpose
        for package in $PRE_LAND_RUST_PACKAGES; do
            scope="$scope -p $package"
        done
    else
        if ! files=$(changed_engine_files); then
            echo "cannot read the changed files (graft auth for a committed draft);" >&2
            echo "set PRE_LAND_RUST_PACKAGES or run --rust before g commit" >&2
            return 1
        fi
        if ! printf '%s\n' "$files" | grep -q '^engine/'; then
            echo "no engine files changed; clippy and nextest skipped" >&2
            return 0
        fi
        scope=$(rust_packages "$files")
    fi
    # shellcheck disable=SC2086  # the -p list is cargo's own words on purpose
    "${NICE[@]}" cargo clippy --locked $scope --all-targets -- -D warnings || return 1
    if ! command -v cargo-nextest >/dev/null 2>&1; then
        echo "cargo-nextest is not installed (cargo install --locked cargo-nextest --version 0.9.103)" >&2
        return 1
    fi
    case " $scope " in
    *" --workspace "* | *" -p vogt-engine-server "*)
        "${NICE[@]}" cargo build --locked -p vogt-core || return 1
        ;;
    esac
    # shellcheck disable=SC2086  # the -p list is cargo's own words on purpose
    "${NICE[@]}" cargo nextest run --locked $scope --no-fail-fast
}

echo "pre-land: $MODE mode, CARGO_BUILD_JOBS=$CARGO_BUILD_JOBS, nice $NICE_LEVEL"

run_gate cargo-deny cargo_deny_gate
run_gate disposition disposition_gate
run_gate rust-core-image rust_core_image_gate
run_gate engine-parity engine_parity_gate
run_gate ruff-check ruff_check_gate
run_gate ruff-format ruff_format_gate
run_gate mypy mypy_gate
run_gate parity parity_gate

if [ "$MODE" = rust ]; then
    run_gate rust rust_gate
fi

echo "pre-land: every gate passed"
