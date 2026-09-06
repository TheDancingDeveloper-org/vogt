#!/usr/bin/env python3
"""Pin and deploy Vogt's dev stack through its configured Komodo resource.

Komodo owns the credentials for the operator's private deployment repository, so
the GitHub workflow does not need a Forgejo token or a GitHub App.  This script
uses Komodo's file-write API to keep the desired state in Git, then deploys
the exact digests that CI verified.

Rules this script lives by, each learned the hard way:

* The stack files it rewrites (`vogt.compose.yml`, `estate.overlay.yml`) are
  read from Komodo's *cached* clone of the stack repo. `fresh_stack()`
  refreshes that cache first, so a commit pushed to the repo since the last
  deploy is kept rather than written back over from the stale copy.
* `read/GetStack` returns `config.environment` with newlines escaped;
  `write/UpdateStack` takes the string verbatim. Writing a scraped value back
  collapses the whole `.env` to one line and silently fails the stack's
  pre-deploy hook. `restore_environment` / `repair_environment` exist for that
  case (`REPAIR_ENVIRONMENT=dry-run|apply`); `REPAIR_ENVIRONMENT=inspect` is a
  read-only dump of what Komodo knows.
* Nothing printed may hold a value. `info.deployed_config` is the *resolved*
  compose (every secret interpolated) and is withheld; environment is shown as
  keys only (`env_shape`, `rendered_shape`); every other line goes through
  `redact()`. Workflow logs outlive the run and are readable by everyone with
  access to the repository.
* The live validation accepts the deploy by what the pod reports, not by what
  was sent: `EXPECT_AGENT_CLIS` in the workflow asserts the agent CLI versions
  from `GET /api/agent-clis`.
"""

from __future__ import annotations

import json
import os
import re
import sys
import time
from pathlib import Path
from urllib.error import HTTPError, URLError
from urllib.parse import urlencode
from urllib.request import Request, urlopen

STACK = ""
CORE_IMAGE = "ghcr.io/thedancingdeveloper-org/vogt"
# The dev stack's `.env` stores secrets as `[[infisical://<project>/<env>/<KEY>]]`
# references that Komodo resolves out of band when it injects the container
# environment. `read/GetStack` returns the *unresolved* template, so any secret
# the smoke needs (the front-door token) must be resolved here against Infisical
# before use. See resolve_secret_ref().
INFISICAL_REF = re.compile(
    r"\A\[\[infisical://(?P<project>[^/]+)/(?P<env>[^/]+)/(?P<key>[^\]]+)\]\]\Z"
)
# The private estate package, not the public `vogt-stack`. `dev` and `prod`
# branch builds publish there; the public one carries the generic AIO and has no
# `dev-<sha>` tags to pin. This constant is what rewrites the image line in the
# deployed `estate.overlay.yml`, so a stale value here repins vogt-dev onto a
# digest from the wrong package.
STACK_IMAGE = "ghcr.io/thedancingdeveloper-org/vogt-stack-estate"
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


def api_raw(endpoint: str, payload: dict[str, object]) -> dict[str, object]:
    """Like `api`, but hand back a response whose `success` is false.

    `read/GetUpdate` returns the *update record*, and a finished deployment
    that failed carries `success: false` on that record — the one case where
    the body is exactly what we need to read (its `logs` say why), not an
    API error to throw away.
    """
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
    return result


# Update fields that carry the stack config (environment included).
WITHHELD_UPDATE_FIELDS = frozenset({"prev_toml", "current_toml", "other_data"})
SECRET_KEY = re.compile(r"token|secret|password|passwd|api_key|apikey", re.IGNORECASE)
CRED_URL = re.compile(r"(\w+://)[^/\s@]+@")


def redact(text: str) -> str:
    """Mask credentials embedded in URLs (`scheme://user:pass@host`). Komodo
    holds the desired-state repository credential itself, outside GitHub's
    secret masking, so a clone command echoed into an update log must not
    reach the workflow log intact."""
    return CRED_URL.sub(r"\1***@", text)


