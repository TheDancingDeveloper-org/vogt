"""The `deploy-lanes` collector: which revision each configured lane runs.

"Is dev at head?" had no single answer source. Each lane in
`deploy_lanes` names where its evidence lives — a receipt a deploy
pipeline commits to a forge repository, and/or the running instance's own
public version endpoint — and this collector reads both, then asks the
project's forge how the deployed revision compares with the branch the lane
tracks. The result is one observation per lane (`deploy.lane`), which
`deployed_versions` reads and the Inbox alerts on when a receipt says the
deploy or its smoke test failed.

Configuration only: no hostname, repository or path is known here. A lane
whose source cannot be read records *why* in its observation (`*_detail`)
rather than failing the sweep, so "not readable" never reads as "not
deployed".
"""

from __future__ import annotations

import json
import re
import urllib.error
import urllib.request
from collections.abc import Iterable

from vogt.adapters.forge.kinds import COLLECTOR_DEPLOY_LANES, KIND_DEPLOY_LANE
from vogt.adapters.forge.registry import provider_for, unsupported_reason
from vogt.adapters.github.client import Transport
from vogt.collectors.base import CollectorContext, Finding, finding
from vogt.config import DeployLane
from vogt.core.entities import Project
from vogt.errors import VogtError

#: How many unpromoted commits a lane observation carries. The count is
#: always exact (`ahead_by`); the list is for reading, and an estate that is
#: hundreds of commits behind has a bigger problem than a truncated list.
MAX_COMMITS = 100
VERSION_TIMEOUT_SECONDS = 10
_SHA = re.compile(r"^[0-9a-f]{7,40}$")
_PASSED = frozenset({"passed", "success", "succeeded", "ok", "green"})
_FAILED = frozenset({"failed", "failure", "error", "errored", "red"})


