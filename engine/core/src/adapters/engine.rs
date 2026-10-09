//! The session engine client. Ports `src/vogt/adapters/engine/client.py`.
//!
//! A deliberately partial view of the engine's wire: Vogt reads the fields it
//! reasons about and leaves the rest the engine's business. The token is read
//! from a file and never from argv or a URL, and the transport is injectable so
//! a test asserts what was sent without an engine.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use serde_json::{Map, Value};

use crate::errors::VogtError;

pub const USER_AGENT: &str = "vogt";
/// The default request timeout (`DEFAULT_TIMEOUT_SECONDS`).
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(20);
/// A wait runs its full course, so the HTTP timeout is the wait plus this.
pub const WAIT_TIMEOUT_MARGIN: Duration = Duration::from_secs(15);
/// Hibernate gives the agent a few seconds to exit cleanly.
pub const HIBERNATE_TIMEOUT_MARGIN: Duration = Duration::from_secs(10);

/// How the client actually talks, so tests never need an engine. The body and
/// method are part of the call because a test of `create_session` has to assert
/// the spec that was sent.
pub type Transport = Box<dyn Fn(&str, &BTreeMap<String, String>, &[u8], &str) -> (u16, Vec<u8>)>;

/// Access to one session engine.
pub struct EngineClient {
    base_url: String,
    token: Option<String>,
    transport: Option<Transport>,
    timeout: Duration,
    /// Which engine did not answer, kept out of the token's reach.
    label: String,
}

impl EngineClient {
    pub fn new(
        base_url: impl Into<String>,
        token: Option<String>,
        transport: Option<Transport>,
    ) -> Self {
        Self {
            base_url: base_url.into().trim().trim_end_matches('/').to_string(),
            token,
            transport,
            timeout: DEFAULT_TIMEOUT,
            label: "engine".to_string(),
        }
    }

