"""The GUI (FR-U1, FR-U2), and the parity rule proved against its source.

The M6 demo is "every M2/M3/M5 demo step repeated through the browser, and
nothing the GUI does is absent from the API". A browser is not available here,
so the demo is split into the two claims it actually makes:

1. **Every view FR-U1 names is reachable and backed by a registered
   operation.** Tested by driving the API the GUI drives, through the same
   application object that serves it.
2. **Nothing the GUI does is absent from the API.** Tested by reading the
   shipped JavaScript and resolving every URL in it against the registry.

The second is the one that matters, and it is deliberately a *source* check
rather than an intent check. Three bugs in this repository so far — the marker
collector matching prose about markers, the deploy test reading its own
explanatory comment, `serve` attributing writes to the OS user — were all
cases where something asserted what was meant instead of what was there. A
view that grows its own endpoint fails here, at the moment it is written.
"""

from __future__ import annotations

import json
import re
from collections.abc import Iterator
from pathlib import Path

import pytest
from fastapi.testclient import TestClient

from vogt.adapters.http.app import API_PREFIX, build_app
from vogt.application.context import AppContext
from vogt.application.models import (
    CreateWorkParams,
    RegisterProjectParams,
    SweepParams,
)
from vogt.application.services import create_work, register_project, sweep
from vogt.gui import GUI_PREFIX, STATIC_ROOT
from vogt.registry import default_registry

WHY = "gui test"

APP_JS = STATIC_ROOT / "app.js"
INDEX_HTML = STATIC_ROOT / "index.html"


def code(path: Path) -> str:
    """An asset's source with its comments removed.

    Every source-reading check below goes through this, because the failure it
    prevents has now happened three times in this repository: a marker
    collector that matched prose *about* markers, a deploy test that read its
    own explanatory comment, and — caught by this very test — a token check
    that found `localStorage` inside the comment saying not to use it. A
    comment that explains a rule will contain the words the rule forbids.

    Deliberately conservative: a `//` only starts a comment when nothing on
    the line quotes before it, which keeps `https://` inside a string intact.
    """
    stripped = re.sub(r"/\*.*?\*/", "", path.read_text(encoding="utf-8"), flags=re.S)
    kept: list[str] = []
    for line in stripped.splitlines():
        head = line.split("//")[0]
        # A `//` after an odd number of quotes is inside a string, not a comment.
        kept.append(line if head.count('"') % 2 or head.count("'") % 2 else head)
    return "\n".join(kept)


@pytest.fixture
def client(instance: AppContext, tmp_path: Path) -> Iterator[TestClient]:
    register_project(
        instance,
        RegisterProjectParams(
            name="Vogt", root_path=str(tmp_path / "vogt"), reason=WHY
        ),
    )
    create_work(
        instance,
        CreateWorkParams(
            kind="bug", title="Sweep drops a page", project="vogt", reason=WHY
        ),
    )
    sweep(instance, SweepParams(reason=WHY))
    with TestClient(build_app(context_factory=lambda: instance)) as test_client:
        yield test_client


# -- the assets are served, from the one port (FR-U1) ----------------------


def test_the_gui_is_served_from_the_same_port_as_the_api(client: TestClient) -> None:
    page = client.get(f"{GUI_PREFIX}/")
    assert page.status_code == 200
    assert "<title>Vogt</title>" in page.text
    assert client.get(f"{API_PREFIX}/status").status_code == 200


def test_the_root_sends_a_browser_to_the_gui(client: TestClient) -> None:
    landing = client.get("/", follow_redirects=False)
    assert landing.status_code in (302, 307)
    assert landing.headers["location"] == f"{GUI_PREFIX}/"


def test_the_assets_load(client: TestClient) -> None:
    for asset in ("app.js", "style.css"):
        response = client.get(f"{GUI_PREFIX}/{asset}")
        assert response.status_code == 200, asset
        assert response.content, asset


def test_the_gui_is_not_in_the_openapi_document(client: TestClient) -> None:
    """The schema describes operations. Static files are not operations."""
    paths = client.get("/openapi.json").json()["paths"]
    assert "/" not in paths
    assert not [path for path in paths if path.startswith(GUI_PREFIX)]
    assert all(path.startswith(API_PREFIX) for path in paths)


def test_the_api_still_serves_when_the_gui_is_absent(instance: AppContext) -> None:
    """The API must not depend on the surface built on top of it."""
    with TestClient(build_app(context_factory=lambda: instance, gui=False)) as bare:
        assert bare.get(f"{API_PREFIX}/status").status_code == 200
        assert bare.get(f"{GUI_PREFIX}/").status_code == 404


