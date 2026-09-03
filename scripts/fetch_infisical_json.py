#!/usr/bin/env python3
"""Fetch one JSON secret from Infisical into a private, short-lived build file.

Generic counterpart to ``fetch_infisical_secret.sh`` (which is Firebase-specific,
CLI-based, and validates an Android package). This one:

  * authenticates with a universal-auth machine identity (env, never argv),
  * resolves a project by its human **slug** so the workflow needs no project-id
    variable — the Play service account lives in a different project (``cicd``)
    than the Firebase config (``apps``),
  * fetches ``SECRET_NAME`` from ``INFISICAL_ENV``,
  * validates it parses as JSON (a Google service-account key is JSON),
  * writes it 0600 to ``OUTPUT_PATH``.

The value is never echoed or logged.

Env: INFISICAL_API_URL, INFISICAL_CLIENT_ID, INFISICAL_CLIENT_SECRET,
     INFISICAL_PROJECT_SLUG, INFISICAL_ENV, SECRET_NAME, OUTPUT_PATH.
"""

from __future__ import annotations

import json
import os
import sys
import urllib.parse
import urllib.request
from pathlib import Path


def _require(name: str) -> str:
    value = os.environ.get(name, "")
    if not value:
        raise SystemExit(f"{name} is required")
    return value


def _api(base: str, path: str, token: str | None, payload: object | None) -> object:
    headers = {"Accept": "application/json"}
    if token:
        headers["Authorization"] = f"Bearer {token}"
    data = None
    if payload is not None:
        headers["Content-Type"] = "application/json"
        data = json.dumps(payload).encode("utf-8")
    request = urllib.request.Request(base + path, data=data, headers=headers)
    with urllib.request.urlopen(request, timeout=20) as response:
        loaded: object = json.load(response)
    return loaded


def main() -> int:
    base = _require("INFISICAL_API_URL").rstrip("/")
    if not base.endswith("/api"):
        base += "/api"
    slug = _require("INFISICAL_PROJECT_SLUG")
    env = _require("INFISICAL_ENV")
    key_name = _require("SECRET_NAME")
    output_path = Path(_require("OUTPUT_PATH"))

    login = _api(
        base,
        "/v1/auth/universal-auth/login",
        None,
        {
            "clientId": _require("INFISICAL_CLIENT_ID"),
            "clientSecret": _require("INFISICAL_CLIENT_SECRET"),
        },
    )
    token = login.get("accessToken") if isinstance(login, dict) else None
    if not isinstance(token, str) or not token:
        raise SystemExit("Infisical login returned no access token")

    workspaces = _api(base, "/v1/workspace", token, None)
    entries = workspaces.get("workspaces") if isinstance(workspaces, dict) else None
    workspace_id = None
    for entry in entries or []:
        if isinstance(entry, dict) and entry.get("name") == slug:
            workspace_id = entry.get("id")
            break
    if not isinstance(workspace_id, str):
        raise SystemExit(f"Infisical has no project named {slug!r}")

    query = urllib.parse.urlencode(
        {"workspaceId": workspace_id, "environment": env, "secretPath": "/"}
    )
    result = _api(
        base,
        f"/v3/secrets/raw/{urllib.parse.quote(key_name, safe='')}?{query}",
        token,
        None,
    )
    secret = result.get("secret") if isinstance(result, dict) else None
    value = secret.get("secretValue") if isinstance(secret, dict) else None
    if not isinstance(value, str) or not value:
        raise SystemExit(f"Infisical returned no value for {slug}/{env}/{key_name}")

    try:
        json.loads(value)
    except json.JSONDecodeError as exc:
        raise SystemExit(f"{key_name} is not valid JSON: {exc}") from exc

    output_path.parent.mkdir(parents=True, exist_ok=True)
    # Owner-only, and created without a world-readable window.
    fd = os.open(output_path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w", encoding="utf-8") as handle:
        handle.write(value)
    print(f"Fetched {key_name} from {slug}/{env}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
