//! Turning "run it on GPT 5.6, medium" into the flags one agent CLI wants
//! (FR-T11).
//!
//! This is engine knowledge on purpose. Vogt decides *which* model a session
//! was asked for and audits that decision; how a model id reaches a running
//! process is `claude --model`, `codex -m`, or `opencode --model`, and which
//! of those exist is a property of this pod's image rather than of the estate.
//!
//! Three rules, each written against a specific way this could go wrong:
//!
//! **A command with no mapping is refused, never started plain.** A session
//! that quietly ignored `model` would spawn, run, answer, and be the wrong
//! model — a failure with no symptom. The refusal names the binary, so the
//! reader learns which template they actually asked for.
//!
//! **The values are validated before they become argv.** The caller here is
//! ultimately an LLM tool call, and these strings are handed to a process
//! spawn. A model id is a narrow vocabulary — letters, digits, and `.` `_`
//! `-` `/` `:` — and anything else is refused rather than escaped, because
//! escaping is where "it's only a model name" turns into an extra flag.
//!
//! **The flags go on the end.** Every supported form ends with the agent
//! binary and its own arguments (`mydevenv2-agent-auth run -- claude`), so
//! appending puts them where that binary reads them.

use crate::error::{ApiError, Result};

/// Ceiling on a model id or effort level. Real ones are far shorter; this is
/// only here so a pathological string cannot reach a spawn.
const MAX_VALUE_LEN: usize = 128;

/// What the engine knows how to tell a model to.
///
/// Kept as data rather than scattered through `create`, so the answer to
/// "which agent CLIs can be asked for a model" is one readable list.
const KNOWN: &[&str] = &["claude", "codex", "opencode"];

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
    let model = trimmed(model);
    let effort = trimmed(effort);
    if model.is_none() && effort.is_none() {
        return Ok(None);
    }
    if let Some(value) = model {
        validate("model", value)?;
    }
    if let Some(value) = effort {
        validate("effort", value)?;
    }

    let Some(command) = command.filter(|c| !c.is_empty()) else {
        return Err(ApiError::BadRequest(
            "a session with no command runs the default shell, which has no \
             model to choose; start it with an agent template (Claude Code, \
             Codex or OpenCode) to name a model"
                .into(),
        ));
    };
    let binary = agent_binary(command);
    let mut extra: Vec<String> = Vec::new();
    match binary.as_str() {
        "claude" => {
            if let Some(model) = model {
                extra.extend(["--model".to_string(), model.to_string()]);
            }
            if let Some(effort) = effort {
                extra.extend(["--effort".to_string(), effort.to_string()]);
            }
        }
        "codex" => {
            if let Some(model) = model {
                extra.extend(["-m".to_string(), model.to_string()]);
            }
            if let Some(effort) = effort {
                // Codex has no dedicated flag; the documented way is a config
                // override, and `-c key=value` is a first-class argument
                // rather than a trick.
                extra.extend(["-c".to_string(), format!("model_reasoning_effort={effort}")]);
            }
        }
        "opencode" => {
            if let Some(model) = model {
                extra.extend(["--model".to_string(), model.to_string()]);
            }
            if effort.is_some() {
                // Named rather than dropped: OpenCode takes a model and has
                // no effort control, and a session that ignored the second
                // half of the request would look like it honoured all of it.
                return Err(ApiError::BadRequest(
                    "opencode takes a model but has no reasoning-effort \
                     control; ask for a model alone, or use Claude Code or \
                     Codex for an effort level"
                        .into(),
                ));
            }
        }
        other => {
            return Err(ApiError::BadRequest(format!(
                "session command `{other}` is not an agent CLI this engine \
                 knows how to pass a model to (it knows: {}); it would have \
                 started on its own default and looked like it worked",
                KNOWN.join(", ")
            )));
        }
    }
    let mut rewritten = command.to_vec();
    rewritten.extend(extra);
    Ok(Some(rewritten))
}

fn trimmed(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|v| !v.is_empty())
}

/// The agent CLI inside a possibly-wrapped command.
///
/// `mydevenv2-agent-auth run -- claude` is the shape every protected template
/// uses, so the binary that cares about `--model` is the one after the `--`.
/// Falls back to the first word for a bare `["claude"]`, which is what
/// vogt-core sends when a caller names a template.
fn agent_binary(command: &[String]) -> String {
    let candidate = match command.iter().position(|arg| arg == "--") {
        Some(idx) if idx + 1 < command.len() => &command[idx + 1],
        _ => &command[0],
    };
    candidate
        .rsplit('/')
        .next()
        .unwrap_or(candidate)
        .to_string()
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
        // Every protected template is `mydevenv2-agent-auth run -- <cli>`. If
        // the first word decided, every one of them would be refused as an
        // unknown CLI and the templates people actually use would be the ones
        // that could not be given a model.
        let out = apply(
            Some(&cmd(&["mydevenv2-agent-auth", "run", "--", "claude"])),
            Some("claude-sonnet-4-5"),
            None,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            out,
            cmd(&[
                "mydevenv2-agent-auth",
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
}
