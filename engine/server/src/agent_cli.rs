//! Turning "run it on GPT 5.6, medium, on this brief" into the argv one agent
//! CLI wants
//!
//! Vogt decides *which* model a session was asked for and audits that decision;
//! how a model id reaches a running process is `claude --model`, `codex -m`, or
//! `opencode --model`, and which of those exist is a property of this pod's
//! image rather than of the estate. Claude's own flags live in
//! `vogt_session_agent` (WI-3881) because the pod's launch line builds them
//! too; this module delegates so host and pod cannot drift.
//!
//! The same goes for the other things a launch can carry: a previous
//! conversation to resume, and the first prompt that points the agent at the
//! brief the engine wrote for it.
//!
//! Four rules, each written against a specific way this could go wrong:
//!
//! **A command with no mapping is refused, never started plain.** A session
//! that quietly ignored `model` (or `resume`) would spawn, run, answer, and be
//! the wrong model or the wrong conversation — a failure with no symptom. The
//! refusal names the binary, so the reader learns which template they
//! actually asked for.
//!
//! **The values are validated before they become argv.** The caller here is
//! ultimately an LLM tool call, and these strings are handed to a process
//! spawn. A model id is a narrow vocabulary — letters, digits, and `.` `_`
//! `-` `/` `:` — a conversation id narrower still, and anything else is
//! refused rather than escaped, because escaping is where "it's only a model
//! name" turns into an extra flag.
//!
//! **The flags go on the end.** Every supported form ends with the agent
//! binary and its own arguments (`vogt-agent-auth run -- claude`), so
//! appending puts them where that binary reads them. The one exception is
//! Codex's `resume`, a subcommand, which goes straight after the binary.
//!
//! **The brief is never argv.** The first prompt is a fixed one-line pointer
//! to the brief file, so a paragraphs-long work-item brief stays out of `ps`
//! and out of quoting.

use std::path::{Path, PathBuf};

use uuid::Uuid;

use crate::error::{ApiError, Result};

/// Ceiling on a model id, effort level or conversation id. Real ones are far
/// shorter; this is only here so a pathological string cannot reach a spawn.
const MAX_VALUE_LEN: usize = 128;

/// What the engine knows how to tell a model to.
///
/// Kept as data rather than scattered through `create`, so the answer to
/// "which agent CLIs can be asked for a model" is one readable list.
const KNOWN: &[&str] = &["claude", "codex", "opencode"];

/// Environment every engine-launched Claude Code session gets, *before* the
/// template's and the caller's own (which therefore still win).
///
/// A session the engine starts is often driven by another agent over
/// `/input` rather than by a person at a keyboard, and two of Claude Code's
/// interactive niceties are hazards there:
///
/// - **prompt suggestions** draw a greyed-out "next prompt" in the input box
///   that a bare Enter accepts — a driver that sends Enter to submit what it
///   typed can submit a suggestion it never read;
/// - **the session feedback survey** is a prompt that takes the next
///   keystrokes as its answer.
///
/// Both are documented Claude Code variables. The "Teach auto mode about your
/// environment?" dialog has no variable; the pod entrypoint dismisses it in
/// Claude Code's own global config instead (`engine/deploy/entrypoint.sh`).
pub const CLAUDE_QUIET_ENV: &[(&str, &str)] = &[
    ("CLAUDE_CODE_ENABLE_PROMPT_SUGGESTION", "false"),
    ("CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY", "1"),
];

/// What a session asked of the agent CLI it runs, beyond the command itself.
///
/// Not `Default`: `codex_session_id` is on unless a caller opts out, and a
/// derived default would silently turn the pin off (WI-1354).
#[derive(Debug, Clone)]
pub struct LaunchRequest<'a> {
    pub model: Option<&'a str>,
    pub effort: Option<&'a str>,
    /// The agent CLI's own id for a previous conversation to continue.
    pub resume: Option<&'a str>,
    /// Where the session's brief will be written, when it has one. The agent
    /// is told to read it as its first prompt.
    pub brief_file: Option<&'a Path>,
    /// The engine's id for the session. A fresh, bare Claude Code or
    /// fork-Codex launch uses it as the conversation id, so the conversation
    /// can later be resumed by the id every session and history listing
    /// already shows. Codex is pinned when
    /// [`LaunchRequest::codex_session_id`] is set, which it is by default:
    /// the image bakes the fork that accepts the flag (WI-1354).
    pub session_id: Option<Uuid>,
    /// The Codex binary accepts `--session-id` (the WI-1126 fork the image
    /// bakes). A fresh Codex launch is pinned when this is true, the default.
    /// Stock npm Codex refuses an unknown flag, so a deployment that pins
    /// that binary sets it false and the launch is left unpinned.
    pub codex_session_id: bool,
    /// `default` / `accept-edits` / `bypass` (WI-926). Codex takes all three (WI-1143); its
    /// sandbox is `danger-full-access` because the pod cannot create the user
    /// namespace `workspace-write` needs.
    pub permission_mode: Option<&'a str>,
    /// The deployment's driven-session settings (an `autoMode` policy) to
    /// hand Claude Code with `--settings`, when it has one.
    pub settings_file: Option<&'a Path>,
    /// Extra directories the agent may read, as `--add-dir`. A fresh Claude
    /// launch derives this from the brief's parent; a wake passes the
    /// directories recorded at the first start so they survive a brief that
    /// is not sent again (WI-1250).
    pub add_dirs: &'a [std::path::PathBuf],
    /// The deployment's opencode driven-session policy (an opencode config
    /// with a `permission` block), for the default posture (WI-932).
    pub opencode_config: Option<&'a str>,
    /// Codex's `approval_policy` for the `default` and `accept-edits`
    /// postures: `-a <this>` (WI-2028). `"never"` by default, so a driven
    /// worker does not stop on every command, matching Claude Code's bypassed
    /// workers; a deployment can set `ENGINE_CODEX_APPROVAL_POLICY=on-request`
    /// to have a person (or an oversight session) answer each command.
    /// `bypass` ignores it, and `codex exec` rejects `-a`.
    pub codex_approval_policy: &'a str,
    /// The trimmed MCP set, when the start asked for one (WI-2740).
    pub mcp: Option<&'a crate::mcp_set::McpTrim>,
    /// Codex's `model_auto_compact_token_limit`, when Vogt overrides the
    /// configured one: a session that thrashed on compaction restarts above
    /// the size it came back to (WI-2832, `compact_backoff`).
    pub codex_compact_limit: Option<u64>,
}

impl<'a> LaunchRequest<'a> {
    /// A launch that asks for nothing extra. Codex is pinned (`codex_session_id`
    /// true): the image bakes the fork that accepts `--session-id` (WI-1354).
    pub fn bare() -> Self {
        Self {
            model: None,
            effort: None,
            resume: None,
            brief_file: None,
            session_id: None,
            codex_session_id: true,
            permission_mode: None,
            settings_file: None,
            add_dirs: &[],
            opencode_config: None,
            codex_approval_policy: CODEX_APPROVAL_DEFAULT,
            mcp: None,
            codex_compact_limit: None,
        }
    }
}

/// The Codex-only launch inputs that live outside [`LaunchRequest`] because
/// they are built from the session's cwd, scope and provider registry. Host
/// and pod paths pass the same values to the shared builder (WI-3882).
#[derive(Default)]
pub struct CodexExtras<'a> {
    /// Workspace guidance gathered by the engine.
    pub guidance: Option<&'a str>,
    /// The scope's charters, appended after the guidance.
    pub charter: Option<&'a str>,
    pub providers: &'a [vogt_session_agent::CodexProvider],
    /// The selected `model_provider` registry key.
    pub provider: Option<&'a str>,
    /// A catalog written for this launch, when the launch model is registered
    /// by a provider but absent from the catalog Codex would otherwise load.
    pub catalog: Option<&'a Path>,
    /// The provider registry's fallback model, used only when the caller named
    /// no model.
    pub config_model: Option<&'a str>,
    /// The checkout Codex should trust without asking.
    pub trust_cwd: Option<&'a Path>,
    /// The MCP servers in this launch, for `default_tools_approval_mode`.
    pub mcp_servers: &'a [String],
}

/// opencode's `accept-edits` posture: file work is allowed, everything else
/// asks, like Claude Code's `acceptEdits`. Every key is named because a key
/// left out falls through to the user's own opencode config.
pub const OPENCODE_ACCEPT_EDITS: &str = r#"{"permission":{"read":"allow","edit":"allow","write":"allow","glob":"allow","grep":"allow","list":"allow","lsp":"allow","todowrite":"allow","question":"allow","task":"ask","skill":"ask","bash":"ask","webfetch":"ask","websearch":"ask","external_directory":"ask","doom_loop":"ask"}}"#;

/// opencode's `bypass` posture: nothing asked, nothing refused. A person's
/// grant only (the core refuses it to agents, WI-926).
pub const OPENCODE_BYPASS: &str = r#"{"permission":{"read":"allow","edit":"allow","write":"allow","glob":"allow","grep":"allow","list":"allow","lsp":"allow","todowrite":"allow","question":"allow","task":"allow","skill":"allow","bash":"allow","webfetch":"allow","websearch":"allow","external_directory":"allow","doom_loop":"allow"}}"#;

