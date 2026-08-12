"""Declared-store entities.

These are the shapes the application layer reads and writes, and — because
every surface is generated from one registry — they are also the shapes the
REST, CLI and MCP surfaces return. One definition, three transports.
"""

from __future__ import annotations

from datetime import datetime
from typing import Annotated, Literal

from pydantic import BaseModel, ConfigDict, StringConstraints

from vogt.core.principal import ActorKind

LifecycleState = Literal["incubating", "active", "maintenance", "archived"]
ComplianceStatus = Literal["compliant", "non_compliant", "not_checked"]
TrustState = Literal["verified", "stale", "unverified", "disputed"]

#: A reason is required on every audited write and may not be blank.
#: Whitespace-only reasons are a way of technically complying while
#: explaining nothing, so they are stripped and then rejected.
Reason = Annotated[str, StringConstraints(strip_whitespace=True, min_length=1)]

#: A non-empty single-line name.
Name = Annotated[str, StringConstraints(strip_whitespace=True, min_length=1)]


class Entity(BaseModel):
    """Base for entities that cross a transport boundary."""

    model_config = ConfigDict(extra="forbid", frozen=True)


class Actor(Entity):
    """A human or an agent. Both are first-class (FR-W7)."""

    id: str
    kind: ActorKind
    display_name: str
    identity_ref: str
    disabled: bool = False
    created_at: datetime


class Project(Entity):
    """One explicitly registered repository or folder (FR-P1, FR-P5)."""

    id: str
    slug: str
    name: str
    root_path: str
    repo_url: str | None = None
    lifecycle_state: LifecycleState = "active"
    current_version: str | None = None
    contract_version: str | None = None
    compliance_status: ComplianceStatus = "not_checked"
    compliance_checked_at: datetime | None = None
    exclusions: list[str] = []
    trust_state: TrustState = "unverified"
    created_at: datetime
    updated_at: datetime


class AuditRecord(Entity):
    """One declared write, explained (FR-S1)."""

    id: str
    txn_id: str
    revision: int
    actor_id: str
    actor_identity_ref: str
    operation: str
    entity_kind: str
    entity_id: str
    reason: str
    payload_digest: str
    at: datetime


class Event(Entity):
    """One row of the ordered notification feed (FR-N1).

    `seq` is the `/events` cursor — a single monotonic sequence over a single
    table, so no client ever merges orderings across the two stores.
    """

    seq: int
    kind: str
    entity_kind: str
    entity_id: str
    actor_id: str | None = None
    audit_id: str | None = None
    summary: dict[str, object] = {}
    at: datetime
