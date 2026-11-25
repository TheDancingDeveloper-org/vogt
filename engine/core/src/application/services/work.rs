//! `work.list`, the native half of `work.create`, and the native half of
//! `work.update`. Ports `list_work`, `_create_native` and `_update_native` in
//! `services/work.py`.
//!
//! On a linked project the list is the forge mirror joined to the overlay; a
//! global list carries every linked project's upstream items alongside the
//! declared rows. An unlinked project scope answers with the CTA marker —
//! empty items and `link_state: "unlinked"` — rather than its native rows,
//! which stay reachable by ref.
//!
//! `work.create` writes a native declared item by default, with or without a
//! project. `upstream=true` on a linked project is the forge write-through:
//! the issue is opened upstream first and the overlay is stored only once the
//! forge accepted it. `upstream=true` on an unlinked project is the
//! decision-10 refusal. `local_only` names the default explicitly.

use serde_json::{json, Value};

use crate::adapters::forge::registry::ForgeClient;
use crate::adapters::forge::{ForgeProvider, RepoRef, WriteBackOutcome, WriteBackResult};
use crate::application::context::AppContext;
use crate::application::delivery_state;
use crate::application::models::views::{
    VerdictFields, VerdictFieldsReviewerStartedBy, VerificationFields,
};
use crate::application::resolve;
use crate::application::services::forge_writeback::writer_provider;
use crate::application::upstream;
use crate::application::writes::{audited_write, WriteOutcome};
use crate::core::{
    Clock, Comment, Effort, IdFactory, Moment, Origin, Priority, Project, TrustState, WorkItem,
    WorkKind, WorkLink, WorkOverlay, WriteBackRecord, TERMINAL_STATES,
};
use crate::errors::VogtError;
use crate::storage::interface::{
    DeclaredStore, ObservedStore, ReadView, WorkFilter, WorkItemUpdate, WriteTxn,
};

const WORK_CREATE: &str = "work.create";
const WORK_CREATED_EVENT: &str = "work.created";
const WORK_UPDATE: &str = "work.update";
const WORK_UPDATED_EVENT: &str = "work.updated";

const UNLINKED_STILL_WORKS: &str = "Native items (WI-n) on this project still take comments, \
     transitions (including to done) and field edits by ref";

crate::typed_op!(create_op, CreateParams, create_work);
crate::typed_op!(update_op, UpdateParams, update_work);
crate::typed_op!(list_op, ListParams, list_work);
crate::typed_op!(get_op, GetParams, get_work);
crate::typed_op!(get_many_op, GetManyParams, get_many_work);
crate::typed_op!(comment_op, CommentParams, comment_work);
crate::typed_op!(transition_op, TransitionParams, transition_work);
crate::typed_op!(relate_op, RelateParams, relate_work);
crate::typed_op!(unrelate_op, RelateParams, unrelate_work);
crate::typed_op!(bind_branch_op, BindBranchParams, bind_branch);
crate::typed_op!(adopt_op, AdoptParams, adopt_work);
crate::typed_op!(board_list_op, BoardParams, list_board);

/// Every wire field of the work operations. Each is held as the JSON the
/// caller sent so the typed parameter types below coerce it with the same
/// leniency, and the same messages, the hand-rolled readers had. `Some(Null)`
/// is a field sent as `null`, which the alias check tells from an absent one.
/// A body that is not an object reads as no fields at all.
#[derive(serde::Deserialize, Default)]
struct Raw {
    #[serde(default, rename = "ref", deserialize_with = "keep")]
    reference: Option<Value>,
    #[serde(default, deserialize_with = "keep")]
    id: Option<Value>,
    #[serde(default, deserialize_with = "keep")]
    kind: Option<Value>,
    #[serde(default, deserialize_with = "keep")]
    title: Option<Value>,
    #[serde(default, deserialize_with = "keep")]
    body: Option<Value>,
    #[serde(default, deserialize_with = "keep")]
    priority: Option<Value>,
    #[serde(default, deserialize_with = "keep")]
    effort: Option<Value>,
    #[serde(default, deserialize_with = "keep")]
    project: Option<Value>,
    #[serde(default, deserialize_with = "keep")]
    initiative: Option<Value>,
    #[serde(default, deserialize_with = "keep")]
    assignee: Option<Value>,
    #[serde(default, deserialize_with = "keep")]
    labels: Option<Value>,
    #[serde(default, deserialize_with = "keep")]
    upstream: Option<Value>,
    #[serde(default, deserialize_with = "keep")]
    local_only: Option<Value>,
    #[serde(default, deserialize_with = "keep")]
    reason: Option<Value>,
    #[serde(default, deserialize_with = "keep")]
    clear_effort: Option<Value>,
    #[serde(default, deserialize_with = "keep")]
    clear_assignee: Option<Value>,
    #[serde(default, deserialize_with = "keep")]
    clear_initiative: Option<Value>,
    #[serde(default, deserialize_with = "keep")]
    add_labels: Option<Value>,
    #[serde(default, deserialize_with = "keep")]
    remove_labels: Option<Value>,
    #[serde(default, deserialize_with = "keep")]
    kinds: Option<Value>,
    #[serde(default, deserialize_with = "keep")]
    states: Option<Value>,
    #[serde(default, deserialize_with = "keep")]
    priorities: Option<Value>,
    #[serde(default, deserialize_with = "keep")]
    label: Option<Value>,
    #[serde(default, deserialize_with = "keep")]
    query: Option<Value>,
    #[serde(default, deserialize_with = "keep")]
    include_finished: Option<Value>,
    #[serde(default, deserialize_with = "keep")]
    with_sessions: Option<Value>,
    #[serde(default, deserialize_with = "keep")]
    mode: Option<Value>,
    #[serde(default, deserialize_with = "keep")]
    limit: Option<Value>,
    #[serde(default, deserialize_with = "keep")]
    offset: Option<Value>,
    #[serde(default, deserialize_with = "keep")]
    refs: Option<Value>,
    #[serde(default, deserialize_with = "keep")]
    comment_limit: Option<Value>,
    #[serde(default, deserialize_with = "keep")]
    comment_order: Option<Value>,
    #[serde(default, deserialize_with = "keep")]
    include: Option<Value>,
    #[serde(default, deserialize_with = "keep")]
    handoff: Option<Value>,
    #[serde(default, deserialize_with = "keep")]
    verdict: Option<Value>,
    #[serde(default, deserialize_with = "keep")]
    verification: Option<Value>,
    #[serde(default, deserialize_with = "keep")]
    to_state: Option<Value>,
    #[serde(default, deserialize_with = "keep")]
    walk: Option<Value>,
    #[serde(default, deserialize_with = "keep")]
    target: Option<Value>,
    #[serde(default, deserialize_with = "keep")]
    related: Option<Value>,
    #[serde(default, deserialize_with = "keep")]
    branch: Option<Value>,
    #[serde(default, deserialize_with = "keep")]
    subject: Option<Value>,
    #[serde(default, deserialize_with = "keep")]
    cells: Option<Value>,
    #[serde(default, deserialize_with = "keep")]
    lane_mode: Option<Value>,
    #[serde(default, deserialize_with = "keep")]
    page_size: Option<Value>,
    #[serde(default, deserialize_with = "keep")]
    snapshot: Option<Value>,
    #[serde(default, deserialize_with = "keep")]
    status: Option<Value>,
    #[serde(default, deserialize_with = "keep")]
    state: Option<Value>,
    #[serde(default, deserialize_with = "keep")]
    text: Option<Value>,
    #[serde(default, deserialize_with = "keep")]
    search: Option<Value>,
    #[serde(default, deserialize_with = "keep")]
    q: Option<Value>,
}

fn keep<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<Value>, D::Error> {
    <Value as serde::Deserialize>::deserialize(deserializer).map(Some)
}

/// The fields of an object body; anything else is no fields at all.
fn raw_of<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Raw, D::Error> {
    use serde::de::Error;
    match <Value as serde::Deserialize>::deserialize(deserializer)? {
        Value::Object(map) => serde_json::from_value(Value::Object(map)).map_err(D::Error::custom),
        _ => Ok(Raw::default()),
    }
}

/// As `raw_of`, but a body that is not an object is refused with `message`.
fn raw_object<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
    message: &str,
) -> Result<Raw, D::Error> {
    use serde::de::Error;
    match <Value as serde::Deserialize>::deserialize(deserializer)? {
        Value::Object(map) => serde_json::from_value(Value::Object(map)).map_err(D::Error::custom),
        _ => Err(D::Error::custom(message)),
    }
}

fn custom<E: serde::de::Error>(err: VogtError) -> E {
    match err {
        VogtError::InvalidRequest(message) => E::custom(message),
        other => E::custom(other),
    }
}

fn str_of(value: &Option<Value>) -> Option<&str> {
    value.as_ref().and_then(Value::as_str)
}

/// A string field; a missing field or `null` is `None` and any other
/// non-string is the empty string (`work.create` and `work.update`).
fn text_lossy(value: &Option<Value>) -> Option<String> {
    match value {
        None | Some(Value::Null) => None,
        Some(Value::String(text)) => Some(text.clone()),
        Some(_) => Some(String::new()),
    }
}

/// A string field; anything that is not a string reads as absent.
fn text_strict(value: &Option<Value>) -> Option<String> {
    str_of(value).map(str::to_string)
}

fn flag(value: &Option<Value>) -> bool {
    value.as_ref().and_then(Value::as_bool).unwrap_or(false)
}

fn names(value: &Option<Value>) -> Vec<String> {
    strings(value.as_ref())
}

fn required_ref(value: Option<String>) -> Result<String, VogtError> {
    value.ok_or_else(|| VogtError::InvalidRequest("ref is required".into()))
}

/// `work.create` parameters.
struct CreateParams {
    kind: String,
    title: String,
    body: String,
    priority: String,
    effort: Option<String>,
    project: Option<String>,
    initiative: Option<String>,
    assignee: Option<String>,
    labels: Vec<String>,
    upstream: bool,
    local_only: bool,
    reason: String,
}

impl<'de> serde::Deserialize<'de> for CreateParams {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = raw_object(deserializer, "work.create takes an object")?;
        let read = || -> Result<Self, VogtError> {
            let kind = text_lossy(&raw.kind)
                .ok_or_else(|| VogtError::InvalidRequest("kind is required".into()))?;
            let title = text_lossy(&raw.title)
                .ok_or_else(|| VogtError::InvalidRequest("title is required".into()))?;
            let priority = text_lossy(&raw.priority).unwrap_or_else(|| "p2".to_string());
            // A string such as "true" is not the boolean the schema declares. The
            // old as_bool().unwrap_or(false) turned it into a native create, which
            // silently drops the upstream half (WI-4061 review).
            let upstream = match &raw.upstream {
                None | Some(Value::Null) => false,
                Some(Value::Bool(value)) => *value,
                Some(_) => {
                    return Err(VogtError::InvalidRequest(
                        "upstream must be a boolean".to_string(),
                    ))
                }
            };
            Ok(Self {
                kind,
                title,
                body: text_lossy(&raw.body).unwrap_or_default(),
                priority,
                effort: text_lossy(&raw.effort),
                project: text_lossy(&raw.project),
                initiative: text_lossy(&raw.initiative),
                assignee: text_lossy(&raw.assignee),
                labels: names(&raw.labels),
                upstream,
                local_only: flag(&raw.local_only),
                reason: text_lossy(&raw.reason).unwrap_or_default(),
            })
        };
        read().map_err(custom)
    }
}

/// `work.update` parameters.
struct UpdateParams {
    reference: String,
    title: Option<String>,
    body: Option<String>,
    priority: Option<String>,
    effort: Option<String>,
    project: Option<String>,
    initiative: Option<String>,
    assignee: Option<String>,
    clear_effort: bool,
    clear_assignee: bool,
    clear_initiative: bool,
    add_labels: Vec<String>,
    remove_labels: Vec<String>,
    reason: String,
}

impl UpdateParams {
    fn touches_labels(&self) -> bool {
        !self.add_labels.is_empty() || !self.remove_labels.is_empty()
    }
}

impl<'de> serde::Deserialize<'de> for UpdateParams {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = raw_object(deserializer, "work.update takes an object")?;
        let reference = text_lossy(&raw.reference)
            .or_else(|| text_lossy(&raw.id))
            .ok_or_else(|| VogtError::InvalidRequest("ref is required".into()))
            .map_err(custom)?;
        Ok(Self {
            reference,
            title: text_lossy(&raw.title),
            body: text_lossy(&raw.body),
            priority: text_lossy(&raw.priority),
            effort: text_lossy(&raw.effort),
            project: text_lossy(&raw.project),
            initiative: text_lossy(&raw.initiative),
            assignee: text_lossy(&raw.assignee),
            clear_effort: flag(&raw.clear_effort),
            clear_assignee: flag(&raw.clear_assignee),
            clear_initiative: flag(&raw.clear_initiative),
            add_labels: names(&raw.add_labels),
            remove_labels: names(&raw.remove_labels),
            reason: text_lossy(&raw.reason).unwrap_or_default(),
        })
    }
}

/// `work.list` parameters, with the aliases applied and a string `states`
/// split on commas.
struct ListParams {
    project: Option<String>,
    kinds: Vec<String>,
    states: Vec<String>,
    priorities: Vec<String>,
    assignee: Option<String>,
    initiative: Option<String>,
    label: Option<String>,
    query: Option<String>,
    include_finished: bool,
    /// Add `live_sessions` to every row (WI-4751). Off by default.
    with_sessions: bool,
    mode: String,
    limit: i64,
    offset: i64,
}

impl Default for ListParams {
    fn default() -> Self {
        Self {
            project: None,
            kinds: Vec::new(),
            states: Vec::new(),
            priorities: Vec::new(),
            assignee: None,
            initiative: None,
            label: None,
            query: None,
            include_finished: false,
            with_sessions: false,
            mode: "summary".to_string(),
            limit: 50,
            offset: 0,
        }
    }
}

impl<'de> serde::Deserialize<'de> for ListParams {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut raw = raw_of(deserializer)?;
        // `apply_aliases`: an alias is accepted only when the canonical field
        // is absent; naming both is refused. An alias sent as `null` still
        // counts as named.
        let mut states = raw.states.take();
        let mut query = raw.query.take();
        let aliased = [
            ("status", raw.status.take(), true),
            ("state", raw.state.take(), true),
            ("text", raw.text.take(), false),
            ("search", raw.search.take(), false),
            ("q", raw.q.take(), false),
        ];
        for (alias, value, is_states) in aliased {
            let Some(value) = value else {
                continue;
            };
            let (slot, field) = if is_states {
                (&mut states, "states")
            } else {
                (&mut query, "query")
            };
            if slot.is_some() {
                return Err(custom(VogtError::InvalidRequest(format!(
                    "work.list: {} is an alias of {field:?}; pass one of them, not both",
                    crate::core::py_repr(alias)
                ))));
            }
            *slot = Some(value);
        }
        let states = match &states {
            Some(Value::String(text)) => text
                .split(',')
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .map(str::to_string)
                .collect(),
            other => strings(other.as_ref()),
        };
        Ok(Self {
            project: text_strict(&raw.project),
            kinds: names(&raw.kinds),
            states,
            priorities: names(&raw.priorities),
            assignee: text_strict(&raw.assignee),
            initiative: text_strict(&raw.initiative),
            label: text_strict(&raw.label),
            query: str_of(&query).map(str::to_string),
            include_finished: flag(&raw.include_finished),
            with_sessions: flag(&raw.with_sessions),
            mode: str_of(&raw.mode).unwrap_or("summary").to_string(),
            limit: raw.limit.as_ref().and_then(Value::as_i64).unwrap_or(50),
            offset: raw.offset.as_ref().and_then(Value::as_i64).unwrap_or(0),
        })
    }
}

/// `work.get` parameters.
struct GetParams {
    reference: String,
    /// None: the default for the shape (3 lean, 200 with `all`).
    comment_limit: Option<i64>,
    newest: bool,
    summary: bool,
    include: Include,
}

/// The sections of `work.get` beyond the item (WI-4726).
#[derive(Clone, Copy, Default)]
struct Include {
    comments: bool,
    sessions: bool,
    branches: bool,
    git: bool,
    delivery: bool,
    relations: bool,
    /// `all`: the full pre-WI-4726 shape, uncapped and with every key.
    all: bool,
}

impl Include {
    /// What an absent `include` returns: the lean read.
    const LEAN: Include = Include {
        comments: true,
        sessions: false,
        branches: false,
        git: false,
        delivery: true,
        relations: true,
        all: false,
    };

    fn parse(value: Option<&Value>) -> Result<Self, VogtError> {
        let Some(value) = value.filter(|value| !value.is_null()) else {
            return Ok(Self::LEAN);
        };
        let items: Vec<&str> = match value {
            Value::Array(items) => items.iter().filter_map(Value::as_str).collect(),
            Value::String(text) => text.split(',').map(str::trim).collect(),
            _ => {
                return Err(VogtError::InvalidRequest(
                    "include must be a list of section names".into(),
                ))
            }
        };
        let mut out = Self::default();
        for item in items {
            match item {
                "comments" => out.comments = true,
                "sessions" => out.sessions = true,
                "branches" => out.branches = true,
                "git" => out.git = true,
                "delivery" => out.delivery = true,
                "relations" => out.relations = true,
                "all" => out.all = true,
                other => {
                    return Err(VogtError::InvalidRequest(format!(
                        "include: unknown section '{other}' (comments, sessions, branches, git, delivery, relations, all)"
                    )))
                }
            }
        }
        if out.all {
            out = Self {
                comments: true,
                sessions: true,
                branches: true,
                git: true,
                delivery: true,
                relations: true,
                all: true,
            };
        }
        Ok(out)
    }
}

impl<'de> serde::Deserialize<'de> for GetParams {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = raw_of(deserializer)?;
        // `ref` wins over `id` even when `ref` is present and not a string.
        let reference = raw
            .reference
            .as_ref()
            .or(raw.id.as_ref())
            .and_then(Value::as_str)
            .map(str::to_string);
        Ok(Self {
            reference: required_ref(reference).map_err(custom)?,
            comment_limit: raw.comment_limit.as_ref().and_then(Value::as_i64),
            newest: str_of(&raw.comment_order) != Some("oldest"),
            summary: str_of(&raw.mode) == Some("summary"),
            include: Include::parse(raw.include.as_ref()).map_err(custom)?,
        })
    }
}

/// `work.get_many` parameters.
struct GetManyParams {
    refs: Vec<String>,
}

impl<'de> serde::Deserialize<'de> for GetManyParams {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = raw_of(deserializer)?;
        Ok(Self {
            refs: names(&raw.refs),
        })
    }
}

/// `work.comment` parameters. The structured field sets stay raw JSON: the
/// handler checks them against `kind` first and only then reads them, and that
/// order decides which message a caller sees.
struct CommentParams {
    reference: String,
    body: String,
    reason: String,
    kind: String,
    handoff: Option<Value>,
    verdict: Option<Value>,
    verification: Option<Value>,
}

impl<'de> serde::Deserialize<'de> for CommentParams {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = raw_of(deserializer)?;
        let read = || -> Result<Self, VogtError> {
            let reference = required_ref(text_strict(&raw.reference))?;
            let body = text_strict(&raw.body)
                .ok_or_else(|| VogtError::InvalidRequest("body is required".into()))?;
            Ok(Self {
                reference,
                body,
                reason: text_strict(&raw.reason).unwrap_or_default(),
                kind: text_strict(&raw.kind).unwrap_or_else(|| "note".to_string()),
                handoff: raw.handoff.filter(|value| !value.is_null()),
                verdict: raw.verdict.filter(|value| !value.is_null()),
                verification: raw.verification.filter(|value| !value.is_null()),
            })
        };
        read().map_err(custom)
    }
}

/// `work.transition` parameters.
struct TransitionParams {
    reference: String,
    to_state: String,
    walk: bool,
    reason: String,
}

impl<'de> serde::Deserialize<'de> for TransitionParams {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = raw_of(deserializer)?;
        let read = || -> Result<Self, VogtError> {
            let reference = required_ref(text_strict(&raw.reference))?;
            let to_state = text_strict(&raw.to_state)
                .ok_or_else(|| VogtError::InvalidRequest("to_state is required".into()))?;
            Ok(Self {
                reference,
                to_state,
                walk: flag(&raw.walk),
                reason: text_strict(&raw.reason).unwrap_or_default(),
            })
        };
        read().map_err(custom)
    }
}

/// `work.relate` and `work.unrelate` parameters.
struct RelateParams {
    reference: String,
    related: String,
    kind_text: String,
    reason: String,
}

impl<'de> serde::Deserialize<'de> for RelateParams {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = raw_of(deserializer)?;
        let read = || -> Result<Self, VogtError> {
            let reference = required_ref(text_strict(&raw.reference))?;
            let related = text_strict(&raw.target)
                .or_else(|| text_strict(&raw.related))
                .ok_or_else(|| VogtError::InvalidRequest("target is required".into()))?;
            Ok(Self {
                reference,
                related,
                kind_text: text_strict(&raw.kind).unwrap_or_else(|| "relates_to".to_string()),
                reason: text_strict(&raw.reason).unwrap_or_default(),
            })
        };
        read().map_err(custom)
    }
}

/// `work.bind_branch` parameters.
struct BindBranchParams {
    reference: String,
    given: Option<String>,
    reason: String,
}

impl<'de> serde::Deserialize<'de> for BindBranchParams {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = raw_of(deserializer)?;
        Ok(Self {
            reference: required_ref(text_strict(&raw.reference)).map_err(custom)?,
            given: text_strict(&raw.branch),
            reason: text_strict(&raw.reason).unwrap_or_default(),
        })
    }
}

/// `work.adopt` parameters.
struct AdoptParams {
    subject: String,
    reason: String,
}

impl<'de> serde::Deserialize<'de> for AdoptParams {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = raw_of(deserializer)?;
        let subject = text_strict(&raw.subject)
            .ok_or_else(|| VogtError::InvalidRequest("subject is required".into()))
            .map_err(custom)?;
        Ok(Self {
            subject,
            reason: text_strict(&raw.reason).unwrap_or_default(),
        })
    }
}

/// One `board.list` cell. A cell that is not an object, or a field that is
/// not a string, reads as blank.
struct Cell {
    lane_key: String,
    state: String,
    /// A cursor was sent (not `null`), whatever its type.
    has_cursor: bool,
    cursor: Option<String>,
}

