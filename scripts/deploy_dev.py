#!/usr/bin/env python3
"""Pin and deploy Vogt's dev stack through its configured Komodo resource.

Komodo owns the credentials for the operator's private deployment repository, so
the GitHub workflow does not need a Forgejo token or a GitHub App.  This script
uses Komodo's file-write API to keep the desired state in Git, then deploys
the exact digests that CI verified.
"""

from __future__ import annotations

import json
import os
import re
import sys
import time
from pathlib import Path
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen

STACK = ""
CORE_IMAGE = "ghcr.io/thedancingdeveloper-org/vogt"
STACK_IMAGE = "ghcr.io/thedancingdeveloper-org/vogt-stack"
SHA256 = re.compile(r"sha256:[0-9a-f]{64}\Z")


class KomodoError(RuntimeError):
    """A safe-to-report Komodo API failure."""


def required(name: str) -> str:
    value = os.environ.get(name, "")
    if not value:
        raise KomodoError(f"{name} is required")
    return value


def api(endpoint: str, payload: dict[str, object]) -> dict[str, object]:
    body = json.dumps(payload).encode("utf-8")
    request = Request(
        f"{required('KOMODO_URL').rstrip('/')}/{endpoint}",
        data=body,
        headers={
            "X-Api-Key": required("KOMODO_API_KEY"),
            "X-Api-Secret": required("KOMODO_API_SECRET"),
            "Content-Type": "application/json",
        },
        method="POST",
    )
    try:
        with urlopen(request, timeout=30) as response:
            result = json.load(response)
    except (HTTPError, URLError, TimeoutError) as exc:
        raise KomodoError(f"Komodo {endpoint} failed: {exc}") from exc
    if not isinstance(result, dict):
        raise KomodoError(f"Komodo {endpoint} returned a non-object response")
    if result.get("success") is False:
        raise KomodoError(f"Komodo {endpoint} reported failure")
    return result


def output(name: str, value: str) -> None:
    destination = os.environ.get("GITHUB_OUTPUT")
    if destination:
        with Path(destination).open("a", encoding="utf-8") as handle:
            handle.write(f"{name}={value}\n")


def write_smoke_token(stack: dict[str, object]) -> None:
    """Pass the active front-door token through a private runner-temp file."""
    runner_temp = os.environ.get("RUNNER_TEMP")
    if not runner_temp:
        return
    config = stack.get("config")
    if not isinstance(config, dict):
        raise KomodoError("Komodo stack response has no config")
    environment = config.get("environment")
    if not isinstance(environment, str):
        raise KomodoError("Komodo stack response has no environment")
    # Komodo stores this field as an escaped .env document in its API response.
    environment = environment.replace("\\n", "\n")
    match = re.search(r"(?m)^MYDEVENV2_TOKEN=(.*)$", environment)
    if not match or not match.group(1):
        raise KomodoError("Komodo dev stack has no MYDEVENV2_TOKEN")
    token_path = Path(runner_temp) / "vogt-dev-smoke-token"
    token_path.write_text(match.group(1), encoding="utf-8")
    token_path.chmod(0o600)


def replace_image(
    contents: str, image: str, digest: str, path: str
) -> tuple[str, bool]:
    pattern = re.compile(rf"({re.escape(image)})@sha256:[0-9a-f]{{64}}")
    updated, count = pattern.subn(rf"\g<1>@{digest}", contents)
    if count != 1:
        raise KomodoError(f"{path} must contain exactly one {image} digest pin")
    return updated, updated != contents


def remote_files(stack: dict[str, object]) -> dict[str, str]:
    info = stack.get("info")
    if not isinstance(info, dict):
        raise KomodoError("Komodo stack response has no info")
    contents = info.get("remote_contents")
    if not isinstance(contents, list):
        raise KomodoError("Komodo did not return the stack's remote files")
    files: dict[str, str] = {}
    for item in contents:
        if isinstance(item, dict) and isinstance(item.get("path"), str):
            value = item.get("contents")
            if isinstance(value, str):
                files[item["path"]] = value
    return files


def update_file(path: str, contents: str) -> None:
    api(
        "write/WriteStackFileContents",
        {"stack": STACK, "file_path": path, "contents": contents},
    )


def update_config(webhook_enabled: bool) -> None:
    api(
        "write/UpdateStack",
        {"id": STACK, "config": {"webhook_enabled": webhook_enabled}},
    )


def update_id(result: dict[str, object]) -> str:
    raw = result.get("_id")
    if isinstance(raw, dict) and isinstance(raw.get("$oid"), str):
        value = raw.get("$oid")
        if isinstance(value, str):
            return value
    if isinstance(raw, str):
        return raw
    value = result.get("id")
    if isinstance(value, str):
        return value
    raise KomodoError("Komodo deploy did not return an update id")


def deploy() -> str:
    result = api("execute/DeployStack", {"stack": STACK})
    deployment_id = update_id(result)
    for _ in range(180):
        current = api("read/GetUpdate", {"id": deployment_id})
        status = current.get("status")
        if status != "InProgress":
            if current.get("success") is not True:
                raise KomodoError("Komodo deployment did not succeed")
            return deployment_id
        time.sleep(5)
    raise KomodoError("Komodo deployment did not finish within 15 minutes")


def main() -> int:
    global STACK
    STACK = required("KOMODO_STACK")
    source_sha = required("SOURCE_SHA")
    core_digest = required("CORE_DIGEST")
    stack_digest = required("STACK_DIGEST")
    for name, digest in (("CORE_DIGEST", core_digest), ("STACK_DIGEST", stack_digest)):
        if not SHA256.fullmatch(digest):
            raise KomodoError(f"{name} is not an immutable sha256 digest")

    stack = api("read/GetStack", {"stack": STACK})
    config = stack.get("config")
    if not isinstance(config, dict):
        raise KomodoError("Komodo stack response has no config")
    original_webhook = bool(config.get("webhook_enabled", True))
    files = remote_files(stack)
    changed: list[tuple[str, str]] = []
    for path, image, digest in (
        ("vogt.compose.yml", CORE_IMAGE, core_digest),
        ("estate.overlay.yml", STACK_IMAGE, stack_digest),
    ):
        if path not in files:
            raise KomodoError(f"Komodo stack is missing active file {path}")
        updated, did_change = replace_image(files[path], image, digest, path)
        if did_change:
            changed.append((path, updated))

    webhook_disabled = False
    try:
        if changed and original_webhook:
            update_config(False)
            webhook_disabled = True
        for path, contents in changed:
            update_file(path, contents)
        api("write/RefreshStackCache", {"stack": STACK})
        deployment_id = deploy()
    finally:
        if webhook_disabled:
            update_config(True)

    latest = api("read/GetStack", {"stack": STACK})
    write_smoke_token(latest)
    latest_info = latest.get("info")
    latest_hash = ""
    if isinstance(latest_info, dict) and isinstance(
        latest_info.get("latest_hash"), str
    ):
        latest_hash = latest_info["latest_hash"]
    output("deployment_id", deployment_id)
    output("desired_state_commit", latest_hash)
    print(f"deployed {STACK} for {source_sha}: {deployment_id}")
    if changed:
        print("updated: " + ", ".join(path for path, _ in changed))
    else:
        print("desired state already had both requested digests")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except KomodoError as exc:
        print(f"error: {exc}", file=sys.stderr)
        raise SystemExit(1) from exc
