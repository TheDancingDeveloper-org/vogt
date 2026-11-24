//! The one place the engine's wire meets the core's structs.
//!
//! Every response is deserialized strictly into the `vogt-engine-contract`
//! types, then mapped field by field into the core's `Engine*` structs. The
//! mapping reads each contract field by name, so renaming or removing a
//! contract field breaks this crate's build instead of silently defaulting.
//!
//! Leniency is kept only where the old hand parsers defaulted on purpose. Each
//! such default is named at the place it is applied, in `prepare_*`, as a
//! comment on that field. A JSON `null` is read as an absent field throughout,
//! as the old parsers did (`strip_nulls`).

use serde::de::DeserializeOwned;
use serde_json::{Map, Value};
use vogt_engine_contract as c;

use super::{
    value_string, EngineApproval, EngineBlocked, EngineChange, EngineChanges, EngineEscalation,
    EngineHibernation, EngineResources, EngineScreen, EngineSession, EngineSweepEntry, EngineWait,
    EngineWaitAny, EngineWorkspace,
};
use crate::errors::VogtError;

/// The id every deserialised contract `Uuid` carries. The core keeps engine ids
/// as the opaque strings they arrive as (fixtures, older engines and the
/// session registry all use non-UUID ids), so `prepare_*` takes the real id out
/// before decoding and the mapping puts it back.
const OPAQUE_ID: &str = "00000000-0000-0000-0000-000000000000";

fn decode<T: DeserializeOwned>(what: &str, object: Map<String, Value>) -> Result<T, VogtError> {
    serde_json::from_value(Value::Object(object)).map_err(|error| {
        VogtError::EngineUnavailable(format!(
            "engine {what} payload does not match the contract: {error}"
        ))
    })
}

fn object_of(payload: &Value) -> Map<String, Value> {
    payload.as_object().cloned().unwrap_or_default()
}

/// A JSON `null` is the same as an absent field.
fn strip_nulls(object: &mut Map<String, Value>) {
    object.retain(|_, value| !value.is_null());
    for value in object.values_mut() {
        match value {
            Value::Object(inner) => strip_nulls(inner),
            Value::Array(items) => {
                for item in items {
                    if let Value::Object(inner) = item {
                        strip_nulls(inner);
                    }
                }
            }
            _ => {}
        }
    }
}

fn fill(object: &mut Map<String, Value>, name: &str, default: Value) {
    object.entry(name).or_insert(default);
}

/// The old readers trimmed an optional text and read `""` and `"null"` as
/// absent.
fn clean(text: Option<String>) -> Option<String> {
    let text = text?.trim().to_string();
    (!text.is_empty() && text != "null").then_some(text)
}

fn nested<'a>(
    object: &'a mut Map<String, Value>,
    name: &str,
) -> Option<&'a mut Map<String, Value>> {
    object.get_mut(name).and_then(Value::as_object_mut)
}

fn needs_str(needs: c::EscalationNeeds) -> String {
    match needs {
        c::EscalationNeeds::Person => "person",
        c::EscalationNeeds::Overseer => "overseer",
        c::EscalationNeeds::Either => "either",
    }
    .to_string()
}

fn trigger_str(trigger: c::HibernateTrigger) -> String {
    match trigger {
        c::HibernateTrigger::Manual => "manual",
        c::HibernateTrigger::Idle => "idle",
        c::HibernateTrigger::Memory => "memory",
        c::HibernateTrigger::Shutdown => "shutdown",
        c::HibernateTrigger::Recovered => "recovered",
        c::HibernateTrigger::Budget => "budget",
        c::HibernateTrigger::Done => "done",
    }
    .to_string()
}

