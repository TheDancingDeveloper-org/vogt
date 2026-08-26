"""The transport-parity harness (FR-A3).

This drives one ordered script of every registered operation through the
CLI, the REST surface and the MCP surface — against three identical, isolated
instances — and asserts that all three return the same answers and leave the
same audit trail. Parity is *tested*, not intended (DESIGN §2).

The script is ordered rather than per-operation-independent because the write
plane is stateful: you cannot relate two work items before creating them, and
a `why` that never ran against a real ranked item proves nothing. Running the
identical sequence on each transport is what makes the comparison meaningful.

Four staleness checks run alongside it, all failing in **both** directions:

1. An operation must appear on exactly the surfaces `transports_for` claims.
2. Every exclusion must name a registered operation.
3. Every exclusion must say why it exists.
4. Every operation on all three surfaces must appear in the script, so a new
   operation cannot be added without being driven on all three.
"""

from __future__ import annotations

import itertools
import json
import subprocess
from collections.abc import Callable
from pathlib import Path
from typing import Any

import pytest
from fastapi.testclient import TestClient

from vogt.adapters.cli.main import EXIT_OK, build_parser, run
from vogt.adapters.engine import EngineClient
from vogt.adapters.git import (
    CloneOutcome,
    Cloner,
    CloneRequest,
    Pusher,
    PushOutcome,
    PushRequest,
)
from vogt.adapters.http.app import API_PREFIX, build_app
from vogt.adapters.mcp.surface import McpSurface
from vogt.application.context import AppContext, build_context
from vogt.application.models import InitParams
from vogt.application.services import init_instance
from vogt.config import VogtConfig
from vogt.registry import HTTP_ONLY, LOCAL_ONLY, default_registry
from vogt.registry.operation import Operation

from tests.conftest import TEST_PRINCIPAL, SequentialIds, StepClock

WHY = "parity harness"

#: `{root}` is replaced with a per-instance directory, so the one operation
#: that touches the filesystem can run on all three transports without them
#: writing over each other.
#: A step's params may be a callable taking the results so far, for the
#: cases where an argument is an id only the previous step knows — the
#: alternative is not driving those operations at all.
StepParams = dict[str, Any] | Callable[[dict[str, Any]], dict[str, Any]]