#[derive(serde::Deserialize, Default)]
struct RawCell {
    #[serde(default)]
    lane_key: Option<Value>,
    #[serde(default)]
    state: Option<Value>,
    #[serde(default)]
    cursor: Option<Value>,
}

impl Cell {
    fn of(value: Value) -> Self {
        let raw: RawCell = serde_json::from_value(value).unwrap_or_default();
        Self {
            lane_key: str_of(&raw.lane_key).unwrap_or("").to_string(),
            state: str_of(&raw.state).unwrap_or("").to_string(),
            has_cursor: raw.cursor.as_ref().is_some_and(|cursor| !cursor.is_null()),
            cursor: text_strict(&raw.cursor),
        }
    }
}

/// `board.list` parameters.
struct BoardParams {
    cells: Vec<Cell>,
    lane_mode: String,
    page_size: i64,
    project: Option<String>,
    kinds: Vec<String>,
    states: Vec<String>,
    priorities: Vec<String>,
    assignee: Option<String>,
    initiative: Option<String>,
    label: Option<String>,
    /// A snapshot was sent (not `null`), whatever its type.
    has_snapshot: bool,
    snapshot: Option<String>,
    /// The digest of the filter, taken from the fields as sent so a token
    /// minted for one filter is refused for another.
    fingerprint: String,
}

impl<'de> serde::Deserialize<'de> for BoardParams {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = raw_of(deserializer)?;
        let cells = match raw.cells {
            Some(Value::Array(values)) => values.into_iter().map(Cell::of).collect(),
            _ => Vec::new(),
        };
        let lane_mode = str_of(&raw.lane_mode).unwrap_or("none").to_string();
        let mut body = serde_json::Map::new();
        for (key, value) in [
            ("project", &raw.project),
            ("assignee", &raw.assignee),
            ("initiative", &raw.initiative),
            ("label", &raw.label),
        ] {
            body.insert(key.into(), value.clone().unwrap_or(Value::Null));
        }
        for (key, value) in [
            ("kinds", &raw.kinds),
            ("states", &raw.states),
            ("priorities", &raw.priorities),
        ] {
            let mut values = names(value);
            values.sort();
            body.insert(key.into(), json!(values));
        }
        body.insert("lane_mode".into(), json!(lane_mode));
        Ok(Self {
            cells,
            lane_mode,
            page_size: raw.page_size.as_ref().and_then(Value::as_i64).unwrap_or(25),
            project: text_strict(&raw.project),
            kinds: names(&raw.kinds),
            states: names(&raw.states),
            priorities: names(&raw.priorities),
            assignee: text_strict(&raw.assignee),
            initiative: text_strict(&raw.initiative),
            label: text_strict(&raw.label),
            has_snapshot: raw.snapshot.as_ref().is_some_and(|value| !value.is_null()),
            snapshot: text_strict(&raw.snapshot),
            fingerprint: crate::decisions::digest_of(&Value::Object(body)),
        })
    }
}

/// The fields of a board token. A token that is not an object is malformed.
#[derive(serde::Deserialize, Default)]
struct Token {
    #[serde(default)]
    kind: Option<Value>,
    #[serde(default)]
    fingerprint: Option<Value>,
    #[serde(default)]
    revision: Option<Value>,
    #[serde(default)]
    high_water: Option<Value>,
    #[serde(default)]
    snapshot_at: Option<Value>,
    #[serde(default)]
    lane_key: Option<Value>,
    #[serde(default)]
    state: Option<Value>,
    #[serde(default)]
    after_created_at: Option<Value>,
    #[serde(default)]
    after_ref: Option<Value>,
}

/// Create a work item. The default, and `local_only`, writes a native declared
/// item, with or without a project. `upstream` on a linked project is the forge
/// write-through (`_create_upstream`). `upstream` on an unlinked project is the
/// decision-10 refusal.
fn create_work<C, I>(ctx: &AppContext<C, I>, params: CreateParams) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    if params.upstream && params.local_only {
        return Err(VogtError::InvalidRequest(
            "give at most one of upstream or local_only: a create is either published on the forge or kept native".to_string(),
        ));
    }
    let project = match params.project.as_deref() {
        None => None,
        Some(slug) => Some(resolve::project(&ctx.declared.read()?, slug)?),
    };
    if params.project.is_none() {
        if params.upstream {
            return Err(VogtError::InvalidRequest(
                "upstream=true needs a project to publish the item to".to_string(),
            ));
        }
        return create_native(ctx, &params, project.as_ref());
    }
    if !params.upstream {
        return create_native(ctx, &params, project.as_ref());
    }
    // A project was resolved above, so this is a project-scoped create.
    let Some(project) = project else {
        return Err(VogtError::InvalidRequest(
            "work.create resolved no project".to_string(),
        ));
    };
    if !upstream::is_linked(&project) {
        return Err(refuse_unlinked(&project, "work.create"));
    }
    create_upstream(ctx, &params, &project)
}

/// Write-through create: the issue first, the overlay only on success. Ports
/// `_create_upstream`.
///
/// Decision 9: the provider call runs before the declared transaction opens,
/// so a network round trip never holds the write lock, and a refusal or
/// failure returns before anything local exists. The crash window left (issue
/// created, local transaction lost) heals on the next sweep.
fn create_upstream<C, I>(
    ctx: &AppContext<C, I>,
    params: &CreateParams,
    project: &Project,
) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    require_permitted(ctx, project, "create", true)?;
    let kind: WorkKind = params.kind.parse().map_err(|_| {
        VogtError::InvalidRequest(format!(
            "unknown work kind {}",
            crate::core::py_repr(&params.kind)
        ))
    })?;
    let priority: Priority = params.priority.parse().map_err(|_| {
        VogtError::InvalidRequest(format!(
            "unknown priority {}",
            crate::core::py_repr(&params.priority)
        ))
    })?;
    let effort = params
        .effort
        .as_deref()
        .map(|value| {
            value.parse::<Effort>().map_err(|_| {
                VogtError::InvalidRequest(format!("unknown effort {}", crate::core::py_repr(value)))
            })
        })
        .transpose()?;
    let labels = params.labels.clone();
    let (workflow, initiative_id, assignee_id) = {
        let view = ctx.declared.read()?;
        let workflow = view.workflow_for(&kind.to_string())?;
        let initiative_id = params
            .initiative
            .as_deref()
            .map(|slug| resolve::initiative(&view, slug).map(|found| found.id))
            .transpose()?;
        let assignee_id = params
            .assignee
            .as_deref()
            .map(|identity| resolve::actor(&view, identity).map(|found| found.id))
            .transpose()?;
        for name in &labels {
            resolve::label_exists(&view, name)?;
        }
        (workflow, initiative_id, assignee_id)
    };

    let (provider, identity, repo) = upstream_writer(ctx, project)?;
    let sent = sent_or_fail(
        provider.create_issue(
            &repo,
            &params.title,
            &params.body,
            (!labels.is_empty()).then_some(labels.as_slice()),
        )?,
        "issue create",
    )?;
    let Some(subject_key) = sent.subject_key.clone().filter(|key| !key.is_empty()) else {
        return Err(VogtError::UpstreamWriteFailed(
            "the forge accepted the issue but returned no number, so there is no subject key to track it under — nothing was stored locally".to_string(),
        ));
    };

    let mut writing = crate::application::context::write_of(ctx);
    let clock = std::sync::Arc::clone(writing.clock());
    let ids = std::sync::Arc::clone(writing.ids());
    let project = project.clone();
    let title = params.title.clone();
    let body = params.body.clone();
    let reason = params.reason.clone();
    let stored = audited_write(
        &mut writing,
        WORK_CREATE,
        &params.reason,
        move |txn, actor| {
            let now = super::now_of(&clock);
            let mut overlay =
                fresh_overlay(txn.work_overlay(&subject_key)?, &subject_key, &project, now);
            overlay.workflow_state = Some(workflow.initial_state.clone());
            overlay.priority = Some(priority);
            overlay.effort = effort;
            overlay.assignee_actor_id = assignee_id.clone();
            overlay.initiative_id = initiative_id.clone();
            txn.upsert_work_overlay(&overlay)?;
            txn.insert_writeback(&upstream_record(
                &ids,
                &clock,
                &actor.id,
                crate::core::WriteBackAction::Create,
                &project,
                Some(&subject_key),
                &reason,
                &identity,
                &sent,
            ))?;
            // The mirror learns about the issue on the next sweep; until then the
            // item is assembled from what was just sent.
            let item = WorkItem {
                id: subject_key.clone(),
                reference: subject_key.clone(),
                kind,
                title: title.clone(),
                body: body.clone(),
                state: workflow.initial_state.clone(),
                priority,
                effort,
                project_id: Some(project.id.clone()),
                project_slug: Some(project.slug.clone()),
                initiative_id: initiative_id.clone(),
                origin: Origin::Observed,
                trust_state: TrustState::Unverified,
                assignee_actor_id: assignee_id.clone(),
                assignee_identity_ref: None,
                labels: labels.clone(),
                relations: Vec::new(),
                superseded_by: None,
                created_at: now,
                updated_at: now,
            };
            let payload = serde_json::to_value(&item).unwrap_or(Value::Null);
            let summary = json!({"ref": subject_key, "kind": kind, "title": title});
            Ok(WriteOutcome::new(
                item,
                "work_item",
                &subject_key,
                payload,
                WORK_CREATED_EVENT,
                summary,
            ))
        },
    )?;
    Ok(json!({
        "item": stored,
        "comments": [],
        "sessions": [],
        "branches": [],
        "git": null,
        "walked": [],
        "live_sessions": [],
    }))
}

fn refuse_unlinked(project: &Project, operation: &str) -> VogtError {
    refuse_unlinked_for(project, operation)
}

/// Decision 10's refusal. `operation` is the phrase the message leads with:
/// `work.create`, or `work.update with labels`.
fn refuse_unlinked_for(project: &Project, operation: &str) -> VogtError {
    VogtError::NotLinked(format!(
        "{operation} needs a forge-linked project, and {} is not linked: link it (`forge link`, or re-import through `project import`) or publish it (`forge publish`) first; or drop `upstream` (the default) to keep a native item in this project. {UNLINKED_STILL_WORKS}.",
        crate::core::py_repr(&project.slug)
    ))
}

/// Every unmet precondition for a write-through, named, in the order Python
/// reports them. Ports `_gaps_for`.
///
/// The chain is the policy, then a forge credential, then whether the
/// project's `repo_url` parses for that provider. Credentials resolve from
/// local configuration (`writer_provider`); nothing here touches the network.
fn write_through_gaps<C, I>(
    ctx: &AppContext<C, I>,
    project: &Project,
    action: &str,
) -> Result<Vec<String>, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let gaps = policy_gap(project, action);
    let actor = ctx
        .declared
        .read()?
        .actor_by_identity(&ctx.principal.identity_ref)?;
    gaps_for(ctx, project, actor.as_ref(), gaps)
}

/// `write_through_gaps` with the acting actor already resolved, after the
/// policy gate. Ports the credential and parse half of `_gaps_for`.
fn gaps_for<C, I>(
    ctx: &AppContext<C, I>,
    project: &Project,
    actor: Option<&crate::core::Actor>,
    mut gaps: Vec<String>,
) -> Result<Vec<String>, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let (provider, _identity) = writer_provider(ctx, actor, project.repo_url.as_deref())?;
    match provider {
        None => gaps.push(
            "no forge credential resolves for this project — link your forge account with `forge account link`, or configure the instance token file".to_string(),
        ),
        Some(provider) if provider.parse(project.repo_url.as_deref()).is_none() => {
            gaps.push(format!(
                "the project's repo_url {} does not parse for the forge provider — correct it with `project update`",
                py_repr_option(project.repo_url.as_deref()),
            ))
        }
        Some(_) => {}
    }
    Ok(gaps)
}

/// Would an explicit upstream `work.create` land in `project` now, and why.
/// Ports `create_writability`: the decision-10 link check, then every
/// write-through precondition, evaluated without writing or touching the
/// network.
pub(crate) fn create_writability<C, I>(
    ctx: &AppContext<C, I>,
    project: &Project,
    actor: Option<&crate::core::Actor>,
) -> Result<(bool, String), VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    if !upstream::is_linked(project) {
        return Ok((
            false,
            "not forge-linked: an upstream work.create (upstream=true) refuses with project_not_linked. Drop upstream for a native record, or link (`forge link`) or publish (`forge publish`) the project.".to_string(),
        ));
    }
    let gaps = gaps_for(ctx, project, actor, policy_gap(project, "create"))?;
    if gaps.is_empty() {
        return Ok((
            true,
            "linked and write-back permits create: work.create with upstream=true opens the issue upstream.".to_string(),
        ));
    }
    Ok((
        false,
        format!(
            "linked, but an upstream work.create refuses with upstream_write_refused: {}. Drop upstream for a native record.",
            gaps.join("; ")
        ),
    ))
}

/// The policy gate of `_gaps_for`: empty when the policy permits `action`.
fn policy_gap(project: &Project, action: &str) -> Vec<String> {
    let policy = project.write_back.to_string();
    if crate::adapters::forge::permits(&policy, action) {
        return Vec::new();
    }
    vec![format!(
        "write-back policy is {}, which does not permit {} — set it with `forge writeback`",
        crate::core::py_repr(&policy),
        crate::core::py_repr(action),
    )]
}

/// Python's `repr` of an optional string: `None` or the quoted text.
fn py_repr_option(value: Option<&str>) -> String {
    value.map_or_else(|| "None".to_string(), crate::core::py_repr)
}

/// Refuse a write-through whose chain is incomplete, naming every gap. Ports
/// `_require_permitted`.
fn require_permitted<C, I>(
    ctx: &AppContext<C, I>,
    project: &Project,
    action: &str,
    offer_local_only: bool,
) -> Result<(), VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let gaps = write_through_gaps(ctx, project, action)?;
    if gaps.is_empty() {
        return Ok(());
    }
    Err(refuse_gaps(project, action, &gaps, offer_local_only))
}

/// The provider, identity and repo a write-through will use, resolved before
/// the declared transaction opens. Ports `_upstream_writer`.
fn upstream_writer<C, I>(
    ctx: &AppContext<C, I>,
    project: &Project,
) -> Result<(ForgeClient, String, RepoRef), VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let actor = ctx
        .declared
        .read()?
        .actor_by_identity(&ctx.principal.identity_ref)?;
    let (provider, identity) = writer_provider(ctx, actor.as_ref(), project.repo_url.as_deref())?;
    let Some(provider) = provider else {
        return Err(VogtError::UpstreamWriteFailed(format!(
            "{} is linked but no usable forge credential resolves for it now — link a forge account or restore the instance token file, then retry",
            crate::core::py_repr(&project.slug)
        )));
    };
    let Some(repo) = provider.parse(project.repo_url.as_deref()) else {
        return Err(VogtError::UpstreamWriteFailed(format!(
            "{} is linked but its repo_url {} no longer parses for the provider",
            crate::core::py_repr(&project.slug),
            py_repr_option(project.repo_url.as_deref()),
        )));
    };
    Ok((provider, identity, repo))
}

/// Decision 9: anything but an upstream success aborts the operation.
fn sent_or_fail(result: WriteBackResult, action: &str) -> Result<WriteBackResult, VogtError> {
    if result.outcome != WriteBackOutcome::Succeeded {
        return Err(VogtError::UpstreamWriteFailed(format!(
            "the forge did not accept the {action}: {} — nothing was changed locally",
            result
                .detail
                .as_deref()
                .filter(|detail| !detail.is_empty())
                .unwrap_or("no detail from the provider"),
        )));
    }
    Ok(result)
}

/// The write-back ledger row for one successful write-through. Ports
/// `_upstream_record`.
#[expect(
    clippy::too_many_arguments,
    reason = "the parameters mirror the call's inputs; a params struct would churn every caller"
)]
fn upstream_record(
    ids: &std::sync::Arc<std::sync::Mutex<impl IdFactory>>,
    clock: &std::sync::Arc<std::sync::Mutex<impl Clock>>,
    actor_id: &str,
    action: crate::core::WriteBackAction,
    project: &Project,
    subject_key: Option<&str>,
    reason: &str,
    identity: &str,
    sent: &WriteBackResult,
) -> WriteBackRecord {
    WriteBackRecord {
        id: super::next_id(ids, "wbk"),
        at: super::now_of(clock),
        project_id: Some(project.id.clone()),
        work_item_id: None,
        actor_id: actor_id.to_string(),
        action,
        subject_key: sent
            .subject_key
            .clone()
            .filter(|key| !key.is_empty())
            .or_else(|| subject_key.map(str::to_string)),
        policy: project.write_back.to_string(),
        outcome: crate::core::WriteBackOutcome::Succeeded,
        reason: reason.to_string(),
        detail: Some(format!("as {identity}")),
        source_url: sent.source_url.clone(),
    }
}

/// The overlay row to build on: the existing one, or an empty one for this
/// subject. The caller sets the changed fields; `updated_at` is `now`.
fn fresh_overlay(
    existing: Option<WorkOverlay>,
    subject_key: &str,
    project: &Project,
    now: crate::core::Moment,
) -> WorkOverlay {
    let mut overlay = existing.unwrap_or_else(|| WorkOverlay {
        subject_key: subject_key.to_string(),
        project_id: project.id.clone(),
        rank: None,
        workflow_state: None,
        priority: None,
        effort: None,
        assignee_actor_id: None,
        initiative_id: None,
        branches: Vec::new(),
        created_at: now,
        updated_at: now,
    });
    overlay.updated_at = now;
    overlay
}

/// `_require_permitted`'s message: every gap numbered, with the create escape
/// hatch when `offer_local_only` is set.
fn refuse_gaps(
    project: &Project,
    action: &str,
    gaps: &[String],
    offer_local_only: bool,
) -> VogtError {
    let enumerated = gaps
        .iter()
        .enumerate()
        .map(|(index, gap)| format!("({}) {gap}", index + 1))
        .collect::<Vec<_>>()
        .join("; ");
    let tail = if offer_local_only {
        " Or pass `local_only=true` (or drop `upstream`) to create the item locally, without upstreaming it now."
    } else {
        ""
    };
    VogtError::UpstreamWriteRefused(format!(
        "cannot {action} on linked project {}: on a linked project the write goes upstream or not at all, and {} precondition(s) are unmet: {enumerated}.{tail}",
        crate::core::py_repr(&project.slug),
        gaps.len(),
    ))
}

/// A native declared item, optionally scoped to a project. `project` set is the
/// `local_only` path: the row belongs to the project but has a `WI-n` ref and
/// `origin="created"`.
fn create_native<C, I>(
    ctx: &AppContext<C, I>,
    params: &CreateParams,
    project: Option<&Project>,
) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let kind: WorkKind = params.kind.parse().map_err(|_| {
        VogtError::InvalidRequest(format!(
            "unknown work kind {}",
            crate::core::py_repr(&params.kind)
        ))
    })?;
    let priority: Priority = params.priority.parse().map_err(|_| {
        VogtError::InvalidRequest(format!(
            "unknown priority {}",
            crate::core::py_repr(&params.priority)
        ))
    })?;
    let effort = params
        .effort
        .as_deref()
        .map(|value| {
            value.parse::<Effort>().map_err(|_| {
                VogtError::InvalidRequest(format!("unknown effort {}", crate::core::py_repr(value)))
            })
        })
        .transpose()?;
    let project_id = project.map(|project| project.id.clone());
    let drawn = Drawn {
        kind,
        title: params.title.clone(),
        body: params.body.clone(),
        priority,
        effort,
        project_id,
        initiative: params.initiative.clone(),
        assignee: params.assignee.clone(),
        labels: params.labels.clone(),
    };

    let mut writing = crate::application::context::write_of(ctx);
    let clock = std::sync::Arc::clone(writing.clock());
    let ids = std::sync::Arc::clone(writing.ids());
    let stored = audited_write(
        &mut writing,
        WORK_CREATE,
        &params.reason,
        move |txn, _actor| {
            let workflow = txn.workflow_for(&drawn.kind.to_string())?;
            let initiative_id = drawn
                .initiative
                .as_deref()
                .map(|slug| resolve::initiative(txn, slug).map(|found| found.id))
                .transpose()?;
            let assignee_id = drawn
                .assignee
                .as_deref()
                .map(|identity| resolve::actor(txn, identity).map(|found| found.id))
                .transpose()?;
            for name in &drawn.labels {
                resolve::label_exists(txn, name)?;
            }
            // Drawn inside the transaction, after the resolutions, which is where
            // Python reads it. The audit row reads the clock again afterwards.
            let now = super::now_of(&clock);
            let id = super::next_id(&ids, "wrk");
            let reference = txn.next_work_ref()?;
            let item = WorkItem {
                id: id.clone(),
                reference: reference.clone(),
                kind: drawn.kind,
                title: drawn.title.clone(),
                body: drawn.body.clone(),
                state: workflow.initial_state,
                priority: drawn.priority,
                effort: drawn.effort,
                project_id: drawn.project_id.clone(),
                project_slug: None,
                initiative_id,
                origin: Origin::Created,
                trust_state: TrustState::Unverified,
                assignee_actor_id: assignee_id,
                assignee_identity_ref: None,
                labels: drawn.labels.clone(),
                relations: Vec::new(),
                superseded_by: None,
                created_at: now,
                updated_at: now,
            };
            txn.insert_work_item(&item)?;
            let stored = txn.work_item_by_id(&id)?.ok_or_else(|| {
                VogtError::NotFound(format!("work item {id} vanished inside its own write"))
            })?;
            let payload = serde_json::to_value(&stored).unwrap_or(Value::Null);
            let summary = json!({"ref": reference, "kind": drawn.kind, "title": drawn.title});
            Ok(WriteOutcome::new(
                stored,
                "work_item",
                &id,
                payload,
                WORK_CREATED_EVENT,
                summary,
            ))
        },
    )?;
    Ok(json!({
        "item": stored,
        "comments": [],
        "sessions": [],
        "branches": [],
        "git": null,
        "walked": [],
        "live_sessions": [],
    }))
}