fn activity_str(activity: c::ActivityState) -> String {
    match activity {
        c::ActivityState::Idle => "idle",
        c::ActivityState::Running => "running",
        c::ActivityState::WaitingForInput => "waiting-for-input",
        c::ActivityState::AwaitingApproval => "awaiting-approval",
        c::ActivityState::Errored => "errored",
        c::ActivityState::Exited => "exited",
        c::ActivityState::Hibernated => "hibernated",
        c::ActivityState::Stopped => "stopped",
    }
    .to_string()
}

// -- approval and blocked ----------------------------------------------------

fn prepare_approval(approval: &mut Map<String, Value>) {
    // Text the engine left out is empty, not an error.
    fill(approval, "question", Value::from(""));
    fill(approval, "command_excerpt", Value::from(""));
    fill(approval, "detected_at", Value::from(""));
    // An empty kind is the default `permission` (mapped below).
    if approval.get("kind").and_then(Value::as_str) == Some("") {
        approval.remove("kind");
    }
    if let Some(Value::Array(options)) = approval.get_mut("options") {
        for option in options {
            if let Value::Object(option) = option {
                // A menu entry without a number or label is still an entry.
                fill(option, "number", Value::from(0));
                fill(option, "label", Value::from(""));
            }
        }
    }
}

impl From<c::ApprovalPrompt> for EngineApproval {
    fn from(approval: c::ApprovalPrompt) -> Self {
        Self {
            question: approval.question,
            command_excerpt: approval.command_excerpt,
            detected_at: approval.detected_at,
            deadline_seconds: approval.deadline_seconds.map(i64::from),
            deadline_at: clean(approval.deadline_at),
            kind: if approval.kind.is_empty() {
                "permission".to_string()
            } else {
                approval.kind
            },
            options: approval
                .options
                .into_iter()
                .map(|option| (i64::from(option.number), option.label, option.selected))
                .collect(),
            key: approval.key,
        }
    }
}

/// A boundary this build does not know reads as `other`: a newer engine may
/// add one, and the ask must still reach a person.
fn known_boundary(object: &mut Map<String, Value>) {
    let known = [
        "permission_prompt",
        "denied_action",
        "credential",
        "decision",
        "other",
    ];
    let unknown = object
        .get("boundary")
        .and_then(Value::as_str)
        .is_some_and(|boundary| !known.contains(&boundary));
    if unknown {
        object.insert("boundary".to_string(), Value::from("other"));
    }
}

fn prepare_blocked(blocked: &mut Map<String, Value>) {
    known_boundary(blocked);
    // A blocked report with no reason or time is still a blocked report.
    fill(blocked, "reason", Value::from(""));
    fill(blocked, "since", Value::from(""));
}

impl From<c::BlockedReport> for EngineBlocked {
    fn from(blocked: c::BlockedReport) -> Self {
        Self {
            reason: blocked.reason,
            items: blocked.items,
            since: clean(Some(blocked.since)),
            needs: needs_str(blocked.needs),
            boundary: blocked.boundary.as_str().to_string(),
            key: clean(Some(blocked.key)),
        }
    }
}

// -- a session ---------------------------------------------------------------

fn prepare_resources(object: &mut Map<String, Value>) {
    let Some(resources) = nested(object, "resources") else {
        return;
    };
    // Resources with no resident-memory figure are no sample at all.
    if !resources.get("rss_bytes").is_some_and(Value::is_u64) {
        object.remove("resources");
        return;
    }
    fill(resources, "cpu_pct", Value::from(0.0));
    fill(resources, "processes", Value::from(0));
    fill(resources, "sampled_at", Value::from(""));
}

fn prepare_hibernation(object: &mut Map<String, Value>) {
    let Some(hibernation) = nested(object, "hibernation") else {
        return;
    };
    fill(hibernation, "at", Value::from(""));
    // A hibernation is resumable unless the engine says it is not.
    fill(hibernation, "resumable", Value::from(true));
}

