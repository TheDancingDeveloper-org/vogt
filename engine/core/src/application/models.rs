//! Parameter and result models, with the JSON Schemas derived from them.
//!
//! Ports the models in `src/vogt/application/models.py`. Every operation's
//! parameter and result schemas are derived here: `schemars` builds them from
//! these types, and [`DERIVED`] lists the operations they serve.
//!
//! The derived schemas are checked against Python's live `registry.dump` by
//! `tests/test_rust_schemas.py`, so a Rust model that validated differently
//! from the Python one fails there rather than at the swap.

use schemars::{json_schema, JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Serialize};

/// The operations whose schemas are derived from the types in this module.
///
/// The registry refuses to start if an operation has no schema here, so an
/// operation is listed when its models are ported.
pub const DERIVED: &[&str] = &[
    "init",
    "migrate",
    "observations.prune",
    "user.remove",
    "chat.list",
    "chat.get",
    "chat.create",
    "chat.send",
    "chat.decide",
    "chat.set_model",
    "chat.interrupt",
    "chat.archive",
    "chat.promote",
    "mcp.stdio",
    "contract.adopt",
    "contract.decline",
    "project.scaffold",
    "project.register",
    "project.create",
    "project.import",
    "project.get",
    "project.list",
    "project.update",
    "project.transition",
    "forge.account_unlink",
    "forge.account_link",
    "forge.account_status",
    "forge.onboard",
    "session.token",
    "session.log_tail",
    "session.commit_for",
    "workflow.list",
    "label.list",
    "label.create",
    "preference.get",
    "preference.set",
    "why",
    "actor.list",
    "actor.create",
    "user.list",
    "user.create",
    "user.set_password",
    "token.issue",
    "token.list",
    "token.revoke",
    "auth.whoami",
    "auth.logout",
    "auth.decisions",
    "suppress",
    "suppression.list",
    "suppression.revoke",
    "export",
    "backup",
    "restore",
    "serve",
    "connect",
    "status",
    "registry.dump",
    "instance.diagnostics",
    "engine.status",
    "place.metrics",
    "project.brief",
    "work.create",
    "work.get",
    "work.get_many",
    "work.list",
    "board.list",
    "work.update",
    "work.transition",
    "work.relate",
    "work.unrelate",
    "work.bind_branch",
    "work.comment",
    "backlog",
    "bugs",
    "initiative.create",
    "initiative.list",
    "initiative.update",
    "initiative.publish",
    "sweep",
    "coverage",
    "observations.list",
    "deps",
    "work.adopt",
    "contract.evaluate",
    "contract.check",
    "contract.inapplicable",
    "contract.applicable",
    "compliance",
    "drift.detect",
    "drift.list",
    "drift.resolve",
    "session.start",
    "session.list",
    "session.stop",
    "session.input",
    "session.sweep",
    "session.hibernate",
    "session.wake",
    "session.keep_awake",
    "session.rename",
    "session.remove",
    "session.set_role",
    "session.bind_work",
    "session.grant_request",
    "session.grant_decide",
    "session.grant_revoke",
    "session.grant_list",
    "watch.create",
    "watch.list",
    "watch.cancel",
    "decision.ask",
    "decision.answer",
    "decision.list",
    "decision.withdraw",
    "session.answer",
    "session.screen",
    "session.last_reply",
    "session.economics",
    "session.digest",
    "session.wait",
    "session.changes",
    "session.wait_any",
    "session.report_blocked",
    "session.report",
    "session.report_unblocked",
    "agent_cli.list",
    "agent_cli.update",
    "agent_activity.search",
    "agent_activity.summary",
    "session.history_list",
    "session.search_output",
    "clone",
    "import",
    "forge.writeback",
    "forge.link",
    "forge.publish",
    "forge.repos",
    "forge.import",
    "forge.actions",
    "events.list",
    "notifications",
    "deployed.versions",
    "usage.board",
    "inbox.list",
    "inbox.archive",
    "inbox.snooze",
    "inbox.restore",
    "charter.get",
    "charter.set",
    "audit.list",
];

mod access;
mod admin;
mod caught_up;
mod chat;
mod entities;
mod forge;
mod project;
mod records;
mod session;
mod session_ops;
mod system;
mod views;
mod work;