/// Change a native item's fields. State changes go through `transition`.
///
/// A native item on an unlinked project refuses only when the edit touches
/// labels, which are shared vocabulary with the forge (decision 10); every
/// other field stays a local edit. An item that exists only upstream — the
/// forge mirror, not a declared row — goes through `update_upstream`. The
/// initiative re-projection that follows a committed update is
/// a forge write and is not done here.
fn update_work<C, I>(ctx: &AppContext<C, I>, params: UpdateParams) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let view = ctx.declared.read()?;
    // Declared ref first, then an upstream subject key, then not_found. A
    // typo'd `WI-n` never reaches the write-through: only a ref that resolves
    // to an observed item on a linked project does.
    let resolved = upstream::resolve_work_ref(ctx, &view, &params.reference)?;
    let native = view.work_item_by_ref(&params.reference)?.is_some();
    if !native {
        let project_id = resolved
            .project_id
            .clone()
            .ok_or_else(|| VogtError::NotFound(format!("{} has no project", resolved.reference)))?;
        let project = view
            .project_by_id(&project_id)?
            .ok_or_else(|| VogtError::NotFound(format!("no project {project_id}")))?;
        drop(view);
        let before = resolved.initiative_id.clone();
        let result = update_upstream(ctx, &params, resolved, &project)?;
        reproject_membership(ctx, &params, before, &result)?;
        return Ok(result);
    }
    let item = resolve::work_item(&view, &params.reference)?;
    let before = item.initiative_id.clone();
    if params.touches_labels() {
        if let Some(project_id) = item.project_id.as_deref() {
            if let Some(project) = view.project_by_id(project_id)? {
                if !upstream::is_linked(&project) {
                    return Err(refuse_unlinked_for(&project, "work.update with labels"));
                }
            }
        }
    }
    drop(view);
    let result = update_native(ctx, &params)?;
    reproject_membership(ctx, &params, before, &result)?;
    Ok(result)
}

/// Re-render the task list of any initiative this update joined or left.
/// Ports `_reproject_membership`: adopt-only, after the local write committed,
/// the left initiative first and then the joined one.
fn reproject_membership<C, I>(
    ctx: &AppContext<C, I>,
    params: &UpdateParams,
    before: Option<String>,
    result: &Value,
) -> Result<(), VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let after = result["item"]["initiative_id"].as_str().map(str::to_owned);
    if before == after {
        return Ok(());
    }
    for initiative_id in [before, after].into_iter().flatten() {
        crate::application::services::initiatives::reproject_initiative(
            ctx,
            &initiative_id,
            &params.reason,
        )?;
    }
    Ok(())
}

/// Split an upstream item's update along the boundary decision 2 draws. Ports
/// `_update_upstream`.
///
/// Labels are shared vocabulary: an upstream `add_labels` write, fail-loud.
/// Title and body are upstream truth vogt does not edit, an item cannot move
/// project, and the write surface never removes a label. Priority, effort,
/// assignee and initiative are vogt-local: overlay only, no provider call.
fn update_upstream<C, I>(
    ctx: &AppContext<C, I>,
    params: &UpdateParams,
    item: WorkItem,
    project: &Project,
) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    if params.title.is_some() || params.body.is_some() {
        return Err(VogtError::InvalidRequest(format!(
            "{} is upstream truth: vogt does not edit issue titles or bodies on a linked project (the write-back verb set has no edit) — edit it on the forge",
            item.reference
        )));
    }
    if params.project.is_some() {
        return Err(VogtError::InvalidRequest(format!(
            "{} lives in {}'s repository; an upstream issue cannot be moved to another project",
            item.reference,
            crate::core::py_repr(&project.slug)
        )));
    }
    if !params.remove_labels.is_empty() {
        return Err(VogtError::InvalidRequest(format!(
            "{} is upstream truth and the write surface is append-only: labels can be added upstream, never removed from here — remove them on the forge",
            item.reference
        )));
    }
    let priority = params
        .priority
        .as_deref()
        .map(|value| {
            value.parse::<Priority>().map_err(|_| {
                VogtError::InvalidRequest("priority must be one of p0, p1, p2, p3, p4".to_string())
            })
        })
        .transpose()?;
    let effort = params
        .effort
        .as_deref()
        .map(|value| {
            value.parse::<Effort>().map_err(|_| {
                VogtError::InvalidRequest(format!("unknown effort {}", crate::core::py_repr(value)))
            })
        })
        .transpose()?;
    let add_labels = params.add_labels.clone();
    let (assignee_id, initiative_id) = {
        let view = ctx.declared.read()?;
        for name in &add_labels {
            resolve::label_exists(&view, name)?;
        }
        let assignee_id = params
            .assignee
            .as_deref()
            .map(|identity| resolve::actor(&view, identity).map(|found| found.id))
            .transpose()?;
        let initiative_id = params
            .initiative
            .as_deref()
            .map(|slug| resolve::initiative(&view, slug).map(|found| found.id))
            .transpose()?;
        (assignee_id, initiative_id)
    };

    let mut sent = None;
    let mut identity = String::new();
    if !add_labels.is_empty() {
        require_permitted(ctx, project, "label", false)?;
        let (provider, who, repo) = upstream_writer(ctx, project)?;
        let number = provider.number_of(Some(&item.reference)).ok_or_else(|| {
            VogtError::UpstreamWriteFailed(format!(
                "{} carries no issue number to label upstream",
                item.reference
            ))
        })?;
        sent = Some(sent_or_fail(
            provider.add_labels(&repo, number, &add_labels)?,
            "label add",
        )?);
        identity = who;
    }

    let mut writing = crate::application::context::write_of(ctx);
    let clock = std::sync::Arc::clone(writing.clock());
    let ids = std::sync::Arc::clone(writing.ids());
    let project = project.clone();
    let reason = params.reason.clone();
    let clear_effort = params.clear_effort;
    let clear_assignee = params.clear_assignee;
    let clear_initiative = params.clear_initiative;
    let stored = audited_write(
        &mut writing,
        WORK_UPDATE,
        &params.reason,
        move |txn, actor| {
            let now = super::now_of(&clock);
            let mut overlay = fresh_overlay(
                txn.work_overlay(&item.reference)?,
                &item.reference,
                &project,
                now,
            );
            let mut updated = item.clone();
            if let Some(priority) = priority {
                overlay.priority = Some(priority);
                updated.priority = priority;
            }
            if effort.is_some() || clear_effort {
                overlay.effort = effort;
                updated.effort = effort;
            }
            if assignee_id.is_some() || clear_assignee {
                overlay.assignee_actor_id = assignee_id.clone();
                updated.assignee_actor_id = assignee_id.clone();
            }
            if initiative_id.is_some() || clear_initiative {
                overlay.initiative_id = initiative_id.clone();
                updated.initiative_id = initiative_id.clone();
            }
            txn.upsert_work_overlay(&overlay)?;
            if let Some(sent) = &sent {
                txn.insert_writeback(&upstream_record(
                    &ids,
                    &clock,
                    &actor.id,
                    crate::core::WriteBackAction::Label,
                    &project,
                    Some(&item.reference),
                    &reason,
                    &identity,
                    sent,
                ))?;
            }
            for label in &add_labels {
                if !updated.labels.contains(label) {
                    updated.labels.push(label.clone());
                }
            }
            updated.updated_at = now;
            let payload = serde_json::to_value(&updated).unwrap_or(Value::Null);
            Ok(WriteOutcome::new(
                updated,
                "work_item",
                &item.reference,
                payload,
                WORK_UPDATED_EVENT,
                json!({"ref": item.reference}),
            ))
        },
    )?;
    // Python re-projects initiative membership after the commit; that is a
    // forge write the native path does not do here either.
    Ok(json!({
        "item": stored,
        "comments": [],
        "sessions": [],
        "branches": [],
        "git": null,
        "walked": [],
        "live_sessions": [],
    }))
}

fn update_native<C, I>(ctx: &AppContext<C, I>, params: &UpdateParams) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let mut writing = crate::application::context::write_of(ctx);
    let clock = std::sync::Arc::clone(writing.clock());
    let reference = params.reference.clone();
    let title = params.title.clone();
    let body = params.body.clone();
    let priority = params.priority.clone();
    let effort = params.effort.clone();
    let project = params.project.clone();
    let initiative = params.initiative.clone();
    let assignee = params.assignee.clone();
    let clear_effort = params.clear_effort;
    let clear_assignee = params.clear_assignee;
    let clear_initiative = params.clear_initiative;
    let add_labels = params.add_labels.clone();
    let remove_labels = params.remove_labels.clone();
    let stored = audited_write(
        &mut writing,
        WORK_UPDATE,
        &params.reason,
        move |txn, _actor| {
            let item = resolve::work_item(txn, &reference)?;
            for name in &add_labels {
                resolve::label_exists(txn, name)?;
            }
            let priority = match priority.as_deref() {
                Some(value) => Some(value.parse::<Priority>().map_err(|_| {
                    VogtError::InvalidRequest(
                        "priority must be one of p0, p1, p2, p3, p4".to_string(),
                    )
                })?),
                None => None,
            };
            let effort = match effort.as_deref() {
                Some(value) => Some(value.parse::<Effort>().map_err(|_| {
                    VogtError::InvalidRequest(
                        "effort must be one of trivial, small, medium, large".to_string(),
                    )
                })?),
                None => None,
            };
            let assignee_id = assignee
                .as_deref()
                .map(|identity| resolve::actor(txn, identity).map(|found| found.id))
                .transpose()?;
            let initiative_id = initiative
                .as_deref()
                .map(|slug| resolve::initiative(txn, slug).map(|found| found.id))
                .transpose()?;
            let project_id = project
                .as_deref()
                .map(|slug| resolve::project(txn, slug).map(|found| found.id))
                .transpose()?;
            let now = super::now_of(&clock);
            txn.update_work_item(
                &item.id,
                &WorkItemUpdate {
                    title,
                    body,
                    state: None,
                    priority: priority.map(|value| value.to_string()),
                    effort: effort.map(|value| value.to_string()),
                    assignee_actor_id: assignee_id,
                    initiative_id,
                    project_id,
                    clear_effort,
                    clear_assignee,
                    clear_initiative,
                    add_labels,
                    remove_labels,
                    superseded_by: None,
                },
                now,
            )?;
            let updated = txn
                .work_item_by_id(&item.id)?
                .ok_or_else(|| VogtError::NotFound(format!("no work item {}", item.reference)))?;
            let payload = serde_json::to_value(&updated).unwrap_or(Value::Null);
            Ok(WriteOutcome::new(
                payload.clone(),
                "work_item",
                &item.id,
                payload,
                WORK_UPDATED_EVENT,
                json!({"ref": item.reference}),
            ))
        },
    )?;
    Ok(json!({
        "item": stored,
        "comments": [],
        "sessions": [],
        "branches": [],
        "git": null,
        "walked": [],
        "live_sessions": [],
    }))
}

struct Drawn {
    kind: WorkKind,
    title: String,
    body: String,
    priority: Priority,
    effort: Option<Effort>,
    project_id: Option<String>,
    initiative: Option<String>,
    assignee: Option<String>,
    labels: Vec<String>,
}

/// The total `list_work` reports for its default parameters: non-terminal
/// declared items plus upstream-truth items no `work_link` has adopted.
/// `place.metrics` `work_total` is this number (`list_work(limit=1).total`).
pub(crate) fn list_work_total<C: Clock + 'static, I: IdFactory + 'static>(
    ctx: &AppContext<C, I>,
) -> Result<i64, VogtError> {
    Ok(list_work(
        ctx,
        ListParams {
            limit: 1,
            ..ListParams::default()
        },
    )?["total"]
        .as_i64()
        .unwrap_or(0))
}

fn list_work<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    params: ListParams,
) -> Result<Value, VogtError> {
    let ListParams {
        project,
        kinds,
        states,
        priorities,
        assignee,
        initiative,
        label,
        query,
        include_finished,
        with_sessions,
        mode,
        limit,
        offset,
    } = params;
    let project = project.as_deref();
    let assignee = assignee.as_deref();
    let initiative = initiative.as_deref();
    let label = label.as_deref();
    let query = query.as_deref();
    let include_finished = include_finished
        || states
            .iter()
            .any(|state| TERMINAL_STATES.contains(&state.as_str()));
    let mode = mode.as_str();

    let view = ctx.declared.read()?;
    let project_row = project
        .map(|slug| resolve::project(&view, slug))
        .transpose()?;
    if let Some(project_row) = project_row.as_ref() {
        if !upstream::is_linked(project_row) {
            let mut result = work_page(mode, offset, Vec::new(), 0, Some("unlinked"), None);
            result["detail"] = unlinked_detail(&view, project_row)?
                .map(Value::String)
                .unwrap_or(Value::Null);
            return Ok(result);
        }
    }
    let work_filter = WorkFilter {
        project_id: project_row.as_ref().map(|project| project.id.clone()),
        kinds,
        states,
        priorities,
        assignee_actor_id: assignee
            .map(|identity| resolve::actor(&view, identity).map(|actor| actor.id))
            .transpose()?,
        initiative_id: initiative
            .map(|slug| resolve::initiative(&view, slug).map(|initiative| initiative.id))
            .transpose()?,
        label: label.map(str::to_string),
        text: query.filter(|text| !text.is_empty()).map(str::to_string),
        exclude_terminal: !include_finished,
        limit,
        offset,
        ..WorkFilter::default()
    };
    let mut upstream_rows = Vec::new();
    for linked in upstream::linked_projects(&view, project_row.as_ref())? {
        for item in upstream::upstream_items(ctx, &view, &linked, include_finished, 1000)? {
            if upstream::matches(&item, &work_filter) {
                upstream_rows.push(item);
            }
        }
    }
    let link_state = project_row.as_ref().map(|_| "linked");
    if upstream_rows.is_empty() {
        let page = view.list_work_items(&work_filter)?;
        let total = view.count_work_items(&work_filter)?;
        let sessions = with_sessions
            .then(|| live_sessions_by_item(ctx, &view, &page))
            .transpose()?;
        return Ok(work_page(
            mode,
            offset,
            page,
            total,
            link_state,
            sessions.as_ref(),
        ));
    }
    // Merged paging: the declared page window cannot be pushed into SQL once
    // upstream rows join the list, so both halves are gathered and the one
    // ordering — (created_at, ref), the same the SQL uses — is sliced once.
    let unpaged = WorkFilter {
        limit: work_filter.limit + work_filter.offset + upstream_rows.len() as i64,
        offset: 0,
        ..work_filter.clone()
    };
    let declared_rows = view.list_work_items(&unpaged)?;
    let mut merged = declared_rows;
    merged.extend(upstream_rows.iter().cloned());
    merged.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then(left.reference.cmp(&right.reference))
    });
    let total = view.count_work_items(&work_filter)? + upstream_rows.len() as i64;
    let start = offset.max(0) as usize;
    let page: Vec<WorkItem> = merged
        .into_iter()
        .skip(start)
        .take(limit.max(0) as usize)
        .collect();
    let sessions = with_sessions
        .then(|| live_sessions_by_item(ctx, &view, &page))
        .transpose()?;
    Ok(work_page(
        mode,
        offset,
        page,
        total,
        link_state,
        sessions.as_ref(),
    ))
}

/// Per work-item id, the `live_sessions` object of one `work.list` row.
type LiveSessionsByItem = std::collections::HashMap<String, Value>;

/// `live_sessions` {count, worst, ids} for every item of one page (WI-4751).
///
/// ONE `ReadView` read for the whole page
/// (`unstopped_sessions_for_items`), joined in memory with one engine list.
/// "Live" is `live_sessions_of`'s definition: the engine says the session is
/// alive, or its activity is `hibernated`; an engine that cannot be asked
/// leaves every count at 0. `worst` is the lowest `Attention::order_of` the
/// session rail, the oversight table and `session.sweep` already sort by
/// (`decisions::classify`, the same `stall_after` of 10 minutes).
fn live_sessions_by_item<C, I>(
    ctx: &AppContext<C, I>,
    view: &impl crate::storage::interface::ReadView,
    page: &[WorkItem],
) -> Result<LiveSessionsByItem, VogtError>
where
    C: Clock,
    I: IdFactory,
{
    use crate::decisions::{classify, Attention};
    let item_ids: Vec<String> = page
        .iter()
        .filter(|item| !item.id.is_empty())
        .map(|item| item.id.clone())
        .collect();
    let recorded = view.unstopped_sessions_for_items(&item_ids)?;
    // Only an item with a recorded session needs the engine to be asked.
    let engine_rows = match ctx.engine.as_ref() {
        Some(engine) if !recorded.is_empty() => engine
            .list_sessions_counted(false)
            .map(|(rows, _hidden)| rows)
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    let now = super::now_of(&ctx.clock);
    let mut grouped: std::collections::HashMap<String, Vec<(u8, String, String)>> =
        std::collections::HashMap::new();
    for session in &recorded {
        let Some(item_id) = session.work_item_id.clone() else {
            continue;
        };
        let Some(live) = engine_rows
            .iter()
            .find(|row| row.id == session.engine_session_id)
        else {
            continue;
        };
        if !(live.alive || live.hibernated()) {
            continue;
        }
        let verdict = classify(
            Some(live.activity.as_str()),
            Some(live.alive),
            Some(live.activity == "waiting-for-input"),
            live.approval
                .as_ref()
                .map(|approval| approval.question.as_str()),
            live.blocked.as_ref().map(|blocked| blocked.reason.as_str()),
            live.approval
                .as_ref()
                .map(|approval| approval.kind.as_str()),
            live.last_output_at
                .as_deref()
                .and_then(|text| crate::core::from_iso(text).ok()),
            now,
            10 * 60,
            live.done_at.as_deref(),
        );
        let name = verdict.attention.as_str().to_string();
        grouped.entry(item_id).or_default().push((
            Attention::order_of(&name),
            name,
            session.id.clone(),
        ));
    }
    let mut out = LiveSessionsByItem::new();
    for item in page {
        let rows = grouped.remove(&item.id).unwrap_or_default();
        let worst = rows
            .iter()
            .min_by_key(|(order, _, _)| *order)
            .map(|(_, name, _)| Value::String(name.clone()))
            .unwrap_or(Value::Null);
        out.insert(
            item.id.clone(),
            json!({
                "count": rows.len(),
                "worst": worst,
                "ids": rows.iter().map(|(_, _, id)| id.clone()).collect::<Vec<_>>(),
            }),
        );
    }
    Ok(out)
}

fn work_page(
    mode: &str,
    offset: i64,
    page: Vec<WorkItem>,
    total: i64,
    link_state: Option<&str>,
    sessions: Option<&LiveSessionsByItem>,
) -> Value {
    let following = offset + page.len() as i64;
    let page_empty = page.is_empty();
    let items: Vec<Value> = page
        .iter()
        .map(|item| {
            let mut row = if mode == "full" {
                serde_json::to_value(item).unwrap_or(Value::Null)
            } else {
                summary_row(item)
            };
            if let (Some(sessions), Some(row)) = (sessions, row.as_object_mut()) {
                // An item with no declared id (upstream truth) has no sessions.
                let live = sessions
                    .get(&item.id)
                    .cloned()
                    .unwrap_or_else(|| json!({"count": 0, "worst": null, "ids": []}));
                row.insert("live_sessions".to_string(), live);
            }
            row
        })
        .collect();
    // `WorkListResult`'s field order, every key present. A scoped list and a
    // global one carry the same shape; `link_state` and `detail` are null where
    // they do not apply, and `next_offset` is null on the last page.
    let mut page = serde_json::Map::new();
    page.insert("items".to_string(), Value::Array(items));
    page.insert("total".to_string(), json!(total));
    page.insert("mode".to_string(), Value::String(mode.to_string()));
    page.insert(
        "next_offset".to_string(),
        (!page_empty && following < total)
            .then_some(json!(following))
            .unwrap_or(Value::Null),
    );
    page.insert(
        "link_state".to_string(),
        link_state.map_or(Value::Null, |state| Value::String(state.to_string())),
    );
    page.insert("detail".to_string(), Value::Null);
    Value::Object(page)
}

/// How many refs one `work.get_many` may name. Python's `WORK_GET_MANY_LIMIT`.
const WORK_GET_MANY_LIMIT: usize = 200;

/// The compact row a summary listing and `work.get_many` both return: what a
/// reader scans, nothing more. Python's `WorkItemRow`.
fn summary_row(item: &WorkItem) -> Value {
    json!({
        "ref": item.reference,
        "title": item.title,
        "kind": item.kind.to_string(),
        "state": item.state,
        "priority": item.priority.to_string(),
        "project_slug": item.project_slug,
    })
}

/// Compact rows for a set of refs, in one read: what the session rail's chips
/// ask so a render costs one request rather than one `work.list` per ref
/// (WI-1224). Each ref resolves as `work.get` resolves it; a repeated ref is
/// read once, a ref that names nothing is reported in `missing`, and the rest
/// still come back. Python's `get_many_work`.
fn get_many_work<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    params: GetManyParams,
) -> Result<Value, VogtError> {
    let refs = params.refs;
    if refs.is_empty() || refs.len() > WORK_GET_MANY_LIMIT {
        return Err(VogtError::InvalidRequest(format!(
            "work.get_many takes 1 to {WORK_GET_MANY_LIMIT} refs"
        )));
    }
    if refs
        .iter()
        .any(|reference| reference.is_empty() || reference.len() > 200)
    {
        return Err(VogtError::InvalidRequest(
            "each ref is 1-200 characters".into(),
        ));
    }
    let view = ctx.declared.read()?;
    let mut items = Vec::new();
    let mut missing = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for reference in refs {
        if !seen.insert(reference.clone()) {
            continue;
        }
        match upstream::resolve_work_ref(ctx, &view, &reference) {
            Ok(item) => items.push(summary_row(&item)),
            Err(VogtError::NotFound(_)) => missing.push(reference),
            Err(other) => return Err(other),
        }
    }
    Ok(json!({"items": items, "missing": missing}))
}

