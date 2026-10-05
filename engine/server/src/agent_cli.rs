//! Turning "run it on GPT 5.6, medium, on this brief" into the argv one agent
//! CLI wants
//!
//! This is engine knowledge on purpose. Vogt decides *which* model a session
//! was asked for and audits that decision; how a model id reaches a running
//! process is `claude --model`, `codex -m`, or `opencode --model`, and which
//! of those exist is a property of this pod's image rather than of the estate.
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
    let asked = model.is_some() || effort.is_some() || resume.is_some();

    let Some(command) = command.filter(|c| !c.is_empty()) else {
        if asked {
            return Err(ApiError::BadRequest(
                "a session with no command runs the default shell, which has no \
                 model to choose or conversation to resume; start it with an \
                 agent template (Claude Code, Codex or OpenCode)"
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
    // after the binary. Only then is Claude Code's conversation id pinned: a
    // command that already carries `--continue` or a `--session-id` of its
    // own would be refused by Claude Code with ours added.
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
    (binary == "claude" && binary_idx + 1 == command.len()).then(|| {
        vogt_engine_contract::AgentConversation {
            agent: binary,
            id: session_id.to_string(),
        }
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