/// `(operation name, params schema, result schema)` for every name in
/// [`DERIVED`], as `registry::dump()` emits them.
pub fn derived_schemas() -> Vec<(&'static str, serde_json::Value, serde_json::Value)> {
    vec![
        ("init", schema_of::<InitParams>(), schema_of::<InitResult>()),
        (
            "migrate",
            schema_of::<MigrateParams>(),
            schema_of::<MigrateResult>(),
        ),
        (
            "observations.prune",
            schema_of::<PruneParams>(),
            schema_of::<PruneResult>(),
        ),
        (
            "user.remove",
            schema_of::<RemoveUserParams>(),
            schema_of::<RemoveUserResult>(),
        ),
        (
            "chat.list",
            schema_of::<chat::ChatListParams>(),
            schema_of::<chat::ChatListResult>(),
        ),
        (
            "chat.get",
            schema_of::<chat::ChatGetParams>(),
            schema_of::<chat::ChatDetailResult>(),
        ),
        (
            "chat.create",
            schema_of::<chat::ChatCreateParams>(),
            schema_of::<chat::ChatSendResult>(),
        ),
        (
            "chat.send",
            schema_of::<chat::ChatSendParams>(),
            schema_of::<chat::ChatSendResult>(),
        ),
        (
            "chat.decide",
            schema_of::<chat::ChatDecideParams>(),
            schema_of::<chat::ChatApprovalView>(),
        ),
        (
            "chat.set_model",
            schema_of::<chat::ChatSetModelParams>(),
            schema_of::<chat::ChatSummaryView>(),
        ),
        (
            "chat.interrupt",
            schema_of::<chat::ChatIdParams>(),
            schema_of::<chat::ChatSummaryView>(),
        ),
        (
            "chat.archive",
            schema_of::<chat::ChatArchiveParams>(),
            schema_of::<chat::ChatSummaryView>(),
        ),
        (
            "chat.promote",
            schema_of::<chat::ChatPromoteParams>(),
            schema_of::<chat::ChatPromoteResult>(),
        ),
        (
            "mcp.stdio",
            schema_of::<admin::McpStdioParams>(),
            schema_of::<admin::McpStdioResult>(),
        ),
        (
            "contract.adopt",
            schema_of::<project::ContractAdoptParams>(),
            schema_of::<project::ContractAdoptResult>(),
        ),
        (
            "contract.decline",
            schema_of::<project::ContractAdoptParams>(),
            schema_of::<project::ContractAdoptResult>(),
        ),
        (
            "project.scaffold",
            schema_of::<project::ScaffoldProjectParams>(),
            schema_of::<project::ScaffoldProjectResult>(),
        ),
        (
            "project.register",
            schema_of::<project::RegisterProjectParams>(),
            schema_of::<project::ProjectResult>(),
        ),
        (
            "project.create",
            schema_of::<project::CreateProjectParams>(),
            schema_of::<project::CreateProjectResult>(),
        ),
        (
            "project.import",
            schema_of::<project::ImportProjectParams>(),
            schema_of::<project::ImportProjectResult>(),
        ),
        (
            "project.get",
            schema_of::<project::GetProjectParams>(),
            schema_of::<project::ProjectResult>(),
        ),
        (
            "project.list",
            schema_of::<project::ListProjectsParams>(),
            schema_of::<project::ProjectListResult>(),
        ),
        (
            "project.update",
            schema_of::<project::UpdateProjectParams>(),
            schema_of::<project::ProjectResult>(),
        ),
        (
            "project.transition",
            schema_of::<project::TransitionProjectParams>(),
            schema_of::<project::ProjectResult>(),
        ),
        (
            "forge.account_unlink",
            schema_of::<forge::ForgeAccountUnlinkParams>(),
            schema_of::<forge::ForgeAccountResult>(),
        ),
        (
            "forge.account_link",
            schema_of::<forge::ForgeAccountLinkParams>(),
            schema_of::<forge::ForgeAccountResult>(),
        ),
        (
            "forge.account_status",
            schema_of::<forge::ForgeAccountStatusParams>(),
            schema_of::<forge::ForgeAccountStatusResult>(),
        ),
        (
            "forge.onboard",
            schema_of::<forge::OnboardParams>(),
            schema_of::<forge::OnboardResult>(),
        ),
        (
            "session.token",
            schema_of::<session::EngineSessionTokenParams>(),
            schema_of::<session::EngineSessionTokenResult>(),
        ),
        (
            "session.log_tail",
            schema_of::<session::LogTailParams>(),
            schema_of::<session::LogTailResult>(),
        ),
        (
            "session.commit_for",
            schema_of::<session::CommitForSessionParams>(),
            schema_of::<session::CommitForSessionResult>(),
        ),
        (
            "workflow.list",
            schema_of::<records::WorkflowListParams>(),
            schema_of::<records::WorkflowListResult>(),
        ),
        (
            "label.list",
            schema_of::<records::ListLabelsParams>(),
            schema_of::<records::LabelListResult>(),
        ),
        (
            "label.create",
            schema_of::<records::CreateLabelParams>(),
            schema_of::<records::LabelResult>(),
        ),
        (
            "preference.get",
            schema_of::<records::PreferenceGetParams>(),
            schema_of::<records::PreferenceGetResult>(),
        ),
        (
            "preference.set",
            schema_of::<records::PreferenceSetParams>(),
            schema_of::<records::PreferenceSetResult>(),
        ),
        (
            "why",
            schema_of::<records::WhyParams>(),
            schema_of::<records::WhyResult>(),
        ),
        (
            "actor.list",
            schema_of::<access::ListActorsParams>(),
            schema_of::<access::ActorListResult>(),
        ),
        (
            "actor.create",
            schema_of::<access::CreateActorParams>(),
            schema_of::<access::ActorResult>(),
        ),
        (
            "user.list",
            schema_of::<access::UserListParams>(),
            schema_of::<access::UserListResult>(),
        ),
        (
            "user.create",
            schema_of::<access::CreateUserParams>(),
            schema_of::<access::UserResult>(),
        ),
        (
            "user.set_password",
            schema_of::<access::SetPasswordParams>(),
            schema_of::<access::UserResult>(),
        ),
        (
            "token.issue",
            schema_of::<access::IssueTokenParams>(),
            schema_of::<access::IssueTokenResult>(),
        ),
        (
            "token.list",
            schema_of::<access::ListTokensParams>(),
            schema_of::<access::TokenListResult>(),
        ),
        (
            "token.revoke",
            schema_of::<access::RevokeTokenParams>(),
            schema_of::<access::TokenResult>(),
        ),
        (
            "auth.whoami",
            schema_of::<access::WhoamiParams>(),
            schema_of::<access::WhoamiResult>(),
        ),
        (
            "auth.logout",
            schema_of::<access::LogoutParams>(),
            schema_of::<access::LogoutResult>(),
        ),
        (
            "auth.decisions",
            schema_of::<access::AuthDecisionListParams>(),
            schema_of::<access::AuthDecisionListResult>(),
        ),
        (
            "suppress",
            schema_of::<access::SuppressParams>(),
            schema_of::<access::SuppressionResult>(),
        ),
        (
            "suppression.list",
            schema_of::<access::ListSuppressionsParams>(),
            schema_of::<access::SuppressionListResult>(),
        ),
        (
            "suppression.revoke",
            schema_of::<access::RevokeSuppressionParams>(),
            schema_of::<access::SuppressionResult>(),
        ),
        (
            "export",
            schema_of::<admin::ExportParams>(),
            schema_of::<admin::ExportResult>(),
        ),
        (
            "backup",
            schema_of::<admin::BackupParams>(),
            schema_of::<admin::BackupResult>(),
        ),
        (
            "restore",
            schema_of::<admin::RestoreParams>(),
            schema_of::<admin::RestoreResult>(),
        ),
        (
            "serve",
            schema_of::<admin::ServeParams>(),
            schema_of::<admin::ServeResult>(),
        ),
        (
            "connect",
            schema_of::<admin::ConnectParams>(),
            schema_of::<admin::ConnectResult>(),
        ),
        (
            "status",
            schema_of::<system::StatusParams>(),
            schema_of::<system::StatusResult>(),
        ),
        (
            "registry.dump",
            schema_of::<system::RegistryDumpParams>(),
            schema_of::<system::RegistryDumpResult>(),
        ),
        (
            "instance.diagnostics",
            schema_of::<system::DiagnosticsParams>(),
            schema_of::<system::DiagnosticsResult>(),
        ),
        (
            "engine.status",
            schema_of::<system::EngineStatusParams>(),
            schema_of::<system::EngineStatusResult>(),
        ),
        (
            "place.metrics",
            schema_of::<system::PlaceMetricsParams>(),
            schema_of::<system::PlaceMetricsResult>(),
        ),
        (
            "project.brief",
            schema_of::<work::ProjectBriefParams>(),
            schema_of::<work::ProjectBriefResult>(),
        ),
        (
            "work.create",
            schema_of::<work::CreateWorkParams>(),
            schema_of::<work::WorkResult>(),
        ),
        (
            "work.get",
            schema_of::<work::GetWorkParams>(),
            schema_of::<work::WorkResult>(),
        ),
        (
            "work.get_many",
            schema_of::<work::GetManyWorkParams>(),
            schema_of::<work::WorkManyResult>(),
        ),
        (
            "work.list",
            schema_of::<work::ListWorkParams>(),
            schema_of::<work::WorkListResult>(),
        ),
        (
            "board.list",
            schema_of::<work::BoardListParams>(),
            schema_of::<work::BoardListResult>(),
        ),
        (
            "work.update",
            schema_of::<work::UpdateWorkParams>(),
            schema_of::<work::WorkResult>(),
        ),
        (
            "work.transition",
            schema_of::<work::TransitionWorkParams>(),
            schema_of::<work::WorkResult>(),
        ),
        (
            "work.relate",
            schema_of::<work::RelateWorkParams>(),
            schema_of::<work::WorkResult>(),
        ),
        (
            "work.unrelate",
            schema_of::<work::UnrelateWorkParams>(),
            schema_of::<work::WorkResult>(),
        ),
        (
            "work.bind_branch",
            schema_of::<work::BindBranchParams>(),
            schema_of::<work::WorkResult>(),
        ),
        (
            "work.comment",
            schema_of::<work::CommentParams>(),
            schema_of::<work::CommentResult>(),
        ),
        (
            "backlog",
            schema_of::<work::BacklogParams>(),
            schema_of::<work::BacklogResult>(),
        ),
        (
            "bugs",
            schema_of::<work::BugsParams>(),
            schema_of::<work::BacklogResult>(),
        ),
        (
            "initiative.create",
            schema_of::<work::CreateInitiativeParams>(),
            schema_of::<work::InitiativeResult>(),
        ),
        (
            "initiative.list",
            schema_of::<work::ListInitiativesParams>(),
            schema_of::<work::InitiativeListResult>(),
        ),
        (
            "initiative.update",
            schema_of::<work::UpdateInitiativeParams>(),
            schema_of::<work::InitiativeResult>(),
        ),
        (
            "initiative.publish",
            schema_of::<work::PublishInitiativeParams>(),
            schema_of::<work::PublishInitiativeResult>(),
        ),
        (
            "sweep",
            schema_of::<work::SweepParams>(),
            schema_of::<work::SweepResult>(),
        ),
        (
            "coverage",
            schema_of::<work::CoverageParams>(),
            schema_of::<work::CoverageResult>(),
        ),
        (
            "observations.list",
            schema_of::<work::ObservationsParams>(),
            schema_of::<work::ObservationsResult>(),
        ),
        (
            "deps",
            schema_of::<work::DepsParams>(),
            schema_of::<work::DepsResult>(),
        ),
        (
            "work.adopt",
            schema_of::<work::AdoptParams>(),
            schema_of::<work::AdoptResult>(),
        ),
        (
            "contract.evaluate",
            schema_of::<work::ContractEvaluateParams>(),
            schema_of::<work::ContractCheckResult>(),
        ),
        (
            "contract.check",
            schema_of::<work::ContractCheckParams>(),
            schema_of::<work::ContractCheckResult>(),
        ),
        (
            "contract.inapplicable",
            schema_of::<work::ContractInapplicableParams>(),
            schema_of::<work::ContractExemptionResult>(),
        ),
        (
            "contract.applicable",
            schema_of::<work::ContractApplicableParams>(),
            schema_of::<work::ContractExemptionResult>(),
        ),
        (
            "compliance",
            schema_of::<work::ComplianceParams>(),
            schema_of::<work::ComplianceResult>(),
        ),
        (
            "drift.detect",
            schema_of::<work::DriftDetectParams>(),
            schema_of::<work::DriftDetectResult>(),
        ),
        (
            "drift.list",
            schema_of::<work::DriftListParams>(),
            schema_of::<work::DriftListResult>(),
        ),
        (
            "drift.resolve",
            schema_of::<work::DriftResolveParams>(),
            schema_of::<work::DriftResult>(),
        ),
        (
            "session.start",
            schema_of::<session_ops::StartSessionParams>(),
            schema_of::<session_ops::SessionResult>(),
        ),
        (
            "session.list",
            schema_of::<session_ops::ListSessionsParams>(),
            schema_of::<session_ops::SessionListResult>(),
        ),
        (
            "session.stop",
            schema_of::<session_ops::StopSessionParams>(),
            schema_of::<session_ops::SessionResult>(),
        ),
        (
            "session.input",
            schema_of::<session_ops::SessionInputParams>(),
            schema_of::<session_ops::SessionInputResult>(),
        ),
        (
            "session.sweep",
            schema_of::<session_ops::SweepSessionsParams>(),
            schema_of::<session_ops::SessionSweepResult>(),
        ),
        (
            "session.hibernate",
            schema_of::<session_ops::HibernateSessionParams>(),
            schema_of::<session_ops::SessionResult>(),
        ),
        (
            "session.wake",
            schema_of::<session_ops::WakeSessionParams>(),
            schema_of::<session_ops::SessionResult>(),
        ),
        (
            "session.keep_awake",
            schema_of::<session_ops::KeepSessionAwakeParams>(),
            schema_of::<session_ops::SessionResult>(),
        ),
        (
            "session.rename",
            schema_of::<session_ops::RenameSessionParams>(),
            schema_of::<session_ops::SessionResult>(),
        ),
        (
            "session.remove",
            schema_of::<session_ops::RemoveSessionParams>(),
            schema_of::<session_ops::SessionResult>(),
        ),
        (
            "session.set_role",
            schema_of::<session_ops::SetSessionRoleParams>(),
            schema_of::<session_ops::SessionResult>(),
        ),
        (
            "session.bind_work",
            schema_of::<session_ops::BindSessionWorkParams>(),
            schema_of::<session_ops::BindSessionWorkResult>(),
        ),
        (
            "session.grant_request",
            schema_of::<session_ops::RequestGrantParams>(),
            schema_of::<session_ops::SessionGrantResult>(),
        ),
        (
            "session.grant_decide",
            schema_of::<session_ops::DecideGrantParams>(),
            schema_of::<session_ops::SessionGrantResult>(),
        ),
        (
            "session.grant_revoke",
            schema_of::<session_ops::RevokeGrantParams>(),
            schema_of::<session_ops::SessionGrantResult>(),
        ),
        (
            "session.grant_list",
            schema_of::<session_ops::ListGrantsParams>(),
            schema_of::<session_ops::ListGrantsResult>(),
        ),
        (
            "watch.create",
            schema_of::<caught_up::CreateWatchParams>(),
            schema_of::<caught_up::WatchResult>(),
        ),
        (
            "watch.list",
            schema_of::<caught_up::ListWatchesParams>(),
            schema_of::<caught_up::ListWatchesResult>(),
        ),
        (
            "watch.cancel",
            schema_of::<caught_up::CancelWatchParams>(),
            schema_of::<caught_up::WatchResult>(),
        ),
        (
            "decision.ask",
            schema_of::<session_ops::AskDecisionParams>(),
            schema_of::<session_ops::DecisionResult>(),
        ),
        (
            "decision.answer",
            schema_of::<session_ops::AnswerDecisionParams>(),
            schema_of::<session_ops::DecisionResult>(),
        ),
        (
            "decision.list",
            schema_of::<session_ops::ListDecisionsParams>(),
            schema_of::<session_ops::ListDecisionsResult>(),
        ),
        (
            "decision.withdraw",
            schema_of::<session_ops::WithdrawDecisionParams>(),
            schema_of::<session_ops::DecisionResult>(),
        ),
        (
            "session.answer",
            schema_of::<session_ops::AnswerSessionParams>(),
            schema_of::<session_ops::SessionAnswerResult>(),
        ),
        (
            "session.screen",
            schema_of::<session_ops::SessionScreenParams>(),
            schema_of::<views::SessionScreenResult>(),
        ),
        (
            "session.last_reply",
            schema_of::<session_ops::SessionLastReplyParams>(),
            schema_of::<session_ops::SessionLastReplyResult>(),
        ),
        (
            "session.economics",
            schema_of::<session_ops::SessionEconomicsParams>(),
            schema_of::<session_ops::SessionEconomicsResult>(),
        ),
        (
            "session.digest",
            schema_of::<session_ops::SessionDigestParams>(),
            schema_of::<session_ops::SessionDigestResult>(),
        ),
        (
            "session.wait",
            schema_of::<session_ops::SessionWaitParams>(),
            schema_of::<session_ops::SessionWaitResult>(),
        ),
        (
            "session.changes",
            schema_of::<session_ops::SessionChangesParams>(),
            schema_of::<session_ops::SessionChangesResult>(),
        ),
        (
            "session.wait_any",
            schema_of::<session_ops::SessionWaitAnyParams>(),
            schema_of::<session_ops::SessionWaitAnyResult>(),
        ),
        (
            "session.report_blocked",
            schema_of::<session_ops::ReportBlockedParams>(),
            schema_of::<session_ops::SessionBlockedResult>(),
        ),
        (
            "session.report",
            schema_of::<caught_up::SessionReportParams>(),
            schema_of::<caught_up::SessionReportResult>(),
        ),
        (
            "session.report_unblocked",
            schema_of::<session_ops::ReportUnblockedParams>(),
            schema_of::<session_ops::SessionBlockedResult>(),
        ),
        (
            "agent_cli.list",
            schema_of::<session_ops::AgentCliListParams>(),
            schema_of::<session_ops::AgentCliListResult>(),
        ),
        (
            "agent_cli.update",
            schema_of::<session_ops::AgentCliUpdateParams>(),
            schema_of::<session_ops::AgentCliUpdateResult>(),
        ),
        (
            "agent_activity.search",
            schema_of::<session_ops::AgentActivitySearchParams>(),
            schema_of::<session_ops::AgentActivitySearchResult>(),
        ),
        (
            "agent_activity.summary",
            schema_of::<session_ops::AgentActivitySummaryParams>(),
            schema_of::<session_ops::AgentActivitySummaryResult>(),
        ),
        (
            "session.history_list",
            schema_of::<session_ops::HistoryListParams>(),
            schema_of::<session_ops::HistoryListResult>(),
        ),
        (
            "session.search_output",
            schema_of::<session_ops::SearchOutputParams>(),
            schema_of::<session_ops::SearchOutputResult>(),
        ),
        (
            "clone",
            schema_of::<system::CloneParams>(),
            schema_of::<system::CloneResult>(),
        ),
        (
            "import",
            schema_of::<system::ImportParams>(),
            schema_of::<system::ImportResult>(),
        ),
        (
            "forge.writeback",
            schema_of::<system::SetWriteBackParams>(),
            schema_of::<project::ProjectResult>(),
        ),
        (
            "forge.link",
            schema_of::<system::ForgeLinkParams>(),
            schema_of::<system::ForgeLinkResult>(),
        ),
        (
            "forge.publish",
            schema_of::<system::ForgePublishParams>(),
            schema_of::<system::ForgePublishResult>(),
        ),
        (
            "forge.repos",
            schema_of::<system::ForgeReposParams>(),
            schema_of::<system::ForgeReposResult>(),
        ),
        (
            "forge.import",
            schema_of::<system::ForgeImportParams>(),
            schema_of::<project::ImportProjectResult>(),
        ),
        (
            "forge.actions",
            schema_of::<system::WriteBackListParams>(),
            schema_of::<system::WriteBackListResult>(),
        ),
        (
            "events.list",
            schema_of::<system::ListEventsParams>(),
            schema_of::<system::EventListResult>(),
        ),
        (
            "notifications",
            schema_of::<system::NotificationsParams>(),
            schema_of::<system::NotificationsResult>(),
        ),
        (
            "deployed.versions",
            schema_of::<system::DeployedVersionsParams>(),
            schema_of::<system::DeployedVersionsResult>(),
        ),
        (
            "usage.board",
            schema_of::<caught_up::UsageBoardParams>(),
            schema_of::<caught_up::UsageBoardResult>(),
        ),
        (
            "inbox.list",
            schema_of::<system::InboxListParams>(),
            schema_of::<system::InboxListResult>(),
        ),
        (
            "inbox.archive",
            schema_of::<system::InboxArchiveParams>(),
            schema_of::<system::InboxTriageResult>(),
        ),
        (
            "inbox.snooze",
            schema_of::<system::InboxSnoozeParams>(),
            schema_of::<system::InboxTriageResult>(),
        ),
        (
            "inbox.restore",
            schema_of::<system::InboxRestoreParams>(),
            schema_of::<system::InboxTriageResult>(),
        ),
        (
            "charter.get",
            schema_of::<caught_up::CharterGetParams>(),
            schema_of::<caught_up::CharterGetResult>(),
        ),
        (
            "charter.set",
            schema_of::<caught_up::CharterSetParams>(),
            schema_of::<caught_up::CharterSetResult>(),
        ),
        (
            "audit.list",
            schema_of::<system::ListAuditParams>(),
            schema_of::<system::AuditListResult>(),
        ),
    ]
}

