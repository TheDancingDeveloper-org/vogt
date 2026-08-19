"""A request that leaves a trace (NFR-OB1–OB4, #139).

The gap these close is specific. On 2026-08-19 `vogt-dev` served the GUI at
15+ seconds and `/health/ready` intermittently past 25, and the logs could
not say which endpoint was slow — uvicorn's stock line carries no timestamp,
no duration, and a client port of `:0`. The cause was found by counting path
frequencies across three thousand lines by hand, which only worked because
the offending requests were numerous.

So these tests assert on the *rendered output*, not on log-record attributes:
the correlation id and the actor arrive through a filter on the handler, and
a test that read `record.request_id` would pass while the line an operator
reads carried neither.
"""

from __future__ import annotations

import io
import json
import logging
from collections.abc import Iterator
from typing import Any

import pytest
from fastapi.testclient import TestClient

from vogt.adapters.http.server import ServeOptions, build_server
from vogt.application.context import AppContext
from vogt.application.models import CreateActorParams, IssueTokenParams
from vogt.application.services import create_actor, issue_token
from vogt.config import VogtConfig
from vogt.observability import configure_logging

WHY = "logging test"


@pytest.fixture
def log_stream() -> Iterator[io.StringIO]:
    """Vogt's real logging pipeline, pointed at a buffer.

    Restores whatever the suite had before, so a test that configures logging
    cannot decide how the rest of the run reports itself.
    """
    root = logging.getLogger()
    before_handlers = list(root.handlers)
    before_level = root.level
    vogt_logger = logging.getLogger("vogt")
    before_vogt_level = vogt_logger.level
    buffer = io.StringIO()
    yield buffer
    root.handlers = before_handlers
    root.setLevel(before_level)
    vogt_logger.setLevel(before_vogt_level)


def serve(instance: AppContext, buffer: io.StringIO, **settings: Any) -> TestClient:
    config = instance.config.model_copy(update=settings)
    configure_logging(level=config.log_level, fmt=config.log_format, stream=buffer)
    options = ServeOptions(
        host="127.0.0.1", port=18099, require_auth=settings.pop("require_auth", False)
    )
    return TestClient(build_server(options, config=config))


def lines(buffer: io.StringIO) -> list[str]:
    return [line for line in buffer.getvalue().splitlines() if line.strip()]


def access_lines(buffer: io.StringIO) -> list[str]:
    return [
        line for line in lines(buffer) if "vogt.http" in line or '"vogt.http"' in line
    ]


# -- one line per request, with a duration on it (NFR-OB1) -----------------


def test_a_served_request_produces_one_line_carrying_its_duration(
    instance: AppContext, log_stream: io.StringIO
) -> None:
    with serve(instance, log_stream) as client:
        assert client.get("/api/projects", params={"limit": 1}).status_code == 200

    written = access_lines(log_stream)
    assert len(written) == 1, written
    line = written[0]
    assert "method=GET" in line
    assert "path=/api/projects" in line
    assert "status=200" in line
    assert "query=limit=1" in line  # the evidence #138 was diagnosed from
    assert "duration_ms=" in line
    assert "ttfb_ms=" in line
    assert "request_id=" in line


def test_the_access_log_can_be_turned_off_without_losing_correlation(
    instance: AppContext, log_stream: io.StringIO
) -> None:
    """An operator who silences the log still has to be able to follow a request."""
    with serve(instance, log_stream, log_requests=False) as client:
        response = client.get("/api/projects", params={"limit": 1})

    assert access_lines(log_stream) == []
    assert response.headers["x-request-id"]


# -- one identifier, across two runtimes (NFR-OB3) -------------------------


def test_the_front_doors_request_id_is_adopted_and_echoed(
    instance: AppContext, log_stream: io.StringIO
) -> None:
    with serve(instance, log_stream) as client:
        response = client.get(
            "/api/projects",
            params={"limit": 1},
            headers={"X-Request-Id": "engine-abc123"},
        )

    assert response.headers["x-request-id"] == "engine-abc123"
    assert "request_id=engine-abc123" in "\n".join(lines(log_stream))