    /// A client, or `None` when no engine is configured. `None` is an ordinary
    /// answer: a Vogt with no engine says so rather than failing like an outage.
    pub fn from_config(url: Option<&str>, token_file: Option<&Path>) -> Option<Self> {
        let url = url.map(str::trim).filter(|url| !url.is_empty())?;
        let token = token_file.and_then(|path| {
            std::fs::read_to_string(path.expanduser_lossy())
                .ok()
                .map(|text| text.trim().to_string())
                .filter(|text| !text.is_empty())
        });
        Some(Self::new(url, token, None))
    }

    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
    }

    // -- what Vogt asks of the engine ---------------------------------------

    /// Start a terminal in `cwd`. `cwd` is required even though the engine would
    /// default it: the default is the engine's workspace root, and a session that
    /// opened there when Vogt meant a project's tree would be plausible and wrong.
    pub fn create_session(&self, spec: &CreateSession) -> Result<EngineSession, VogtError> {
        let mut body = Map::new();
        body.insert("name".to_string(), Value::String(spec.name.clone()));
        body.insert("cwd".to_string(), Value::String(spec.cwd.clone()));
        if let Some(command) = &spec.command {
            body.insert(
                "command".to_string(),
                serde_json::to_value(command).unwrap_or(Value::Null),
            );
        }
        if let Some(template) = &spec.template {
            body.insert("template".to_string(), Value::String(template.clone()));
        }
        if let Some(prompt) = &spec.prompt {
            body.insert("prompt".to_string(), Value::String(prompt.clone()));
        }
        if let Some(env) = &spec.env {
            // Pairs, not an object: ordering is the caller's and duplicates show.
            let pairs: Vec<Value> = env
                .iter()
                .map(|(key, value)| serde_json::json!([key, value]))
                .collect();
            body.insert("env".to_string(), Value::Array(pairs));
        }
        // Sent only when asked for, so a session that named neither is the
        // request this client has always made.
        if let Some(model) = &spec.model {
            body.insert("model".to_string(), Value::String(model.clone()));
        }
        if let Some(effort) = &spec.effort {
            body.insert("effort".to_string(), Value::String(effort.clone()));
        }
        if let Some(resume) = &spec.resume {
            body.insert("resume".to_string(), Value::String(resume.clone()));
        }
        if let Some(mode) = &spec.permission_mode {
            if mode != "default" {
                body.insert(
                    "permission_mode".to_string(),
                    Value::String(mode.replace('_', "-")),
                );
            }
        }
        if spec.autopilot {
            body.insert("autopilot".to_string(), Value::Bool(true));
        }
        if spec.role != "worker" {
            body.insert("role".to_string(), Value::String(spec.role.clone()));
        }
        if let Some(work_item) = &spec.work_item {
            body.insert("work_item".to_string(), Value::String(work_item.clone()));
        }
        Ok(EngineSession::from_payload(&self.call(
            "/api/sessions",
            "POST",
            Some(&Value::Object(body)),
            false,
            None,
        )?))
    }

    pub fn list_sessions(&self) -> Result<Vec<EngineSession>, VogtError> {
        let payload = self.call("/api/sessions", "GET", None, false, None)?;
        Ok(rows(&payload).map(EngineSession::from_payload).collect())
    }

    /// Every live and hibernated session with its screen's last lines, in one
    /// request. `Ok(None)` from an engine that predates the route.
    pub fn sweep_sessions(
        &self,
        screen_lines: i64,
    ) -> Result<Option<Vec<EngineSweepEntry>>, VogtError> {
        let payload = self.call(
            &format!("/api/sessions/sweep?screen_lines={screen_lines}"),
            "GET",
            None,
            true,
            None,
        )?;
        Ok(match &payload {
            Value::Null => None,
            other => Some(rows(other).map(EngineSweepEntry::from_payload).collect()),
        })
    }

    /// One session, or `Ok(None)` if the engine has forgotten it. Forgetting is
    /// normal: a session the engine restarted without is gone.
    pub fn get_session(&self, session_id: &str) -> Result<Option<EngineSession>, VogtError> {
        let payload = self.call(&session_path(session_id, ""), "GET", None, true, None)?;
        Ok(match &payload {
            Value::Object(object) => {
                let summary = object
                    .get("summary")
                    .filter(|value| value.is_object())
                    .unwrap_or(&payload);
                Some(EngineSession::from_payload(summary))
            }
            _ => None,
        })
    }

    /// Stop a session. `Ok(false)` when the engine no longer had it. `reason`
    /// and `by` are recorded before the kill, so the exit reads `stopped`.
    pub fn kill_session(
        &self,
        session_id: &str,
        reason: Option<&str>,
        by: Option<&str>,
    ) -> Result<bool, VogtError> {
        let mut body = Map::new();
        if let Some(reason) = reason.filter(|text| !text.is_empty()) {
            body.insert("reason".to_string(), Value::String(reason.to_string()));
        }
        if let Some(by) = by.filter(|text| !text.is_empty()) {
            body.insert("by".to_string(), Value::String(by.to_string()));
        }
        let payload = self.call(
            &session_path(session_id, "/kill"),
            "POST",
            Some(&Value::Object(body)),
            true,
            None,
        )?;
        Ok(!payload.is_null())
    }

    pub fn archived_session(
        &self,
        session_id: &str,
    ) -> Result<Option<EngineArchivedSession>, VogtError> {
        let payload = self.call(
            &format!("/api/history/{}", quote(session_id)),
            "GET",
            None,
            true,
            None,
        )?;
        Ok(payload
            .as_object()
            .map(|_| EngineArchivedSession::from_payload(&payload)))
    }

    pub fn list_agent_tasks(&self) -> Result<Vec<EngineAgentTask>, VogtError> {
        let payload = self.call("/api/agent-tasks", "GET", None, false, None)?;
        Ok(rows(&payload).map(EngineAgentTask::from_payload).collect())
    }

    pub fn history_sessions(
        &self,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<EngineHistorySession>, VogtError> {
        let payload = self.call(
            &format!("/api/history/sessions?limit={limit}&offset={offset}"),
            "GET",
            None,
            false,
            None,
        )?;
        Ok(rows(&payload)
            .map(EngineHistorySession::from_payload)
            .collect())
    }

    pub fn search_history(
        &self,
        query: &str,
        limit: i64,
        include_live: bool,
    ) -> Result<Vec<EngineHistoryMatch>, VogtError> {
        let live = if include_live { "true" } else { "false" };
        let payload = self.call(
            &format!(
                "/api/history/search?q={}&limit={limit}&include_live={live}",
                quote(query)
            ),
            "GET",
            None,
            false,
            None,
        )?;
        Ok(rows(&payload)
            .map(EngineHistoryMatch::from_payload)
            .collect())
    }

    pub fn history_log(
        &self,
        session_id: &str,
        tail_bytes: i64,
        strip_ansi: bool,
    ) -> Result<Option<EngineSessionLog>, VogtError> {
        let ansi = if strip_ansi { "true" } else { "false" };
        let payload = self.call(
            &format!(
                "/api/history/{}/log?tail_bytes={tail_bytes}&strip_ansi={ansi}",
                quote(session_id)
            ),
            "GET",
            None,
            true,
            None,
        )?;
        Ok(payload
            .as_object()
            .map(|_| EngineSessionLog::from_payload(&payload)))
    }

    /// Write `text` to a session's PTY. `submit` appends a carriage return.
    /// `Ok(false)` when the engine has no such session.
    pub fn send_input(
        &self,
        session_id: &str,
        text: &str,
        submit: bool,
    ) -> Result<bool, VogtError> {
        let payload = self.call(
            &session_path(session_id, "/input"),
            "POST",
            Some(&serde_json::json!({"text": text, "submit": submit})),
            true,
            None,
        )?;
        Ok(!payload.is_null())
    }

    pub fn session_screen(
        &self,
        session_id: &str,
        scrollback_lines: i64,
    ) -> Result<Option<EngineScreen>, VogtError> {
        let query = if scrollback_lines > 0 {
            format!("?scrollback_lines={scrollback_lines}")
        } else {
            String::new()
        };
        let payload = self.call(
            &session_path(session_id, &format!("/screen{query}")),
            "GET",
            None,
            true,
            None,
        )?;
        Ok(payload
            .as_object()
            .map(|_| EngineScreen::from_payload(&payload)))
    }

    /// An opencode session's last `n` replies: `(conversation_id, [(text, at)])`,
    /// oldest first. `Ok(None)` on a 404.
    #[allow(clippy::type_complexity)]
    pub fn session_replies(
        &self,
        session_id: &str,
        n: i64,
    ) -> Result<Option<(Option<String>, Vec<(String, Option<String>)>)>, VogtError> {
        let payload = self.call(
            &session_path(session_id, &format!("/replies?n={n}")),
            "GET",
            None,
            true,
            None,
        )?;
        let Some(object) = payload.as_object() else {
            return Ok(None);
        };
        let found = object
            .get("replies")
            .and_then(Value::as_array)
            .map(|replies| {
                replies
                    .iter()
                    .filter_map(|reply| {
                        let text = reply.get("text").and_then(Value::as_str)?;
                        Some((text.to_string(), optional_str(reply.get("at"))))
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(Some((optional_str(object.get("conversation_id")), found)))
    }

    /// Block until the session reaches `until` (`ready`, `exited`, `any-change`)
    /// or `timeout` passes. The HTTP timeout is the wait plus a margin.
    pub fn wait_session(
        &self,
        session_id: &str,
        until: &str,
        timeout: Duration,
    ) -> Result<Option<EngineWait>, VogtError> {
        let seconds = timeout.as_secs();
        let payload = self.call(
            &session_path(
                session_id,
                &format!("/wait?until={}&timeout_s={seconds}", quote(until)),
            ),
            "GET",
            None,
            true,
            Some(timeout + WAIT_TIMEOUT_MARGIN),
        )?;
        Ok(payload
            .as_object()
            .map(|_| EngineWait::from_payload(&payload)))
    }

    pub fn set_blocked(
        &self,
        session_id: &str,
        blocked: bool,
        reason: Option<&str>,
        items: &[String],
    ) -> Result<Option<EngineSession>, VogtError> {
        let payload = self.call(
            &session_path(session_id, "/blocked"),
            "POST",
            Some(&serde_json::json!({"blocked": blocked, "reason": reason, "items": items})),
            true,
            None,
        )?;
        Ok(payload
            .as_object()
            .map(|_| EngineSession::from_payload(&payload)))
    }

    /// Rename a session, live or hibernated. `false` when the engine no longer
    /// had it; a name the engine refuses is its 400, said.
    pub fn rename_session(&self, session_id: &str, name: &str) -> Result<bool, VogtError> {
        let payload = self.call(
            &format!("/api/sessions/{}", quote(session_id)),
            "PATCH",
            Some(&serde_json::json!({"name": name})),
            true,
            None,
        )?;
        Ok(!payload.is_null())
    }

    /// Kill a session if it still runs and forget it. `false` when the engine
    /// no longer had it.
    pub fn remove_session(&self, session_id: &str) -> Result<bool, VogtError> {
        let payload = self.call(
            &format!("/api/sessions/{}", quote(session_id)),
            "DELETE",
            None,
            true,
            None,
        )?;
        Ok(!payload.is_null())
    }

    /// Choose an option of the dialog on screen. A dialog that is gone, changed,
    /// or lacks the option is a `Conflict` naming why.
    pub fn answer_session(
        &self,
        session_id: &str,
        option: Option<i64>,
        label: Option<&str>,
        expect_question: Option<&str>,
    ) -> Result<Option<Value>, VogtError> {
        let mut body = Map::new();
        if let Some(option) = option {
            body.insert("option".to_string(), Value::from(option));
        }
        if let Some(label) = label {
            body.insert("label".to_string(), Value::String(label.to_string()));
        }
        if let Some(question) = expect_question {
            body.insert(
                "expect_question".to_string(),
                Value::String(question.to_string()),
            );
        }
        let payload = self.call(
            &session_path(session_id, "/answer"),
            "POST",
            Some(&Value::Object(body)),
            true,
            None,
        )?;
        Ok(payload.as_object().map(|_| payload.clone()))
    }

    pub fn hibernate_session(
        &self,
        session_id: &str,
        reason: Option<&str>,
        allow_shell: bool,
    ) -> Result<Option<EngineSession>, VogtError> {
        let mut body = Map::new();
        body.insert("allow_shell".to_string(), Value::Bool(allow_shell));
        if let Some(reason) = reason.filter(|text| !text.is_empty()) {
            body.insert("reason".to_string(), Value::String(reason.to_string()));
        }
        let payload = self.call(
            &session_path(session_id, "/hibernate"),
            "POST",
            Some(&Value::Object(body)),
            true,
            Some(self.timeout + HIBERNATE_TIMEOUT_MARGIN),
        )?;
        Ok(payload
            .as_object()
            .map(|_| EngineSession::from_payload(&payload.clone())))
    }

    pub fn wake_session(
        &self,
        session_id: &str,
        env: Option<&[(String, String)]>,
    ) -> Result<Option<EngineSession>, VogtError> {
        let mut body = Map::new();
        if let Some(env) = env.filter(|pairs| !pairs.is_empty()) {
            let pairs: Vec<Value> = env.iter().map(|(k, v)| serde_json::json!([k, v])).collect();
            body.insert("env".to_string(), Value::Array(pairs));
        }
        let payload = self.call(
            &session_path(session_id, "/wake"),
            "POST",
            Some(&Value::Object(body)),
            true,
            None,
        )?;
        Ok(payload
            .as_object()
            .map(|_| EngineSession::from_payload(&payload)))
    }

    pub fn keep_awake(
        &self,
        session_id: &str,
        keep_awake: bool,
    ) -> Result<Option<EngineSession>, VogtError> {
        let payload = self.call(
            &session_path(session_id, "/keep-awake"),
            "POST",
            Some(&serde_json::json!({"keep_awake": keep_awake})),
            true,
            None,
        )?;
        Ok(payload
            .as_object()
            .map(|_| EngineSession::from_payload(&payload)))
    }

    pub fn set_role(
        &self,
        session_id: &str,
        role: &str,
    ) -> Result<Option<EngineSession>, VogtError> {
        let payload = self.call(
            &session_path(session_id, "/role"),
            "POST",
            Some(&serde_json::json!({"role": role})),
            true,
            None,
        )?;
        Ok(payload
            .as_object()
            .map(|_| EngineSession::from_payload(&payload)))
    }

    pub fn set_work_item(
        &self,
        session_id: &str,
        work_item: Option<&str>,
    ) -> Result<Option<EngineSession>, VogtError> {
        let payload = self.call(
            &session_path(session_id, "/work-item"),
            "POST",
            Some(&serde_json::json!({"work_item": work_item})),
            true,
            None,
        )?;
        Ok(payload
            .as_object()
            .map(|_| EngineSession::from_payload(&payload)))
    }

    /// Hand the engine a person-approved grant. `NotFound` when it does not know
    /// the session, `Conflict` when it cannot hold the grant, `GrantRefused` for
    /// a project not open to grants, `InvalidRequest` for a field it rejects.
    pub fn apply_grant(&self, session_id: &str, grant: &Value) -> Result<Value, VogtError> {
        let payload = self.call(
            &session_path(session_id, "/grants"),
            "POST",
            Some(grant),
            true,
            None,
        )?;
        if payload.is_null() {
            return Err(VogtError::NotFound(format!(
                "the {} has no session {session_id:?}",
                self.label
            )));
        }
        Ok(if payload.is_object() {
            payload
        } else {
            Value::Object(Map::new())
        })
    }

    /// Drop one grant at the engine; whether it still held it.
    pub fn revoke_grant(&self, session_id: &str, grant_id: &str) -> Result<bool, VogtError> {
        let payload = self.call(
            &format!(
                "{}/{}",
                session_path(session_id, "/grants"),
                quote(grant_id)
            ),
            "DELETE",
            None,
            true,
            None,
        )?;
        Ok(payload
            .get("revoked")
            .and_then(Value::as_bool)
            .unwrap_or(false))
    }

    /// `GET /api/status`: the engine's own operational report.
    pub fn operational_status(&self) -> Result<Value, VogtError> {
        let payload = self.call("/api/status", "GET", None, false, None)?;
        if !payload.is_object() {
            return Err(VogtError::EngineUnavailable(format!(
                "the {} answered GET /api/status with no object",
                self.label
            )));
        }
        Ok(payload)
    }

    // -- quick chats (WI-1097) ------------------------------------------------

    /// The engine's chats, or `None` when it has chats off (404).
    pub fn chat_list(
        &self,
        q: Option<&str>,
        archived: &str,
        limit: i64,
    ) -> Result<Option<Vec<Value>>, VogtError> {
        let mut query = format!("archived={}&limit={limit}", quote(archived));
        if let Some(q) = q.filter(|q| !q.is_empty()) {
            query.push_str(&format!("&q={}", quote(q)));
        }
        let payload = self.call(&format!("/api/chats?{query}"), "GET", None, true, None)?;
        if payload.is_null() {
            return Ok(None);
        }
        Ok(Some(
            payload
                .as_array()
                .map(|rows| rows.iter().filter(|row| row.is_object()).cloned().collect())
                .unwrap_or_default(),
        ))
    }

    pub fn chat_get(&self, chat_id: &str, tail: i64) -> Result<Option<Value>, VogtError> {
        self.chat_call(&format!("/{}?tail={tail}", quote(chat_id)), "GET", None, 0)
    }

    pub fn chat_create(&self, body: &Value, wait_s: i64) -> Result<Option<Value>, VogtError> {
        self.chat_call("", "POST", Some(body), wait_s)
    }

    pub fn chat_send(&self, chat_id: &str, body: &Value) -> Result<Option<Value>, VogtError> {
        let wait = body.get("wait_secs").and_then(Value::as_i64).unwrap_or(0);
        self.chat_call(
            &format!("/{}/messages", quote(chat_id)),
            "POST",
            Some(body),
            wait,
        )
    }

    /// A person's answer to a chat's approval. `person` says whether the
    /// principal behind it is one; the engine refuses anyone else.
    pub fn chat_decide(
        &self,
        chat_id: &str,
        approval_id: &str,
        allow: bool,
        message: Option<&str>,
        person: bool,
    ) -> Result<Option<Value>, VogtError> {
        let mut body = serde_json::json!({"allow": allow, "person": person});
        if let Some(message) = message.filter(|m| !m.is_empty()) {
            body["message"] = Value::String(message.to_string());
        }
        self.chat_call(
            &format!("/{}/approvals/{}", quote(chat_id), quote(approval_id)),
            "POST",
            Some(&body),
            0,
        )
    }

    pub fn chat_set_model(&self, chat_id: &str, model: &str) -> Result<Option<Value>, VogtError> {
        self.chat_call(
            &format!("/{}/model", quote(chat_id)),
            "POST",
            Some(&serde_json::json!({"model": model})),
            0,
        )
    }

    pub fn chat_interrupt(&self, chat_id: &str) -> Result<Option<Value>, VogtError> {
        self.chat_call(
            &format!("/{}/interrupt", quote(chat_id)),
            "POST",
            Some(&serde_json::json!({})),
            0,
        )
    }

    pub fn chat_archive(&self, chat_id: &str, archived: bool) -> Result<Option<Value>, VogtError> {
        self.chat_call(
            &format!("/{}/archive", quote(chat_id)),
            "POST",
            Some(&serde_json::json!({"archived": archived})),
            0,
        )
    }

    pub fn chat_promote(&self, chat_id: &str, body: &Value) -> Result<Option<Value>, VogtError> {
        self.chat_call(
            &format!("/{}/promote", quote(chat_id)),
            "POST",
            Some(body),
            0,
        )
    }

    /// One `/api/chats` call; `None` on a 404 (no such chat, or chats off). A
    /// send that waits for its reply outlasts the ordinary timeout.
    fn chat_call(
        &self,
        suffix: &str,
        method: &str,
        payload: Option<&Value>,
        wait_s: i64,
    ) -> Result<Option<Value>, VogtError> {
        let timeout = (wait_s > 0).then(|| self.timeout + Duration::from_secs(wait_s as u64));
        let answer = self.call(
            &format!("/api/chats{suffix}"),
            method,
            payload,
            true,
            timeout,
        )?;
        if answer.is_null() {
            return Ok(None);
        }
        Ok(Some(if answer.is_object() {
            answer
        } else {
            Value::Object(Map::new())
        }))
    }

    /// Raise `EngineUnavailable` unless the engine answers its liveness probe.
    pub fn healthz(&self) -> Result<(), VogtError> {
        self.call("/healthz", "GET", None, false, None).map(|_| ())
    }

    pub fn agent_clis(&self, upstream: bool) -> Result<Value, VogtError> {
        let query = if upstream { "?upstream=true" } else { "" };
        let payload = self.call(&format!("/api/agent-clis{query}"), "GET", None, false, None)?;
        Ok(if payload.is_object() {
            payload
        } else {
            Value::Object(Map::new())
        })
    }

    /// Ask the engine to make `version` of `tool` current. Its refusals are
    /// mapped to Vogt's errors rather than flattened into "did not answer".
    pub fn update_agent_cli(&self, tool: &str, version: &str) -> Result<Value, VogtError> {
        let url = format!("{}/api/agent-clis/{}", self.base_url, quote_strict(tool));
        let mut headers = base_headers();
        headers.insert("Content-Type".to_string(), "application/json".to_string());
        if let Some(token) = &self.token {
            headers.insert("Authorization".to_string(), format!("Bearer {token}"));
        }
        let body = serde_json::to_vec(&serde_json::json!({"version": version})).unwrap_or_default();
        let (status, response) = self.fetch(&url, &headers, &body, "POST", None)?;
        let text = String::from_utf8_lossy(&response);
        let said = engine_error_text(&text);
        match status {
            400 => Err(VogtError::InvalidRequest(
                nonempty(&said).unwrap_or_else(|| format!("the {} refused the version {version:?}", self.label)),
            )),
            404 => Err(VogtError::NotFound(format!(
                "the {} knows no agent CLI named {tool:?}",
                self.label
            ))),
            409 => Err(VogtError::Conflict(
                nonempty(&said).unwrap_or_else(|| format!("{tool} {version} was not made current")),
            )),
            401 | 403 => Err(VogtError::EngineUnavailable(format!(
                "the {} refused this request ({status}): the token lacks the `agent-clis-write` capability",
                self.label
            ))),
            status if status >= 400 => Err(VogtError::EngineUnavailable(format!(
                "the {} answered {status} for POST /api/agent-clis/{tool}",
                self.label
            ))),
            _ => {
                let payload: Value = if text.trim().is_empty() {
                    Value::Object(Map::new())
                } else {
                    serde_json::from_str(text.trim()).map_err(|_| {
                        VogtError::EngineUnavailable(format!(
                            "the {} answered with something that is not JSON",
                            self.label
                        ))
                    })?
                };
                Ok(if payload.is_object() { payload } else { Value::Object(Map::new()) })
            }
        }
    }

    fn call(
        &self,
        path: &str,
        method: &str,
        payload: Option<&Value>,
        allow_missing: bool,
        timeout: Option<Duration>,
    ) -> Result<Value, VogtError> {
        let url = format!("{}{path}", self.base_url);
        let mut headers = base_headers();
        if let Some(token) = &self.token {
            headers.insert("Authorization".to_string(), format!("Bearer {token}"));
        }
        let body = match payload {
            Some(payload) => {
                headers.insert("Content-Type".to_string(), "application/json".to_string());
                serde_json::to_vec(payload).unwrap_or_default()
            }
            None => Vec::new(),
        };
        let (status, response) = self.fetch(&url, &headers, &body, method, timeout)?;
        if status == 404 && allow_missing {
            return Ok(Value::Null);
        }
        if status == 403 {
            let said = engine_error_text(&String::from_utf8_lossy(&response));
            // The WI-983 person gate is a different refusal from a missing
            // grant: "forbidden: person required: …" means the session names no
            // person, not that the caller lacks a capability.
            if let Some(reason) = said.strip_prefix("forbidden: person required") {
                let detail = reason.trim().trim_start_matches(':').trim();
                return Err(VogtError::PersonRequired(
                    nonempty(detail).unwrap_or_else(|| "person required".to_string()),
                ));
            }
            if let Some(reason) = said.strip_prefix("forbidden: ") {
                return Err(VogtError::GrantRefused(reason.to_string()));
            }
        }
        if status == 401 || status == 403 {
            return Err(VogtError::EngineUnavailable(format!(
                "the {} refused this request ({status}): the token is missing, wrong, or lacks the `sessions` capability",
                self.label
            )));
        }
        if status == 400 {
            let said = engine_error_text(&String::from_utf8_lossy(&response));
            return Err(VogtError::InvalidRequest(nonempty(&said).unwrap_or_else(
                || format!("the {} refused {method} {path}", self.label),
            )));
        }
        if status == 409 {
            let said = engine_error_text(&String::from_utf8_lossy(&response));
            return Err(VogtError::Conflict(nonempty(&said).unwrap_or_else(|| {
                format!("the {} refused {method} {path} (409)", self.label)
            })));
        }
        if status >= 400 {
            return Err(VogtError::EngineUnavailable(format!(
                "the {} answered {status} for {method} {path}",
                self.label
            )));
        }
        let text = String::from_utf8(response).map_err(|_| {
            VogtError::EngineUnavailable(format!(
                "the {} answered with bytes that are not UTF-8",
                self.label
            ))
        })?;
        if text.trim().is_empty() {
            return Ok(Value::Object(Map::new()));
        }
        // A 2xx that is not JSON is a broken engine, not an empty answer.
        serde_json::from_str(text.trim()).map_err(|_| {
            VogtError::EngineUnavailable(format!(
                "the {} answered with something that is not JSON",
                self.label
            ))
        })
    }

    fn fetch(
        &self,
        url: &str,
        headers: &BTreeMap<String, String>,
        body: &[u8],
        method: &str,
        timeout: Option<Duration>,
    ) -> Result<(u16, Vec<u8>), VogtError> {
        if let Some(transport) = &self.transport {
            return Ok(transport(url, headers, body, method));
        }
        // std, as Python uses urllib: this adapter makes a handful of requests
        // and the core stays free of an HTTP dependency.
        match http1::exchange(url, method, headers, body, timeout.unwrap_or(self.timeout)) {
            Ok(response) => Ok(response),
            Err(error) => Err(VogtError::EngineUnavailable(format!(
                "the {} is not answering: {error}",
                self.label
            ))),
        }
    }
}

/// One terminal to start. Fields left `None` are omitted from the wire, so a
/// default start is the request this client has always made.
pub struct CreateSession {
    pub name: String,
    pub cwd: String,
    pub command: Option<Vec<String>>,
    pub template: Option<String>,
    pub env: Option<Vec<(String, String)>>,
    pub prompt: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub resume: Option<String>,
    pub permission_mode: Option<String>,
    pub autopilot: bool,
    pub role: String,
    pub work_item: Option<String>,
}

impl CreateSession {
    pub fn new(name: impl Into<String>, cwd: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            cwd: cwd.into(),
            command: None,
            template: None,
            env: None,
            prompt: None,
            model: None,
            effort: None,
            resume: None,
            permission_mode: None,
            autopilot: false,
            role: "worker".to_string(),
            work_item: None,
        }
    }
}

// -- the wire, read field by field ------------------------------------------

/// A permission dialog an agent CLI is showing. `command_excerpt` is terminal
/// output: untrusted data, shown, never acted on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineApproval {
    pub question: String,
    pub command_excerpt: String,
    pub detected_at: String,
    pub deadline_seconds: Option<i64>,
    pub deadline_at: Option<String>,
    pub kind: String,
    /// `(number, label, selected)` per option, in menu order.
    pub options: Vec<(i64, String, bool)>,
}

impl EngineApproval {
    fn from_payload(payload: Option<&Value>) -> Option<Self> {
        let object = payload?.as_object()?;
        Some(Self {
            question: string_field(object, "question"),
            command_excerpt: string_field(object, "command_excerpt"),
            detected_at: string_field(object, "detected_at"),
            deadline_seconds: optional_int(object.get("deadline_seconds")),
            deadline_at: optional_str(object.get("deadline_at")),
            kind: object
                .get("kind")
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
                .unwrap_or("permission")
                .to_string(),
            options: object
                .get("options")
                .and_then(Value::as_array)
                .map(|options| {
                    options
                        .iter()
                        .filter_map(|option| {
                            let object = option.as_object()?;
                            Some((
                                object.get("number").and_then(Value::as_i64).unwrap_or(0),
                                string_field(object, "label"),
                                object.get("selected") == Some(&Value::Bool(true)),
                            ))
                        })
                        .collect()
                })
                .unwrap_or_default(),
        })
    }
}

/// An agent's own report that it is blocked on a person. The text is the
/// agent's: untrusted data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineBlocked {
    pub reason: String,
    pub items: Vec<String>,
    pub since: Option<String>,
}

impl EngineBlocked {
    fn from_payload(payload: Option<&Value>) -> Option<Self> {
        let object = payload?.as_object()?;
        Some(Self {
            reason: string_field(object, "reason"),
            items: object
                .get("items")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .map(|item| item.to_string().trim_matches('"').to_string())
                        .collect()
                })
                .unwrap_or_default(),
            since: optional_str(object.get("since")),
        })
    }
}

