"""The use-cases. Adapters call these and nothing deeper.

Grouped by the thing they act on rather than by transport, because the
transports are generated and there is only one of each use-case.
"""

from __future__ import annotations

from vogt.application.services.auth import (
    issue_token,
    list_auth_decisions,
    list_tokens,
    revoke_token,
)
from vogt.application.services.board import list_board
from vogt.application.services.collect import coverage, deps, observations, sweep
from vogt.application.services.connect import connect
from vogt.application.services.contracts import (
    compliance,
    contract_adopt,
    contract_applicable,
    contract_check,
    contract_decline,
    contract_evaluate,
    contract_inapplicable,
)
from vogt.application.services.drift_service import (
    detect_drift,
    list_drift,
    resolve_drift,
)
from vogt.application.services.forge_accounts import (
    link_forge_account,
    status_forge_account,
    unlink_forge_account,
)
from vogt.application.services.forge_import import import_forge_repo
from vogt.application.services.forge_link import link_project
from vogt.application.services.forge_publish import publish_project
from vogt.application.services.forge_repos import list_forge_repos
from vogt.application.services.history import list_audit, list_events
from vogt.application.services.imports import import_project
from vogt.application.services.inbox import (
    archive_inbox,
    list_inbox,
    restore_inbox,
    snooze_inbox,
)
from vogt.application.services.initiative_publish import (
    publish_initiative,
    reproject_initiative,
)
from vogt.application.services.install import install_bootstrap, install_status
from vogt.application.services.instance import (
    init_instance,
    migrate_instance,
    serve,
    serve_mcp_stdio,
    status,
)
from vogt.application.services.lifecycle import (
    backup,
    export_instance,
    import_instance,
    restore,
)
from vogt.application.services.notifications import list_notifications
from vogt.application.services.observed_first import (
    adopt,
    list_suppressions,
    revoke_suppression,
    suppress,
)
from vogt.application.services.place import place_metrics
from vogt.application.services.projects import (
    brief_project,
    create_project,
    get_project,
    list_projects,
    register_project,
    scaffold_project,
    transition_project,
    update_project,
)
from vogt.application.services.retention import prune
from vogt.application.services.sessions import (
    history_list,
    list_sessions,
    log_tail,
    search_output,
    start_session,
    stop_session,
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
    bind_branch,
    comment_work,
    create_work,
    get_work,
    list_work,
    relate_work,
    transition_work,
    unrelate_work,
    update_work,
)
from vogt.application.services.writeback import (
    list_write_backs,
    onboard,
    set_write_back,
)

__all__ = [
    "adopt",
    "archive_inbox",
    "backlog",
    "backup",
    "bind_branch",
    "brief_project",
    "bugs",
    "comment_work",
    "compliance",
    "connect",
    "contract_adopt",
    "contract_applicable",
    "contract_check",
    "contract_decline",
    "contract_evaluate",
    "contract_inapplicable",
    "coverage",
    "create_actor",
    "create_initiative",
    "create_label",
    "create_project",
    "create_work",
    "deps",
    "detect_drift",
    "export_instance",
    "get_project",
    "get_work",
    "history_list",
    "import_forge_repo",
    "import_instance",
    "import_project",
    "init_instance",
    "install_bootstrap",
    "install_status",
    "issue_token",
    "link_forge_account",
    "link_project",
    "list_actors",
    "list_audit",
    "list_auth_decisions",
    "list_board",
    "list_drift",
    "list_events",
    "list_forge_repos",
    "list_inbox",
    "list_initiatives",
    "list_labels",
    "list_notifications",
    "list_projects",
    "list_sessions",
    "list_suppressions",
    "list_tokens",
    "list_work",
    "list_workflows",
    "list_write_backs",
    "log_tail",
    "migrate_instance",
    "observations",
    "onboard",
    "place_metrics",
    "prune",
    "publish_initiative",
    "publish_project",
    "register_project",
    "relate_work",
    "reproject_initiative",
    "resolve_drift",
    "restore",
    "restore_inbox",
    "revoke_suppression",
    "revoke_token",
    "scaffold_project",
    "search_output",
    "serve",
    "serve_mcp_stdio",
    "set_write_back",
    "snooze_inbox",
    "start_session",
    "status",
    "status_forge_account",
    "stop_session",
    "suppress",
    "sweep",
    "transition_project",
    "transition_work",
    "unlink_forge_account",
    "unrelate_work",
    "update_project",
    "update_work",
    "why",
]
