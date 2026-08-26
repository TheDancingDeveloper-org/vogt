"""The REST adapter."""

from __future__ import annotations

from collections.abc import Iterator

import pytest
from fastapi.testclient import TestClient

from vogt.adapters.http.app import API_PREFIX, build_app
from vogt.application.context import AppContext
from vogt.application.services.auth import Authenticated, Unauthenticated


@pytest.fixture
def client(instance: AppContext) -> Iterator[TestClient]:
    with TestClient(build_app(context_factory=lambda: instance)) as test_client:
        yield test_client


def test_status_is_readable(client: TestClient) -> None:
    response = client.get(f"{API_PREFIX}/status")
    assert response.status_code == 200
    assert response.json()["revision"] == 0


 def test_place_metrics_are_one_bounded_read(client: TestClient) -> None:
    response = client.get(f"{API_PREFIX}/place/metrics")
    assert response.status_code == 200
    assert response.json() == {
        "inbox_active": 0,
        "projects_total": 0,
        "work_total": 0,
        "backlog_total_considered": 0,
        "drift_present": False,
        "revision": 0,
        "generated_at": response.json()["generated_at"],
    }

def test_whitelisted_metadata_reads_carry_the_declared_revision_etag(
    client: TestClient,
) -> None:
    response = client.get(f"{API_PREFIX}/projects")
    assert response.status_code == 200
    assert response.headers["etag"] == 'W/"0"'


def test_a_matching_metadata_etag_returns_304_without_a_body(
    client: TestClient,
) -> None:
    first = client.get(f"{API_PREFIX}/projects")
    second = client.get(
        f"{API_PREFIX}/projects",
        headers={"If-None-Match": first.headers["etag"]},
    )
    assert second.status_code == 304
    assert second.headers["etag"] == first.headers["etag"]
    assert second.content == b""


def test_a_declared_write_changes_the_metadata_validator(client: TestClient) -> None:
    before = client.get(f"{API_PREFIX}/projects")
    assert before.headers["etag"] == 'W/"0"'

    created = client.post(
        f"{API_PREFIX}/projects",
        json={
            "name": "Revision change",
            "root_path": "/srv/revision-change",
            "reason": "test the conditional read validator",
        },
    )
    assert created.status_code == 200
    assert "etag" not in created.headers

    after = client.get(
        f"{API_PREFIX}/projects",
        headers={"If-None-Match": before.headers["etag"]},
    )
    assert after.status_code == 200
    assert after.headers["etag"] == 'W/"1"'


def test_non_whitelisted_reads_do_not_emit_or_honor_an_etag(
    client: TestClient,
) -> None:
    response = client.get(f"{API_PREFIX}/status", headers={"If-None-Match": 'W/"0"'})
    assert response.status_code == 200
    assert "etag" not in response.headers
 
 
 def test_authentication_precedes_a_conditional_304(instance: AppContext) -> None:
    def refuse(_request: object) -> tuple[AppContext, Authenticated]:
        raise Unauthenticated("no bearer token presented")

    with TestClient(
        build_app(
            context_factory=lambda: instance,
            authorize_request=refuse,
        )
    ) as client:
        response = client.get(
            f"{API_PREFIX}/projects", headers={"If-None-Match": 'W/"0"'}
        )
    assert response.status_code == 401
     assert "etag" not in response.headers


def test_registering_a_project_returns_it(client: TestClient) -> None:
    response = client.post(
        f"{API_PREFIX}/projects",
        json={
            "name": "Over HTTP",
            "root_path": "/srv/http",
            "reason": "registered over REST",
        },
    )
    assert response.status_code == 200
    assert response.json()["project"]["slug"] == "over-http"

    listed = client.get(f"{API_PREFIX}/projects").json()
    assert listed["total"] == 1


def test_a_missing_reason_is_a_422(client: TestClient) -> None:
    response = client.post(
        f"{API_PREFIX}/projects", json={"name": "X", "root_path": "/srv/x"}
    )
    assert response.status_code == 422
    assert response.json()["error"]["code"] == "invalid_arguments"


def test_a_duplicate_slug_is_a_409(client: TestClient) -> None:
    payload = {"name": "Dupe", "root_path": "/srv/dupe", "reason": "first"}
    assert client.post(f"{API_PREFIX}/projects", json=payload).status_code == 200
    conflict = client.post(f"{API_PREFIX}/projects", json=payload)
    assert conflict.status_code == 409
    assert conflict.json()["error"]["code"] == "conflict"


def test_query_parameters_are_validated(client: TestClient) -> None:
    assert client.get(f"{API_PREFIX}/events", params={"limit": 5}).status_code == 200
    over = client.get(f"{API_PREFIX}/events", params={"limit": 10_000})
    assert over.status_code == 422


def test_an_empty_post_body_is_accepted_when_nothing_is_required(
    client: TestClient,
) -> None:
    """`init` over HTTP is excluded, but the shape must still be handled."""
    response = client.request("POST", f"{API_PREFIX}/projects", content=b"")
    assert response.status_code == 422


def test_local_only_operations_are_absent(client: TestClient) -> None:
    assert client.post(f"{API_PREFIX}/instance/init", json={}).status_code == 404


def test_the_openapi_document_is_generated(client: TestClient) -> None:
    document = client.get("/openapi.json").json()
    assert f"{API_PREFIX}/projects" in document["paths"]
    assert document["info"]["title"] == "Vogt"