/// What an unlinked project's empty list is hiding, and how to reach it. An
/// agent reading only `items: []` would learn that its own native items do not
/// exist; naming the count and a few refs keeps the CTA while pointing at the
/// by-ref path that still works.
fn unlinked_detail(view: &impl ReadView, project: &Project) -> Result<Option<String>, VogtError> {
    let work_filter = WorkFilter {
        project_id: Some(project.id.clone()),
        exclude_terminal: true,
        limit: 5,
        ..WorkFilter::default()
    };
    let total = view.count_work_items(&work_filter)?;
    if total == 0 {
        return Ok(None);
    }
    let refs = view
        .list_work_items(&work_filter)?
        .iter()
        .map(|item| item.reference.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    Ok(Some(format!(
        "{} is not forge-linked, so its {total} open native item(s) are not listed here \
         (e.g. {refs}). {UNLINKED_STILL_WORKS}: work.get, work.comment and work.transition \
         them directly; link or publish the project to list them.",
        crate::core::py_repr(&project.slug)
    )))
}

fn strings(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Comments `work.get` returns when the caller names no `comment_limit` and
/// does not ask for `all` (WI-4726).
const LEAN_COMMENT_LIMIT: i64 = 3;
/// The old default, kept for `include=all`.
const FULL_COMMENT_LIMIT: i64 = 200;
/// Stopped sessions `include=sessions` returns besides the live ones.
const SESSIONS_NEWEST_STOPPED: usize = 3;

/// One work item with the sections `include` names (WI-4726).
///
/// A declared `WI-n` and an upstream subject key resolve the same way. Comments
/// and sessions belong to a declared row; an upstream item has none of either
/// until it is adopted. Branches and the git story are derived reads and come
/// back for either. The default is lean: the item, its newest comments, its
/// relations and its delivery. `include=all` is the full shape.
fn get_work<C: Clock + 'static, I: IdFactory + 'static>(
    ctx: &AppContext<C, I>,
    params: GetParams,
) -> Result<Value, VogtError> {
    let GetParams {
        reference,
        comment_limit,
        newest,
        summary,
        include,
    } = params;
    let limit = comment_limit.unwrap_or(if include.all {
        FULL_COMMENT_LIMIT
    } else {
        LEAN_COMMENT_LIMIT
    });
    let reference = reference.as_str();
    let view = ctx.declared.read()?;
    let mut item = upstream::resolve_work_ref(ctx, &view, reference)?;
    let native = view.work_item_by_ref(reference)?.is_some();
    let mut comments = if include.comments && native && limit > 0 && !item.id.is_empty() {
        view.comments_for(&item.id, limit, newest)?
    } else {
        Vec::new()
    };
    if summary {
        item.body.clear();
        comments.clear();
    }
    if !include.relations {
        item.relations.clear();
    }
    // Branches, then the git story, then the session join. Delivery reads the
    // branches too, so they are derived whenever any of the three is asked for.
    let branches = if include.branches || include.git || include.delivery {
        crate::application::git_story::branch_views_for(ctx, &view, &item)?
    } else {
        Vec::new()
    };
    let mut git = if include.git {
        crate::application::git_story::git_story_for(ctx, &item, &branches)?
    } else {
        None
    };
    if include.branches && !include.all {
        // `git.branches` repeats the top-level `branches`.
        if let Some(Value::Object(story)) = git.as_mut() {
            story.remove("branches");
        }
    }
    let delivery = if include.delivery {
        Some(delivery_of(ctx, &view, &item, native, &branches)?)
    } else {
        None
    };
    let mut out = serde_json::Map::new();
    out.insert("item".into(), json!(item));
    if include.comments || include.all {
        out.insert("comments".into(), json!(comments));
    }
    if include.sessions {
        let mut sessions = sessions_for(ctx, &view, &item)?;
        if !include.all {
            let omitted = cap_sessions(&mut sessions);
            if omitted > 0 {
                out.insert("sessions_omitted".into(), json!(omitted));
            }
        }
        out.insert("sessions".into(), json!(sessions));
    }
    if include.branches {
        out.insert("branches".into(), json!(branches));
    }
    if include.git {
        out.insert("git".into(), json!(git));
    }
    if let Some(delivery) = delivery {
        out.insert("delivery".into(), json!(delivery));
    }
    if include.all {
        out.insert("walked".into(), json!([]));
        out.insert("live_sessions".into(), json!(live_sessions_of(ctx, &item)));
        // Always present, null when no transition recorded a claim. Python's
        // WorkResult serializes the field even when it is None; omitting the
        // key fails a client that decodes the schema.
        out.insert("claim_check".into(), Value::Null);
    }
    Ok(Value::Object(out))
}

/// Keep the live sessions and the newest few stopped ones; return how many
/// were dropped. `rows` come from `sessions_for` (`running`, `started_at`).
fn cap_sessions(rows: &mut Vec<Value>) -> usize {
    let live = |row: &Value| row["stopped_at"].is_null();
    let mut stopped: Vec<(usize, String)> = rows
        .iter()
        .enumerate()
        .filter(|(_, row)| !live(row))
        .map(|(at, row)| {
            (
                at,
                row["started_at"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    stopped.sort_by(|a, b| b.1.cmp(&a.1));
    let keep: std::collections::HashSet<usize> = stopped
        .iter()
        .take(SESSIONS_NEWEST_STOPPED)
        .map(|(at, _)| *at)
        .collect();
    let before = rows.len();
    let mut at = 0;
    rows.retain(|row| {
        let kept = live(row) || keep.contains(&at);
        at += 1;
        kept
    });
    before - rows.len()
}

/// How many of an item's comments the delivery projection reads, newest
/// first. A hand-off or verification older than this is not looked for.
const DELIVERY_COMMENT_LIMIT: i64 = 500;

/// The `delivery` projection (WI-1330): every input is already recorded.
///
/// `landed` is the newest hand-off's branch, observed with nothing ahead of
/// the default branch (every commit of it is on the default branch).
/// `deployed` reads the project's `deployed.versions` lanes. `verified` reads
/// the item's `verification` comments against the policy's lanes.
fn delivery_of<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    view: &impl crate::storage::interface::ReadView,
    item: &WorkItem,
    native: bool,
    branches: &[Value],
) -> Result<crate::application::models::work::Delivery, VogtError> {
    let project = match item.project_id.as_deref() {
        Some(id) => view.project_by_id(id)?,
        None => None,
    };
    let comments = if native {
        view.comments_for(&item.id, DELIVERY_COMMENT_LIMIT, true)?
    } else {
        Vec::new()
    };
    let landed = landed_of(&comments, branches);
    let mut lanes: Vec<delivery_state::LaneView> = Vec::new();
    if let (Some(_), Some(project)) = (&landed, &project) {
        let result = crate::application::deployed::versions(ctx, json!({"project": project.slug}))?;
        lanes = result["lanes"]
            .as_array()
            .map(|rows| rows.iter().map(lane_view_of).collect())
            .unwrap_or_default();
    }
    let policy = delivery_state::VerificationPolicy::from_value(
        project
            .as_ref()
            .and_then(|project| project.agent_policy.verification.as_ref()),
    );
    Ok(delivery_state::project(
        landed,
        &lanes,
        &verification_records(&comments),
        policy
            .as_ref()
            .map_or(&[][..], |policy| policy.lanes.as_slice()),
    ))
}

fn lane_view_of(row: &Value) -> delivery_state::LaneView {
    delivery_state::LaneView {
        name: row["name"].as_str().unwrap_or_default().to_string(),
        status: row["status"].as_str().unwrap_or("unknown").to_string(),
        commits_behind: row["commits_behind"].as_i64(),
        unpromoted_shas: row["unpromoted_commits"]
            .as_array()
            .map(|commits| {
                commits
                    .iter()
                    .filter_map(|commit| commit["sha"].as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
        deployed_sha: row["deployed_sha"].as_str().map(str::to_string),
    }
}

pub(super) fn landed_of(
    comments: &[Comment],
    branches: &[Value],
) -> Option<crate::application::models::work::Landed> {
    let mut newest_first: Vec<&Comment> = comments.iter().collect();
    newest_first.sort_by_key(|comment| std::cmp::Reverse(comment.created_at));
    let handoff = newest_first
        .into_iter()
        .filter(|comment| comment.kind == "handoff")
        .filter_map(|comment| comment.handoff.as_ref())
        .find(|handoff| {
            handoff["branch"]
                .as_str()
                .is_some_and(|branch| !branch.is_empty())
        })?;
    let branch = handoff["branch"].as_str()?;
    let row = branches
        .iter()
        .find(|row| row["name"].as_str() == Some(branch))?;
    let tip = row["tip"].as_str()?;
    if row["ahead"].as_i64() != Some(0) {
        return None;
    }
    let base = row["default_branch"]
        .as_str()
        .filter(|name| !name.is_empty())
        .or_else(|| handoff["base"].as_str().filter(|name| !name.is_empty()))
        .unwrap_or("the default branch");
    Some(crate::application::models::work::Landed {
        sha: tip.to_string(),
        branch: Some(branch.to_string()),
        evidence: format!("branch {branch} tip {tip} has nothing ahead of {base}"),
    })
}

fn verification_records(comments: &[Comment]) -> Vec<delivery_state::VerificationRecord> {
    comments
        .iter()
        .filter(|comment| comment.kind == "verification")
        .filter_map(|comment| {
            let fields = serde_json::from_value(comment.verification.clone()?).ok()?;
            Some(delivery_state::VerificationRecord {
                comment_id: comment.id.clone(),
                at: comment.created_at,
                fields,
            })
        })
        .collect()
}

/// Refuse an agent's close of an item that needs a verification record.
///
/// Needed when the project's `agent_policy.verification.required_for_done` is
/// set and the item is a feature or bug (`delivery_state::VERIFIED_KINDS`). A
/// verification on one of the policy's lanes (any lane when none are listed)
/// lets it through. A person is never refused.
fn require_verification(
    is_agent: bool,
    view: &impl crate::storage::interface::ReadView,
    item: &WorkItem,
) -> Result<(), VogtError> {
    if !is_agent {
        return Ok(());
    }
    let project = match item.project_id.as_deref() {
        Some(id) => view.project_by_id(id)?,
        None => None,
    };
    let policy = delivery_state::VerificationPolicy::from_value(
        project
            .as_ref()
            .and_then(|project| project.agent_policy.verification.as_ref()),
    );
    if !delivery_state::needs_verification(&item.kind.to_string(), policy.as_ref()) {
        return Ok(());
    }
    let Some(policy) = policy else {
        return Ok(());
    };
    let records =
        verification_records(&view.comments_for(&item.id, DELIVERY_COMMENT_LIMIT, true)?);
    if delivery_state::project(None, &[], &records, &policy.lanes)
        .verified
        .is_none()
    {
        let lanes = if policy.lanes.is_empty() {
            String::new()
        } else {
            format!(" on lane {}", policy.lanes.join(", "))
        };
        return Err(VogtError::PersonRequired(format!(
            "{} needs a verification record{lanes} before an agent closes it: {}",
            item.reference,
            delivery_state::VERIFICATION_REQUIRED_MESSAGE
        )));
    }
    Ok(())
}

/// The sessions still bound to `item` and alive or hibernated, as the full
/// session rows the web's `liveSessionsWarning` reads (WI-998, WI-4746).
/// Python's `_warn_live_sessions` defines "live" the same way. A warning,
/// never a refusal: an engine that cannot be asked, or a failed read, yields
/// no rows rather than failing a write that already happened.
fn live_sessions_of<C, I>(ctx: &AppContext<C, I>, item: &WorkItem) -> Vec<Value>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    if item.id.is_empty() {
        return Vec::new();
    }
    let params = super::sessions::ListSessionsParams {
        project: None,
        work_item: Some(item.reference.clone()),
        include_stopped: false,
        limit: 200,
        offset: 0,
        order: "started".to_string(),
        mode: "full".to_string(),
        states: None,
        stall_after_minutes: 10,
        include_hidden: false,
        fields: None,
        started_by_session: None,
    };
    let Ok(listed) = super::sessions::list_sessions(ctx, &params) else {
        return Vec::new();
    };
    listed
        .sessions
        .into_iter()
        .filter(|row| row.alive == Some(true) || row.activity.as_deref() == Some("hibernated"))
        .filter_map(|row| serde_json::to_value(row).ok())
        .collect()
}

/// Sessions bound to a declared item. An upstream item has no declared id, so
/// it has no sessions; the live engine half is not joined here.
fn sessions_for<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    view: &impl crate::storage::interface::ReadView,
    item: &WorkItem,
) -> Result<Vec<Value>, VogtError> {
    if item.id.is_empty() {
        return Ok(Vec::new());
    }
    // Python's session list reads the clock once for the attention of the
    // linked sessions, whether or not the engine answers. Nothing here uses the
    // value yet, but the tick is spent so the next timestamp matches.
    let _now = super::now_of(&ctx.clock);
    let rows = view.list_sessions(None, Some(&item.id), true, 200, 0)?;
    let mut actor_ids: Vec<String> = rows.iter().map(|row| row.actor_id.clone()).collect();
    actor_ids.sort();
    actor_ids.dedup();
    let mut project_ids: Vec<String> = rows.iter().map(|row| row.project_id.clone()).collect();
    project_ids.sort();
    project_ids.dedup();
    let actors: std::collections::HashMap<String, crate::core::Actor> = view
        .actors_by_ids(&actor_ids)?
        .into_iter()
        .map(|actor| (actor.id.clone(), actor))
        .collect();
    let projects: std::collections::HashMap<String, crate::core::Project> = view
        .projects_by_ids(&project_ids)?
        .into_iter()
        .map(|project| (project.id.clone(), project))
        .collect();
    let mut out = Vec::new();
    for session in rows {
        let actor = actors.get(&session.actor_id);
        let project = projects.get(&session.project_id);
        out.push(json!({
            "id": session.id,
            "engine_session_id": session.engine_session_id,
            "project": project.as_ref().map(|project| &project.slug),
            "work_item": item.reference,
            "work_item_title": item.title,
            "work_item_state": item.state,
            "actor": actor.as_ref().map(|actor| &actor.identity_ref),
            "cwd": session.cwd,
            "template": session.template,
            "model": session.model,
            "effort": session.effort,
            "reason": session.reason,
            "started_at": session.started_at,
            "stopped_at": session.stopped_at,
            "running": session.stopped_at.is_none() && ctx.engine.is_some(),
            "engine_available": ctx.engine.is_some(),
        }));
    }
    Ok(out)
}

const WORK_COMMENT: &str = "work.comment";
const WORK_COMMENTED: &str = "work.commented";
const WORK_TRANSITION: &str = "work.transition";
const WORK_TRANSITIONED: &str = "work.transitioned";
const WORK_RELATE: &str = "work.relate";
const WORK_RELATED: &str = "work.related";
const WORK_UNRELATE: &str = "work.unrelate";
const WORK_UNRELATED: &str = "work.unrelated";
const WORK_BIND: &str = "work.bind_branch";
const WORK_BRANCH_BOUND: &str = "work.branch_bound";
const WORK_ADOPT: &str = "work.adopt";
const WORK_ADOPTED: &str = "work.adopted";

/// Append a comment to a native item. `kind` is `note`, `handoff`, `verdict`
/// or `verification`; each structured kind stores its own fields, and a
/// verdict or verification is first checked against who is posting it. The write-back record is written
/// in the same transaction (`writeback.attempt`), outcome `skipped` while no
/// forge provider is wired. An upstream item is the write-through and stores
/// nothing.
fn comment_work<C, I>(ctx: &AppContext<C, I>, params: CommentParams) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let CommentParams {
        reference,
        body,
        reason,
        kind,
        handoff,
        verdict,
        verification,
    } = params;
    if !COMMENT_KINDS.contains(&kind.as_str()) {
        return Err(VogtError::InvalidRequest(format!(
            "unknown comment kind {}",
            crate::core::py_repr(&kind)
        )));
    }
    let sent = [
        ("handoff", handoff.is_some()),
        ("verdict", verdict.is_some()),
        ("verification", verification.is_some()),
    ];
    for (field, present) in sent {
        if kind != field && present {
            return Err(VogtError::InvalidRequest(format!(
                "{field} fields belong on a comment of kind {field}"
            )));
        }
    }
    let verdict = structured::<VerdictFields>(verdict, "verdict")?;
    let verification = structured::<VerificationFields>(verification, "verification")?;
    let view = ctx.declared.read()?;
    let resolved = upstream::resolve_work_ref(ctx, &view, &reference)?;
    if view.work_item_by_ref(&reference)?.is_none() {
        let project = resolved
            .project_id
            .as_deref()
            .map(|id| view.project_by_id(id))
            .transpose()?
            .flatten()
            .ok_or_else(|| VogtError::NotFound(format!("no project for {reference}")))?;
        drop(view);
        if kind != "note" {
            // The forge thread has no kind: a hand-off posted there would
            // flatten to free text and the next session could not find it.
            return Err(VogtError::InvalidRequest(format!(
                "{} is an upstream item; a {kind} comment is a local record and cannot be posted to the forge",
                resolved.reference
            )));
        }
        return comment_upstream(ctx, &resolved, &project, &body, &reason);
    }
    // The forge is called before the declared transaction opens; a failed
    // post is recorded and the local comment still lands.
    let item = resolve::work_item(&view, &reference)?;
    let project = item
        .project_id
        .as_deref()
        .map(|id| view.project_by_id(id))
        .transpose()?
        .flatten();
    let subject = write_back_subject(
        linked_subject(&view, &item.id)?,
        item.superseded_by.as_deref(),
    );
    drop(view);
    let writer = native_writer(ctx, &reference)?;
    // Resolved before the write: the caller's own session row, read from its
    // token, never from the parameters.
    let agent = ctx.principal.kind == crate::core::ActorKind::Agent;
    let engine_id = if agent && (kind == "verdict" || kind == "verification") {
        crate::application::session_commit::caller_engine_session_id(ctx)?
    } else {
        None
    };
    let caller = Caller {
        agent,
        identity_ref: ctx.principal.identity_ref.clone(),
        engine_id,
    };
    let plan = plan_writeback(
        crate::core::WriteBackAction::Comment,
        project.as_ref(),
        Some(&item.id),
        subject.as_deref(),
        writer.as_ref(),
        Some(&format!(
            "{body}\n\n— posted from Vogt as {}",
            item.reference
        )),
    );
    let mut writing = crate::application::context::write_of(ctx);
    let clock = std::sync::Arc::clone(writing.clock());
    let ids = std::sync::Arc::clone(writing.ids());
    let clock_for_record = std::sync::Arc::clone(&clock);
    let ids_for_record = std::sync::Arc::clone(&ids);
    let reason_for_record = reason.clone();
    let (stored, outcome) =
        audited_write(&mut writing, WORK_COMMENT, &reason, move |txn, actor| {
            let item = resolve::work_item(txn, &reference)?;
            let (verdict, verification) =
                authorize_structured(txn, &caller, &item, verdict, verification)?;
            let now = super::now_of(&clock);
            let comment = Comment {
                id: super::next_id(&ids, "cmt"),
                work_item_id: item.id.clone(),
                actor_id: actor.id.clone(),
                actor_display_name: actor.display_name.clone(),
                body: body.clone(),
                created_at: now,
                kind: kind.clone(),
                handoff: handoff.clone(),
                verdict,
                verification,
            };
            txn.insert_comment(&comment)?;
            let record = record_writeback(
                txn,
                &ids_for_record,
                &clock_for_record,
                &actor.id,
                &reason_for_record,
                plan,
            )?;
            let outcome = record.outcome.to_string();
            let comment_id = comment.id.clone();
            let payload = serde_json::to_value(&comment).unwrap_or(Value::Null);
            Ok(WriteOutcome::new(
                (comment, outcome),
                "comment",
                &comment_id,
                payload,
                WORK_COMMENTED,
                json!({"ref": item.reference, "comment_id": comment_id}),
            ))
        })?;
    Ok(json!({
        "comment": stored,
        "write_back": outcome,
    }))
}

/// `comment_work` for in-process callers that build the wire JSON themselves
/// (the budget hand-off notes).
pub(crate) fn comment_json<C, I>(ctx: &AppContext<C, I>, params: Value) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    comment_work(ctx, crate::application::typed::decode(params)?)
}

const COMMENT_KINDS: [&str; 4] = ["note", "handoff", "verdict", "verification"];

const VERDICT_RULE: &str = "a verdict is posted by a person, or by a review session started by a person or oversight, on the item it reviews";
const VERIFICATION_RULE: &str = "a verification is posted by someone other than the worker";

/// Who is commenting, read from the principal before the write opens.
struct Caller {
    agent: bool,
    identity_ref: String,
    engine_id: Option<String>,
}

/// A structured field set off the (already validated) parameters.
fn structured<T: serde::de::DeserializeOwned>(
    value: Option<Value>,
    field: &str,
) -> Result<Option<T>, VogtError> {
    value
        .map(|value| {
            serde_json::from_value(value)
                .map_err(|_| VogtError::InvalidRequest(format!("{field} fields are malformed")))
        })
        .transpose()
}

/// Whether any of `ids` is the session named by the item's latest hand-off.
fn is_worker(view: &impl ReadView, item: &WorkItem, ids: &[&str]) -> Result<bool, VogtError> {
    for prior in view.comments_for(&item.id, 500, true)? {
        if prior.kind == "handoff" {
            if let Some(handoff) = &prior.handoff {
                return Ok(handoff["session"]
                    .as_str()
                    .is_some_and(|session| ids.contains(&session)));
            }
        }
    }
    Ok(false)
}

/// Who may post a verdict or verification (WI-1319), and the filled fields.
/// Ports `_authorize_structured`: every fact comes from the acting principal
/// and the session store; the reviewer and `by` fields a caller supplies are
/// overwritten.
fn authorize_structured(
    view: &impl ReadView,
    caller: &Caller,
    item: &WorkItem,
    verdict: Option<VerdictFields>,
    verification: Option<VerificationFields>,
) -> Result<(Option<Value>, Option<Value>), VogtError> {
    let acting = || -> Result<Option<crate::core::CodingSession>, VogtError> {
        match caller.engine_id.as_deref() {
            Some(id) => view.session_by_engine_id(id),
            None => Ok(None),
        }
    };
    if let Some(mut verdict) = verdict {
        if !caller.agent {
            verdict.reviewer_session = None;
            verdict.reviewer_started_by = Some(VerdictFieldsReviewerStartedBy::Person);
            return Ok((Some(to_value(&verdict)), None));
        }
        let refused = |why: String| VogtError::CommentRefused(format!("{VERDICT_RULE}: {why}"));
        let session = acting()?.ok_or_else(|| refused("this caller is not a session".into()))?;
        if session.purpose.as_deref() != Some("review") {
            return Err(refused("this session is not a review".into()));
        }
        let started_by = match session.started_by_kind.as_deref() {
            Some("person") => VerdictFieldsReviewerStartedBy::Person,
            Some("oversight") => VerdictFieldsReviewerStartedBy::Oversight,
            other => {
                return Err(refused(format!(
                    "this review was started by {}",
                    other
                        .filter(|text| !text.is_empty())
                        .unwrap_or("an unknown starter")
                )))
            }
        };
        if session.work_item_id.as_deref() != Some(item.id.as_str()) {
            return Err(refused(format!(
                "this review is not bound to {}",
                item.reference
            )));
        }
        if is_worker(view, item, &[&session.id, &session.engine_session_id])? {
            return Err(refused("this session is the worker".into()));
        }
        verdict.reviewer_session = Some(session.engine_session_id);
        verdict.reviewer_started_by = Some(started_by);
        return Ok((Some(to_value(&verdict)), None));
    }
    if let Some(mut verification) = verification {
        let session = acting()?;
        if let Some(session) = &session {
            if is_worker(view, item, &[&session.id, &session.engine_session_id])? {
                return Err(VogtError::CommentRefused(VERIFICATION_RULE.into()));
            }
        }
        verification.by = Some(match session {
            None => caller.identity_ref.clone(),
            Some(session) => session.engine_session_id,
        });
        return Ok((None, Some(to_value(&verification))));
    }
    Ok((None, None))
}

fn to_value<T: serde::Serialize>(fields: &T) -> Value {
    serde_json::to_value(fields).unwrap_or(Value::Null)
}

/// Post a comment on an upstream item's forge thread. Ports
/// `_comment_upstream`: the forge thread is the comment's home, so only the
/// write-back row is stored and the comment is returned, not kept.
fn comment_upstream<C, I>(
    ctx: &AppContext<C, I>,
    item: &WorkItem,
    project: &Project,
    body: &str,
    reason: &str,
) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    require_permitted(ctx, project, "comment", false)?;
    let (provider, identity, repo) = upstream_writer(ctx, project)?;
    let number = provider.number_of(Some(&item.reference)).ok_or_else(|| {
        VogtError::UpstreamWriteFailed(format!(
            "{} carries no issue number to comment on upstream",
            item.reference
        ))
    })?;
    let sent = sent_or_fail(provider.comment(&repo, number, body)?, "comment")?;
    let mut writing = crate::application::context::write_of(ctx);
    let clock = std::sync::Arc::clone(writing.clock());
    let ids = std::sync::Arc::clone(writing.ids());
    let project = project.clone();
    let reference = item.reference.clone();
    let body = body.to_string();
    let reason_for_record = reason.to_string();
    let (comment, outcome) =
        audited_write(&mut writing, WORK_COMMENT, reason, move |txn, actor| {
            let record = upstream_record(
                &ids,
                &clock,
                &actor.id,
                crate::core::WriteBackAction::Comment,
                &project,
                Some(&reference),
                &reason_for_record,
                &identity,
                &sent,
            );
            txn.insert_writeback(&record)?;
            let comment = Comment {
                id: super::next_id(&ids, "cmt"),
                work_item_id: reference.clone(),
                actor_id: actor.id.clone(),
                actor_display_name: actor.display_name.clone(),
                body: body.clone(),
                created_at: super::now_of(&clock),
                kind: "note".to_string(),
                handoff: None,
                verdict: None,
                verification: None,
            };
            let comment_id = comment.id.clone();
            let payload = serde_json::to_value(&comment).unwrap_or(Value::Null);
            Ok(WriteOutcome::new(
                (comment, record.outcome.to_string()),
                "comment",
                &comment_id,
                payload,
                WORK_COMMENTED,
                json!({"ref": reference, "comment_id": comment_id}),
            ))
        })?;
    Ok(json!({"comment": comment, "write_back": outcome}))
}

/// One workflow edge of an upstream item. Ports `_transition_upstream`:
/// entering a terminal state closes the issue and leaving one reopens it,
/// fail-loud; a move between open states is overlay-only and sends nothing.
/// The forge write a hop from `from` to `to` makes: entering a terminal state
/// closes the issue, leaving one reopens it, anything else writes nothing.
fn upstream_verb(from: &str, to: &str) -> Option<&'static str> {
    if TERMINAL_STATES.contains(&to) {
        Some("close")
    } else if TERMINAL_STATES.contains(&from) {
        Some("reopen")
    } else {
        None
    }
}

fn transition_upstream<C, I>(
    ctx: &AppContext<C, I>,
    item: WorkItem,
    project: &Project,
    to_state: &str,
    reason: &str,
) -> Result<WorkItem, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let workflow = ctx.declared.read()?.workflow_for(&item.kind.to_string())?;
    workflow.check(&item.state, to_state)?;
    let action = upstream_verb(&item.state, to_state).map(|verb| {
        let kind = if verb == "close" {
            crate::core::WriteBackAction::Close
        } else {
            crate::core::WriteBackAction::Reopen
        };
        (verb, kind)
    });
    let mut sent = None;
    let mut identity = String::new();
    if let Some((verb, _)) = action {
        require_permitted(ctx, project, verb, false)?;
        let (provider, who, repo) = upstream_writer(ctx, project)?;
        let number = provider.number_of(Some(&item.reference)).ok_or_else(|| {
            VogtError::UpstreamWriteFailed(format!(
                "{} carries no issue number to {verb} upstream",
                item.reference
            ))
        })?;
        let state = if verb == "close" { "closed" } else { "open" };
        sent = Some(sent_or_fail(
            provider.set_state(&repo, number, state)?,
            verb,
        )?);
        identity = who;
    }
    let mut writing = crate::application::context::write_of(ctx);
    let clock = std::sync::Arc::clone(writing.clock());
    let ids = std::sync::Arc::clone(writing.ids());
    let project = project.clone();
    let to_state = to_state.to_string();
    let reason_for_record = reason.to_string();
    audited_write(&mut writing, WORK_TRANSITION, reason, move |txn, actor| {
        let now = super::now_of(&clock);
        let mut overlay = fresh_overlay(
            txn.work_overlay(&item.reference)?,
            &item.reference,
            &project,
            now,
        );
        overlay.workflow_state = Some(to_state.clone());
        txn.upsert_work_overlay(&overlay)?;
        if let (Some(sent), Some((_, kind))) = (&sent, action) {
            txn.insert_writeback(&upstream_record(
                &ids,
                &clock,
                &actor.id,
                kind,
                &project,
                Some(&item.reference),
                &reason_for_record,
                &identity,
                sent,
            ))?;
        }
        let mut summary = serde_json::Map::new();
        summary.insert("ref".into(), json!(item.reference));
        summary.insert("from".into(), json!(item.state));
        summary.insert("to".into(), json!(to_state));
        summary.insert("kind".into(), json!(item.kind.to_string()));
        summary.insert("labels".into(), json!(item.labels));
        summary.insert("project".into(), json!(project.slug));
        let mut updated = item.clone();
        updated.state = to_state.clone();
        updated.updated_at = now;
        let payload = serde_json::to_value(&updated).unwrap_or(Value::Null);
        Ok(WriteOutcome::new(
            updated,
            "work_item",
            &item.reference,
            payload,
            WORK_TRANSITIONED,
            Value::Object(summary),
        ))
    })
}

/// Move a native item along its workflow. `walk` takes the shortest legal path
/// one audited hop at a time. Completion is refused while a `depends_on`
/// target is unfinished. An upstream item is the write-through and stores
/// nothing.
fn transition_work<C, I>(
    ctx: &AppContext<C, I>,
    params: TransitionParams,
) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let TransitionParams {
        reference,
        to_state,
        walk,
        reason,
    } = params;
    let view = ctx.declared.read()?;
    let resolved = upstream::resolve_work_ref(ctx, &view, &reference)?;
    if view.work_item_by_ref(&reference)?.is_none() {
        let project = resolved
            .project_id
            .as_deref()
            .map(|id| view.project_by_id(id))
            .transpose()?
            .flatten()
            .ok_or_else(|| VogtError::NotFound(format!("no project for {reference}")))?;
        let workflow = view.workflow_for(&resolved.kind.to_string())?;
        drop(view);
        let path = if walk {
            workflow
                .shortest_path(&resolved.state, &to_state)
                .ok_or_else(|| {
                    VogtError::InvalidRequest(format!(
                        "no path from {} to {to_state}",
                        resolved.state
                    ))
                })?
        } else {
            vec![resolved.state.clone(), to_state.clone()]
        };
        // Check every hop's write-back policy before moving anything, as
        // Python does, so a refused close or reopen cannot stop a walk
        // half-way with the forge already changed.
        for window in path.windows(2) {
            if let Some(verb) = upstream_verb(&window[0], &window[1]) {
                require_permitted(ctx, &project, verb, false)?;
            }
        }
        let hops = path.len().saturating_sub(1);
        let mut current = resolved;
        for (number, window) in path.windows(2).enumerate() {
            let hop_reason = if walk {
                format!(
                    "{reason} (walk {}/{hops}: {} -> {})",
                    number + 1,
                    window[0],
                    window[1]
                )
            } else {
                reason.clone()
            };
            current = transition_upstream(ctx, current, &project, &window[1], &hop_reason)?;
        }
        return Ok(json!({
            "item": current,
            "comments": [],
            "sessions": [],
            "branches": [],
            "git": null,
            "walked": if walk { json!(path) } else { json!([]) },
            "live_sessions": [],
        }));
    }
    let item = resolve::work_item(&view, &reference)?;
    let is_agent = ctx.principal.kind == crate::core::ActorKind::Agent;
    // A walk is refused before its first hop (WI-1330); a single move is
    // judged in its own transaction below, after the edge and the blockers.
    if walk && to_state == "done" {
        require_verification(is_agent, &view, &item)?;
    }
    let workflow = view.workflow_for(&item.kind.to_string())?;
    let path = if walk {
        workflow
            .shortest_path(&item.state, &to_state)
            .ok_or_else(|| {
                VogtError::InvalidRequest(format!("no path from {} to {to_state}", item.state))
            })?
    } else {
        workflow.check(&item.state, &to_state)?;
        vec![item.state.clone(), to_state.clone()]
    };
    if path.len() >= 2 && crate::core::TERMINAL_STATES.contains(&path.last().unwrap().as_str()) {
        let blockers = view.unfinished_blockers(&item.id, crate::core::TERMINAL_STATES)?;
        if !blockers.is_empty() {
            let names = blockers
                .iter()
                .map(|blocker| format!("{} ({})", blocker.r#ref, blocker.state))
                .collect::<Vec<_>>()
                .join(", ");
            return Err(crate::core::transition_rejected(
                "transition.blocked",
                &format!(
                    "{} is blocked by unfinished dependencies: {names}",
                    item.reference
                ),
            ));
        }
    }
    let hops = path.len().saturating_sub(1);
    drop(view);
    let mut current = item;
    for (number, window) in path.windows(2).enumerate() {
        let from = window[0].clone();
        let to = window[1].clone();
        let hop_reason = if walk {
            format!("{reason} (walk {}/{hops}: {from} -> {to})", number + 1)
        } else {
            reason.clone()
        };
        let mut writing = crate::application::context::write_of(ctx);
        let clock = std::sync::Arc::clone(writing.clock());
        let ids = std::sync::Arc::clone(writing.ids());
        let id = current.id.clone();
        let reference = current.reference.clone();
        let entering = crate::core::TERMINAL_STATES.contains(&to.as_str())
            && !crate::core::TERMINAL_STATES.contains(&from.as_str());
        let leaving = crate::core::TERMINAL_STATES.contains(&from.as_str())
            && !crate::core::TERMINAL_STATES.contains(&to.as_str());
        let hop_for_record = hop_reason.clone();
        let is_agent = ctx.principal.kind == crate::core::ActorKind::Agent;
        let action = if entering {
            Some(crate::core::WriteBackAction::Close)
        } else if leaving {
            Some(crate::core::WriteBackAction::Reopen)
        } else {
            None
        };
        // The forge is called before the declared transaction opens; a
        // failed close or reopen is recorded and the local move still lands.
        let plan = match action {
            Some(action) => {
                let view = ctx.declared.read()?;
                let project = current
                    .project_id
                    .as_deref()
                    .map(|id| view.project_by_id(id))
                    .transpose()?
                    .flatten();
                let subject = write_back_subject(
                    linked_subject(&view, &current.id)?,
                    current.superseded_by.as_deref(),
                );
                drop(view);
                let writer = native_writer(ctx, &reference)?;
                Some(plan_writeback(
                    action,
                    project.as_ref(),
                    Some(&current.id),
                    subject.as_deref(),
                    writer.as_ref(),
                    None,
                ))
            }
            None => None,
        };
        current = audited_write(
            &mut writing,
            WORK_TRANSITION,
            &hop_reason,
            move |txn, actor| {
                let fresh = txn
                    .work_item_by_id(&id)?
                    .ok_or_else(|| VogtError::NotFound(format!("no work item {reference}")))?;
                let workflow = txn.workflow_for(&fresh.kind.to_string())?;
                workflow.check(&fresh.state, &to)?;
                if crate::core::TERMINAL_STATES.contains(&to.as_str()) {
                    let blockers =
                        txn.unfinished_blockers(&fresh.id, crate::core::TERMINAL_STATES)?;
                    if !blockers.is_empty() {
                        let names = blockers
                            .iter()
                            .map(|blocker| format!("{} ({})", blocker.r#ref, blocker.state))
                            .collect::<Vec<_>>()
                            .join(", ");
                        return Err(crate::core::transition_rejected(
                            "transition.blocked",
                            &format!("{reference} is blocked by unfinished dependencies: {names}"),
                        ));
                    }
                }
                if to == "done" {
                    require_verification(is_agent, txn, &fresh)?;
                }
                let now = super::now_of(&clock);
                txn.update_work_item(
                    &fresh.id,
                    &WorkItemUpdate {
                        state: Some(to.clone()),
                        ..WorkItemUpdate::default()
                    },
                    now,
                )?;
                let updated = txn
                    .work_item_by_id(&fresh.id)?
                    .ok_or_else(|| VogtError::NotFound(format!("no work item {reference}")))?;
                let project = updated
                    .project_id
                    .as_deref()
                    .map(|id| txn.project_by_id(id))
                    .transpose()?
                    .flatten();
                if let Some(plan) = plan {
                    record_writeback(txn, &ids, &clock, &actor.id, &hop_for_record, plan)?;
                }
                let mut summary = serde_json::Map::new();
                summary.insert("ref".into(), json!(reference));
                summary.insert("from".into(), json!(from));
                summary.insert("to".into(), json!(to));
                summary.insert("kind".into(), json!(updated.kind.to_string()));
                summary.insert("labels".into(), json!(updated.labels));
                if let Some(project) = project.as_ref() {
                    summary.insert("project".into(), json!(project.slug));
                }
                let payload = serde_json::to_value(&updated).unwrap_or(Value::Null);
                Ok(WriteOutcome::new(
                    updated,
                    "work_item",
                    &fresh.id,
                    payload,
                    WORK_TRANSITIONED,
                    Value::Object(summary),
                ))
            },
        )?;
    }
    // Only a move into a finished state warns (WI-998); nothing is unbound.
    let live = if TERMINAL_STATES.contains(&current.state.as_str()) {
        live_sessions_of(ctx, &current)
    } else {
        Vec::new()
    };
    Ok(json!({
        "item": current,
        "comments": [],
        "sessions": [],
        "branches": [],
        "git": null,
        "walked": if walk { path } else { Vec::<String>::new() },
        "live_sessions": live,
    }))
}

/// Record a relation between two declared items. `implemented_by` is observed
/// only and refused. A duplicate edge is a conflict. An upstream ref is
/// refused as an invalid request, not a not-found.
fn relate_work<C, I>(ctx: &AppContext<C, I>, params: RelateParams) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let RelateParams {
        reference,
        related,
        kind_text,
        reason,
    } = params;
    if !matches!(
        kind_text.as_str(),
        "depends_on" | "relates_to" | "duplicate_of" | "parent_of"
    ) {
        return Err(VogtError::InvalidRequest(format!(
            "{kind_text} is not a declarable relation; implemented_by is observed from the forge"
        )));
    }
    let kind: crate::core::RelationKind = kind_text.parse().map_err(|_| {
        VogtError::InvalidRequest(format!(
            "unknown relation kind {}",
            crate::core::py_repr(&kind_text)
        ))
    })?;
    let mut writing = crate::application::context::write_of(ctx);
    let clock = std::sync::Arc::clone(writing.clock());
    let stored = audited_write(&mut writing, WORK_RELATE, &reason, move |txn, _actor| {
        let item = declared_only(ctx, txn, &reference)?;
        let other = declared_only(ctx, txn, &related)?;
        if item.id == other.id {
            return Err(VogtError::InvalidRequest(
                "an item cannot relate to itself".into(),
            ));
        }
        if item
            .relations
            .iter()
            .any(|edge| edge.kind == kind && edge.related_id == other.id)
        {
            return Err(VogtError::Conflict(format!(
                "{} already {kind_text} {}",
                item.reference, other.reference
            )));
        }
        if matches!(
            kind,
            crate::core::RelationKind::DependsOn | crate::core::RelationKind::DuplicateOf
        ) && would_cycle(txn, &item.id, &other.id, kind)?
        {
            return Err(VogtError::InvalidRequest(format!(
                "relating {} {kind_text} {} would create a cycle",
                item.reference, other.reference
            )));
        }
        txn.insert_relation(&item.id, &other.id, kind, super::now_of(&clock))?;
        let updated = txn
            .work_item_by_id(&item.id)?
            .ok_or_else(|| VogtError::NotFound(format!("no work item {}", item.reference)))?;
        let edge = json!({"ref": item.reference, "kind": kind_text, "target": other.reference});
        Ok(WriteOutcome::new(
            updated,
            "work_item",
            &item.id,
            edge.clone(),
            WORK_RELATED,
            edge,
        ))
    })?;
    Ok(json!({
        "item": stored, "comments": [], "sessions": [], "branches": [],
        "git": null, "walked": [], "live_sessions": [],
    }))
}

/// Drop one declared relation. An edge that is not there is an invalid
/// request, and an upstream ref is refused rather than reported missing.
fn unrelate_work<C, I>(ctx: &AppContext<C, I>, params: RelateParams) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let RelateParams {
        reference,
        related,
        kind_text,
        reason,
    } = params;
    let kind: crate::core::RelationKind = kind_text.parse().map_err(|_| {
        VogtError::InvalidRequest(format!(
            "unknown relation kind {}",
            crate::core::py_repr(&kind_text)
        ))
    })?;
    let mut writing = crate::application::context::write_of(ctx);
    let stored = audited_write(&mut writing, WORK_UNRELATE, &reason, move |txn, _actor| {
        let item = declared_only(ctx, txn, &reference)?;
        let other = declared_only(ctx, txn, &related)?;
        let removed = txn.delete_relation(&item.id, &other.id, kind)?;
        if !removed {
            return Err(VogtError::InvalidRequest(format!(
                "{} does not {kind_text} {}",
                item.reference, other.reference
            )));
        }
        let updated = txn
            .work_item_by_id(&item.id)?
            .ok_or_else(|| VogtError::NotFound(format!("no work item {}", item.reference)))?;
        let edge = json!({"ref": item.reference, "kind": kind_text, "target": other.reference});
        Ok(WriteOutcome::new(
            updated,
            "work_item",
            &item.id,
            edge.clone(),
            WORK_UNRELATED,
            edge,
        ))
    })?;
    Ok(json!({
        "item": stored, "comments": [], "sessions": [], "branches": [],
        "git": null, "walked": [], "live_sessions": [],
    }))
}

/// True when `target` can already reach `source` through `kind`, so adding the
/// edge would close a cycle.
fn would_cycle(
    txn: &impl crate::storage::interface::ReadView,
    source: &str,
    target: &str,
    kind: crate::core::RelationKind,
) -> Result<bool, VogtError> {
    let mut frontier = vec![target.to_string()];
    let mut seen = std::collections::BTreeSet::from([target.to_string()]);
    while let Some(current) = frontier.pop() {
        if current == source {
            return Ok(true);
        }
        let Some(item) = txn.work_item_by_id(&current)? else {
            continue;
        };
        for relation in item.relations {
            if relation.kind == kind && seen.insert(relation.related_id.clone()) {
                frontier.push(relation.related_id);
            }
        }
    }
    Ok(false)
}

/// Declare the branch a native item is worked on. An omitted name comes from
/// `branch_binding_template`. Declaring the name already recorded is a no-op.
/// An upstream item's overlay write is the write-through and stores nothing.
fn bind_branch<C, I>(ctx: &AppContext<C, I>, params: BindBranchParams) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let BindBranchParams {
        reference,
        given,
        reason,
    } = params;
    // Python resolves the ref inside the write body, so a refused bind opens
    // a transaction and burns its id; the same order keeps the audit ids equal.
    let mut writing = crate::application::context::write_of(ctx);
    let clock = std::sync::Arc::clone(writing.clock());
    let stored = audited_write(&mut writing, WORK_BIND, &reason, move |txn, _actor| {
        // Declared-only `resolve::work_item` misses an upstream subject key.
        // Resolve the same way Python does inside the transaction body.
        let fresh = upstream::resolve_work_ref(ctx, txn, &reference)?;
        let branch = given
            .clone()
            .map(|name| name.trim().to_string())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| {
                crate::branches::default_branch_name(
                    &fresh.reference,
                    &ctx.config.branch_binding_template,
                )
            });
        let Some(project_id) = fresh.project_id.clone() else {
            return Err(VogtError::InvalidRequest(format!(
                "{} belongs to no project, so there is no branch to bind it to",
                fresh.reference
            )));
        };
        let mut overlay = txn.work_overlay(&fresh.reference)?.unwrap_or(WorkOverlay {
            subject_key: fresh.reference.clone(),
            project_id,
            rank: None,
            workflow_state: None,
            priority: None,
            effort: None,
            assignee_actor_id: None,
            initiative_id: None,
            branches: Vec::new(),
            created_at: super::now_of(&clock),
            updated_at: super::now_of(&clock),
        });
        if !overlay.branches.iter().any(|name| name == &branch) {
            overlay.branches.push(branch.clone());
            overlay.updated_at = super::now_of(&clock);
            txn.upsert_work_overlay(&overlay)?;
        }
        let payload = serde_json::to_value(&overlay).unwrap_or(Value::Null);
        let updated = upstream::resolve_work_ref(ctx, txn, &fresh.reference)?;
        Ok(WriteOutcome::new(
            updated,
            "work_item",
            &fresh.id,
            payload,
            WORK_BRANCH_BOUND,
            json!({"ref": fresh.reference, "branch": branch}),
        ))
    })?;
    Ok(json!({
        "item": stored, "comments": [], "sessions": [], "branches": [],
        "git": null, "walked": [], "live_sessions": [],
    }))
}