/// The schema `schemars` derives for `T`, shaped as pydantic's is: no draft
/// marker at the root, and keys in the order the dump records them.
fn schema_of<T: JsonSchema>() -> serde_json::Value {
    let mut value =
        serde_json::to_value(schemars::schema_for!(T)).expect("a derived schema is JSON");
    if let Some(object) = value.as_object_mut() {
        object.remove("$schema");
    }
    pydantic_shape(&mut value);
    canonical(value)
}

/// Reshapes `schemars`' output into pydantic's dialect, the one the registry
/// records: a nullable type is an `anyOf` with `null`, an integer has no
/// `format`, and every named property and definition has a title. A property
/// that is a `$ref` gets none, as pydantic's is not titled either, and a
/// title set on the type stays.
fn pydantic_shape(value: &mut serde_json::Value) {
    use serde_json::{json, Value};
    match value {
        Value::Object(map) => {
            nullable_as_any_of(map);
            if let Some(Value::String(format)) = map.get("format") {
                // pydantic emits no format for an integer or a float.
                if format.starts_with("int") || format.starts_with("uint") || format == "double" {
                    map.remove("format");
                }
            }
            if let Some(Value::Object(definitions)) = map.get_mut("$defs") {
                for (name, schema) in definitions.iter_mut() {
                    if let Value::Object(definition) = schema {
                        definition
                            .entry("title")
                            .or_insert_with(|| Value::String(name.clone()));
                    }
                }
            }
            if let Some(Value::Object(properties)) = map.get_mut("properties") {
                for (name, schema) in properties.iter_mut() {
                    if let Value::Object(property) = schema {
                        if !refers_to_definition(property) && !property.contains_key("title") {
                            property.insert("title".into(), Value::String(pydantic_title(name)));
                        }
                    }
                }
            }
            // `default_factory=list` records no default, where serde's
            // `default` always does: an empty collection default is that
            // factory, so it is dropped unless the field asks to keep it.
            // Done on every object, including one nested under `$defs`.
            let keep_default = map.remove("x-keep-default").is_some();
            let empty =
                map.get("default") == Some(&json!([])) || map.get("default") == Some(&json!({}));
            if !keep_default && empty {
                map.remove("default");
            }
            for child in map.values_mut() {
                pydantic_shape(child);
            }
        }
        Value::Array(items) => items.iter_mut().for_each(pydantic_shape),
        _ => {}
    }
}