def failure_detail(update: dict[str, object], limit: int = 6000) -> str:
    """Render why a Komodo update failed: the update's own fields, then every
    field of each failing stage (a stage can fail with empty stdout/stderr and
    its reason elsewhere), tail-truncated and credential-redacted. Successful
    stages are skipped unless nothing is flagged failed."""
    lines: list[str] = ["Komodo update:"]
    for key, value in update.items():
        if key == "logs":
            continue
        if key in WITHHELD_UPDATE_FIELDS:
            # The stack's serialised config, environment included — never
            # into a workflow log. Its presence and size are enough.
            size = len(value) if isinstance(value, str) else len(repr(value))
            lines.append(f"  {key}=<withheld, {size} chars>")
            continue
        lines.append(f"  {key}={redact(repr(value))[:200]}")
    logs = update.get("logs")
    if isinstance(logs, list):
        entries = [entry for entry in logs if isinstance(entry, dict)]
        failed = [entry for entry in entries if entry.get("success") is False]
        for entry in failed or entries[-1:]:
            stage = entry.get("stage") or entry.get("command") or "?"
            lines.append(f"--- stage: {stage} (success={entry.get('success')})")
            for key, value in entry.items():
                if key in ("stage", "success"):
                    continue
                if SECRET_KEY.search(key):
                    lines.append(f"[{key}] ***")
                elif isinstance(value, str):
                    if not value.strip():
                        continue
                    lines.append(f"[{key}]")
                    lines.extend(
                        redact(line) for line in value.strip().splitlines()[-40:]
                    )
                else:
                    lines.append(f"[{key}] {redact(repr(value))[:300]}")
    rendered = "\n".join(lines)
    return rendered if len(rendered) <= limit else rendered[-limit:]


def output(name: str, value: str) -> None:
    destination = os.environ.get("GITHUB_OUTPUT")
    if destination:
        with Path(destination).open("a", encoding="utf-8") as handle:
            handle.write(f"{name}={value}\n")


def _infisical(
    base: str, method: str, path: str, token: str, payload: object
) -> object:
    headers = {"Accept": "application/json"}
    if token:
        headers["Authorization"] = f"Bearer {token}"
    data = None
    if payload is not None:
        headers["Content-Type"] = "application/json"
        data = json.dumps(payload).encode("utf-8")
    request = Request(base + path, data=data, headers=headers, method=method)
    try:
        with urlopen(request, timeout=20) as response:
            loaded: object = json.load(response)
    except (HTTPError, URLError, TimeoutError) as exc:
        raise KomodoError(f"Infisical {path} failed: {exc}") from exc
    return loaded


def resolve_secret_ref(value: str) -> str:
    """Resolve a `[[infisical://project/env/KEY]]` reference to its live value.

    Komodo dereferences these when it deploys, but `read/GetStack` hands back the
    raw template, so the smoke would otherwise present the literal placeholder as
    a bearer token. Plain values pass through untouched. Requires the deploy job
    to expose INFISICAL_API_URL and a machine identity (INFISICAL_CLIENT_ID /
    INFISICAL_CLIENT_SECRET), which the workflow wires from repository secrets.
    """
    ref = INFISICAL_REF.match(value)
    if not ref:
        return value
    base = os.environ.get("INFISICAL_API_URL", "").rstrip("/")
    client_id = os.environ.get("INFISICAL_CLIENT_ID", "")
    client_secret = os.environ.get("INFISICAL_CLIENT_SECRET", "")
    if not (base and client_id and client_secret):
        raise KomodoError(
            "front-door token is an infisical reference but INFISICAL_API_URL / "
            "INFISICAL_CLIENT_ID / INFISICAL_CLIENT_SECRET are not set"
        )
    if not base.endswith("/api"):
        base += "/api"
    project, env, key = ref.group("project"), ref.group("env"), ref.group("key")
    login = _infisical(
        base,
        "POST",
        "/v1/auth/universal-auth/login",
        "",
        {"clientId": client_id, "clientSecret": client_secret},
    )
    if not isinstance(login, dict) or not isinstance(login.get("accessToken"), str):
        raise KomodoError("Infisical login did not return an access token")
    token = login["accessToken"]
    workspaces = _infisical(base, "GET", "/v1/workspace", token, None)
    entries = workspaces.get("workspaces") if isinstance(workspaces, dict) else None
    workspace_id = None
    if isinstance(entries, list):
        for entry in entries:
            if isinstance(entry, dict) and entry.get("name") == project:
                workspace_id = entry.get("id")
                break
    if not isinstance(workspace_id, str):
        raise KomodoError(f"Infisical has no project named {project!r}")
    query = urlencode(
        {"workspaceId": workspace_id, "environment": env, "secretPath": "/"}
    )
    result = _infisical(base, "GET", f"/v3/secrets/raw/{key}?{query}", token, None)
    secret = result.get("secret") if isinstance(result, dict) else None
    resolved = secret.get("secretValue") if isinstance(secret, dict) else None
    if not isinstance(resolved, str) or not resolved:
        raise KomodoError(f"Infisical returned no value for {project}/{env}/{key}")
    return resolved


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
    token = resolve_secret_ref(match.group(1))
    token_path = Path(runner_temp) / "vogt-dev-smoke-token"
    token_path.write_text(token, encoding="utf-8")
    token_path.chmod(0o600)