/// Promote one observed subject into a native work item and record the link.
///
/// The observation must still be the latest for its subject, belong to a
/// project, and not already be linked. The created item takes the observation's
/// kind, title and priority; the link is the audit of the promotion.
fn adopt_work<C, I>(ctx: &AppContext<C, I>, params: AdoptParams) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let AdoptParams { subject, reason } = params;
    // Python's `_subject`: nothing swept is an invalid request, and an unknown
    // subject is a not_found that names it.
    if !ctx.observed.has_evidence_tables()? {
        return Err(VogtError::InvalidRequest(
            "nothing has been swept yet; there are no subjects to adopt".into(),
        ));
    }
    let unknown = || VogtError::NotFound(format!("no observed subject '{subject}'"));
    let observation = ctx
        .observed
        .latest_by_subject(&subject)?
        .ok_or_else(unknown)?;
    let project_id = observation.project_id.clone().ok_or_else(|| {
        VogtError::InvalidRequest(format!(
            "observation {subject} has no project and cannot be adopted"
        ))
    })?;
    let mut writing = crate::application::context::write_of(ctx);
    let clock = std::sync::Arc::clone(writing.clock());
    let ids = std::sync::Arc::clone(writing.ids());
    let (item, link) = audited_write(&mut writing, WORK_ADOPT, &reason, move |txn, _actor| {
        if txn.work_item_by_subject(&subject)?.is_some() {
            return Err(VogtError::InvalidRequest(format!(
                "{subject} is already adopted"
            )));
        }
        let project = txn
            .project_by_id(&project_id)?
            .ok_or_else(|| VogtError::NotFound(format!("no project {project_id}")))?;
        let kind_text = crate::decisions::work_kind_of(&observation);
        let kind: WorkKind = kind_text.parse().unwrap_or(WorkKind::Chore);
        let priority_text = crate::decisions::priority_of(&observation);
        let priority: Priority = priority_text.parse().unwrap_or(Priority::P2);
        let workflow = txn.workflow_for(kind_text)?;
        let now = super::now_of(&clock);
        let id = super::next_id(&ids, "wrk");
        let reference = txn.next_work_ref()?;
        let item = WorkItem {
            id: id.clone(),
            reference: reference.clone(),
            kind,
            title: crate::decisions::title_of(&observation),
            body: String::new(),
            state: workflow.initial_state,
            priority,
            effort: None,
            project_id: Some(project.id.clone()),
            project_slug: Some(project.slug.clone()),
            initiative_id: None,
            origin: crate::core::Origin::Adopted,
            trust_state: crate::core::TrustState::Unverified,
            assignee_actor_id: None,
            assignee_identity_ref: None,
            labels: Vec::new(),
            relations: Vec::new(),
            superseded_by: None,
            created_at: now,
            updated_at: now,
        };
        txn.insert_work_item(&item)?;
        let link = WorkLink {
            work_item_id: id.clone(),
            subject_key: subject.clone(),
            origin_kind: observation.kind.clone(),
            source_url: observation.source_url.clone(),
            relation: crate::core::LinkRelation::Completion,
            created_at: now,
        };
        txn.insert_work_link(&link)?;
        let payload = serde_json::to_value(&item).unwrap_or(Value::Null);
        Ok(WriteOutcome::new(
            (item, link),
            "work_item",
            &id,
            payload,
            WORK_ADOPTED,
            json!({"ref": reference, "subject_key": subject}),
        ))
    })?;
    Ok(json!({"item": item, "link": link}))
}

