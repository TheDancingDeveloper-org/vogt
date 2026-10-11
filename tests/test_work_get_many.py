"""WI-1224: `work.get_many` reads a rail's worth of chips in one request.

The session rail names one work item per session. It used to ask the core one
`work.list` text search per ref, all at once, which on its own overran the
front door's in-flight cap. These pin the batched read that replaced it: exact
refs, in order, duplicates read once, unknown refs reported rather than
failing the rest, and a bounded batch.
"""

from __future__ import annotations

from collections.abc import Iterator

import pytest
from fastapi.testclient import TestClient
from pydantic import ValidationError

from vogt.adapters.http.app import API_PREFIX, build_app
from vogt.application.context import AppContext
from vogt.application.models import (
    WORK_GET_MANY_LIMIT,
    CreateWorkParams,
    GetManyWorkParams,
    TransitionWorkParams,
)
from vogt.application.services import create_work, get_many_work, transition_work

WHY = "work.get_many test"


def _item(ctx: AppContext, title: str) -> str:
    return create_work(
        ctx, CreateWorkParams(kind="feature", title=title, reason=WHY)
    ).item.ref


def test_rows_come_back_in_the_order_asked_with_unknown_refs_missing(
    instance: AppContext,
) -> None:
    first = _item(instance, "First")
    second = _item(instance, "Second")
    transition_work(
        instance, TransitionWorkParams(ref=second, to_state="in_progress", reason=WHY)
    )

    answer = get_many_work(
        instance, GetManyWorkParams(refs=[second, "WI-404", first, second])
    )

    assert [(row.ref, row.title, row.state) for row in answer.items] == [
        (second, "Second", "in_progress"),
        (first, "First", "open"),
    ]
    assert answer.missing == ["WI-404"]


def test_finished_items_are_read_like_any_other(instance: AppContext) -> None:
    # A chip on a finished item goes muted, so the read must not hide it the
    # way `work.list` does without `include_finished`.
    ref = _item(instance, "Shipped")
    transition_work(
        instance, TransitionWorkParams(ref=ref, to_state="wont_do", reason=WHY)
    )

    answer = get_many_work(instance, GetManyWorkParams(refs=[ref]))

    assert [(row.ref, row.state) for row in answer.items] == [(ref, "wont_do")]


def test_a_batch_is_bounded() -> None:
    GetManyWorkParams(refs=[f"WI-{n}" for n in range(WORK_GET_MANY_LIMIT)])
    with pytest.raises(ValidationError):
        GetManyWorkParams(refs=[f"WI-{n}" for n in range(WORK_GET_MANY_LIMIT + 1)])
    with pytest.raises(ValidationError):
        GetManyWorkParams(refs=[])
    with pytest.raises(ValidationError):
        GetManyWorkParams(refs=[""])


@pytest.fixture
def client(instance: AppContext) -> Iterator[TestClient]:
    with TestClient(build_app(context_factory=lambda: instance)) as test_client:
        yield test_client


def test_the_http_read_takes_repeated_refs(
    instance: AppContext, client: TestClient
) -> None:
    first = _item(instance, "First")
    second = _item(instance, "Second")

    # The PWA's `call()` sends a list as a repeated query parameter.
    response = client.get(
        f"{API_PREFIX}/work/get-many",
        params=[("refs", first), ("refs", "WI-404"), ("refs", second)],
    )

    assert response.status_code == 200
    body = response.json()
    assert [row["ref"] for row in body["items"]] == [first, second]
    assert body["missing"] == ["WI-404"]