SCRIPT: list[tuple[str, StepParams]] = [
    ("status", {}),
    ("place.metrics", {}),
    ("connect", {}),
    ("workflow.list", {}),
    (
        "actor.create",
        {
            "identity_ref": "agent:parity",
            "kind": "agent",
            "display_name": "Parity Agent",
            "reason": WHY,
        },
    ),
    ("actor.list", {}),
    ("label.create", {"name": "parity", "color": "#d73a4a", "reason": WHY}),
    ("label.list", {}),
    (
        "initiative.create",
        {"title": "Parity Initiative", "weight": 40, "reason": WHY},
    ),
    ("initiative.list", {}),
    # Project the initiative onto its forge tracking issues (#286). At this
    # point the initiative spans no forge-linked project, so the projection is
    # a deterministic no-op — zero forge calls, an empty `tracking_issues` — and
    # what parity checks is that the same receipt arrives on all three surfaces.
    ("initiative.publish", {"slug": "parity-initiative", "reason": WHY}),
    (
        "project.register",
        {
            "name": "Parity Project",
            "root_path": "/srv/parity",
            "repo_url": "https://github.com/parity-org/parity-project",
            "reason": WHY,
        },
    ),
    ("project.get", {"slug": "parity-project"}),
    ("project.list", {}),
    (
        "project.update",
        {
            "slug": "parity-project",
            "exclusions": ["corpus/", ".claude/"],
            "reason": WHY,
        },
    ),
    (
        "project.transition",
        {"slug": "parity-project", "to_state": "maintenance", "reason": WHY},
    ),
    (
        "project.create",
        {
            "name": "Parity Scaffold",
            "root_path": "{root}/scaffold",
            "owner": "parity",
            "reason": WHY,
        },
    ),
    (
        "project.import",
        {
            "repo": "parity-org/parity-import",
            "consolidate": False,
            "reason": WHY,
        },
    ),
    ("notifications", {}),
    # -- the forge foundation, armed before the work plane (#179/#180/#181) --
    # Per-actor account linking: paste, confirm, enumerate. The token is
    # validated against the stand-in forge and never echoed back; it is also
    # the credential the linked project's write-through lands under.
    ("forge.account_link", {"token": "ghp_parity_token", "reason": WHY}),
    ("forge.account_status", {}),
    # The import picker (#180): the same accessible-repo list must arrive
    # byte-identically on all three surfaces.
    ("forge.repos", {}),
    # The verb the picker leads to (#344): clone a listed repository under the
    # actor's linked credential, register it, consolidate. The stand-in forge
    # answers `describe` and the sync reads; the injected cloner writes the
    # tree — so import parity covers the production path up to the network
    # edges, on all three surfaces.
    (
        "forge.import",
        {
            "owner": "parity-org",
            "name": "parity-forge",
            "reason": WHY,
        },
    ),
    (
        "forge.writeback",
        {"project": "parity-project", "policy": "full", "reason": WHY},
    ),
    # The #181 pivot: an explicit act makes the project upstream-truth.
    ("forge.link", {"project": "parity-project", "reason": WHY}),
    # The other explicit act (#182): a local-only project is published — the
    # stand-in forge mints the repository, the injected pusher records the
    # plain push — and comes back linked on every transport. The fixture
    # checkout under {root} is a real git repository, so the read-only
    # publish gate runs for real; only the network edges are stand-ins.
    (
        "project.register",
        {
            "name": "Parity Publish",
            "root_path": "{root}/publishable",
            "reason": WHY,
        },
    ),
    ("forge.publish", {"project": "parity-publish", "reason": WHY}),
    # -- the work plane, both shapes (#181) ---------------------------------
    # Native declared items carry the relation/blocker flows; they belong to
    # no project because `work.create` on an unlinked project is the typed
    # decision-10 refusal, and relations stay declared-only in v1.
    (
        "work.create",
        {
            "kind": "bug",
            "title": "Ranked bug",
            "body": "raised by the parity harness",
            "priority": "p1",
            "effort": "s",
            "initiative": "parity-initiative",
            "assignee": "agent:parity",
            "labels": ["parity"],
            "reason": WHY,
        },
    ),
    (
        "work.create",
        {
            "kind": "feature",
            "title": "Blocking feature",
            "reason": WHY,
        },
    ),
    # Write-through create on the linked project: the stand-in forge mints
    # issue #1, and the subject key comes back as the ref on every surface.
    (
        "work.create",
        {
            "kind": "feature",
            "title": "Upstream tracked",
            "body": "created through the forge",
            "project": "parity-project",
            "labels": ["parity"],
            "reason": WHY,
        },
    ),
    # The explicit local-only create (#347): on a linked project this makes a
    # native declared item that carries no upstream subject yet — no forge call,
    # the same receipt on every surface. It is the opt-in that neither the
    # policy gate nor the decision-10 refusal applies to.
    (
        "work.create",
        {
            "kind": "chore",
            "title": "Local only",
            "project": "parity-project",
            "local_only": True,
            "reason": WHY,
        },
    ),
    # Consolidate so the mirror knows the issue the create just minted; the
    # upstream-truth reads below join that mirror to the overlay.
    ("forge.onboard", {"project": "parity-project", "reason": WHY}),
    ("work.get", {"ref": "WI-1"}),
    ("work.get", {"ref": "gh:parity-org/parity-project#1"}),
    ("work.list", {"project": "parity-project"}),
    (
        "board.list",
        {
            "project": "parity-project",
            "lane_mode": "none",
            "cells": [
                {"lane_key": "", "state": "open"},
                {"lane_key": "", "state": "in_progress"},
            ],
            "page_size": 1,
        },
    ),
    ("work.update", {"ref": "WI-1", "priority": "p0", "reason": WHY}),
    # Overlay-only on the linked item: priority is vogt-local (decision 2).
    (
        "work.update",
        {"ref": "gh:parity-org/parity-project#1", "priority": "p0", "reason": WHY},
    ),
    (
        "work.relate",
        {"ref": "WI-1", "kind": "depends_on", "target": "WI-2", "reason": WHY},
    ),
    ("work.transition", {"ref": "WI-2", "to_state": "in_progress", "reason": WHY}),
    # A vogt-only state move on the linked item: overlay-only, no upstream
    # write — the invariant the transport-recording test pins.
    (
        "work.transition",
        {
            "ref": "gh:parity-org/parity-project#1",
            "to_state": "in_progress",
            "reason": WHY,
        },
    ),
    # The declared branch binding (#283): a branch name lands on the linked
    # item's overlay, defaulted from the pattern (the upstream item's `gh-1`
    # form) since none is given. The result echoes the item's branches, and
    # all three surfaces must agree on the declared-side view.
    (
        "work.bind_branch",
        {"ref": "gh:parity-org/parity-project#1", "reason": WHY},
    ),
    ("work.comment", {"ref": "WI-1", "body": "seen by the harness", "reason": WHY}),
    # A comment on the linked item posts upstream, fail-loud.
    (
        "work.comment",
        {
            "ref": "gh:parity-org/parity-project#1",
            "body": "posted through the forge",
            "reason": WHY,
        },
    ),
    ("backlog", {}),
    ("bugs", {}),
    ("why", {"ref": "WI-1"}),
    (
        "work.unrelate",
        {"ref": "WI-1", "kind": "depends_on", "target": "WI-2", "reason": WHY},
    ),
    ("project.brief", {"slug": "parity-project"}),
    # -- collection ---------------------------------------------------------
    (
        "project.register",
        {"name": "Parity Fixture", "root_path": "{root}/fixture", "reason": WHY},
    ),
    (
        "sweep",
        {"project": "parity-fixture", "offline_only": True, "reason": WHY},
    ),
    ("coverage", {}),
    ("observations.list", {"project": "parity-fixture"}),
    ("deps", {"project": "parity-fixture"}),
    ("backlog", {"limit": 50}),
    (
        "suppress",
        {
            "subject": "mark:parity-fixture/notes.md#L2",
            "reason": WHY,
        },
    ),
    ("suppression.list", {}),
    # -- contract and drift -------------------------------------------------
    ("contract.evaluate", {"path": "{root}"}),
    # Adoption first: the contract is opt-in, so a project that never adopted
    # reports `not_applicable` and records nothing (FR-G16).
    ("contract.adopt", {"project": "parity-fixture", "reason": WHY}),
    ("project.scaffold", {"project": "parity-fixture", "reason": WHY}),
    (
        "contract.inapplicable",
        {
            "project": "parity-fixture",
            "rule": "required_dir",
            "target": "design",
            "reason": WHY,
        },
    ),
    ("contract.check", {"project": "parity-fixture", "reason": WHY}),
    (
        "contract.applicable",
        {
            "project": "parity-fixture",
            "rule": "required_dir",
            "target": "design",
            "reason": WHY,
        },
    ),
    ("compliance", {"project": "parity-fixture"}),
    ("contract.decline", {"project": "parity-fixture", "reason": WHY}),
    ("drift.detect", {"reason": WHY}),
    ("drift.list", {"status": "open"}),
    ("inbox.list", {}),
    (
        "inbox.snooze",
        lambda seen: {
            "entry_key": seen["inbox.list"]["entries"][0]["entry_key"],
            "until": "2099-01-01T00:00:00+00:00",
            "reason": WHY,
        },
    ),
    ("inbox.list", {"triage_states": ["snoozed"]}),
    (
        "inbox.restore",
        lambda seen: {
            "entry_key": seen["inbox.snooze"]["entry"]["entry_key"],
            "reason": WHY,
        },
    ),
    (
        "inbox.archive",
        lambda seen: {
            "entry_key": seen["inbox.restore"]["entry"]["entry_key"],
            "reason": WHY,
        },
    ),
    (
        "drift.resolve",
        lambda seen: {
            "id": seen["drift.list"]["proposals"][0]["id"],
            "resolution": "contested",
            "reason": "the target is a project nobody has registered yet",
        },
    ),
    # Adopting needs a subject that survived suppression, so the marker in
    # the scaffolded AGENTS.md is the one the script reaches for. Its key is
    # deterministic: same scaffold, same file, same line, on every transport.
    (
        "work.adopt",
        {"subject": "mark:parity-fixture/notes.md#L1", "reason": WHY},
    ),
    (
        "suppression.revoke",
        lambda seen: {
            "id": seen["suppress"]["suppression"]["id"],
            "reason": "the marker turned out to be real work after all",
        },
    ),
    ("observations.prune", {"reason": WHY}),
    # -- identity and portability -------------------------------------------
    # -- coding sessions ----------------------------------------------------
    #
    # Driven against a stand-in engine (`_stand_in_engine`), because the
    # operations are Vogt's and the PTY is not: what parity has to prove is
    # that all three transports start the same session in the same tree and
    # get the same answer back.
    (
        "session.start",
        {"project": "parity-project", "reason": WHY},
    ),
    ("session.list", {}),
    (
        "session.stop",
        lambda seen: {
            "id": seen["session.start"]["session"]["id"],
            "reason": "the harness is finished with it",
        },
    ),
    (
        "token.issue",
        {
            "actor": "agent:parity",
            "name": "harness",
            "scopes": "read,work.write",
            "reason": WHY,
        },
    ),
    ("token.list", {}),
    (
        "token.revoke",
        lambda seen: {
            "id": seen["token.issue"]["token"]["id"],
            "reason": "the harness is finished with it",
        },
    ),
    ("auth.decisions", {}),
    # -- the forge module's tail --------------------------------------------
    # The account stayed linked through every write-through step above; the
    # unlink here proves removal on all three surfaces and returns writes to
    # the file token.
    ("forge.account_unlink", {"reason": WHY}),
    ("forge.actions", {}),
    ("export", {"destination": "{root}/export.json", "reason": WHY}),
    ("events.list", {}),
    ("audit.list", {}),
    # Every narrowing the audit query offers, driven on all three transports:
    # a date arrives as a string on each of them and has to mean the same
    # instant, and a project filter answered in the query has to select the
    # same writes whoever asked.
    (
        "audit.list",
        {
            "project": "parity-project",
            "since": "2026-01-01T00:00:00+00:00",
            "until": "2027-01-01T00:00:00+00:00",
            "limit": 5,
            "offset": 2,
        },
    ),
]