/// Whether a schema is a reference to a definition, or an optional one. The
/// title pydantic gives a field is left off these.
fn refers_to_definition(property: &serde_json::Map<String, serde_json::Value>) -> bool {
    use serde_json::Value;
    property.contains_key("$ref")
        || matches!(property.get("anyOf"), Some(Value::Array(branches))
            if branches.iter().any(|branch| branch.get("$ref").is_some()))
}

/// Rewrites a nullable schema as pydantic lays out `T | None`: an `anyOf` of
/// the value's branch and `null`. The constraints go on the value's branch;
/// the title, description and default stay beside the `anyOf`.
fn nullable_as_any_of(map: &mut serde_json::Map<String, serde_json::Value>) {
    use serde_json::Value;
    let kinds = match map.get("type") {
        Some(Value::Array(types)) if types.iter().any(|t| t == "null") => types
            .iter()
            .filter(|t| *t != "null")
            .cloned()
            .collect::<Vec<Value>>(),
        _ => Vec::new(),
    };
    let nullable_enum =
        matches!(map.get("enum"), Some(Value::Array(values)) if values.iter().any(Value::is_null));
    if kinds.is_empty() && !nullable_enum {
        return;
    }
    let mut branch = serde_json::Map::new();
    map.remove("type");
    match kinds.len() {
        0 => {}
        1 => {
            branch.insert("type".to_string(), kinds[0].clone());
        }
        _ => {
            branch.insert("type".to_string(), Value::Array(kinds));
        }
    }
    if let Some(Value::Array(values)) = map.remove("enum") {
        let kept: Vec<Value> = values.into_iter().filter(|v| !v.is_null()).collect();
        branch.insert("enum".to_string(), Value::Array(kept));
    }
    for key in [
        "minLength",
        "maxLength",
        "pattern",
        "minimum",
        "maximum",
        "format",
        "items",
        "exclusiveMinimum",
        "exclusiveMaximum",
        "minItems",
        "maxItems",
    ] {
        if let Some(value) = map.remove(key) {
            branch.insert(key.to_string(), value);
        }
    }
    let mut null = serde_json::Map::new();
    null.insert("type".to_string(), Value::String("null".to_string()));
    map.insert(
        "anyOf".to_string(),
        Value::Array(vec![Value::Object(branch), Value::Object(null)]),
    );
}