/// The `OPENCODE_CONFIG_CONTENT` an opencode session starts with for a
/// posture: the deployment's policy in the default posture (none when it has
/// none), the fixed configs above otherwise.
pub fn opencode_posture(mode: Option<&str>, policy: Option<&str>) -> Option<String> {
    match mode.map(str::trim).filter(|m| !m.is_empty()) {
        Some("bypass") => Some(OPENCODE_BYPASS.to_string()),
        Some("accept-edits") | Some("accept_edits") => Some(OPENCODE_ACCEPT_EDITS.to_string()),
        _ => policy.map(str::to_string),
    }
}

/// A permission posture a session may be started with, as Claude Code's
/// flags for Claude Code. `None` is the default posture: no flag at all.
///
/// The flags live in `vogt_session_agent` (WI-3881), because the pod's own
/// launch line needs the same ones: an agent template runs the session agent,
/// not this rewritten argv, so the two must not be able to drift.
pub fn permission_flags(mode: Option<&str>) -> Result<Option<Vec<String>>> {
    vogt_session_agent::claude_permission_flags(mode).map_err(ApiError::BadRequest)
}

pub use vogt_session_agent::{CODEX_APPROVAL_DEFAULT, CODEX_APPROVAL_POLICIES};

/// Check `ENGINE_CODEX_APPROVAL_POLICY` (or the config file's key): one of
/// [`CODEX_APPROVAL_POLICIES`], `never` when unset or blank.
pub fn parse_codex_approval_policy(raw: Option<&str>) -> Result<String> {
    let said = raw.map(str::trim).filter(|value| !value.is_empty());
    let Some(said) = said else {
        return Ok(CODEX_APPROVAL_DEFAULT.to_string());
    };
    if CODEX_APPROVAL_POLICIES.contains(&said) {
        return Ok(said.to_string());
    }
    Err(ApiError::Config(format!(
        "ENGINE_CODEX_APPROVAL_POLICY {said:?} is not one of {}",
        CODEX_APPROVAL_POLICIES.join(", ")
    )))
}

/// A permission posture as Codex flags (WI-1143, WI-2028, WI-3864).
///
/// The flags themselves live in `vogt_session_agent`, because the pod's own
/// launch line needs the same ones: an agent template runs the session agent,
/// not this rewritten argv, so the two must not be able to drift. This
/// function only maps the crate's error into the engine's `ApiError`.
pub fn codex_permission_flags(
    mode: Option<&str>,
    exec: bool,
    approval: &str,
) -> Result<Option<Vec<String>>> {
    vogt_session_agent::codex_permission_flags(mode, exec, approval).map_err(ApiError::BadRequest)
}

/// The command and the extra environment a session should start with.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Launch {
    /// `None` means the command is unchanged.
    pub command: Option<Vec<String>>,
    /// Defaults to put *before* the template's and the caller's env.
    pub env: Vec<(String, String)>,
}

/// The first prompt an agent started with a brief is given: where the brief
/// is and what to do with it. Short and fixed, so it is quoting-proof and
/// carries nothing of the brief into a `ps` listing.
pub fn brief_instruction(path: &Path) -> String {
    vogt_session_agent::brief_instruction(path)
}

/// Rewrite `command` so the agent CLI it names runs `model` at `effort`.
///
/// `Ok(None)` means nothing was asked for and the command is unchanged —
/// which is the ordinary case and byte-for-byte what the engine did before
/// this existed.
pub fn apply(
    command: Option<&[String]>,
    model: Option<&str>,
    effort: Option<&str>,
) -> Result<Option<Vec<String>>> {
    Ok(launch(
        command,
        &LaunchRequest {
            model,
            effort,
            ..LaunchRequest::bare()
        },
    )?
    .command)
}

/// Everything the engine adds to an agent CLI's launch, in one place: model
/// and effort flags, a conversation to resume, the first prompt that points
/// the agent at its brief, and Claude Code's quiet defaults.
///
/// Asking for something only an agent CLI can do — a model, an effort, a
/// resume — on a command the engine cannot tell is refused. A brief is not an
/// ask of that kind: a plain shell started with one keeps only the
/// `VOGT_ENGINE_AGENT_TASK_PROMPT_FILE` variable it always had.
pub fn launch(command: Option<&[String]>, req: &LaunchRequest<'_>) -> Result<Launch> {
    launch_with_codex(command, req, &CodexExtras::default())
}

/// [`launch`] with the Codex inputs that come from the session's cwd, scope and
/// provider registry (WI-3882). A non-Codex launch ignores `codex`.
pub fn launch_with_codex(
    command: Option<&[String]>,
    req: &LaunchRequest<'_>,
    codex: &CodexExtras<'_>,
) -> Result<Launch> {
    let model = trimmed(req.model);
    let effort = trimmed(req.effort);
    let resume = trimmed(req.resume);
    if let Some(value) = model {
        validate("model", value)?;
    }
    if let Some(value) = effort {
        validate("effort", value)?;
    }
    if let Some(value) = resume {
        validate_conversation_id(value)?;
    }
    let permission = permission_flags(req.permission_mode)?;
    // `codex exec` rejects `-a`. The binary is not known yet, so the pin is
    // decided from the word after it and only the codex arm reads the result.
    let codex_exec = command.filter(|c| !c.is_empty()).is_some_and(|c| {
        let (idx, binary) = agent_binary(c);
        binary == "codex" && c.get(idx + 1).is_some_and(|arg| arg == "exec")
    });
    let codex_permission =
        codex_permission_flags(req.permission_mode, codex_exec, req.codex_approval_policy)?;
    let asked = model.is_some()
        || effort.is_some()
        || resume.is_some()
        || permission.is_some()
        || codex_permission.is_some();

    let Some(command) = command.filter(|c| !c.is_empty()) else {
        if asked {
            return Err(ApiError::BadRequest(
                "a session with no command runs the default shell, which has no \
                 model to choose or conversation to resume; start it with an \
                 agent template (Claude Code, Codex, OpenCode)"
                    .into(),
            ));
        }
        return Ok(Launch::default());
    };
    let (binary_idx, binary) = agent_binary(command);
    if !KNOWN.contains(&binary.as_str()) {
        if asked {
            return Err(ApiError::BadRequest(format!(
                "session command `{binary}` is not an agent CLI this engine \
                 knows how to pass a model or a resume to (it knows: {}); it \
                 would have started on its own default and looked like it \
                 worked",
                KNOWN.join(", ")
            )));
        }
        return Ok(Launch::default());
    }
    if binary == "opencode" && effort.is_some() {
        // Named rather than dropped: OpenCode takes a model and has no effort
        // control, and a session that ignored the second half of the request
        // would look like it honoured all of it.
        return Err(ApiError::BadRequest(
            "opencode takes a model but has no reasoning-effort control; ask \
             for a model alone, or use Claude Code or Codex for an effort level"
                .into(),
        ));
    }
    // Whether the command is the bare agent — nothing of the template's own
    // after the binary. Only then is the conversation id pinned: a command
    // that already carries `--continue` or a `--session-id` of its own would
    // be refused with ours added.
    let bare = binary_idx + 1 == command.len();
    let prompt = req.brief_file.map(brief_instruction);

    let mut rewritten = command.to_vec();
    let mut env = Vec::new();
    match binary.as_str() {
        "claude" => {
            // The brief lives under the engine's state directory, outside the
            // session's working directory, and reading it there would stop a
            // fresh session at a permission prompt before it began (WI-912).
            // `--add-dir` is variadic: the `=` form takes one value, so the
            // positional prompt after it stays a prompt. A wake passes the
            // directories recorded at the first start (`req.add_dirs`) because
            // it has no new brief to derive them from (WI-1250).
            let mut dirs: Vec<&Path> = req.add_dirs.iter().map(PathBuf::as_path).collect();
            if dirs.is_empty() {
                if let Some(dir) = req.brief_file.and_then(Path::parent) {
                    dirs.push(dir);
                }
            }
            // The flags live in `vogt_session_agent` (WI-3881): the pod's own
            // launch line builds them with the same function, so a model,
            // effort or posture cannot reach one launch and silently miss the
            // other. Only the values differ, not the order or the spelling.
            let session_id = req.session_id.map(|id| id.to_string());
            let conversation = match (resume, bare, session_id.as_deref()) {
                (Some(id), _, _) => Some((id, true)),
                (None, true, Some(id)) => Some((id, false)),
                _ => None,
            };
            let add_dirs: Vec<String> = dirs.iter().map(|dir| dir.display().to_string()).collect();
            rewritten.extend(
                vogt_session_agent::claude_args(&vogt_session_agent::ClaudeArgs {
                    model,
                    effort,
                    conversation,
                    permission_mode: req.permission_mode,
                    settings: req.settings_file,
                    mcp_config: req.mcp.and_then(|m| m.claude_config.as_deref()),
                    add_dirs: &add_dirs,
                    prompt: prompt.as_deref(),
                })
                .map_err(ApiError::BadRequest)?,
            );
            env.extend(
                CLAUDE_QUIET_ENV
                    .iter()
                    .map(|(k, v)| ((*k).to_string(), (*v).to_string())),
            );
        }
        "codex" => {
            // The flags live in `vogt_session_agent` (WI-3882): the pod's own
            // launch line builds them with the same function, so guidance,
            // providers, the approval-mode overrides and the compact limit
            // cannot reach one launch and silently miss the other.
            let session_id = req
                .session_id
                .filter(|_| req.codex_session_id)
                .map(|id| id.to_string());
            let pin = session_id.as_deref().filter(|_| resume.is_none() && bare);
            let trust_cwd = codex
                .trust_cwd
                .map(|path| path.to_string_lossy().into_owned());
            let args = vogt_session_agent::codex_args(&vogt_session_agent::CodexArgs {
                command_args: &command[binary_idx + 1..],
                resume,
                session_id: pin,
                model,
                config_model: codex.config_model,
                effort,
                permission_mode: req.permission_mode,
                approval_policy: req.codex_approval_policy,
                exec: codex_exec,
                compact_limit: req.codex_compact_limit,
                provider: codex.provider,
                providers: codex.providers,
                catalog: codex.catalog,
                trust_cwd: trust_cwd.as_deref(),
                guidance: codex.guidance,
                charter: codex.charter,
                mcp_servers: codex.mcp_servers,
                mcp_args: req.mcp.map(|m| m.codex_args.as_slice()).unwrap_or(&[]),
                prompt: prompt.as_deref(),
            })
            .map_err(ApiError::BadRequest)?;
            rewritten.truncate(binary_idx + 1);
            rewritten.extend(args);
        }
        "opencode" => {
            if let Some(model) = model {
                rewritten.extend(["--model".to_string(), model.to_string()]);
            }
            if let Some(id) = resume {
                rewritten.extend(["--session".to_string(), id.to_string()]);
            }
            // OpenCode's positional is a project directory, not a prompt.
            if let Some(prompt) = prompt {
                rewritten.extend(["--prompt".to_string(), prompt]);
            }
            // The posture (WI-932), per session and layered over the user's
            // own config rather than written into it: an unattended session
            // must not stall at "Access external directory".
            if let Some(config) = opencode_posture(req.permission_mode, req.opencode_config) {
                env.push(("OPENCODE_CONFIG_CONTENT".to_string(), config));
            }
        }
        _ => unreachable!("checked against KNOWN above"),
    }
    let command = (rewritten.len() != command.len()).then_some(rewritten);
    Ok(Launch { command, env })
}