#: Values that legitimately differ between two runs of the same sequence.
VOLATILE_KEYS = frozenset(
    {
        "id",
        "sweep_id",
        "observation_id",
        "content_digest",
        "observed_at",
        "last_swept_at",
        "age_seconds",
        "oldest_relevant_sweep",
        "collectors",
        "suppression",
        "opened_at",
        "resolved_at",
        "checked_at",
        "evidence_snapshot",
        "evidence_observation_id",
        "subject_id",
        "path",
        "secret",
        "last_used_at",
        "expires_at",
        "token_id",
        "revoked_at",
        "instance_id",
        "entity_id",
        "actor_id",
        "audit_id",
        "txn_id",
        "assignee_actor_id",
        "initiative_id",
        "project_id",
        "related_id",
        "work_item_id",
        "created_at",
        "updated_at",
        "compliance_checked_at",
        "contract_adopted_at",
        "adopted_at",
        "declared_at",
        "at",
        "data_dir",
        "payload_digest",
    }
)


def normalise(value: Any, replacements: dict[str, str]) -> Any:
    """Blank out what cannot match across independent instances."""
    if isinstance(value, dict):
        return {
            key: (
                "<volatile>" if key in VOLATILE_KEYS else normalise(item, replacements)
            )
            for key, item in value.items()
        }
    if isinstance(value, list):
        return [normalise(item, replacements) for item in value]
    if isinstance(value, str):
        for needle, token in replacements.items():
            value = value.replace(needle, token)
        return value
    if isinstance(value, float):
        # Staleness is a function of wall-clock age; the injected clock makes
        # it deterministic, but rounding keeps the comparison about ordering.
        return round(value, 3)
    return value


