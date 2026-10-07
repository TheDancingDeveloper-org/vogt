"""The operation set.

Every capability of the product, defined once. Adding an entry here is what
gives an operation a CLI command, a REST route with request and response
schemas, and an MCP tool — and the parity harness fails if any of the three
is missing.
"""

from __future__ import annotations

from typing import Any

from vogt.application import services
from vogt.application.models import (
    ActorListResult,
    ActorResult,
    AdoptParams,
    AdoptResult,
    AgentActivitySearchParams,
    AgentActivitySearchResult,
    AgentActivitySummaryParams,
    AgentActivitySummaryResult,
    AgentCliListParams,
    AgentCliListResult,
    AgentCliUpdateParams,
    AgentCliUpdateResult,
    AnswerSessionParams,
    AuditListResult,
    AuthDecisionListParams,
    AuthDecisionListResult,
    BacklogParams,
    BacklogResult,
    BackupParams,
    BackupResult,
    BindBranchParams,
    BindSessionWorkParams,
    BindSessionWorkResult,
    BoardListParams,
    BoardListResult,
    BugsParams,
    CloneParams,
    CloneResult,
    CommentParams,
    CommentResult,
    ComplianceParams,
    ComplianceResult,
    ConnectParams,
    ConnectResult,
    ContractAdoptParams,
    ContractAdoptResult,
    ContractApplicableParams,
    ContractCheckParams,
    ContractCheckResult,
    ContractEvaluateParams,
    ContractExemptionResult,
    ContractInapplicableParams,
    CoverageParams,
    CoverageResult,
    CreateActorParams,
    CreateInitiativeParams,
    CreateLabelParams,
    CreateProjectParams,
    CreateProjectResult,
    CreateUserParams,
    CreateWorkParams,
    DecideGrantParams,
    DeployedVersionsParams,
    DeployedVersionsResult,
    DepsParams,
    DepsResult,
    DiagnosticsParams,
    DiagnosticsResult,
    DriftDetectParams,
    DriftDetectResult,
    DriftListParams,
    DriftListResult,
    DriftResolveParams,
    DriftResult,
    EngineSessionTokenParams,
    EngineSessionTokenResult,
    EventListResult,
    ExportParams,
    ExportResult,
    ForgeAccountLinkParams,
    ForgeAccountResult,
    ForgeAccountStatusParams,
    ForgeAccountStatusResult,
    ForgeAccountUnlinkParams,
    ForgeImportParams,
    ForgeLinkParams,
    ForgeLinkResult,
    ForgePublishParams,
    ForgePublishResult,
    ForgeReposParams,
    ForgeReposResult,
    GetProjectParams,
    GetWorkParams,
    HibernateSessionParams,
    HistoryListParams,
    HistoryListResult,
    ImportParams,
    ImportProjectParams,
    ImportProjectResult,
    ImportResult,
    InboxArchiveParams,
    InboxListParams,
    InboxListResult,
    InboxRestoreParams,
    InboxSnoozeParams,
    InboxTriageResult,
    InitiativeListResult,
    InitiativeResult,
    InitParams,
    InitResult,
    IssueTokenParams,
    IssueTokenResult,
    KeepSessionAwakeParams,
    LabelListResult,
    LabelResult,
    ListActorsParams,
    ListAuditParams,
    ListEventsParams,
    ListGrantsParams,
    ListGrantsResult,
    ListInitiativesParams,
    ListLabelsParams,
    ListProjectsParams,
    ListSessionsParams,
    ListSuppressionsParams,
    ListTokensParams,
    ListWorkParams,
    LogoutParams,
    LogoutResult,
    LogTailParams,
    LogTailResult,
    McpStdioParams,
    McpStdioResult,
    MigrateParams,
    MigrateResult,
    NotificationsParams,
    NotificationsResult,
    ObservationsParams,
    ObservationsResult,
    OnboardParams,
    OnboardResult,
    PlaceMetricsParams,
    PlaceMetricsResult,
    PreferenceGetParams,
    PreferenceGetResult,
    PreferenceSetParams,
    PreferenceSetResult,
    ProjectBriefParams,
    ProjectBriefResult,
    ProjectListResult,
    ProjectResult,
    PruneParams,
    PruneResult,
    PublishInitiativeParams,
    PublishInitiativeResult,
    RegisterProjectParams,
    RelateWorkParams,
    RemoveUserParams,
    RemoveUserResult,
    ReportBlockedParams,
    ReportUnblockedParams,
    RequestGrantParams,
    RestoreParams,
    RestoreResult,
    RevokeGrantParams,
    RevokeSuppressionParams,
    RevokeTokenParams,
    ScaffoldProjectParams,
    ScaffoldProjectResult,
    SearchOutputParams,
    SearchOutputResult,
    ServeParams,
    ServeResult,
    SessionAnswerResult,
    SessionBlockedResult,
    SessionGrantResult,
    SessionInputParams,
    SessionInputResult,
    SessionLastReplyParams,
    SessionLastReplyResult,
    SessionListResult,
    SessionResult,
    SessionScreenParams,
    SessionScreenResult,
    SessionSweepResult,
    SessionWaitParams,
    SessionWaitResult,
    SetPasswordParams,
    SetSessionRoleParams,
    SetWriteBackParams,
    StartSessionParams,
    StatusParams,
    StatusResult,
    StopSessionParams,
    SuppressionListResult,
    SuppressionResult,
    SuppressParams,
    SweepParams,
    SweepResult,
    SweepSessionsParams,
    TokenListResult,
    TokenResult,
    TransitionProjectParams,
    TransitionWorkParams,
    UnrelateWorkParams,
    UpdateInitiativeParams,
    UpdateProjectParams,
    UpdateWorkParams,
    UserListParams,
    UserListResult,
    UserResult,
    WakeSessionParams,
    WhoamiParams,
    WhoamiResult,
    WhyParams,
    WhyResult,
    WorkflowListParams,
    WorkflowListResult,
    WorkListResult,
    WorkResult,
    WriteBackListParams,
    WriteBackListResult,
)
from vogt.registry.operation import CliBinding, HttpRoute, Operation