/// The agent CLI a command runs (`claude`, `codex`, `opencode`), when it is
/// one this engine knows, wrapped or not.
pub fn agent_name(command: &[String]) -> Option<String> {
    if command.is_empty() {
        return None;
    }
    let (_, binary) = agent_binary(command);
    KNOWN.contains(&binary.as_str()).then_some(binary)
}

/// The arguments a Codex command carries after its binary, when it is one.
/// The pod path needs them because the session agent rebuilds the launch line
/// instead of running the template's argv (WI-3882).
pub fn codex_args_of(command: &[String]) -> Option<Vec<String>> {
    if agent_name(command).as_deref() != Some("codex") {
        return None;
    }
    let (binary_idx, _) = agent_binary(command);
    Some(command[binary_idx + 1..].to_vec())
}

/// The agent conversation a session started with `command` runs, when its
/// id is known before the CLI says anything: the one it resumes, or — for a
/// bare launch [`launch`] pins with `--session-id` — the engine's own id for
/// the session. Claude Code is always pinned that way. Codex is
/// pinned when `codex_session_id` is set (the WI-1126 fork the image bakes, on
/// by default); stock Codex (`codex_session_id` false) and OpenCode mint their
/// own id, and guessing one could continue somebody else's conversation.
pub fn conversation(
    command: Option<&[String]>,
    resume: Option<&str>,
    session_id: Uuid,
    codex_session_id: bool,
) -> Option<vogt_engine_contract::AgentConversation> {
    let command = command.filter(|c| !c.is_empty())?;
    let (binary_idx, binary) = agent_binary(command);
    if !KNOWN.contains(&binary.as_str()) {
        return None;
    }
    if let Some(id) = trimmed(resume) {
        // A malformed id is refused by `launch` before anything spawns;
        // here it is simply not a conversation the engine can name.
        return is_conversation_id(id).then(|| vogt_engine_contract::AgentConversation {
            agent: binary,
            id: id.to_string(),
        });
    }
    let pinned = binary == "claude" || (binary.as_str() == "codex" && codex_session_id);
    (pinned && binary_idx + 1 == command.len()).then(|| vogt_engine_contract::AgentConversation {
        agent: binary,
        id: session_id.to_string(),
    })
}

/// Whether `value` is a well-formed conversation id (see
/// [`validate_conversation_id`]), for callers that use one as a file name.
pub fn is_conversation_id(value: &str) -> bool {
    validate_conversation_id(value).is_ok()
}

fn trimmed(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|v| !v.is_empty())
}

/// The agent CLI inside a possibly-wrapped command, and where it sits.
///
/// `vogt-agent-auth run -- claude` is the shape every protected template
/// uses, so the binary that cares about `--model` is the one after the `--`.
/// Falls back to the first word for a bare `["claude"]`, which is what
/// vogt-core sends when a caller names a template.
fn agent_binary(command: &[String]) -> (usize, String) {
    let idx = match command.iter().position(|arg| arg == "--") {
        Some(idx) if idx + 1 < command.len() => idx + 1,
        _ => 0,
    };
    let candidate = &command[idx];
    let name = candidate
        .rsplit('/')
        .next()
        .unwrap_or(candidate)
        .to_string();
    (idx, name)
}

/// A conversation id becomes an argument to the agent CLI, so it is held to a
/// narrower vocabulary than a model id: Claude Code and Codex ids are UUIDs,
/// OpenCode's are `ses_…`, and a Codex session *name* is a plain word. No `/`
/// or `:` (nothing that reads as a path or a URL), and never a leading dash,
/// which would reach the CLI as a flag.
fn validate_conversation_id(value: &str) -> Result<()> {
    if value.len() > MAX_VALUE_LEN {
        return Err(ApiError::BadRequest(format!(
            "resume is longer than {MAX_VALUE_LEN} characters"
        )));
    }
    if value.starts_with('-') {
        return Err(ApiError::BadRequest(format!(
            "resume {value:?} starts with a dash, which would reach the agent \
             CLI as another flag rather than as a conversation id"
        )));
    }
    if !value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        return Err(ApiError::BadRequest(format!(
            "resume {value:?} is not a conversation id: letters, digits and . _ - only"
        )));
    }
    Ok(())
}

