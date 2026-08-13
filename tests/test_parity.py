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

import json
from collections.abc import Callable
from pathlib import Path
from typing import Any

import pytest
from fastapi.testclient import TestClient

from vogt.adapters.cli.main import EXIT_OK, build_parser, run
from vogt.adapters.git import CloneOutcome, Cloner, CloneRequest
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
    (
        "project.register",
        {"name": "Parity Project", "root_path": "/srv/parity", "reason": WHY},
    ),
    ("project.get", {"slug": "parity-project"}),
    ("project.list", {}),
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
    (
        "work.create",
        {
            "kind": "bug",
            "title": "Ranked bug",
            "body": "raised by the parity harness",
            "priority": "p1",
            "effort": "s",
            "project": "parity-project",
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
            "project": "parity-project",
            "reason": WHY,
        },
    ),
    ("work.get", {"ref": "WI-1"}),
    ("work.list", {"project": "parity-project"}),
    ("work.update", {"ref": "WI-1", "priority": "p0", "reason": WHY}),
    (
        "work.relate",
        {"ref": "WI-1", "kind": "depends_on", "target": "WI-2", "reason": WHY},
    ),
    ("work.transition", {"ref": "WI-2", "to_state": "in_progress", "reason": WHY}),
    ("work.comment", {"ref": "WI-1", "body": "seen by the harness", "reason": WHY}),
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
    ("contract.check", {"project": "parity-fixture", "reason": WHY}),
    ("compliance", {"project": "parity-fixture"}),
    ("drift.detect", {"reason": WHY}),
    ("drift.list", {"status": "open"}),
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
    # -- the forge module ---------------------------------------------------
    (
        "forge.writeback",
        {"project": "parity-project", "policy": "comment_only", "reason": WHY},
    ),
    ("forge.onboard", {"project": "parity-project", "reason": WHY}),
    ("forge.actions", {}),
    ("export", {"destination": "{root}/export.json", "reason": WHY}),
    ("events.list", {}),
    ("audit.list", {}),
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


def _fresh(
    tmp_path_factory: pytest.TempPathFactory, label: str
) -> tuple[AppContext, Path]:
    root = tmp_path_factory.mktemp(label)
    _write_fixture_tree(root)
    context = build_context(
        config=VogtConfig(data_dir=root / "instance", import_root=root / "imported"),
        principal=TEST_PRINCIPAL,
        clock=StepClock(),
        id_factory=SequentialIds(),
        cloner=_recording_cloner(root),
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
                argv += [flag, str(entry)]
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
