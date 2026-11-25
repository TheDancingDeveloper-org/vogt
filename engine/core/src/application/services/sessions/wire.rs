use super::*;

// -- the registry's entry points ----------------------------------------------
//
// One wrapper per operation: parse the transport's JSON into the parameter
// model (defaults matching the Python models), then the service. The table in
// `registry::service_for` is the only place that names them.

crate::typed_op!(start_session_op, StartRequest, start_session_typed);
crate::typed_op!(stop_session_op, StopSessionParams, stop_session_typed);
crate::typed_op!(list_sessions_op, ListRequest, list_sessions_typed);
crate::typed_op!(
    list_session_templates_op,
    TemplatesParams,
    list_session_templates_typed
);
crate::typed_op!(sweep_sessions_op, SweepSessionsParams, sweep_sessions_typed);
crate::typed_op!(log_tail_op, LogTailParams, log_tail_typed);
crate::typed_op!(input_op, InputParams, input_typed);
crate::typed_op!(screen_op, ScreenParams, screen_typed);
crate::typed_op!(wait_op, WaitParams, wait_typed);
crate::typed_op!(wake_op, WakeParams, wake_typed);
crate::typed_op!(keep_awake_op, KeepAwakeParams, keep_awake_typed);
crate::typed_op!(set_role_op, SetRoleParams, set_role_typed);
crate::typed_op!(answer_op, AnswerParams, answer_typed);
crate::typed_op!(hibernate_op, HibernateParams, hibernate_typed);

fn start_session_typed<C, I>(
    ctx: &AppContext<C, I>,
    request: StartRequest,
) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let StartRequest {
        params: parsed,
        provider,
    } = request;
    check_named_provider(ctx, provider.as_deref(), parsed.model.as_deref())?;
    let warning = unserved_model_warning(ctx, parsed.model.as_deref());
    let mut result =
        serde_json::to_value(start_session(ctx, &parsed)?).expect("a session result serialises");
    if let (Some(warning), Value::Object(map)) = (warning, &mut result) {
        map.insert("warning".to_string(), Value::String(warning));
    }
    Ok(result)
}

fn list_session_templates_typed<C, I>(
    ctx: &AppContext<C, I>,
    _params: TemplatesParams,
) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let Some(engine) = ctx.engine.as_ref() else {
        return Ok(
            json!({"templates": [], "engine": "no session engine is configured (VOGT_ENGINE_URL is unset)"}),
        );
    };
    match engine.list_session_templates() {
        Ok(templates) => Ok(json!({"templates": templates})),
        Err(VogtError::EngineUnavailable(error)) => Ok(json!({"templates": [], "engine": error})),
        Err(error) => Err(error),
    }
}

fn stop_session_typed<C, I>(
    ctx: &AppContext<C, I>,
    parsed: StopSessionParams,
) -> Result<SessionResult, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    stop_session(ctx, &parsed)
}

fn list_sessions_typed<C, I>(
    ctx: &AppContext<C, I>,
    request: ListRequest,
) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let parsed = &request.params;
    if request.starters_only {
        return list_starters(ctx, parsed);
    }
    let mut listed =
        serde_json::to_value(list_sessions(ctx, parsed)?).expect("a session result serialises");
    if parsed.mode == "full" {
        slim_full_rows(&mut listed, parsed.fields.as_deref());
    }
    Ok(listed)
}

fn sweep_sessions_typed<C, I>(
    ctx: &AppContext<C, I>,
    parsed: SweepSessionsParams,
) -> Result<SessionSweepResult, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    sweep_sessions(ctx, &parsed)
}

fn log_tail_typed<C, I>(
    ctx: &AppContext<C, I>,
    parsed: LogTailParams,
) -> Result<LogTailResult, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    log_tail(ctx, &parsed)
}

fn input_typed<C, I>(
    ctx: &AppContext<C, I>,
    parsed: InputParams,
) -> Result<SessionInputResult, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    input(ctx, &parsed)
}

fn screen_typed<C, I>(
    ctx: &AppContext<C, I>,
    parsed: ScreenParams,
) -> Result<SessionScreenResult, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    screen(ctx, &parsed)
}

fn wait_typed<C, I>(
    ctx: &AppContext<C, I>,
    parsed: WaitParams,
) -> Result<SessionWaitResult, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    wait(ctx, &parsed)
}

fn wake_typed<C, I>(ctx: &AppContext<C, I>, parsed: WakeParams) -> Result<SessionResult, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    wake(ctx, &parsed)
}