def _write_fixture_tree(root: Path) -> None:
    """A tiny project the offline collectors have something to say about.

    Deterministic on purpose: the same files, the same line numbers, and so
    the same subject keys on every transport — which is what lets the script
    adopt a specific marker by key and compare the results.
    """
    project = root / "fixture"
    (project / "src").mkdir(parents=True)
    (project / "notes.md").write_text(
        "TODO(vogt): the parity harness adopts this one\n"
        "TODO: this one is not promoted and must stay out of ranked views\n",
        encoding="utf-8",
    )
    (project / "pyproject.toml").write_text(
        '[project]\nname = "fixture"\n\n'
        '[tool.uv.sources]\nsibling = { path = "../sibling" }\n',
        encoding="utf-8",
    )
    _write_publishable_repo(root / "publishable")


def _write_publishable_repo(root: Path) -> None:
    """A real, clean git checkout for `forge.publish`'s read-only gate.

    The gate (`inspect_publish_source`) runs for real on every transport's
    instance — clean tree, branch `main`, one commit — so publish parity
    covers the production path up to the network edges, which the stand-in
    forge and the recording pusher then answer deterministically.
    """
    root.mkdir(parents=True)
    (root / "README.md").write_text("published by the parity harness\n")

    def git(*args: str) -> None:
        subprocess.run(
            ["git", *args],
            cwd=root,
            check=True,
            capture_output=True,
        )

    git("init", "-q", "-b", "main")
    git("add", ".")
    git(
        "-c",
        "user.email=parity@example.invalid",
        "-c",
        "user.name=Parity",
        "commit",
        "-q",
        "-m",
        "seed",
    )


