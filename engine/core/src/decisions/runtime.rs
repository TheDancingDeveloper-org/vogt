// Which agent, model and effort a session is running, and how we know.
// Ports `core/runtime.py`. Three sources, best first, and every answer says
// which it came from. None of them is a default the CLI might pick: an
// unknown is absent, not a guess.

const AGENTS: &[&str] = &["claude", "codex", "opencode", "klaudia"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandRuntime {
    pub agent: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
}

/// The agent CLI a command runs and the model and effort flags it carries.
/// A command that does not parse as a shell line is split on whitespace.
pub fn runtime_from_command(command: Option<&str>) -> CommandRuntime {
    let Some(command) = command.filter(|text| !text.is_empty()) else {
        return CommandRuntime {
            agent: None,
            model: None,
            effort: None,
        };
    };
    let words = split_command(command);
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
            if let Some(value) = following.filter(|text| !text.is_empty()) {
                model = Some(value.to_string());
            }
        } else if let Some(value) = word.strip_prefix("--model=") {
            model = Some(value.to_string());
        } else if word == "--effort" {
            if let Some(value) = following.filter(|text| !text.is_empty()) {
                effort = Some(value.to_string());
            }
        } else if let Some(value) = word.strip_prefix("--effort=") {
            effort = Some(value.to_string());
        } else if word == "-c" {
            if let Some(value) = following
                .filter(|text| !text.is_empty())
                .and_then(effort_override)
            {
                effort = Some(value);
            }
        }
    }
    CommandRuntime {
        agent,
        model,
        effort,
    }
}

/// `model_reasoning_effort=<value>`, with one layer of quotes stripped.
fn effort_override(value: &str) -> Option<String> {
    let rest = value.strip_prefix("model_reasoning_effort=")?;
    Some(rest.trim_matches(|c| c == '\'' || c == '"').to_string())
}

/// `shlex.split` in POSIX mode, falling back to a split on space, tab and
/// newline when the line is unbalanced. An empty quoted token is kept, and a
/// backslash before a character shlex does not escape stays a backslash.
pub(super) fn split_command(command: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut chars = command.chars().peekable();
    let mut quote: Option<char> = None;
    let mut started = false;
    let mut ok = true;
    while let Some(ch) = chars.next() {
        match (quote, ch) {
            (None, '"' | '\'') => {
                quote = Some(ch);
                started = true;
            }
            (Some(open), c) if c == open => quote = None,
            (None, '\\') => match chars.next() {
                Some('\n') => {
                    current.push('\n');
                    started = true;
                }
                Some(c) => {
                    current.push(c);
                    started = true;
                }
                None => ok = false,
            },
            (Some('"'), '\\') => match chars.peek().copied() {
                Some('\n') => {
                    chars.next();
                    current.push('\\');
                    current.push('\n');
                }
                Some(c @ ('"' | '\\')) => {
                    chars.next();
                    current.push(c);
                }
                Some(_) => current.push('\\'),
                None => ok = false,
            },
            (None, ' ' | '\t' | '\r' | '\n') => {
                if started {
                    words.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            (_, c) => {
                current.push(c);
                started = true;
            }
        }
    }
    if quote.is_some() {
        ok = false;
    }
    if started {
        words.push(current);
    }
    if ok {
        words
    } else {
        command
            .split(|ch: char| ch.is_whitespace() || matches!(ch, '\u{1c}'..='\u{1f}'))
            .filter(|word| !word.is_empty())
            .map(str::to_string)
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedRuntime {
    pub agent: Option<String>,
    pub model: Option<String>,
    pub model_basis: Option<&'static str>,
    pub effort: Option<String>,
    pub effort_basis: Option<&'static str>,
}

/// The best answer for each of model and effort, with its source. Transcript
/// beats the command line, which beats what the session was asked for.
pub fn resolve_runtime(
    command: Option<&str>,
    conversation_agent: Option<&str>,
    transcript_model: Option<&str>,
    transcript_effort: Option<&str>,
    asked_model: Option<&str>,
    asked_effort: Option<&str>,
) -> ResolvedRuntime {
    let flags = runtime_from_command(command);
    let pick =
        |candidates: &[(&Option<String>, &'static str)]| -> (Option<String>, Option<&'static str>) {
            for (value, basis) in candidates {
                if let Some(value) = value.as_deref().filter(|text| !text.is_empty()) {
                    return (Some(value.to_string()), Some(*basis));
                }
            }
            (None, None)
        };
    let transcript_model = transcript_model.map(str::to_string);
    let asked_model = asked_model.map(str::to_string);
    let transcript_effort = transcript_effort.map(str::to_string);
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
    ResolvedRuntime {
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
mod runtime_tests {
    use super::*;

    #[test]
    fn a_command_names_its_agent_model_and_effort() {
        let found = runtime_from_command(Some("claude --model opus --effort high"));
        assert_eq!(found.agent.as_deref(), Some("claude"));
        assert_eq!(found.model.as_deref(), Some("opus"));
        assert_eq!(found.effort.as_deref(), Some("high"));

        let equals = runtime_from_command(Some(
            "/usr/bin/codex --model=gpt-5 -c model_reasoning_effort='xhigh'",
        ));
        assert_eq!(equals.agent.as_deref(), Some("codex"));
        assert_eq!(equals.model.as_deref(), Some("gpt-5"));
        assert_eq!(equals.effort.as_deref(), Some("xhigh"));

        assert!(runtime_from_command(None).agent.is_none());
        assert!(runtime_from_command(Some("")).model.is_none());

        // An empty quoted token is a word, but an empty flag value is absent,
        // so the effort stays unset and the agent is the word after it.
        let quoted = runtime_from_command(Some("--effort '' claude"));
        assert!(quoted.effort.is_none());
        assert_eq!(quoted.agent.as_deref(), Some("claude"));
        let kept = runtime_from_command(Some(r#""a\$b" claude"#));
        assert_eq!(kept.agent.as_deref(), Some("claude"));
    }

    #[test]
    fn the_transcript_beats_the_command_which_beats_what_was_asked() {
        let found = resolve_runtime(
            Some("claude --model haiku --effort low"),
            None,
            Some("opus"),
            Some("high"),
            Some("sonnet"),
            Some("medium"),
        );
        assert_eq!(found.model.as_deref(), Some("opus"));
        assert_eq!(found.model_basis, Some("transcript"));
        assert_eq!(found.effort.as_deref(), Some("high"));
        assert_eq!(found.effort_basis, Some("transcript"));

        let asked = resolve_runtime(
            Some("echo hi"),
            Some("klaudia"),
            None,
            None,
            Some("sonnet"),
            None,
        );
        assert_eq!(asked.agent.as_deref(), Some("klaudia"));
        assert_eq!(asked.model.as_deref(), Some("sonnet"));
        assert_eq!(asked.model_basis, Some("asked"));
        assert_eq!(asked.effort, None);

        let blank = resolve_runtime(None, Some(""), None, None, None, None);
        assert_eq!(blank.agent.as_deref(), Some(""));
    }
}