def replace_image(
    contents: str, image: str, digest: str, path: str
) -> tuple[str, bool]:
    pattern = re.compile(rf"({re.escape(image)})@sha256:[0-9a-f]{{64}}")
    updated, count = pattern.subn(rf"\g<1>@{digest}", contents)
    if count != 1:
        raise KomodoError(f"{path} must contain exactly one {image} digest pin")
    return updated, updated != contents


def env_shape(stack: dict[str, object]) -> str:
    """Describe the stack's `environment` without exposing a single value.

    Line count, whether newlines arrived real or as literal `\\n`, the key
    names in order, and how many lines are not `KEY=value` at all — enough to
    tell a well-formed `.env` from a mangled one, which is what a silently
    failing Pre Deploy hook (it sources the file) turns on.
    """
    config = stack.get("config")
    raw = config.get("environment") if isinstance(config, dict) else None
    if not isinstance(raw, str):
        return "environment: <absent>"
    literal, real = "\\n" in raw, "\n" in raw
    text = raw.replace("\\n", "\n") if literal and not real else raw
    lines = [line for line in text.splitlines() if line.strip()]
    keys = [
        line.split("=", 1)[0].strip()
        for line in lines
        if "=" in line and not line.lstrip().startswith("#")
    ]
    malformed = sum(
        1 for line in lines if "=" not in line and not line.lstrip().startswith("#")
    )
    joined = [
        line.split("=", 1)[0].strip()
        for line in lines
        if "=" in line
        and not line.lstrip().startswith("#")
        and "\\n" in line.split("=", 1)[1]
    ]
    return (
        f"environment: {len(lines)} non-empty lines; literal-backslash-n={literal}; "
        f"real-newlines={real}; malformed-lines={malformed}; "
        f"values-with-literal-backslash-n={joined}; keys={keys}"
    )


REPAIR_DROP_KEYS = ("VOGT_CODEX_VERSION", "VOGT_CLAUDE_CODE_VERSION")
# What begins a new .env line: a key assignment, a comment, or a blank. Keys
# here are UPPER_SNAKE (VOGT_*, MYDEVENV2_*, ...); base64/PEM material inside
# a JSON value is mixed-case and never matches, so a literal \n followed by it
# is value data and stays literal.
ENV_LINE_START = re.compile(r"\A(#.*|\s*|[A-Z][A-Z0-9_]*=.*)\Z")


def restore_environment(raw: str) -> str:
    """Undo a literal-newline environment collapse, losslessly and structurally.

    A prior deploy wrote `config.environment` back in the escaped form
    `read/GetStack` returns it in, so the whole original `.env` became one
    line joined by literal backslash-n, followed by two real-newline lines
    it had appended. Komodo writes that blob verbatim and the stack's
    pre_deploy hook — `sed -n 's/^KEY=//p' .env` — can no longer match a line
    start.

    Not every literal backslash-n is a lost line break: the FCM
    service-account JSON carries `\\n` *inside* its private key, as data. A
    backslash-n is kept as data only while the current line's value is a
    JSON/quoted value (starts with `{`, `"` or `'`) and the next token does
    not itself begin an env line; everywhere else — including after a plain
    value, before an indented comment — it is a line break. The two lines
    that run added are dropped; nothing else changes. Idempotent on a
    healthy environment.
    """
    lines: list[str] = []
    for chunk in raw.replace("\r\n", "\n").split("\n"):
        for i, token in enumerate(chunk.split("\\n")):
            if i == 0 or not lines:
                lines.append(token)
                continue
            prev = lines[-1]
            match = re.match(r"\A[A-Z][A-Z0-9_]*=(.*)\Z", prev, re.S)
            in_data = match is not None and match.group(1).lstrip().startswith(
                ("{", '"', "'")
            )
            if in_data and not ENV_LINE_START.match(token):
                lines[-1] = prev + "\\n" + token
            else:
                lines.append(token)
    kept = [
        line
        for line in lines
        if not any(line.startswith(f"{key}=") for key in REPAIR_DROP_KEYS)
    ]
    return "\n".join(kept).rstrip("\n") + "\n"


def shape_of(environment: str) -> str:
    return env_shape({"config": {"environment": environment}})