pub(crate) fn validate(field: &str, value: &str) -> Result<()> {
    if value.len() > MAX_VALUE_LEN {
        return Err(ApiError::BadRequest(format!(
            "{field} is longer than {MAX_VALUE_LEN} characters"
        )));
    }
    if value.starts_with('-') {
        return Err(ApiError::BadRequest(format!(
            "{field} {value:?} starts with a dash, which would reach the agent \
             CLI as another flag rather than as a value"
        )));
    }
    if !value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/' | ':'))
    {
        return Err(ApiError::BadRequest(format!(
            "{field} {value:?} is not a model id: letters, digits and . _ - / : \
             only"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {

    #[test]
    fn shared_codex_instructions_preserve_guidance_charter_and_caps() {
        let value = vogt_session_agent::codex_developer_instructions(
            Some("WS \"quoted\""),
            Some("## Charters\n\nNo brand names."),
        )
        .unwrap();
        let body = value.strip_prefix("developer_instructions=").unwrap();
        let parsed: toml::Table = toml::from_str(&format!("v = {body}")).unwrap();
        let body = parsed["v"].as_str().unwrap();
        assert!(body.contains("WS \"quoted\""));
        assert!(body.ends_with("## Charters\n\nNo brand names."));
        assert!(body.find("WS").unwrap() < body.find("No brand names.").unwrap());

        let pointer =
            vogt_session_agent::codex_developer_instructions(None, Some(&"x".repeat(4 * 1024 + 1)))
                .unwrap();
        assert!(pointer.contains("too long to repeat here"));
        assert!(vogt_session_agent::codex_developer_instructions(
            Some(&"y".repeat(120 * 1024)),
            Some("charter"),
        )
        .is_some());
    }

    #[test]
    fn shared_codex_trust_and_provider_overrides_are_secret_free() {
        let provider = vogt_session_agent::CodexProvider {
            name: "ds".into(),
            base_url: "http://l/v1".into(),
            wire_api: "chat".into(),
            env_key: "DS_KEY".into(),
            context_window: None,
            auto_compact_token_limit: None,
        };
        let args = vogt_session_agent::codex_args(&vogt_session_agent::CodexArgs {
            command_args: &[],
            resume: None,
            session_id: None,
            model: None,
            config_model: None,
            effort: None,
            permission_mode: None,
            approval_policy: "never",
            exec: false,
            compact_limit: None,
            provider: None,
            providers: &[provider],
            catalog: None,
            trust_cwd: Some("/work"),
            guidance: None,
            charter: None,
            mcp_servers: &[],
            mcp_args: &[],
            prompt: None,
        })
        .unwrap();
        assert!(args
            .iter()
            .any(|arg| arg == r#"projects={"/work"={trust_level="trusted"}}"#));
        let provider_arg = args
            .iter()
            .find(|arg| arg.starts_with("model_providers.ds="))
            .unwrap();
        assert!(provider_arg.contains("DS_KEY"));
        assert!(!provider_arg.contains("secret"));
    }

    use super::*;

    fn cmd(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn asking_for_nothing_changes_nothing() {
        assert!(apply(Some(&cmd(&["claude"])), None, None)
            .unwrap()
            .is_none());
        // Whitespace is not an ask either — a client that sent "" must not
        // turn a plain shell into a refusal.
        assert!(apply(Some(&cmd(&["bash"])), Some("  "), Some(""))
            .unwrap()
            .is_none());
    }

    #[test]
    fn claude_takes_model_and_effort_as_its_own_flags() {
        let out = apply(
            Some(&cmd(&["claude"])),
            Some("claude-opus-4-5"),
            Some("high"),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            out,
            cmd(&["claude", "--model", "claude-opus-4-5", "--effort", "high"])
        );
    }

    #[test]
    fn codex_takes_a_short_flag_and_a_config_override() {
        let out = apply(Some(&cmd(&["codex"])), Some("gpt-5.6"), Some("medium"))
            .unwrap()
            .unwrap();
        assert_eq!(
            out,
            cmd(&[
                "codex",
                "-c",
                "features.daemon_auto_start=false",
                "-m",
                "gpt-5.6",
                "-c",
                "model_reasoning_effort=medium",
                "-a",
                "never",
                "-s",
                "danger-full-access",
                "-c",
                "tui.terminal_title=[\"agent-state\"]",
                "-c",
                "tui.animations=false"
            ])
        );
    }

    #[test]
    fn the_broker_wrapper_does_not_hide_the_agent_from_us() {
        // Every protected template is `vogt-agent-auth run -- <cli>`. If
        // the first word decided, every one of them would be refused as an
        // unknown CLI and the templates people actually use would be the ones
        // that could not be given a model.
        let out = apply(
            Some(&cmd(&["vogt-agent-auth", "run", "--", "claude"])),
            Some("claude-sonnet-4-5"),
            None,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            out,
            cmd(&[
                "vogt-agent-auth",
                "run",
                "--",
                "claude",
                "--model",
                "claude-sonnet-4-5"
            ])
        );
    }

    #[test]
    fn an_absolute_path_to_a_known_cli_is_still_that_cli() {
        let out = apply(Some(&cmd(&["/usr/local/bin/codex"])), Some("gpt-5.6"), None)
            .unwrap()
            .unwrap();
        assert_eq!(
            out,
            cmd(&[
                "/usr/local/bin/codex",
                "-c",
                "features.daemon_auto_start=false",
                "-m",
                "gpt-5.6",
                "-a",
                "never",
                "-s",
                "danger-full-access",
                "-c",
                "tui.terminal_title=[\"agent-state\"]",
                "-c",
                "tui.animations=false"
            ])
        );
    }

    #[test]
    fn a_command_with_no_mapping_is_refused_and_named() {
        // The failure this is against has no symptom: the session spawns, the
        // agent answers, and it is not the model that was asked for.
        let err = apply(Some(&cmd(&["bash"])), Some("gpt-5.6"), None)
            .expect_err("bash cannot be told which model to use");
        let message = format!("{err:?}");
        assert!(message.contains("bash"), "{message}");
        assert!(message.contains("claude"), "{message}");
    }

    #[test]
    fn a_plain_shell_session_says_why_it_cannot_take_a_model() {
        let err = apply(None, Some("gpt-5.6"), None).expect_err("no command, no model");
        assert!(format!("{err:?}").contains("default shell"));
    }

    #[test]
    fn opencode_refuses_the_half_it_cannot_do() {
        assert_eq!(
            apply(
                Some(&cmd(&["opencode"])),
                Some("anthropic/claude-sonnet-4-5"),
                None
            )
            .unwrap()
            .unwrap(),
            cmd(&["opencode", "--model", "anthropic/claude-sonnet-4-5"])
        );
        let err = apply(Some(&cmd(&["opencode"])), Some("x/y"), Some("high"))
            .expect_err("opencode has no effort control");
        assert!(format!("{err:?}").contains("effort"));
    }

    #[test]
    fn hostile_values_are_refused_rather_than_escaped() {
        // These strings arrive from a model's tool call and end up in argv.
        // `--dangerously-skip-permissions` is the one that matters: a "model
        // id" that is really a second flag turns a session into a different
        // session, and the approval card the user read said "model".
        for hostile in [
            "--dangerously-skip-permissions",
            "gpt-5.6 --model other",
            "gpt-5.6;rm -rf /",
            "gpt\n--model",
            "-m",
        ] {
            assert!(
                apply(Some(&cmd(&["claude"])), Some(hostile), None).is_err(),
                "{hostile:?} reached argv"
            );
        }
        assert!(apply(Some(&cmd(&["claude"])), Some(&"x".repeat(200)), None).is_err());
    }

    #[test]
    fn ordinary_model_ids_survive() {
        for good in [
            "gpt-5.6",
            "claude-opus-4-5",
            "qwen/qwen3-coder",
            "openai:gpt-5.6",
            "gpt_4o_mini",
        ] {
            assert!(
                apply(Some(&cmd(&["claude"])), Some(good), None).is_ok(),
                "{good:?} is a real model id and was refused"
            );
        }
    }

    fn protected(cli: &str) -> Vec<String> {
        cmd(&["vogt-agent-auth", "run", "--", cli])
    }

    const BRIEF: &str = "/state/agent-task-prompts/sessions/x.md";

    fn with_brief() -> LaunchRequest<'static> {
        LaunchRequest {
            brief_file: Some(Path::new(BRIEF)),
            ..LaunchRequest::bare()
        }
    }

    #[test]
    fn a_brief_becomes_the_agents_first_prompt_not_its_argv() {
        // WI-827: the protected template ran a bare `claude`, so an agent
        // started with a task opened idle. The first prompt now points at the
        // file — never the brief's text, which stays out of `ps`.
        let out = launch(Some(&protected("claude")), &with_brief()).unwrap();
        let command = out.command.expect("rewritten");
        assert_eq!(&command[..4], &protected("claude")[..]);
        let prompt = command.last().unwrap();
        assert!(prompt.contains(BRIEF), "{prompt}");
        assert!(prompt.contains("Task"), "{prompt}");
        assert!(!prompt.starts_with('-'));

        let codex = launch(Some(&protected("codex")), &with_brief())
            .unwrap()
            .command
            .unwrap();
        // The brief stays last. Ahead of it: the default posture (WI-1143),
        // the daemon override (WI-3882) and the two `-c` pairs that ask the
        // fork for its `agent-state` title (WI-1145).
        assert!(codex.last().unwrap().contains(BRIEF));
        assert!(codex.windows(2).any(|w| w == ["-a", "never"]));
        assert!(codex.windows(2).any(|w| w == ["-s", "danger-full-access"]));
        assert!(codex
            .windows(2)
            .any(|w| w == ["-c", "tui.terminal_title=[\"agent-state\"]"]));
        assert!(codex
            .windows(2)
            .any(|w| w == ["-c", "tui.animations=false"]));

        let opencode = launch(Some(&protected("opencode")), &with_brief())
            .unwrap()
            .command
            .unwrap();
        assert_eq!(opencode[4], "--prompt");
        assert!(opencode[5].contains(BRIEF));
    }

    #[test]
    fn model_flags_stay_before_the_prompt() {
        let out = launch(
            Some(&protected("claude")),
            &LaunchRequest {
                model: Some("claude-opus-4-5"),
                effort: Some("high"),
                ..with_brief()
            },
        )
        .unwrap()
        .command
        .unwrap();
        assert_eq!(
            &out[4..8],
            &cmd(&["--model", "claude-opus-4-5", "--effort", "high"])[..]
        );
        assert!(
            out[8].starts_with("--add-dir="),
            "the brief's directory, `=` form, so it takes one value: {out:?}"
        );
        assert!(out[9].contains(BRIEF));
        assert_eq!(out.len(), 10);
    }

    #[test]
    fn a_shell_with_a_brief_is_left_alone() {
        // A plain shell has no first prompt to take: it keeps the variable it
        // always had and nothing else, rather than being refused.
        assert_eq!(
            launch(Some(&cmd(&["bash"])), &with_brief()).unwrap(),
            Launch::default()
        );
        assert_eq!(launch(None, &with_brief()).unwrap(), Launch::default());
    }

    #[test]
    fn resume_maps_to_each_clis_own_form() {
        let id = "0f8fad5b-d9cb-469f-a165-70867728950e";
        let req = LaunchRequest {
            resume: Some(id),
            model: Some("m1"),
            ..LaunchRequest::bare()
        };
        assert_eq!(
            launch(Some(&protected("claude")), &req)
                .unwrap()
                .command
                .unwrap(),
            [protected("claude"), cmd(&["--model", "m1", "--resume", id])].concat()
        );
        // `resume` is a codex subcommand, so it sits straight after the
        // binary, ahead of the flags, and the title overrides follow.
        assert_eq!(
            launch(Some(&protected("codex")), &req)
                .unwrap()
                .command
                .unwrap(),
            [
                protected("codex"),
                cmd(&[
                    "resume",
                    id,
                    "-c",
                    "features.daemon_auto_start=false",
                    "-m",
                    "m1",
                    "-a",
                    "never",
                    "-s",
                    "danger-full-access",
                    "-c",
                    "tui.terminal_title=[\"agent-state\"]",
                    "-c",
                    "tui.animations=false"
                ])
            ]
            .concat()
        );
        assert_eq!(
            launch(Some(&cmd(&["codex", "--search"])), &req)
                .unwrap()
                .command
                .unwrap(),
            cmd(&[
                "codex",
                "resume",
                id,
                "-c",
                "features.daemon_auto_start=false",
                "--search",
                "-m",
                "m1",
                "-a",
                "never",
                "-s",
                "danger-full-access",
                "-c",
                "tui.terminal_title=[\"agent-state\"]",
                "-c",
                "tui.animations=false"
            ])
        );
        assert_eq!(
            launch(
                Some(&protected("opencode")),
                &LaunchRequest {
                    resume: Some("ses_abc123"),
                    ..LaunchRequest::bare()
                }
            )
            .unwrap()
            .command
            .unwrap(),
            [protected("opencode"), cmd(&["--session", "ses_abc123"])].concat()
        );
    }

    #[test]
    fn resume_on_a_shell_is_refused() {
        let req = LaunchRequest {
            resume: Some("abc"),
            ..LaunchRequest::bare()
        };
        assert!(launch(Some(&cmd(&["bash"])), &req).is_err());
        assert!(launch(None, &req).is_err());
    }

    #[test]
    fn hostile_conversation_ids_are_refused() {
        for hostile in [
            "--dangerously-skip-permissions",
            "-r",
            "abc def",
            "abc;rm -rf /",
            "../../etc/passwd",
            "http://x",
            "abc\n--model",
        ] {
            let req = LaunchRequest {
                resume: Some(hostile),
                ..LaunchRequest::bare()
            };
            assert!(
                launch(Some(&protected("claude")), &req).is_err(),
                "{hostile:?} reached argv"
            );
        }
    }

    #[test]
    fn a_backed_off_codex_launch_carries_the_compact_limit() {
        // WI-2832: a session that thrashed on compaction restarts with a limit
        // above the size it came back to, in total scope.
        let req = LaunchRequest {
            codex_compact_limit: Some(152_000),
            ..LaunchRequest::bare()
        };
        let out = launch(Some(&protected("codex")), &req)
            .unwrap()
            .command
            .unwrap();
        let at = out
            .iter()
            .position(|a| a == "model_auto_compact_token_limit=152000")
            .expect("limit override present");
        assert_eq!(out[at - 1], "-c");
        assert_eq!(out[at + 1], "-c");
        assert_eq!(
            out[at + 2],
            "model_auto_compact_token_limit_scope=\"total\""
        );
        // Not asked for: the configured limit stands, nothing is added.
        let plain = launch(Some(&protected("codex")), &LaunchRequest::bare())
            .unwrap()
            .command
            .unwrap();
        assert!(!plain.iter().any(|a| a.contains("auto_compact")));
        // Never for another agent.
        let claude = launch(Some(&protected("claude")), &req).unwrap().command;
        assert!(!claude.iter().flatten().any(|a| a.contains("auto_compact")));
    }

    #[test]
    fn a_fresh_bare_claude_launch_pins_its_conversation_id() {
        // WI-833: the conversation is then resumable by the engine session
        // id every listing already shows.
        let sid = Uuid::new_v4();
        let req = LaunchRequest {
            session_id: Some(sid),
            ..LaunchRequest::bare()
        };
        assert_eq!(
            launch(Some(&protected("claude")), &req)
                .unwrap()
                .command
                .unwrap(),
            [
                protected("claude"),
                cmd(&["--session-id", &sid.to_string()])
            ]
            .concat()
        );
        // Not when the template carries arguments of its own (it may already
        // say `--continue`), and not when resuming, which keeps the old id.
        assert!(launch(Some(&cmd(&["claude", "--continue"])), &req)
            .unwrap()
            .command
            .is_none());
        let resumed = launch(
            Some(&protected("claude")),
            &LaunchRequest {
                resume: Some("abc"),
                ..req
            },
        )
        .unwrap()
        .command
        .unwrap();
        assert!(!resumed.contains(&"--session-id".to_string()));
        // Stock Codex refuses `--session-id`, so that flag is added only when
        // the deployment says its binary is the fork (WI-1126 / WI-1144).
        // The image bakes that fork, so the pin is on by default (WI-1354):
        // a fresh bare Codex launch gets the engine id without the caller
        // having to set the flag, and hibernation then has a conversation to
        // resume. The `agent-state` title overrides are added either way
        // (WI-1145): stock Codex accepts `-c` and ignores a `tui` key it does
        // not know.
        let pinned = launch(Some(&protected("codex")), &req)
            .unwrap()
            .command
            .unwrap();
        assert_eq!(
            pinned,
            [
                protected("codex"),
                cmd(&[
                    "-c",
                    "features.daemon_auto_start=false",
                    "-a",
                    "never",
                    "-s",
                    "danger-full-access",
                    "-c",
                    "tui.terminal_title=[\"agent-state\"]",
                    "-c",
                    "tui.animations=false",
                    "--session-id",
                    &sid.to_string()
                ])
            ]
            .concat()
        );
        // An explicit opt-out (stock npm Codex, which refuses the flag) leaves
        // the launch unpinned. The title overrides stay.
        let stock = LaunchRequest {
            codex_session_id: false,
            ..req
        };
        let titled = launch(Some(&protected("codex")), &stock)
            .unwrap()
            .command
            .unwrap();
        assert!(!titled.iter().any(|arg| arg == "--session-id"));
        let forked = LaunchRequest {
            codex_session_id: true,
            ..req
        };
        assert_eq!(
            launch(Some(&protected("codex")), &forked)
                .unwrap()
                .command
                .unwrap(),
            [
                protected("codex"),
                cmd(&[
                    "-c",
                    "features.daemon_auto_start=false",
                    "-a",
                    "never",
                    "-s",
                    "danger-full-access",
                    "-c",
                    "tui.terminal_title=[\"agent-state\"]",
                    "-c",
                    "tui.animations=false",
                    "--session-id",
                    &sid.to_string()
                ])
            ]
            .concat()
        );
        // `--session-id` is not added when the template carries arguments of
        // its own, nor when resuming: `codex resume <id>` already names the
        // conversation. The title overrides still are.
        let searching = launch(Some(&cmd(&["codex", "--search"])), &forked)
            .unwrap()
            .command
            .unwrap();
        assert!(!searching.iter().any(|arg| arg == "--session-id"));
        assert!(searching
            .windows(2)
            .any(|w| w == ["-c", "tui.animations=false"]));
        let resumed = launch(
            Some(&protected("codex")),
            &LaunchRequest {
                resume: Some("abc"),
                ..forked
            },
        )
        .unwrap()
        .command
        .unwrap();
        assert!(!resumed.iter().any(|arg| arg == "--session-id"));
        assert!(resumed.windows(2).any(|w| w == ["resume", "abc"]));
        assert!(resumed
            .windows(2)
            .any(|w| w == ["-c", "tui.terminal_title=[\"agent-state\"]"]));
    }

    #[test]
    fn claude_sessions_get_the_quiet_defaults_and_others_do_not() {
        // WI-832: prompt suggestions are accepted by a bare Enter, which a
        // driving agent sends.
        let env = launch(Some(&protected("claude")), &LaunchRequest::bare())
            .unwrap()
            .env;
        assert!(env.contains(&(
            "CLAUDE_CODE_ENABLE_PROMPT_SUGGESTION".to_string(),
            "false".to_string()
        )));
        assert!(env.contains(&(
            "CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY".to_string(),
            "1".to_string()
        )));
        assert!(launch(Some(&protected("codex")), &LaunchRequest::bare())
            .unwrap()
            .env
            .is_empty());
        assert!(launch(Some(&cmd(&["bash"])), &LaunchRequest::bare())
            .unwrap()
            .env
            .is_empty());
    }
}

#[cfg(test)]
mod opencode_posture_tests {
    use super::*;

    fn env_of(mode: Option<&str>, policy: Option<&str>) -> Option<String> {
        let command = vec![
            "vogt-agent-auth".into(),
            "run".into(),
            "--".into(),
            "opencode".into(),
        ];
        launch(
            Some(&command),
            &LaunchRequest {
                permission_mode: mode,
                opencode_config: policy,
                ..LaunchRequest::bare()
            },
        )
        .unwrap()
        .env
        .into_iter()
        .find(|(k, _)| k == "OPENCODE_CONFIG_CONTENT")
        .map(|(_, v)| v)
    }

    #[test]
    fn a_driven_opencode_session_gets_its_posture_per_session() {
        let policy = r#"{"permission":{"bash":{"*":"allow","git push --force*":"deny"}}}"#;
        assert_eq!(env_of(None, Some(policy)).as_deref(), Some(policy));
        assert_eq!(
            env_of(Some("default"), Some(policy)).as_deref(),
            Some(policy)
        );
        assert_eq!(env_of(None, None), None, "no policy, nothing added");
        assert_eq!(
            env_of(Some("bypass"), Some(policy)).as_deref(),
            Some(OPENCODE_BYPASS)
        );
        assert_eq!(
            env_of(Some("accept_edits"), None).as_deref(),
            Some(OPENCODE_ACCEPT_EDITS)
        );
    }

    #[test]
    fn codex_bypass_is_its_own_flag_and_not_claudes_settings() {
        let command = vec!["codex".to_string()];
        let got = launch(
            Some(&command),
            &LaunchRequest {
                permission_mode: Some("bypass"),
                ..LaunchRequest::bare()
            },
        )
        .expect("codex takes a posture");
        assert_eq!(
            got.command.unwrap(),
            [
                "codex",
                "-c",
                "features.daemon_auto_start=false",
                "--dangerously-bypass-approvals-and-sandbox",
                "-c",
                "tui.terminal_title=[\"agent-state\"]",
                "-c",
                "tui.animations=false",
            ]
        );
        assert!(got.env.is_empty(), "codex is not given claude settings");
    }
}

#[cfg(test)]
mod conversation_tests {
    use super::conversation;
    use uuid::Uuid;

    fn argv(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    #[test]
    fn a_bare_claude_runs_the_engines_id_and_a_resume_runs_its_own() {
        let id = Uuid::new_v4();
        let wrapped = argv(&["vogt-agent-auth", "run", "--", "claude"]);
        let found = conversation(Some(&wrapped), None, id, false).unwrap();
        assert_eq!((found.agent.as_str(), found.id), ("claude", id.to_string()));

        let resumed = conversation(Some(&wrapped), Some("abc-123"), id, false).unwrap();
        assert_eq!(resumed.id, "abc-123");
        let codex = conversation(Some(&argv(&["codex"])), Some("r1"), id, false).unwrap();
        assert_eq!((codex.agent.as_str(), codex.id.as_str()), ("codex", "r1"));
    }

    #[test]
    fn an_unknown_id_is_none_not_a_guess() {
        let id = Uuid::new_v4();
        // The fork (codex_session_id, on by default) is pinned to the engine
        // id when the command is bare, and not when it carries arguments of
        // its own. Stock Codex (the flag explicitly off) mints its own id. A
        // claude with arguments of its own may carry `--continue` or a session
        // id the engine did not pin.
        assert!(conversation(Some(&argv(&["codex"])), None, id, false).is_none());
        let forked = conversation(Some(&argv(&["codex"])), None, id, true).unwrap();
        assert_eq!(
            (forked.agent.as_str(), forked.id.as_str()),
            ("codex", id.to_string().as_str())
        );
        assert!(conversation(Some(&argv(&["codex", "--search"])), None, id, true).is_none());
        assert!(conversation(Some(&argv(&["claude", "--continue"])), None, id, false).is_none());
        assert!(conversation(Some(&argv(&["bash"])), Some("x"), id, false).is_none());
        assert!(conversation(None, None, id, false).is_none());
        assert!(conversation(Some(&argv(&["claude"])), Some("-bad"), id, false).is_none());
    }
}

#[cfg(test)]
mod permission_tests {
    use super::*;

    fn cmd(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn postures_map_to_claudes_flags_and_default_adds_none() {
        // Claude's own mapping is tested beside the flags it builds
        // (`vogt_session_agent::claude_permission_flags`, WI-3881); this
        // wrapper only maps its error.
        assert!(permission_flags(Some("yolo")).is_err());
        assert!(codex_permission_flags(Some("yolo"), false, "never").is_err());
        assert_eq!(codex_permission_flags(None, false, "never").unwrap(), None);
        assert_eq!(
            codex_permission_flags(Some("default"), false, "never").unwrap(),
            Some(cmd(&["-a", "never", "-s", "danger-full-access"]))
        );
        // A deployment that restores asking gets `on-request`, and
        // accept-edits pins it too so the asks the reviewer is meant to
        // answer cannot be dropped.
        assert_eq!(
            codex_permission_flags(Some("default"), false, "on-request").unwrap(),
            Some(cmd(&["-a", "on-request", "-s", "danger-full-access"]))
        );
        assert_eq!(
            codex_permission_flags(Some("accept_edits"), false, "on-request").unwrap(),
            Some(cmd(&[
                "-a",
                "on-request",
                "-c",
                "approvals_reviewer=\"auto_review\"",
                "-s",
                "danger-full-access"
            ]))
        );
        assert_eq!(
            codex_permission_flags(Some("accept_edits"), false, "never").unwrap(),
            Some(cmd(&[
                "-a",
                "never",
                "-c",
                "approvals_reviewer=\"auto_review\"",
                "-s",
                "danger-full-access"
            ]))
        );
        assert_eq!(
            codex_permission_flags(Some("bypass"), false, "never").unwrap(),
            Some(cmd(&["--dangerously-bypass-approvals-and-sandbox"]))
        );
        // `codex exec` rejects `-a`. The sandbox and the reviewer stay.
        assert_eq!(
            codex_permission_flags(Some("default"), true, "on-request").unwrap(),
            Some(cmd(&["-s", "danger-full-access"]))
        );
        assert_eq!(
            codex_permission_flags(Some("accept-edits"), true, "on-request").unwrap(),
            Some(cmd(&[
                "-c",
                "approvals_reviewer=\"auto_review\"",
                "-s",
                "danger-full-access"
            ]))
        );
    }

    #[test]
    fn the_deployment_chooses_codex_approvals_and_never_is_the_default() {
        assert_eq!(parse_codex_approval_policy(None).unwrap(), "never");
        assert_eq!(parse_codex_approval_policy(Some("  ")).unwrap(), "never");
        for policy in CODEX_APPROVAL_POLICIES {
            assert_eq!(parse_codex_approval_policy(Some(policy)).unwrap(), policy);
        }
        assert_eq!(
            parse_codex_approval_policy(Some(" on-request ")).unwrap(),
            "on-request"
        );
        assert!(parse_codex_approval_policy(Some("yes-please")).is_err());
        assert!(parse_codex_approval_policy(Some("auto")).is_err());
    }

    #[test]
    fn the_policy_rides_every_claude_launch_and_a_posture_only_claude() {
        let policy = Path::new("/usr/local/share/vogt/driven-session-settings.json");
        let out = launch(
            Some(&cmd(&["vogt-agent-auth", "run", "--", "claude"])),
            &LaunchRequest {
                permission_mode: Some("bypass"),
                settings_file: Some(policy),
                ..LaunchRequest::bare()
            },
        )
        .unwrap()
        .command
        .unwrap();
        assert!(
            out.contains(&"--dangerously-skip-permissions".to_string()),
            "{out:?}"
        );
        assert!(
            out.contains(&format!("--settings={}", policy.display())),
            "{out:?}"
        );
        // A plain shell is refused a posture, never started plain. Codex is
        // told its own flags (WI-1143) and is not handed Claude's settings file.
        let refused = launch(
            Some(&cmd(&["bash"])),
            &LaunchRequest {
                permission_mode: Some("bypass"),
                ..LaunchRequest::bare()
            },
        );
        assert!(refused.is_err(), "bash");
        let codex = launch(
            Some(&cmd(&["codex"])),
            &LaunchRequest {
                permission_mode: Some("bypass"),
                settings_file: Some(policy),
                ..LaunchRequest::bare()
            },
        )
        .unwrap()
        .command
        .unwrap();
        assert!(codex.contains(&"--dangerously-bypass-approvals-and-sandbox".to_string()));
        assert!(!codex
            .iter()
            .any(|arg| arg.contains("driven-session-settings")));
        assert!(!codex.iter().any(|arg| arg == "--approve-for-me"));
        let edits = launch(
            Some(&cmd(&["codex"])),
            &LaunchRequest {
                permission_mode: Some("accept-edits"),
                ..LaunchRequest::bare()
            },
        )
        .unwrap()
        .command
        .unwrap();
        assert!(edits.windows(2).any(|w| w == ["-a", "never"]));
        assert!(edits
            .windows(2)
            .any(|w| w == ["-c", "approvals_reviewer=\"auto_review\""]));
        assert!(edits.windows(2).any(|w| w == ["-s", "danger-full-access"]));
        // A policy file alone is not an ask of the caller, but a Codex launch
        // that names no posture still starts on the default one: the wrapper
        // no longer forces bypass, so omitting the flags would look like it had.
        assert_eq!(
            launch(
                Some(&cmd(&["codex"])),
                &LaunchRequest {
                    settings_file: Some(policy),
                    ..LaunchRequest::bare()
                }
            )
            .unwrap()
            .command
            .unwrap(),
            cmd(&[
                "codex",
                "-c",
                "features.daemon_auto_start=false",
                "-a",
                "never",
                "-s",
                "danger-full-access",
                "-c",
                "tui.terminal_title=[\"agent-state\"]",
                "-c",
                "tui.animations=false"
            ])
        );
        // `codex exec` rejects `-a`, so neither a default launch nor an
        // accept-edits launch emits it. The sandbox and the reviewer stay.
        let exec = launch(
            Some(&cmd(&["codex", "exec"])),
            &LaunchRequest {
                permission_mode: Some("accept-edits"),
                ..LaunchRequest::bare()
            },
        )
        .unwrap()
        .command
        .unwrap();
        assert!(!exec.iter().any(|arg| arg == "-a"), "{exec:?}");
        assert!(exec
            .windows(2)
            .any(|w| w == ["-c", "approvals_reviewer=\"auto_review\""]));
        assert!(exec.windows(2).any(|w| w == ["-s", "danger-full-access"]));
        assert_eq!(
            launch(Some(&cmd(&["codex", "exec"])), &LaunchRequest::bare())
                .unwrap()
                .command
                .unwrap(),
            cmd(&[
                "codex",
                "-c",
                "features.daemon_auto_start=false",
                "exec",
                "-s",
                "danger-full-access",
                "-c",
                "tui.terminal_title=[\"agent-state\"]",
                "-c",
                "tui.animations=false"
            ])
        );
    }

    #[test]
    fn a_worker_mcp_set_reaches_claude_and_codex_and_nothing_else() {
        let cmd = |parts: &[&str]| -> Vec<String> { parts.iter().map(|s| s.to_string()).collect() };
        let trim = crate::mcp_set::McpTrim {
            claude_config: Some(PathBuf::from("/state/s.mcp.json")),
            codex_args: vec!["-c".into(), "mcp_servers.komodo.enabled=false".into()],
            ..Default::default()
        };
        let req = LaunchRequest {
            mcp: Some(&trim),
            ..LaunchRequest::bare()
        };
        let claude = launch(Some(&cmd(&["claude"])), &req)
            .unwrap()
            .command
            .unwrap();
        assert!(claude.contains(&"--strict-mcp-config".to_string()));
        assert!(claude.contains(&"--mcp-config=/state/s.mcp.json".to_string()));
        assert!(!claude.iter().any(|a| a.contains("komodo")));
        let codex = launch(Some(&cmd(&["codex"])), &req)
            .unwrap()
            .command
            .unwrap();
        assert!(codex.contains(&"mcp_servers.komodo.enabled=false".to_string()));
        assert!(!codex.iter().any(|a| a.contains("mcp-config")));
        let opencode = launch(Some(&cmd(&["opencode"])), &req).unwrap().command;
        assert!(opencode.is_none(), "opencode is left as it was");
    }

    /// WI-3881: the host launcher and the pod's own launch line build Claude's
    /// argv from the same function. The model, effort, posture, settings,
    /// conversation, add-dir and first prompt must arrive as the same words in
    /// the same order, so a pod session cannot silently lose a launch input a
    /// host session would have received.
    #[test]
    #[cfg(unix)]
    fn host_and_pod_claude_argv_agree() {
        let dir = tempfile::tempdir().unwrap();
        let settings = dir.path().join("settings.json");
        let brief = dir.path().join("brief.md");
        std::fs::write(&settings, r#"{"permissions":{}}"#).unwrap();
        std::fs::write(&brief, "# task\n").unwrap();
        let id = Uuid::new_v4();
        let cwd = "/work";

        let host = launch(
            Some(&cmd(&["claude"])),
            &LaunchRequest {
                model: Some("claude-opus-4-6"),
                effort: Some("high"),
                resume: None,
                brief_file: Some(&brief),
                session_id: Some(id),
                codex_session_id: true,
                permission_mode: Some("accept-edits"),
                settings_file: Some(&settings),
                add_dirs: &[PathBuf::from(cwd)],
                opencode_config: None,
                codex_approval_policy: "never",
                mcp: None,
                codex_compact_limit: None,
            },
        )
        .unwrap()
        .command
        .expect("a rewritten command");
        assert_eq!(host[0], "claude");

        let pod_config = vogt_session_agent::Config {
            session_id: "s".into(),
            engine_url: "http://127.0.0.1:1".into(),
            token: "t".into(),
            cli: "claude".into(),
            command: None,
            cwd: Some(cwd.into()),
            cols: 80,
            rows: 24,
            buffer_bytes: 1024,
            claude_config_dir: None,
            codex_home: None,
            account: None,
            conversation_id: Some(id.to_string()),
            resume: false,
            model: Some("claude-opus-4-6".into()),
            effort: Some("high".into()),
            permission_mode: Some("accept-edits".into()),
            codex_approval_policy: "never".into(),
            claude: vogt_session_agent::ClaudeLaunch {
                settings: Some(settings),
                mcp_config: None,
                brief: Some(brief),
            },
            codex: vogt_session_agent::CodexLaunch::default(),
        };
        let line = vogt_session_agent::command_line(&pod_config).unwrap();
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("claude() {{ printf '%s\\n' \"$@\"; }}; {line}"))
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        let pod: Vec<String> = String::from_utf8(out.stdout)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect();

        assert_eq!(&host[1..], pod.as_slice());
        for flag in ["--model", "--effort", "--permission-mode", "--settings"] {
            assert!(
                pod.iter()
                    .any(|arg| arg == flag || arg.starts_with(&format!("{flag}="))),
                "the pod argv lacks {flag}: {pod:?}"
            );
        }
    }

    /// WI-3882: the host launcher and the pod's own launch line build Codex's
    /// argv from the same builder. Guidance, charter, the provider table, the
    /// approval-mode and compact overrides, and the template's own arguments
    /// must be the same words in the same order on both, so a pod cannot
    /// silently lose an override a host launch would have received.
    #[test]
    #[cfg(unix)]
    fn host_and_pod_codex_argv_agree() {
        let dir = tempfile::tempdir().unwrap();
        let fragment = dir.path().join("vogt-codex.toml");
        std::fs::write(
            &fragment,
            r#"model_provider = "litellm"
model = "gpt-5.6"

[model_providers.litellm]
base_url = "http://100.92.54.45:4000"
wire_api = "responses"
env_key = "LITELLM_API_KEY"
context_window = 1000000
auto_compact_token_limit = 500000

[mcp_servers.vogt]
url = "http://vogt/mcp"

[mcp_servers.komodo]
url = "http://komodo/mcp"
"#,
        )
        .unwrap();
        let id = Uuid::new_v4();
        let conversation = id.to_string();
        let cwd = "/work/wi-3882";
        let guidance = "Follow AGENTS.md.";
        let charter = "## Charter\nStop at push, do not poll CI.";
        let template = cmd(&["-c", "tui.notifications=true"]);
        let providers = [vogt_session_agent::CodexProvider {
            name: "litellm".into(),
            base_url: "http://100.92.54.45:4000".into(),
            wire_api: "responses".into(),
            env_key: "LITELLM_API_KEY".into(),
            context_window: Some(1_000_000),
            auto_compact_token_limit: Some(500_000),
        }];
        let servers = ["vogt".to_string(), "komodo".to_string()];

        let host_command = [cmd(&["codex"]), template.clone()].concat();
        let host = launch_with_codex(
            Some(&host_command),
            &LaunchRequest {
                model: Some("gpt-5.6"),
                effort: Some("high"),
                resume: Some(&conversation),
                brief_file: None,
                session_id: Some(id),
                codex_session_id: true,
                permission_mode: Some("accept-edits"),
                settings_file: None,
                add_dirs: &[],
                opencode_config: None,
                codex_approval_policy: "never",
                mcp: None,
                codex_compact_limit: None,
            },
            &CodexExtras {
                guidance: Some(guidance),
                charter: Some(charter),
                providers: &providers,
                provider: Some("litellm"),
                catalog: None,
                config_model: None,
                trust_cwd: Some(Path::new(cwd)),
                mcp_servers: &servers,
            },
        )
        .unwrap()
        .command
        .expect("a rewritten command");
        let host = codex_args_of(&host).unwrap();

        let pod_config = vogt_session_agent::Config {
            session_id: "s".into(),
            engine_url: "http://127.0.0.1:1".into(),
            token: "t".into(),
            cli: "codex".into(),
            command: None,
            cwd: Some(cwd.into()),
            cols: 80,
            rows: 24,
            buffer_bytes: 1024,
            claude_config_dir: None,
            codex_home: Some(dir.path().to_path_buf()),
            account: None,
            conversation_id: Some(conversation),
            resume: true,
            model: Some("gpt-5.6".into()),
            effort: Some("high".into()),
            permission_mode: Some("accept-edits".into()),
            codex_approval_policy: "never".into(),
            claude: vogt_session_agent::ClaudeLaunch::default(),
            codex: vogt_session_agent::CodexLaunch {
                guidance: Some(guidance.into()),
                charter: Some(charter.into()),
                compact_limit: None,
                template_args: template,
                config_file: None,
            },
        };
        let line = vogt_session_agent::command_line(&pod_config).unwrap();
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("codex() {{ printf '%s\\n' \"$@\"; }}; {line}"))
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        let pod: Vec<String> = String::from_utf8(out.stdout)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect();
        // The shell function stands in for the binary, so `$@` is the argv
        // after it; `codex_args_of` already stripped the host's binary.
        assert_eq!(host, pod, "host and pod Codex argv differ");

        for flag in [
            "model_provider=\"litellm\"",
            "mcp_servers.vogt.default_tools_approval_mode=\"approve\"",
            "model_context_window=1000000",
            "model_auto_compact_token_limit=500000",
        ] {
            assert!(
                host.iter().any(|arg| arg == flag),
                "the shared argv lacks {flag}: {host:?}"
            );
        }
        assert!(host.iter().any(|arg| arg == "model_context_window=1000000"));
        assert!(host
            .iter()
            .any(|arg| arg == "model_auto_compact_token_limit=500000"));
        assert!(
            !host.iter().any(|arg| arg.contains("komodo")),
            "komodo is not in the auto-approval list: {host:?}"
        );
        assert!(host.windows(2).any(|w| w == ["-a", "never"]));
        assert!(host
            .windows(2)
            .any(|w| w == ["-c", "model_reasoning_effort=high"]));
        assert!(host
            .windows(2)
            .any(|w| w == ["-c", "approvals_reviewer=\"auto_review\""]));
        assert!(host
            .windows(2)
            .any(|w| w == ["-c", "features.daemon_auto_start=false"]));
        // One emission per key: a provider that states its limit gets exactly
        // that value, not the deployment's 200k default (WI-3871 review).
        assert_eq!(
            host.iter()
                .filter(|arg| arg.starts_with("model_auto_compact_token_limit="))
                .count(),
            1,
            "{host:?}"
        );
        assert!(!host
            .iter()
            .any(|arg| arg.starts_with("model_auto_compact_token_limit_scope")));
        let instructions = host
            .iter()
            .find_map(|arg| arg.strip_prefix("developer_instructions="))
            .expect("the guidance override");
        let parsed: toml::Table = toml::from_str(&format!("v = {instructions}")).unwrap();
        let body = parsed["v"].as_str().unwrap();
        assert!(body.contains(guidance), "{body}");
        assert!(body.ends_with("Stop at push, do not poll CI."), "{body}");
        assert!(!host.iter().any(|arg| arg.contains("CODEX_HOME")));
    }

    fn codex_host_and_pod_argv(
        dir: &tempfile::TempDir,
        fragment: &str,
        providers: &[vogt_session_agent::CodexProvider],
        selected: &str,
        model: &str,
        compact_limit: Option<u64>,
    ) -> (Vec<String>, Vec<String>) {
        std::fs::write(dir.path().join("vogt-codex.toml"), fragment).unwrap();
        let id = Uuid::new_v4();
        let conversation = id.to_string();
        let cwd = "/work/wi-3871";
        let template = cmd(&["-c", "tui.notifications=true"]);
        let launch_request = LaunchRequest {
            model: Some(model),
            effort: None,
            resume: Some(&conversation),
            brief_file: None,
            session_id: Some(id),
            codex_session_id: true,
            permission_mode: Some("accept-edits"),
            settings_file: None,
            add_dirs: &[],
            opencode_config: None,
            codex_approval_policy: "never",
            mcp: None,
            codex_compact_limit: compact_limit,
        };
        let host_command = [cmd(&["codex"]), template.clone()].concat();
        let host = launch_with_codex(
            Some(&host_command),
            &launch_request,
            &CodexExtras {
                guidance: None,
                charter: None,
                providers,
                provider: Some(selected),
                catalog: None,
                config_model: None,
                trust_cwd: Some(Path::new(cwd)),
                mcp_servers: &[],
            },
        )
        .unwrap()
        .command
        .expect("a rewritten command");
        let host = codex_args_of(&host).unwrap();

        let pod_config = vogt_session_agent::Config {
            session_id: "s".into(),
            engine_url: "http://127.0.0.1:1".into(),
            token: "t".into(),
            cli: "codex".into(),
            command: None,
            cwd: Some(cwd.into()),
            cols: 80,
            rows: 24,
            buffer_bytes: 1024,
            claude_config_dir: None,
            codex_home: Some(dir.path().to_path_buf()),
            account: None,
            conversation_id: Some(conversation),
            resume: true,
            model: Some(model.into()),
            effort: None,
            permission_mode: Some("accept-edits".into()),
            codex_approval_policy: "never".into(),
            claude: vogt_session_agent::ClaudeLaunch::default(),
            codex: vogt_session_agent::CodexLaunch {
                guidance: None,
                charter: None,
                compact_limit,
                template_args: template,
                config_file: None,
            },
        };
        let line = vogt_session_agent::command_line(&pod_config).unwrap();
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("codex() {{ printf '%s\\n' \"$@\"; }}; {line}"))
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        let pod: Vec<String> = String::from_utf8(out.stdout)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect();
        (host, pod)
    }

    /// WI-3871 review blocker 2: a provider that sets neither limit puts
    /// neither key on the host argv or the pod's launch line.
    #[test]
    #[cfg(unix)]
    fn codex_launch_omits_limits_a_provider_does_not_set() {
        let dir = tempfile::tempdir().unwrap();
        let plain = vogt_session_agent::CodexProvider {
            name: "litellm".into(),
            base_url: "http://litellm:4000".into(),
            wire_api: "responses".into(),
            env_key: "LITELLM_API_KEY".into(),
            context_window: None,
            auto_compact_token_limit: None,
        };
        let fragment = r#"model_provider = "litellm"
model = "gpt-5.6"

[model_providers.litellm]
base_url = "http://litellm:4000"
wire_api = "responses"
env_key = "LITELLM_API_KEY"
"#;
        let (host, pod) = codex_host_and_pod_argv(
            &dir,
            fragment,
            std::slice::from_ref(&plain),
            "litellm",
            "gpt-5.6",
            None,
        );
        for argv in [&host, &pod] {
            assert!(
                !argv.iter().any(
                    |arg| arg.contains("context_window") || arg.contains("compact_token_limit")
                ),
                "an unconfigured provider put a limit on the line: {argv:?}"
            );
        }
        assert_eq!(host, pod, "host and pod Codex argv differ");
    }

    /// A provider that is registered but not selected contributes its table,
    /// never the launch flags.
    #[test]
    #[cfg(unix)]
    fn codex_launch_ignores_limits_of_an_unselected_provider() {
        let dir = tempfile::tempdir().unwrap();
        let plain = vogt_session_agent::CodexProvider {
            name: "litellm".into(),
            base_url: "http://litellm:4000".into(),
            wire_api: "responses".into(),
            env_key: "LITELLM_API_KEY".into(),
            context_window: None,
            auto_compact_token_limit: None,
        };
        let limited = vogt_session_agent::CodexProvider {
            name: "deepseek".into(),
            base_url: "http://litellm:4000/v1".into(),
            wire_api: "chat".into(),
            env_key: "DEEPSEEK_KEY".into(),
            context_window: Some(1_000_000),
            auto_compact_token_limit: Some(500_000),
        };
        let fragment = r#"model_provider = "litellm"
model = "gpt-5.6"

[model_providers.litellm]
base_url = "http://litellm:4000"
wire_api = "responses"
env_key = "LITELLM_API_KEY"

[model_providers.deepseek]
base_url = "http://litellm:4000/v1"
wire_api = "chat"
env_key = "DEEPSEEK_KEY"
context_window = 1000000
auto_compact_token_limit = 500000
"#;
        let providers = [plain, limited];
        let (host, pod) =
            codex_host_and_pod_argv(&dir, fragment, &providers, "litellm", "gpt-5.6", None);
        for argv in [&host, &pod] {
            assert!(
                !argv
                    .iter()
                    .any(|arg| arg.starts_with("model_context_window")
                        || arg.starts_with("model_auto_compact_token_limit")),
                "an unselected provider put a launch limit on the line: {argv:?}"
            );
            // Its table still travels whole, for a launch that selects it.
            assert!(argv
                .iter()
                .any(|arg| arg.starts_with("model_providers.deepseek=")));
        }
        // Two provider tables arrive in registry order on the host and TOML
        // order (sorted) from the pod's file; the words are the same.
        let mut host_sorted = host.clone();
        host_sorted.sort();
        let mut pod_sorted = pod.clone();
        pod_sorted.sort();
        assert_eq!(host_sorted, pod_sorted, "host and pod Codex argv differ");
    }

    /// WI-3871 review blocker 1: the backoff cannot replace the provider's
    /// limit with a lower deployment value, and there is exactly one emission
    /// of the key when both are present.
    #[test]
    #[cfg(unix)]
    fn codex_compact_limit_never_lowers_the_provider_limit() {
        let dir = tempfile::tempdir().unwrap();
        let deepseek = vogt_session_agent::CodexProvider {
            name: "litellm".into(),
            base_url: "http://litellm:4000/v1".into(),
            wire_api: "chat".into(),
            env_key: "LITELLM_API_KEY".into(),
            context_window: Some(1_000_000),
            auto_compact_token_limit: Some(500_000),
        };
        let fragment = r#"model_provider = "litellm"
model = "deepseek-v4-flash"

[model_providers.litellm]
base_url = "http://litellm:4000/v1"
wire_api = "chat"
env_key = "LITELLM_API_KEY"
context_window = 1000000
auto_compact_token_limit = 500000
"#;
        // A backoff derived from the provider's 500k base, not from 200k/272k.
        let (host, pod) = codex_host_and_pod_argv(
            &dir,
            fragment,
            std::slice::from_ref(&deepseek),
            "litellm",
            "deepseek-v4-flash",
            Some(540_000),
        );
        for argv in [&host, &pod] {
            let limits: Vec<&String> = argv
                .iter()
                .filter(|arg| arg.starts_with("model_auto_compact_token_limit="))
                .collect();
            assert_eq!(limits.len(), 1, "{argv:?}");
            assert_eq!(limits[0].as_str(), "model_auto_compact_token_limit=540000");
            assert!(argv.iter().any(|arg| arg == "model_context_window=1000000"));
        }
        assert_eq!(host, pod, "host and pod Codex argv differ");

        // A stale deployment-sized floor (96k) must not come back at all.
        let (host, pod) = codex_host_and_pod_argv(
            &dir,
            fragment,
            std::slice::from_ref(&deepseek),
            "litellm",
            "deepseek-v4-flash",
            Some(96_000),
        );
        for argv in [&host, &pod] {
            assert!(
                !argv
                    .iter()
                    .any(|arg| arg == "model_auto_compact_token_limit=96000"),
                "{argv:?}"
            );
            assert_eq!(
                argv.iter()
                    .filter(|arg| arg.starts_with("model_auto_compact_token_limit="))
                    .count(),
                1,
                "{argv:?}"
            );
            assert!(argv
                .iter()
                .any(|arg| arg == "model_auto_compact_token_limit=500000"));
        }
        assert_eq!(host, pod, "host and pod Codex argv differ");
    }
}