class DeployLanesCollector:
    """One observation per configured deployment lane of a project."""

    name = COLLECTOR_DEPLOY_LANES

    def __init__(self, *, transport: Transport | None = None) -> None:
        self._transport = transport

    @property
    def requires_network(self) -> bool:
        return True

    def collect(self, ctx: CollectorContext, project: Project) -> Iterable[Finding]:
        for lane in ctx.config.deploy_lanes:
            if lane.project != project.slug:
                continue
            yield finding(
                kind=KIND_DEPLOY_LANE,
                subject_key=f"deploy:{project.slug}/{lane.name}",
                project=project,
                source_url=lane.version_url,
                payload=self._observe(ctx, project, lane),
            )

    def _observe(
        self, ctx: CollectorContext, project: Project, lane: DeployLane
    ) -> dict[str, object]:
        receipt, receipt_detail = self._receipt(ctx, lane)
        live, live_detail = self._live(lane)
        deployed_sha: str | None = None
        deployed_from: str | None = None
        if live is not None and _is_sha(live.get("source_sha")):
            deployed_sha, deployed_from = str(live["source_sha"]), "live"
        elif receipt is not None and _is_sha(receipt.get("source_sha")):
            deployed_sha, deployed_from = str(receipt["source_sha"]), "receipt"
        compare: dict[str, object] | None = None
        compare_detail: str | None = None
        if deployed_sha is not None:
            compare, compare_detail = self._compare(ctx, project, lane, deployed_sha)
        return {
            "lane": lane.name,
            "project": project.slug,
            "branch": lane.branch,
            "receipt": receipt,
            "receipt_detail": receipt_detail,
            "live": live,
            "live_detail": live_detail,
            "deployed_sha": deployed_sha,
            "deployed_from": deployed_from,
            "compare": compare,
            "compare_detail": compare_detail,
        }

    def _receipt(
        self, ctx: CollectorContext, lane: DeployLane
    ) -> tuple[dict[str, object] | None, str | None]:
        if lane.receipt_repo is None or lane.receipt_path is None:
            return None, None
        provider = provider_for(
            lane.receipt_repo, ctx.config, transport=self._transport
        )
        ref = None if provider is None else provider.parse(lane.receipt_repo)
        if provider is None or ref is None:
            return None, unsupported_reason(lane.receipt_repo, ctx.config)
        try:
            raw = provider.read_file(ref, lane.receipt_path)
        except VogtError as exc:
            return None, f"receipt unreadable: {exc}"
        if raw is None:
            return None, f"no receipt at {lane.receipt_path}"
        parsed = _json_object(raw)
        if parsed is None:
            return None, f"receipt at {lane.receipt_path} is not a JSON object"
        return _receipt_fields(parsed), None

    def _live(self, lane: DeployLane) -> tuple[dict[str, object] | None, str | None]:
        if lane.version_url is None:
            return None, None
        try:
            status, body = self._get(lane.version_url)
        except (urllib.error.URLError, TimeoutError, OSError) as exc:
            return None, f"version endpoint unreachable: {exc}"
        if status >= 400:
            return None, f"version endpoint answered {status}"
        parsed = _json_object(body)
        if parsed is None:
            return None, "version endpoint did not answer a JSON object"
        sha = _first_text(parsed, "source_sha", "sha", "revision", "commit")
        version = _first_text(parsed, "product_version", "version")
        return {
            "source_sha": sha,
            "version": version,
            "source_ref": _first_text(parsed, "source_ref", "ref"),
        }, None

    def _compare(
        self,
        ctx: CollectorContext,
        project: Project,
        lane: DeployLane,
        deployed_sha: str,
    ) -> tuple[dict[str, object] | None, str | None]:
        provider = provider_for(project.repo_url, ctx.config, transport=self._transport)
        ref = None if provider is None else provider.parse(project.repo_url)
        if provider is None or ref is None:
            return None, unsupported_reason(project.repo_url, ctx.config)
        try:
            comparison = provider.compare(ref, deployed_sha, lane.branch)
        except VogtError as exc:
            return None, f"compare failed: {exc}"
        if comparison is None:
            return None, (
                f"the forge could not compare {deployed_sha[:12]} with {lane.branch}"
            )
        commits = comparison.commits[-MAX_COMMITS:]
        return {
            "status": comparison.status,
            "ahead_by": comparison.ahead_by,
            "behind_by": comparison.behind_by,
            "head_sha": comparison.head_sha,
            "commits": [{"sha": sha, "subject": subject} for sha, subject in commits],
            "truncated": comparison.ahead_by > len(commits),
        }, None

    def _get(self, url: str) -> tuple[int, bytes]:
        headers = {"Accept": "application/json", "User-Agent": "vogt-deploy-lanes"}
        if self._transport is not None:
            return self._transport(url, headers, b"", "GET")
        if not url.startswith(("https://", "http://")):  # pragma: no cover
            msg = "version_url must be http(s)"
            raise OSError(msg)
        request = urllib.request.Request(url, headers=headers, method="GET")
        try:
            with urllib.request.urlopen(  # scheme checked above
                request, timeout=VERSION_TIMEOUT_SECONDS
            ) as response:
                return int(response.status), bytes(response.read(65536))
        except urllib.error.HTTPError as exc:  # pragma: no cover - network shape
            return int(exc.code), b""


def receipt_status(value: object) -> str | None:
    """`passed`, `failed`, or the receipt's own word, from what it reported."""
    if not isinstance(value, str) or not value:
        return None
    lowered = value.strip().lower()
    if lowered in _PASSED:
        return "passed"
    if lowered in _FAILED:
        return "failed"
    return lowered


def _receipt_fields(parsed: dict[str, object]) -> dict[str, object]:
    """The receipt facts Vogt reads — never the whole document, which may
    carry image digests and deployment ids nobody asked to retain."""
    smoke = parsed.get("live_smoke")
    status = parsed.get("status") or parsed.get("outcome")
    if status is None and isinstance(smoke, dict):
        status = smoke.get("status")
    return {
        "source_sha": _first_text(parsed, "source_sha", "sha", "commit"),
        "source_tag": _first_text(parsed, "source_tag", "tag"),
        "timestamp": _first_text(parsed, "timestamp", "deployed_at", "finished_at"),
        "environment": _first_text(parsed, "environment"),
        "status": receipt_status(status),
        "url": _first_text(parsed, "url", "pipeline_url", "log_url"),
    }


def _json_object(raw: bytes) -> dict[str, object] | None:
    try:
        parsed = json.loads(raw.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError):
        return None
    return parsed if isinstance(parsed, dict) else None


def _first_text(payload: dict[str, object], *names: str) -> str | None:
    for name in names:
        value = payload.get(name)
        if isinstance(value, str) and value:
            return value
    return None


def _is_sha(value: object) -> bool:
    return isinstance(value, str) and bool(_SHA.match(value))


__all__ = ["DeployLanesCollector", "receipt_status"]
