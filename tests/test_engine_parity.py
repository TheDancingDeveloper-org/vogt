"""Every session-engine route is answered for on MCP.

The core's operations reach MCP by construction (`tests/test_parity.py`).
The engine's routes do not: they are Rust handlers the PWA calls directly, so
the GUI can gain a capability agents lack — renaming a session was one. This
reads the engine's router and holds every route to the tables in
`vogt.registry.engine_routes`: an MCP counterpart, or a written reason there
is none. Like the core's exclusions, the check fails in both directions.
"""

from __future__ import annotations

import re
from pathlib import Path

import pytest

from vogt.registry import ENGINE_COUNTERPARTS, ENGINE_ONLY, default_registry

REPO_ROOT = Path(__file__).resolve().parents[1]
ENGINE_SRC = REPO_ROOT / "engine" / "server" / "src"
ENGINE_APP = ENGINE_SRC / "app.rs"
WEB_SRC = REPO_ROOT / "web" / "src"

pytestmark = pytest.mark.skipif(
    not ENGINE_APP.is_file(),
    reason="the merged tree carries the engine; a core-only checkout does not",
)

_METHOD = re.compile(r"\b(get|post|put|patch|delete|any)\(")


def _route_constants() -> dict[str, str]:
    """`pub const NAME: &str = "/api/..."` across the engine, by name."""
    found: dict[str, str] = {}
    for path in ENGINE_SRC.glob("*.rs"):
        for name, value in re.findall(
            r'pub const ([A-Z_]+): &str = "(/[^"]*)"', path.read_text("utf-8")
        ):
            found[name] = value
    return found


def _probe_paths() -> list[str]:
    text = (ENGINE_SRC / "vogt_core.rs").read_text("utf-8")
    block = re.search(r"pub const PROBE_PATHS: \[&str; \d+\] = \[(.*?)\];", text, re.S)
    assert block, "PROBE_PATHS was not found in vogt_core.rs"
    return re.findall(r'"([^"]+)"', block.group(1))


def engine_routes() -> set[str]:
    """Every `METHOD /path` the engine's router serves.

    Each `.route(` call is read to its matching parenthesis, so a route whose
    handlers span lines (`get(..).patch(..).delete(..)`) yields one entry per
    method. A path named by a constant is resolved through it, and the probe
    loop through `PROBE_PATHS`.
    """
    text = ENGINE_APP.read_text("utf-8")
    constants = _route_constants()
    routes: set[str] = set()
    start = 0
    while (found := text.find(".route(", start)) != -1:
        cursor = found + len(".route(")
        depth = 1
        end = cursor
        while depth:
            depth += {"(": 1, ")": -1}.get(text[end], 0)
            end += 1
        call = text[cursor : end - 1]
        start = end
        literal = re.match(r'\s*"([^"]+)"', call)
        if literal:
            paths = [literal.group(1)]
        else:
            name = re.match(r"\s*([\w:]+)", call)
            assert name, f"unreadable route call: {call[:60]!r}"
            ident = name.group(1).rsplit("::", 1)[-1]
            if ident == "path":
                paths = _probe_paths()
            else:
                assert ident in constants, f"route constant {ident} not found"
                paths = [constants[ident]]
        methods = _METHOD.findall(call)
        assert methods, f"no handler method in route call: {call[:60]!r}"
        routes |= {f"{m.upper()} {p}" for m in methods for p in paths}
    assert len(routes) > 50, "the router read implausibly few routes"
    return routes


def test_every_engine_route_has_an_mcp_counterpart_or_a_reason() -> None:
    unaccounted = sorted(engine_routes() - set(ENGINE_COUNTERPARTS) - set(ENGINE_ONLY))
    assert not unaccounted, (
        f"engine routes {unaccounted} have no MCP counterpart and no reason: add "
        "the core operation that proxies each (as session.rename does) to "
        "ENGINE_COUNTERPARTS, or say why there is none in ENGINE_ONLY "
        "(src/vogt/registry/engine_routes.py)"
    )


def test_no_engine_route_entry_is_stale() -> None:
    routes = engine_routes()
    stale = sorted((set(ENGINE_COUNTERPARTS) | set(ENGINE_ONLY)) - routes)
    assert not stale, f"entries name routes the engine no longer serves: {stale}"


def test_no_engine_route_is_both_covered_and_excluded() -> None:
    both = sorted(set(ENGINE_COUNTERPARTS) & set(ENGINE_ONLY))
    assert not both, f"routes listed as both counterpart and exclusion: {both}"


def test_every_counterpart_is_an_operation_on_mcp() -> None:
    registry = default_registry()
    wrong = {
        route: name
        for route, name in ENGINE_COUNTERPARTS.items()
        if name not in registry or "mcp" not in registry.transports_for(name)
    }
    assert not wrong, f"counterparts that are not MCP operations: {wrong}"


def test_every_exclusion_says_why() -> None:
    for route, reason in ENGINE_ONLY.items():
        assert len(reason.split()) >= 8, f"{route} needs a real justification"


def _normalise(path: str) -> str:
    interpolated = path.find("${")
    if interpolated != -1:
        path = path[:interpolated]
    return re.sub(r"\{[^}]*\}", "{}", path).rstrip("/")


@pytest.mark.skipif(not WEB_SRC.is_dir(), reason="no PWA in this checkout")
def test_every_engine_path_the_pwa_calls_is_accounted_for() -> None:
    """The GUI's own calls, named directly: the router check above already
    covers them, but this failure says which GUI call is the uncovered one."""
    accounted = {
        _normalise(entry.split(" ", 1)[1])
        for entry in (*ENGINE_COUNTERPARTS, *ENGINE_ONLY)
    }
    literals: set[str] = set()
    sources = sorted(WEB_SRC.rglob("*.ts")) + sorted(WEB_SRC.rglob("*.tsx"))
    for path in sources:
        if "__tests__" in path.parts:
            continue
        literals |= set(
            re.findall(r"""["'`](/api/[A-Za-z0-9/_.$\-{}]*)""", path.read_text("utf-8"))
        )
    unresolved = sorted(
        literal
        for literal in literals
        if not literal.startswith("/api/vogt")
        and not any(
            known == _normalise(literal) or known.startswith(_normalise(literal))
            for known in accounted
        )
    )
    assert not unresolved, f"the PWA calls {unresolved}, which no entry accounts for"
