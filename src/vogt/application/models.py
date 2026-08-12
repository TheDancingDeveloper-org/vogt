"""Parameter and result models.

These are the argument schemas the operation registry publishes: the CLI
builds its flags from them, FastAPI builds its request/response schemas from
them, and MCP builds its `inputSchema` from them. One definition, three
transports — which is the mechanical half of transport parity (FR-A2).
"""

from __future__ import annotations

from pydantic import BaseModel, ConfigDict, Field

from vogt.core.entities import (
    Actor,
    AuditRecord,
    Event,
    LifecycleState,
    Name,
    Project,
    Reason,
)

#: Applied to collection within a project (FR-G12). Recorded at registration
#: so the value exists before the collectors that honour it arrive at M2.
DEFAULT_EXCLUSIONS: tuple[str, ...] = (
    ".venv/",
    "node_modules/",
    "target/",
    "dist/",
    "build/",
    ".git/",
)


class Params(BaseModel):
    """Base for operation parameters."""

    model_config = ConfigDict(extra="forbid")


class Result(BaseModel):
    """Base for operation results."""

    model_config = ConfigDict(extra="forbid")


class InitParams(Params):
    """`init` takes nothing: the data directory comes from configuration."""


class InitResult(Result):
    instance_id: str
    data_dir: str
    created: bool
    declared_schema_version: int
    observed_schema_version: int
    migrations_applied: list[str]


class StatusParams(Params):
    pass


class StoreCounts(Result):
    projects: int
    actors: int
    events: int
    audit: int


class StatusResult(Result):
    vogt_version: str
    instance_id: str
    data_dir: str
    principal: str
    revision: int
    declared_schema_version: int
    observed_schema_version: int
    counts: StoreCounts


class RegisterProjectParams(Params):
    name: Name = Field(description="Display name; the slug is derived from it.")
    root_path: str = Field(description="Folder or git repository this project is.")
    repo_url: str | None = Field(
        default=None, description="Optional remote the project is published at."
    )
    lifecycle_state: LifecycleState = Field(
        default="active", description="incubating / active / maintenance / archived."
    )
    reason: Reason = Field(description="Why this write is being made (audited).")


class ProjectResult(Result):
    project: Project


class ListProjectsParams(Params):
    limit: int = Field(default=50, ge=1, le=500)
    offset: int = Field(default=0, ge=0)


class ProjectListResult(Result):
    projects: list[Project]
    total: int


class ListEventsParams(Params):
    after: int = Field(
        default=0, ge=0, description="Cursor: return events with seq greater than this."
    )
    limit: int = Field(default=100, ge=1, le=1000)


class EventListResult(Result):
    events: list[Event]
    next_cursor: int


class ListAuditParams(Params):
    limit: int = Field(default=50, ge=1, le=500)
    actor_id: str | None = None
    operation: str | None = None
    entity_id: str | None = None


class AuditListResult(Result):
    records: list[AuditRecord]


__all__ = [
    "DEFAULT_EXCLUSIONS",
    "Actor",
    "AuditListResult",
    "AuditRecord",
    "EventListResult",
    "InitParams",
    "InitResult",
    "ListAuditParams",
    "ListEventsParams",
    "ListProjectsParams",
    "Params",
    "ProjectListResult",
    "ProjectResult",
    "RegisterProjectParams",
    "Result",
    "StatusParams",
    "StatusResult",
    "StoreCounts",
]
