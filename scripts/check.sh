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
# no `engine/` and no `web/`, and that is the whole point of a core-only
# checkout rather than a broken environment.
#
# This script is the local floor for what CI runs, not a mirror of it: a CI
# job may legitimately do more, so keep the commands below in step with
# `ci.yml` by hand.
# Product scripts ported to Rust (WI-1079), run from the checkout as:
#   vogt-core check-product-version [EXPECTED]
#   vogt-core check-docs
#   vogt-core gen-config-docs
#   vogt-core sanitise-transcript [--check] INPUT [OUTPUT]
#   vogt-core capture-transcript OUTPUT [--max-bytes N] -- CMD [ARG ...]
# The engine half below runs the Rust copies. The Python originals stay
# because the python half below and the docs, ci and release workflows still
# call them: those jobs have no Rust toolchain, and the docs and release jobs
# run on a core-only checkout that has no engine/core to build.
#
# Still Python, deliberately (WI-1079). Each is a developer tool that shells
# out to git, gh or a running stack, and none is called by a Rust job:
#   load.py            registry-driven load generator (needs the Python core)
#   fake_agent_core.py deterministic agent stand-in for agent-task tests / e2e
#   fixture_reset.py   rebuilds the forge fixture repo over the GitHub API
# CI-only scripts stay Python and run under a uv-less python3 on the runner
# with no project dependency:
#   play_preflight, check_workflow_policy, check_bundle, check_rust_version,
#   check_engine_spec, check_demo_assets, call_latency.

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
        run "check_docs" cargo run --quiet --manifest-path engine/core/Cargo.toml --bin vogt-core -- check-docs
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
        # The product scripts ported in WI-1079, from the checkout itself.
        run "check-product-version" cargo run --quiet --manifest-path engine/core/Cargo.toml --bin vogt-core -- check-product-version
        run "check-docs" cargo run --quiet --manifest-path engine/core/Cargo.toml --bin vogt-core -- check-docs
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
