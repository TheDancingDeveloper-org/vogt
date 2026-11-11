//! The environment-dump guard (WI-4046, after WI-3988).
//!
//! A session's environment holds its own credentials — the broker token, the
//! Vogt HTTP token, whatever a grant put there — and a command that prints
//! the environment puts them in the transcript, where a redaction pass may or
//! may not catch them. The workspace rule is "redact before print"; this
//! guard refuses the commands whose whole purpose is to print the
//! environment, or a variable whose name says it is a credential:
//!
//! - `env` with no program (`env`, `env -0`, `env -u NAME`, `env | grep X`),
//!   but not `env FOO=bar cmd` (which runs `cmd`) or `env -i` (which prints
//!   only what the line gives it);
//! - `printenv` with no name, with a name matching `*TOKEN*`, `*SECRET*`,
//!   `*KEY*`, `*PASSWORD*`, `*PASSWD*`, `*CREDENTIAL*` or `*PRIVATE*`, or
//!   with a name that is an expansion or arrives on stdin (`xargs printenv`);
//!   `printenv HOME` is allowed;
//! - `set` with no arguments, `export` / `readonly` with no name (`export
//!   -p`), `declare` / `typeset` with no name (`declare -x`, `declare -p`),
//!   and `declare -p` of a credential-named variable;
//! - any read of `/proc/<pid>/environ`, and `ps` with BSD `e` (`ps auxe`),
//!   which prints every process's environment;
//! - `echo` of a credential-named variable (`echo $VOGT_HTTP_TOKEN`) or of an
//!   indirect expansion (`echo ${!name}`); `echo $AGENT_AUTH_MODE` is
//!   allowed;
//! - a one-liner that prints the whole of `os.environ` or `process.env`.
//!
//! It reads the same simple commands the rest of the policy reads, including
//! substitutions, `sh -c` / `su -c` payloads, `eval` and `xargs`. A line the
//! lexer cannot read with certainty that names one of these programs is
//! refused (fail closed).
//!
//! Like the rest of this module, it is a guardrail, not a boundary: a session
//! can write a script file and run it, spell the program as a variable, or
//! read the environment with a non-shell tool. It exists to stop the
//! accident — an agent reading its own environment to find a variable — not
//! a determined leak. The way to check a credential is there is `test -n
//! "$NAME"`, which the refusal says.

use std::sync::LazyLock;

use regex::Regex;

use super::{
    base, command_words_assigning, dash_c_payload, flatten, is_assignment, lex, mentions,
    shell_payload, shell_words, xargs_command, Payload, Piece,
};

/// The hint every refusal ends with.
const HINT: &str = "never print environment variables or tokens; name the variable you \
                    need and check it with `test -n \"$NAME\"`";

/// How deep `sh -c` / `eval` payloads are followed before the line counts as
/// unreadable.
const MAX_DEPTH: usize = 8;

/// A variable name that says it holds a credential.
static SENSITIVE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)TOKEN|SECRET|KEY|PASSWORD|PASSWD|CREDENTIAL|PRIVATE").expect("static regex")
});

/// An expansion of a named variable: `$NAME`, `${NAME…}`, or an indirect
/// `${!NAME}`.
static EXPANSION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\$\{?(!?)([A-Za-z_][A-Za-z0-9_]*)").expect("static regex"));

/// A one-liner printing the whole environment object.
static WHOLE_ENVIRON: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?x)
        (print|pprint|dumps|dict|repr|str|list|sorted)\s*\(\s*(dict\s*\(\s*)?os\.environ\s*\)
        | os\.environ\.(items|values|copy)\s*\(
        | console\.(log|dir|error)\s*\(\s*process\.env\s*\)
        | JSON\.stringify\s*\(\s*process\.env\b
        | \bprint\s+%ENV\b
        ",
    )
    .expect("static regex")
});

/// `Some(reason)` when `command` would print the session's environment or a
/// credential-named variable. The reason names the rule, never the match.
pub fn check(command: &str) -> Option<String> {
    let normalized = normalize(command);
    if reads_proc_environ(&normalized) {
        return Some(refuse("reading /proc/<pid>/environ"));
    }
    if WHOLE_ENVIRON.is_match(&normalized) {
        return Some(refuse("printing the whole environment object"));
    }
    line(command, 0).map(refuse)
}

fn refuse(what: &str) -> String {
    format!("refusing: environment guard: {what} would print the session's credentials; {HINT}")
}

/// `text` with quotes and backslashes removed, so `/pr"oc"/self/environ` and
/// `print(os."environ")` read as what they spell.
fn normalize(text: &str) -> String {
    text.replace("\\\n", "")
        .chars()
        .filter(|c| !matches!(c, '\'' | '"' | '\\'))
        .collect()
}