def build_operations() -> list[Operation[Any, Any]]:
    """Define every operation this build exposes."""
    return [
        # -- instance ------------------------------------------------------
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
            name="migrate",
            summary="Bring both stores forward to this build's schema.",
            scope="admin",
            mutating=False,
            params_model=MigrateParams,
            result_model=MigrateResult,
            handler=services.migrate_instance,
            route=HttpRoute("POST", "/instance/migrate"),
            cli=CliBinding(("migrate",)),
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
            name="instance.diagnostics",
            summary=(
                "Deploy diagnostics: version, image digest, uptime, readiness "
                "checks, migration state and recent error log lines (redacted); "
                "peer=true also asks the configured peer instance (e.g. prod)."
            ),
            scope="read",
            mutating=False,
            params_model=DiagnosticsParams,
            result_model=DiagnosticsResult,
            handler=services.instance_diagnostics,
            route=HttpRoute("GET", "/instance/diagnostics"),
            cli=CliBinding(("diagnostics",)),
        ),
        Operation(
            name="place.metrics",
            summary="Read all bounded shell navigation counts in one response.",
            scope="read",
            mutating=False,
            params_model=PlaceMetricsParams,
            result_model=PlaceMetricsResult,
            handler=services.place_metrics,
            route=HttpRoute("GET", "/place/metrics"),
            cli=CliBinding(("place", "metrics")),
        ),
        Operation(
            name="connect",
            summary="How to reach this instance, and the client configuration "
            "for doing so. Renders the connection document.",
            scope="read",
            mutating=False,
            params_model=ConnectParams,
            result_model=ConnectResult,
            handler=services.connect,
            route=HttpRoute("GET", "/connect"),
            cli=CliBinding(("connect",)),
        ),
        Operation(
            name="mcp.stdio",
            summary="Serve MCP over stdin/stdout until the stream closes.",
            scope="read",
            mutating=False,
            params_model=McpStdioParams,
            result_model=McpStdioResult,
            handler=services.serve_mcp_stdio,
            route=HttpRoute("POST", "/mcp/stdio"),
            cli=CliBinding(("mcp", "stdio")),
        ),
        # -- projects ------------------------------------------------------
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
            name="project.create",
            summary="Scaffold a contract-compliant skeleton, then register it.",
            scope="project.write",
            mutating=True,
            params_model=CreateProjectParams,
            result_model=CreateProjectResult,
            handler=services.create_project,
            route=HttpRoute("POST", "/projects/create"),
            cli=CliBinding(("project", "create")),
        ),
        Operation(
            name="project.import",
            summary="Clone a GitHub repository into the import root, register "
            "it, and consolidate its existing forge state.",
            scope="project.write",
            mutating=True,
            params_model=ImportProjectParams,
            result_model=ImportProjectResult,
            handler=services.import_project,
            route=HttpRoute("POST", "/projects/import"),
            cli=CliBinding(("project", "import")),
        ),
        Operation(
            name="project.get",
            summary="Fetch one project by slug.",
            scope="read",
            mutating=False,
            params_model=GetProjectParams,
            result_model=ProjectResult,
            handler=services.get_project,
            route=HttpRoute("GET", "/projects/get"),
            cli=CliBinding(("project", "get")),
        ),
        Operation(
            name="project.list",
            summary=(
                "List registered projects, each with `writable`: whether a "
                "default work.create lands there now, and why not."
            ),
            scope="read",
            mutating=False,
            params_model=ListProjectsParams,
            result_model=ProjectListResult,
            handler=services.list_projects,
            route=HttpRoute("GET", "/projects"),
            cli=CliBinding(("project", "list")),
        ),
        Operation(
            name="project.brief",
            summary=(
                "The per-repo view: state, work, bugs, version, compliance. "
                "mode=summary (default) omits full items from top_backlog."
            ),
            scope="read",
            mutating=False,
            params_model=ProjectBriefParams,
            result_model=ProjectBriefResult,
            handler=services.brief_project,
            route=HttpRoute("GET", "/projects/brief"),
            cli=CliBinding(("project", "brief")),
        ),
        Operation(
            name="project.update",
            summary="Correct a project's declared repo URL or exclusions.",
            scope="project.write",
            mutating=True,
            params_model=UpdateProjectParams,
            result_model=ProjectResult,
            handler=services.update_project,
            route=HttpRoute("POST", "/projects/update"),
            cli=CliBinding(("project", "update")),
        ),
        Operation(
            name="project.transition",
            summary="Move a project through its lifecycle states.",
            scope="project.write",
            mutating=True,
            params_model=TransitionProjectParams,
            result_model=ProjectResult,
            handler=services.transition_project,
            route=HttpRoute("POST", "/projects/transition"),
            cli=CliBinding(("project", "transition")),
        ),
        # -- work ----------------------------------------------------------
        Operation(
            name="work.create",
            summary="Create a work item (feature / bug / chore / question).",
            scope="work.write",
            mutating=True,
            params_model=CreateWorkParams,
            result_model=WorkResult,
            handler=services.create_work,
            route=HttpRoute("POST", "/work"),
            cli=CliBinding(("work", "create")),
        ),
        Operation(
            name="work.get",
            summary=(
                "Fetch one work item with its relations, labels and comments "
                "(`id` is accepted for `ref`)."
            ),
            scope="read",
            mutating=False,
            params_model=GetWorkParams,
            result_model=WorkResult,
            handler=services.get_work,
            route=HttpRoute("GET", "/work/get"),
            cli=CliBinding(("work", "get")),
        ),
        Operation(
            name="work.list",
            summary=(
                "List work items with filters, paged (limit/offset, next_offset). "
                "mode=summary (default) returns compact rows; mode=full returns "
                "whole items. `query` searches title/body/ref; `status`/`state` "
                "alias `states`."
            ),
            scope="read",
            mutating=False,
            params_model=ListWorkParams,
            result_model=WorkListResult,
            handler=services.list_work,
            route=HttpRoute("GET", "/work"),
            cli=CliBinding(("work", "list")),
        ),
        Operation(
            name="board.list",
            summary="Read bounded, independently pageable Board cells in one snapshot.",
            scope="read",
            mutating=False,
            params_model=BoardListParams,
            result_model=BoardListResult,
            handler=services.list_board,
            # A structured cell batch belongs in a body even though the
            # operation is read-only; adapters still derive it from here.
            route=HttpRoute("POST", "/board/list"),
            cli=CliBinding(("board", "list")),
        ),
        Operation(
            name="work.update",
            summary="Change a work item's fields, assignee, or labels.",
            scope="work.write",
            mutating=True,
            params_model=UpdateWorkParams,
            result_model=WorkResult,
            handler=services.update_work,
            route=HttpRoute("POST", "/work/update"),
            cli=CliBinding(("work", "update")),
        ),
        Operation(
            name="work.transition",
            summary=(
                "Move a work item to another state, validating the edge; a "
                "refusal lists the allowed edges and the shortest path. "
                "walk=true takes that path, one audited hop per edge."
            ),
            scope="work.write",
            mutating=True,
            params_model=TransitionWorkParams,
            result_model=WorkResult,
            handler=services.transition_work,
            route=HttpRoute("POST", "/work/transition"),
            cli=CliBinding(("work", "transition")),
        ),
        Operation(
            name="work.relate",
            summary="Add a typed relation between two work items.",
            scope="work.write",
            mutating=True,
            params_model=RelateWorkParams,
            result_model=WorkResult,
            handler=services.relate_work,
            route=HttpRoute("POST", "/work/relate"),
            cli=CliBinding(("work", "relate")),
        ),
        Operation(
            name="work.unrelate",
            summary="Remove a typed relation between two work items.",
            scope="work.write",
            mutating=True,
            params_model=UnrelateWorkParams,
            result_model=WorkResult,
            handler=services.unrelate_work,
            route=HttpRoute("POST", "/work/unrelate"),
            cli=CliBinding(("work", "unrelate")),
        ),
        Operation(
            name="work.bind_branch",
            summary="Declare the git branch a work item is worked on.",
            scope="work.write",
            mutating=True,
            params_model=BindBranchParams,
            result_model=WorkResult,
            handler=services.bind_branch,
            route=HttpRoute("POST", "/work/bind-branch"),
            cli=CliBinding(("work", "bind-branch")),
        ),
        Operation(
            name="work.comment",
            summary="Comment on a work item, attributed to the acting actor.",
            scope="work.write",
            mutating=True,
            params_model=CommentParams,
            result_model=CommentResult,
            handler=services.comment_work,
            route=HttpRoute("POST", "/work/comment"),
            cli=CliBinding(("work", "comment")),
        ),
        # -- views ---------------------------------------------------------
        Operation(
            name="backlog",
            summary=(
                "The ranked backlog, globally or for one project, paged "
                "(limit/offset, next_offset). mode=summary (default) omits each "
                "row's full item; mode=full includes it."
            ),
            scope="read",
            mutating=False,
            params_model=BacklogParams,
            result_model=BacklogResult,
            handler=services.backlog,
            route=HttpRoute("GET", "/backlog"),
            cli=CliBinding(("backlog",)),
        ),
        Operation(
            name="bugs",
            summary="Open bugs across every project, ranked.",
            scope="read",
            mutating=False,
            params_model=BugsParams,
            result_model=BacklogResult,
            handler=services.bugs,
            route=HttpRoute("GET", "/bugs"),
            cli=CliBinding(("bugs",)),
        ),
        Operation(
            name="why",
            summary="Per-input score contributions for one ranked item.",
            scope="read",
            mutating=False,
            params_model=WhyParams,
            result_model=WhyResult,
            handler=services.why,
            route=HttpRoute("GET", "/why"),
            cli=CliBinding(("why",)),
        ),
        # -- taxonomy ------------------------------------------------------
        Operation(
            name="label.create",
            summary="Define a label.",
            scope="work.write",
            mutating=True,
            params_model=CreateLabelParams,
            result_model=LabelResult,
            handler=services.create_label,
            route=HttpRoute("POST", "/labels"),
            cli=CliBinding(("label", "create")),
        ),
        Operation(
            name="label.list",
            summary="List labels.",
            scope="read",
            mutating=False,
            params_model=ListLabelsParams,
            result_model=LabelListResult,
            handler=services.list_labels,
            route=HttpRoute("GET", "/labels"),
            cli=CliBinding(("label", "list")),
        ),
        Operation(
            name="initiative.create",
            summary="Create a cross-project initiative with a ranking weight.",
            scope="work.write",
            mutating=True,
            params_model=CreateInitiativeParams,
            result_model=InitiativeResult,
            handler=services.create_initiative,
            route=HttpRoute("POST", "/initiatives"),
            cli=CliBinding(("initiative", "create")),
        ),
        Operation(
            name="initiative.list",
            summary="List initiatives.",
            scope="read",
            mutating=False,
            params_model=ListInitiativesParams,
            result_model=InitiativeListResult,
            handler=services.list_initiatives,
            route=HttpRoute("GET", "/initiatives"),
            cli=CliBinding(("initiative", "list")),
        ),
        Operation(
            name="initiative.update",
            summary="Correct an initiative's title, body or weight, or close or "
            "reopen it.",
            scope="work.write",
            mutating=True,
            params_model=UpdateInitiativeParams,
            result_model=InitiativeResult,
            handler=services.update_initiative,
            route=HttpRoute("POST", "/initiatives/update"),
            cli=CliBinding(("initiative", "update")),
        ),
        # Project an initiative onto a forge tracking issue per linked repo
        #: additive, forward-only, and never a close. The scope is
        # `writeback` for the same arming rationale as the other verbs
        # that speak upstream — it creates and edits issues under the resolved
        # credential and nothing else.
        Operation(
            name="initiative.publish",
            summary="Create or adopt one forge tracking issue per linked repo "
            "the initiative spans, each carrying a managed checkbox task list "
            "of its member work items. Additive and forward-only; a closed "
            "initiative proposes closing its tracking issues, never writes it.",
            scope="writeback",
            mutating=True,
            params_model=PublishInitiativeParams,
            result_model=PublishInitiativeResult,
            handler=services.publish_initiative,
            route=HttpRoute("POST", "/initiatives/publish"),
            cli=CliBinding(("initiative", "publish")),
        ),
        Operation(
            name="actor.create",
            summary="Register a human or agent so work can be assigned to it.",
            scope="admin",
            mutating=True,
            params_model=CreateActorParams,
            result_model=ActorResult,
            handler=services.create_actor,
            route=HttpRoute("POST", "/actors"),
            cli=CliBinding(("actor", "create")),
        ),
        Operation(
            name="actor.list",
            summary="List actors.",
            scope="read",
            mutating=False,
            params_model=ListActorsParams,
            result_model=ActorListResult,
            handler=services.list_actors,
            route=HttpRoute("GET", "/actors"),
            cli=CliBinding(("actor", "list")),
        ),
        Operation(
            name="workflow.list",
            summary="The state machine each work-item kind is governed by.",
            scope="read",
            mutating=False,
            params_model=WorkflowListParams,
            result_model=WorkflowListResult,
            handler=services.list_workflows,
            route=HttpRoute("GET", "/workflows"),
            cli=CliBinding(("workflow", "list")),
        ),
        # -- collection ----------------------------------------------------
        Operation(
            name="sweep",
            summary="Run collectors over the registered projects.",
            scope="project.write",
            mutating=True,
            params_model=SweepParams,
            result_model=SweepResult,
            handler=services.sweep,
            route=HttpRoute("POST", "/sweep"),
            cli=CliBinding(("sweep",)),
        ),
        Operation(
            name="coverage",
            summary="What has looked at what, and how long ago.",
            scope="read",
            mutating=False,
            params_model=CoverageParams,
            result_model=CoverageResult,
            handler=services.coverage,
            route=HttpRoute("GET", "/coverage"),
            cli=CliBinding(("coverage",)),
        ),
        Operation(
            name="observations.list",
            summary="Raw evidence, including subjects ranked views filter out.",
            scope="read",
            mutating=False,
            params_model=ObservationsParams,
            result_model=ObservationsResult,
            handler=services.observations,
            route=HttpRoute("GET", "/observations"),
            cli=CliBinding(("observations", "list")),
        ),
        Operation(
            name="deps",
            summary="Dependency references out of a project, and into it.",
            scope="read",
            mutating=False,
            params_model=DepsParams,
            result_model=DepsResult,
            handler=services.deps,
            route=HttpRoute("GET", "/deps"),
            cli=CliBinding(("deps",)),
        ),
        Operation(
            name="observations.prune",
            summary="Apply the retention policy to observation history.",
            scope="admin",
            mutating=True,
            params_model=PruneParams,
            result_model=PruneResult,
            handler=services.prune,
            route=HttpRoute("POST", "/observations/prune"),
            cli=CliBinding(("observations", "prune")),
        ),
        # -- observed-first ------------------------------------------------
        Operation(
            name="suppress",
            summary="Exclude an observed subject from ranked views.",
            scope="work.write",
            mutating=True,
            params_model=SuppressParams,
            result_model=SuppressionResult,
            handler=services.suppress,
            route=HttpRoute("POST", "/suppressions"),
            cli=CliBinding(("suppress",)),
        ),
        Operation(
            name="suppression.list",
            summary="List suppressions.",
            scope="read",
            mutating=False,
            params_model=ListSuppressionsParams,
            result_model=SuppressionListResult,
            handler=services.list_suppressions,
            route=HttpRoute("GET", "/suppressions"),
            cli=CliBinding(("suppression", "list")),
        ),
        Operation(
            name="suppression.revoke",
            summary="Revoke a suppression, returning the subject to ranked views.",
            scope="work.write",
            mutating=True,
            params_model=RevokeSuppressionParams,
            result_model=SuppressionResult,
            handler=services.revoke_suppression,
            route=HttpRoute("POST", "/suppressions/revoke"),
            cli=CliBinding(("suppression", "revoke")),
        ),
        Operation(
            name="work.adopt",
            summary="Promote an observed subject into a declared work item.",
            scope="work.write",
            mutating=True,
            params_model=AdoptParams,
            result_model=AdoptResult,
            handler=services.adopt,
            route=HttpRoute("POST", "/work/adopt"),
            cli=CliBinding(("work", "adopt")),
        ),
        # -- contract and drift --------------------------------------------
        Operation(
            name="contract.evaluate",
            summary="Evaluate the contract against any path; stores nothing.",
            scope="read",
            mutating=False,
            params_model=ContractEvaluateParams,
            result_model=ContractCheckResult,
            handler=services.contract_evaluate,
            route=HttpRoute("POST", "/contract/evaluate"),
            cli=CliBinding(("contract", "evaluate")),
        ),
        Operation(
            name="contract.check",
            summary="Evaluate the project contract; returns every failing rule.",
            scope="project.write",
            mutating=True,
            params_model=ContractCheckParams,
            result_model=ContractCheckResult,
            handler=services.contract_check,
            route=HttpRoute("POST", "/contract/check"),
            cli=CliBinding(("contract", "check")),
        ),
        # -- adoption: opt-in, and the means to close what it reports -----
        Operation(
            name="contract.adopt",
            summary="Opt a project into the contract; it is not applied by default.",
            scope="project.write",
            mutating=True,
            params_model=ContractAdoptParams,
            result_model=ContractAdoptResult,
            handler=services.contract_adopt,
            route=HttpRoute("POST", "/contract/adopt"),
            cli=CliBinding(("contract", "adopt")),
        ),
        Operation(
            name="contract.decline",
            summary="Opt a project back out of the contract.",
            scope="project.write",
            mutating=True,
            params_model=ContractAdoptParams,
            result_model=ContractAdoptResult,
            handler=services.contract_decline,
            route=HttpRoute("POST", "/contract/decline"),
            cli=CliBinding(("contract", "decline")),
        ),
        Operation(
            name="contract.inapplicable",
            summary="Declare that a criterion cannot apply to a project, and why.",
            scope="project.write",
            mutating=True,
            params_model=ContractInapplicableParams,
            result_model=ContractExemptionResult,
            handler=services.contract_inapplicable,
            route=HttpRoute("POST", "/contract/inapplicable"),
            cli=CliBinding(("contract", "inapplicable")),
        ),
        Operation(
            name="contract.applicable",
            summary="Withdraw an inapplicability declaration.",
            scope="project.write",
            mutating=True,
            params_model=ContractApplicableParams,
            result_model=ContractExemptionResult,
            handler=services.contract_applicable,
            route=HttpRoute("POST", "/contract/applicable"),
            cli=CliBinding(("contract", "applicable")),
        ),
        Operation(
            name="project.scaffold",
            summary=(
                "Write the contract's skeleton into a registered project, "
                "never overwriting."
            ),
            scope="project.write",
            mutating=True,
            params_model=ScaffoldProjectParams,
            result_model=ScaffoldProjectResult,
            handler=services.scaffold_project,
            route=HttpRoute("POST", "/projects/scaffold"),
            cli=CliBinding(("project", "scaffold")),
        ),
        Operation(
            name="compliance",
            summary="A project's last recorded contract result, with its age.",
            scope="read",
            mutating=False,
            params_model=ComplianceParams,
            result_model=ComplianceResult,
            handler=services.compliance,
            route=HttpRoute("GET", "/compliance"),
            cli=CliBinding(("compliance",)),
        ),
        Operation(
            name="drift.detect",
            summary="Compare declared state against observation; raise proposals.",
            scope="project.write",
            mutating=True,
            params_model=DriftDetectParams,
            result_model=DriftDetectResult,
            handler=services.detect_drift,
            route=HttpRoute("POST", "/drift/detect"),
            cli=CliBinding(("drift", "detect")),
        ),
        Operation(
            name="drift.list",
            summary="Open drift proposals and their evidence.",
            scope="read",
            mutating=False,
            params_model=DriftListParams,
            result_model=DriftListResult,
            handler=services.list_drift,
            route=HttpRoute("GET", "/drift"),
            cli=CliBinding(("drift", "list")),
        ),
        Operation(
            name="drift.resolve",
            summary="Accept, reject, or contest a drift proposal.",
            scope="project.write",
            mutating=True,
            params_model=DriftResolveParams,
            result_model=DriftResult,
            handler=services.resolve_drift,
            route=HttpRoute("POST", "/drift/resolve"),
            cli=CliBinding(("drift", "resolve")),
        ),
        # -- service and identity ------------------------------------------
        Operation(
            name="serve",
            summary="Serve the API, MCP and health endpoints on one port.",
            scope="admin",
            mutating=False,
            params_model=ServeParams,
            result_model=ServeResult,
            handler=services.serve,
            route=HttpRoute("POST", "/instance/serve"),
            cli=CliBinding(("serve",)),
        ),
        # -- coding sessions --------------------------------------
        #
        # Three operations, so the capability arrives on CLI, REST and MCP at
        # once. `session.start` is scoped `work.write` rather than
        # `admin`: opening a terminal on a bug is the ordinary act of working
        # on it, and gating it behind admin would mean the agents that most
        # need it are the ones that cannot.
        Operation(
            name="session.start",
            summary=(
                "Open a coding session (a terminal) for a work item or a "
                "project. With `template` (e.g. claude) it runs that agent, "
                "and a `task` is delivered as the agent's first prompt, so it "
                "starts working instead of opening idle. Returns both ids "
                "(ses_… and engine_session_id); then poll session_screen "
                "until `ready`."
            ),
            scope="work.write",
            mutating=True,
            params_model=StartSessionParams,
            result_model=SessionResult,
            handler=services.start_session,
            route=HttpRoute("POST", "/sessions"),
            cli=CliBinding(("session", "start")),
        ),
        Operation(
            name="session.list",
            summary=(
                "List coding sessions with their live activity state "
                "(running / idle / waiting-for-input / exited / errored) and "
                "`alive`, including sessions opened from the GUI (unlinked, "
                "engine UUID only). Each row has both ids: `id` (ses_…) and "
                "`engine_session_id`."
            ),
            scope="read",
            mutating=False,
            params_model=ListSessionsParams,
            result_model=SessionListResult,
            handler=services.list_sessions,
            route=HttpRoute("GET", "/sessions"),
            cli=CliBinding(("session", "list")),
        ),
        Operation(
            name="session.stop",
            summary=(
                "Stop a coding session and revoke the token it ran with. "
                "Takes either id: Vogt's ses_… id or the engine's session UUID "
                "(an unlinked GUI session is killed, with no token to revoke). "
                "The process is killed; its screen and log stay readable."
            ),
            scope="work.write",
            mutating=True,
            params_model=StopSessionParams,
            result_model=SessionResult,
            handler=services.stop_session,
            route=HttpRoute("POST", "/sessions/stop"),
            cli=CliBinding(("session", "stop")),
        ),
        # -- driving a session -------------------------------------
        #
        # Typing into a terminal is `work.write`, the scope that already
        # opens and stops one (and that the engine maps to its `sessions`
        # capability); every call is audited with the byte count and key
        # names, never the text. Reading the screen is `read`, the same as
        # reading the output log — both show what the terminal printed.
        Operation(
            name="session.input",
            summary=(
                "Type into a session: text, then named keys (enter, esc, tab, "
                "arrows, ctrl-c, ctrl-d, backspace), then Enter if submit. "
                "Takes either id: ses_… or the engine UUID. Read "
                "session_screen first and never send a blind Enter: at a "
                "menu it accepts whatever is highlighted (dismiss with esc). "
                "Audited (byte count and keys, never the text)."
            ),
            scope="work.write",
            mutating=True,
            params_model=SessionInputParams,
            result_model=SessionInputResult,
            handler=services.session_input,
            route=HttpRoute("POST", "/sessions/input"),
            cli=CliBinding(("session", "input")),
        ),
        # Hibernation: `work.write`, like stopping and typing. Waking spawns a
        # process and mints the session a new token, so it is a write too —
        # which is why `session.wait` (a `read`) reports a hibernated session
        # rather than waking it.
        Operation(
            name="session.sweep",
            summary=(
                "Oversee every session at once: one row per live or "
                "hibernated session with its activity, turn timing, last "
                "reply excerpt, blocked report, permission dialog and the "
                "last lines of its screen, ordered by who needs attention "
                "(approval, blocked, waiting, stalled, running, idle, "
                "hibernated) with the reason. Use this instead of "
                "session_screen per session."
            ),
            scope="read",
            mutating=False,
            params_model=SweepSessionsParams,
            result_model=SessionSweepResult,
            handler=services.sweep_sessions,
            route=HttpRoute("GET", "/sessions/sweep"),
            cli=CliBinding(("session", "sweep")),
        ),
        Operation(
            name="session.hibernate",
            summary=(
                "Hibernate a session: stop its processes to free their memory, "
                "keep it listed (activity `hibernated`, with its last screen), "
                "and revoke its token. session_wake (or session_input, which "
                "wakes it) resumes the same agent conversation under the same "
                "id. Only for a session whose conversation id the engine knows "
                "(Claude Code started by Vogt, or any resume); a shell needs "
                "allow_shell and wakes fresh. Takes either id."
            ),
            scope="work.write",
            mutating=True,
            params_model=HibernateSessionParams,
            result_model=SessionResult,
            handler=services.hibernate_session,
            route=HttpRoute("POST", "/sessions/hibernate"),
            cli=CliBinding(("session", "hibernate")),
        ),
        Operation(
            name="session.wake",
            summary=(
                "Wake a hibernated session: start it again under the same id, "
                "resuming its agent conversation in the directory it ran in, "
                "with a new token for the same actor. A live session is "
                "returned as it is. session_input wakes one by itself; wait "
                "for ready (session_wait) before typing after this. Takes "
                "either id."
            ),
            scope="work.write",
            mutating=True,
            params_model=WakeSessionParams,
            result_model=SessionResult,
            handler=services.wake_session,
            route=HttpRoute("POST", "/sessions/wake"),
            cli=CliBinding(("session", "wake")),
        ),
        Operation(
            name="session.keep_awake",
            summary=(
                "Pin a session awake so the engine's idle policy never "
                "hibernates it (keep_awake=true), or unpin it. Takes either id."
            ),
            scope="work.write",
            mutating=True,
            params_model=KeepSessionAwakeParams,
            result_model=SessionResult,
            handler=services.keep_session_awake,
            route=HttpRoute("POST", "/sessions/keep-awake"),
            cli=CliBinding(("session", "keep-awake")),
        ),
        Operation(
            name="session.set_role",
            summary=(
                "Nominate a session as oversight (role=oversight): one that "
                "supervises other sessions. It is pinned awake, so it comes "
                "back by itself after a redeploy, and the GUI lists it first. "
                "role=worker makes it an ordinary session again. Takes either "
                "id."
            ),
            scope="work.write",
            mutating=True,
            params_model=SetSessionRoleParams,
            result_model=SessionResult,
            handler=services.set_session_role,
            route=HttpRoute("POST", "/sessions/role"),
            cli=CliBinding(("session", "set-role")),
        ),
        Operation(
            name="session.bind_work",
            summary=(
                "Declare which work item a session serves (work_item=WI-n), "
                "or that it serves none (work_item=null) — one current item "
                "per session, re-declarable, audited. Any principal with "
                "work.write may bind: a person, the session itself (omit id "
                "inside a session Vogt started), or its overseer. Never moves "
                "the terminal or the item's state; the item then shows the "
                "session as being worked by it. Takes either id."
            ),
            scope="work.write",
            mutating=True,
            params_model=BindSessionWorkParams,
            result_model=BindSessionWorkResult,
            handler=services.bind_session_work,
            route=HttpRoute("POST", "/sessions/work-item"),
            cli=CliBinding(("session", "bind")),
        ),
        Operation(
            name="session.grant_request",
            summary=(
                "Ask a person to approve one scoped item for one live session "
                "(WI-973): a named credential (secret_name + project_id) the "
                "target then fetches with `vogt-agent-auth fetch VAR`. It "
                "appears in the Inbox; nothing is granted until a person "
                "approves. Ask for your own session, or — as an oversight "
                "session — for a session you drive. uses=once (default) or "
                "ttl; ttl_seconds 60..86400."
            ),
            scope="work.write",
            mutating=True,
            params_model=RequestGrantParams,
            result_model=SessionGrantResult,
            handler=services.request_grant,
            route=HttpRoute("POST", "/sessions/grants"),
            cli=CliBinding(("session", "grant-request")),
        ),
        Operation(
            name="session.grant_decide",
            summary=(
                "Approve or deny a pending grant. A person's decision only: "
                "every agent is refused. Approval is applied to the live "
                "session by the engine before it is recorded."
            ),
            scope="work.write",
            mutating=True,
            params_model=DecideGrantParams,
            result_model=SessionGrantResult,
            handler=services.decide_grant,
            route=HttpRoute("POST", "/sessions/grants/decide"),
            cli=CliBinding(("session", "grant-decide")),
        ),
        Operation(
            name="session.grant_revoke",
            summary=(
                "Withdraw a pending grant or revoke an approved one at once. "
                "A person, or the session that asked, may."
            ),
            scope="work.write",
            mutating=True,
            params_model=RevokeGrantParams,
            result_model=SessionGrantResult,
            handler=services.revoke_grant,
            route=HttpRoute("POST", "/sessions/grants/revoke"),
            cli=CliBinding(("session", "grant-revoke")),
        ),
        Operation(
            name="session.grant_list",
            summary=(
                "Grants to sessions, newest first, by state (pending, "
                "approved, denied, revoked, expired) or target. Names only, "
                "never a value."
            ),
            scope="read",
            mutating=False,
            params_model=ListGrantsParams,
            result_model=ListGrantsResult,
            handler=services.list_grants,
            route=HttpRoute("GET", "/sessions/grants"),
            cli=CliBinding(("session", "grants")),
        ),
        Operation(
            name="session.answer",
            summary=(
                "Answer the dialog a session shows (activity awaiting-approval) "
                "by choice: `option` (its number) or `label` (unique text of "
                "it). Works for permission dialogs and startup gates (folder "
                "trust, external CLAUDE.md imports, reading outside the "
                "working directory); the engine moves the highlight itself. "
                "Pass expect_question = approval.question so a stale answer "
                "is refused. Audited. Takes either id."
            ),
            scope="work.write",
            mutating=True,
            params_model=AnswerSessionParams,
            result_model=SessionAnswerResult,
            handler=services.answer_session,
            route=HttpRoute("POST", "/sessions/answer"),
            cli=CliBinding(("session", "answer")),
        ),
        Operation(
            name="session.token",
            summary=(
                "The session engine's call: mint the agent credential of a "
                "session it started itself (GUI, protected template), bound to "
                "agent:engine:<uuid>, or revoke it (`revoke`). Refused to every "
                "other caller (403 engine_only). Gives such sessions an agent "
                "identity, so the core tells their agents from people. Audited."
            ),
            scope="work.write",
            mutating=True,
            params_model=EngineSessionTokenParams,
            result_model=EngineSessionTokenResult,
            handler=services.engine_session_token,
            route=HttpRoute("POST", "/sessions/token"),
            # HTTP-only (registry.HTTP_ONLY): never registered on the CLI.
            cli=CliBinding(("engine-session-token",)),
        ),
        Operation(
            name="session.screen",
            summary=(
                "Read what a session's terminal shows right now: visible "
                "lines, cursor, title, activity and readiness. `ready` true "
                "means the program is waiting for input — wait for it before "
                "typing. Takes either id: ses_… or the engine UUID. Needs an "
                "engine with the screen route."
            ),
            scope="read",
            mutating=False,
            params_model=SessionScreenParams,
            result_model=SessionScreenResult,
            handler=services.session_screen,
            route=HttpRoute("GET", "/sessions/screen"),
            cli=CliBinding(("session", "screen")),
        ),
        Operation(
            name="session.last_reply",
            summary=(
                "Read the last N replies of a session's agent (Claude Code or "
                "Codex) from its own transcript — whole, current and "
                "redacted, unlike the screen. Says how the conversation was "
                "found (`basis`). Takes either id."
            ),
            scope="read",
            mutating=False,
            params_model=SessionLastReplyParams,
            result_model=SessionLastReplyResult,
            handler=services.last_reply,
            route=HttpRoute("GET", "/sessions/last-reply"),
            cli=CliBinding(("session", "last-reply")),
        ),
        Operation(
            name="session.wait",
            summary=(
                "Wait for a session instead of polling it: blocks (up to "
                "timeout_s, max 600) until it is ready for input — or needs a "
                "person (awaiting-approval, blocked) or exits — or, with "
                "until=exited / any_change, until it exits or changes at all. "
                "Returns why (`outcome`, `matched`) and the screen then. "
                "Takes either id."
            ),
            scope="read",
            mutating=False,
            params_model=SessionWaitParams,
            result_model=SessionWaitResult,
            handler=services.session_wait,
            route=HttpRoute("GET", "/sessions/wait"),
            cli=CliBinding(("session", "wait")),
        ),
        # An agent saying it cannot go on without a person. `work.write`, the
        # scope a session's own token holds, because it is the agent's own
        # report about its own session; audited with the text, which is meant
        # to be read.
        Operation(
            name="session.report_blocked",
            summary=(
                "Report that this session's agent is blocked on a person: "
                "what it needs (`blocker`) and the concrete `items` to do. "
                "Shown on session_list/session_screen, raised in the Inbox "
                "and pushed; drivers stop re-prompting. Omit `id` from inside "
                "a session Vogt started. Clear with session_report_unblocked."
            ),
            scope="work.write",
            mutating=True,
            params_model=ReportBlockedParams,
            result_model=SessionBlockedResult,
            handler=services.report_blocked,
            route=HttpRoute("POST", "/sessions/blocked"),
            cli=CliBinding(("session", "blocked")),
        ),
        Operation(
            name="session.report_unblocked",
            summary=(
                "Clear a session's blocked report once the person has acted "
                "(or the agent found a way on). Omit `id` from inside a "
                "session Vogt started."
            ),
            scope="work.write",
            mutating=True,
            params_model=ReportUnblockedParams,
            result_model=SessionBlockedResult,
            handler=services.report_unblocked,
            route=HttpRoute("POST", "/sessions/unblocked"),
            cli=CliBinding(("session", "unblocked")),
        ),
        # -- runtime-pinned agent CLIs ------------------------------
        #
        # The engine decides which Claude Code / Codex a new session runs;
        # these put its report and its installer on the three surfaces so an
        # agent can say what it runs and an operator can move the pin with a
        # reason, without an image build. `admin` for the move: it downloads
        # and executes a package from npm inside the pod.
        Operation(
            name="agent_cli.list",
            summary="Report the pod's agent CLIs: active, baked and upstream versions.",
            scope="read",
            mutating=False,
            params_model=AgentCliListParams,
            result_model=AgentCliListResult,
            handler=services.agent_cli_list,
            route=HttpRoute("GET", "/agent-clis"),
            cli=CliBinding(("agent-cli", "list")),
        ),
        Operation(
            name="agent_cli.update",
            summary="Make a version of an agent CLI the one new sessions run.",
            scope="admin",
            mutating=True,
            params_model=AgentCliUpdateParams,
            result_model=AgentCliUpdateResult,
            handler=services.agent_cli_update,
            route=HttpRoute("POST", "/agent-clis/update"),
            cli=CliBinding(("agent-cli", "update")),
        ),
        # -- agent activity index ---------------------------------
        #
        # What agents did, read from their transcripts by the opt-in
        # `agent-activity` collector and redacted before it was stored. Reads
        # only: the index is built by `sweep`, never written by a caller.
        Operation(
            name="agent_activity.search",
            summary=(
                "Search the tool calls agents made (from their transcripts, "
                "redacted): by text, service, tool, errors, time, project or "
                "session."
            ),
            scope="read",
            mutating=False,
            params_model=AgentActivitySearchParams,
            result_model=AgentActivitySearchResult,
            handler=services.search_agent_activity,
            route=HttpRoute("GET", "/agent-activity"),
            cli=CliBinding(("agent-activity", "search")),
        ),
        Operation(
            name="agent_activity.summary",
            summary=(
                "Per agent conversation: tool counts, error rate and time spent "
                "waiting on tools, for a session or a project."
            ),
            scope="read",
            mutating=False,
            params_model=AgentActivitySummaryParams,
            result_model=AgentActivitySummaryResult,
            handler=services.summarize_agent_activity,
            route=HttpRoute("GET", "/agent-activity/summary"),
            cli=CliBinding(("agent-activity", "summary")),
        ),
        # -- session history ---------------------------------------
        #
        # Three reads that surface the engine's session history to agents
        # (MCP), scripts (REST) and operators (CLI) at once — the GUI already
        # reaches the engine directly. Static GET paths with the id as a query
        # field, the house convention (there are no `{param}` routes); the id
        # in `session.log_tail` rides on `LogTailParams.id`.
        Operation(
            name="session.history_list",
            summary="List archived sessions (history), newest first.",
            scope="read",
            mutating=False,
            params_model=HistoryListParams,
            result_model=HistoryListResult,
            handler=services.history_list,
            route=HttpRoute("GET", "/sessions/history"),
            cli=CliBinding(("session", "history")),
        ),
        Operation(
            name="session.search_output",
            summary="Search session output (live sessions included).",
            scope="read",
            mutating=False,
            params_model=SearchOutputParams,
            result_model=SearchOutputResult,
            handler=services.search_output,
            route=HttpRoute("GET", "/sessions/history/search"),
            cli=CliBinding(("session", "search")),
        ),
        Operation(
            name="session.log_tail",
            summary=(
                "Read the tail of a session's output log, readable — the "
                "history of what it printed, not its current screen (use "
                "session_screen for that). Takes either id: Vogt's ses_… id "
                "or the engine's session UUID."
            ),
            scope="read",
            mutating=False,
            params_model=LogTailParams,
            result_model=LogTailResult,
            handler=services.log_tail,
            route=HttpRoute("GET", "/sessions/log"),
            cli=CliBinding(("session", "log")),
        ),
        Operation(
            name="token.issue",
            summary="Issue a scoped token bound to an actor. Shown once.",
            scope="admin",
            mutating=True,
            params_model=IssueTokenParams,
            result_model=IssueTokenResult,
            handler=services.issue_token,
            route=HttpRoute("POST", "/tokens"),
            cli=CliBinding(("token", "issue")),
        ),
        Operation(
            name="token.list",
            summary="List tokens. Never returns a secret.",
            scope="admin",
            mutating=False,
            params_model=ListTokensParams,
            result_model=TokenListResult,
            handler=services.list_tokens,
            route=HttpRoute("GET", "/tokens"),
            cli=CliBinding(("token", "list")),
        ),
        Operation(
            name="token.revoke",
            summary="Revoke a token.",
            scope="admin",
            mutating=True,
            params_model=RevokeTokenParams,
            result_model=TokenResult,
            handler=services.revoke_token,
            route=HttpRoute("POST", "/tokens/revoke"),
            cli=CliBinding(("token", "revoke")),
        ),
        Operation(
            name="auth.decisions",
            summary="The allow and deny log. The denials are the interesting half.",
            scope="admin",
            mutating=False,
            params_model=AuthDecisionListParams,
            result_model=AuthDecisionListResult,
            handler=services.list_auth_decisions,
            route=HttpRoute("GET", "/auth/decisions"),
            cli=CliBinding(("auth", "decisions")),
        ),
        Operation(
            name="auth.whoami",
            summary="Who the caller is, as authentication decided, and their scopes.",
            scope="read",
            mutating=False,
            params_model=WhoamiParams,
            result_model=WhoamiResult,
            handler=services.whoami,
            route=HttpRoute("GET", "/auth/whoami"),
            cli=CliBinding(("auth", "whoami")),
        ),
        Operation(
            name="auth.logout",
            summary="Revoke the session or token this call arrived with.",
            scope="read",
            mutating=True,
            params_model=LogoutParams,
            result_model=LogoutResult,
            handler=services.logout,
            route=HttpRoute("POST", "/auth/logout"),
            cli=CliBinding(("auth", "logout")),
        ),
        # -- users: humans who sign in with a password ----------------------
        Operation(
            name="user.create",
            summary="Give a human a username and password to sign in with.",
            scope="admin",
            mutating=True,
            params_model=CreateUserParams,
            result_model=UserResult,
            handler=services.create_user,
            route=HttpRoute("POST", "/users"),
            cli=CliBinding(("user", "create")),
        ),
        Operation(
            name="user.list",
            summary="List logins. Never returns a hash.",
            scope="admin",
            mutating=False,
            params_model=UserListParams,
            result_model=UserListResult,
            handler=services.list_users,
            route=HttpRoute("GET", "/users"),
            cli=CliBinding(("user", "list")),
        ),
        Operation(
            name="user.set_password",
            summary="Replace a user's password, ending their sessions.",
            scope="admin",
            mutating=True,
            params_model=SetPasswordParams,
            result_model=UserResult,
            handler=services.set_password,
            route=HttpRoute("POST", "/users/password"),
            cli=CliBinding(("user", "passwd")),
        ),
        Operation(
            name="user.remove",
            summary="Take a login away. The actor and its history stay.",
            scope="admin",
            mutating=True,
            params_model=RemoveUserParams,
            result_model=RemoveUserResult,
            handler=services.remove_user,
            route=HttpRoute("POST", "/users/remove"),
            cli=CliBinding(("user", "remove")),
        ),
        # -- backup and portability ----------------------------------------
        Operation(
            name="backup",
            summary="Snapshot both stores with a schema-version manifest.",
            scope="admin",
            mutating=False,
            params_model=BackupParams,
            result_model=BackupResult,
            handler=services.backup,
            route=HttpRoute("POST", "/instance/backup"),
            cli=CliBinding(("backup",)),
        ),
        Operation(
            name="restore",
            summary="Verify a backup's manifest, then replace the live stores.",
            scope="admin",
            mutating=False,
            params_model=RestoreParams,
            result_model=RestoreResult,
            handler=services.restore,
            route=HttpRoute("POST", "/instance/restore"),
            cli=CliBinding(("restore",)),
        ),
        Operation(
            name="clone",
            summary="Restore another instance's backup here as a copy: keep "
            "this instance's id and credentials, revoke the source's tokens, "
            "disarm forge write-back, never copy push subscriptions.",
            scope="admin",
            # Unlike restore, a clone lands an audit row (in the copy it
            # writes), so it is a mutation with a required reason.
            mutating=True,
            params_model=CloneParams,
            result_model=CloneResult,
            handler=services.clone,
            route=HttpRoute("POST", "/instance/clone"),
            cli=CliBinding(("clone",)),
        ),
        Operation(
            name="export",
            summary="Write the declared entities as JSON (format 2: with "
            "comments, relations and the clone stamp; --project for one).",
            scope="read",
            mutating=False,
            params_model=ExportParams,
            result_model=ExportResult,
            handler=services.export_instance,
            route=HttpRoute("POST", "/instance/export"),
            cli=CliBinding(("export",)),
        ),
        Operation(
            name="import",
            summary="Merge an export into this instance under the documented "
            "conflict policy. A dry-run diff unless --apply --confirm; "
            "--strict refuses any both-sides change.",
            scope="admin",
            # With --apply it lands one audited write (operation `import`), so
            # it is a mutation with a required reason, like clone.
            mutating=True,
            params_model=ImportParams,
            result_model=ImportResult,
            handler=services.import_instance,
            route=HttpRoute("POST", "/instance/import"),
            cli=CliBinding(("import",)),
        ),
        # -- the forge module -----------------------------------------
        Operation(
            name="forge.onboard",
            summary="Read a repository's existing issues, PRs, labels and "
            "releases into observations. Changes nothing upstream.",
            scope="project.write",
            mutating=True,
            params_model=OnboardParams,
            result_model=OnboardResult,
            handler=services.onboard,
            route=HttpRoute("POST", "/forge/onboard"),
            cli=CliBinding(("forge", "onboard")),
        ),
        Operation(
            name="forge.writeback",
            summary="Set a project's write-back policy: none, comment_only, full.",
            # The scope named `writeback` gates arming write-back, and
            # nothing else does. It required `project.write` until r13, which
            # left `writeback` gating no operation at all — a scope a token can
            # hold and be granted nothing by. Arming upstream pushes is a
            # different power from registering a project or moving its
            # lifecycle state, and a deployment that wants an agent to manage
            # projects without ever letting it speak to a forge now has a way
            # to say so.
            scope="writeback",
            mutating=True,
            params_model=SetWriteBackParams,
            result_model=ProjectResult,
            handler=services.set_write_back,
            route=HttpRoute("POST", "/forge/writeback"),
            cli=CliBinding(("forge", "writeback")),
        ),
        # Linking a project is what arms write-through on it: from
        # here `work.create` opens issues under the resolved credential, so
        # the scope is `writeback` — "arming write-back and nothing else" —
        # exactly as `forge.writeback` and the account link are.
        Operation(
            name="forge.link",
            summary="Make a registered project upstream-truth: its work "
            "items become its forge issues, work writes go through, and any "
            "open native items migrate upstream.",
            scope="writeback",
            mutating=True,
            params_model=ForgeLinkParams,
            result_model=ForgeLinkResult,
            handler=services.link_project,
            route=HttpRoute("POST", "/forge/link"),
            cli=CliBinding(("forge", "link")),
        ),
        # The first verb that creates upstream state and pushes commits
        # — beyond the deliberately non-destructive write-back set.
        # It refuses an existing remote, never force-pushes, and requires a
        # clean, explicit local checkout; the scope is `writeback` for the
        # same arming rationale as `forge.link`, because a published
        # project is a linked one the moment the push lands.
        Operation(
            name="forge.publish",
            summary="Create a repository under your forge credential, push "
            "the local default branch, and make the project upstream-truth. "
            "Refuses existing remotes; never force-pushes.",
            scope="writeback",
            mutating=True,
            params_model=ForgePublishParams,
            result_model=ForgePublishResult,
            handler=services.publish_project,
            route=HttpRoute("POST", "/forge/publish"),
            cli=CliBinding(("forge", "publish")),
        ),
        # -- per-actor forge accounts -------------------------------
        #
        # Linking a personal PAT arms upstream writes attributed to the actor,
        # so link/unlink take the `writeback` scope: that is the scope for
        # arming write-back and nothing else, and a per-actor link is
        # exactly that — arming it under the actor's own credential rather than
        # the instance's. `admin` would be heavier than the intent ("manage my
        # own link") and would deny it to the very agents meant to self-serve.
        # `account_status` reads only the actor's own cleartext state, so it is
        # a plain `read`.
        Operation(
            name="forge.account_link",
            summary="Link your own forge account by pasting a PAT, stored "
            "encrypted; upstream writes are then attributed to you.",
            scope="writeback",
            mutating=True,
            params_model=ForgeAccountLinkParams,
            result_model=ForgeAccountResult,
            handler=services.link_forge_account,
            route=HttpRoute("POST", "/forge/accounts"),
            cli=CliBinding(("forge", "account", "link")),
        ),
        Operation(
            name="forge.account_status",
            summary="Whether you have linked a forge account, and as whom. "
            "Never returns the token.",
            scope="read",
            mutating=False,
            params_model=ForgeAccountStatusParams,
            result_model=ForgeAccountStatusResult,
            handler=services.status_forge_account,
            route=HttpRoute("GET", "/forge/accounts"),
            cli=CliBinding(("forge", "account", "status")),
        ),
        Operation(
            name="forge.account_unlink",
            summary="Remove your linked forge account; writes fall back to the "
            "instance file token.",
            scope="writeback",
            mutating=True,
            params_model=ForgeAccountUnlinkParams,
            result_model=ForgeAccountResult,
            handler=services.unlink_forge_account,
            route=HttpRoute("POST", "/forge/accounts/unlink"),
            cli=CliBinding(("forge", "account", "unlink")),
        ),
        Operation(
            name="forge.repos",
            summary="List the repositories your linked credential can see, so "
            "you can pick which to import (clone + full sync).",
            # A plain read: it enumerates what the acting credential is entitled
            # to see and computes `already_registered` against declared state.
            # It changes nothing, so no `writeback` scope and no reason.
            scope="read",
            mutating=False,
            params_model=ForgeReposParams,
            result_model=ForgeReposResult,
            handler=services.list_forge_repos,
            route=HttpRoute("GET", "/forge/repos"),
            cli=CliBinding(("forge", "repos")),
        ),
        # The verb the picker leads to: turn a repository `forge.repos`
        # listed into a project — clone under the acting credential, register,
        # consolidate. It writes (a project row, a clone on disk) and arms
        # write-through by linking, exactly as `project.import` does, so it
        # takes the same `project.write` scope and reason as that verb.
        Operation(
            name="forge.import",
            summary="Import a repository the picker listed as a project: clone "
            "it under your forge credential, register it, and consolidate its "
            "forge state so it comes back linked and ready for write-back.",
            scope="project.write",
            mutating=True,
            params_model=ForgeImportParams,
            result_model=ImportProjectResult,
            handler=services.import_forge_repo,
            route=HttpRoute("POST", "/forge/import"),
            cli=CliBinding(("forge", "import")),
        ),
        Operation(
            name="forge.actions",
            summary="The ledger of what Vogt has said upstream, and what landed.",
            scope="read",
            mutating=False,
            params_model=WriteBackListParams,
            result_model=WriteBackListResult,
            handler=services.list_write_backs,
            route=HttpRoute("GET", "/forge/actions"),
            cli=CliBinding(("forge", "actions")),
        ),
        # -- history -------------------------------------------------------
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
            name="notifications",
            summary="What GitHub is trying to say about the registered "
            "projects. Separate from the events feed, which is this "
            "instance's own history.",
            scope="read",
            mutating=False,
            params_model=NotificationsParams,
            result_model=NotificationsResult,
            handler=services.list_notifications,
            route=HttpRoute("GET", "/notifications"),
            cli=CliBinding(("notifications",)),
        ),
        Operation(
            name="deployed.versions",
            summary="What each configured deployment lane runs (deployed "
            "SHA/version) against its branch head, with the commits and work "
            "items merged but not yet deployed. Read from the deploy-lanes "
            "collector's last sweep.",
            scope="read",
            mutating=False,
            params_model=DeployedVersionsParams,
            result_model=DeployedVersionsResult,
            handler=services.deployed_versions,
            route=HttpRoute("GET", "/deployed-versions"),
            cli=CliBinding(("deployed-versions",)),
        ),
        Operation(
            name="inbox.list",
            summary="List the normalized attention Inbox with coverage.",
            scope="read",
            mutating=False,
            params_model=InboxListParams,
            result_model=InboxListResult,
            handler=services.list_inbox,
            route=HttpRoute("GET", "/inbox"),
            cli=CliBinding(("inbox", "list")),
        ),
        Operation(
            name="inbox.archive",
            summary="Archive one normalized Inbox occurrence.",
            scope="work.write",
            mutating=True,
            params_model=InboxArchiveParams,
            result_model=InboxTriageResult,
            handler=services.archive_inbox,
            route=HttpRoute("POST", "/inbox/archive"),
            cli=CliBinding(("inbox", "archive")),
        ),
        Operation(
            name="inbox.snooze",
            summary="Snooze one normalized Inbox occurrence until a deadline.",
            scope="work.write",
            mutating=True,
            params_model=InboxSnoozeParams,
            result_model=InboxTriageResult,
            handler=services.snooze_inbox,
            route=HttpRoute("POST", "/inbox/snooze"),
            cli=CliBinding(("inbox", "snooze")),
        ),
        Operation(
            name="inbox.restore",
            summary="Restore one archived or snoozed Inbox occurrence.",
            scope="work.write",
            mutating=True,
            params_model=InboxRestoreParams,
            result_model=InboxTriageResult,
            handler=services.restore_inbox,
            route=HttpRoute("POST", "/inbox/restore"),
            cli=CliBinding(("inbox", "restore")),
        ),
        # -- per-actor preferences ------------------------------------------
        #
        # Both act only on the calling principal's own rows — neither takes an
        # actor — so `set` is a `read`-scope write, as `auth.logout` is: a
        # read-only token may still save its own Inbox filter, and can touch
        # nobody else's. It is still audited and takes a reason.
        Operation(
            name="preference.get",
            summary="Read your own saved settings (e.g. inbox.filter), "
            "each with its version.",
            scope="read",
            mutating=False,
            params_model=PreferenceGetParams,
            result_model=PreferenceGetResult,
            handler=services.get_preferences,
            route=HttpRoute("GET", "/preferences"),
            cli=CliBinding(("preference", "get")),
        ),
        Operation(
            name="preference.set",
            summary="Save one of your own settings as a JSON object; {} clears "
            "it. Optionally only if it is still at expected_version.",
            scope="read",
            mutating=True,
            params_model=PreferenceSetParams,
            result_model=PreferenceSetResult,
            handler=services.set_preference,
            route=HttpRoute("POST", "/preferences"),
            cli=CliBinding(("preference", "set")),
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
