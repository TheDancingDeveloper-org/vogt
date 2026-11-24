"""Tests for `scripts/check_engine_spec.py`, and the real spec against the routes."""

from __future__ import annotations

import pytest

from check_engine_spec import (
    APP_RS,
    SPEC,
    check_repository,
    engine_routes,
    problems,
    spec_paths,
)

APP = """
    let api_routes = Router::new()
        .route(
            "/api/sessions",
            get(api::list_sessions).post(api::create_session),
        )
        .route("/api/sessions/{id}/screen", get(api::get_session_screen))
        .route("/api/status", get(api::operational_status))
        .route(secret_broker::FETCH_ROUTE, post(secret_broker::fetch));
    let ws_routes = Router::new().route("/api/sessions/{id}/attach", get(ws::attach));
"""

SPEC_TEXT = """openapi: 3.1.0
info:
  title: t
paths:
  /api/sessions:
    get:
      summary: list
  /api/sessions/{id}/screen:
    get:
      summary: screen
  /api/sessions/{id}/attach:
    get:
      summary: attach
components:
  schemas:
    /not/a/path:
      type: object
"""


def test_routes_are_read_across_lines_and_constants_are_ignored() -> None:
    assert engine_routes(APP) == {
        "/api/sessions",
        "/api/sessions/{id}/screen",
        "/api/status",
        "/api/sessions/{id}/attach",
    }


def test_spec_paths_are_only_the_paths_block() -> None:
    assert spec_paths(SPEC_TEXT) == {
        "/api/sessions",
        "/api/sessions/{id}/screen",
        "/api/sessions/{id}/attach",
    }


def test_a_matching_spec_has_no_problems() -> None:
    # `/api/status` is not a session route, so the spec need not cover it.
    assert problems(APP, SPEC_TEXT) == []


def test_an_undocumented_session_route_is_reported() -> None:
    app = APP + '\n        .route("/api/sessions/{id}/kill", post(api::kill_session))\n'
    assert problems(app, SPEC_TEXT) == [
        "/api/sessions/{id}/kill is an engine route with no path in the spec"
    ]


def test_a_documented_route_the_engine_lacks_is_reported() -> None:
    spec = SPEC_TEXT.replace(
        "components:",
        "  /api/sessions/{id}/gone:\n    get:\n      summary: x\ncomponents:",
    )
    assert problems(APP, spec) == [
        "/api/sessions/{id}/gone is in the spec but is not an engine route"
    ]


def test_a_spec_without_paths_reports_every_session_route() -> None:
    assert len(problems(APP, "openapi: 3.1.0\n")) == 3


@pytest.mark.skipif(
    not (APP_RS.exists() and SPEC.exists()), reason="engine source or spec absent"
)
def test_the_engine_spec_matches_the_engine_routes() -> None:
    found = check_repository()
    assert found == [], "\n".join(found)