def _recording_pusher(pushes: list[PushRequest]) -> Pusher:
    """A pusher that records the request and answers deterministically.

    The revision is fixed for the same reason the cloner's is: three
    independent checkouts commit at three different instants, and the parity
    assertion is about the surfaces, not about commit hashing.
    """

    def push(request: PushRequest) -> PushOutcome:
        pushes.append(request)
        return PushOutcome(
            remote=request.remote, branch=request.branch, revision="0" * 40
        )

    return push


def _recording_cloner(root: Path) -> Cloner:
    """A clone that writes a directory and never touches the network.

    `project.import` is the one operation whose real implementation reaches
    the internet, and parity has to drive every shared operation (FR-A3). The
    cloner is injected through the context for exactly this reason, and what
    the three transports then compare is the operation's behaviour rather
    than GitHub's availability.
    """

    def clone(request: CloneRequest) -> CloneOutcome:
        destination = Path(str(request.destination).replace("{root}", str(root)))
        destination.mkdir(parents=True, exist_ok=True)
        (destination / "README.md").write_text("imported\n", encoding="utf-8")
        return CloneOutcome(
            destination=destination,
            revision="0" * 40,
            default_branch="main",
        )

    return clone


class _StandInForge:
    """A deterministic forge, one per instance (#179, #180, #181).

    It validates the parity PAT, enumerates the picker's repos, *mints
    issues* for the write-through plane and then lists them back for the
    sync collectors — so `work.create` on the linked project and the
    `forge.onboard` that mirrors it produce byte-identical state on all
    three transports without a network. Deterministic issue numbers per
    instance are what make the subject-key refs comparable.
    """

    def __init__(self) -> None:
        self.issues: list[dict[str, Any]] = []

    def __call__(
        self,
        url: str,
        headers: dict[str, str],
        body: bytes = b"",
        method: str = "GET",
    ) -> tuple[int, bytes]:
        del headers
        payload = json.loads(body.decode()) if body else {}
        if method == "GET" and url.endswith("/user"):
            return 200, json.dumps({"login": "parity-user"}).encode()
        if method == "GET" and "/user/repos" in url:
            return 200, json.dumps(
                [
                    {
                        "name": "parity-import",
                        "owner": {"login": "parity-org"},
                        "default_branch": "main",
                        "private": False,
                        "html_url": "https://github.com/parity-org/parity-import",
                    },
                    {
                        "name": "private-thing",
                        "owner": {"login": "parity-user"},
                        "default_branch": "trunk",
                        "private": True,
                        "html_url": "https://github.com/parity-user/private-thing",
                    },
                ]
            ).encode()
        if method == "POST" and url.rstrip("/").endswith("/user/repos"):
            # `forge.publish` (#182): mint the repository deterministically.
            name = str(payload.get("name", ""))
            return 201, json.dumps(
                {
                    "name": name,
                    "owner": {"login": "parity-user"},
                    "private": payload.get("private", True),
                    "default_branch": None,
                    "html_url": f"https://github.com/parity-user/{name}",
                }
            ).encode()
        if method == "POST" and url.rstrip("/").endswith("/issues"):
            number = len(self.issues) + 1
            issue = {
                "number": number,
                "title": payload.get("title", ""),
                "state": "open",
                "labels": [{"name": str(name)} for name in payload.get("labels", [])],
                "comments": 0,
                "updated_at": f"2026-08-01T00:00:{number:02d}Z",
                "html_url": (
                    f"https://github.com/parity-org/parity-project/issues/{number}"
                ),
            }
            self.issues.append(issue)
            return 200, json.dumps(issue).encode()
        if method == "POST" and "/comments" in url:
            return 200, json.dumps(
                {"html_url": "https://github.com/parity-org/parity-project/issues/1"}
            ).encode()
        if method == "POST" and "/labels" in url:
            return 200, json.dumps({"number": 1}).encode()
        if method == "PATCH" and "/issues/" in url:
            number = int(url.rstrip("/").rsplit("/", 1)[1])
            for issue in self.issues:
                if issue["number"] == number:
                    issue["state"] = payload.get("state", issue["state"])
            return 200, json.dumps({"number": number}).encode()
        if method == "GET" and "/issues" in url and "/comments" not in url:
            return 200, json.dumps(self.issues).encode()
        if "/actions/runs" in url:
            return 200, json.dumps({"workflow_runs": []}).encode()
        if "/contents/" in url:
            return 404, b""
        if "/vulnerability-alerts" in url or "/automated-security-fixes" in url:
            return 404, b""
        if method == "GET" and (
            "/pulls" in url
            or "/labels" in url
            or "/releases" in url
            or "/notifications" in url
        ):
            return 200, b"[]"
        if method == "GET" and "/repos/" in url:
            # `describe` — the bare repository read `project.import` makes.
            tail = url.split("/repos/", 1)[1].split("?", 1)[0]
            if tail.count("/") == 1:
                return 200, json.dumps({"default_branch": "main"}).encode()
        return 404, b""