/// One terminal, as the engine describes it. Being listed is not being alive:
/// the engine keeps an exited session until it is deleted.
#[derive(Debug, Clone, PartialEq)]
pub struct EngineSession {
    pub id: String,
    pub name: String,
    /// The engine's `activity`, never a renamed `state`.
    pub activity: String,
    pub cwd: String,
    pub exit_code: Option<i64>,
    pub activity_changed_at: Option<String>,
    pub created_at: Option<String>,
    pub alive: bool,
    pub turn_started_at: Option<String>,
    pub last_output_at: Option<String>,
    pub approval: Option<EngineApproval>,
    pub command: Option<String>,
    pub blocked: Option<EngineBlocked>,
    pub conversation_agent: Option<String>,
    pub conversation_id: Option<String>,
    pub hibernation: Option<EngineHibernation>,
    pub keep_awake: bool,
    pub autopilot: bool,
    pub autopilot_nudges: i64,
    pub role: String,
    pub work_item: Option<String>,
    pub resources: Option<EngineResources>,
    pub template: Option<String>,
    pub permission_mode: Option<String>,
    pub stopped_by: Option<String>,
    pub stop_reason: Option<String>,
}

impl EngineSession {
    pub fn hibernated(&self) -> bool {
        self.activity == "hibernated"
    }

