//! Which agent, model and effort a session is running, and how we know (WI-919).
//!
//! Ports `src/vogt/core/runtime.py`. Three sources, best first, and every answer
//! says which it came from: `transcript` (what the agent CLI recorded for its
//! latest turn), `command` (a flag on the session's command line), `asked`
//! (what `session.start` was asked for). None of them is a default the CLI
//! might pick: an unknown is null, not a guess. Pure.

use regex::Regex;
use std::sync::LazyLock;

const AGENTS: [&str; 4] = ["claude", "codex", "opencode", "klaudia"];

static EFFORT_OVERRIDE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^model_reasoning_effort=(.+)$").expect("static"));

/// The agent CLI a command runs and the model/effort flags it carries.
pub struct CommandRuntime {
    pub agent: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
}

/// `shlex.split`, falling back to a whitespace split when the quoting is
/// unbalanced — the `ValueError` path `from_command` takes.
fn words_of(command: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut chars = command.chars().peekable();
    let mut quote: Option<char> = None;
    let mut started = false;
    while let Some(ch) = chars.next() {
        match (quote, ch) {
            (None, '"' | '\'') => {
                quote = Some(ch);
                started = true;
            }
            (Some(open), candidate) if candidate == open => quote = None,
            (None, '\\') => {
                if let Some(next) = chars.next() {
                    current.push(next);
                    started = true;
                }
            }
            (None, ' ' | '\t' | '\n') => {
                if started {
                    words.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            (_, other) => {
                current.push(other);
                started = true;
            }
        }
    }
    if quote.is_some() {
        return command.split_whitespace().map(str::to_string).collect();
    }
    if started {
        words.push(current);
    }
    words
}

pub fn from_command(command: Option<&str>) -> CommandRuntime {
    let Some(command) = command.filter(|command| !command.is_empty()) else {
        return CommandRuntime {
            agent: None,
            model: None,
            effort: None,
        };
    };
    let words = words_of(command);
    let mut agent = None;
    let mut model = None;
    let mut effort = None;
    for (index, word) in words.iter().enumerate() {
        let name = word.rsplit('/').next().unwrap_or(word);
        if agent.is_none() && AGENTS.contains(&name) {
            agent = Some(name.to_string());
        }
        let following = words.get(index + 1).map(String::as_str);
        if matches!(word.as_str(), "--model" | "-m") {
            if let Some(value) = following {
                model = Some(value.to_string());
            }
        } else if let Some(value) = word.strip_prefix("--model=") {
            model = Some(value.to_string());
        } else if word == "--effort" {
            if let Some(value) = following {
                effort = Some(value.to_string());
            }
        } else if let Some(value) = word.strip_prefix("--effort=") {
            effort = Some(value.to_string());
        } else if word == "-c" {
            if let Some(captures) = following.and_then(|value| EFFORT_OVERRIDE.captures(value)) {
                effort = Some(
                    captures[1]
                        .trim_matches(|ch| ch == '\'' || ch == '"')
                        .to_string(),
                );
            }
        }
    }
    CommandRuntime {
        agent,
        model,
        effort,
    }
}

/// The best answer for each of model and effort, with its source.
pub struct Resolved {
    pub agent: Option<String>,
    pub model: Option<String>,
    pub model_basis: Option<&'static str>,
    pub effort: Option<String>,
    pub effort_basis: Option<&'static str>,
}

fn pick(candidates: &[(&Option<String>, &'static str)]) -> (Option<String>, Option<&'static str>) {
    for (value, basis) in candidates {
        if let Some(value) = value.as_ref().filter(|value| !value.is_empty()) {
            return (Some(value.clone()), Some(*basis));
        }
    }
    (None, None)
}

pub fn resolve(
    command: Option<&str>,
    conversation_agent: Option<&str>,
    transcript_model: Option<&str>,
    transcript_effort: Option<&str>,
    asked_model: Option<&str>,
    asked_effort: Option<&str>,
) -> Resolved {
    let flags = from_command(command);
    let transcript_model = transcript_model.map(str::to_string);
    let transcript_effort = transcript_effort.map(str::to_string);
    let asked_model = asked_model.map(str::to_string);
    let asked_effort = asked_effort.map(str::to_string);
    let (model, model_basis) = pick(&[
        (&transcript_model, "transcript"),
        (&flags.model, "command"),
        (&asked_model, "asked"),
    ]);
    let (effort, effort_basis) = pick(&[
        (&transcript_effort, "transcript"),
        (&flags.effort, "command"),
        (&asked_effort, "asked"),
    ]);
    Resolved {
        agent: flags
            .agent
            .or_else(|| conversation_agent.map(str::to_string)),
        model,
        model_basis,
        effort,
        effort_basis,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_and_the_agent_name_come_off_the_command() {
        let parsed = from_command(Some("claude --model opus --effort high"));
        assert_eq!(parsed.agent.as_deref(), Some("claude"));
        assert_eq!(parsed.model.as_deref(), Some("opus"));
        assert_eq!(parsed.effort.as_deref(), Some("high"));
    }

    #[test]
    fn a_codex_config_override_is_the_effort() {
        let parsed = from_command(Some("codex -c model_reasoning_effort='low'"));
        assert_eq!(parsed.effort.as_deref(), Some("low"));
    }

    #[test]
    fn the_transcript_beats_the_command_and_what_was_asked() {
        let resolved = resolve(
            Some("claude --model haiku"),
            None,
            Some("opus"),
            None,
            Some("sonnet"),
            Some("low"),
        );
        assert_eq!(resolved.model.as_deref(), Some("opus"));
        assert_eq!(resolved.model_basis, Some("transcript"));
        assert_eq!(resolved.effort.as_deref(), Some("low"));
        assert_eq!(resolved.effort_basis, Some("asked"));
    }

    #[test]
    fn an_unknown_stays_unknown() {
        let resolved = resolve(None, Some("klaudia"), None, None, None, None);
        assert_eq!(resolved.agent.as_deref(), Some("klaudia"));
        assert!(resolved.model.is_none());
        assert!(resolved.model_basis.is_none());
    }
}
