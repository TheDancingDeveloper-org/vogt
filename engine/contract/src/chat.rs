//! Quick chat (WI-1097): a persistent text conversation with an agent CLI
//! driven over its stream-json protocol rather than in a terminal.
//!
//! A chat is not a session. It has no PTY, no scrollback and no activity
//! heuristic: the agent says when a turn ends (`result`), asks for approval
//! in-band, and is relaunched with `--resume` when the next message arrives
//! after its process was stopped for being idle. Chats are kept until a
//! person deletes the engine's state; archiving only hides one from the
//! default list.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// One model a chat may run, as the deployment configured it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatModel {
    /// The id the driver is asked for (`--model` / `set_model`).
    pub id: String,
    /// What the picker shows.
    pub label: String,
}

/// A program that can drive a chat, and the models it offers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatDriver {
    /// `klaudia`: the only driver this engine knows how to speak to.
    pub name: String,
    pub label: String,
    /// Empty means the picker offers only the driver's own default model.
    #[serde(default)]
    pub models: Vec<ChatModel>,
}

/// Advertised on `/api/config` as `chat` when chats are available.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatConfigInfo {
    pub drivers: Vec<ChatDriver>,
}

/// What a chat is doing now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ChatState {
    /// No turn is running. The process may still be alive, or stopped for
    /// being idle; either way the next message is answered.
    #[default]
    Idle,
    /// A turn is running.
    Running,
    /// A turn is parked on an approval only a person can answer.
    AwaitingApproval,
}

/// A request for a person's decision before a tool runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatApproval {
    pub id: String,
    pub tool_name: String,
    /// A short, readable rendering of what the tool would do.
    pub summary: String,
    /// `gate` (Vogt's own pre-tool gate) or `driver` (the agent CLI asked).
    pub source: String,
    /// `pending`, `allowed`, `denied` or `expired`.
    pub status: String,
    pub requested_at: String,
    /// When a pending approval is denied for want of an answer.
    pub expires_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_by: Option<String>,
}

/// A chat as listed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatSummary {
    pub id: Uuid,
    pub title: String,
    pub driver: String,
    /// The model the next turn runs on; `None` is the driver's default.
    #[serde(default)]
    pub model: Option<String>,
    /// Who started it: a core actor's `identity_ref`, or `primary` for the
    /// break-glass token.
    pub creator: String,
    pub created_at: String,
    pub updated_at: String,
    pub archived: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_item: Option<String>,
    /// The session the chat was promoted into; a promoted chat takes no
    /// further messages, because that session now holds the conversation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub promoted_session: Option<Uuid>,
    pub state: ChatState,
    /// Whether the driver process is running now.
    pub live: bool,
    pub message_count: u64,
    /// The last thing said, for the list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
}

/// One line of a chat's transcript.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatEntry {
    pub seq: i64,
    pub at: String,
    /// `user`, `assistant`, `tool-call`, `tool-result`, `notice`, `error`,
    /// `turn-end` or `approval`.
    pub kind: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_error: bool,
    /// An `error` the person can retry by sending the message again (a
    /// provider refusal mid-turn, WI-1007), as opposed to one that needs
    /// something fixed first.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub retryable: bool,
    /// Who sent a `user` entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
}

/// A chat with its transcript.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatDetail {
    #[serde(flatten)]
    pub chat: ChatSummary,
    pub entries: Vec<ChatEntry>,
    /// Approvals still waiting for a person.
    pub approvals: Vec<ChatApproval>,
}

/// `POST /api/chats`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChatCreateRequest {
    #[serde(default)]
    pub title: Option<String>,
    /// Defaults to the first configured driver.
    #[serde(default)]
    pub driver: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    /// The first message, sent as soon as the chat exists.
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub work_item: Option<String>,
    /// Who the chat is for, when vogt-core relays a request: honoured only
    /// from the core's own credential, which has authenticated that actor.
    #[serde(default)]
    pub creator: Option<String>,
}

/// `POST /api/chats/{id}/messages`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChatSendRequest {
    pub text: String,
    /// Wait up to this many seconds (at most 300) for the turn to end and
    /// return what it produced. 0 or absent returns as soon as it is sent.
    #[serde(default)]
    pub wait_secs: Option<u32>,
    /// Who sent it, when vogt-core relays (see [`ChatCreateRequest::creator`]).
    #[serde(default)]
    pub by: Option<String>,
}

/// What a send returns.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatSendResult {
    pub chat: ChatSummary,
    /// Everything the chat recorded from this message on, while waiting.
    pub entries: Vec<ChatEntry>,
    /// Whether the turn ended within the wait.
    pub finished: bool,
}

/// `POST /api/chats/{id}/approvals/{approval_id}`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChatDecisionRequest {
    pub allow: bool,
    /// Told to the agent with a denial.
    #[serde(default)]
    pub message: Option<String>,
    /// Whether the principal behind a relayed request is a person, as for
    /// `AnswerRequest::person`: honoured only from the core's credential and
    /// the break-glass token.
    #[serde(default)]
    pub person: Option<bool>,
}

/// `POST /api/chats/{id}/model`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChatModelRequest {
    /// A configured model id, or `default` for the driver's own.
    pub model: String,
}

/// `POST /api/chats/{id}/archive`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChatArchiveRequest {
    pub archived: bool,
}

/// `POST /api/chats/{id}/promote`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChatPromoteRequest {
    /// Where the session opens, inside the workspace. Defaults to the
    /// engine's default directory.
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    /// The session template to continue the conversation in. Defaults to the
    /// one the chat's driver was launched from.
    #[serde(default)]
    pub template: Option<String>,
    #[serde(default)]
    pub work_item: Option<String>,
}

/// What a promotion returns.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatPromoteResult {
    pub chat: ChatSummary,
    pub session: crate::SessionSummary,
}

/// `GET /api/chats/{id}/events`, one per SSE `data:` line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum ChatEvent {
    /// A transcript entry was recorded.
    Entry { entry: ChatEntry },
    /// Live status a client may show and drop: a long tool's progress line.
    Progress {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_name: Option<String>,
        text: String,
    },
    /// An approval was raised or decided.
    Approval { approval: ChatApproval },
    /// The chat's summary changed (state, model, title, archive, promotion).
    Chat { chat: ChatSummary },
    /// The client fell behind and missed events; re-read the chat.
    Lagged { skipped: u64 },
}
