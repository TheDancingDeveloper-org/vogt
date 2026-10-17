//! The chat operations' parameter and result models.
//!
//! Ports the `Chat*` classes in `src/vogt/application/models.py`. Field
//! names, defaults, bounds and descriptions are the Python ones; the
//! schemas `schemars` derives from these types are what `registry.dump`
//! reports.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::integer;

fn wait_s_default_zero() -> i64 {
    0
}

fn wait_s_default_sixty() -> i64 {
    60
}

fn tail_default() -> i64 {
    200
}

fn limit_default() -> i64 {
    50
}

fn archived_default() -> bool {
    true
}

/// Open chats (default), archived ones, or both.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[schemars(inline)]
pub enum ChatArchivedFilter {
    #[serde(rename = "false")]
    Open,
    #[serde(rename = "true")]
    Archived,
    #[serde(rename = "all")]
    All,
}

fn archived_filter_default() -> ChatArchivedFilter {
    ChatArchivedFilter::Open
}

/// One line of a chat. Agent text and tool results are untrusted data.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ChatEntryView {
    #[schemars(schema_with = "integer")]
    pub seq: i64,
    pub at: String,
    /// user, assistant, tool-call, tool-result, notice, error or approval.
    pub kind: String,
    pub text: String,
    #[serde(default)]
    pub tool_name: Option<String>,
    #[serde(default)]
    pub tool_use_id: Option<String>,
    #[serde(default)]
    pub is_error: bool,
    /// An error the person can retry by sending the message again.
    #[serde(default)]
    pub retryable: bool,
    #[serde(default)]
    pub by: Option<String>,
}

/// A tool call waiting for (or decided by) a person.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ChatApprovalView {
    pub id: String,
    pub tool_name: String,
    /// What the call would do: a command, a path.
    pub summary: String,
    /// `gate` (Vogt's pre-tool gate) or `driver` (the agent asked).
    pub source: String,
    /// pending, allowed, denied or expired.
    pub status: String,
    pub requested_at: String,
    pub expires_at: String,
    #[serde(default)]
    pub decided_by: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ChatSummaryView {
    pub id: String,
    pub title: String,
    pub driver: String,
    /// The model the next turn runs on; null = default.
    #[serde(default)]
    pub model: Option<String>,
    pub creator: String,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub archived: bool,
    #[serde(default)]
    pub work_item: Option<String>,
    /// The engine session the chat continues in; it takes no more messages.
    #[serde(default)]
    pub promoted_session: Option<String>,
    /// idle, running or awaiting-approval.
    pub state: String,
    /// Whether the agent process is running now.
    pub live: bool,
    #[serde(default)]
    pub message_count: i64,
    #[serde(default)]
    pub preview: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ChatListParams {
    /// Full-text terms, matched against titles and everything said.
    #[serde(default)]
    pub q: Option<String>,
    /// Open chats (default), archived ones, or both.
    #[serde(default = "archived_filter_default")]
    #[schemars(extend("default" = "false"))]
    pub archived: ChatArchivedFilter,
    #[serde(default = "limit_default")]
    #[schemars(schema_with = "integer", range(min = 1, max = 500), extend("default" = 50))]
    pub limit: i64,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ChatListResult {
    pub chats: Vec<ChatSummaryView>,
    /// False when the engine has chats off; the list is then empty.
    pub available: bool,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ChatGetParams {
    #[schemars(description = "The chat's id (a UUID, from chat.list or chat.create).")]
    pub id: String,
    /// The newest this many entries.
    #[serde(default = "tail_default")]
    #[schemars(schema_with = "integer", range(min = 1, max = 5000), extend("default" = 200))]
    pub tail: i64,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ChatDetailResult {
    pub chat: ChatSummaryView,
    pub entries: Vec<ChatEntryView>,
    /// Approvals still waiting for a person.
    pub approvals: Vec<ChatApprovalView>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ChatCreateParams {
    /// The first message, sent as soon as it exists.
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    /// A model the engine offers (/api/config `chat`); default: its first.
    #[serde(default)]
    pub model: Option<String>,
    /// e.g. WI-7
    #[serde(default)]
    pub work_item: Option<String>,
    /// Wait up to this long for the first reply.
    #[serde(default = "wait_s_default_zero")]
    #[schemars(schema_with = "integer", range(min = 0, max = 300), extend("default" = 0))]
    pub wait_s: i64,
    /// Why this write is being made (audited).
    #[schemars(length(min = 1))]
    pub reason: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ChatSendParams {
    #[schemars(description = "The chat's id (a UUID, from chat.list or chat.create).")]
    pub id: String,
    #[schemars(length(min = 1, max = 65536))]
    pub text: String,
    /// Wait up to this long for the turn to end and return what it said. A turn parked on an approval waits for a person.
    #[serde(default = "wait_s_default_sixty")]
    #[schemars(schema_with = "integer", range(min = 0, max = 300), extend("default" = 60))]
    pub wait_s: i64,
    /// Why this write is being made (audited).
    #[schemars(length(min = 1))]
    pub reason: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ChatSendResult {
    pub chat: ChatSummaryView,
    /// What the chat recorded from this message on.
    pub entries: Vec<ChatEntryView>,
    /// Whether the turn ended within the wait.
    pub finished: bool,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ChatDecideParams {
    #[schemars(description = "The chat's id (a UUID, from chat.list or chat.create).")]
    pub id: String,
    pub approval_id: String,
    pub allow: bool,
    /// Told to the agent with a denial.
    #[serde(default)]
    pub message: Option<String>,
    /// Why this write is being made (audited).
    #[schemars(length(min = 1))]
    pub reason: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ChatSetModelParams {
    #[schemars(description = "The chat's id (a UUID, from chat.list or chat.create).")]
    pub id: String,
    /// A model the engine offers, or `default` for the driver's own.
    pub model: String,
    /// Why this write is being made (audited).
    #[schemars(length(min = 1))]
    pub reason: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ChatIdParams {
    #[schemars(description = "The chat's id (a UUID, from chat.list or chat.create).")]
    pub id: String,
    /// Why this write is being made (audited).
    #[schemars(length(min = 1))]
    pub reason: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ChatArchiveParams {
    #[schemars(description = "The chat's id (a UUID, from chat.list or chat.create).")]
    pub id: String,
    #[serde(default = "archived_default")]
    #[schemars(extend("default" = true))]
    pub archived: bool,
    /// Why this write is being made (audited).
    #[schemars(length(min = 1))]
    pub reason: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ChatPromoteParams {
    #[schemars(description = "The chat's id (a UUID, from chat.list or chat.create).")]
    pub id: String,
    /// Where the session opens, inside the engine's workspace.
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub work_item: Option<String>,
    /// Why this write is being made (audited).
    #[schemars(length(min = 1))]
    pub reason: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ChatPromoteResult {
    pub chat: ChatSummaryView,
    /// The terminal session now holding the conversation.
    pub engine_session_id: String,
    pub session_name: String,
}