/// An engine session always has an id; a payload without one is the engine
/// answering with something else, so it is an error, not a session named "".
pub(super) fn session(payload: &Value) -> Result<EngineSession, VogtError> {
    let mut object = object_of(payload);
    let id = object.get("id").map(value_string).unwrap_or_default();
    if id.is_empty() {
        return Err(VogtError::EngineUnavailable(
            "engine session payload has no id".to_string(),
        ));
    }
    object.insert("id".to_string(), Value::from(OPAQUE_ID));
    // An unknown role is kept as given and logged, not refused: a newer engine
    // may add one and the listing must still work.
    let role = match object.remove("role") {
        None => "worker".to_string(),
        Some(Value::Null) => "worker".to_string(),
        Some(role) => match role.as_str() {
            Some("") => "worker".to_string(),
            Some(role @ ("worker" | "oversight")) => role.to_string(),
            _ => {
                let other = value_string(&role);
                tracing::warn!(
                    role = other.as_str(),
                    "engine session has an unknown role; kept as given"
                );
                other
            }
        },
    };
    // The program's own OSC 7501 report is untrusted terminal output: it is
    // passed through untouched and never decides whether the listing parses.
    let reported_status = match object.remove("reported_status") {
        Some(Value::Object(map)) => Some(map),
        _ => None,
    };
    // The engine tailer's economics reading is passed through as sent, even
    // partial: the core reads it key by key and treats a missing key as zero.
    let economics = match object.remove("economics") {
        Some(Value::Object(map)) => Some(map),
        _ => None,
    };
    // The budget likewise: the core reads the limits it knows and an absent
    // one means no limit, so the object is kept as sent.
    let budget = match object.remove("budget") {
        Some(Value::Object(map)) => Some(map),
        _ => None,
    };
    strip_nulls(&mut object);
    // No activity (or an empty one) reads as `unknown`; the contract enum is
    // given a placeholder to decode and the core keeps `unknown`.
    let activity_known = object
        .get("activity")
        .and_then(Value::as_str)
        .is_some_and(|text| !text.is_empty());
    if !activity_known {
        object.insert("activity".to_string(), Value::from("idle"));
    }
    // A session without a name reads as unnamed.
    fill(&mut object, "name", Value::from(""));
    // An engine that predates `alive` is alive exactly when it has no exit
    // code; the contract's own default would say true.
    let alive_sent = object.get("alive").is_some_and(Value::is_boolean);
    if let Some(approval) = nested(&mut object, "approval") {
        prepare_approval(approval);
    }
    if let Some(blocked) = nested(&mut object, "blocked") {
        prepare_blocked(blocked);
    }
    prepare_hibernation(&mut object);
    prepare_resources(&mut object);
    let summary: c::SessionSummary = decode("session", object)?;
    let kept = Kept {
        id,
        role,
        activity_known,
        alive_sent,
        reported_status,
        economics,
        budget,
    };
    Ok(session_from(summary, kept))
}

/// What `session` takes out of the payload before the contract decode.
struct Kept {
    id: String,
    role: String,
    activity_known: bool,
    alive_sent: bool,
    reported_status: Option<Map<String, Value>>,
    economics: Option<Map<String, Value>>,
    budget: Option<Map<String, Value>>,
}