/// A native item's write-back, decided and sent before the declared
/// transaction opens, so no forge call runs while the write lock is held.
struct PlannedWriteback {
    action: crate::core::WriteBackAction,
    policy: String,
    project_id: Option<String>,
    work_item_id: Option<String>,
    subject_key: Option<String>,
    outcome: crate::core::WriteBackOutcome,
    detail: String,
    source_url: Option<String>,
}

/// One write-back attempt from a native item. Ports `writeback.attempt`:
/// policy, link and forge gates answer `skipped` with the reason; otherwise
/// the provider is called. A provider error is a `failed` outcome, not an
/// error: the local change still lands and the ledger records why.
fn plan_writeback(
    action: crate::core::WriteBackAction,
    project: Option<&Project>,
    work_item_id: Option<&str>,
    subject_key: Option<&str>,
    writer: Option<&(ForgeClient, String)>,
    body: Option<&str>,
) -> PlannedWriteback {
    use crate::core::WriteBackOutcome as Outcome;
    let policy = project
        .map(|project| project.write_back.to_string())
        .unwrap_or_else(|| "none".into());
    let mut plan = PlannedWriteback {
        action,
        policy: policy.clone(),
        project_id: project.map(|project| project.id.clone()),
        work_item_id: work_item_id.map(str::to_string),
        subject_key: subject_key.map(str::to_string),
        outcome: Outcome::Skipped,
        detail: String::new(),
        source_url: None,
    };
    let permitted = matches!(
        (policy.as_str(), action),
        ("full", _) | ("comment_only", crate::core::WriteBackAction::Comment)
    );
    plan.detail = if !permitted {
        if policy == "none" {
            "write-back is off for this project".to_string()
        } else {
            format!("policy is {policy:?}; {} is not permitted", action)
        }
    } else if subject_key.is_none() && action != crate::core::WriteBackAction::Create {
        "this item is not linked to a forge object".to_string()
    } else if let Some((provider, identity)) = writer {
        let repo = provider.parse(project.and_then(|project| project.repo_url.as_deref()));
        let has_repo = repo.is_some();
        let number = provider.number_of(subject_key);
        let sent = match (repo, number, action) {
            (None, _, _) => None,
            (Some(repo), Some(number), crate::core::WriteBackAction::Comment) => body
                .filter(|text| !text.is_empty())
                .map(|text| provider.comment(&repo, number, text)),
            (Some(repo), Some(number), crate::core::WriteBackAction::Close) => {
                Some(provider.set_state(&repo, number, "closed"))
            }
            (Some(repo), Some(number), crate::core::WriteBackAction::Reopen) => {
                Some(provider.set_state(&repo, number, "open"))
            }
            _ => None,
        };
        match sent {
            Some(Ok(sent)) => {
                if let Some(key) = sent.subject_key.clone().filter(|key| !key.is_empty()) {
                    plan.subject_key = Some(key);
                }
                plan.outcome = sent.outcome.as_str().parse().unwrap_or(Outcome::Failed);
                plan.source_url = sent.source_url.clone();
                plan.detail = match sent.detail.as_deref() {
                    Some(detail) => format!("as {identity}; {detail}"),
                    None => format!("as {identity}"),
                };
                return plan;
            }
            Some(Err(error)) => {
                plan.outcome = Outcome::Failed;
                plan.detail = format!("as {identity}; {error}");
                return plan;
            }
            None if !has_repo => "this project has no forge repository to write to".to_string(),
            None => format!("nothing to send for {action}"),
        }
    } else {
        "no forge is configured to write to for this project".to_string()
    };
    plan
}

/// Record a planned write-back in the declared transaction.
fn record_writeback(
    txn: &mut dyn WriteTxn,
    ids: &std::sync::Arc<std::sync::Mutex<impl IdFactory>>,
    clock: &std::sync::Arc<std::sync::Mutex<impl Clock>>,
    actor_id: &str,
    reason: &str,
    plan: PlannedWriteback,
) -> Result<WriteBackRecord, VogtError> {
    let record = WriteBackRecord {
        id: super::next_id(ids, "wbk"),
        at: super::now_of(clock),
        project_id: plan.project_id,
        work_item_id: plan.work_item_id,
        actor_id: actor_id.to_string(),
        action: plan.action,
        subject_key: plan.subject_key,
        policy: plan.policy,
        outcome: plan.outcome,
        reason: reason.to_string(),
        detail: Some(plan.detail),
        source_url: plan.source_url,
    };
    txn.insert_writeback(&record)?;
    Ok(record)
}

/// The writer a native item's write-back would use, resolved before the
/// declared transaction opens. `None` when the project's policy is `none`
/// or no forge credential resolves, which `plan_writeback` records.
fn native_writer<C, I>(
    ctx: &AppContext<C, I>,
    reference: &str,
) -> Result<Option<(ForgeClient, String)>, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let view = ctx.declared.read()?;
    let Some(item) = view.work_item_by_ref(reference)? else {
        return Ok(None);
    };
    let Some(project) = item
        .project_id
        .as_deref()
        .map(|id| view.project_by_id(id))
        .transpose()?
        .flatten()
    else {
        return Ok(None);
    };
    if project.write_back.to_string() == "none" {
        return Ok(None);
    }
    let actor = view.actor_by_identity(&ctx.principal.identity_ref)?;
    drop(view);
    let (provider, identity) = writer_provider(ctx, actor.as_ref(), project.repo_url.as_deref())?;
    Ok(provider.map(|provider| (provider, identity)))
}

fn linked_subject(txn: &impl ReadView, item_id: &str) -> Result<Option<String>, VogtError> {
    Ok(txn
        .work_links_for_subjects_by_item(item_id)?
        .into_keys()
        .next())
}

/// The forge subject a native item's write-back goes to. An item migrated
/// upstream by `forge link` keeps no link row of its own; it is retired with
/// `superseded_by` naming its successor, so that successor is the write-back
/// target. Without this, a comment or a done transition on a migrated item is
/// skipped as "not linked to a forge object".
fn write_back_subject(own_link: Option<String>, superseded_by: Option<&str>) -> Option<String> {
    own_link.or_else(|| {
        superseded_by
            .filter(|subject| !subject.is_empty())
            .map(str::to_owned)
    })
}

/// A ref that must name a declared row. An upstream-truth item is refused
/// with the reason named, rather than a bare not-found.
fn declared_only<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    view: &impl ReadView,
    reference: &str,
) -> Result<WorkItem, VogtError> {
    match resolve::work_item(view, reference) {
        Ok(item) => Ok(item),
        Err(VogtError::NotFound(_)) => {
            if upstream::resolve_upstream(ctx, view, reference)?.is_some() {
                Err(VogtError::InvalidRequest(format!(
                    "{reference} is an upstream-truth item; relations between upstream issues are not supported in v1 of the upstream-truth model — relate them on the forge"
                )))
            } else {
                Err(VogtError::NotFound(format!(
                    "no work item {}",
                    crate::core::py_repr(reference)
                )))
            }
        }
        Err(other) => Err(other),
    }
}

fn board_token(value: &Value) -> String {
    use base64::Engine;
    // `json.dumps(..., sort_keys=True, separators=(",", ":"))`, then urlsafe
    // base64 with the padding stripped. Not the spaced form the stores use.
    let raw = crate::decisions::render_python_json_compact(value);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw.as_bytes())
}

/// The lane an upstream item lands in, mirroring the SQL lane expression.
fn board_upstream_lane(lane_mode: &str, item: &WorkItem) -> String {
    match lane_mode {
        "project" => item.project_slug.clone().unwrap_or_default(),
        "initiative" => item.initiative_id.clone().unwrap_or_default(),
        _ => String::new(),
    }
}

fn board_dump_high_water(high_water: Option<&(Moment, String)>) -> Value {
    high_water.map_or(Value::Null, |(moment, reference)| {
        json!([moment.to_iso(), reference])
    })
}

fn board_decode(token: &str, kind: &str) -> Result<Token, VogtError> {
    use base64::Engine;
    let malformed = || VogtError::InvalidCursor(format!("Board {kind} token is malformed"));
    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(token.trim_end_matches('='))
        .map_err(|_| malformed())?;
    let decoded: Token = serde_json::from_slice(&raw).map_err(|_| malformed())?;
    if str_of(&decoded.kind) != Some(kind) {
        return Err(malformed());
    }
    Ok(decoded)
}

fn board_decode_high_water(token: &Token) -> Result<Option<(Moment, String)>, VogtError> {
    let invalid =
        || VogtError::InvalidCursor("Board snapshot has an invalid high-water mark".to_string());
    match &token.high_water {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(pair)) if pair.len() == 2 => {
            let reference = pair[1].as_str().ok_or_else(invalid)?;
            let moment = pair[0]
                .as_str()
                .and_then(|text| crate::core::from_iso(text).ok())
                .ok_or_else(invalid)?;
            Ok(Some((moment, reference.to_string())))
        }
        Some(_) => Err(invalid()),
    }
}

fn board_cell_query(
    cell: &Cell,
    fingerprint: &str,
    revision: i64,
    high_water: Option<&(Moment, String)>,
) -> Result<crate::storage::interface::BoardCellQuery, VogtError> {
    let lane_key = cell.lane_key.as_str();
    let state = cell.state.as_str();
    let mut query = crate::storage::interface::BoardCellQuery {
        lane_key: lane_key.to_string(),
        state: state.to_string(),
        after_created_at: None,
        after_ref: None,
    };
    let Some(cursor) = cell.cursor.as_deref() else {
        return Ok(query);
    };
    let token = board_decode(cursor, "cell")?;
    if str_of(&token.fingerprint) != Some(fingerprint)
        || token.revision.as_ref().and_then(Value::as_i64) != Some(revision)
        || str_of(&token.lane_key) != Some(lane_key)
        || str_of(&token.state) != Some(state)
        || token.high_water.clone().unwrap_or(Value::Null) != board_dump_high_water(high_water)
    {
        return Err(VogtError::InvalidCursor(
            "Board cell cursor belongs to another cell or snapshot".to_string(),
        ));
    }
    let malformed = || VogtError::InvalidCursor("Board cell cursor is malformed".to_string());
    query.after_created_at = Some(
        str_of(&token.after_created_at)
            .and_then(|text| crate::core::from_iso(text).ok())
            .ok_or_else(malformed)?,
    );
    query.after_ref = Some(str_of(&token.after_ref).ok_or_else(malformed)?.to_string());
    Ok(query)
}

/// One cell's declared page merged with its upstream items in
/// `(created_at, ref)` order. Each stream is complete up to `limit` rows past
/// the cursor, so the first `limit` of the merge are the union's first.
fn board_merge_cell(
    declared: Vec<WorkItem>,
    entries: &[(String, WorkItem)],
    query: &crate::storage::interface::BoardCellQuery,
    limit: usize,
) -> Vec<WorkItem> {
    let after = query.after_created_at.zip(query.after_ref.as_ref());
    let mine: Vec<WorkItem> = entries
        .iter()
        .filter(|(lane, item)| *lane == query.lane_key && item.state == query.state)
        .filter(|(_, item)| {
            after.is_none_or(|(moment, reference)| {
                (item.created_at, &item.reference) > (moment, reference)
            })
        })
        .map(|(_, item)| item.clone())
        .collect();
    let mut merged = declared;
    if !mine.is_empty() {
        merged.extend(mine);
        merged.sort_by(|a, b| (a.created_at, &a.reference).cmp(&(b.created_at, &b.reference)));
    }
    merged.truncate(limit);
    merged
}

/// A bounded, snapshot-stable Board for native items.
///
/// An unlinked project answers with empty cells and `link_state: "unlinked"`.
/// The upstream half of a linked board is gathered the same way `work.list`
/// gathers it and merged into the counts; the snapshot token carries the
/// declared revision so a continuation after a write is refused.
fn list_board<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    params: BoardParams,
) -> Result<Value, VogtError> {
    let BoardParams {
        cells,
        lane_mode,
        page_size,
        project,
        kinds,
        states,
        priorities,
        assignee,
        initiative,
        label,
        has_snapshot,
        snapshot: snapshot_token,
        fingerprint,
    } = params;
    if cells.is_empty() {
        return Err(VogtError::InvalidRequest(
            "board.list needs at least one cell".to_string(),
        ));
    }
    let lane_mode = lane_mode.as_str();
    let mut seen = std::collections::BTreeSet::new();
    for cell in &cells {
        let lane_key = cell.lane_key.as_str();
        let state = cell.state.as_str();
        if !seen.insert((lane_key, state)) {
            return Err(VogtError::InvalidRequest(format!(
                "board.list cell {}/{} is duplicated",
                crate::core::py_repr(lane_key),
                crate::core::py_repr(state)
            )));
        }
        if lane_mode == "none" && !lane_key.is_empty() {
            return Err(VogtError::InvalidRequest(
                "board.list lane_key must be blank in `none` mode".to_string(),
            ));
        }
        if cell.has_cursor && !has_snapshot {
            return Err(VogtError::InvalidCursor(
                "a Board cell cursor requires its snapshot token".to_string(),
            ));
        }
    }
    let view = ctx.declared.read()?;
    let project_slug = project.clone();
    let project = project
        .as_deref()
        .map(|slug| resolve::project(&view, slug))
        .transpose()?;
    if let Some(project) = project.as_ref() {
        if !upstream::is_linked(project) {
            let pending = view.count_work_items(&WorkFilter {
                project_id: Some(project.id.clone()),
                exclude_terminal: true,
                ..WorkFilter::default()
            })?;
            let empty: Vec<Value> = cells
                .iter()
                .map(|cell| {
                    json!({
                        "lane_key": cell.lane_key,
                        "state": cell.state,
                        "items": [],
                        "total": 0,
                        "next_cursor": null,
                    })
                })
                .collect();
            let snapshot_at = super::now_of(&ctx.clock);
            let revision = view.current_revision()?;
            let snapshot = board_token(&json!({
                "kind": "snapshot",
                "fingerprint": fingerprint,
                "revision": revision,
                "snapshot_at": snapshot_at.to_iso(),
                "high_water": Value::Null,
            }));
            return Ok(json!({
                "cells": empty,
                "column_totals": {},
                "lane_totals": {},
                "total": 0,
                "backlog_candidates": 0,
                "declared_total": 0,
                "link_state": "unlinked",
                "excluded_unlinked": pending,
                "snapshot": snapshot,
                "snapshot_at": snapshot_at,
                "revision": revision,
            }));
        }
    }
    let filter = WorkFilter {
        project_id: project.as_ref().map(|project| project.id.clone()),
        kinds: kinds.clone(),
        states,
        priorities: priorities.clone(),
        assignee_actor_id: assignee
            .as_deref()
            .map(|identity| resolve::actor(&view, identity).map(|actor| actor.id))
            .transpose()?,
        initiative_id: initiative
            .as_deref()
            .map(|slug| resolve::initiative(&view, slug).map(|initiative| initiative.id))
            .transpose()?,
        label: label.clone(),
        // The Board draws terminal columns when asked; the forge-less
        // withdrawal keeps unlinked projects' native rows off the global Board.
        exclude_terminal: false,
        exclude_unlinked_native: true,
        ..WorkFilter::default()
    };
    let excluded_unlinked = view.count_work_items(&WorkFilter {
        exclude_unlinked_native: false,
        ..filter.clone()
    })? - view.count_work_items(&filter)?;
    let revision = view.current_revision()?;

    // The upstream-truth half: linked projects' items live in the observed
    // mirror + overlay, not in `work_items`, so they are merged into every
    // count and page here, each issue once (adopted subjects are excluded).
    let mut upstream_entries: Vec<(String, WorkItem)> = Vec::new();
    for linked in upstream::linked_projects(&view, project.as_ref())? {
        for item in upstream::upstream_items(ctx, &view, &linked, true, 1000)? {
            if upstream::matches(&item, &filter) {
                upstream_entries.push((board_upstream_lane(lane_mode, &item), item));
            }
        }
    }
    upstream_entries
        .sort_by(|a, b| (a.1.created_at, &a.1.reference).cmp(&(b.1.created_at, &b.1.reference)));

    let (high_water, snapshot_at, snapshot) = match snapshot_token.as_deref() {
        Some(token) => {
            let decoded = board_decode(token, "snapshot")?;
            if str_of(&decoded.fingerprint) != Some(fingerprint.as_str()) {
                return Err(VogtError::InvalidCursor(
                    "Board snapshot belongs to another filter or lane mode".to_string(),
                ));
            }
            if decoded.revision.as_ref().and_then(Value::as_i64) != Some(revision) {
                return Err(VogtError::InvalidCursor(
                    "Board snapshot is stale because declared work changed; refresh the Board"
                        .to_string(),
                ));
            }
            let high_water = board_decode_high_water(&decoded)?;
            let snapshot_at = str_of(&decoded.snapshot_at)
                .and_then(|text| crate::core::from_iso(text).ok())
                .ok_or_else(|| {
                    VogtError::InvalidCursor("Board snapshot has an invalid timestamp".to_string())
                })?;
            (high_water, snapshot_at, token.to_string())
        }
        None => {
            let mut high_water = view.board_high_water(&filter)?;
            if let Some(last) = upstream_entries
                .iter()
                .map(|(_, item)| (item.created_at, item.reference.clone()))
                .max()
            {
                if high_water.as_ref().is_none_or(|mark| &last > mark) {
                    high_water = Some(last);
                }
            }
            let snapshot_at = super::now_of(&ctx.clock);
            let snapshot = board_token(&json!({
                "kind": "snapshot",
                "fingerprint": fingerprint,
                "revision": revision,
                "snapshot_at": snapshot_at.to_iso(),
                "high_water": board_dump_high_water(high_water.as_ref()),
            }));
            (high_water, snapshot_at, snapshot)
        }
    };
    upstream_entries.retain(|(_, item)| {
        high_water.as_ref().is_some_and(|(moment, reference)| {
            (item.created_at, &item.reference) <= (*moment, reference)
        })
    });

    let mut queries = Vec::with_capacity(cells.len());
    for cell in &cells {
        queries.push(board_cell_query(
            cell,
            &fingerprint,
            revision,
            high_water.as_ref(),
        )?);
    }
    let mut counts = view.board_counts(&filter, lane_mode, high_water.as_ref())?;
    for (lane, item) in &upstream_entries {
        *counts
            .entry((lane.clone(), item.state.clone()))
            .or_insert(0) += 1;
    }
    let pages = view.board_work_items(
        &filter,
        lane_mode,
        &queries,
        high_water.as_ref(),
        page_size + 1,
    )?;
    let mut column_totals = serde_json::Map::new();
    let mut lane_totals = serde_json::Map::new();
    let mut total = 0_i64;
    for ((lane, state), count) in &counts {
        total += count;
        let column = column_totals.entry(state.clone()).or_insert(json!(0));
        *column = json!(column.as_i64().unwrap_or(0) + count);
        let lane_total = lane_totals.entry(lane.clone()).or_insert(json!(0));
        *lane_total = json!(lane_total.as_i64().unwrap_or(0) + count);
    }
    let limit = (page_size + 1).max(0) as usize;
    let rendered: Vec<Value> = queries
        .iter()
        .map(|query| {
            let key = (query.lane_key.clone(), query.state.clone());
            let declared = pages.get(&key).cloned().unwrap_or_default();
            let fetched = board_merge_cell(declared, &upstream_entries, query, limit);
            let shown: Vec<&WorkItem> = fetched.iter().take(page_size.max(0) as usize).collect();
            let next_cursor = match shown.last() {
                Some(last) if fetched.len() > shown.len() => Value::String(board_token(&json!({
                    "kind": "cell",
                    "fingerprint": fingerprint,
                    "revision": revision,
                    "high_water": board_dump_high_water(high_water.as_ref()),
                    "lane_key": query.lane_key,
                    "state": last.state,
                    "after_created_at": last.created_at.to_iso(),
                    "after_ref": last.reference,
                }))),
                _ => Value::Null,
            };
            json!({
                "lane_key": query.lane_key,
                "state": query.state,
                "items": shown,
                "total": counts.get(&key).copied().unwrap_or(0),
                "next_cursor": next_cursor,
            })
        })
        .collect();
    // Python's `candidate_population`, read after the snapshot as Python reads
    // the clock: the observed-inclusive count and the declared-only count.
    let (backlog_candidates, declared_total) = crate::application::views::candidate_population(
        ctx,
        project_slug.as_deref(),
        kinds,
        priorities,
        assignee.as_deref(),
        initiative.as_deref(),
        label.as_deref(),
    )?;
    Ok(json!({
        "cells": rendered,
        "column_totals": column_totals,
        "lane_totals": lane_totals,
        "total": total,
        "backlog_candidates": backlog_candidates,
        "declared_total": declared_total,
        "link_state": project.as_ref().map(|_| "linked"),
        "excluded_unlinked": excluded_unlinked,
        "snapshot": snapshot,
        "snapshot_at": snapshot_at,
        "revision": revision,
    }))
}