/// A read of a process's environment file: `/proc/self/environ`,
/// `/proc/$$/environ`, `/proc/1/environ`, or an `environ` read after a `cd`
/// into `/proc`.
fn reads_proc_environ(normalized: &str) -> bool {
    let lower = normalized.to_ascii_lowercase();
    if !lower.contains("/proc") {
        return false;
    }
    lower
        .split(|c: char| {
            c.is_whitespace() || matches!(c, '/' | ';' | '&' | '|' | '<' | '>' | '(' | ')')
        })
        .any(|w| w == "environ")
}

/// The first rule `command` breaks, read as a shell would split it.
fn line(command: &str, depth: usize) -> Option<&'static str> {
    let (_, opaque) = lex(command);
    if opaque && mentions(command, &["env", "printenv"]) {
        return Some("a line that cannot be read and names an environment-printing program");
    }
    if depth > MAX_DEPTH {
        return Some("a nested payload too deep to read");
    }
    pieces(&lex(command).0, command, depth, false)
}

/// The first rule `pieces` break. `captured` is true inside a substitution,
/// whose output feeds the line rather than the terminal. `text` is the line
/// the pieces came from, read to see whether a command's output is piped.
fn pieces(pieces_: &[Piece], text: &str, depth: usize, captured: bool) -> Option<&'static str> {
    let mut cursor = 0;
    for piece in pieces_ {
        let found = match piece {
            Piece::Cmd(simple) => {
                let piped = match text[cursor..].find(simple.as_str()) {
                    Some(at) => {
                        cursor += at + simple.len();
                        let after = text[cursor..].trim_start();
                        after.starts_with('|') && !after.starts_with("||")
                    }
                    None => false,
                };
                simple_command(simple, depth, false, captured || piped)
            }
            Piece::Sub(inner) => pieces(inner, &flatten(inner).join("\n"), depth, true),
            Piece::Open | Piece::Close => None,
        };
        if found.is_some() {
            return found;
        }
    }
    None
}

/// The rule one simple command breaks. `stdin_args` is true when the
/// command's operands arrive on stdin (an `xargs` payload).
/// `quiet` is true when the command's output does not reach the terminal
/// (piped on, or captured by a substitution): an `echo` of a credential is
/// then how it is handed to a program, not printed.
fn simple_command(
    simple: &str,
    depth: usize,
    stdin_args: bool,
    quiet: bool,
) -> Option<&'static str> {
    let raw = shell_words(simple);
    let (words, _, _) = command_words_assigning(simple);
    // `env` alone, `env -u X`, `FOO=1 env`: the wrapper reader steps over
    // the env and leaves its last word (or nothing) as the "program", so the
    // raw words decide whether an env runs nothing.
    if bare_env(&raw) {
        return Some("`env` with no program");
    }
    if words.is_empty() {
        return None;
    }
    let program = base(&words[0]);
    let args = &words[1..];
    let operands: Vec<&String> = args
        .iter()
        .filter(|w| !w.starts_with('-') && !w.starts_with('+') && !redirect(w))
        .collect();
    let options: Vec<&String> = args.iter().filter(|w| w.starts_with('-')).collect();
    match program {
        "printenv" => {
            if stdin_args {
                return Some("`printenv` of names read from stdin");
            }
            if operands.is_empty() {
                return Some("`printenv` with no name");
            }
            if operands.iter().any(|w| w.contains(['$', '`'])) {
                return Some("`printenv` of a name that is an expansion");
            }
            operands
                .iter()
                .any(|w| SENSITIVE.is_match(w))
                .then_some("`printenv` of a credential-named variable")
        }
        "set" => {
            (stdin_args || args.iter().all(|w| redirect(w))).then_some("`set` with no arguments")
        }
        "export" | "readonly" => {
            (stdin_args || operands.is_empty()).then_some("`export`/`readonly` with no name")
        }
        "declare" | "typeset" | "local" => {
            if stdin_args {
                return Some("`declare` of names read from stdin");
            }
            if operands.is_empty() {
                // `declare -f` / `-F` list functions, not variables.
                let functions_only = !options.is_empty()
                    && options
                        .iter()
                        .all(|o| o[1..].chars().all(|c| matches!(c, 'f' | 'F')));
                return (!functions_only).then_some("`declare` with no name");
            }
            let prints = options.iter().any(|o| o.contains('p'));
            (prints
                && operands
                    .iter()
                    .any(|w| SENSITIVE.is_match(w) || w.contains(['$', '`'])))
            .then_some("`declare -p` of a credential-named variable")
        }
        "ps" => args
            .iter()
            .any(|w| {
                !w.starts_with('-') && w.chars().all(|c| c.is_ascii_alphabetic()) && w.contains('e')
            })
            .then_some("`ps` with BSD `e`, which prints every process's environment"),
        "echo" | "print" | "printf" => {
            let to_file = raw.iter().enumerate().any(|(i, w)| {
                let r = w.trim_start_matches('1');
                (r == ">" || r == ">>") && raw.get(i + 1).is_some_and(|t| !t.starts_with('&'))
                    || (r.starts_with('>') && r.len() > 1 && !r[1..].starts_with(['&', '>']))
                    || (r.starts_with(">>") && r.len() > 2 && !r[2..].starts_with('&'))
            });
            if quiet || to_file {
                None
            } else {
                echo(simple)
            }
        }
        "bash" | "sh" | "zsh" | "dash" | "ksh" => match shell_payload(&words) {
            Payload::Script(payload) => line(payload, depth + 1),
            Payload::File | Payload::Stdin => None,
        },
        "su" | "runuser" | "script" => dash_c_payload(&words).and_then(|p| line(p, depth + 1)),
        "eval" => line(&args.join(" "), depth + 1),
        "xargs" => {
            let payload = xargs_command(&words).join(" ");
            if payload.is_empty() {
                // `xargs` alone runs `echo`.
                return None;
            }
            simple_command(&payload, depth + 1, true, quiet)
        }
        _ => None,
    }
}