fn keep_awake_typed<C, I>(
    ctx: &AppContext<C, I>,
    parsed: KeepAwakeParams,
) -> Result<SessionResult, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    keep_awake(ctx, &parsed)
}

fn set_role_typed<C, I>(
    ctx: &AppContext<C, I>,
    parsed: SetRoleParams,
) -> Result<SessionResult, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    set_role(ctx, &parsed)
}

fn answer_typed<C, I>(ctx: &AppContext<C, I>, parsed: AnswerParams) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    answer(ctx, &parsed)
}

fn hibernate_typed<C, I>(
    ctx: &AppContext<C, I>,
    parsed: HibernateParams,
) -> Result<SessionResult, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    hibernate(ctx, &parsed)
}

/// Decode the transport's JSON with a reader that keeps the old messages: an
/// `InvalidRequest` becomes the deserializer's custom error with the same text.
fn from_wire<'de, D, P>(
    deserializer: D,
    read: impl FnOnce(&Value) -> Result<P, VogtError>,
) -> Result<P, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;
    let params = <Value as serde::Deserialize>::deserialize(deserializer)?;
    read(&params).map_err(|error| match error {
        VogtError::InvalidRequest(message) => D::Error::custom(message),
        other => D::Error::custom(other),
    })
}

pub(super) fn rename_session_typed<C, I>(
    ctx: &AppContext<C, I>,
    parsed: RenameSessionParams,
) -> Result<SessionResult, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    rename_session(ctx, &parsed)
}

pub(super) fn remove_session_typed<C, I>(
    ctx: &AppContext<C, I>,
    parsed: RemoveSessionParams,
) -> Result<SessionResult, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    remove_session(ctx, &parsed)
}

pub(super) fn session_token_typed<C, I>(
    ctx: &AppContext<C, I>,
    parsed: SessionTokenParams,
) -> Result<SessionTokenResult, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    session_token(ctx, &parsed)
}

// -- wire decoding: custom Deserialize keeps every message byte-identical --

impl<'de> serde::Deserialize<'de> for StopSessionParams {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        from_wire(deserializer, |params| {
            Ok(Self {
                id: require_session_id(params)?,
                reason: require_string(params, "reason")?,
            })
        })
    }
}

impl<'de> serde::Deserialize<'de> for SweepSessionsParams {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        from_wire(deserializer, |params| {
            Ok(Self {
                project: optional_string(params, "project")?,
                screen_lines: optional_i64(params, "screen_lines", 8)?,
                stall_after_minutes: optional_i64(params, "stall_after_minutes", 10)?,
                exclude_roles: optional_string_list(params, "exclude_roles")?
                    .unwrap_or_else(|| vec!["operator".to_string()]),
                supervised_by: optional_string(params, "supervised_by")?,
                since: optional_string(params, "since")?,
            })
        })
    }
}

impl<'de> serde::Deserialize<'de> for LogTailParams {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        from_wire(deserializer, |params| {
            Ok(Self {
                id: require_session_id(params)?,
                tail_bytes: optional_i64(params, "tail_bytes", 64 * 1024)?,
                strip_ansi: optional_bool(params, "strip_ansi")?.unwrap_or(true),
            })
        })
    }
}

impl<'de> serde::Deserialize<'de> for InputParams {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        from_wire(deserializer, |params| {
            Ok(Self {
                id: require_session_id(params)?,
                text: optional_string(params, "text")?,
                keys: optional_string_list(params, "keys")?,
                submit: optional_bool(params, "submit")?.unwrap_or(false),
                reason: require_string(params, "reason")?,
                confirm: optional_bool(params, "confirm")?.unwrap_or(true),
                wake_timeout_s: optional_i64(params, "wake_timeout_s", 120)?,
            })
        })
    }
}

impl<'de> serde::Deserialize<'de> for ScreenParams {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        from_wire(deserializer, |params| {
            Ok(Self {
                id: require_session_id(params)?,
                scrollback_lines: optional_i64(params, "scrollback_lines", 0)?,
            })
        })
    }
}

impl<'de> serde::Deserialize<'de> for WaitParams {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        from_wire(deserializer, |params| {
            Ok(Self {
                id: require_session_id(params)?,
                until: optional_string(params, "until")?.unwrap_or_else(|| "ready".to_string()),
                timeout_s: optional_i64(params, "timeout_s", 120)?,
                screen: optional_bool(params, "screen")?.unwrap_or(false),
            })
        })
    }
}

