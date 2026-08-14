"""The Solid PWA, held to the rule the legacy GUI has kept since M6 (FR-U8).

`REQUIREMENTS.md` FR-U8: the PWA shall consume only public APIs, and every URL
in the shipped bundle shall resolve against the operation registry *and* the
engine's API contract.

Both halves are checked here, and both are checked against *source*, not
intent — the same reasoning `test_gui.py` gives at length. The engine half
resolves against `app.rs`'s route table *and* against `docs/engine/
API_CONTRACT.md`, and they are not the same check. The router is the stronger
one: it is what answers requests, and a check against the description alone
would pass while the product was broken. The document is the thing that rots,
so it is checked too — and when the two disagree, the document is the one that
is wrong.
"""

from __future__ import annotations

import re
from pathlib import Path

import pytest

from vogt.adapters.http.app import API_PREFIX
from vogt.registry import default_registry

REPO_ROOT = Path(__file__).resolve().parents[1]
WEB_SRC = REPO_ROOT / "web" / "src"
VOGT_CLIENT = WEB_SRC / "vogtApi.ts"
ENGINE_APP = REPO_ROOT / "engine" / "server" / "src" / "app.rs"
ENGINE_CONTRACT_DOC = REPO_ROOT / "docs" / "engine" / "API_CONTRACT.md"

#: The front door's own mount. Paths under it are Vogt operations and are
#: resolved by the registry half of this file; the engine has three catch-all
#: routes for the prefix and nothing more specific to match against.
FRONT_DOOR_PREFIX = "/api/vogt"

pytestmark = pytest.mark.skipif(
    not VOGT_CLIENT.is_file(),
    reason="the merged tree carries the PWA; a core-only checkout does not",
)


def source(path: Path) -> str:
    """A TypeScript file with its comments removed.

    Same precaution as the legacy GUI's checker: a comment explaining a rule
    contains the words the rule forbids, and this file's own module comment
    names `/api/vogt/...` while forbidding unregistered paths.
    """
    text = re.sub(r"/\*.*?\*/", "", path.read_text(encoding="utf-8"), flags=re.S)
    kept: list[str] = []
    for line in text.splitlines():
        head = line.split("//")[0]
        kept.append(line if head.count('"') % 2 or head.count("`") % 2 else head)
    return "\n".join(kept)


def client_routes() -> dict[str, str]:
    block = re.search(
        r"export const ROUTES = \{(.*?)\n\} as const;", source(VOGT_CLIENT), re.S
    )
    assert block, "the Vogt client's route table was not found"
    entries = re.findall(r'"?([a-z][a-z._]*)"?:\s*"([^"]+)"', block.group(1))
    assert entries, "the route table is empty, so this file proves nothing"
    return dict(entries)


# -- half one: every Vogt path is a registered operation --------------------


def test_every_vogt_path_in_the_pwa_is_a_registered_operation() -> None:
    registry = default_registry()
    registered = {op.name: op for op in registry.for_transport("http")}

    for name, path in client_routes().items():
        assert name in registered, f"the PWA names {name}, which is not an operation"
        assert registered[name].route.path == path, (
            f"{name} is served at {registered[name].route.path}, "
            f"but the PWA asks for {path}"
        )


def test_the_pwa_reaches_vogt_only_through_the_front_door() -> None:
    """One prefix, one place. `/api` at this origin is the engine's own API."""
    text = source(VOGT_CLIENT)
    assert f'export const VOGT_PREFIX = "{FRONT_DOOR_PREFIX}"' in text
    assert len(re.findall(r"\bfetch\(", text)) == 1, (
        "a second call site is how a path escapes the route table, and the "
        "route table is what the check above reads"
    )


def test_every_vogt_write_the_pwa_offers_collects_a_reason() -> None:
    """FR-W1 through the surface most likely to erode it.

    Vogt refuses a write without a reason, so a helper that did not take one
    could only ever fail — but it could fail *at the user*, having already
    opened a form. The requirement is that the view collects one, and the
    smallest checkable proxy is that no exported write can be called without
    it.
    """
    registry = default_registry()
    mutating = {
        name for name, _ in client_routes().items() if registry.get(name).mutating
    }
    assert mutating, "the PWA names no writes; this check would prove nothing"

    text = source(VOGT_CLIENT)
    for match in re.finditer(r'call<[^>]*>\(\s*"([a-z][a-z._]*)"', text):
        name = match.group(1)
        if name not in mutating:
            continue
        # The call site's enclosing declaration must mention a reason.
        start = text.rfind("export const", 0, match.start())
        declaration = text[start : match.end()]
        assert "reason" in declaration, (
            f"{name} is a write the PWA can make without a reason"
        )


def test_the_vogt_client_reaches_no_other_origin() -> None:
    offenders = re.findall(r"""["'(](?:https?:)?//[^"')\s]+""", source(VOGT_CLIENT))
    assert not offenders, f"the Vogt client reaches outside the origin: {offenders}"


# -- half two: every engine path is a route the engine serves ---------------