/// `str.title()` of a snake-case field name, as pydantic titles a field.
fn pydantic_title(name: &str) -> String {
    name.split('_')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first
                    .to_uppercase()
                    .chain(chars.as_str().to_lowercase().chars())
                    .collect(),
                None => String::new(),
            }
        })
        .collect::<Vec<String>>()
        .join(" ")
}

/// Sorts the keys of every schema node, as pydantic's dump does, and keeps
/// the `properties` map in declaration order, which pydantic does too. The
/// dump is compared text-for-text, so the order is part of what is checked.
fn canonical(value: serde_json::Value) -> serde_json::Value {
    use serde_json::{Map, Value};
    match value {
        Value::Object(map) => {
            let mut entries: Vec<(String, Value)> = map
                .into_iter()
                .map(|(key, child)| {
                    // A `default` is a value, not a schema: it keeps the order
                    // serde serialised it in, which is the declaration order.
                    let child = match key.as_str() {
                        "properties" => keep_order(child),
                        "default" => child,
                        _ => canonical(child),
                    };
                    (key, child)
                })
                .collect();
            entries.sort_by(|left, right| left.0.cmp(&right.0));
            Value::Object(entries.into_iter().collect::<Map<String, Value>>())
        }
        Value::Array(items) => Value::Array(items.into_iter().map(canonical).collect()),
        other => other,
    }
}

