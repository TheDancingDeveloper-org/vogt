#!/usr/bin/env bash
# Fetch one Infisical secret into a private, short-lived build file.
#
# This is intentionally generic: the workflow chooses the secret name and
# validates the Android package it is meant to build. The value is never
# echoed, uploaded as a standalone artifact, or committed.

set -euo pipefail

: "${INFISICAL_API_URL:?INFISICAL_API_URL is required}"
: "${INFISICAL_CLIENT_ID:?INFISICAL_CLIENT_ID is required}"
: "${INFISICAL_CLIENT_SECRET:?INFISICAL_CLIENT_SECRET is required}"
: "${INFISICAL_PROJECT_ID:?INFISICAL_PROJECT_ID is required}"
: "${INFISICAL_ENV:?INFISICAL_ENV is required}"
: "${VOGT_FIREBASE_SECRET_NAME:?VOGT_FIREBASE_SECRET_NAME is required}"
: "${VOGT_FIREBASE_OUTPUT:?VOGT_FIREBASE_OUTPUT is required}"
: "${VOGT_ANDROID_EXPECTED_PACKAGE:?VOGT_ANDROID_EXPECTED_PACKAGE is required}"

command -v infisical >/dev/null 2>&1 || {
  echo "infisical CLI is required on the self-hosted runner" >&2
  exit 1
}

output_dir="$(dirname -- "$VOGT_FIREBASE_OUTPUT")"
mkdir -p -- "$output_dir"
umask 077

temp_home="$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/vogt-infisical.XXXXXXXX")"
temp_output="${VOGT_FIREBASE_OUTPUT}.tmp.$$"
cleanup() {
  rm -rf -- "$temp_home"
  rm -f -- "$temp_output"
}
trap cleanup EXIT

access_token="$({
  HOME="$temp_home" infisical login \
    --method universal-auth \
    --domain "$INFISICAL_API_URL" \
    --client-id "$INFISICAL_CLIENT_ID" \
    --client-secret "$INFISICAL_CLIENT_SECRET" \
    --plain --silent
})"

test -n "$access_token" || {
  echo "Infisical login returned an empty access token" >&2
  exit 1
}

# Pass the access token by env, not `--token` on argv (#524.4): argv is
# visible in `ps`/`/proc/*/cmdline` to any other process on the runner. The
# CLI reads INFISICAL_TOKEN as the fallback for `--token`.
INFISICAL_TOKEN="$access_token" \
infisical secrets get "$VOGT_FIREBASE_SECRET_NAME" \
  --domain "$INFISICAL_API_URL" \
  --projectId "$INFISICAL_PROJECT_ID" \
  --env "$INFISICAL_ENV" \
  --plain --silent >"$temp_output"

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
    raise SystemExit(f"fetched Firebase config is not valid JSON: {exc}") from exc

packages = {
    client.get("client_info", {}).get("android_client_info", {}).get("package_name")
    for client in document.get("client", [])
}
if expected_package not in packages:
    raise SystemExit(
        f"fetched Firebase config has no Android client for {expected_package}"
    )
PY

mv -- "$temp_output" "$VOGT_FIREBASE_OUTPUT"
trap - EXIT
rm -rf -- "$temp_home"
echo "Fetched and validated $VOGT_FIREBASE_SECRET_NAME for $VOGT_ANDROID_EXPECTED_PACKAGE"
