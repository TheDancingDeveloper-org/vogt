#!/usr/bin/env bash
# Write one Firebase config, provided as an environment value, into a private,
# short-lived build file and validate the Android package it is meant to build.
#
# Generic and broker-free: the workflow passes the config through a plain
# GitHub Actions secret (VOGT_FIREBASE_JSON) and names the package it expects.
# The value is never echoed, uploaded as a standalone artifact, or committed.

set -euo pipefail

: "${VOGT_FIREBASE_JSON:?VOGT_FIREBASE_JSON is required}"
: "${VOGT_FIREBASE_OUTPUT:?VOGT_FIREBASE_OUTPUT is required}"
: "${VOGT_ANDROID_EXPECTED_PACKAGE:?VOGT_ANDROID_EXPECTED_PACKAGE is required}"

output_dir="$(dirname -- "$VOGT_FIREBASE_OUTPUT")"
mkdir -p -- "$output_dir"
umask 077

temp_output="${VOGT_FIREBASE_OUTPUT}.tmp.$$"
trap 'rm -f -- "$temp_output"' EXIT

# The value arrives through the environment, so it is written without ever
# being echoed to the log.
printf '%s' "$VOGT_FIREBASE_JSON" >"$temp_output"

python3 - "$temp_output" "$VOGT_ANDROID_EXPECTED_PACKAGE" <<'PY'
from __future__ import annotations

import json
import sys
from pathlib import Path

path = Path(sys.argv[1])
expected_package = sys.argv[2]
try:
    document = json.loads(path.read_text(encoding="utf-8"))
except (OSError, json.JSONDecodeError) as exc:
    raise SystemExit(f"Firebase config is not valid JSON: {exc}") from exc

packages = {
    client.get("client_info", {}).get("android_client_info", {}).get("package_name")
    for client in document.get("client", [])
}
if expected_package not in packages:
    raise SystemExit(
        f"Firebase config has no Android client for {expected_package}"
    )
PY

mv -- "$temp_output" "$VOGT_FIREBASE_OUTPUT"
