"""The use-cases. Adapters call these and nothing deeper.

Grouped by the thing they act on rather than by transport, because the
transports are generated and there is only one of each use-case.
"""

from __future__ import annotations

from vogt.application.services.history import list_audit, list_events
from vogt.application.services.instance import (
    init_instance,
    serve_mcp_stdio,
    status,
)
from vogt.application.services.projects import (
    brief_project,
    create_project,
    get_project,
    list_projects,
    register_project,
    transition_project,
)
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
    "backlog",
    "brief_project",
    "bugs",
    "comment_work",
    "create_actor",
    "create_initiative",
    "create_label",
    "create_project",
    "create_work",
    "get_project",
    "get_work",
    "init_instance",
    "list_actors",
    "list_audit",
    "list_events",
    "list_initiatives",
    "list_labels",
    "list_projects",
    "list_work",
    "list_workflows",
    "register_project",
    "relate_work",
    "serve_mcp_stdio",
    "status",
    "transition_project",
    "transition_work",
    "unrelate_work",
    "update_work",
    "why",
]
