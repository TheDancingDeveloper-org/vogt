"""Two real processes, and the actor that has to survive the hop.

Every other test of the front door uses a stand-in core that approves
everything, so the core's second gate is asserted on its own and never
*behind* the first — which is the arrangement that actually ships. That was
the recorded gap, and it is the one worth closing by hand: the claim is that a
caller presenting a core credential to the front door — a session minted by a
password login, or an API token — writes to Vogt as *that* actor, and both
halves of that sentence live in different processes written in different
languages.

So this boots both. `vogt serve` on loopback with a real database, the engine
binary in front of it holding nothing but the stack secret and a break-glass
token, and the assertions read the audit log afterwards through Vogt's own
CLI. Nothing here is stood in for.

Skipped when the engine binary is absent, which is every core-only checkout
and the `core` job in CI. Build it with `cargo build` in `engine/`.
"""

from __future__ import annotations

import json
import os
import socket
import subprocess
import time
from collections.abc import Iterator, Mapping
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[1]
ENGINE_BIN = REPO_ROOT / "engine" / "target" / "debug" / "vogt-engine"

pytestmark = pytest.mark.skipif(
    not ENGINE_BIN.is_file(),
    reason=(
        "needs the engine binary; run `cargo build` in `engine/`. A core-only "
        "checkout has no engine and that is by design rather than a broken setup"
    ),
)

#: Long enough for two processes to open a socket on a loaded machine, short
#: enough that a genuine failure is a failure rather than a hang. The trap
#: this file is most likely to fall into is waiting forever for something that
#: died at startup, so every wait polls the process too.
BOOT_TIMEOUT = 30.0


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return int(s.getsockname()[1])


def vogt(
    *args: str, data_dir: Path, env: dict[str, str] | None = None
) -> subprocess.CompletedProcess[str]:
    """Run the Vogt CLI against a specific instance."""
    env = {**os.environ, "VOGT_DATA_DIR": str(data_dir), **(env or {})}
    return subprocess.run(
        ["uv", "run", "vogt", *args],
        cwd=REPO_ROOT,
        env=env,
        capture_output=True,
        text=True,
        check=True,
    )


def wait_for(url: str, process: subprocess.Popen[str], what: str) -> None:
    """Poll until it answers, or the process dies, or time runs out."""
    import urllib.error
    import urllib.request

    deadline = time.monotonic() + BOOT_TIMEOUT
    while time.monotonic() < deadline:
        if process.poll() is not None:
            output = (process.stdout.read() if process.stdout else "") or ""
            raise AssertionError(f"{what} exited {process.returncode}:\n{output}")
        try:
            with urllib.request.urlopen(url, timeout=1):
                return
        except urllib.error.HTTPError:
            return  # answering, even if it does not like an unauthenticated GET
        except OSError:
            time.sleep(0.1)
    raise AssertionError(f"{what} never answered {url} within {BOOT_TIMEOUT}s")


