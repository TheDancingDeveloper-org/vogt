"""The M0 operation set.

Six operations, each defined once. `status`, `project.register`,
`project.list`, `events.list` and `audit.list` are available on all three
surfaces; `init` is local-only and says why in `LOCAL_ONLY`.
"""

from __future__ import annotations

from typing import Any

from vogt.application import services
from vogt.application.models import (
    AuditListResult,
    EventListResult,
    InitParams,
    InitResult,
    ListAuditParams,
    ListEventsParams,
    ListProjectsParams,
    ProjectListResult,
    ProjectResult,
    RegisterProjectParams,
    StatusParams,
    StatusResult,
)
from vogt.registry.operation import CliBinding, HttpRoute, Operation


def build_operations() -> list[Operation[Any, Any]]:
    """Define every operation the M0 build exposes."""
    return [
        Operation(
            name="init",
            summary="Create or bring forward the instance in the data directory.",
            scope="admin",
            mutating=False,
            params_model=InitParams,
            result_model=InitResult,
            handler=services.init_instance,
            route=HttpRoute("POST", "/instance/init"),
            cli=CliBinding(("init",)),
        ),
        Operation(
            name="status",
            summary="Report instance identity, schema versions, and row counts.",
            scope="read",
            mutating=False,
            params_model=StatusParams,
            result_model=StatusResult,
            handler=services.status,
            route=HttpRoute("GET", "/status"),
            cli=CliBinding(("status",)),
        ),
        Operation(
            name="project.register",
            summary="Register an existing folder or repository as a project.",
            scope="project.write",
            mutating=True,
            params_model=RegisterProjectParams,
            result_model=ProjectResult,
            handler=services.register_project,
            route=HttpRoute("POST", "/projects"),
            cli=CliBinding(("project", "register")),
        ),
        Operation(
            name="project.list",
            summary="List registered projects.",
            scope="read",
            mutating=False,
            params_model=ListProjectsParams,
            result_model=ProjectListResult,
            handler=services.list_projects,
            route=HttpRoute("GET", "/projects"),
            cli=CliBinding(("project", "list")),
        ),
        Operation(
            name="events.list",
            summary="Read the cursor-based event feed.",
            scope="read",
            mutating=False,
            params_model=ListEventsParams,
            result_model=EventListResult,
            handler=services.list_events,
            route=HttpRoute("GET", "/events"),
            cli=CliBinding(("events", "list")),
        ),
        Operation(
            name="audit.list",
            summary="Query the audit log.",
            scope="read",
            mutating=False,
            params_model=ListAuditParams,
            result_model=AuditListResult,
            handler=services.list_audit,
            route=HttpRoute("GET", "/audit"),
            cli=CliBinding(("audit", "list")),
        ),
    ]