fn session_from(summary: c::SessionSummary, kept: Kept) -> EngineSession {
    let Kept {
        id,
        role,
        activity_known,
        alive_sent,
        reported_status,
        economics,
        budget,
    } = kept;
    let alive = if alive_sent {
        summary.alive
    } else {
        summary.exit_code.is_none()
    };
    let (conversation_agent, conversation_id) = match summary.conversation {
        Some(conversation) => (Some(conversation.agent), Some(conversation.id)),
        None => (None, None),
    };
    let (stopped_by, stop_reason) = match summary.stop {
        Some(stop) => (stop.by, stop.reason),
        None => (None, None),
    };
    EngineSession {
        id,
        name: summary.name,
        activity: if activity_known {
            activity_str(summary.activity)
        } else {
            "unknown".to_string()
        },
        cwd: summary.cwd,
        exit_code: summary.exit_code.map(i64::from),
        activity_changed_at: clean(Some(summary.activity_changed_at)),
        created_at: clean(Some(summary.created_at)),
        alive,
        turn_started_at: clean(summary.turn_started_at),
        last_output_at: clean(summary.last_output_at),
        approval: summary.approval.map(EngineApproval::from),
        command: clean(summary.command),
        blocked: summary.blocked.map(EngineBlocked::from),
        reported_status,
        conversation_agent: clean(conversation_agent),
        conversation_id: clean(conversation_id),
        hibernation: summary.hibernation.map(EngineHibernation::from),
        keep_awake: summary.keep_awake,
        autopilot: summary.autopilot,
        autopilot_nudges: i64::from(summary.autopilot_nudges),
        role,
        work_item: clean(summary.work_item),
        resources: summary.resources.map(EngineResources::from),
        template: clean(summary.template),
        permission_mode: clean(summary.permission_mode),
        posture_lost_on_resume: summary.posture_lost_on_resume,
        stopped_by: clean(stopped_by),
        stop_reason: clean(stop_reason),
        account: clean(summary.account),
        done_at: clean(summary.done_at),
        economics,
        budget,
    }
}

impl From<c::SessionResources> for EngineResources {
    fn from(resources: c::SessionResources) -> Self {
        Self {
            rss_bytes: i64::try_from(resources.rss_bytes).unwrap_or(i64::MAX),
            // `f32` on the wire; its shortest text, not its widened bits.
            cpu_pct: resources
                .cpu_pct
                .to_string()
                .parse()
                .unwrap_or_else(|_| f64::from(resources.cpu_pct)),
            processes: i64::from(resources.processes),
            sampled_at: resources.sampled_at,
            over_threshold: resources.over_threshold,
        }
    }
}

impl From<c::Hibernation> for EngineHibernation {
    fn from(hibernation: c::Hibernation) -> Self {
        Self {
            at: hibernation.at,
            trigger: trigger_str(hibernation.trigger),
            resumable: hibernation.resumable,
            reason: clean(hibernation.reason),
            superseded_by: clean(hibernation.superseded_by),
        }
    }
}

pub(super) fn sweep_entry(payload: &Value) -> Result<EngineSweepEntry, VogtError> {
    let summary = payload
        .get("summary")
        .filter(|value| value.is_object())
        .cloned()
        .unwrap_or(Value::Null);
    let mut object = object_of(payload);
    object.remove("summary");
    strip_nulls(&mut object);
    let wrapper: SweepWrapper = decode("sweep entry", object)?;
    Ok(EngineSweepEntry {
        session: session(&summary)?,
        screen_tail: wrapper.screen_tail,
        ready: wrapper.ready,
    })
}

/// A sweep row without its `summary`, which `session` reads (it keeps the
/// opaque id and the deliberate defaults).
#[derive(serde::Deserialize)]
struct SweepWrapper {
    #[serde(default)]
    screen_tail: Vec<String>,
    #[serde(default)]
    ready: bool,
}

// -- a screen, a wait, changes -----------------------------------------------