def engine_routes() -> set[str]:
    declared = re.findall(r'\.route\(\s*"([^"]+)"', ENGINE_APP.read_text("utf-8"))
    assert declared, "no routes found in the engine's router"
    return {normalise(route) for route in declared}


def documented_routes() -> set[str]:
    """Every engine path `API_CONTRACT.md` names, in a code span.

    Prose is not read: a path only counts when the document set it in
    backticks, which is what its route entries do and what a passing mention
    in a sentence does not. Method prefixes are dropped because this compares
    paths — the router half already proves the path exists, and a method the
    document got wrong is not something a path check can see.
    """
    text = ENGINE_CONTRACT_DOC.read_text("utf-8")
    declared = re.findall(
        r"`(?:[A-Z]+ )?(/(?:api|healthz|readyz|mcp|ui-legacy)[^`\s?]*)", text
    )
    assert declared, "no routes found in the engine's API contract document"
    return {normalise(route) for route in declared}


def normalise(path: str) -> str:
    """Reduce a path to the part that can be compared.

    Everything from the first `${` is interpolation — often a call such as
    `${encodeURIComponent(id)}`, which no regexp over a literal will read
    correctly — so the comparison uses the fixed head and the prefix rule
    below resolves the rest. Path parameters collapse to `{}` for the same
    reason, in either notation: the router writes `{id}` and the contract
    document writes `:id`.
    """
    interpolated = path.find("${")
    if interpolated != -1:
        path = path[:interpolated]
    path = re.sub(r"\{[^}]*\}", "{}", path)
    path = re.sub(r"(?<=/):[A-Za-z_][A-Za-z0-9_]*", "{}", path)
    return path.rstrip("/")


def pwa_engine_paths() -> set[str]:
    """Every engine URL literal in the bundle's sources, front door excluded.

    Paths under `/api/vogt` are Vogt operations and belong to the registry
    half above; the engine serves three catch-all routes for that prefix and
    nothing more specific to compare against.
    """
    literals: set[str] = set()
    for path in sorted(WEB_SRC.glob("*.ts")) + sorted(WEB_SRC.glob("*.tsx")):
        literals |= set(
            re.findall(r"""["'`](/api/[A-Za-z0-9/_.$\-{}]*)""", source(path))
        )
    return {
        literal for literal in literals if not literal.startswith(FRONT_DOOR_PREFIX)
    }


def resolves(candidate: str, known: set[str]) -> bool:
    """Does `candidate` name something in `known`?

    A literal is often the head of a URL a template completes
    (`/api/history/${id}/log`), so a known path the literal is a prefix of
    resolves it. A literal that matches nothing in either direction is one
    that is not there.
    """
    return candidate in known or any(path.startswith(candidate) for path in known)


def test_every_engine_path_in_the_pwa_is_a_route_the_engine_serves() -> None:
    routes = engine_routes()
    unresolved = [
        literal
        for literal in sorted(pwa_engine_paths())
        if not resolves(normalise(literal), routes)
    ]
    assert not unresolved, (
        f"the PWA calls {unresolved}, which the engine's router does not serve"
    )


def test_every_engine_path_in_the_pwa_is_in_the_engine_s_api_contract() -> None:
    """The second half of FR-U8, against the document rather than the router.

    The check above is the stronger one and stays: it reads what answers
    requests. This one reads what *describes* what answers requests, which is
    the half that rots — a route added to `app.rs` and never written down
    leaves the document quietly describing a smaller product than the one
    shipped, and every reader of it wrong.

    A path missing from either side is a finding, and they are different
    findings — one is a broken call, the other is a stale document — so the
    failure says which side is short rather than making a reader diff two
    files to find out.
    """
    routes = engine_routes()
    documented = documented_routes()

    disagreements: list[str] = []
    for literal in sorted(pwa_engine_paths()):
        candidate = normalise(literal)
        missing_from = [
            name
            for name, known in (("the router", routes), ("API_CONTRACT.md", documented))
            if not resolves(candidate, known)
        ]
        if missing_from:
            short = " and ".join(missing_from)
            disagreements.append(f"{literal} (missing from {short})")

    assert not disagreements, (
        "the PWA's engine paths do not resolve against both the router and the "
        f"contract document: {disagreements}"
    )


def test_the_two_halves_do_not_share_a_prefix() -> None:
    """The front door's mount must not shadow one of the engine's own routes.

    `/api/vogt` sits in the same namespace as `/api/sessions` and the rest;
    if the engine ever grew a `/api/vogt*` route of its own, the proxy would
    stop being reachable and every Vogt surface would answer with something
    plausible from the wrong half of the product.
    """
    own = {
        route
        for route in engine_routes()
        if route.startswith(FRONT_DOOR_PREFIX) and "{}" not in route
    }
    assert own == {FRONT_DOOR_PREFIX}, (
        f"the engine serves {sorted(own)} under the front door's own prefix"
    )


def test_the_api_prefix_matches_what_the_front_door_strips() -> None:
    """`/api/vogt/backlog` reaches the core's `/api/backlog`, not `/backlog`."""
    assert API_PREFIX == "/api"
    assert f"{API_PREFIX}/vogt" == FRONT_DOOR_PREFIX