    fn from_payload(payload: &Value) -> Self {
        let object = payload.as_object();
        let get = |name: &str| object.and_then(|object| object.get(name));
        let exit_code = optional_int(get("exit_code"));
        let alive = match get("alive") {
            Some(Value::Bool(alive)) => *alive,
            _ => exit_code.is_none(),
        };
        let conversation = get("conversation").and_then(Value::as_object);
        let stop = get("stop").and_then(Value::as_object);
        Self {
            id: get("id").map(value_string).unwrap_or_default(),
            name: get("name").map(value_string).unwrap_or_default(),
            activity: get("activity")
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
                .unwrap_or("unknown")
                .to_string(),
            cwd: get("cwd").map(value_string).unwrap_or_default(),
            exit_code,
            activity_changed_at: optional_str(get("activity_changed_at")),
            created_at: optional_str(get("created_at")),
            alive,
            turn_started_at: optional_str(get("turn_started_at")),
            last_output_at: optional_str(get("last_output_at")),
            approval: EngineApproval::from_payload(get("approval")),
            command: optional_str(get("command")),
            blocked: EngineBlocked::from_payload(get("blocked")),
            conversation_agent: optional_str(conversation.and_then(|c| c.get("agent"))),
            conversation_id: optional_str(conversation.and_then(|c| c.get("id"))),
            hibernation: EngineHibernation::from_payload(get("hibernation")),
            keep_awake: get("keep_awake") == Some(&Value::Bool(true)),
            autopilot: get("autopilot") == Some(&Value::Bool(true)),
            autopilot_nudges: optional_int(get("autopilot_nudges")).unwrap_or(0),
            role: if get("role").and_then(Value::as_str) == Some("oversight") {
                "oversight".to_string()
            } else {
                "worker".to_string()
            },
            work_item: optional_str(get("work_item")),
            resources: EngineResources::from_payload(get("resources")),
            template: optional_str(get("template")),
            permission_mode: optional_str(get("permission_mode")),
            stopped_by: optional_str(stop.and_then(|stop| stop.get("by"))),
            stop_reason: optional_str(stop.and_then(|stop| stop.get("reason"))),
        }
    }
}

