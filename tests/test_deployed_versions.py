"""`deployed.versions` on an instance with no `deploy_lanes` (WI-1014).

An instance that configures no deployment lanes is a stated answer, not a
failure: the read returns an empty lane list, a zero configured count and a
detail that says why nothing is collected. It must not raise, so an overseer
asking which build is live gets the reason rather than an error.
"""

from __future__ import annotations

from vogt.application.context import AppContext
from vogt.application.models import DeployedVersionsParams
from vogt.application.services import deployed_versions


def test_no_lanes_is_a_stated_answer_not_an_error(instance: AppContext) -> None:
    assert instance.config.deploy_lanes == ()

    result = deployed_versions(instance, DeployedVersionsParams())

    assert result.lanes == []
    assert result.configured == 0
    assert result.detail == (
        "deploy_lanes is not configured, so deployed versions are not collected"
    )


def test_no_lanes_stays_a_stated_answer_under_a_filter(instance: AppContext) -> None:
    result = deployed_versions(
        instance, DeployedVersionsParams(project="vogt", lane="dev")
    )

    assert result.lanes == []
    assert result.configured == 0
    assert result.detail is not None
    assert "not configured" in result.detail
