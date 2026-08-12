"""The use-cases. Adapters call these and nothing deeper.

Grouped by the thing they act on rather than by transport, because the
transports are generated and there is only one of each use-case.
"""

from __future__ import annotations

from vogt.application.services.collect import coverage, deps, observations, sweep
from vogt.application.services.contracts import compliance, contract_check
from vogt.application.services.drift_service import (
    detect_drift,
    list_drift,
    resolve_drift,
)
from vogt.application.services.history import list_audit, list_events
from vogt.application.services.instance import (
    init_instance,
    serve_mcp_stdio,
    status,
)
from vogt.application.services.observed_first import (
    adopt,
    list_suppressions,
    revoke_suppression,
    suppress,
)
from vogt.application.services.projects import (
    brief_project,
    create_project,
    get_project,
    list_projects,
    register_project,
    transition_project,
)
from vogt.application.services.retention import prune
from vogt.application.services.taxonomy import (
    create_actor,
    create_initiative,
    create_label,
    list_actors,
    list_initiatives,
    list_labels,
    list_workflows,
)
from vogt.application.services.views import backlog, bugs, why
from vogt.application.services.work import (
    comment_work,
    create_work,
    get_work,
    list_work,
    relate_work,
    transition_work,
    unrelate_work,
    update_work,
)

__all__ = [
    "adopt",
    "backlog",
    "brief_project",
    "bugs",
    "comment_work",
    "compliance",
    "contract_check",
    "coverage",
    "create_actor",
    "create_initiative",
    "create_label",
    "create_project",
    "create_work",
    "deps",
    "detect_drift",
    "get_project",
    "get_work",
    "init_instance",
    "list_actors",
    "list_audit",
    "list_drift",
    "list_events",
    "list_initiatives",
    "list_labels",
    "list_projects",
    "list_suppressions",
    "list_work",
    "list_workflows",
    "observations",
    "prune",
    "register_project",
    "relate_work",
    "resolve_drift",
    "revoke_suppression",
    "serve_mcp_stdio",
    "status",
    "suppress",
    "sweep",
    "transition_project",
    "transition_work",
    "unrelate_work",
    "update_work",
    "why",
]