#[cfg(test)]
mod tests {
    use super::{gaps_for, list_work_total, policy_gap, work_page};
    use crate::application::context::{build_context, Built};
    use crate::core::{
        ActorKind, LinkState, Moment, Principal, Project, SequentialIds, StepClock, WriteBack,
    };
    use crate::errors::VogtError;
    use crate::storage::interface::{DeclaredStore, ObservedStore, ReadView, WriteTxn};
    use serde_json::{json, Value};

    // The tests drive the handlers with the wire JSON, decoded the way the
    // registry decodes it.
    fn decoded<P: serde::de::DeserializeOwned>(params: Value) -> Result<P, VogtError> {
        crate::application::typed::decode(params)
    }
    fn create_work<C, I>(
        ctx: &crate::application::context::AppContext<C, I>,
        params: Value,
    ) -> Result<Value, VogtError>
    where
        C: crate::core::Clock + 'static,
        I: crate::core::IdFactory + 'static,
    {
        super::create_work(ctx, decoded(params)?)
    }
    fn update_work<C, I>(
        ctx: &crate::application::context::AppContext<C, I>,
        params: Value,
    ) -> Result<Value, VogtError>
    where
        C: crate::core::Clock + 'static,
        I: crate::core::IdFactory + 'static,
    {
        super::update_work(ctx, decoded(params)?)
    }
    fn list_work<C, I>(
        ctx: &crate::application::context::AppContext<C, I>,
        params: Value,
    ) -> Result<Value, VogtError>
    where
        C: crate::core::Clock + 'static,
        I: crate::core::IdFactory + 'static,
    {
        super::list_work(ctx, decoded(params)?)
    }
    fn get_work<C, I>(
        ctx: &crate::application::context::AppContext<C, I>,
        params: Value,
    ) -> Result<Value, VogtError>
    where
        C: crate::core::Clock + 'static,
        I: crate::core::IdFactory + 'static,
    {
        super::get_work(ctx, decoded(params)?)
    }
    fn get_many_work<C, I>(
        ctx: &crate::application::context::AppContext<C, I>,
        params: Value,
    ) -> Result<Value, VogtError>
    where
        C: crate::core::Clock + 'static,
        I: crate::core::IdFactory + 'static,
    {
        super::get_many_work(ctx, decoded(params)?)
    }
    fn comment_work<C, I>(
        ctx: &crate::application::context::AppContext<C, I>,
        params: Value,
    ) -> Result<Value, VogtError>
    where
        C: crate::core::Clock + 'static,
        I: crate::core::IdFactory + 'static,
    {
        super::comment_work(ctx, decoded(params)?)
    }
    fn transition_work<C, I>(
        ctx: &crate::application::context::AppContext<C, I>,
        params: Value,
    ) -> Result<Value, VogtError>
    where
        C: crate::core::Clock + 'static,
        I: crate::core::IdFactory + 'static,
    {
        super::transition_work(ctx, decoded(params)?)
    }
    fn relate_work<C, I>(
        ctx: &crate::application::context::AppContext<C, I>,
        params: Value,
    ) -> Result<Value, VogtError>
    where
        C: crate::core::Clock + 'static,
        I: crate::core::IdFactory + 'static,
    {
        super::relate_work(ctx, decoded(params)?)
    }
    fn unrelate_work<C, I>(
        ctx: &crate::application::context::AppContext<C, I>,
        params: Value,
    ) -> Result<Value, VogtError>
    where
        C: crate::core::Clock + 'static,
        I: crate::core::IdFactory + 'static,
    {
        super::unrelate_work(ctx, decoded(params)?)
    }
    fn bind_branch<C, I>(
        ctx: &crate::application::context::AppContext<C, I>,
        params: Value,
    ) -> Result<Value, VogtError>
    where
        C: crate::core::Clock + 'static,
        I: crate::core::IdFactory + 'static,
    {
        super::bind_branch(ctx, decoded(params)?)
    }
    fn list_board<C, I>(
        ctx: &crate::application::context::AppContext<C, I>,
        params: Value,
    ) -> Result<Value, VogtError>
    where
        C: crate::core::Clock + 'static,
        I: crate::core::IdFactory + 'static,
    {
        super::list_board(ctx, decoded(params)?)
    }

    /// The decode errors are the messages the hand-rolled readers gave. Each
    /// is pinned here so a serde-style rewrite cannot change the wire text.
    #[test]
    fn decode_messages_are_unchanged() {
        fn message<P: serde::de::DeserializeOwned>(params: Value) -> String {
            match decoded::<P>(params) {
                Err(error) => error.message().to_string(),
                Ok(_) => "accepted".to_string(),
            }
        }
        use super::*;
        assert_eq!(
            message::<CreateParams>(json!(null)),
            "work.create takes an object"
        );
        assert_eq!(
            message::<UpdateParams>(json!([1])),
            "work.update takes an object"
        );
        assert_eq!(
            message::<CreateParams>(json!({"title": "t"})),
            "kind is required"
        );
        assert_eq!(
            message::<CreateParams>(json!({"kind": "bug"})),
            "title is required"
        );
        assert_eq!(
            message::<CreateParams>(json!({"kind": "bug", "title": "t", "upstream": "true"})),
            "upstream must be a boolean"
        );
        assert_eq!(message::<UpdateParams>(json!({})), "ref is required");
        assert_eq!(
            message::<GetParams>(json!({"ref": null, "id": "WI-1"})),
            "ref is required"
        );
        assert_eq!(message::<GetParams>(json!({"id": "WI-1"})), "accepted");
        assert_eq!(
            message::<CommentParams>(json!({"body": "b"})),
            "ref is required"
        );
        assert_eq!(
            message::<CommentParams>(json!({"ref": "WI-1"})),
            "body is required"
        );
        assert_eq!(
            message::<TransitionParams>(json!({"ref": "WI-1"})),
            "to_state is required"
        );
        assert_eq!(
            message::<RelateParams>(json!({"ref": "WI-1"})),
            "target is required"
        );
        assert_eq!(message::<AdoptParams>(json!({})), "subject is required");
        assert_eq!(
            message::<ListParams>(json!({"status": "open", "states": ["done"]})),
            "work.list: 'status' is an alias of \"states\"; pass one of them, not both"
        );
        assert_eq!(
            message::<ListParams>(json!({"q": "x", "text": "y"})),
            "work.list: 'q' is an alias of \"query\"; pass one of them, not both"
        );
        assert_eq!(message::<ListParams>(json!(null)), "accepted");
        let split: ListParams = decoded(json!({"state": "open, done,,"})).unwrap();
        assert_eq!(split.states, ["open", "done"]);
    }

    fn moment() -> Moment {
        Moment::from_unix(1_700_000_000, 0)
    }

