"""Group 10 — state on a PVC, backups, and engine restarts (cluster read)."""

from __future__ import annotations

import json
from datetime import UTC, datetime, timedelta
from typing import Any

from ..harness import Case, Check, Context, failed, passed, skipped


def _kubectl_json(ctx: Context, *arguments: str) -> dict[str, Any]:
    from .sessions import kubectl

    return kubectl(ctx, *arguments)


def state_on_a_pvc(ctx: Context) -> Check:
    ctx.require_kubectl()
    pvcs = _kubectl_json(ctx, "-n", "vogt-dev", "get", "pvc", "-o", "json")
    items = pvcs.get("items") or []
    bound = [
        str((item.get("metadata") or {}).get("name"))
        for item in items
        if (item.get("status") or {}).get("phase") == "Bound"
    ]
    if not bound:
        return failed("no Bound PVC in vogt-dev")
    engine = ctx.client.tool("engine_status")
    state_dir = str(engine.get("state_dir") or "")
    if not state_dir:
        return skipped(
            "engine_status did not report a state_dir; cannot tell which mount "
            "holds state"
        )
    deployment = _kubectl_json(
        ctx, "-n", "vogt-dev", "get", "deployment", "vogt-dev-engine", "-o", "json"
    )
    pods = _kubectl_json(
        ctx, "-n", "vogt-dev", "get", "pods", "-l", "app=vogt-dev-engine", "-o", "json"
    )
    spec = ((deployment.get("spec") or {}).get("template") or {}).get("spec") or {}
    volumes = {str(volume.get("name")): volume for volume in spec.get("volumes") or []}
    state_mounts: list[tuple[str, str, str]] = []
    for container in spec.get("containers") or []:
        for mount in container.get("volumeMounts") or []:
            path = str(mount.get("mountPath") or "")
            if path and (
                state_dir == path or state_dir.startswith(path.rstrip("/") + "/")
            ):
                state_mounts.append(
                    (str(container.get("name")), str(mount.get("name")), path)
                )
    if not state_mounts:
        return failed(f"no container mounts the engine state_dir {state_dir!r}")
    ephemeral = [
        f"{container}:{name}:{path}"
        for container, name, path in state_mounts
        if "emptyDir" in volumes.get(name, {})
    ]
    if ephemeral:
        return failed(
            f"engine state ({state_dir}) is mounted from emptyDir: {ephemeral}",
            bound=bound,
        )
    mounted = ", ".join(f"{container}:{name}" for container, name, _ in state_mounts)
    return passed(
        f"PVCs Bound: {bound}; state_dir {state_dir} is on {mounted} "
        f"({len(pods.get('items') or [])} engine pod(s))"
    )


def backup_succeeded(ctx: Context) -> Check:
    ctx.require_kubectl()
    cron = _kubectl_json(
        ctx, "-n", "vogt-dev", "get", "cronjob", "k8s-backup", "-o", "json"
    )
    suspended = bool((cron.get("spec") or {}).get("suspend"))
    if suspended:
        return failed("the k8s-backup CronJob is suspended")
    jobs = _kubectl_json(ctx, "-n", "vogt-dev", "get", "jobs", "-o", "json")
    owned: list[dict[str, Any]] = []
    for job in jobs.get("items") or []:
        owners = (job.get("metadata") or {}).get("ownerReferences") or []
        if any(str(owner.get("name")) == "k8s-backup" for owner in owners):
            owned.append(job)
    if not owned:
        return skipped("k8s-backup has no Job yet on this cluster")
    latest = max(
        owned, key=lambda job: str((job.get("metadata") or {}).get("creationTimestamp"))
    )
    status = latest.get("status") or {}
    if int(status.get("succeeded") or 0) < 1:
        conditions = status.get("conditions") or []
        return failed(
            "the latest k8s-backup Job has not succeeded: "
            f"{json.dumps(conditions)[:300]}"
        )
    return passed(
        "latest k8s-backup Job succeeded: "
        f"{((latest.get('metadata') or {}).get('name'))}"
    )


def engine_restarts(ctx: Context) -> Check:
    ctx.require_kubectl()
    pods = _kubectl_json(
        ctx, "-n", "vogt-dev", "get", "pods", "-l", "app=vogt-dev-engine", "-o", "json"
    )
    items = pods.get("items") or []
    if not items:
        return failed("no vogt-dev-engine pod is running")
    recent: list[str] = []
    cutoff = datetime.now(UTC) - timedelta(hours=24)
    for pod in items:
        for status in (pod.get("status") or {}).get("containerStatuses") or []:
            count = int(status.get("restartCount") or 0)
            if count == 0:
                continue
            finished = ((status.get("lastState") or {}).get("terminated") or {}).get(
                "finishedAt"
            )
            when = _parse(finished)
            if when is None or when >= cutoff:
                recent.append(
                    f"{pod.get('metadata', {}).get('name')}:"
                    f"{status.get('name')} x{count} at {finished}"
                )
    if recent:
        return failed("the engine restarted in the last 24h: " + "; ".join(recent))
    return passed(f"{len(items)} engine pod(s), zero restarts in the last 24h")


def _parse(value: object) -> datetime | None:
    if not isinstance(value, str) or not value:
        return None
    try:
        return datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return None


CASES = [
    Case(
        "K1",
        "persistence",
        "P1",
        "critical",
        "state is on a Bound PVC, not emptyDir",
        state_on_a_pvc,
    ),
    Case(
        "K2",
        "persistence",
        "P2",
        "high",
        "the daily backup Job succeeded",
        backup_succeeded,
    ),
    Case(
        "K3",
        "persistence",
        "R1",
        "high",
        "the engine has not restarted in 24h",
        engine_restarts,
    ),
]
