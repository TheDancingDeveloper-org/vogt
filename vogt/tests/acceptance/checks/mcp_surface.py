"""Group 3 — the MCP surface an agent sees."""

from __future__ import annotations

import json
import tomllib
from pathlib import Path

from ..harness import (
    Case,
    Check,
    Context,
    HttpStatusError,
    RpcError,
    ToolError,
    failed,
    passed,
)

HERE = Path(__file__).resolve().parent.parent
GOLDEN = HERE.parent / "parity" / "golden" / "mcp-tools.json"


def expected_tool_names() -> set[str]:
    document = json.loads(GOLDEN.read_text("utf-8"))
    return {str(tool["name"]) for tool in document["tools"]}


def tool_inventory(ctx: Context) -> Check:
    tools = ctx.client.list_tools()
    actual = {str(tool.get("name")) for tool in tools}
    expected = expected_tool_names()
    missing = sorted(expected - actual)
    unexpected = sorted(actual - expected)
    if missing or unexpected:
        return failed(
            f"tools/list differs from tests/parity/golden/mcp-tools.json: "
            f"missing={missing[:12]} unexpected={unexpected[:12]} "
            f"(missing {len(missing)}, unexpected {len(unexpected)})",
            missing=missing,
            unexpected=unexpected,
        )
    return passed(f"tools/list matches the checked-in registry ({len(actual)} tools)")


def initialize(ctx: Context) -> Check:
    result = ctx.client.initialize()
    version = result.get("protocolVersion")
    server = result.get("serverInfo")
    if not version or not isinstance(server, dict):
        return failed(f"initialize returned {result!r}")
    return passed(
        f"initialize -> protocol {version}, "
        f"{server.get('name')} {server.get('version')}"
    )


def unknown_tool(ctx: Context) -> Check:
    try:
        ctx.client.call("tools/call", {"name": "no_such_tool", "arguments": {}})
    except RpcError as exc:
        if exc.code not in (-32601, -32602):
            return failed(f"unknown tool answered JSON-RPC {exc.code}: {exc.message}")
        return passed(f"unknown tool refused with JSON-RPC {exc.code}: {exc.message}")
    return failed("an unknown tool was accepted")


def registry_shape(ctx: Context) -> Check:
    payload = ctx.client.tool("registry_dump")
    operations = payload.get("operations")
    if not isinstance(operations, list):
        return failed(f"registry_dump has no operations list: {list(payload)[:6]}")
    names = {str(row.get("name")) for row in operations if isinstance(row, dict)}
    wanted = {"session.start", "work.create", "work.get", "status", "project.list"}
    missing = sorted(wanted - names)
    if missing:
        return failed(f"registry_dump is missing {missing} ({len(names)} operations)")
    if len(names) < 100:
        return failed(
            f"registry_dump has only {len(names)} operations; "
            "expected the full registry"
        )
    return passed(
        f"registry_dump lists {len(names)} operations, including {sorted(wanted)}"
    )


def minimal_read_calls(ctx: Context) -> Check:
    calls = tomllib.loads((HERE / "minimal_read_calls.toml").read_text("utf-8"))["call"]
    failures: list[str] = []
    answered: list[str] = []
    for call in calls:
        tool = str(call["tool"])
        arguments = dict(call.get("args") or {})
        try:
            ctx.client.tool(tool, arguments)
            answered.append(tool)
        except ToolError as exc:
            text = str(exc)
            if "internal error (ref" in text:
                failures.append(f"{tool}: server-side error: {text[:120]}")
            else:
                answered.append(tool)
        except RpcError as exc:
            failures.append(f"{tool}: no such tool/params: {exc.message}")
        except HttpStatusError as exc:
            failures.append(f"{tool}: HTTP {exc.status}")
    if failures:
        return failed("; ".join(failures), failures=failures)
    return passed(
        f"{len(answered)} read tools answered without a 5xx or internal error"
    )


def invalid_arguments(ctx: Context) -> Check:
    result, error = ctx.client.try_tool("work_get", {"ref": {"not": "a ref"}})
    if result is not None:
        return failed("work_get accepted a non-string ref")
    if error is None:
        return failed("work_get neither answered nor refused")
    lowered = error.lower()
    if "internal error" in lowered:
        return failed(f"a bad argument produced a server error: {error}")
    return passed(f"bad arguments refused as data: {error[:160]}")


def whoami(ctx: Context) -> Check:
    payload = ctx.client.tool("auth_whoami")
    identity = (
        payload.get("identity_ref")
        or payload.get("display_name")
        or payload.get("identity")
        or payload.get("name")
    )
    if not identity:
        return failed(f"auth_whoami returned {payload!r}")
    scopes = payload.get("scopes")
    return passed(f"auth_whoami -> {identity} scopes={scopes}")


CASES = [
    Case(
        "M1",
        "mcp",
        "W1",
        "critical",
        "tools/list matches the checked-in registry",
        tool_inventory,
    ),
    Case("M2", "mcp", "W1", "high", "initialize completes", initialize),
    Case(
        "M3", "mcp", "W1", "medium", "an unknown tool is a JSON-RPC error", unknown_tool
    ),
    Case(
        "M4",
        "mcp",
        "W1",
        "high",
        "registry_dump carries the full operation set",
        registry_shape,
    ),
    Case(
        "M5",
        "mcp",
        "T1",
        "critical",
        "minimal read calls never 5xx",
        minimal_read_calls,
    ),
    Case(
        "M6",
        "mcp",
        "T2",
        "high",
        "bad arguments are a data refusal, not a server error",
        invalid_arguments,
    ),
    Case("M7", "mcp", "W1", "medium", "auth_whoami names the credential", whoami),
]