@pytest.fixture(scope="module")
def pair(tmp_path_factory: pytest.TempPathFactory) -> Iterator[dict[str, str]]:
    """Vogt and the engine, both real, wired the way the stack wires them."""
    root = tmp_path_factory.mktemp("front-door")
    data_dir = root / "vogt"
    data_dir.mkdir()
    # The one secret both halves share. The core adopts it at init as its
    # front-door actor; the engine recognises it as the core's identity and
    # lends it to the break-glass token.
    stack_secret = root / "stack-secret"
    stack_secret.write_text(STACK_SECRET, encoding="utf-8")
    vogt(
        "init",
        data_dir=data_dir,
        env={"VOGT_BOOTSTRAP_CORE_TOKEN_FILE": str(stack_secret)},
    )

    # Two actors, two core tokens. Two, because one proves nothing: a single
    # actor's writes would be attributed correctly by a proxy that hard-coded
    # it, and the claim is that the *credential* decides.
    secrets: dict[str, str] = {}
    for actor, scopes in (
        ("alpha", "read,work.write"),
        ("beta", "read,work.write"),
        # Deliberately weaker: read only. The front door derives no write
        # capability from it, so the door's own gate is what refuses it.
        ("reader", "read"),
        # Clears the front door — `project.write` earns `vogt-write` there —
        # but the core refuses a work write from it. That is the core's gate
        # standing behind the door.
        ("projector", "read,project.write"),
    ):
        vogt(
            "actor",
            "create",
            "--identity-ref",
            f"agent:{actor}",
            "--kind",
            "agent",
            "--display-name",
            actor,
            "--reason",
            "front door test",
            data_dir=data_dir,
        )
        issued = vogt(
            "--json",
            "token",
            "issue",
            "--actor",
            f"agent:{actor}",
            "--name",
            actor,
            "--scopes",
            scopes,
            "--reason",
            "front door test",
            data_dir=data_dir,
        )
        secrets[actor] = json.loads(issued.stdout)["secret"]

    # A person, with a password: the shape a browser signs in with.
    password_file = root / "ada-password"
    password_file.write_text(ADA_PASSWORD, encoding="utf-8")
    vogt(
        "user",
        "create",
        "--username",
        "ada",
        "--display-name",
        "Ada",
        "--scopes",
        "read,work.write",
        "--password-file",
        str(password_file),
        "--reason",
        "front door test",
        data_dir=data_dir,
    )

    vogt(
        "project",
        "register",
        "--name",
        "alpha",
        "--root-path",
        str(root / "tree"),
        "--reason",
        "front door test",
        data_dir=data_dir,
    )

    core_port = free_port()
    core = subprocess.Popen(
        [
            "uv",
            "run",
            "vogt",
            "serve",
            "--host",
            "127.0.0.1",
            "--port",
            str(core_port),
        ],
        cwd=REPO_ROOT,
        env={**os.environ, "VOGT_DATA_DIR": str(data_dir)},
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )
    core_url = f"http://127.0.0.1:{core_port}"

    config = root / "engine.toml"
    config.write_text(
        "\n".join(
            [
                f'token = "{BREAK_GLASS}"',
                f'workspace_root = "{root}"',
                f'state_dir = "{root / "engine-state"}"',
                f'vogt_core_url = "{core_url}"',
                f'vogt_core_token_file = "{stack_secret}"',
            ]
        )
        + "\n",
        encoding="utf-8",
    )

    engine_port = free_port()
    engine = subprocess.Popen(
        [
            str(ENGINE_BIN),
            "--config",
            str(config),
            "--bind",
            f"127.0.0.1:{engine_port}",
        ],
        cwd=REPO_ROOT / "engine",
        env={**os.environ, "ENGINE_TOKEN": BREAK_GLASS},
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )
    base = f"http://127.0.0.1:{engine_port}"

    try:
        wait_for(f"{core_url}/health/ready", core, "vogt-core")
        wait_for(f"{base}/healthz", engine, "the engine")
        yield {
            "base": base,
            "core": core_url,
            "data_dir": str(data_dir),
            **{f"token:{actor}": secret for actor, secret in secrets.items()},
        }
    finally:
        for process in (engine, core):
            process.terminate()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()


STACK_SECRET = "stack-secret-shared-by-both-halves-0000"
BREAK_GLASS = "front-door-primary-token-000000"
ADA_PASSWORD = "correct horse battery staple"


def post(
    base: str, path: str, token: str | None, body: Mapping[str, object]
) -> tuple[int, str]:
    import urllib.error
    import urllib.request

    headers = {"Content-Type": "application/json"}
    if token is not None:
        headers["Authorization"] = f"Bearer {token}"
    request = urllib.request.Request(
        f"{base}{path}", data=json.dumps(body).encode(), headers=headers, method="POST"
    )
    try:
        with urllib.request.urlopen(request, timeout=10) as answer:
            return answer.status, answer.read().decode()
    except urllib.error.HTTPError as refusal:
        return refusal.code, refusal.read().decode()


def get(base: str, path: str, token: str) -> tuple[int, str]:
    import urllib.error
    import urllib.request

    request = urllib.request.Request(
        f"{base}{path}", headers={"Authorization": f"Bearer {token}"}
    )
    try:
        with urllib.request.urlopen(request, timeout=10) as answer:
            return answer.status, answer.read().decode()
    except urllib.error.HTTPError as refusal:
        return refusal.code, refusal.read().decode()


def audit_by_actor(data_dir: str, operation: str) -> dict[str, dict[str, object]]:
    trail = json.loads(
        vogt(
            "--json",
            "audit",
            "list",
            "--operation",
            operation,
            "--limit",
            "20",
            data_dir=Path(data_dir),
        ).stdout
    )["records"]
    return {record["actor_identity_ref"]: record for record in trail}