/// A `properties` map: each property's schema is canonical, the names stay
/// in the order the fields were declared.
fn keep_order(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => serde_json::Value::Object(
            map.into_iter()
                .map(|(name, schema)| (name, canonical(schema)))
                .collect(),
        ),
        other => other,
    }
}

/// `int` in Python is `"type": "integer"` with no format. `schemars` would
/// add `"format": "int64"` or `"uint32"` to a Rust integer, which pydantic
/// does not emit, so every integer field names this instead.
pub(super) fn integer(_: &mut SchemaGenerator) -> Schema {
    json_schema!({ "type": "integer" })
}

/// pydantic's `datetime`: a string in the `date-time` format.
pub(super) fn date_time(_: &mut SchemaGenerator) -> Schema {
    json_schema!({ "type": "string", "format": "date-time" })
}

/// pydantic's `float`: a JSON number, with no format.
pub(super) fn number(_: &mut SchemaGenerator) -> Schema {
    json_schema!({ "type": "number" })
}

/// pydantic's `dict[str, object]`: an object of any values.
pub(super) fn open_object(_: &mut SchemaGenerator) -> Schema {
    json_schema!({ "type": "object", "additionalProperties": true })
}

/// `dict[str, str]`: an object whose values are strings. No description, so a
/// field's doc comment is not copied onto the schema.
pub(super) fn string_map(_: &mut SchemaGenerator) -> Schema {
    json_schema!({ "type": "object", "additionalProperties": { "type": "string" } })
}