def repair_environment(stack: dict[str, object], mode: str) -> None:
    """Gated one-shot repair. `dry-run` prints the restored shape and writes
    nothing; `apply` writes it with real newlines (the form Komodo expects —
    never the escaped form GetStack returns) and reports the shape read back."""
    config = stack.get("config")
    raw = config.get("environment") if isinstance(config, dict) else None
    if not isinstance(raw, str):
        raise KomodoError("repair: Komodo stack response has no environment")
    restored = restore_environment(raw)
    print(f"repair[{mode}] restored {shape_of(restored)}")
    if mode != "apply":
        return
    api("write/UpdateStack", {"id": STACK, "config": {"environment": restored}})
    after = api("read/GetStack", {"stack": STACK})
    print(f"repair[apply] read-back {env_shape(after)}")


# `info.deployed_config` is `docker compose config` output: the *resolved*
# stack, every `${VAR}` and secret interpolated. It is never printed whole,
# and the lines `inspect` shows from it carry keys only — see `rendered_shape`.
WITHHELD_INFO_FIELDS = frozenset(
    {"remote_contents", "remote_errors", "deployed_config", "deployed_contents"}
)
ENV_MAPPING_LINE = re.compile(r"^(\s*)([A-Z][A-Z0-9_]*):(\s.*|$)")
ENV_LIST_LINE = re.compile(r"^(\s*-\s*)([A-Z][A-Z0-9_]*)=.*$")
STRUCTURAL_LINE = re.compile(
    r"^\s*(-\s*)?(image|target|source|environment|volumes|secrets|container_name|name):(\s|$)"
)


def rendered_shape(rendered: str) -> list[str]:
    """The lines of a rendered compose config that say what a service is
    handed, with every environment value replaced by a marker: `KEY: <value>`
    for mapping form, `- KEY=<value>` for list form. Structural lines (image,
    mount source/target, section headers) pass through `redact`; every other
    line is dropped, so a `command:` carrying a token never appears."""
    shape: list[str] = []
    for line in rendered.splitlines():
        listed = ENV_LIST_LINE.match(line)
        if listed:
            shape.append(f"{listed.group(1)}{listed.group(2)}=<value>")
            continue
        mapped = ENV_MAPPING_LINE.match(line)
        if mapped:
            shape.append(f"{mapped.group(1)}{mapped.group(2)}: <value>")
            continue
        if STRUCTURAL_LINE.match(line):
            shape.append(redact(line)[:200])
    return shape


def stack_log(stack: dict[str, object]) -> None:
    """Best-effort: the engine container's own recent logs, tail-truncated and
    credential-redacted — where the entrypoint says whether tailscale joined
    and whether the core came up."""
    raw_config = stack.get("config")
    config: dict[str, object] = raw_config if isinstance(raw_config, dict) else {}
    raw_info = stack.get("info")
    info: dict[str, object] = raw_info if isinstance(raw_info, dict) else {}
    raw_name = stack.get("name")
    name = raw_name if isinstance(raw_name, str) else STACK
    container = "vogt-dev"
    services = info.get("deployed_services")
    if isinstance(services, list):
        for svc in services:
            if not isinstance(svc, dict) or svc.get("service_name") != "engine":
                continue
            found = svc.get("container_name")
            if isinstance(found, str):
                container = found
    attempts: list[tuple[str, dict[str, object]]] = [
        (
            "read/GetStackLog",
            {"stack": name, "services": ["engine"], "tail": 200, "timestamps": False},
        ),
        (
            "read/GetContainerLog",
            {
                "server": config.get("server_id", ""),
                "container": container,
                "tail": 200,
                "timestamps": False,
            },
        ),
    ]
    for endpoint, payload in attempts:
        try:
            log = api_raw(endpoint, payload)
        except KomodoError as exc:
            print(f"{endpoint} unavailable: {exc}")
            continue
        print(f"--- {endpoint} for {container} (tail) ---")
        for stream in ("stdout", "stderr"):
            text = log.get(stream)
            if isinstance(text, str) and text.strip():
                print(f"[{stream}]")
                for line in text.strip().splitlines()[-200:]:
                    print(redact(line)[:300])
        return


