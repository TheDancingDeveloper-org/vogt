//! Turning "run it on GPT 5.6, medium, on this brief" into the argv one agent
//! CLI wants
//!
//! This is engine knowledge on purpose. Vogt decides *which* model a session
//! was asked for and audits that decision; how a model id reaches a running
//! process is `claude --model`, `codex -m`, or `opencode --model`, and which
//! of those exist is a property of this pod's image rather than of the estate
//! (`klaudia --model` too, since WI-950).
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

use std::path::Path;

use uuid::Uuid;

use crate::error::{ApiError, Result};

/// Ceiling on a model id, effort level or conversation id. Real ones are far
/// shorter; this is only here so a pathological string cannot reach a spawn.
const MAX_VALUE_LEN: usize = 128;

/// What the engine knows how to tell a model to.
///
/// Kept as data rather than scattered through `create`, so the answer to
/// "which agent CLIs can be asked for a model" is one readable list.
const KNOWN: &[&str] = &["claude", "codex", "opencode", "klaudia"];

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
#[derive(Debug, Default, Clone, Copy)]
pub struct LaunchRequest<'a> {
    pub model: Option<&'a str>,
    pub effort: Option<&'a str>,
    /// The agent CLI's own id for a previous conversation to continue.
    pub resume: Option<&'a str>,
    /// Where the session's brief will be written, when it has one. The agent
    /// is told to read it as its first prompt.
    pub brief_file: Option<&'a Path>,
    /// The engine's id for the session. A fresh, bare Claude Code launch uses
    /// it as the conversation id, so the conversation can later be resumed by
    /// the id every session and history listing already shows.
    pub session_id: Option<Uuid>,
    /// `default` / `accept-edits` / `bypass` (WI-926).
    pub permission_mode: Option<&'a str>,
    /// The deployment's driven-session settings (an `autoMode` policy) to
    /// hand Claude Code with `--settings`, when it has one.
    pub settings_file: Option<&'a Path>,
    /// The deployment's opencode driven-session policy (an opencode config
    /// with a `permission` block), for the default posture (WI-932).
    pub opencode_config: Option<&'a str>,
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
/// flags — which Klaudia shares (its legacy `acceptEdits` mode is the same
/// "file edits yes, everything else asks", and `--dangerously-skip-permissions`
/// its bypass). `None` is the default posture: no flag at all.
pub fn permission_flags(mode: Option<&str>) -> Result<Option<Vec<String>>> {
    match mode.map(str::trim).filter(|m| !m.is_empty()) {
        None | Some("default") => Ok(None),
        Some("accept-edits") | Some("accept_edits") => Ok(Some(vec![
            "--permission-mode".to_string(),
            "acceptEdits".to_string(),
        ])),
        Some("bypass") => Ok(Some(vec!["--dangerously-skip-permissions".to_string()])),
        Some(other) => Err(ApiError::BadRequest(format!(
            "permission_mode {other:?} is not one of default, accept-edits, bypass"
        ))),
    }
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
    format!(
        "Vogt started this session with a brief in {}. Read that file now. \
         If it has a \"Task\" section, carry that task out; otherwise \
         summarise the brief in one line and wait for instructions.",
        path.display()
    )
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
            ..LaunchRequest::default()
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
    let asked = model.is_some() || effort.is_some() || resume.is_some() || permission.is_some();

    let Some(command) = command.filter(|c| !c.is_empty()) else {
        if asked {
            return Err(ApiError::BadRequest(
                "a session with no command runs the default shell, which has no \
                 model to choose or conversation to resume; start it with an \
                 agent template (Claude Code, Codex, OpenCode or Klaudia)"
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
    if permission.is_some() && binary == "codex" {
        // Named rather than dropped: a posture that silently did not apply
        // is the failure this refusal exists against.
        return Err(ApiError::BadRequest(format!(
            "permission_mode is applied to Claude Code, opencode and Klaudia; `{binary}` has \
             its own approval settings in its launcher and is not told one per session"
        )));
    }
    if binary == "klaudia" && effort.is_some() {
        // Named rather than dropped, as for opencode: Klaudia takes a model
        // and has no reasoning-effort flag.
        return Err(ApiError::BadRequest(
            "klaudia takes a model but has no reasoning-effort control; ask \
             for a model alone, or use Claude Code or Codex for an effort level"
                .into(),
        ));
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
    // after the binary. Only then is Claude Code's (or Klaudia's)
    // conversation id pinned: a command that already carries `--continue` or
    // a `--session-id` of its own would be refused with ours added.
    let bare = binary_idx + 1 == command.len();
    let prompt = req.brief_file.map(brief_instruction);

    let mut rewritten = command.to_vec();
    let mut env = Vec::new();
    match binary.as_str() {
        "claude" => {
            if let Some(model) = model {
                rewritten.extend(["--model".to_string(), model.to_string()]);
            }
            if let Some(effort) = effort {
                rewritten.extend(["--effort".to_string(), effort.to_string()]);
            }
            if let Some(id) = resume {
                rewritten.extend(["--resume".to_string(), id.to_string()]);
            } else if let (true, Some(id)) = (bare, req.session_id) {
                rewritten.extend(["--session-id".to_string(), id.to_string()]);
            }
            if let Some(flags) = permission {
                rewritten.extend(flags);
            }
            if let Some(file) = req.settings_file {
                // The driven-session policy (WI-926): auto-mode rules added to
                // Claude Code's own through `$defaults`. `=` form, like
                // `--add-dir`, so it can never take a following positional.
                rewritten.push(format!("--settings={}", file.display()));
            }
            if let Some(dir) = req.brief_file.and_then(Path::parent) {
                // The brief lives under the engine's state directory, outside
                // the session's working directory, and reading it there would
                // stop a fresh session at a permission prompt before it began
                // (WI-912). `--add-dir` is variadic: the `=` form takes one
                // value, so the positional prompt after it stays a prompt.
                rewritten.push(format!("--add-dir={}", dir.display()));
            }
            // Positional, and last: `claude [options] [prompt]`.
            if let Some(prompt) = prompt {
                rewritten.push(prompt);
            }
            env.extend(
                CLAUDE_QUIET_ENV
                    .iter()
                    .map(|(k, v)| ((*k).to_string(), (*v).to_string())),
            );
        }
        "codex" => {
            if let Some(id) = resume {
                // A subcommand, so straight after the binary; whatever the
                // template carried follows it, and `codex resume` takes the
                // same options the bare TUI does.
                rewritten.splice(
                    binary_idx + 1..binary_idx + 1,
                    ["resume".to_string(), id.to_string()],
                );
            }
            if let Some(model) = model {
                rewritten.extend(["-m".to_string(), model.to_string()]);
            }
            if let Some(effort) = effort {
                // Codex has no dedicated flag; the documented way is a config
                // override, and `-c key=value` is a first-class argument
                // rather than a trick.
                rewritten.extend(["-c".to_string(), format!("model_reasoning_effort={effort}")]);
            }
            // Positional, and last: `codex [PROMPT]` / `codex resume [ID] [PROMPT]`.
            if let Some(prompt) = prompt {
                rewritten.push(prompt);
            }
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
        "klaudia" => {
            // Klaudia (WI-950) takes Claude Code's launch flags. Its TUI
            // auto-resumes the newest conversation in the directory unless
            // told otherwise, so a fresh launch always names one: the
            // engine's id when the command is bare (so a wake resumes it by
            // the id the session already shows), else `--new-session`.
            if let Some(model) = model {
                rewritten.extend(["--model".to_string(), model.to_string()]);
            }
            if let Some(id) = resume {
                rewritten.extend(["--resume".to_string(), id.to_string()]);
            } else if let (true, Some(id)) = (bare, req.session_id) {
                rewritten.extend(["--session-id".to_string(), id.to_string()]);
            } else if bare {
                rewritten.push("--new-session".to_string());
            }
            if let Some(flags) = permission {
                rewritten.extend(flags);
            }
            // Never positional: Klaudia reads a positional prompt as `-p`
            // (answer and exit), so a session started that way did one turn
            // and ended. `--prompt-interactive` opens the TUI and submits it
            // as the first message (msp-klaudia WI-953), and the session
            // stays for the next turn. `=` form, so the brief pointer can
            // never be read as a flag.
            if let Some(prompt) = prompt {
                rewritten.push(format!("--prompt-interactive={prompt}"));
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

/// The agent conversation a session started with `command` runs, when its
/// id is known before the CLI says anything: the one it resumes, or — for a
/// bare Claude Code launch, which [`launch`] pins with `--session-id` — the
/// engine's own id for the session. `None` for anything else, including a
/// fresh Codex or OpenCode session, whose id the CLI mints itself: resuming
/// by a guess could continue somebody else's conversation.
pub fn conversation(
    command: Option<&[String]>,
    resume: Option<&str>,
    session_id: Uuid,
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
    (matches!(binary.as_str(), "claude" | "klaudia") && binary_idx + 1 == command.len()).then(
        || vogt_engine_contract::AgentConversation {
            agent: binary,
            id: session_id.to_string(),
        },
    )
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

fn validate(field: &str, value: &str) -> Result<()> {
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
                "-m",
                "gpt-5.6",
                "-c",
                "model_reasoning_effort=medium"
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
        assert_eq!(out, cmd(&["/usr/local/bin/codex", "-m", "gpt-5.6"]));
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
            ..LaunchRequest::default()
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
        assert_eq!(codex.len(), 5);
        assert!(codex[4].contains(BRIEF));

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
            ..LaunchRequest::default()
        };
        assert_eq!(
            launch(Some(&protected("claude")), &req)
                .unwrap()
                .command
                .unwrap(),
            [protected("claude"), cmd(&["--model", "m1", "--resume", id])].concat()
        );
        // `resume` is a codex subcommand, so it sits straight after the
        // binary, ahead of the flags.
        assert_eq!(
            launch(Some(&protected("codex")), &req)
                .unwrap()
                .command
                .unwrap(),
            [protected("codex"), cmd(&["resume", id, "-m", "m1"])].concat()
        );
        assert_eq!(
            launch(Some(&cmd(&["codex", "--search"])), &req)
                .unwrap()
                .command
                .unwrap(),
            cmd(&["codex", "resume", id, "--search", "-m", "m1"])
        );
        assert_eq!(
            launch(
                Some(&protected("opencode")),
                &LaunchRequest {
                    resume: Some("ses_abc123"),
                    ..LaunchRequest::default()
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
            ..LaunchRequest::default()
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
                ..LaunchRequest::default()
            };
            assert!(
                launch(Some(&protected("claude")), &req).is_err(),
                "{hostile:?} reached argv"
            );
        }
    }

    #[test]
    fn a_fresh_bare_claude_launch_pins_its_conversation_id() {
        // WI-833: the conversation is then resumable by the engine session
        // id every listing already shows.
        let sid = Uuid::new_v4();
        let req = LaunchRequest {
            session_id: Some(sid),
            ..LaunchRequest::default()
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
        // Codex cannot be told an id, so nothing is added for it.
        assert!(launch(Some(&protected("codex")), &req)
            .unwrap()
            .command
            .is_none());
    }

    #[test]
    fn claude_sessions_get_the_quiet_defaults_and_others_do_not() {
        // WI-832: prompt suggestions are accepted by a bare Enter, which a
        // driving agent sends.
        let env = launch(Some(&protected("claude")), &LaunchRequest::default())
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
        assert!(launch(Some(&protected("codex")), &LaunchRequest::default())
            .unwrap()
            .env
            .is_empty());
        assert!(launch(Some(&cmd(&["bash"])), &LaunchRequest::default())
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
                ..LaunchRequest::default()
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
    fn codex_is_still_not_told_a_posture() {
        let command = vec!["codex".to_string()];
        let err = launch(
            Some(&command),
            &LaunchRequest {
                permission_mode: Some("bypass"),
                ..LaunchRequest::default()
            },
        )
        .expect_err("codex has no per-session posture");
        assert!(err.to_string().contains("codex"), "{err}");
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
        let found = conversation(Some(&wrapped), None, id).unwrap();
        assert_eq!((found.agent.as_str(), found.id), ("claude", id.to_string()));

        let resumed = conversation(Some(&wrapped), Some("abc-123"), id).unwrap();
        assert_eq!(resumed.id, "abc-123");
        let codex = conversation(Some(&argv(&["codex"])), Some("r1"), id).unwrap();
        assert_eq!((codex.agent.as_str(), codex.id.as_str()), ("codex", "r1"));
    }

    #[test]
    fn an_unknown_id_is_none_not_a_guess() {
        let id = Uuid::new_v4();
        // Codex mints its own id; a claude with arguments of its own may
        // carry `--continue` or a session id the engine did not pin.
        assert!(conversation(Some(&argv(&["codex"])), None, id).is_none());
        assert!(conversation(Some(&argv(&["claude", "--continue"])), None, id).is_none());
        assert!(conversation(Some(&argv(&["bash"])), Some("x"), id).is_none());
        assert!(conversation(None, None, id).is_none());
        assert!(conversation(Some(&argv(&["claude"])), Some("-bad"), id).is_none());
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
        assert_eq!(permission_flags(None).unwrap(), None);
        assert_eq!(permission_flags(Some("default")).unwrap(), None);
        assert_eq!(
            permission_flags(Some("accept_edits")).unwrap(),
            Some(cmd(&["--permission-mode", "acceptEdits"]))
        );
        assert_eq!(
            permission_flags(Some("bypass")).unwrap(),
            Some(cmd(&["--dangerously-skip-permissions"]))
        );
        assert!(permission_flags(Some("yolo")).is_err());
    }

    #[test]
    fn the_policy_rides_every_claude_launch_and_a_posture_only_claude() {
        let policy = Path::new("/usr/local/share/vogt/driven-session-settings.json");
        let out = launch(
            Some(&cmd(&["vogt-agent-auth", "run", "--", "claude"])),
            &LaunchRequest {
                permission_mode: Some("bypass"),
                settings_file: Some(policy),
                ..LaunchRequest::default()
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
        // Codex and a plain shell are refused a posture, never started plain.
        for command in [cmd(&["codex"]), cmd(&["bash"])] {
            let refused = launch(
                Some(&command),
                &LaunchRequest {
                    permission_mode: Some("bypass"),
                    ..LaunchRequest::default()
                },
            );
            assert!(refused.is_err(), "{command:?}");
        }
        // A policy file alone is not an ask: Codex starts without it.
        assert_eq!(
            launch(
                Some(&cmd(&["codex"])),
                &LaunchRequest {
                    settings_file: Some(policy),
                    ..LaunchRequest::default()
                }
            )
            .unwrap(),
            Launch::default()
        );
    }

    #[test]
    fn klaudia_takes_claude_codes_flags_and_pins_the_engine_id() {
        let id = Uuid::new_v4();
        let brief = Path::new("/state/sessions/x.md");
        let out = launch(
            Some(&cmd(&["vogt-agent-auth", "run", "--", "klaudia"])),
            &LaunchRequest {
                model: Some("grok-4.7"),
                brief_file: Some(brief),
                session_id: Some(id),
                permission_mode: Some("accept-edits"),
                ..LaunchRequest::default()
            },
        )
        .unwrap();
        let command = out.command.unwrap();
        assert_eq!(
            command[..8],
            cmd(&[
                "vogt-agent-auth",
                "run",
                "--",
                "klaudia",
                "--model",
                "grok-4.7",
                "--session-id",
                &id.to_string(),
            ])
        );
        assert_eq!(command[8..10], cmd(&["--permission-mode", "acceptEdits"]));
        // The brief pointer goes to the TUI as its first message, never as
        // the positional prompt Klaudia would answer headless and exit on.
        assert_eq!(
            command.last().unwrap(),
            &format!("--prompt-interactive={}", brief_instruction(brief))
        );
        assert!(
            !command.contains(&brief_instruction(brief)),
            "a positional prompt makes Klaudia one-shot"
        );
        // None of Claude Code's own environment or settings: they are
        // Claude Code's, and Klaudia would ignore or misread them.
        assert!(out.env.is_empty());
        assert!(!command.iter().any(|a| a.starts_with("--settings")));
    }

    #[test]
    fn klaudia_resumes_by_id_and_a_fresh_one_never_picks_up_another() {
        let out = launch(
            Some(&cmd(&["klaudia"])),
            &LaunchRequest {
                resume: Some("72d33a6b-bed4-4229-8f35-ac6bbcf960f5"),
                session_id: Some(Uuid::new_v4()),
                ..LaunchRequest::default()
            },
        )
        .unwrap()
        .command
        .unwrap();
        assert_eq!(
            out,
            cmd(&[
                "klaudia",
                "--resume",
                "72d33a6b-bed4-4229-8f35-ac6bbcf960f5"
            ])
        );
        // Without an engine id (a caller that has none), the TUI would
        // otherwise resume whatever ran last in the directory.
        let out = launch(
            Some(&cmd(&["klaudia"])),
            &LaunchRequest {
                model: Some("opus"),
                ..LaunchRequest::default()
            },
        )
        .unwrap()
        .command
        .unwrap();
        assert_eq!(out, cmd(&["klaudia", "--model", "opus", "--new-session"]));
    }

    #[test]
    fn klaudia_bypass_is_its_skip_flag_and_effort_is_refused() {
        let out = launch(
            Some(&cmd(&["klaudia"])),
            &LaunchRequest {
                permission_mode: Some("bypass"),
                session_id: Some(Uuid::nil()),
                ..LaunchRequest::default()
            },
        )
        .unwrap()
        .command
        .unwrap();
        assert!(out.contains(&"--dangerously-skip-permissions".to_string()));
        let refused = launch(
            Some(&cmd(&["klaudia"])),
            &LaunchRequest {
                effort: Some("high"),
                ..LaunchRequest::default()
            },
        )
        .unwrap_err();
        assert!(refused.to_string().contains("klaudia"), "{refused}");
    }

    #[test]
    fn a_bare_klaudia_launch_runs_the_engines_conversation() {
        let id = Uuid::new_v4();
        let conversation = conversation(Some(&cmd(&["klaudia"])), None, id).unwrap();
        assert_eq!(conversation.agent, "klaudia");
        assert_eq!(conversation.id, id.to_string());
        assert_eq!(
            agent_name(&cmd(&["vogt-agent-auth", "run", "--", "klaudia"])).as_deref(),
            Some("klaudia")
        );
    }
}