/// Whether `words` holds an `env` that runs no program and so prints the
/// environment: no `-i`/`--ignore-environment`/`-`, and only options and
/// assignments after it. Only an `env` in program position counts: leading
/// assignments and `sudo`/`command`/`exec` wrappers (with their options) may
/// come first.
fn bare_env(words: &[String]) -> bool {
    let mut i = 0;
    while i < words.len() {
        let word = base(&words[i]);
        if is_assignment(&words[i]) || word.starts_with('-') {
            i += 1;
            continue;
        }
        if matches!(
            word,
            "sudo" | "doas" | "command" | "exec" | "nohup" | "builtin"
        ) {
            i += 1;
            continue;
        }
        break;
    }
    if words.get(i).map(|w| base(w)) != Some("env") {
        return false;
    }
    let mut clears = false;
    let mut rest = words[i + 1..].iter();
    while let Some(word) = rest.next() {
        if redirect(word) {
            continue;
        }
        if word == "-" {
            clears = true;
            continue;
        }
        if let Some(long) = word.strip_prefix("--") {
            if long.is_empty() {
                continue;
            }
            let (name, attached) = match long.split_once('=') {
                Some((n, _)) => (n, true),
                None => (long, false),
            };
            let is = |full: &str| name.len() >= 2 && full.starts_with(name);
            if is("ignore-environment") {
                clears = true;
            } else if is("split-string") {
                // The program is in the string: not a bare env.
                return false;
            } else if (is("unset") || is("chdir")) && !attached {
                rest.next();
            }
            continue;
        }
        if word.starts_with('-') {
            for (i, letter) in word.char_indices().skip(1) {
                match letter {
                    'i' => clears = true,
                    'S' => return false,
                    'u' | 'C' => {
                        if word[i + 1..].is_empty() {
                            rest.next();
                        }
                        break;
                    }
                    _ => {}
                }
            }
            continue;
        }
        if is_assignment(word) {
            continue;
        }
        // A program: env runs it.
        return false;
    }
    !clears
}

/// A redirection word (`>out`, `2>&1`, `>`): not an operand.
fn redirect(word: &str) -> bool {
    word.trim_start_matches(|c: char| c.is_ascii_digit())
        .starts_with(['<', '>', '&'])
}

/// An `echo` that prints a credential-named variable or an indirect
/// expansion.
fn echo(simple: &str) -> Option<&'static str> {
    EXPANSION.captures_iter(simple).find_map(|caps| {
        if &caps[1] == "!" {
            Some("`echo` of an indirect expansion")
        } else if SENSITIVE.is_match(&caps[2]) {
            Some("`echo` of a credential-named variable")
        } else {
            None
        }
    })
}

#[cfg(test)]
mod tests {
    use super::check;