def test_a_write_through_the_front_door_is_audited_as_the_caller(
    pair: dict[str, str],
) -> None:
    """The front door's whole claim, and the only test that runs it end to end.

    A caller presents a *core* credential to the front door. What lands in
    the audit log has to be that credential's actor — not the pod's shared
    identity, which is the failure the old pairing existed against and the
    drift it suffered from: every session's work filed under one name,
    silently, with the writes all succeeding.

    The write exercised is `label.create`, not `work.create`. Since the
    upstream-truth pivot a work write refuses an unlinked project, and the
    fixture's `alpha` is registered but not forge-linked; a label is an
    instance-wide `work.write` that does not gate on a project, so it drives
    the same audited write path through both processes without dragging a
    forge provider and credential into a test whose subject is the actor that
    survives the hop.
    """
    for actor in ("alpha", "beta"):
        status, body = post(
            pair["base"],
            "/api/vogt/labels",
            pair[f"token:{actor}"],
            {"name": f"{actor}-was-here", "reason": "front door test"},
        )
        assert status == 200, f"{actor}: {status} {body}"

    by_actor = audit_by_actor(pair["data_dir"], "label.create")
    assert "agent:alpha" in by_actor and "agent:beta" in by_actor, (
        "each write is attributed to the actor whose credential was presented; "
        f"the log says {sorted(by_actor)}"
    )
    assert by_actor["agent:alpha"]["reason"] == "front door test"


def test_the_break_glass_token_writes_as_the_stack_secrets_actor(
    pair: dict[str, str],
) -> None:
    """The one shared identity left, and it is named: the static engine token
    has no actor of its own, so the door lends it the stack secret's."""
    status, body = post(
        pair["base"],
        "/api/vogt/labels",
        BREAK_GLASS,
        {"name": "break-glass-was-here", "reason": "front door test"},
    )
    assert status == 200, f"{status} {body}"
    by_actor = audit_by_actor(pair["data_dir"], "label.create")
    assert "agent:vogt-engine" in by_actor, sorted(by_actor)


def test_the_front_doors_gate_and_the_cores_gate_both_stand(
    pair: dict[str, str],
) -> None:
    """Two gates, and neither substitutes for the other.

    A read-only credential earns no `vogt-write` at the door and is refused
    there, before the core is asked. A `project.write` credential clears the
    door — it may write to Vogt in general — and is still refused by the core
    for a *work* write its scopes do not cover. Either gate failing open would
    be invisible from the other side.
    """
    work = {
        "kind": "bug",
        "title": "nope",
        "project": "alpha",
        "reason": "front door test",
    }
    reader, body = post(pair["base"], "/api/vogt/work", pair["token:reader"], work)
    assert reader == 403, (
        f"the front door refuses a credential without `vogt-write`: {reader} {body}"
    )
    assert "capability" in body

    projector, body = post(
        pair["base"], "/api/vogt/work", pair["token:projector"], work
    )
    assert projector == 403, (
        f"the core refuses a work write from a project.write credential that "
        f"cleared the front door: {projector} {body}"
    )
    assert "work.write" in body, body

    titles = json.loads(
        vogt(
            "--json", "work", "list", "--limit", "50", data_dir=Path(pair["data_dir"])
        ).stdout
    )["items"]
    assert not [item for item in titles if item["title"] == "nope"], (
        "a refusal at either gate writes nothing"
    )


def test_an_unknown_credential_is_refused_at_the_door(pair: dict[str, str]) -> None:
    status, body = post(
        pair["base"],
        "/api/vogt/labels",
        "nobody-issued-this-token-0000000000",
        {"name": "never", "reason": "front door test"},
    )
    assert status == 401, f"{status} {body}"
    assert "not valid" in body


def test_a_person_signs_in_with_a_password_and_writes_as_themselves(
    pair: dict[str, str],
) -> None:
    """The browser's path, end to end: an open login through the door mints a
    session at the core; the session is a credential the door resolves; a
    write with it is audited to the person; a logout ends it at both halves.
    """
    refused, body = post(
        pair["base"],
        "/api/auth/login",
        None,
        {"username": "ada", "password": "not it"},
    )
    assert refused == 401, f"{refused} {body}"

    status, body = post(
        pair["base"],
        "/api/auth/login",
        None,
        {"username": "Ada", "password": ADA_PASSWORD},
    )
    assert status == 200, f"{status} {body}"
    session = json.loads(body)
    assert session["token"]["kind"] == "session"
    secret = session["secret"]

    status, body = get(pair["base"], "/api/auth/check", secret)
    assert status == 200, body
    assert json.loads(body)["identity"]["name"] == "human:ada"

    status, body = post(
        pair["base"],
        "/api/vogt/labels",
        secret,
        {"name": "ada-was-here", "reason": "front door test"},
    )
    assert status == 200, f"{status} {body}"
    assert "human:ada" in audit_by_actor(pair["data_dir"], "label.create")

    status, body = post(
        pair["base"], "/api/vogt/auth/logout", secret, {"reason": "front door test"}
    )
    assert status == 200 and json.loads(body)["revoked"] is True, body
    # Refused at the door on the very next request: the logout evicted the
    # door's cache and the core no longer knows the session.
    status, _ = get(pair["base"], "/api/auth/check", secret)
    assert status == 401