/// What a session's process tree held at the engine's last sample.
#[derive(Debug, Clone, PartialEq)]
pub struct EngineResources {
    pub rss_bytes: i64,
    pub cpu_pct: f64,
    pub processes: i64,
    pub sampled_at: String,
    pub over_threshold: bool,
}

impl EngineResources {
    fn from_payload(payload: Option<&Value>) -> Option<Self> {
        let object = payload?.as_object()?;
        Some(Self {
            rss_bytes: object.get("rss_bytes").and_then(Value::as_i64)?,
            cpu_pct: object.get("cpu_pct").and_then(Value::as_f64).unwrap_or(0.0),
            processes: object.get("processes").and_then(Value::as_i64).unwrap_or(0),
            sampled_at: string_field(object, "sampled_at"),
            over_threshold: object.get("over_threshold") == Some(&Value::Bool(true)),
        })
    }
}

/// When and why the engine hibernated a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineHibernation {
    pub at: String,
    pub trigger: String,
    pub resumable: bool,
    pub reason: Option<String>,
}

impl EngineHibernation {
    fn from_payload(payload: Option<&Value>) -> Option<Self> {
        let object = payload?.as_object()?;
        Some(Self {
            at: string_field(object, "at"),
            trigger: string_field(object, "trigger"),
            resumable: object.get("resumable") != Some(&Value::Bool(false)),
            reason: optional_str(object.get("reason")),
        })
    }
}

/// One row of `GET /api/sessions/sweep`.
#[derive(Debug, Clone, PartialEq)]
pub struct EngineSweepEntry {
    pub session: EngineSession,
    pub screen_tail: Vec<String>,
    pub ready: bool,
}

impl EngineSweepEntry {
    fn from_payload(payload: &Value) -> Self {
        let summary = payload
            .get("summary")
            .filter(|value| value.is_object())
            .cloned()
            .unwrap_or(Value::Null);
        let tail = payload.get("screen_tail").and_then(Value::as_array);
        Self {
            session: EngineSession::from_payload(&summary),
            screen_tail: tail.map(|values| string_list(values)).unwrap_or_default(),
            ready: payload.get("ready") == Some(&Value::Bool(true)),
        }
    }
}

/// One terminal that has ended, as the engine's history records it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineArchivedSession {
    pub id: String,
    pub created_at: String,
    pub ended_at: Option<String>,
    pub exit_code: Option<i64>,
}

impl EngineArchivedSession {
    fn from_payload(payload: &Value) -> Self {
        Self {
            id: payload.get("id").map(value_string).unwrap_or_default(),
            created_at: payload
                .get("created_at")
                .map(value_string)
                .unwrap_or_default(),
            ended_at: optional_str(payload.get("ended_at")),
            exit_code: optional_int(payload.get("exit_code")),
        }
    }
}