/// `str | None` with no default: a string, or null, and required.
pub(super) fn optional_string(_: &mut SchemaGenerator) -> Schema {
    json_schema!({ "anyOf": [{ "type": "string" }, { "type": "null" }] })
}

/// `dict[str, object] | None`: an object of any values, or null.
pub(super) fn optional_open_object(_: &mut SchemaGenerator) -> Schema {
    json_schema!({ "anyOf": [{ "type": "object", "additionalProperties": true }, { "type": "null" }] })
}

/// `datetime | None`: a `date-time` string, or null.
pub(super) fn optional_date_time(_: &mut SchemaGenerator) -> Schema {
    json_schema!({ "anyOf": [{ "type": "string", "format": "date-time" }, { "type": "null" }] })
}

fn not_configured() -> String {
    "not_configured".to_string()
}

// `init` and `migrate` take nothing. The data directory comes from configuration.

/// `init` takes nothing: the data directory comes from configuration.
#[derive(Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("properties" = {}))]
pub struct InitParams {}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InitResult {
    #[schemars(title = "Instance Id")]
    pub instance_id: String,
    #[schemars(title = "Data Dir")]
    pub data_dir: String,
    #[schemars(title = "Created")]
    pub created: bool,
    #[schemars(title = "Declared Schema Version", schema_with = "integer")]
    pub declared_schema_version: i64,
    #[schemars(title = "Observed Schema Version", schema_with = "integer")]
    pub observed_schema_version: i64,
    #[schemars(title = "Migrations Applied")]
    pub migrations_applied: Vec<String>,
    // What became of `bootstrap_core_token_file`. Plain comments, not doc
    // comments: pydantic's `#:` comments do not reach the schema.
    #[serde(default = "not_configured")]
    #[schemars(title = "Bootstrap Core Token", default = "not_configured")]
    pub bootstrap_core_token: String,
    // The same, for `bootstrap_agent_token_file`.
    #[serde(default = "not_configured")]
    #[schemars(title = "Bootstrap Agent Token", default = "not_configured")]
    pub bootstrap_agent_token: String,
}

