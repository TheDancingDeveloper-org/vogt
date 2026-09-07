#!/bin/sh
# Generic lifecycle contract for the core and engine images.
# Deployment-owned hooks are mounted at VOGT_HOOK_DIR. Each phase is a
# directory of executable files: pre-start.d, post-start.d and post-health.d.
# Files run in lexical order. The image contains this runner, never an
# operator's hook or credential.

set -eu
LC_ALL=C
export LC_ALL

hook_dir=${VOGT_HOOK_DIR:-/run/vogt/hooks}
state_dir=${VOGT_LIFECYCLE_STATE_DIR:-/tmp/vogt-lifecycle}
work_dir=${VOGT_LIFECYCLE_WORKDIR:-$PWD}
required=${VOGT_HOOKS_REQUIRED:-false}

truthy() {
    case "$1" in
        1|true|TRUE|yes|YES|on|ON) return 0 ;;
        *) return 1 ;;
    esac
}

run_phase() {
    phase=$1
    directory=$hook_dir/$phase.d
    if [ ! -d "$hook_dir" ]; then
        if truthy "$required"; then
            echo "vogt lifecycle: required hook bundle is missing: $hook_dir" >&2
            return 78
        fi
        return 0
    fi
    if [ ! -d "$directory" ]; then
        # A bundle may intentionally provide only one or two phases. The
        # required flag makes the bundle itself mandatory; an absent optional
        # phase is a no-op, not a phantom failure.
        return 0
    fi

    set -- "$directory"/*
    for path do
        [ -f "$path" ] || continue
        [ -x "$path" ] || {
            echo "vogt lifecycle: hook must be executable: $path" >&2
            return 78
        }
        echo "vogt lifecycle: running $phase hook $path" >&2
        if (cd "$work_dir" && \
            VOGT_LIFECYCLE_PHASE="$phase" \
            VOGT_LIFECYCLE_HOOK_DIR="$hook_dir" \
            VOGT_LIFECYCLE_WORKDIR="$work_dir" \
            VOGT_LIFECYCLE_FIRST_START="$first_start" \
            "$path"); then
            :
        else
            status=$?
            echo "vogt lifecycle: failed $phase hook $path (status=$status)" >&2
            return "$status"
        fi
        echo "vogt lifecycle: completed $phase hook $path" >&2
    done
}

check_health() {
    url=${VOGT_LIFECYCLE_HEALTHCHECK_URL:?VOGT_LIFECYCLE_HEALTHCHECK_URL is required for health mode}
    # Resolve the interpreter rather than assuming `python`: the images this
    # runs in (the core and the stack pod) ship `python3`, and Debian/Ubuntu
    # do not install a bare `python` alias by default. Assuming `python` made
    # the container healthcheck exit 127 ("python: not found"), so the pod read
    # as unhealthy forever even though the core was serving. An explicit
    # override still wins; otherwise prefer python3, then python, and fail with
    # a named reason rather than a bare 127 if neither exists.
    py=${VOGT_LIFECYCLE_PYTHON:-}
    if [ -z "$py" ]; then
        if command -v python3 >/dev/null 2>&1; then
            py=python3
        elif command -v python >/dev/null 2>&1; then
            py=python
        else
            echo "vogt lifecycle: no python3 or python on PATH for the health check" >&2
            return 127
        fi
    fi
    "$py" - "$url" <<'PY'
import sys
import urllib.request

with urllib.request.urlopen(sys.argv[1], timeout=5) as response:
    if response.status < 200 or response.status >= 300:
        raise SystemExit("health status was %s" % response.status)
PY
}

mkdir -p "$state_dir"
first_start=0
[ -e "$state_dir/started" ] || first_start=1

if [ "${1:-}" = health ]; then
    [ "$#" -eq 1 ] || { echo "vogt lifecycle: health takes no arguments" >&2; exit 64; }
    check_health
    run_phase post-health
    exit 0
fi

[ "$#" -gt 0 ] || { echo "vogt lifecycle: no service command supplied" >&2; exit 64; }
case "$1" in
    -*) set -- vogt "$@" ;;
esac
run_phase pre-start
"$@" &
child=$!
terminate() { kill -TERM "$child" 2>/dev/null || true; }
trap terminate TERM INT

if run_phase post-start; then
    :
else
    status=$?
    echo "vogt lifecycle: post-start failed; stopping service (status=$status)" >&2
    kill -TERM "$child" 2>/dev/null || true
    wait "$child" 2>/dev/null || true
    exit "$status"
fi

# Only successful startup hooks mark the persistent start. A failed restore or
# verify is retried as a first start after the next boot.
if ! : > "$state_dir/started"; then
    echo "vogt lifecycle: cannot persist startup state: $state_dir/started" >&2
    kill -TERM "$child" 2>/dev/null || true
    wait "$child" 2>/dev/null || true
    exit 78
fi
wait "$child"