    fn unique() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        COUNTER.fetch_add(1, Ordering::Relaxed)
    }

    fn opened() -> Built {
        let dir =
            std::env::temp_dir().join(format!("vogt-work-{}-{}", std::process::id(), unique()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let config = crate::config::VogtConfig {
            data_dir: dir,
            ..crate::config::VogtConfig::default()
        };
        let built = build_context(
            config,
            Some(Principal::new("local:test-user", ActorKind::Human, "Test").unwrap()),
            Some(StepClock::new(moment())),
            Some(SequentialIds::new(None).unwrap()),
            None,
            None,
            None,
            None,
        )
        .unwrap();
        let Built::StepSequential(ctx) = &built else {
            panic!("expected a step clock");
        };
        ctx.declared.migrate().unwrap();
        ctx.declared
            .bootstrap(&Principal::new("local:test-user", ActorKind::Human, "Test").unwrap())
            .unwrap();
        ctx.observed.migrate().unwrap();
        built
    }

    fn insert_project(built: &Built, slug: &str, linked: bool, policy: WriteBack) {
        let Built::StepSequential(ctx) = built else {
            unreachable!("opened() builds a step clock");
        };
        let mut project = Project::new(&format!("prj_{slug}"), slug, slug, "/tmp", moment());
        project.link_state = if linked {
            LinkState::Linked
        } else {
            LinkState::Unlinked
        };
        project.write_back = policy;
        project.repo_url = Some("https://github.com/acme/beta".to_string());
        let mut txn = ctx.declared.write().unwrap();
        txn.insert_project(&project).unwrap();
        txn.commit().unwrap();
    }

    #[test]
    fn the_envelope_carries_every_key_in_the_model_order() {
        // `WorkListResult`'s field order, with the nulls present. A global list
        // has no link state and an empty page has no next offset, and neither
        // is a reason to drop the key.
        let page = work_page("summary", 0, Vec::new(), 0, None, None);
        let Value::Object(map) = &page else {
            panic!("a page is an object");
        };
        let keys: Vec<&String> = map.keys().collect();
        assert_eq!(
            keys,
            [
                "items",
                "total",
                "mode",
                "next_offset",
                "link_state",
                "detail"
            ]
        );
        assert_eq!(page["next_offset"], Value::Null);
        assert_eq!(page["link_state"], Value::Null);
        assert_eq!(page["detail"], Value::Null);

        let scoped = work_page("full", 0, Vec::new(), 0, Some("unlinked"), None);
        let Value::Object(scoped_map) = &scoped else {
            panic!("a page is an object");
        };
        assert_eq!(scoped_map.keys().collect::<Vec<_>>(), keys);
        assert_eq!(scoped["link_state"], "unlinked");
    }

    #[test]
    fn a_native_create_keeps_the_body_and_numbers_refs_in_order() {
        let built = opened();
        let Built::StepSequential(ctx) = &built else {
            unreachable!("opened() builds a step clock");
        };
        let first = create_work(
            ctx,
            json!({"kind": "chore", "title": "one", "body": "  kept  ", "reason": "test"}),
        )
        .unwrap();
        assert_eq!(first["item"]["ref"], "WI-1");
        assert_eq!(first["item"]["body"], "  kept  ");
        assert_eq!(first["item"]["origin"], "created");
        assert_eq!(first["item"]["trust_state"], "unverified");
        let second = create_work(
            ctx,
            json!({"kind": "bug", "title": "two", "reason": "test"}),
        )
        .unwrap();
        assert_eq!(second["item"]["ref"], "WI-2");
        assert_eq!(second["item"]["state"], "open");
    }

    #[test]
    fn a_non_boolean_upstream_is_refused_not_treated_as_native() {
        let built = opened();
        let Built::StepSequential(ctx) = &built else {
            unreachable!("opened() builds a step clock");
        };
        let error = create_work(
            ctx,
            json!({
                "kind": "chore",
                "title": "one",
                "upstream": "true",
                "reason": "test",
            }),
        )
        .unwrap_err();
        assert!(
            matches!(&error, VogtError::InvalidRequest(message) if message.contains("upstream")),
            "{error}"
        );
        // Nothing was created: the native ref is still WI-1.
        let native = create_work(
            ctx,
            json!({"kind": "chore", "title": "two", "reason": "test"}),
        )
        .unwrap();
        assert_eq!(native["item"]["ref"], "WI-1");
    }

    #[test]
    fn an_update_changes_the_title_and_keeps_the_ref() {
        let built = opened();
        let Built::StepSequential(ctx) = &built else {
            unreachable!("opened() builds a step clock");
        };
        create_work(
            ctx,
            json!({"kind": "chore", "title": "one", "reason": "test"}),
        )
        .unwrap();
        let updated = update_work(
            ctx,
            json!({"ref": "WI-1", "title": "renamed", "priority": "p0", "reason": "test"}),
        )
        .unwrap();
        assert_eq!(updated["item"]["ref"], "WI-1");
        assert_eq!(updated["item"]["title"], "renamed");
        assert_eq!(updated["item"]["priority"], "p0");
        assert_eq!(updated["item"]["kind"], "chore");
    }

    #[test]
    fn a_label_edit_on_an_unlinked_project_is_refused() {
        let built = opened();
        let Built::StepSequential(ctx) = &built else {
            unreachable!("opened() builds a step clock");
        };
        insert_project(&built, "beta", false, WriteBack::Disabled);
        create_work(
            ctx,
            json!({"kind": "chore", "title": "one", "project": "beta", "local_only": true, "reason": "test"}),
        )
        .unwrap();
        let refused = update_work(
            ctx,
            json!({"ref": "WI-1", "add_labels": ["parity"], "reason": "test"}),
        )
        .unwrap_err();
        assert!(matches!(refused, VogtError::NotLinked(_)), "{refused}");
        let retitled = update_work(
            ctx,
            json!({"ref": "WI-1", "title": "still editable", "reason": "test"}),
        )
        .unwrap();
        assert_eq!(retitled["item"]["title"], "still editable");
    }

    #[test]
    fn an_unknown_ref_is_not_found_rather_than_an_upstream_item() {
        let built = opened();
        let Built::StepSequential(ctx) = &built else {
            unreachable!("opened() builds a step clock");
        };
        for reference in ["WI-99", "wi-2"] {
            let refused = update_work(
                ctx,
                json!({"ref": reference, "title": "nope", "reason": "test"}),
            )
            .unwrap_err();
            let VogtError::NotFound(message) = &refused else {
                panic!("{reference}: expected not_found, got {refused}");
            };
            assert!(message.contains("no work item"), "{reference}: {message}");
        }
    }

    #[test]
    fn an_unlinked_project_refuses_without_burning_a_ref() {
        let built = opened();
        let Built::StepSequential(ctx) = &built else {
            unreachable!("opened() builds a step clock");
        };
        insert_project(&built, "beta", false, WriteBack::Disabled);
        let refused = create_work(
            ctx,
            json!({"kind": "chore", "title": "nope", "project": "beta", "upstream": true, "reason": "test"}),
        )
        .unwrap_err();
        assert!(matches!(refused, VogtError::NotLinked(_)), "{refused}");
        assert!(
            refused.message().contains("drop `upstream`"),
            "{}",
            refused.message()
        );
        let after = create_work(
            ctx,
            json!({"kind": "chore", "title": "still first", "reason": "test"}),
        )
        .unwrap();
        assert_eq!(after["item"]["ref"], "WI-1");
    }

    #[test]
    fn a_linked_project_names_every_unmet_precondition() {
        let built = opened();
        let Built::StepSequential(ctx) = &built else {
            unreachable!("opened() builds a step clock");
        };
        insert_project(&built, "beta", true, WriteBack::Disabled);
        let refused = create_work(
            ctx,
            json!({"kind": "chore", "title": "nope", "project": "beta", "upstream": true, "reason": "test"}),
        )
        .unwrap_err();
        let VogtError::UpstreamWriteRefused(message) = &refused else {
            panic!("expected upstream_write_refused, got {refused}");
        };
        assert!(
            message.contains("write-back policy is 'none'")
                && message.contains("(2) no forge credential")
                && message.contains("local_only=true"),
            "{message}"
        );
        let after = create_work(
            ctx,
            json!({"kind": "chore", "title": "still first", "reason": "test"}),
        )
        .unwrap();
        assert_eq!(after["item"]["ref"], "WI-1");
    }

    #[test]
    fn upstream_and_local_only_are_mutually_exclusive() {
        let built = opened();
        let Built::StepSequential(ctx) = &built else {
            unreachable!("opened() builds a step clock");
        };
        let refused = create_work(
            ctx,
            json!({"kind": "chore", "title": "two-faced", "upstream": true, "local_only": true, "reason": "test"}),
        )
        .unwrap_err();
        assert!(matches!(refused, VogtError::InvalidRequest(_)), "{refused}");
        assert!(
            refused
                .message()
                .contains("at most one of upstream or local_only"),
            "{}",
            refused.message()
        );
    }

    #[test]
    fn a_configured_github_app_is_a_credential_without_a_token_file() {
        // Python's `github_client_for` returns the App client when the App id
        // and a readable RSA key are set, even with no PAT. The writability
        // reason then names only the policy, not a missing credential.
        let dir =
            std::env::temp_dir().join(format!("vogt-work-app-{}-{}", std::process::id(), unique()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        use rsa::pkcs1::EncodeRsaPrivateKey;
        use rsa::RsaPrivateKey;
        let key = dir.join("app.pem");
        let private = RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048).unwrap();
        std::fs::write(
            &key,
            private
                .to_pkcs1_pem(rsa::pkcs8::LineEnding::LF)
                .unwrap()
                .as_bytes(),
        )
        .unwrap();
        let config = crate::config::VogtConfig {
            data_dir: dir,
            github_app_id: Some("12345".to_string()),
            github_app_private_key_file: Some(key),
            ..crate::config::VogtConfig::default()
        };
        let built = build_context(
            config,
            Some(Principal::new("local:test-user", ActorKind::Human, "Test").unwrap()),
            Some(StepClock::new(moment())),
            Some(SequentialIds::new(None).unwrap()),
            None,
            None,
            None,
            None,
        )
        .unwrap();
        let Built::StepSequential(ctx) = &built else {
            panic!("expected a step clock");
        };
        ctx.declared.migrate().unwrap();
        ctx.declared
            .bootstrap(&Principal::new("local:test-user", ActorKind::Human, "Test").unwrap())
            .unwrap();
        insert_project(&built, "beta", true, WriteBack::Disabled);
        let view = ctx.declared.read().unwrap();
        let project = view.project_by_slug("beta").unwrap().unwrap();
        let actor = view.actor_by_identity(&ctx.principal.identity_ref).unwrap();
        let gaps = gaps_for(
            ctx,
            &project,
            actor.as_ref(),
            policy_gap(&project, "create"),
        )
        .unwrap();
        assert_eq!(gaps.len(), 1, "{gaps:?}");
        assert!(gaps[0].contains("write-back policy is 'none'"), "{gaps:?}");
        assert!(
            !gaps.iter().any(|gap| gap.contains("no forge credential")),
            "{gaps:?}"
        );
    }

    #[test]
    fn a_comment_is_returned_by_the_get() {
        let built = opened();
        let Built::StepSequential(ctx) = &built else {
            unreachable!("opened() builds a step clock");
        };
        create_work(
            ctx,
            json!({"kind": "chore", "title": "one", "reason": "test"}),
        )
        .unwrap();
        let commented = comment_work(
            ctx,
            json!({"ref": "WI-1", "body": "noted", "reason": "test"}),
        )
        .unwrap();
        assert_eq!(commented["comment"]["body"], "noted");
        assert_eq!(commented["write_back"], "skipped");
        assert_eq!(commented["comment"]["kind"], "note");
        let fetched = get_work(ctx, json!({"ref": "WI-1"})).unwrap();
        assert_eq!(fetched["comments"][0]["body"], "noted");
        assert_eq!(fetched["item"]["ref"], "WI-1");
        assert!(fetched["claim_check"].is_null());
    }

    #[test]
    fn write_back_subject_prefers_the_items_own_link_then_its_successor() {
        use super::write_back_subject;
        assert_eq!(
            write_back_subject(Some("gh:o/r#1".into()), Some("gh:o/r#2")).as_deref(),
            Some("gh:o/r#1")
        );
        assert_eq!(
            write_back_subject(None, Some("gh:o/r#22")).as_deref(),
            Some("gh:o/r#22")
        );
        assert_eq!(write_back_subject(None, Some("")), None);
        assert_eq!(write_back_subject(None, None), None);
    }

    /// Bug WI-799: a native item migrated upstream by `forge link` keeps no link
    /// row, so its comment was skipped as "not linked to a forge object" even
    /// though `superseded_by` names the GitHub issue. The ledger detail is the
    /// evidence: it must now reach the forge step, not stop at the link gate.
    #[test]
    fn a_comment_on_a_migrated_item_reaches_its_successor_not_the_link_gate() {
        use crate::application::services::forge_ops_test::insert_project;
        use crate::application::services::forge_writeback::{writeback_list_op, writeback_op};
        use crate::storage::interface::WorkItemUpdate;

        let built = opened();
        let Built::StepSequential(ctx) = &built else {
            unreachable!("opened() builds a step clock");
        };
        insert_project(
            &built,
            "widget",
            Some("https://github.com/o/r"),
            "/tmp",
            true,
        );
        writeback_op(
            &built,
            json!({"project": "widget", "policy": "full", "reason": "arm it"}),
        )
        .unwrap();
        create_work(
            ctx,
            json!({"kind": "chore", "title": "one", "project": "widget", "reason": "test"}),
        )
        .unwrap();
        {
            let id = ctx
                .declared
                .read()
                .unwrap()
                .work_item_by_ref("WI-1")
                .unwrap()
                .unwrap()
                .id;
            let mut txn = ctx.declared.write().unwrap();
            txn.update_work_item(
                &id,
                &WorkItemUpdate {
                    superseded_by: Some("gh:o/r#22".into()),
                    ..WorkItemUpdate::default()
                },
                moment(),
            )
            .unwrap();
            txn.commit().unwrap();
        }
        comment_work(
            ctx,
            json!({"ref": "WI-1", "body": "noted", "reason": "test"}),
        )
        .unwrap();
        let ledger = writeback_list_op(&built, json!({})).unwrap();
        let comment = ledger["actions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["action"] == "comment")
            .expect("the comment wrote a ledger row");
        let detail = comment["detail"].as_str().unwrap_or_default();
        assert!(!detail.contains("not linked"), "{detail}");
        assert_eq!(comment["subject_key"], "gh:o/r#22");
    }

    /// A native item migrated upstream (`superseded_by` names its gh: successor),
    /// on a project with the given write-back policy.
    fn migrated_item(policy: &str) -> Built {
        use crate::application::services::forge_ops_test::insert_project;
        use crate::application::services::forge_writeback::writeback_op;
        use crate::storage::interface::WorkItemUpdate;

        let built = opened();
        let Built::StepSequential(ctx) = &built else {
            unreachable!("opened() builds a step clock");
        };
        insert_project(
            &built,
            "widget",
            Some("https://github.com/o/r"),
            "/tmp",
            true,
        );
        writeback_op(
            &built,
            json!({"project": "widget", "policy": policy, "reason": "arm it"}),
        )
        .unwrap();
        create_work(
            ctx,
            json!({"kind": "chore", "title": "one", "project": "widget", "reason": "test"}),
        )
        .unwrap();
        let id = ctx
            .declared
            .read()
            .unwrap()
            .work_item_by_ref("WI-1")
            .unwrap()
            .unwrap()
            .id;
        let mut txn = ctx.declared.write().unwrap();
        txn.update_work_item(
            &id,
            &WorkItemUpdate {
                superseded_by: Some("gh:o/r#22".into()),
                ..WorkItemUpdate::default()
            },
            moment(),
        )
        .unwrap();
        txn.commit().unwrap();
        built
    }

    /// The ledger row for `action`, or a panic naming what was written.
    fn ledger_row(built: &Built, action: &str) -> Value {
        use crate::application::services::forge_writeback::writeback_list_op;
        let ledger = writeback_list_op(built, json!({})).unwrap();
        ledger["actions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["action"] == action)
            .cloned()
            .unwrap_or_else(|| panic!("no {action} ledger row in {ledger}"))
    }

    /// Bug WI-799: a transition to done on a migrated item must close the linked
    /// GitHub issue. Without the successor fallback the close is skipped as
    /// "not linked to a forge object" and the issue stays open.
    #[test]
    fn a_transition_to_done_on_a_migrated_item_closes_its_successor() {
        let built = migrated_item("full");
        let Built::StepSequential(ctx) = &built else {
            unreachable!("opened() builds a step clock");
        };
        transition_work(
            ctx,
            json!({"ref": "WI-1", "to_state": "done", "walk": true, "reason": "test"}),
        )
        .unwrap();
        let close = ledger_row(&built, "close");
        let detail = close["detail"].as_str().unwrap_or_default();
        assert!(!detail.contains("not linked"), "{detail}");
        assert_eq!(close["subject_key"], "gh:o/r#22");
        assert_eq!(close["policy"], "full");
    }

    /// The close respects the write-back policy: `comment_only` may not close,
    /// so the ledger names the refusal and nothing is sent to the forge.
    #[test]
    fn a_transition_to_done_respects_a_comment_only_policy() {
        let built = migrated_item("comment_only");
        let Built::StepSequential(ctx) = &built else {
            unreachable!("opened() builds a step clock");
        };
        transition_work(
            ctx,
            json!({"ref": "WI-1", "to_state": "done", "walk": true, "reason": "test"}),
        )
        .unwrap();
        let close = ledger_row(&built, "close");
        let detail = close["detail"].as_str().unwrap_or_default();
        assert!(detail.contains("not permitted"), "{detail}");
        assert!(!detail.contains("not linked"), "{detail}");
        assert_eq!(close["outcome"], "skipped");
    }

    #[test]
    fn a_walk_records_each_hop_and_a_cycle_is_refused() {
        let built = opened();
        let Built::StepSequential(ctx) = &built else {
            unreachable!("opened() builds a step clock");
        };
        create_work(
            ctx,
            json!({"kind": "chore", "title": "one", "reason": "test"}),
        )
        .unwrap();
        create_work(
            ctx,
            json!({"kind": "chore", "title": "two", "reason": "test"}),
        )
        .unwrap();
        let walked = transition_work(
            ctx,
            json!({"ref": "WI-1", "to_state": "review", "walk": true, "reason": "test"}),
        )
        .unwrap();
        assert_eq!(walked["item"]["state"], "review");
        assert_eq!(walked["walked"].as_array().unwrap().len(), 3);
        relate_work(
            ctx,
            json!({"ref": "WI-1", "related": "WI-2", "kind": "depends_on", "reason": "test"}),
        )
        .unwrap();
        let cycle = relate_work(
            ctx,
            json!({"ref": "WI-2", "related": "WI-1", "kind": "depends_on", "reason": "test"}),
        )
        .unwrap_err();
        assert!(matches!(cycle, VogtError::InvalidRequest(_)), "{cycle}");
        unrelate_work(
            ctx,
            json!({"ref": "WI-1", "related": "WI-2", "kind": "depends_on", "reason": "test"}),
        )
        .unwrap();
    }

    #[test]
    fn binding_a_branch_is_idempotent_and_the_board_counts_it() {
        let built = opened();
        let Built::StepSequential(ctx) = &built else {
            unreachable!("opened() builds a step clock");
        };
        insert_project(&built, "beta", false, WriteBack::Disabled);
        create_work(
            ctx,
            json!({"kind": "chore", "title": "one", "project": "beta", "local_only": true, "reason": "test"}),
        )
        .unwrap();
        let bound = bind_branch(
            ctx,
            json!({"ref": "WI-1", "branch": "feat", "reason": "test"}),
        )
        .unwrap();
        assert_eq!(bound["item"]["ref"], "WI-1");
        bind_branch(
            ctx,
            json!({"ref": "WI-1", "branch": "feat", "reason": "test"}),
        )
        .unwrap();
        let board = list_board(ctx, json!({"cells": [{"lane_key": "", "state": "open"}]})).unwrap();
        // An unlinked project's native rows are withdrawn from the global
        // Board, as in Python, and reported as `excluded_unlinked`.
        assert_eq!(board["total"], 0);
        assert_eq!(board["excluded_unlinked"], 1);
    }

    #[test]
    fn binding_a_branch_to_an_upstream_subject_stores_the_overlay() {
        let built = opened();
        let Built::StepSequential(ctx) = &built else {
            unreachable!("opened() builds a step clock");
        };
        insert_project(&built, "beta", true, WriteBack::Disabled);
        let sweep = ctx
            .observed
            .begin_sweep("forge", &["prj_beta".into()], moment())
            .unwrap();
        ctx.observed
            .append(
                &sweep.id,
                &[crate::storage::observed_types::PendingObservation {
                    kind: "forge.issue".into(),
                    subject_key: "gh:acme/beta#7".into(),
                    payload: serde_json::json!({
                        "title": "upstream",
                        "state": "open",
                        "number": 7
                    }),
                    content_digest: "sha256:issue-7".into(),
                    project_id: Some("prj_beta".into()),
                    source_url: Some("https://github.com/acme/beta/issues/7".into()),
                    promoted: false,
                }],
                moment(),
            )
            .unwrap();
        ctx.observed.rebuild_latest().unwrap();
        let bound = bind_branch(
            ctx,
            json!({"ref": "gh:acme/beta#7", "branch": "feat/upstream", "reason": "test"}),
        )
        .unwrap();
        assert_eq!(bound["item"]["ref"], "gh:acme/beta#7");
        assert_eq!(bound["item"]["project_id"], "prj_beta");
        let view = ctx.declared.read().unwrap();
        let overlay = view.work_overlay("gh:acme/beta#7").unwrap().unwrap();
        assert_eq!(overlay.branches, vec!["feat/upstream".to_string()]);
        drop(view);
        bind_branch(
            ctx,
            json!({"ref": "gh:acme/beta#7", "branch": "feat/upstream", "reason": "test"}),
        )
        .unwrap();
        let view = ctx.declared.read().unwrap();
        let again = view.work_overlay("gh:acme/beta#7").unwrap().unwrap();
        assert_eq!(again.branches, vec!["feat/upstream".to_string()]);
    }

    #[test]
    fn get_many_reads_rows_in_the_order_asked_and_names_the_missing() {
        let built = opened();
        let Built::StepSequential(ctx) = &built else {
            unreachable!("opened() builds a step clock");
        };
        create_work(
            ctx,
            json!({"kind": "feature", "title": "First", "reason": "test"}),
        )
        .unwrap();
        create_work(
            ctx,
            json!({"kind": "feature", "title": "Second", "reason": "test"}),
        )
        .unwrap();
        transition_work(
            ctx,
            json!({"ref": "WI-2", "to_state": "in_progress", "reason": "test"}),
        )
        .unwrap();

        let answer =
            get_many_work(ctx, json!({"refs": ["WI-2", "WI-404", "WI-1", "WI-2"]})).unwrap();

        let refs: Vec<&str> = answer["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["ref"].as_str().unwrap())
            .collect();
        assert_eq!(refs, ["WI-2", "WI-1"]);
        assert_eq!(answer["items"][0]["title"], "Second");
        assert_eq!(answer["items"][0]["state"], "in_progress");
        assert_eq!(answer["items"][0]["kind"], "feature");
        assert_eq!(answer["missing"], json!(["WI-404"]));
    }

    #[test]
    fn get_many_reads_a_finished_item_like_any_other() {
        let built = opened();
        let Built::StepSequential(ctx) = &built else {
            unreachable!("opened() builds a step clock");
        };
        create_work(
            ctx,
            json!({"kind": "feature", "title": "Shipped", "reason": "test"}),
        )
        .unwrap();
        transition_work(
            ctx,
            json!({"ref": "WI-1", "to_state": "wont_do", "reason": "test"}),
        )
        .unwrap();

        let answer = get_many_work(ctx, json!({"refs": ["WI-1"]})).unwrap();

        assert_eq!(answer["items"][0]["ref"], "WI-1");
        assert_eq!(answer["items"][0]["state"], "wont_do");
    }

    #[test]
    fn get_many_bounds_the_batch() {
        let built = opened();
        let Built::StepSequential(ctx) = &built else {
            unreachable!("opened() builds a step clock");
        };
        assert!(get_many_work(ctx, json!({"refs": []})).is_err());
        assert!(get_many_work(ctx, json!({"refs": [""]})).is_err());
        let too_many: Vec<String> = (0..=super::WORK_GET_MANY_LIMIT)
            .map(|n| format!("WI-{n}"))
            .collect();
        assert!(get_many_work(ctx, json!({"refs": too_many})).is_err());
        let at_the_limit: Vec<String> = (0..super::WORK_GET_MANY_LIMIT)
            .map(|n| format!("WI-{n}"))
            .collect();
        let answer = get_many_work(ctx, json!({"refs": at_the_limit})).unwrap();
        assert_eq!(
            answer["missing"].as_array().unwrap().len(),
            super::WORK_GET_MANY_LIMIT
        );
    }

    /// Two engine sessions per row is enough to rank: an approval beats a
    /// running one, and a hibernated one counts but ranks last.
    const ENGINE_SESSIONS: &str = r#"[
      {"id":"eng-run","name":"a","alive":true,"activity":"running","role":"worker"},
      {"id":"eng-appr","name":"b","alive":true,"activity":"awaiting-approval","role":"worker","approval":{"question":"Proceed?","command_excerpt":"x","detected_at":"2026-10-31T06:00:00Z","kind":"permission","key":"k","options":[]}},
      {"id":"eng-hib","name":"c","alive":false,"activity":"hibernated","role":"worker"},
      {"id":"eng-hib2","name":"d","alive":false,"activity":"hibernated","role":"worker"},
      {"id":"eng-ghost","name":"f","alive":true,"activity":"running","role":"worker"},
      {"id":"eng-dead","name":"e","alive":false,"activity":"exited","role":"worker"}
    ]"#;

    fn opened_with_engine() -> Built {
        let dir =
            std::env::temp_dir().join(format!("vogt-work-{}-{}", std::process::id(), unique()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let engine = crate::adapters::engine::EngineClient::new(
            "http://engine",
            None,
            Some(std::sync::Arc::new(move |_path, _, _body, _method| {
                (200, ENGINE_SESSIONS.as_bytes().to_vec())
            })),
        );
        let config = crate::config::VogtConfig {
            data_dir: dir,
            ..crate::config::VogtConfig::default()
        };
        let built = build_context(
            config,
            Some(Principal::new("local:test-user", ActorKind::Human, "Test").unwrap()),
            Some(StepClock::new(moment())),
            Some(SequentialIds::new(None).unwrap()),
            None,
            Some(engine),
            None,
            None,
        )
        .unwrap();
        let Built::StepSequential(ctx) = &built else {
            panic!("expected a step clock");
        };
        ctx.declared.migrate().unwrap();
        ctx.declared
            .bootstrap(&Principal::new("local:test-user", ActorKind::Human, "Test").unwrap())
            .unwrap();
        ctx.observed.migrate().unwrap();
        built
    }

    /// Bind a recorded session (`engine` id) to the item `reference`.
    fn bind_session(built: &Built, reference: &str, session_id: &str, engine: &str, stopped: bool) {
        let Built::StepSequential(ctx) = built else {
            unreachable!("builds a step clock");
        };
        let view = ctx.declared.read().unwrap();
        let item_id = crate::application::resolve::work_item(&view, reference)
            .unwrap()
            .id;
        let actor_id = view.list_actors(1, 0).unwrap()[0].id.clone();
        drop(view);
        let mut txn = ctx.declared.write().unwrap();
        txn.insert_session(&crate::core::CodingSession {
            id: session_id.to_string(),
            engine_session_id: engine.to_string(),
            project_id: "prj_alpha".to_string(),
            work_item_id: Some(item_id),
            actor_id,
            cwd: "/tmp".to_string(),
            template: None,
            model: None,
            effort: None,
            reason: "test".to_string(),
            purpose: None,
            started_by_session: None,
            started_by_kind: None,
            started_at: moment(),
            stopped_at: stopped.then(moment),
        })
        .unwrap();
        txn.commit().unwrap();
    }

    fn live_fixture() -> Built {
        let built = opened_with_engine();
        let Built::StepSequential(ctx) = &built else {
            unreachable!("builds a step clock");
        };
        insert_project(&built, "alpha", false, WriteBack::Disabled);
        for title in ["Mixed", "Dead", "Empty", "Asleep"] {
            create_work(
                ctx,
                json!({"kind": "feature", "title": title, "project": "alpha", "reason": "t"}),
            )
            .unwrap();
        }
        bind_session(&built, "WI-1", "ses_run", "eng-run", false);
        bind_session(&built, "WI-1", "ses_appr", "eng-appr", false);
        bind_session(&built, "WI-1", "ses_hib", "eng-hib", false);
        // Vogt stopped one; the engine's process of the other has exited.
        bind_session(&built, "WI-2", "ses_stopped", "eng-ghost", true);
        bind_session(&built, "WI-2", "ses_dead", "eng-dead", false);
        bind_session(&built, "WI-4", "ses_hib2", "eng-hib2", false);
        built
    }

    fn row<'a>(page: &'a Value, reference: &str) -> &'a Value {
        page["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["ref"] == reference)
            .unwrap_or_else(|| panic!("no {reference} in {page}"))
    }

    #[test]
    fn with_sessions_is_off_by_default_and_adds_no_key() {
        let built = live_fixture();
        let Built::StepSequential(ctx) = &built else {
            unreachable!("builds a step clock");
        };
        for params in [json!({}), json!({"with_sessions": false})] {
            let page = list_work(ctx, params).unwrap();
            for item in page["items"].as_array().unwrap() {
                assert!(item.get("live_sessions").is_none(), "{item}");
            }
        }
        let page = list_work(ctx, json!({"with_sessions": true, "mode": "full"})).unwrap();
        assert!(row(&page, "WI-1").get("live_sessions").is_some(), "{page}");
    }

    #[test]
    fn with_sessions_counts_alive_and_hibernated_and_ranks_worst_by_attention() {
        let built = live_fixture();
        let Built::StepSequential(ctx) = &built else {
            unreachable!("builds a step clock");
        };
        let page = list_work(ctx, json!({"with_sessions": true})).unwrap();
        // running + awaiting approval + hibernated: all three are live, and
        // an approval outranks running and hibernated.
        let mixed = &row(&page, "WI-1")["live_sessions"];
        assert_eq!(mixed["count"], 3, "{page}");
        assert_eq!(mixed["worst"], "approval", "{page}");
        let mut ids: Vec<&str> = mixed["ids"]
            .as_array()
            .unwrap()
            .iter()
            .map(|id| id.as_str().unwrap())
            .collect();
        ids.sort_unstable();
        assert_eq!(ids, ["ses_appr", "ses_hib", "ses_run"]);
        // A stopped record and an exited process are not live.
        assert_eq!(
            row(&page, "WI-2")["live_sessions"],
            json!({"count": 0, "worst": null, "ids": []})
        );
        // No session at all.
        assert_eq!(
            row(&page, "WI-3")["live_sessions"],
            json!({"count": 0, "worst": null, "ids": []})
        );
        // A hibernated session counts, and is the worst there is.
        assert_eq!(
            row(&page, "WI-4")["live_sessions"],
            json!({"count": 1, "worst": "hibernated", "ids": ["ses_hib2"]})
        );
    }

    #[test]
    fn with_sessions_costs_one_grouped_read_for_the_whole_page() {
        let built = live_fixture();
        let Built::StepSequential(ctx) = &built else {
            unreachable!("builds a step clock");
        };
        let reads = |f: &dyn Fn()| {
            crate::storage::sqlite::declared::READS.with(|r| *r.borrow_mut() = Default::default());
            f();
            crate::storage::sqlite::declared::READS.with(|r| *r.borrow())
        };
        let without = reads(&|| {
            list_work(ctx, json!({})).unwrap();
        });
        assert_eq!((without.grouped, without.per_item), (0, 0));
        let with = reads(&|| {
            let page = list_work(ctx, json!({"with_sessions": true})).unwrap();
            assert_eq!(page["items"].as_array().unwrap().len(), 4);
        });
        assert_eq!((with.grouped, with.per_item), (1, 0));
    }

    #[test]
    fn work_total_counts_open_declared_and_unadopted_upstream_items() {
        // `place.metrics` `work_total` is `list_work(limit=1).total`: open
        // declared items plus upstream-truth items no `work_link` adopted.
        // A finished declared item does not count.
        let built = opened();
        let Built::StepSequential(ctx) = &built else {
            unreachable!("opened() builds a step clock");
        };
        assert_eq!(list_work_total(ctx).unwrap(), 0);
        create_work(
            ctx,
            json!({"kind": "feature", "title": "Open", "reason": "test"}),
        )
        .unwrap();
        create_work(
            ctx,
            json!({"kind": "feature", "title": "Finished", "reason": "test"}),
        )
        .unwrap();
        transition_work(
            ctx,
            json!({"ref": "WI-2", "to_state": "wont_do", "reason": "test"}),
        )
        .unwrap();
        assert_eq!(list_work_total(ctx).unwrap(), 1);
        assert_eq!(list_work(ctx, json!({"limit": 1})).unwrap()["total"], 1);

        insert_project(&built, "beta", true, WriteBack::Disabled);
        let sweep = ctx
            .observed
            .begin_sweep("forge", &["prj_beta".into()], moment())
            .unwrap();
        ctx.observed
            .append(
                &sweep.id,
                &[crate::storage::observed_types::PendingObservation {
                    kind: "forge.issue".into(),
                    subject_key: "gh:acme/beta#7".into(),
                    payload: serde_json::json!({
                        "title": "upstream",
                        "state": "open",
                        "number": 7
                    }),
                    content_digest: "sha256:issue-7".into(),
                    project_id: Some("prj_beta".into()),
                    source_url: Some("https://github.com/acme/beta/issues/7".into()),
                    promoted: false,
                }],
                moment(),
            )
            .unwrap();
        ctx.observed.rebuild_latest().unwrap();
        assert_eq!(list_work_total(ctx).unwrap(), 2);
        assert_eq!(list_work(ctx, json!({"limit": 1})).unwrap()["total"], 2);
    }

    #[test]
    fn an_exact_ref_query_leads_the_page_ahead_of_substring_hits() {
        let built = opened();
        let Built::StepSequential(ctx) = &built else {
            unreachable!("opened() builds a step clock");
        };
        // WI-1 is created first and cites WI-10 in its body, so a substring
        // match on "10" reaches it before WI-10 in creation order.
        create_work(
            ctx,
            json!({"kind": "feature", "title": "An older note", "body": "see WI-10 for the real one", "reason": "test"}),
        )
        .unwrap();
        for n in 2..=10 {
            create_work(
                ctx,
                json!({"kind": "chore", "title": format!("Item {n}"), "reason": "test"}),
            )
            .unwrap();
        }

        for query in ["10", "WI-10", "wi-10"] {
            let answer = list_work(ctx, json!({"query": query, "limit": 1})).unwrap();
            assert_eq!(answer["items"][0]["ref"], "WI-10", "query {query}");
            assert_eq!(answer["total"], 2, "query {query}");
        }
        let whole = list_work(ctx, json!({"query": "10"})).unwrap();
        let refs: Vec<&str> = whole["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["ref"].as_str().unwrap())
            .collect();
        assert_eq!(refs, ["WI-10", "WI-1"]);
    }
}