def inspect_stack(stack: dict[str, object]) -> None:
    """Read-only: what Komodo knows about the stack right now — its info
    (state, deployed hash, containers) and the most recent update's stages
    including the ones that succeeded (compose up output lives there).
    Prints nothing that could hold a secret: file contents and the resolved
    config withheld, environment shown as keys only, the rest redacted."""
    info = stack.get("info")
    print("stack info:")
    if isinstance(info, dict):
        for key, value in info.items():
            if key in WITHHELD_INFO_FIELDS or key.endswith(("_contents", "_config")):
                size = len(value) if isinstance(value, (str, list)) else 0
                print(f"  {key}=<withheld, {size}>")
            else:
                print(f"  {key}={redact(repr(value))[:400]}")
    rendered = info.get("deployed_config") if isinstance(info, dict) else None
    if isinstance(rendered, str):
        print("deployed_config (resolved values stripped; keys and mounts only):")
        for line in rendered_shape(rendered):
            print("  " + line)
    stack_id = stack.get("_id")
    if isinstance(stack_id, dict):
        stack_id = stack_id.get("$oid")
    if not isinstance(stack_id, str):
        print("stack id unavailable; skipping update history")
        return
    try:
        listed = api_raw(
            "read/ListUpdates",
            {"query": {"target": {"type": "Stack", "id": stack_id}}, "page": 0},
        )
    except KomodoError as exc:
        print(f"ListUpdates unavailable: {exc}")
        return
    updates = listed.get("updates")
    if not isinstance(updates, list) or not updates:
        print("no updates listed")
        return
    latest = updates[0]
    if not isinstance(latest, dict):
        return
    ident = latest.get("id") or latest.get("_id")
    print(
        f"latest update: operation={latest.get('operation')} "
        f"success={latest.get('success')} status={latest.get('status')} id={ident}"
    )
    if isinstance(ident, dict):
        ident = ident.get("$oid")
    stack_log(stack)
    if isinstance(ident, str):
        full = api_raw("read/GetUpdate", {"id": ident})
        logs = full.get("logs")
        if isinstance(logs, list):
            for entry in logs:
                if not isinstance(entry, dict):
                    continue
                print(
                    f"--- stage: {entry.get('stage')} (success={entry.get('success')})"
                )
                for stream in ("stdout", "stderr"):
                    text = entry.get(stream)
                    if isinstance(text, str) and text.strip():
                        print(f"[{stream}]")
                        for line in text.strip().splitlines()[-60:]:
                            print(redact(line)[:300])


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


def fresh_stack() -> dict[str, object]:
    """`write/RefreshStackCache` then `read/GetStack`: the stack as the repo
    branch head has it, not as Komodo last cached it. Says which repo commit
    the deploy is about to rewrite, so the run log can be checked against
    the repo."""
    api("write/RefreshStackCache", {"stack": STACK})
    stack = api("read/GetStack", {"stack": STACK})
    info = stack.get("info")
    if isinstance(info, dict):
        head = info.get("latest_hash")
        message = redact(str(info.get("latest_message", "")))[:120]
        print(f"stack repo head after refresh: {head} {message}")
    return stack


def deploy() -> str:
    result = api("execute/DeployStack", {"stack": STACK})
    deployment_id = update_id(result)
    for _ in range(180):
        # api_raw: a finished-but-failed update reports `success: false` on the
        # record itself; that is the answer, not a transport error.
        current = api_raw("read/GetUpdate", {"id": deployment_id})
        status = current.get("status")
        if status != "InProgress":
            if current.get("success") is not True:
                detail = failure_detail(current)
                if detail:
                    print(detail, file=sys.stderr)
                raise KomodoError(
                    "Komodo deployment did not succeed (its update logs are above)"
                )
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
    # Keys only, never values: the shape of the .env Komodo will write.
    print(env_shape(stack))
    repair = os.environ.get("REPAIR_ENVIRONMENT", "").strip()
    if repair == "inspect":
        inspect_stack(stack)
        print("inspect: read-only; not deploying")
        return 0
    if repair:
        if repair not in ("dry-run", "apply"):
            raise KomodoError(
                "REPAIR_ENVIRONMENT must be 'dry-run', 'apply' or 'inspect'"
            )
        repair_environment(stack, repair)
        if repair == "dry-run":
            print("repair[dry-run]: nothing written; not deploying")
            return 0
        stack = api("read/GetStack", {"stack": STACK})
        config = stack.get("config")
        if not isinstance(config, dict):
            raise KomodoError("Komodo stack response has no config")
    # The files rewritten below are read from Komodo's *cached* clone of the
    # stack repo. Without a refresh first, anything pushed to the repo since
    # the last deploy is written back over from the stale copy (an overlay edit
    # made between deploys would be lost). Only the deploy path refreshes, so
    # inspect and dry-run stay read-only.
    stack = fresh_stack()
    config = stack.get("config")
    if not isinstance(config, dict):
        raise KomodoError("Komodo stack response has no config after refresh")
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
