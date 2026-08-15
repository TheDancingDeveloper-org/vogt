#!/usr/bin/env bash
# Run what CI runs, in one command.
#
# This exists because the alternative is remembering four commands per half
# and being right about all of them every time — and on 2026-08-14 somebody
# ran `ruff check` and not `ruff format --check`, which are two commands that
# feel like one, and pushed a red build. The check that catches you is not
# the one you remember to run.
#
#     scripts/check.sh            # every half this checkout has
#     scripts/check.sh python     # one half
#     scripts/check.sh python web engine
#
# A half whose tree is absent is skipped and said so: a core-only checkout has
# no `engine/` and no `web/`, and that is NFR-Q6's whole point rather than a
# broken environment.
#
# `tests/test_deploy.py::test_the_local_check_runs_what_ci_runs` asserts that
# every command below appears in `ci.yml`. It cannot assert the reverse — a
# job may legitimately do more — so this script is a floor, not a mirror.
set -uo pipefail

# `|| exit` because this script sets `-uo pipefail` and deliberately not
# `-e`: without the guard a failed cd is silent, and every check below
# then runs against whatever directory we happen to be in and passes.
cd "$(dirname "$0")/.." || exit 1

halves=("$@")
if [ ${#halves[@]} -eq 0 ]; then
    halves=(python engine web)
fi

failures=()

run() {
    local label="$1"
    shift
    printf '\n\033[1m── %s\033[0m\n' "$label"
    if "$@"; then
        return 0
    fi
    failures+=("$label")
    return 1
}

for half in "${halves[@]}"; do
    case "$half" in
    python)
        run "ruff check" uv run ruff check .
        run "ruff format" uv run ruff format --check .
        run "mypy" uv run mypy
        run "check_docs" uv run python scripts/check_docs.py
        run "pytest" uv run pytest
        ;;
    engine)
        if [ ! -d engine ]; then
            echo "engine: absent from this checkout, skipped"
            continue
        fi
        run "cargo fmt" cargo fmt --all --manifest-path engine/Cargo.toml -- --check
        run "cargo clippy" cargo clippy --manifest-path engine/Cargo.toml \
            --workspace --all-targets -- -D warnings
        run "cargo test" cargo test --manifest-path engine/Cargo.toml --workspace
        ;;
    web)
        if [ ! -d web ]; then
            echo "web: absent from this checkout, skipped"
            continue
        fi
        run "pnpm typecheck" pnpm --dir web typecheck
        run "pnpm test" pnpm --dir web test
        ;;
    *)
        echo "unknown half: $half (expected python, engine or web)" >&2
        exit 64 # EX_USAGE
        ;;
    esac
done

echo
if [ ${#failures[@]} -gt 0 ]; then
    printf '\033[31mfailed: %s\033[0m\n' "${failures[*]}" >&2
    exit 1
fi
echo "all checks passed"