/// One row of the engine's session-history listing.
#[derive(Debug, Clone, PartialEq)]
pub struct EngineHistorySession {
    pub id: String,
    pub name: String,
    pub created_at: String,
    pub ended_at: Option<String>,
    pub exit_code: Option<i64>,
    pub cwd: Option<String>,
    pub command: Option<String>,
    pub scrollback_bytes: i64,
    pub template: Option<String>,
    pub role: Option<String>,
    pub conversation_agent: Option<String>,
    pub conversation_id: Option<String>,
    pub resume_template: Option<String>,
    pub work_item: Option<String>,
}

impl EngineHistorySession {
    fn from_payload(payload: &Value) -> Self {
        Self {
            id: field_string(payload, "id"),
            name: field_string(payload, "name"),
            created_at: field_string(payload, "created_at"),
            ended_at: optional_str(payload.get("ended_at")),
            exit_code: optional_int(payload.get("exit_code")),
            cwd: optional_str(payload.get("cwd")),
            command: optional_str(payload.get("command")),
            scrollback_bytes: optional_int(payload.get("scrollback_bytes")).unwrap_or(0),
            template: optional_str(payload.get("template")),
            role: optional_str(payload.get("role")),
            conversation_agent: optional_str(payload.get("conversation_agent")),
            conversation_id: optional_str(payload.get("conversation_id")),
            resume_template: optional_str(payload.get("resume_template")),
            work_item: optional_str(payload.get("work_item")),
        }
    }
}

/// One hit from a session-output search. `live` marks a running session's
/// scrollback rather than the archived index.
#[derive(Debug, Clone, PartialEq)]
pub struct EngineHistoryMatch {
    pub session_id: String,
    pub session_name: String,
    pub created_at: String,
    pub match_snippet: String,
    pub rank: f64,
    pub live: bool,
}

impl EngineHistoryMatch {
    fn from_payload(payload: &Value) -> Self {
        Self {
            session_id: field_string(payload, "session_id"),
            session_name: field_string(payload, "session_name"),
            created_at: field_string(payload, "created_at"),
            match_snippet: field_string(payload, "match_snippet"),
            rank: payload.get("rank").and_then(Value::as_f64).unwrap_or(0.0),
            live: payload
                .get("live")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        }
    }
}

/// The tail of a session's raw output log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineSessionLog {
    pub session_id: String,
    pub text: String,
    pub bytes: i64,
    pub total_bytes: i64,
    pub truncated: bool,
}

impl EngineSessionLog {
    fn from_payload(payload: &Value) -> Self {
        Self {
            session_id: field_string(payload, "session_id"),
            text: field_string(payload, "text"),
            bytes: optional_int(payload.get("bytes")).unwrap_or(0),
            total_bytes: optional_int(payload.get("total_bytes")).unwrap_or(0),
            truncated: payload
                .get("truncated")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        }
    }
}

/// A session's current visible screen. Fields the engine leaves out stay
/// `None` rather than being guessed.
#[derive(Debug, Clone, PartialEq)]
pub struct EngineScreen {
    pub id: String,
    pub cols: i64,
    pub rows: i64,
    pub lines: Vec<String>,
    pub cursor_row: Option<i64>,
    pub cursor_col: Option<i64>,
    pub title: Option<String>,
    pub activity: Option<String>,
    pub alive: Option<bool>,
    pub ready: Option<bool>,
    pub scrollback: Vec<String>,
    pub turn_started_at: Option<String>,
    pub last_output_at: Option<String>,
    pub approval: Option<EngineApproval>,
    pub blocked: Option<EngineBlocked>,
}

impl EngineScreen {
    fn from_payload(payload: &Value) -> Self {
        let cursor = payload.get("cursor").and_then(Value::as_object);
        Self {
            id: field_string(payload, "id"),
            cols: optional_int(payload.get("cols")).unwrap_or(0),
            rows: optional_int(payload.get("rows")).unwrap_or(0),
            lines: payload
                .get("lines")
                .and_then(Value::as_array)
                .map(|values| string_list(values))
                .unwrap_or_default(),
            cursor_row: optional_int(cursor.and_then(|cursor| cursor.get("row"))),
            cursor_col: optional_int(cursor.and_then(|cursor| cursor.get("col"))),
            title: optional_str(payload.get("title")),
            activity: optional_str(payload.get("activity")),
            alive: optional_bool(payload.get("alive")),
            ready: optional_bool(payload.get("ready")),
            scrollback: payload
                .get("scrollback")
                .and_then(Value::as_array)
                .map(|values| string_list(values))
                .unwrap_or_default(),
            turn_started_at: optional_str(payload.get("turn_started_at")),
            last_output_at: optional_str(payload.get("last_output_at")),
            approval: EngineApproval::from_payload(payload.get("approval")),
            blocked: EngineBlocked::from_payload(payload.get("blocked")),
        }
    }
}

/// Why `GET /api/sessions/{id}/wait` returned, and the screen then.
#[derive(Debug, Clone, PartialEq)]
pub struct EngineWait {
    pub outcome: String,
    pub matched: bool,
    pub waited_ms: i64,
    pub screen: EngineScreen,
}

impl EngineWait {
    fn from_payload(payload: &Value) -> Self {
        let screen = payload
            .get("screen")
            .filter(|value| value.is_object())
            .cloned()
            .unwrap_or(Value::Null);
        Self {
            outcome: field_string(payload, "outcome"),
            matched: payload
                .get("matched")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            waited_ms: optional_int(payload.get("waited_ms")).unwrap_or(0),
            screen: EngineScreen::from_payload(&screen),
        }
    }
}

/// Something a bound agent-task run reported about itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineTaskFinding {
    pub at: String,
    pub text: String,
    pub source: String,
}

impl EngineTaskFinding {
    fn from_payload(payload: &Value) -> Self {
        Self {
            at: field_string(payload, "at"),
            text: field_string(payload, "text"),
            source: payload
                .get("source")
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
                .unwrap_or("notify-phrase")
                .to_string(),
        }
    }
}

/// One execution of an agent task.
#[derive(Debug, Clone, PartialEq)]
pub struct EngineTaskRun {
    pub id: String,
    pub session_id: String,
    pub started_at: String,
    pub status: String,
    pub completed_at: Option<String>,
    pub exit_code: Option<i64>,
    pub summary: Option<String>,
    pub findings: Vec<EngineTaskFinding>,
}

impl EngineTaskRun {
    fn from_payload(payload: &Value) -> Self {
        Self {
            id: field_string(payload, "id"),
            session_id: field_string(payload, "session_id"),
            started_at: field_string(payload, "started_at"),
            status: payload
                .get("status")
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
                .unwrap_or("running")
                .to_string(),
            completed_at: optional_str(payload.get("completed_at")),
            exit_code: optional_int(payload.get("exit_code")),
            summary: optional_str(payload.get("summary")),
            findings: payload
                .get("findings")
                .and_then(Value::as_array)
                .map(|rows| {
                    rows.iter()
                        .filter(|row| row.is_object())
                        .map(EngineTaskFinding::from_payload)
                        .collect()
                })
                .unwrap_or_default(),
        }
    }
}

/// A scheduled agent task, and what Vogt subject it was bound to. The binding
/// is the names a person types, because the engine cannot resolve a Vogt id.
#[derive(Debug, Clone, PartialEq)]
pub struct EngineAgentTask {
    pub id: String,
    pub name: String,
    pub cwd: Option<String>,
    pub project: Option<String>,
    pub work_item: Option<String>,
    pub runs: Vec<EngineTaskRun>,
}