# -- nothing the GUI does is absent from the API --------------------------


def registered_paths() -> set[str]:
    return {
        f"{API_PREFIX}{operation.route.path}"
        for operation in default_registry().for_transport("http")
    }


#: The one `/api/...` literal in the GUI that is not an operation.
#:
#: It is where the merged product's front door mounts this core (NFR-D11): the
#: engine publishes the only port and proxies `/api/vogt` to `/api` here, so a
#: bundle served at `/ui-legacy` asks for `/api/vogt/backlog` and the core
#: still sees `/api/backlog`. Named here rather than pattern-matched, because
#: the exemption has to be as narrow as the thing it excuses — a second
#: unregistered path still fails.
FRONT_DOOR_MOUNT = "/api/vogt"


def test_every_url_in_the_gui_resolves_to_a_registered_operation() -> None:
    """Read the shipped source; resolve what it actually asks for."""
    source = code(APP_JS)
    referenced = set(re.findall(r'"(/api/[A-Za-z0-9/_.-]*)"', source))
    assert referenced, "the parity check found no API paths, so it proves nothing"

    unknown = referenced - registered_paths() - {FRONT_DOOR_MOUNT}
    assert not unknown, (
        f"the GUI calls {sorted(unknown)}, which no registered operation serves"
    )


def test_the_front_door_mount_is_only_ever_a_prefix() -> None:
    """The same bundle serves two mounts, and neither is configured.

    Served by the core at `/ui`, this GUI asks `/api`. Served through the
    engine at `/ui-legacy`, it asks `/api/vogt` and the engine strips the
    difference back off (NFR-D11, FR-U9). What makes that safe is that the
    route table never learns about it: the prefix is chosen from
    `window.location`, applied once in `call()`, and every route stays a
    registry path the test above can resolve.
    """
    source = code(APP_JS)
    assert 'const FRONT_DOOR_GUI = "/ui-legacy"' in source
    assert f'const FRONT_DOOR_API = "{FRONT_DOOR_MOUNT}"' in source
    assert "window.location.pathname.startsWith(FRONT_DOOR_GUI)" in source, (
        "the prefix must be derived from where the page was served — a "
        "configured one is a setting the wrong deployment can be given"
    )
    assert "API_ROOT + path.slice(API_BASE.length)" in source, (
        "call() is the one place the mount point is applied"
    )
    assert len(re.findall(re.escape(FRONT_DOOR_MOUNT), source)) == 1, (
        "the front-door mount appears once, as a constant; a second use is a "
        "route that would not resolve against the registry"
    )


def test_the_gui_reaches_nothing_but_the_api() -> None:
    """No absolute URLs, no second origin, no CDN — in JS, HTML or CSS.

    This is a deployment constraint as much as an architectural one: Vogt runs
    on a tailnet, often with no route off it, and a page that fetches from
    elsewhere renders differently depending on whether the network happens to
    be reachable.
    """
    for asset in sorted(STATIC_ROOT.iterdir()):
        offenders = re.findall(r"""["'(](?:https?:)?//[^"')\s]+""", code(asset))
        assert not offenders, f"{asset.name} reaches outside the origin: {offenders}"


def test_the_gui_has_one_way_to_reach_the_server() -> None:
    """Exactly one `fetch(`, inside `call()`.

    A second call site is how an endpoint escapes the route table, and the
    route table is what the parity check above reads.
    """
    source = code(APP_JS)
    assert len(re.findall(r"\bfetch\(", source)) == 1
    others = re.findall(r"\bXMLHttpRequest\b|\bEventSource\b|\bWebSocket\b", source)
    assert not others, f"another transport crept in: {others}"


#: The mutating operations the GUI is permitted to name (r6, FR-U3).
#:
#: The M6 rule was "every operation the GUI names is a read", justified by
#: FR-W1: a write needs a reason its author typed, and a button cannot type
#: one. That justification does not extend to a *form*, which is why import
#: is here and drift resolution still is not — resolving drift from a list is
#: a button, and "accepted via GUI" is not a reason.
#:
#: Anything added to this set must collect a reason the user typed. The test
#: below checks that rather than trusting the comment.
GUI_WRITES = {"project.import"}


