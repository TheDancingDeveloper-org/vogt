"""Two real processes, and the actor that has to survive the hop (FR-S9).

Every other test of the front door uses a stand-in core that approves
everything, so FR-S4's second gate is asserted on its own and never *behind*
the first — which is the arrangement that actually ships. §6.2a recorded that
as the gap, and it is the one worth closing by hand: the claim is that a
browser holding a front-door token writes to Vogt as the actor that token is
paired with, and both halves of that sentence live in different processes
written in different languages.

So this boots both. `vogt serve` on loopback with a real database, the engine
binary in front of it with two paired tokens, and the assertions read the
audit log afterwards through Vogt's own CLI. Nothing here is stood in for.

Skipped when the engine binary is absent, which is every core-only checkout
and the `core` job in CI (NFR-Q6). Build it with `cargo build` in `engine/`.
"""

from __future__ import annotations

import json
import os
import socket
import subprocess
import time
from collections.abc import Iterator
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[1]
ENGINE_BIN = REPO_ROOT / "engine" / "target" / "debug" / "vogt-engine"

pytestmark = pytest.mark.skipif(
    not ENGINE_BIN.is_file(),
    reason=(
        "needs the engine binary; run `cargo build` in `engine/`. A core-only "
        "checkout has no engine and that is NFR-Q6 rather than a broken setup"
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


def vogt(*args: str, data_dir: Path) -> subprocess.CompletedProcess[str]:
    """Run the Vogt CLI against a specific instance."""
    env = {**os.environ, "VOGT_DATA_DIR": str(data_dir)}
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
    vogt("init", data_dir=data_dir)

    # Two actors, two core tokens. Two, because one proves nothing: a single
    # actor's writes would be attributed correctly by a proxy that hard-coded
    # it, and the claim is that the *pairing* decides.
    secrets: dict[str, str] = {}
    for actor in ("alpha", "beta"):
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
            "read,work.write",
            "--reason",
            "front door test",
            data_dir=data_dir,
        )
        secrets[actor] = json.loads(issued.stdout)["secret"]

    # A third, deliberately weaker: read only. This is the core's own gate,
    # and the point of having it is that the front door cannot substitute for
    # it — a caller can hold `vogt-write` at the front door and still be
    # refused by the core, which is FR-S4 standing behind FR-S9.
    vogt(
        "actor",
        "create",
        "--identity-ref",
        "agent:reader",
        "--kind",
        "agent",
        "--display-name",
        "reader",
        "--reason",
        "front door test",
        data_dir=data_dir,
    )
    secrets["reader"] = json.loads(
        vogt(
            "--json",
            "token",
            "issue",
            "--actor",
            "agent:reader",
            "--name",
            "reader",
            "--scopes",
            "read",
            "--reason",
            "front door test",
            data_dir=data_dir,
        ).stdout
    )["secret"]

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

    # Each front-door token names the file holding the core token it is paired
    # with — a file rather than a value, because the two sit beside each other
    # in a config and one of them is a browser's.
    paired = {}
    for actor in ("alpha", "beta", "reader"):
        path = root / f"{actor}.core-token"
        path.write_text(secrets[actor], encoding="utf-8")
        paired[actor] = path

    config = root / "engine.toml"
    config.write_text(
        "\n".join(
            [
                'token = "front-door-primary-token-000000"',
                f'workspace_root = "{root}"',
                f'state_dir = "{root / "engine-state"}"',
                f'vogt_core_url = "{core_url}"',
                *[
                    line
                    for actor in ("alpha", "beta")
                    for line in (
                        "[[extra_tokens]]",
                        f'name = "{actor}"',
                        f'token = "front-door-{actor}-token-000000"',
                        'capabilities = ["vogt-write"]',
                        f'vogt_core_token_file = "{paired[actor]}"',
                    )
                ],
                # Holds the front door's write capability and is paired with a
                # core token that has only `read`.
                "[[extra_tokens]]",
                'name = "reader"',
                'token = "front-door-reader-token-000000"',
                'capabilities = ["vogt-write"]',
                f'vogt_core_token_file = "{paired["reader"]}"',
                # Holds no capability at all, paired with a core token that
                # could write. The front door must refuse it on its own.
                "[[extra_tokens]]",
                'name = "ungranted"',
                'token = "front-door-ungranted-token-0000"',
                "capabilities = []",
                f'vogt_core_token_file = "{paired["alpha"]}"',
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
        env={**os.environ, "MYDEVENV2_TOKEN": "front-door-primary-token-000000"},
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )
    base = f"http://127.0.0.1:{engine_port}"

    try:
        wait_for(f"{core_url}/health/ready", core, "vogt-core")
        wait_for(f"{base}/healthz", engine, "the engine")
        yield {"base": base, "core": core_url, "data_dir": str(data_dir)}
    finally:
        for process in (engine, core):
            process.terminate()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()


def post(base: str, path: str, token: str, body: dict[str, object]) -> tuple[int, str]:
    import urllib.error
    import urllib.request

    request = urllib.request.Request(
        f"{base}{path}",
        data=json.dumps(body).encode(),
        headers={
            "Authorization": f"Bearer {token}",
            "Content-Type": "application/json",
        },
        method="POST",
    )
    try:
        with urllib.request.urlopen(request, timeout=10) as answer:
            return answer.status, answer.read().decode()
    except urllib.error.HTTPError as refusal:
        return refusal.code, refusal.read().decode()


def test_a_write_through_the_front_door_is_audited_as_the_paired_actor(
    pair: dict[str, str],
) -> None:
    """FR-S9's whole claim, and the only test that runs it end to end.

    A browser holds a *front-door* token and never a core one. What lands in
    the audit log has to be the actor that token is paired with — not the
    pod's shared identity, which is the failure this pairing exists against:
    every session's work filed under one name, silently, with the writes all
    succeeding.

    The write exercised is `label.create`, not `work.create`. Since the
    upstream-truth pivot (#181, decision 10) a work write refuses an unlinked
    project, and the fixture's `alpha` is registered but not forge-linked; a
    label is an instance-wide `work.write` that does not gate on a project, so
    it drives the same audited write path through both processes without
    dragging a forge provider and credential into a test whose subject is the
    actor that survives the hop, not linking.
    """
    for actor in ("alpha", "beta"):
        status, body = post(
            pair["base"],
            "/api/vogt/labels",
            f"front-door-{actor}-token-000000",
            {
                "name": f"{actor}-was-here",
                "reason": "front door test",
            },
        )
        assert status == 200, f"{actor}: {status} {body}"

    trail = json.loads(
        vogt(
            "--json",
            "audit",
            "list",
            "--operation",
            "label.create",
            "--limit",
            "10",
            data_dir=Path(pair["data_dir"]),
        ).stdout
    )["records"]
    by_actor = {record["actor_identity_ref"]: record for record in trail}
    assert "agent:alpha" in by_actor and "agent:beta" in by_actor, (
        "each write is attributed to the actor its front-door token is paired "
        f"with; the log says {sorted(by_actor)}"
    )
    assert by_actor["agent:alpha"]["reason"] == "front door test"


def test_the_front_doors_gate_and_the_cores_gate_both_stand(
    pair: dict[str, str],
) -> None:
    """FR-S4 behind FR-S9: two gates, and neither substitutes for the other.

    Asserted together because separately they are both already covered and
    the arrangement is what ships. A token with no capability is refused by
    the front door *before* the core is asked — even though the core token it
    is paired with could write. A token that clears the front door is still
    refused by the core when its own scopes do not allow the write. Either
    gate failing open would be invisible from the other side.
    """
    ungranted, _ = post(
        pair["base"],
        "/api/vogt/work",
        "front-door-ungranted-token-0000",
        {
            "kind": "bug",
            "title": "nope",
            "project": "alpha",
            "reason": "front door test",
        },
    )
    assert ungranted == 403, (
        "the front door refuses a token without `vogt-write`, whatever the "
        "core token behind it could do"
    )

    reader, body = post(
        pair["base"],
        "/api/vogt/work",
        "front-door-reader-token-000000",
        {
            "kind": "bug",
            "title": "nope",
            "project": "alpha",
            "reason": "front door test",
        },
    )
    assert reader in (401, 403), (
        f"the core refuses a read-only pairing that cleared the front door: "
        f"{reader} {body}"
    )

    titles = json.loads(
        vogt(
            "--json", "work", "list", "--limit", "50", data_dir=Path(pair["data_dir"])
        ).stdout
    )["items"]
    assert not [item for item in titles if item["title"] == "nope"], (
        "a refusal at either gate writes nothing"
    )