impl<'de> serde::Deserialize<'de> for WakeParams {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        from_wire(deserializer, |params| {
            Ok(Self {
                id: require_session_id(params)?,
                reason: require_string(params, "reason")?,
                force: optional_bool(params, "force")?.unwrap_or(false),
            })
        })
    }
}

impl<'de> serde::Deserialize<'de> for KeepAwakeParams {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        from_wire(deserializer, |params| {
            Ok(Self {
                id: require_session_id(params)?,
                keep_awake: require_bool(params, "keep_awake")?,
                reason: require_string(params, "reason")?,
            })
        })
    }
}

impl<'de> serde::Deserialize<'de> for SetRoleParams {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        from_wire(deserializer, |params| {
            Ok(Self {
                id: require_session_id(params)?,
                role: require_string(params, "role")?,
                reason: require_string(params, "reason")?,
            })
        })
    }
}

impl<'de> serde::Deserialize<'de> for AnswerParams {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        from_wire(deserializer, |params| {
            Ok(Self {
                id: require_session_id(params)?,
                option: optional_i64_some(params, "option")?,
                label: optional_string(params, "label")?,
                expect_question: optional_string(params, "expect_question")?,
                reason: require_string(params, "reason")?,
            })
        })
    }
}

impl<'de> serde::Deserialize<'de> for HibernateParams {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        from_wire(deserializer, |params| {
            Ok(Self {
                id: require_session_id(params)?,
                reason: require_string(params, "reason")?,
                allow_shell: optional_bool(params, "allow_shell")?.unwrap_or(false),
                cold_wake_opt_out: optional_bool(params, "cold_wake_opt_out")?,
                superseded_by: optional_string(params, "superseded_by")?,
            })
        })
    }
}

/// `session.start` parameters plus the named `provider`, which is checked
/// against the engine's registry before the start and is not a stored field.
pub(super) struct StartRequest {
    params: StartSessionParams,
    provider: Option<String>,
}

impl<'de> serde::Deserialize<'de> for StartRequest {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        from_wire(deserializer, |params| {
            let parsed = StartSessionParams {
                project: optional_string(params, "project")?,
                work_item: optional_string(params, "work_item")?,
                name: optional_string(params, "name")?,
                template: optional_string(params, "template")?,
                placement: optional_string(params, "placement")?,
                task: optional_string(params, "task")?,
                reason: require_string(params, "reason")?,
                model: optional_string(params, "model")?,
                effort: optional_string(params, "effort")?,
                resume: optional_string(params, "resume")?,
                permission_mode: optional_string(params, "permission_mode")?
                    .unwrap_or_else(|| "default".to_string()),
                autopilot: optional_bool(params, "autopilot")?,
                role: optional_string(params, "role")?.unwrap_or_else(|| "worker".to_string()),
                purpose: optional_string(params, "purpose")?,
                review_of: optional_string(params, "review_of")?,
                from_handoff: optional_string(params, "from_handoff")?,
                budget: match params.get("budget") {
                    None | Some(Value::Null) => None,
                    Some(raw) => Some(serde_json::from_value(raw.clone()).map_err(|error| {
                        VogtError::InvalidRequest(format!("invalid budget: {error}"))
                    })?),
                },
                worktree: optional_bool(params, "worktree")?,
                repo_map: optional_bool(params, "repo_map")?,
                mcp_set: optional_mcp_set(params)?,
                cold_wake_opt_out: optional_bool(params, "cold_wake_opt_out")?.unwrap_or(false),
                wake_on: optional_string_list(params, "wake_on")?.unwrap_or_default(),
                explicit_null: PROFILE_FIELDS
                    .into_iter()
                    .filter(|field| matches!(params.get(*field), Some(Value::Null)))
                    .collect(),
                override_reason: optional_string(params, "override")?
                    .map(|text| {
                        if text.is_empty() {
                            Err(VogtError::InvalidRequest(
                                "override must not be empty".to_string(),
                            ))
                        } else {
                            Ok(text)
                        }
                    })
                    .transpose()?,
            };
            Ok(Self {
                params: parsed,
                provider: optional_string(params, "provider")?,
            })
        })
    }
}

/// `session.templates` takes no fields, only an object body.
pub(super) struct TemplatesParams;

impl<'de> serde::Deserialize<'de> for TemplatesParams {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        from_wire(deserializer, |params| {
            if !params.is_object() {
                return Err(VogtError::InvalidRequest(
                    "parameters must be an object".into(),
                ));
            }
            Ok(Self)
        })
    }
}