def test_the_gui_adds_no_capability_of_its_own(client: TestClient) -> None:
    """Every operation the GUI names is a read, or an audited form."""
    source = code(APP_JS)
    named = set(re.findall(r'"?([a-z]+(?:\.[a-z_]+)?)"?:\s*"/api/', source))
    by_name = {operation.name: operation for operation in default_registry()}

    assert named, "no operations found in the route table"
    for name in sorted(named):
        assert name in by_name, f"{name} is not a registered operation"
        if by_name[name].mutating:
            assert name in GUI_WRITES, f"the GUI names the mutating op {name}"


def test_every_gui_write_collects_a_reason_the_user_typed() -> None:
    """FR-W1, kept honest at the one place the GUI writes.

    A form that defaults, hides or generates its reason is the same failure
    as a button: the audit row then records that somebody clicked, which is
    the thing the reason field exists to prevent.
    """
    source = code(APP_JS)
    assert sorted(GUI_WRITES) == ["project.import"], (
        "a new GUI write needs its own reason assertion here"
    )
    match = re.search(r"async function importView\(.*?\n}\n", source, re.DOTALL)
    assert match, "importView not found"
    body = match.group(0)
    assert 'name: "reason", required: "required"' in body, (
        "the import form must require a reason (FR-W1)"
    )
    assert "reason: reason.value.trim()" in body, (
        "the import form must send the reason the user typed, not one of its own"
    )


def test_the_gui_offers_no_repository_listing(client: TestClient) -> None:
    """FR-G15 through the door it is most likely to come back in.

    An import form with a text field is one call away from an import form
    with a dropdown of your repositories, and that dropdown is the
    registration-candidate listing r3 removed. There is no operation that
    could serve it, and this asserts the GUI has not grown one.
    """
    source = code(APP_JS)
    for banned in ("/user/repos", "/search/repositories", "/orgs/"):
        assert banned not in source, (
            f"the GUI reaches for a repository listing: {banned}"
        )


# -- trust and freshness on every aggregate (FR-U2) ------------------------


AGGREGATES = ["project.brief", "backlog", "bugs", "drift.list", "deps"]


@pytest.mark.parametrize("operation", AGGREGATES)
def test_every_aggregate_the_gui_shows_carries_freshness(
    client: TestClient, operation: str
) -> None:
    """FR-U2, enforced in the API rather than synthesised in the browser.

    If the GUI computed this itself it would be doing something the API
    cannot, which is the parity rule broken from the other direction.
    """
    route = {op.name: op.route.path for op in default_registry()}[operation]
    params = {"slug": "vogt"} if operation == "project.brief" else {}
    if operation == "deps":
        params = {"project": "vogt"}

    body = client.get(f"{API_PREFIX}{route}", params=params).json()
    assert "freshness" in body, f"{operation} carries no freshness"
    assert body["freshness"]["status"] in ("fresh", "partial", "never_swept")


def test_ranked_items_carry_a_trust_state(client: TestClient) -> None:
    items = client.get(f"{API_PREFIX}/backlog").json()["items"]
    assert items, "the fixture created work, so the backlog is not empty"
    assert all(item["trust_state"] for item in items)


def test_the_gui_renders_freshness_on_every_aggregating_view() -> None:
    """Having it in the payload is not the same as showing it.

    Each view function that reads an aggregate must pass its freshness to the
    shared renderer. Checked in the source because the alternative is a
    browser, and the failure being guarded against — somebody adds a view and
    forgets the line — is exactly the one a source check catches.
    """
    source = code(APP_JS)
    for view in ("projectView", "rankedView", "driftView", "depsView", "inboxView"):
        match = re.search(rf"async function {view}\(.*?\n}}\n", source, re.DOTALL)
        assert match, f"{view} not found"
        assert "freshness(" in match.group(0), (
            f"{view} renders an aggregate without showing how old it is (FR-U2)"
        )


def test_the_trust_renderer_never_shows_a_blank(client: TestClient) -> None:
    """An unknown trust state reads as `unverified`, never as nothing.

    A blank cell says "no opinion"; the honest answer is "nobody has verified
    this", which is a different and more useful thing to know (FR-D1).
    """
    del client
    source = code(APP_JS)
    match = re.search(r"function trust\(state\) \{.*?\n}\n", source, re.DOTALL)
    assert match and 'state || "unverified"' in match.group(0)


# -- the token never leaves the header (FR-S7) ----------------------------


def test_the_gui_sends_its_token_as_a_header_and_never_in_a_url() -> None:
    source = code(APP_JS)
    assert "headers.authorization = `Bearer ${token}`" in source
    assert 'searchParams.append("token"' not in source
    assert re.search(r"searchParams\.append\([^)]*token", source) is None