impl EngineAgentTask {
    pub fn is_bound(&self) -> bool {
        self.project.is_some() || self.work_item.is_some()
    }

    fn from_payload(payload: &Value) -> Self {
        Self {
            id: field_string(payload, "id"),
            name: field_string(payload, "name"),
            cwd: optional_str(payload.get("cwd")),
            project: optional_str(payload.get("vogt_project")),
            work_item: optional_str(payload.get("vogt_work_item")),
            runs: payload
                .get("runs")
                .and_then(Value::as_array)
                .map(|rows| {
                    rows.iter()
                        .filter(|row| row.is_object())
                        .map(EngineTaskRun::from_payload)
                        .collect()
                })
                .unwrap_or_default(),
        }
    }
}

// -- small readers -----------------------------------------------------------

fn base_headers() -> BTreeMap<String, String> {
    BTreeMap::from([
        ("Accept".to_string(), "application/json".to_string()),
        ("User-Agent".to_string(), USER_AGENT.to_string()),
    ])
}

/// Percent-encode one path segment, leaving a `/` that the caller put there.
fn quote(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(byte as char);
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// `urllib.parse.quote(text, safe="")`: every byte outside the unreserved set.
fn quote_strict(text: &str) -> String {
    quote(text).replace('/', "%2F")
}

fn session_path(session_id: &str, suffix: &str) -> String {
    format!("/api/sessions/{}{suffix}", quote(session_id))
}

fn rows(payload: &Value) -> impl Iterator<Item = &Value> {
    payload
        .as_array()
        .into_iter()
        .flatten()
        .filter(|row| row.is_object())
}

fn string_list(values: &[Value]) -> Vec<String> {
    values.iter().map(value_string).collect()
}

fn value_string(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn field_string(payload: &Value, name: &str) -> String {
    payload.get(name).map(value_string).unwrap_or_default()
}

fn string_field(object: &Map<String, Value>, name: &str) -> String {
    object.get(name).map(value_string).unwrap_or_default()
}

fn optional_str(value: Option<&Value>) -> Option<String> {
    let text = value_string(value?).trim().to_string();
    (!text.is_empty() && text != "null").then_some(text)
}

/// An integer that is not a boolean. JSON has no bool/int overlap, so this is
/// `as_i64`.
fn optional_int(value: Option<&Value>) -> Option<i64> {
    value.and_then(Value::as_i64)
}

fn optional_bool(value: Option<&Value>) -> Option<bool> {
    value.and_then(Value::as_bool)
}

fn nonempty(text: &str) -> Option<String> {
    (!text.is_empty()).then(|| text.to_string())
}

/// One HTTP/1.1 exchange. Public so the other urllib-style adapters share it
/// rather than each carrying a copy. `http` is spoken by the standard library;
/// `https` goes through ureq on rustls, because GitHub and an https peer are
/// both real callers.
pub mod http1 {
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::time::Duration;

    use super::BTreeMap;

    /// The most of a response body kept. A peer or a misbehaving engine must
    /// not be able to grow memory without bound.
    const MAX_BODY: usize = 8 * 1024 * 1024;

    pub fn exchange(
        url: &str,
        method: &str,
        headers: &BTreeMap<String, String>,
        body: &[u8],
        timeout: Duration,
    ) -> Result<(u16, Vec<u8>), String> {
        exchange_limited(url, method, headers, body, timeout, MAX_BODY)
    }

    /// Like `exchange`, but the body is capped at `limit` bytes while it is
    /// read, not after. A peer's 512 KiB bound has to hold memory, not just the
    /// value returned.
    pub fn exchange_limited(
        url: &str,
        method: &str,
        headers: &BTreeMap<String, String>,
        body: &[u8],
        timeout: Duration,
        limit: usize,
    ) -> Result<(u16, Vec<u8>), String> {
        if url.starts_with("https://") {
            return exchange_tls(url, method, headers, body, timeout, limit);
        }
        exchange_plain(url, method, headers, body, timeout, limit)
    }

    fn exchange_tls(
        url: &str,
        method: &str,
        headers: &BTreeMap<String, String>,
        body: &[u8],
        timeout: Duration,
        limit: usize,
    ) -> Result<(u16, Vec<u8>), String> {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(timeout)
            .timeout(timeout)
            .tls_config(tls_config())
            .build();
        let mut request = agent.request(method, url);
        for (name, value) in headers {
            request = request.set(name, value);
        }
        let response = if body.is_empty() {
            request.call()
        } else {
            request.send_bytes(body)
        };
        let response = match response {
            Ok(response) => response,
            Err(ureq::Error::Status(_, response)) => response,
            Err(error) => return Err(error.to_string()),
        };
        let status = response.status();
        let mut buf = Vec::new();
        response
            .into_reader()
            .take(limit as u64)
            .read_to_end(&mut buf)
            .map_err(|error| error.to_string())?;
        Ok((status, buf))
    }

    /// Trust the platform certificate store. Built once: loading the store reads
    /// and parses a few hundred kilobytes, which is wasted on every request.
    ///
    /// When `SSL_CERT_FILE` or `SSL_CERT_DIR` is set, `rustls-native-certs`
    /// returns only that bundle and ignores the platform store. Python's
    /// urllib keeps both. The variable is not unset around the load, because
    /// changing the process environment races with every other thread and with
    /// a git child spawned in between, so a deployment that points the variable
    /// at a private CA must also include the public roots in that bundle.
    fn tls_config() -> std::sync::Arc<rustls::ClientConfig> {
        use std::sync::OnceLock;
        static CONFIG: OnceLock<std::sync::Arc<rustls::ClientConfig>> = OnceLock::new();
        CONFIG
            .get_or_init(|| {
                let mut store = rustls::RootCertStore::empty();
                for cert in rustls_native_certs::load_native_certs().unwrap_or_default() {
                    let _ = store.add(cert);
                }
                std::sync::Arc::new(
                    rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(
                        rustls::crypto::ring::default_provider(),
                    ))
                    .with_safe_default_protocol_versions()
                    .expect("ring supports the default protocol versions")
                    .with_root_certificates(store)
                    .with_no_client_auth(),
                )
            })
            .clone()
    }

    fn exchange_plain(
        url: &str,
        method: &str,
        headers: &BTreeMap<String, String>,
        body: &[u8],
        timeout: Duration,
        limit: usize,
    ) -> Result<(u16, Vec<u8>), String> {
        let (host, port, path) = split_http_url(url)?;
        let mut stream = TcpStream::connect((host, port)).map_err(|error| error.to_string())?;
        stream.set_read_timeout(Some(timeout)).ok();
        stream.set_write_timeout(Some(timeout)).ok();
        // The default port stays off the Host header; any other port is part of
        // the authority and must be sent.
        let host_header = if port == 80 {
            host.to_string()
        } else {
            format!("{host}:{port}")
        };
        let mut request =
            format!("{method} {path} HTTP/1.1\r\nHost: {host_header}\r\nConnection: close\r\n");
        for (name, value) in headers {
            request.push_str(&format!("{name}: {value}\r\n"));
        }
        if !body.is_empty() {
            request.push_str(&format!("Content-Length: {}\r\n", body.len()));
        }
        request.push_str("\r\n");
        stream
            .write_all(request.as_bytes())
            .and_then(|()| stream.write_all(body))
            .map_err(|error| error.to_string())?;
        let mut raw = Vec::new();
        stream
            .take(limit as u64)
            .read_to_end(&mut raw)
            .map_err(|error| error.to_string())?;
        parse_response(&raw)
    }

    fn split_http_url(url: &str) -> Result<(&str, u16, String), String> {
        let rest = url
            .strip_prefix("http://")
            .ok_or_else(|| format!("{url} is not an http URL"))?;
        let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
        let (host, port) = match authority.split_once(':') {
            Some((host, port)) => (
                host,
                port.parse::<u16>()
                    .map_err(|_| format!("bad port in {url}"))?,
            ),
            None => (authority, 80),
        };
        if host.is_empty() {
            return Err(format!("no host in {url}"));
        }
        Ok((host, port, format!("/{path}")))
    }

    fn parse_response(raw: &[u8]) -> Result<(u16, Vec<u8>), String> {
        let split = raw
            .windows(4)
            .position(|mark| mark == b"\r\n\r\n")
            .ok_or("no response headers")?;
        let head = std::str::from_utf8(&raw[..split])
            .map_err(|_| "response headers are not UTF-8".to_string())?;
        let status = head
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|code| code.parse().ok())
            .ok_or_else(|| "no status line".to_string())?;
        let mut body = raw[split + 4..].to_vec();
        if head
            .lines()
            .any(|line| line.eq_ignore_ascii_case("transfer-encoding: chunked"))
        {
            body = decode_chunks(&body)?;
        }
        Ok((status, body))
    }

    fn decode_chunks(raw: &[u8]) -> Result<Vec<u8>, String> {
        let mut out = Vec::new();
        let mut rest = raw;
        loop {
            let line_end = rest
                .windows(2)
                .position(|mark| mark == b"\r\n")
                .ok_or("truncated chunk")?;
            let size = std::str::from_utf8(&rest[..line_end])
                .ok()
                .and_then(|text| {
                    usize::from_str_radix(text.trim().split(';').next().unwrap_or(""), 16).ok()
                })
                .ok_or("bad chunk size")?;
            rest = &rest[line_end + 2..];
            if size == 0 {
                return Ok(out);
            }
            if rest.len() < size + 2 {
                return Err("truncated chunk".to_string());
            }
            out.extend_from_slice(&rest[..size]);
            rest = &rest[size + 2..];
        }
    }
}