def _forge_key_file(root: Path) -> Path:
    """A per-instance Fernet key, so account linking is configured in parity."""
    from cryptography.fernet import Fernet

    path = root / "forge_account_key"
    path.write_bytes(Fernet.generate_key())
    return path


def _stand_in_engine() -> EngineClient:
    """An engine that answers predictably, so three transports can be compared.

    Deterministic ids on purpose: the parity assertion is that CLI, REST and
    MCP produce the *same* answer, and an engine handing out random session
    ids would make three identical runs differ for a reason that has nothing
    to do with the surfaces.
    """
    counter = itertools.count(1)

    def transport(
        url: str,
        headers: dict[str, str],
        body: bytes = b"",
        method: str = "GET",
    ) -> tuple[int, bytes]:
        spec = json.loads(body.decode("utf-8")) if body else {}
        if method == "POST" and url.endswith("/api/sessions"):
            return 200, json.dumps(
                {
                    "id": f"eng-{next(counter)}",
                    "name": spec.get("name", ""),
                    "activity": "running",
                    "cwd": spec.get("cwd", ""),
                    "exit_code": None,
                }
            ).encode()
        if method == "POST" and url.endswith("/kill"):
            return 200, b'{"ok":true}'
        if method == "GET" and url.endswith("/api/sessions"):
            return 200, b"[]"
        return 404, b""

    return EngineClient(base_url="http://127.0.0.1:8910", transport=transport)