/// `session.list` parameters plus the `starters_only` decision, made while
/// decoding so its refusal comes after every field's own.
pub(super) struct ListRequest {
    params: ListSessionsParams,
    starters_only: bool,
}

impl<'de> serde::Deserialize<'de> for ListRequest {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        from_wire(deserializer, |params| {
            let parsed = ListSessionsParams {
                project: optional_string(params, "project")?,
                work_item: optional_string(params, "work_item")?,
                include_stopped: optional_bool(params, "include_stopped")?.unwrap_or(false),
                limit: optional_i64(params, "limit", 50)?,
                offset: optional_i64(params, "offset", 0)?,
                order: optional_string(params, "order")?.unwrap_or_else(|| "started".to_string()),
                mode: optional_string(params, "mode")?.unwrap_or_else(|| "digest".to_string()),
                states: optional_string_list(params, "states")?,
                stall_after_minutes: optional_i64(params, "stall_after_minutes", 10)?,
                include_hidden: optional_bool(params, "include_hidden")?.unwrap_or(false),
                fields: optional_string_list(params, "fields")?,
                started_by_session: optional_string(params, "started_by_session")?,
            };
            let flag = optional_bool(params, "starters_only")?.unwrap_or(false);
            let set = params
                .get("started_by_session")
                .is_some_and(|value| !value.is_null());
            let starters_only = starters_only(&parsed, flag, set)?;
            Ok(Self {
                params: parsed,
                starters_only,
            })
        })
    }
}

impl<'de> serde::Deserialize<'de> for RenameSessionParams {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        from_wire(deserializer, |params| {
            Ok(Self {
                id: require_string(params, "id")?,
                name: require_string(params, "name")?,
                reason: require_string(params, "reason")?,
            })
        })
    }
}

impl<'de> serde::Deserialize<'de> for RemoveSessionParams {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        from_wire(deserializer, |params| {
            Ok(Self {
                id: require_string(params, "id")?,
                reason: require_string(params, "reason")?,
            })
        })
    }
}

impl<'de> serde::Deserialize<'de> for SessionTokenParams {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        from_wire(deserializer, |params| {
            let parsed = Self {
                engine_session_id: require_string(params, "engine_session_id")?,
                revoke: params
                    .get("revoke")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                reason: require_string(params, "reason")?,
            };
            if !ENGINE_SESSION_ID.is_match(&parsed.engine_session_id) {
                return Err(VogtError::InvalidRequest(
                    "engine_session_id must be a UUID".to_string(),
                ));
            }
            Ok(parsed)
        })
    }
}

/// A session id the caller must give. Blank or whitespace is the same refusal
/// `resolve_target` gives, so the JSON entry points and the typed functions
/// agree on the text.
fn require_session_id(params: &Value) -> Result<String, VogtError> {
    match params.get("id").and_then(Value::as_str) {
        Some(text) if !text.trim().is_empty() => Ok(text.to_string()),
        _ => Err(VogtError::InvalidRequest(
            "a session id is required (ses_… or the engine's session UUID)".to_string(),
        )),
    }
}

/// A bool that may be absent.
fn optional_bool(params: &Value, field: &str) -> Result<Option<bool>, VogtError> {
    match params.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        Some(_) => Err(VogtError::InvalidRequest(format!(
            "{field} must be a boolean"
        ))),
    }
}

/// A bool the caller must give.
fn require_bool(params: &Value, field: &str) -> Result<bool, VogtError> {
    optional_bool(params, field)?
        .ok_or_else(|| VogtError::InvalidRequest(format!("{field} is required")))
}

/// An integer that may be absent, unlike `optional_i64` which fills a default.
fn optional_i64_some(params: &Value, field: &str) -> Result<Option<i64>, VogtError> {
    match params.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(number)) => number
            .as_i64()
            .map(Some)
            .ok_or_else(|| VogtError::InvalidRequest(format!("{field} must be an integer"))),
        Some(_) => Err(VogtError::InvalidRequest(format!(
            "{field} must be an integer"
        ))),
    }
}

/// A list of strings that may be absent.
fn optional_string_list(params: &Value, field: &str) -> Result<Option<Vec<String>>, VogtError> {
    match params.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| {
                item.as_str().map(str::to_string).ok_or_else(|| {
                    VogtError::InvalidRequest(format!("{field} must be a list of strings"))
                })
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Some),
        Some(_) => Err(VogtError::InvalidRequest(format!(
            "{field} must be a list of strings"
        ))),
    }
}