    #[test]
    fn refuses_commands_that_print_the_environment() {
        for command in [
            "env",
            "env -0",
            "env -u PATH",
            "env --unset=PATH",
            "env | grep VOGT",
            "env|sort",
            "sudo env",
            "FOO=bar env",
            "/usr/bin/env",
            "printenv",
            "printenv -0",
            "printenv VOGT_HTTP_TOKEN",
            "printenv VOGT_ENGINE_BROKER_TOKEN",
            "printenv AWS_SECRET_ACCESS_KEY",
            "printenv DB_PASSWORD",
            "printenv \"$v\"",
            "for v in $(compgen -e); do printenv \"$v\"; done",
            "compgen -e | xargs printenv",
            "compgen -v | xargs -n1 declare -p",
            "set",
            "set | grep TOKEN",
            "set -o posix; set",
            "export",
            "export -p",
            "readonly -p",
            "declare -x",
            "declare -p",
            "declare -px",
            "typeset",
            "declare -p VOGT_HTTP_TOKEN",
            "cat /proc/self/environ",
            "cat /proc/1/environ | tr '\\0' '\\n'",
            "tr '\\0' '\\n' < /proc/$$/environ",
            "xargs -0 -n1 < /proc/self/environ",
            "strings /pr\"oc\"/self/environ",
            "cd /proc/self && cat environ",
            "ps auxe",
            "ps eww",
            "echo $VOGT_HTTP_TOKEN",
            "echo \"${VOGT_ENGINE_BROKER_TOKEN}\"",
            "echo token=$GITHUB_TOKEN",
            "printf '%s\\n' \"$VOGT_HTTP_TOKEN\"",
            "echo $VOGT_HTTP_TOKEN >&2",
            "echo $VOGT_HTTP_TOKEN || true",
            "true && echo $VOGT_HTTP_TOKEN",
            "echo ${!name}",
            "bash -c 'env'",
            "sh -lc \"printenv | sort\"",
            "eval printenv",
            "su -c env",
            "x=$(env)",
            "echo \"$(printenv)\"",
            "diff <(env) /dev/null",
            "python3 -c 'import os; print(os.environ)'",
            "python3 -c 'import os,json; print(json.dumps(dict(os.environ)))'",
            "python3 -c 'import os; [print(k, v) for k, v in os.environ.items()]'",
            "node -e 'console.log(process.env)'",
            "perl -e 'print %ENV'",
            "echo 'unterminated && printenv",
        ] {
            let reason = check(command).unwrap_or_else(|| panic!("allowed: {command}"));
            assert!(reason.contains("environment guard"), "{command}: {reason}");
            assert!(reason.contains("test -n"), "{command}: {reason}");
        }
    }

    #[test]
    fn allows_ordinary_commands() {
        for command in [
            "env FOO=bar cargo test",
            "env -i PATH=/usr/bin cargo build",
            "env -i",
            "env -i FOO=bar",
            "env -u FOO cargo test",
            "env -C /tmp ls",
            "env -S 'cargo test'",
            "FOO=bar cargo test",
            "printenv HOME",
            "printenv PATH HOME",
            "printenv AGENT_AUTH_MODE",
            "echo $AGENT_AUTH_MODE",
            "echo \"$HOME\" $PATH",
            "echo hello",
            "echo ${arr[@]}",
            "test -n \"$VOGT_HTTP_TOKEN\" && echo set",
            "[ -n \"$VOGT_ENGINE_BROKER_TOKEN\" ] && echo present",
            "curl -H @<(printf 'Authorization: Bearer %s\\n' \"$TOKEN\") https://example.invalid",
            "echo \"$REGISTRY_TOKEN\" | docker login --password-stdin ghcr.io",
            "printf '%s' \"$VOGT_HTTP_TOKEN\" > /tmp/t",
            "curl -H \"Authorization: Bearer $(printf %s \"$TOKEN\")\" https://example.invalid",
            "set -euo pipefail",
            "set -e",
            "set -o",
            "set +x",
            "set -- a b",
            "export FOO=bar",
            "export PATH=\"$HOME/bin:$PATH\"",
            "export -n FOO",
            "readonly X=1",
            "declare -A map",
            "declare -p HOME",
            "declare -f",
            "declare -F",
            "local x=1",
            "compgen -v",
            "compgen -e | wc -l",
            "ps aux",
            "ps -ef",
            "ps -e",
            "ls /proc/self",
            "cat /proc/cpuinfo",
            "grep -r environment docs/",
            "git status",
            "cargo test -p vogt-engine-server policy",
            "python3 -c 'import os; print(os.environ[\"HOME\"])'",
            "python3 -c 'import os; print(os.environ.get(\"HOME\"))'",
            "node -e 'console.log(process.env.HOME)'",
            "bash -c 'cargo test'",
            "bash script.sh",
            "xargs -n1 echo < list",
            "ls | xargs",
            "rg -n printenv engine/",
        ] {
            assert_eq!(check(command), None, "{command}");
        }
    }

    #[test]
    fn the_reason_never_repeats_the_command() {
        let reason = check("printenv VOGT_HTTP_TOKEN").unwrap();
        assert!(!reason.contains("VOGT_HTTP_TOKEN"), "{reason}");
        let reason = check("echo $GITHUB_TOKEN").unwrap();
        assert!(!reason.contains("GITHUB_TOKEN"), "{reason}");
    }
}
