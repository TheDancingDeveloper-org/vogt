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


# -- FR-U9: the legacy GUI leaves when the PWA has caught up ---------------

LEGACY_APP_JS = REPO_ROOT / "src" / "vogt" / "gui" / "static" / "app.js"

#: Operations the vanilla GUI exposes that no PWA surface renders.
#:
#: **Empty as of M11**, which is FR-U9's condition for retiring the legacy
#: GUI. It stays an exact set rather than a `<=` so it fails in both
#: directions: an operation dropping out of the PWA fails here, and so does a
#: view added to the wrong front end.
LEGACY_ONLY: set[str] = set()


def legacy_routes() -> set[str]:
    block = re.search(
        r"const ROUTES = \{(.*?)\n\};", LEGACY_APP_JS.read_text("utf-8"), re.S
    )
    assert block, "the legacy GUI's route table was not found"
    return set(re.findall(r'"?([a-z][a-z._]*)"?:\s*"/api/', block.group(1)))


def surfaces() -> str:
    """Every Vogt surface's source, concatenated.

    The surfaces are the components the shell mounts; `vogtApi.ts` itself is
    excluded deliberately, because a binding nothing calls is the thing this
    check exists to catch.
    """
    names = ("Board", "Backlog", "WorkItemDetail", "Projects", "AuditBrowser")
    return "\n".join(
        source(WEB_SRC / f"{name}.tsx")
        for name in names
        if (WEB_SRC / f"{name}.tsx").is_file()
    )


def rendered_operations() -> set[str]:
    """The operations some surface actually calls.

    Read through the binding names `vogtApi.ts` exports for them, since a
    surface calls `listDrift()`, not `"drift.list"`. The map is written out
    rather than derived: a derivation would have to guess, and a wrong guess
    here silently reports parity.
    """
    bindings = {
        "status": ("status",),
        "project.list": ("listProjects",),
        "project.brief": ("projectBrief",),
        "project.import": ("importProject",),
        "backlog": ("backlog",),
        "bugs": ("bugs",),
        "drift.list": ("listDrift",),
        "drift.resolve": ("resolveDrift",),
        "deps": ("deps",),
        "compliance": ("compliance",),
        "audit.list": ("listAudit",),
        "notifications": ("notifications", "listNotifications"),
    }
    text = surfaces()
    return {
        operation
        for operation, names in bindings.items()
        if any(re.search(rf"\b{name}\s*\(", text) for name in names)
    }


def test_the_pwa_renders_everything_the_legacy_gui_did() -> None:
    gaps = legacy_routes() - rendered_operations()
    assert gaps == LEGACY_ONLY, (
        f"the PWA no longer renders {sorted(gaps)}, which the legacy GUI does. "
        "FR-U9's parity condition is met at M11 and this asserts it stays met."
    )


def test_the_legacy_gui_is_still_here_and_says_why() -> None:
    """Parity of operations is met; parity of *interactions* is not proven.

    FR-U9 permits removing the vanilla GUI once every operation it exposed is
    reachable in the PWA, and the check above says that day has come. It has
    not been removed, deliberately: none of the five Solid surfaces has been
    rendered in a browser — this environment has none — so what is proven is
    that they call the right operations, not that a person can use them.
    Deleting the working front end on that evidence would be trading a
    verified surface for an unverified one.

    This test is the reminder, and removing it is part of the act it is
    waiting for: run the M11 demo in a browser, then delete `src/vogt/gui/`,
    the `/ui-legacy` routes, and both of these tests together.
    """
    assert LEGACY_APP_JS.is_file()
    assert (WEB_SRC / "Board.tsx").is_file()


# -- FR-U16: the palette reaches writes, and never performs one ------------

PALETTE = WEB_SRC / "CommandPalette.tsx"


def test_the_command_palette_never_writes_to_vogt() -> None:
    """FR-U16's second clause, and r6's rule reaching the keyboard.

    The palette may name a mutating verb, but only by opening the view that
    collects its reason. A palette entry cannot type a reason any more than a
    button can — so an entry that called a write directly would be inventing
    one, which is the failure r6 wrote its rule against.

    Checked by import: the writes all live in `vogtApi.ts` and are named, so
    a palette that imports one is a palette that can call it.
    """
    writes = {
        "createWork",
        "updateWork",
        "transitionWork",
        "commentWork",
        "startSession",
        "stopSession",
    }
    text = source(PALETTE)
    imported = set()
    for block in re.findall(r"import \{([^}]*)\} from \"\./vogtApi\";", text):
        imported |= {name.strip().removeprefix("type ") for name in block.split(",")}
    offenders = sorted(imported & writes)
    assert not offenders, (
        f"the command palette imports {offenders}; a palette entry that writes "
        "is one that invented the reason"
    )


def test_the_palette_reaches_every_vogt_surface() -> None:
    """Every read surface, by name, from the keyboard."""
    text = source(PALETTE)
    for opener in (
        "openBoardTab",
        "openBacklogTab",
        "openProjectsTab",
        "openAuditTab",
    ):
        assert opener in text, f"the palette cannot reach {opener}"


# -- FR-U21: every surface has a designed absent state ---------------------

VOGT_SURFACES = ("Board", "Backlog", "WorkItemDetail", "Projects", "AuditBrowser")


def test_every_vogt_surface_distinguishes_an_outage_from_emptiness() -> None:
    """FR-U21, checked at the one place it can be checked without a browser.

    The requirement is about what a person sees, and nothing here renders. So
    this asserts the structural precondition instead: a surface that never
    imports `VogtUnavailable` cannot tell "Vogt could not be asked" from
    "Vogt says there is nothing", and will draw an empty board or an empty
    audit log — which, on these surfaces above all, reads as a claim.

    What it cannot check is whether the resulting copy is any good. That is
    in the M11 demo, and the demo needs a browser.
    """
    for name in VOGT_SURFACES:
        path = WEB_SRC / f"{name}.tsx"
        assert path.is_file(), f"{name}.tsx is missing"
        text = source(path)
        assert "VogtUnavailable" in text, (
            f"{name} cannot distinguish an outage from an empty answer"
        )
        assert re.search(r"\.message\b", text), (
            f"{name} never renders the server's own reason; a client-authored "
            "'something went wrong' is the thing FR-U21 is against"
        )


def test_no_vogt_surface_opens_its_own_door() -> None:
    """One transport, one route table, one place the rule can be kept."""
    for name in VOGT_SURFACES:
        text = source(WEB_SRC / f"{name}.tsx")
        assert not re.search(r"\bfetch\(", text), (
            f"{name} calls fetch directly, so its URL is not in the route "
            "table the registry check reads"
        )


def test_drift_is_resolved_one_proposal_at_a_time() -> None:
    """FR-U18's last clause, and §3's deferral, kept where they erode.

    Bulk accept is deferred *by name*: a drift acceptance is a declared-state
    write carrying its own reason, and making that convenient in bulk is
    exactly how r6's rule stops meaning anything. One call site is the
    smallest checkable form of "one act, one proposal" — a loop over selected
    ids would need a second, or a `.map` around this one.
    """
    text = source(WEB_SRC / "Projects.tsx")
    calls = len(re.findall(r"\bresolveDrift\s*\(", text))
    assert calls == 1, (
        f"drift is resolved from {calls} places; it was written to have one, "
        "and a second is how a bulk accept arrives"
    )
    assert not re.search(r"\bselectAll\b|\bselectedProposals\b", text), (
        "a multi-select over drift proposals is a bulk accept with extra steps"
    )