def test_an_unusable_request_id_is_replaced_rather_than_logged(
    instance: AppContext, log_stream: io.StringIO
) -> None:
    """A correlation id ends up in a log line, so it is constrained, not trusted."""
    forged = "abc\ndef INFO  vogt.http request status=200"
    with serve(instance, log_stream) as client:
        response = client.get(
            "/api/projects", params={"limit": 1}, headers={"X-Request-Id": forged}
        )

    assert response.headers["x-request-id"] != forged
    assert "\ndef" not in log_stream.getvalue()


# -- the pathological request surfaces itself (NFR-OB2) --------------------


def test_a_slow_request_is_logged_at_warning(
    instance: AppContext, log_stream: io.StringIO
) -> None:
    with serve(instance, log_stream, log_slow_request_ms=0) as client:
        client.get("/health/live")

    assert any("WARNING" in line for line in lines(log_stream))


def test_a_probe_is_quiet_until_it_is_slow(
    instance: AppContext, log_stream: io.StringIO
) -> None:
    """`/health/ready` at 25 seconds is the line worth keeping (NFR-OB4)."""
    with serve(instance, log_stream, log_level="info") as client:
        client.get("/health/live")
    assert access_lines(log_stream) == []

    with serve(instance, log_stream, log_level="info", log_slow_request_ms=0) as client:
        client.get("/health/live")
    assert len(access_lines(log_stream)) == 1


def test_a_failing_route_says_so_at_warning(
    instance: AppContext, log_stream: io.StringIO
) -> None:
    with serve(instance, log_stream) as client:
        assert client.get("/api/work/WI-nope").status_code == 404

    written = access_lines(log_stream)
    assert len(written) == 1
    assert "status=404" in written[0]


# -- who, not only where ---------------------------------------------------


def test_an_authenticated_request_names_the_actor(
    instance: AppContext, log_stream: io.StringIO
) -> None:
    """ "Which client is hammering this endpoint" needs an actor, not an address."""
    create_actor(
        instance,
        CreateActorParams(
            identity_ref="agent:noisy",
            kind="agent",
            display_name="Noisy",
            reason=WHY,
        ),
    )
    secret = issue_token(
        instance,
        IssueTokenParams(actor="agent:noisy", name="t", scopes="read", reason=WHY),
    ).secret

    with serve(instance, log_stream, require_auth=True) as client:
        response = client.get(
            "/api/projects",
            params={"limit": 1},
            headers={"authorization": f"Bearer {secret}"},
        )
    assert response.status_code == 200
    assert "actor=agent:noisy" in access_lines(log_stream)[0]


# -- the same fields, queryable (NFR-OB5) ----------------------------------


def test_json_output_is_one_object_per_line_with_the_same_fields(
    instance: AppContext, log_stream: io.StringIO
) -> None:
    with serve(instance, log_stream, log_format="json") as client:
        client.get("/api/projects", params={"limit": 1})

    payload = json.loads(access_lines(log_stream)[0])
    assert payload["logger"] == "vogt.http"
    assert payload["level"] == "info"
    assert payload["method"] == "GET"
    assert payload["path"] == "/api/projects"
    assert payload["status"] == 200
    assert isinstance(payload["duration_ms"], float)
    assert payload["request_id"]
    assert payload["ts"].endswith("+00:00")


def test_a_bare_config_is_a_text_log_at_info(tmp_path: Any) -> None:
    """The defaults are the deployed shape, so they are asserted as such."""
    config = VogtConfig(data_dir=tmp_path)
    assert config.log_format == "text"
    assert config.log_requests is True
    assert config.log_slow_request_ms == 1000
    assert "/health/ready" in config.log_quiet_paths
