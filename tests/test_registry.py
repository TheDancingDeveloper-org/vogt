"""The registry refuses to build an inconsistent surface.

These are all programming errors caught at import time rather than in
review — which is the point of having one registry instead of three
hand-written surfaces.
"""

from __future__ import annotations

import ast
import inspect
import textwrap
from typing import Any

import pytest
from pydantic import BaseModel

from vogt.application.context import AppContext
from vogt.application.models import Params, Result, StatusParams, StatusResult
from vogt.application.services import status
from vogt.registry import default_registry
from vogt.registry.operation import CliBinding, HttpRoute, Operation
from vogt.registry.registry import OperationRegistry, RegistryError


class NoReasonParams(Params):
    thing: str


class OptionalReasonParams(Params):
    reason: str = "because"


class Ok(Result):
    ok: bool = True


def _noop(ctx: AppContext, params: BaseModel) -> Ok:
    del ctx, params
    return Ok()


def _operation(
    name: str,
    *,
    mutating: bool = False,
    params: type[BaseModel] = StatusParams,
    route: HttpRoute | None = None,
    cli: tuple[str, ...] = ("thing",),
) -> Operation[Any, Any]:
    return Operation(
        name=name,
        summary="test operation",
        scope="read",
        mutating=mutating,
        params_model=params,
        result_model=Ok,
        handler=_noop,
        route=route or HttpRoute("GET", "/thing"),
        cli=CliBinding(cli),
    )


def test_the_shipped_registry_builds() -> None:
    registry = default_registry()
    assert "status" in registry
    assert len(registry) == len(registry.names)


def test_duplicate_names_are_refused() -> None:
    with pytest.raises(RegistryError, match="duplicate operation name"):
        OperationRegistry([_operation("thing"), _operation("thing")])


def test_two_operations_cannot_claim_one_route() -> None:
    with pytest.raises(RegistryError, match="route GET /thing"):
        OperationRegistry(
            [_operation("a"), _operation("b", cli=("other",))],
        )


def test_two_operations_cannot_claim_one_cli_path() -> None:
    with pytest.raises(RegistryError, match="CLI path thing"):
        OperationRegistry(
            [
                _operation("a"),
                _operation("b", route=HttpRoute("GET", "/other")),
            ]
        )


def test_two_operations_cannot_claim_one_mcp_tool_name() -> None:
    """`a.b` and `a_b` would collide once dots become underscores."""
    with pytest.raises(RegistryError, match="MCP tool"):
        OperationRegistry(
            [
                _operation("a.b", cli=("one",)),
                _operation("a_b", route=HttpRoute("GET", "/other"), cli=("two",)),
            ]
        )


def test_a_mutating_operation_without_a_reason_is_refused() -> None:
    with pytest.raises(RegistryError, match="no reason field"):
        OperationRegistry([_operation("w", mutating=True, params=NoReasonParams)])


def test_a_defaulted_reason_is_refused() -> None:
    with pytest.raises(RegistryError, match="optional reason"):
        OperationRegistry([_operation("w", mutating=True, params=OptionalReasonParams)])


def test_a_route_path_must_be_absolute() -> None:
    with pytest.raises(ValueError, match="must start with"):
        HttpRoute("GET", "thing")


def test_a_cli_binding_needs_a_path() -> None:
    with pytest.raises(ValueError, match="at least one path segment"):
        CliBinding(())


def test_unknown_lookups_are_errors() -> None:
    registry = default_registry()
    with pytest.raises(RegistryError, match="unknown operation"):
        registry.get("nope")
    with pytest.raises(RegistryError, match="unknown MCP tool"):
        registry.by_mcp_tool("nope")


def test_operations_can_be_run_from_raw_input(instance: AppContext) -> None:
    operation = default_registry().get("status")
    assert operation.run_raw(instance, {}).instance_id


def test_every_operation_declares_a_scope_and_a_summary() -> None:
    for operation in default_registry():
        assert operation.summary.strip()
        assert operation.scope in {
            "read",
            "work.write",
            "project.write",
            "admin",
            "writeback",
        }


def test_read_operations_use_get_and_writes_do_not() -> None:
    for operation in default_registry():
        if operation.mutating:
            assert operation.route.method != "GET", (
                f"{operation.name} mutates behind GET"
            )


#: Mutating operations that deliberately write no audit row, each with the
#: reason it is exempt. An addition here is a decision, which is the point of
#: making it a literal a reviewer has to read rather than an absence nobody
#: notices — every entry below was once an absence nobody noticed.
UNAUDITED_BY_DESIGN = {
    "sweep": (
        "runs every sweep_interval_seconds on the in-process schedule with a "
        "fixed reason and no person to name; auditing it would add ninety-six "
        "identical rows a day and bury the writes somebody chose. Its "
        "accountability is the coverage record and sweep.completed (FR-O3)."
    ),
}


def _audits(function: object, *, seen: set[object] | None = None) -> bool:
    """Whether a call reaches `audited_write`/`audited_action`, at any depth.

    Transitive on purpose: `project.create` scaffolds, then calls
    `register_project`, which calls `record_registration`, which audits. Three
    frames is a reasonable way to write that and an unreasonable thing for a
    test to be fooled by.
    """
    seen = set() if seen is None else seen
    if function in seen or not inspect.isfunction(function):
        return False
    seen.add(function)
    module = inspect.getmodule(function)
    if module is None or not module.__name__.startswith("vogt."):
        return False
    called = {
        node.func.id
        for node in ast.walk(ast.parse(textwrap.dedent(inspect.getsource(function))))
        if isinstance(node, ast.Call) and isinstance(node.func, ast.Name)
    }
    if called & {"audited_write", "audited_action"}:
        return True
    # Locally-imported names — `writeback.py` imports `audited_write` inside
    # the function that uses it — are not in module globals, so check both.
    scope = dict(vars(module))
    scope.update(function.__globals__)
    return any(_audits(scope.get(name), seen=seen) for name in called)


def test_every_mutating_operation_records_who_asked_and_why() -> None:
    """FR-S1: a required `reason` that is discarded is worse than none.

    Four operations demanded one and threw it away — `forge.onboard`,
    `observations.prune`, `drift.detect` and `sweep`. Three are fixed; `sweep`
    is exempt above with the reason it is exempt. This walks the handlers
    rather than trusting a list, because a list is what was wrong.
    """
    for operation in default_registry():
        if not operation.mutating or operation.name in UNAUDITED_BY_DESIGN:
            continue
        assert _audits(inspect.unwrap(operation.handler)), (
            f"{operation.name} is mutating, requires a reason, and stores it "
            "nowhere. Either route it through audited_write/audited_action, or "
            "add it to UNAUDITED_BY_DESIGN with the reason it is exempt."
        )


def test_the_audit_exemptions_are_real_operations() -> None:
    """An exemption for an operation that no longer exists is stale cover."""
    names = {operation.name for operation in default_registry()}
    assert set(UNAUDITED_BY_DESIGN) <= names
    for name, why in UNAUDITED_BY_DESIGN.items():
        assert len(why) > 40, f"{name}'s exemption needs a reason, not a label"


def test_status_handler_is_wired_to_the_status_operation() -> None:
    operation = default_registry().get("status")
    assert operation.handler is status
    assert operation.params_model is StatusParams
    assert operation.result_model is StatusResult