pub(super) fn screen(payload: &Value) -> Result<EngineScreen, VogtError> {
    let mut object = object_of(payload);
    let id = object.get("id").map(value_string).unwrap_or_default();
    object.insert("id".to_string(), Value::from(OPAQUE_ID));
    strip_nulls(&mut object);
    // Fields the engine leaves out stay `None` rather than being guessed: the
    // contract's required fields are given placeholders to decode, and the
    // mapping reads only what was sent.
    let sent_activity = object
        .get("activity")
        .and_then(Value::as_str)
        .is_some_and(|text| !text.is_empty());
    let sent_alive = object.get("alive").is_some_and(Value::is_boolean);
    let sent_ready = object.get("ready").is_some_and(Value::is_boolean);
    let sent_row = object
        .get("cursor")
        .and_then(|cursor| cursor.get("row"))
        .is_some_and(Value::is_u64);
    let sent_col = object
        .get("cursor")
        .and_then(|cursor| cursor.get("col"))
        .is_some_and(Value::is_u64);
    // No size, no lines and no cursor read as an empty screen.
    fill(&mut object, "cols", Value::from(0));
    fill(&mut object, "rows", Value::from(0));
    fill(&mut object, "lines", Value::Array(Vec::new()));
    if !object.get("cursor").is_some_and(Value::is_object) {
        object.insert("cursor".to_string(), Value::Object(Map::new()));
    }
    if let Some(cursor) = nested(&mut object, "cursor") {
        fill(cursor, "row", Value::from(0));
        fill(cursor, "col", Value::from(0));
    }
    if !sent_activity {
        object.insert("activity".to_string(), Value::from("idle"));
    }
    fill(&mut object, "alive", Value::from(false));
    fill(&mut object, "ready", Value::from(false));
    if let Some(approval) = nested(&mut object, "approval") {
        prepare_approval(approval);
    }
    if let Some(blocked) = nested(&mut object, "blocked") {
        prepare_blocked(blocked);
    }
    let screen: c::SessionScreen = decode("screen", object)?;
    Ok(EngineScreen {
        id,
        cols: i64::from(screen.cols),
        rows: i64::from(screen.rows),
        lines: screen.lines,
        cursor_row: sent_row.then_some(i64::from(screen.cursor.row)),
        cursor_col: sent_col.then_some(i64::from(screen.cursor.col)),
        title: clean(screen.title),
        activity: sent_activity.then(|| activity_str(screen.activity)),
        alive: sent_alive.then_some(screen.alive),
        ready: sent_ready.then_some(screen.ready),
        scrollback: screen.scrollback,
        turn_started_at: clean(screen.turn_started_at),
        last_output_at: clean(screen.last_output_at),
        approval: screen.approval.map(EngineApproval::from),
        blocked: screen.blocked.map(EngineBlocked::from),
    })
}

pub(super) fn wait(payload: &Value) -> Result<EngineWait, VogtError> {
    let mut object = object_of(payload);
    // A wait that answers without a screen has an empty one.
    let screen_payload = object
        .remove("screen")
        .filter(Value::is_object)
        .unwrap_or(Value::Null);
    strip_nulls(&mut object);
    fill(&mut object, "outcome", Value::from(""));
    fill(&mut object, "matched", Value::from(false));
    fill(&mut object, "waited_ms", Value::from(0));
    // The screen is read by `screen`; the contract's copy is a placeholder.
    object.insert("screen".to_string(), placeholder_screen());
    let wait: c::SessionWait = decode("wait", object)?;
    Ok(EngineWait {
        outcome: wait.outcome,
        matched: wait.matched,
        waited_ms: i64::try_from(wait.waited_ms).unwrap_or(i64::MAX),
        screen: screen(&screen_payload)?,
    })
}

fn placeholder_screen() -> Value {
    serde_json::json!({
        "id": OPAQUE_ID, "cols": 0, "rows": 0, "lines": [],
        "cursor": {"row": 0, "col": 0}, "activity": "idle",
        "alive": false, "ready": false,
    })
}

/// Reads one change's wire object. Returns the session id as the engine sent
/// it, which the contract's `Uuid` cannot carry (see `OPAQUE_ID`).
fn prepare_change(change: &mut Map<String, Value>) -> String {
    let session = change.get("session").map(value_string).unwrap_or_default();
    change.insert("session".to_string(), Value::from(OPAQUE_ID));
    // A change missing its position, time or text reads as empty.
    fill(change, "seq", Value::from(0));
    fill(change, "at", Value::from(""));
    fill(change, "kind", Value::from(""));
    fill(change, "summary", Value::from(""));
    if let Some(escalation) = nested(change, "escalation") {
        // An escalation that names no needs or boundary needs a person and is
        // `other`, as the engine defaults them.
        fill(escalation, "needs", Value::from("person"));
        fill(escalation, "boundary", Value::from("other"));
        known_boundary(escalation);
        fill(escalation, "ask", Value::from(""));
    }
    session
}