def _fresh(
    tmp_path_factory: pytest.TempPathFactory, label: str
) -> tuple[AppContext, Path]:
    root = tmp_path_factory.mktemp(label)
    _write_fixture_tree(root)
    # A file token alongside the per-actor PAT: `forge.onboard`'s sync path
    # deliberately uses the FR-S7 file token (sweeps have no acting person),
    # so mirroring the write-through issue needs one. Every provider the
    # instance builds is wired to this instance's own stand-in forge.
    token_file = root / "github_token"
    token_file.write_text("ghp_parity_file", encoding="utf-8")
    context = build_context(
        config=VogtConfig(
            data_dir=root / "instance",
            import_root=root / "imported",
            forge_account_key_file=_forge_key_file(root),
            github_token_file=token_file,
        ),
        principal=TEST_PRINCIPAL,
        clock=StepClock(),
        id_factory=SequentialIds(),
        cloner=_recording_cloner(root),
        pusher=_recording_pusher([]),
        engine=_stand_in_engine(),
        forge_transport=_StandInForge(),
    )
    init_instance(context, InitParams())
    return context, root


def _resolved(params: dict[str, Any], root: Path) -> dict[str, Any]:
    return {
        key: (value.replace("{root}", str(root)) if isinstance(value, str) else value)
        for key, value in params.items()
    }


def _argv_for(operation: Operation[Any, Any], params: dict[str, Any]) -> list[str]:
    argv = [*operation.cli.path]
    for key, value in params.items():
        flag = f"--{key.replace('_', '-')}"
        if isinstance(value, bool):
            argv.append(flag if value else f"--no-{key.replace('_', '-')}")
        elif isinstance(value, list):
            for entry in value:
                argv += [
                    flag,
                    json.dumps(entry) if isinstance(entry, dict) else str(entry),
                ]
        else:
            argv += [flag, str(value)]
    return argv


def _via_cli(context: AppContext, name: str, params: dict[str, Any]) -> Any:
    registry = default_registry()
    result = run(
        ["--json", *_argv_for(registry.get(name), params)],
        registry=registry,
        context=context,
    )
    assert result.exit_code == EXIT_OK, f"{name}: {result.stderr}"
    return json.loads(result.stdout)


def _via_http(context: AppContext, name: str, params: dict[str, Any]) -> Any:
    registry = default_registry()
    operation = registry.get(name)
    client = TestClient(build_app(registry=registry, context_factory=lambda: context))
    url = f"{API_PREFIX}{operation.route.path}"
    if operation.route.method == "GET":
        response = client.get(url, params=params)
    else:
        response = client.post(url, json=params)
    assert response.status_code == 200, f"{name}: {response.text}"
    return response.json()


def _via_mcp(context: AppContext, name: str, params: dict[str, Any]) -> Any:
    registry = default_registry()
    surface = McpSurface(registry=registry, context_factory=lambda: context)
    return surface.call_tool(registry.get(name).mcp_tool_name, params)


DRIVERS = {"cli": _via_cli, "http": _via_http, "mcp": _via_mcp}