def test_the_token_does_not_outlive_the_tab() -> None:
    """`sessionStorage`, not `localStorage`.

    A credential that survives a browser restart by default is one somebody
    forgets they granted.
    """
    source = code(APP_JS)
    assert "localStorage" not in source
    assert "sessionStorage" in source


def test_static_assets_do_not_require_a_token(instance: AppContext) -> None:
    """Otherwise there is no page on which to enter one.

    The assets carry no instance data — every answer arrives from `/api`,
    which is authenticated — so serving them openly reveals nothing.
    """

    def refuse(request: object) -> tuple[AppContext, object]:
        raise AssertionError("static files must not reach the authenticator")

    app = build_app(
        context_factory=lambda: instance,
        authorize_request=refuse,  # type: ignore[arg-type]
    )
    with TestClient(app) as guarded:
        assert guarded.get(f"{GUI_PREFIX}/").status_code == 200
        assert guarded.get(f"{GUI_PREFIX}/app.js").status_code == 200


# -- the assets are what they claim to be ---------------------------------


def test_the_gui_needs_no_build_step() -> None:
    """No bundler output, no `node_modules`, no committed build artefact.

    `DESIGN.md` §10 said "React SPA"; this is plain ES modules. The reason is
    packaging: Vogt installs as a wheel, and a framework build means either a
    Node toolchain at wheel-build time or generated files in version control
    that nothing verifies. This test is what keeps the deviation honest — if
    a build step ever appears, it fails and somebody has to argue for it.
    """
    assert not (STATIC_ROOT.parent / "node_modules").exists()
    assert not list(STATIC_ROOT.rglob("*.min.js"))
    assert {asset.name for asset in STATIC_ROOT.iterdir()} == {
        "index.html",
        "app.js",
        "style.css",
    }
    assert 'type="module"' in INDEX_HTML.read_text(encoding="utf-8")


def test_the_route_table_is_valid_json_shaped() -> None:
    """The route table is data, so a typo in it is findable.

    Extracted and parsed rather than eyeballed — the table is the single
    thing every other check in this file depends on being real.
    """
    source = code(APP_JS)
    match = re.search(r"const ROUTES = \{(.*?)\n\};", source, re.DOTALL)
    assert match
    entries = re.findall(r'"?([a-z][a-z.]*)"?:\s*"([^"]+)"', match.group(1))
    assert len(entries) >= 8
    table = dict(entries)
    assert json.loads(json.dumps(table)) == table
    assert set(table.values()) <= registered_paths()


# -- the fields it renders are fields that exist ---------------------------


def test_every_field_the_gui_reads_off_a_result_exists() -> None:
    """Three columns were em dashes on every row, in every estate.

    `depsTable` read `ecosystem` and `constraint`, which r2 removed from the
    product when it removed lockfiles and resolved versions (FR-D1), and
    `from_slug`/`to_slug`, which were never the field names. `driftView` read
    `subject_key` and `raised_at` for the same kind of reason. Nothing failed:
    the GUI renders a missing field as `—`, which is also how it renders "not
    collected" — so a typo and an honest absence looked identical, and the
    typo won for months.

    Checked against the models rather than by eye, over the accessors that
    read a *result object*. The loop variables are named after what they hold
    (`proposal`, `ref`, `record`), which is what makes this checkable at all.
    """
    from pydantic import BaseModel

    from vogt.application.models import RankedItem
    from vogt.core import entities

    source_text = code(APP_JS)
    # `item` holds either shape depending on the view — a ranked entry in the
    # backlog and bugs tables, a work item elsewhere — so it is checked
    # against both. The others hold one thing each.
    subjects: dict[str, tuple[type[BaseModel], ...]] = {
        "proposal": (entities.DriftProposal,),
        "ref": (entities.DepRef,),
        "record": (entities.AuditRecord,),
        "item": (RankedItem, entities.WorkItem),
    }
    unknown: list[str] = []
    for name, candidates in subjects.items():
        fields = {field for model in candidates for field in model.model_fields}
        for attribute in set(re.findall(rf"\b{name}\.([a-z_]+)\b", source_text)):
            if attribute not in fields:
                unknown.append(f"{name}.{attribute}")

    assert not unknown, (
        f"the GUI reads {sorted(unknown)}, which no model carries — every one "
        "of those renders as an em dash, indistinguishable from 'not collected'"
    )
