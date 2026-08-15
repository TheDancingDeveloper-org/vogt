"""Readiness gates on migration, and `migrate` is a verb (NFR-I3, FR-L1).

The failure these exist to stop is specific and was open from M4 to r13. The
deployed topology runs `command: serve` and never runs `init`, `serve` did not
migrate, and `/health/ready` reported the *applied* schema version without
comparing it to anything. So an image carrying a new migration started against
the old schema, passed its healthcheck, was routed traffic, and failed later on
a missing table — as a SQL error at whatever touched it first, at a moment
unrelated to the deploy.

Every part of that is a separate hole and each is tested here: the server must
migrate, the probe must compare, and an operator must have a verb to reach for
that is not `init` (a word that reads like "start over" on a live data
directory).
"""

from __future__ import annotations

from pathlib import Path

import pytest
from fastapi.testclient import TestClient

from vogt.adapters.http.server import ServeOptions, build_server
from vogt.application.context import AppContext
from vogt.application.models import MigrateParams
from vogt.application.services import migrate_instance
from vogt.config import VogtConfig
from vogt.errors import InvalidRequest
from vogt.storage.sqlite.declared import SqliteDeclaredStore
from vogt.storage.sqlite.observed import SqliteObservedStore


def _serve(config: VogtConfig) -> TestClient:
    options = ServeOptions(host="127.0.0.1", port=18099, require_auth=False)
    return TestClient(build_server(options, config=config))


# -- the verb (FR-L1) -------------------------------------------------------


def test_migrate_reports_where_both_stores_landed(instance: AppContext) -> None:
    """The operator's question is "is this current?", not "what ran?".

    So the result carries the expected version beside the applied one. A
    result reporting only what it applied cannot answer it: applying nothing
    is what both a current instance and a broken one look like.
    """
    result = migrate_instance(instance, MigrateParams())

    assert result.declared_schema_version == result.declared_schema_expected
    assert result.observed_schema_version == result.observed_schema_expected
    assert result.declared_schema_version > 0
    assert result.observed_schema_version > 0
    # Already migrated by `init`, so this run had nothing to do and says so
    # rather than reporting the whole history as though it had just run.
    assert result.migrations_applied == []


def test_migrate_is_idempotent(instance: AppContext) -> None:
    """Safe to run twice, because a deploy runbook will."""
    first = migrate_instance(instance, MigrateParams())
    second = migrate_instance(instance, MigrateParams())
    assert second.declared_schema_version == first.declared_schema_version
    assert second.migrations_applied == []


def test_migrate_refuses_a_data_directory_with_no_instance(
    context: AppContext,
) -> None:
    """`migrate` does not conjure an estate.

    `init` creates and `migrate` moves forward. Collapsing them would make
    the more destructive-sounding verb the safe one and leave `migrate` able
    to silently produce an instance somebody then writes to — the expensive
    surprise, since the mistake it covers for is a mistyped data directory.
    """
    with pytest.raises(InvalidRequest) as caught:
        migrate_instance(context, MigrateParams())

    assert "vogt init" in str(caught.value)


def test_migrate_is_reachable_as_a_cli_verb() -> None:
    """FR-L1 names `migrate`. The gap was never the capability."""
    from vogt.registry import LOCAL_ONLY, default_registry

    operation = default_registry().get("migrate")
    assert operation.cli is not None
    assert operation.cli.path == ("migrate",)
    # Local-only for the same reason `init` is: it acts on the data directory
    # this process can see. The registry's staleness check reads this list, so
    # the exclusion has to be stated rather than inferred.
    assert "migrate" in LOCAL_ONLY


# -- the gate (NFR-I3) ------------------------------------------------------


def test_a_current_instance_reports_expected_equal_to_applied(
    instance: AppContext, config: VogtConfig
) -> None:
    with _serve(config) as client:
        body = client.get("/health/ready").json()

    assert body["status"] == "ready"
    assert body["declared_schema_version"] == body["declared_schema_expected"] > 0
    assert body["observed_schema_version"] == body["observed_schema_expected"] > 0