/// `migrate` takes nothing: the data directory comes from configuration.
#[derive(Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("properties" = {}))]
pub struct MigrateParams {}

/// What `migrate` moved, and whether anything is still behind.
///
/// Reports the expected versions beside the applied ones for the same reason
/// `/health/ready` does: the applied number alone cannot answer
/// "is this instance current?", which is the only question the operator
/// running this has.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MigrateResult {
    #[schemars(title = "Data Dir")]
    pub data_dir: String,
    #[schemars(title = "Declared Schema Version", schema_with = "integer")]
    pub declared_schema_version: i64,
    #[schemars(title = "Observed Schema Version", schema_with = "integer")]
    pub observed_schema_version: i64,
    #[schemars(title = "Declared Schema Expected", schema_with = "integer")]
    pub declared_schema_expected: i64,
    #[schemars(title = "Observed Schema Expected", schema_with = "integer")]
    pub observed_schema_expected: i64,
    #[schemars(title = "Migrations Applied")]
    pub migrations_applied: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PruneParams {
    #[schemars(title = "Reason", length(min = 1))]
    pub reason: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PruneResult {
    #[schemars(title = "Removed", schema_with = "integer")]
    pub removed: i64,
    #[schemars(title = "Kept Latest", schema_with = "integer")]
    pub kept_latest: i64,
    #[schemars(title = "Kept Referenced", schema_with = "integer")]
    pub kept_referenced: i64,
    #[schemars(title = "Horizon Days", schema_with = "integer")]
    pub horizon_days: i64,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RemoveUserParams {
    #[schemars(title = "Username", length(min = 1))]
    pub username: String,
    #[schemars(title = "Reason", length(min = 1))]
    pub reason: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RemoveUserResult {
    #[schemars(title = "Username")]
    pub username: String,
    #[schemars(title = "Sessions Revoked", schema_with = "integer")]
    pub sessions_revoked: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derived_names_are_exactly_the_listed_set() {
        let names: Vec<&str> = derived_schemas().iter().map(|(name, _, _)| *name).collect();
        assert_eq!(names, DERIVED);
    }

    #[test]
    fn init_params_takes_nothing() {
        let (_, params, _) = derived_schemas().remove(0);
        assert_eq!(params["title"], "InitParams");
        assert_eq!(params["additionalProperties"], false);
        assert_eq!(
            params["description"],
            "`init` takes nothing: the data directory comes from configuration."
        );
        assert!(params.get("$schema").is_none());
    }

    #[test]
    fn a_defaulted_field_is_not_required() {
        let (_, _, result) = derived_schemas().remove(0);
        let required = result["required"].as_array().expect("required list");
        assert!(!required.iter().any(|name| name == "bootstrap_core_token"));
        assert_eq!(
            result["properties"]["bootstrap_core_token"]["default"],
            "not_configured"
        );
    }
}
