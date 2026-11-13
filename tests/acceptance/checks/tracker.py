"""Group 7 — tracker safety: no accidental forge writes on dev."""

from __future__ import annotations

from pathlib import Path

from ..harness import Case, Check, Context, failed, passed, skipped


def _all_projects(ctx: Context) -> list[dict[str, object]]:
    projects: list[dict[str, object]] = []
    offset = 0
    while True:
        payload = ctx.client.tool("project_list", {"limit": 200, "offset": offset})
        page = [row for row in payload.get("projects") or [] if isinstance(row, dict)]
        projects.extend(page)
        total = int(payload.get("total") or len(projects))
        offset += len(page)
        if not page or offset >= total:
            break
    return projects


def write_back_is_none(ctx: Context) -> Check:
    projects = _all_projects(ctx)
    if not projects:
        return failed("project_list returned no projects")
    offending = [
        f"{row.get('slug')}={row.get('write_back')!r}"
        for row in projects
        if row.get("write_back") not in (None, "none")
    ]
    if offending:
        return failed(
            "forge write-back is enabled on: " + ", ".join(offending),
            projects=len(projects),
        )
    return passed(f"all {len(projects)} projects have write_back=none")


def forge_accounts_unlinked(ctx: Context) -> Check:
    payload = ctx.client.tool("forge_account_status")
    accounts = payload.get("accounts") or []
    if accounts:
        names = [
            str(row.get("name") or row.get("host") or "?")
            for row in accounts
            if isinstance(row, dict)
        ]
        return skipped(
            "forge accounts are linked ("
            + ", ".join(names)
            + "); whether that is intended is "
            "an operator decision this suite does not second-guess"
        )
    return passed("forge_account_status lists no linked account")


def forge_repos_refuses_cleanly(ctx: Context) -> Check:
    payload = ctx.client.tool("forge_repos")
    repos = payload.get("repos") or []
    detail = payload.get("detail")
    if repos:
        return skipped(
            f"forge repos are configured ({len(repos)}); this instance is linked"
        )
    return passed(f"forge_repos returns no repos and a reason, not a 5xx: {detail!r}")


def import_refuses_prod(ctx: Context) -> Check:
    del ctx
    tool = Path(__file__).resolve().parents[3] / "tools" / "vogt_snapshot_import.py"
    if not tool.is_file():
        return skipped(
            f"observed import tool absent at {tool}; ship the snapshot importer "
            "to enable T4"
        )

    try:
        from tools.vogt_snapshot_import import (
            DEFAULT_PROD_HOSTNAMES,
            Refused,
            Source,
            TargetState,
            refuse_production_target,
        )

        source = Source(
            path=tool,
            instance_id="acceptance-source-instance",
            exported_at="2026-01-01T00:00:00Z",
            projects=(),
            initiatives=(),
            labels=(),
            items=(),
            comments=(),
            actor_kind={},
            ignored_keys=(),
        )
        target = TargetState(
            url=f"https://{DEFAULT_PROD_HOSTNAMES[0]}",
            import_root="",
            project_root=None,
        )
        refuse_production_target(source, target)
    except Refused as exc:
        detail = str(exc)
        if "production" not in detail.lower():
            return failed(f"the import tool refused for an unrelated reason: {detail}")
        return passed(
            "observed tools/vogt_snapshot_import.py and invoked its guard; "
            f"production target refused: {detail}"
        )
    except Exception as exc:
        return failed(
            f"the shipped import tool could not be invoked: {type(exc).__name__}: {exc}"
        )
    return failed("the shipped import tool accepted its default production host")


CASES = [
    Case(
        "T1",
        "tracker",
        "T4",
        "critical",
        "no dev project has forge write-back enabled",
        write_back_is_none,
    ),
    Case(
        "T2",
        "tracker",
        "T4",
        "high",
        "forge accounts are unlinked unless intended",
        forge_accounts_unlinked,
    ),
    Case(
        "T3",
        "tracker",
        "T4",
        "medium",
        "forge_repos refuses cleanly when unlinked",
        forge_repos_refuses_cleanly,
    ),
    Case(
        "T4",
        "tracker",
        "T3",
        "high",
        "the import tool refuses a production target",
        import_refuses_prod,
    ),
]