/// The engine's `{"error": "..."}` body as a sentence, or the raw text.
fn engine_error_text(text: &str) -> String {
    serde_json::from_str::<Value>(text)
        .ok()
        .and_then(|payload| {
            payload
                .get("error")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| text.to_string())
}

trait ExpandUser {
    fn expanduser_lossy(&self) -> std::borrow::Cow<'_, Path>;
}

impl ExpandUser for Path {
    fn expanduser_lossy(&self) -> std::borrow::Cow<'_, Path> {
        let text = self.to_string_lossy();
        if let Some(rest) = text.strip_prefix("~/") {
            if let Some(home) = std::env::var_os("HOME") {
                return std::borrow::Cow::Owned(Path::new(&home).join(rest));
            }
        }
        std::borrow::Cow::Borrowed(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// A transport that records what it was asked and answers with a script.
    fn scripted(status: u16, body: &'static str) -> (Arc<Mutex<Vec<String>>>, Transport) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&seen);
        let transport: Transport = Box::new(move |url, headers, body_bytes, method| {
            record.lock().unwrap().push(format!(
                "{method} {url} auth={} body={}",
                headers.contains_key("Authorization"),
                String::from_utf8_lossy(body_bytes)
            ));
            (status, body.as_bytes().to_vec())
        });
        (seen, transport)
    }

    fn client(transport: Transport) -> EngineClient {
        EngineClient::new("http://engine", Some("tok".to_string()), Some(transport))
    }

    #[test]
    fn a_session_is_read_by_its_activity_not_a_renamed_state() {
        let (_, transport) = scripted(
            200,
            r#"[{"id":"s1","name":"one","activity":"idle","cwd":"/work","alive":false,"exit_code":1,"keep_awake":true,"role":"oversight","autopilot_nudges":3,"conversation":{"agent":"claude","id":"c1"}}]"#,
        );
        let sessions = client(transport).list_sessions().unwrap();
        let session = &sessions[0];
        assert_eq!(session.activity, "idle");
        assert!(!session.alive);
        assert_eq!(session.exit_code, Some(1));
        assert!(session.keep_awake);
        assert_eq!(session.role, "oversight");
        assert_eq!(session.autopilot_nudges, 3);
        assert_eq!(session.conversation_id.as_deref(), Some("c1"));
    }

    #[test]
    fn an_older_engine_without_alive_is_alive_exactly_when_it_has_no_exit_code() {
        let (_, transport) = scripted(
            200,
            r#"{"id":"s","summary":{"id":"s","name":"n","cwd":"/w"}}"#,
        );
        let session = client(transport).get_session("s").unwrap().unwrap();
        assert!(session.alive);
        assert_eq!(session.activity, "unknown");
    }

    #[test]
    fn a_wait_carries_its_outcome_and_the_screen() {
        let (seen, transport) = scripted(
            200,
            r#"{"outcome":"ready","matched":true,"waited_ms":40,"screen":{"id":"s","lines":["ok"],"ready":true}}"#,
        );
        let wait = client(transport)
            .wait_session("s", "ready", Duration::from_secs(5))
            .unwrap()
            .unwrap();
        assert_eq!(wait.outcome, "ready");
        assert!(wait.matched);
        assert_eq!(wait.waited_ms, 40);
        assert_eq!(wait.screen.lines, ["ok"]);
        assert!(seen.lock().unwrap()[0].contains("/wait?until=ready&timeout_s=5"));
    }

    #[test]
    fn create_session_omits_what_was_not_asked_and_sends_the_cwd() {
        let (seen, transport) = scripted(200, r#"{"id":"s","name":"n","cwd":"/proj"}"#);
        let mut spec = CreateSession::new("n", "/proj");
        spec.permission_mode = Some("default".to_string());
        spec.role = "worker".to_string();
        client(transport).create_session(&spec).unwrap();
        let sent = &seen.lock().unwrap()[0];
        assert!(sent.contains("POST"));
        assert!(sent.contains(r#""cwd":"/proj""#));
        assert!(!sent.contains("permission_mode"));
        assert!(!sent.contains("role"));
    }

    #[test]
    fn a_403_that_names_its_reason_is_a_grant_refusal() {
        let (_, transport) = scripted(403, r#"{"error":"forbidden: project not open"}"#);
        let error = client(transport)
            .apply_grant("s", &serde_json::json!({}))
            .unwrap_err();
        assert_eq!(error.code(), "grant_refused");
        assert_eq!(error.message(), "project not open");
    }

    #[test]
    fn a_missing_session_on_a_grant_is_not_found() {
        let (_, transport) = scripted(404, "");
        let error = client(transport)
            .apply_grant("s", &serde_json::json!({}))
            .unwrap_err();
        assert_eq!(error.code(), "not_found");
    }

    #[test]
    fn a_2xx_that_is_not_json_is_the_engine_being_unavailable() {
        let (_, transport) = scripted(200, "not json");
        let error = client(transport).healthz().unwrap_err();
        assert_eq!(error.code(), "engine_unavailable");
        assert_eq!(error.http_status(), 502);
    }

    #[test]
    fn a_session_id_keeps_its_slashes() {
        let (seen, transport) = scripted(200, r#"{"id":"a/b"}"#);
        client(transport).get_session("a/b").unwrap();
        assert!(seen.lock().unwrap()[0].contains("/api/sessions/a/b"));
    }

    #[test]
    fn no_engine_configured_is_none_not_an_error() {
        assert!(EngineClient::from_config(Some("  "), None).is_none());
    }
}