@pytest.mark.parametrize("store", ["declared", "observed"])
def test_readiness_is_red_when_a_store_is_behind_this_build(
    instance: AppContext,
    config: VogtConfig,
    monkeypatch: pytest.MonkeyPatch,
    store: str,
) -> None:
    """The whole of NFR-I3, one store at a time.

    Simulated by moving the *expected* version rather than by rolling a
    database back, because that is the direction the real failure arrives
    from: the database is untouched and the image is new. Both stores are
    parametrized because a check that only read `declared` would pass a
    deploy whose observed migration had not run.
    """
    target = SqliteDeclaredStore if store == "declared" else SqliteObservedStore
    monkeypatch.setattr(
        target,
        "bundled_schema_version",
        lambda self: self.schema_version() + 1,
    )

    with _serve(config) as client:
        response = client.get("/health/ready")

    assert response.status_code == 503
    body = response.json()
    assert body["status"] == "not_ready"
    assert body[f"{store}_schema_expected"] == body[f"{store}_schema_version"] + 1


def test_a_red_probe_names_the_store_the_numbers_and_the_fix(
    instance: AppContext, config: VogtConfig, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A bare `not_ready` costs whoever holds the pager twenty minutes.

    The detail is asserted rather than merely present because the reason this
    requirement was open so long is that nothing said what was wrong: a probe
    reporting a number with nothing to compare it against reads as healthy.
    """
    monkeypatch.setattr(
        SqliteDeclaredStore,
        "bundled_schema_version",
        lambda self: self.schema_version() + 2,
    )

    with _serve(config) as client:
        detail = client.get("/health/ready").json()["detail"]

    assert "declared" in detail
    assert "this build expects" in detail
    assert "vogt migrate" in detail


def test_readiness_stays_green_when_the_database_is_ahead(
    instance: AppContext, config: VogtConfig, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A store ahead of the build is not this probe's finding to report.

    `migrate` refuses it with the forward-only message, which names the
    migration and says to restore a backup or deploy the newer build. A probe
    that also went red here would send an operator to the healthcheck for a
    diagnosis only the migrator can give — and would make a rollback, which is
    a deliberate act, look like a broken container.
    """
    monkeypatch.setattr(
        SqliteDeclaredStore,
        "bundled_schema_version",
        lambda self: self.schema_version() - 1,
    )

    with _serve(config) as client:
        assert client.get("/health/ready").status_code == 200


# -- the server migrates, rather than reporting on migration ----------------


def test_building_the_server_migrates_the_stores(tmp_path: Path) -> None:
    """`serve` is the only entrypoint the deployed stack runs.

    Asserted against a data directory nothing has initialised, so the schema
    can only be non-zero if assembling the server applied it. Deleting the two
    `migrate` calls in `build_server` turns this red — which is the point, as
    a fix that only production exercises is one nothing catches regressing.
    """
    config = VogtConfig(data_dir=tmp_path / "fresh", sqlite_synchronous="off")

    from vogt.application.context import build_context

    before = build_context(config=config)
    assert before.declared.schema_version() == 0

    with _serve(config):
        pass

    after = build_context(config=config)
    assert after.declared.schema_version() == after.declared.bundled_schema_version()
    assert after.observed.schema_version() == after.observed.bundled_schema_version()


def test_the_expected_version_is_read_from_the_migrations_that_shipped(
    config: VogtConfig,
) -> None:
    """Not a constant somebody has to remember to bump.

    A hand-maintained "current schema version" is a second place to record
    the same fact, and the failure mode is silent: the number says the build
    expects less than it ships, and the gate above passes a container that is
    genuinely behind.
    """
    from vogt.application.context import build_context
    from vogt.storage.sqlite import declared as declared_module

    ctx = build_context(config=config)
    on_disk = sorted(declared_module.MIGRATIONS_DIR.glob("*.sql"))
    assert on_disk, "no migrations shipped, so this test proves nothing"

    highest = max(int(path.stem.split("_", 1)[0]) for path in on_disk)
    assert ctx.declared.bundled_schema_version() == highest