@pytest.fixture(scope="module")
def parity_run(
    tmp_path_factory: pytest.TempPathFactory,
) -> dict[str, Any]:
    """Run the whole script on every transport, against three fresh instances."""
    instances = {
        transport: _fresh(tmp_path_factory, transport) for transport in DRIVERS
    }
    replacements = {
        transport: {
            str(root): "<root>",
            str(context.config.resolved_data_dir): "<data>",
        }
        for transport, (context, root) in instances.items()
    }

    answers: list[dict[str, Any]] = []
    seen: dict[str, dict[str, Any]] = {transport: {} for transport in DRIVERS}
    for name, params in SCRIPT:
        step: dict[str, Any] = {"operation": name}
        for transport, (context, root) in instances.items():
            resolved = params(seen[transport]) if callable(params) else params
            raw = DRIVERS[transport](context, name, _resolved(resolved, root))
            seen[transport][name] = raw
            step[transport] = normalise(raw, replacements[transport])
        answers.append(step)

    trails: dict[str, Any] = {}
    for transport, (context, _) in instances.items():
        with context.declared.read() as view:
            trails[transport] = [
                normalise(record.model_dump(mode="json"), replacements[transport])
                for record in view.list_audit(limit=200)
            ]
    return {"steps": answers, "trails": trails}


# -- the matrix ------------------------------------------------------------


@pytest.mark.parametrize("index", range(len(SCRIPT)))
def test_transports_return_the_same_answer(
    index: int, parity_run: dict[str, Any]
) -> None:
    step = parity_run["steps"][index]
    name = step["operation"]
    assert step["cli"] == step["http"], f"{name}: CLI and REST disagree"
    assert step["cli"] == step["mcp"], f"{name}: CLI and MCP disagree"


def test_transports_leave_the_same_audit_trail(parity_run: dict[str, Any]) -> None:
    trails = parity_run["trails"]
    assert trails["cli"] == trails["http"]
    assert trails["cli"] == trails["mcp"]

    operations = {record["operation"] for record in trails["cli"]}
    assert "instance.init" in operations
    assert "work.create" in operations
    assert "work.transition" in operations
    assert all(record["reason"] for record in trails["cli"])


# -- staleness, in both directions ----------------------------------------


def test_every_operation_appears_on_exactly_its_expected_surfaces() -> None:
    registry = default_registry()
    parser_text = build_parser(registry).format_help()
    app = build_app(registry=registry, context_factory=build_context)
    http_routes = {
        (getattr(route, "path", ""), method)
        for route in app.routes
        for method in getattr(route, "methods", set())
    }
    mcp_tools = {tool.name for tool in McpSurface(registry=registry).list_tools()}

    for operation in registry:
        expected = registry.transports_for(operation.name)
        route_key = (f"{API_PREFIX}{operation.route.path}", operation.route.method)

        assert (route_key in http_routes) is ("http" in expected), (
            f"{operation.name}: REST presence does not match its exclusion state"
        )
        assert (operation.mcp_tool_name in mcp_tools) is ("mcp" in expected), (
            f"{operation.name}: MCP presence does not match its exclusion state"
        )
        assert (operation.cli.path[0] in parser_text) is ("cli" in expected), (
            f"{operation.name}: CLI presence does not match its exclusion state"
        )


def test_exclusion_lists_name_only_registered_operations() -> None:
    registry = default_registry()
    for name in (*LOCAL_ONLY, *HTTP_ONLY):
        assert name in registry, f"stale parity exclusion: {name}"


def test_exclusions_carry_a_justification() -> None:
    for name, reason in (*LOCAL_ONLY.items(), *HTTP_ONLY.items()):
        assert reason.strip(), f"{name} is excluded without saying why"


def test_every_shared_operation_is_driven_by_the_script() -> None:
    registry = default_registry()
    shared = {
        operation.name
        for operation in registry
        if registry.transports_for(operation.name) >= frozenset({"cli", "http", "mcp"})
    }
    covered = {name for name, _ in SCRIPT}
    assert shared == covered, (
        "every operation on all three surfaces must appear in the parity script; "
        f"missing: {sorted(shared - covered)}, stale: {sorted(covered - shared)}"
    )