fn change_from(change: c::SessionChange, session: String) -> EngineChange {
    EngineChange {
        seq: i64::try_from(change.seq).unwrap_or(i64::MAX),
        at: change.at,
        session,
        kind: change.kind,
        summary: change.summary,
        escalation: change.escalation.map(EngineEscalation::from),
    }
}

impl From<c::ChangeEscalation> for EngineEscalation {
    fn from(escalation: c::ChangeEscalation) -> Self {
        Self {
            needs: needs_str(escalation.needs),
            boundary: escalation.boundary.as_str().to_string(),
            ask: escalation.ask,
            key: escalation.key,
            resolve: escalation.resolve,
        }
    }
}

pub(super) fn changes(payload: &Value) -> Result<EngineChanges, VogtError> {
    let mut object = object_of(payload);
    strip_nulls(&mut object);
    // A page with no changes, cursor or truncation flag is an empty page.
    fill(&mut object, "changes", Value::Array(Vec::new()));
    fill(&mut object, "next_cursor", Value::from(0));
    fill(&mut object, "truncated", Value::from(false));
    let mut sessions = Vec::new();
    if let Some(Value::Array(rows)) = object.get_mut("changes") {
        for row in rows {
            if let Value::Object(row) = row {
                sessions.push(prepare_change(row));
            }
        }
    }
    let page: c::SessionChanges = decode("changes", object)?;
    Ok(EngineChanges {
        changes: page
            .changes
            .into_iter()
            .zip(sessions)
            .map(|(change, session)| change_from(change, session))
            .collect(),
        next_cursor: i64::try_from(page.next_cursor).unwrap_or(i64::MAX),
        truncated: page.truncated,
    })
}

pub(super) fn wait_any(payload: &Value) -> Result<EngineWaitAny, VogtError> {
    let mut object = object_of(payload);
    strip_nulls(&mut object);
    fill(&mut object, "outcome", Value::from(""));
    fill(&mut object, "next_cursor", Value::from(0));
    let mut session = String::new();
    if let Some(change) = nested(&mut object, "change") {
        session = prepare_change(change);
    }
    let answer: c::SessionWaitAny = decode("wait-any", object)?;
    Ok(EngineWaitAny {
        outcome: answer.outcome,
        change: answer.change.map(|change| change_from(change, session)),
        next_cursor: i64::try_from(answer.next_cursor).unwrap_or(i64::MAX),
    })
}

// -- a workspace -------------------------------------------------------------

/// One row of `GET /api/workspaces`: the contract's workspace with the session
/// and sizing the route adds.
#[derive(serde::Deserialize)]
struct WorkspaceRow {
    #[serde(default)]
    session_id: String,
    #[serde(default)]
    alive: bool,
    #[serde(default)]
    size_bytes: Option<i64>,
    #[serde(flatten)]
    workspace: c::Workspace,
}

pub(super) fn workspace(payload: &Value) -> Result<EngineWorkspace, VogtError> {
    let mut object = object_of(payload);
    strip_nulls(&mut object);
    // The listing's creation time is not read by the core.
    fill(&mut object, "created_at", Value::from(""));
    let row: WorkspaceRow = decode("workspace", object)?;
    let workspace = row.workspace;
    Ok(EngineWorkspace {
        session_id: row.session_id,
        alive: row.alive,
        size_bytes: row.size_bytes,
        kind: workspace.kind.as_str().to_string(),
        path: workspace.path,
        repo_root: workspace.repo_root,
        branch: workspace.branch,
        base_sha: workspace.base_sha,
        base_ref: workspace.base_ref,
        cleaned_at: workspace.cleaned_at,
    })
}
