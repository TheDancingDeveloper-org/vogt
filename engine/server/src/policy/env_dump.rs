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
//! - `echo`/`printf` of a credential-named variable (`echo
//!   $VOGT_HTTP_TOKEN`) or of an indirect expansion (`echo ${!name}`), whether
//!   it reaches the terminal directly, through a display program (`| cat`,
//!   `| tee`, `| xxd`, `| grep`) or through a device file (`> /dev/stdout`,
//!   `> /dev/tty`); `echo $AGENT_AUTH_MODE` is allowed, and so is a pipe into
//!   a consumer (`docker login --password-stdin`, `wc`, a checksum) or a
//!   redirect to a regular file;
//! - a here-string of a credential that a reader would print (`cat <<<`);
//! - an inline Python, Node, Perl, Ruby, awk or jq program that reads the
//!   environment (`os.environ`, `os.getenv("TOKEN")`, `process.env`, `%ENV`,
//!   `ENV`, `ENVIRON`, `jq env`), and the whole-object one-liners;
//! - a credential word is matched as a whole underscore-separated word, so
//!   `$MONKEY`, `$KEY_COUNT` and `$SORT_KEYS` are ordinary names.
//!
//! It reads the same simple commands the rest of the policy reads, including
//! substitutions, `sh -c` / `su -c` payloads, `eval`, `xargs` and the wrappers
//! the policy steps over (`nice`, `timeout`, `time`, `stdbuf`, `busybox`,
//! `sudo`, …). A line the lexer cannot read with certainty that names one of
//! these programs is refused (fail closed).
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

/// Words that, as a whole underscore-separated word of a variable name, say
/// the name holds a credential. `KEY` is matched only at the end
/// (`AWS_ACCESS_KEY`), so `KEY_COUNT` and `SORT_KEYS` are not keys; the
/// plurals catch `ENGINE_EXTRA_TOKENS_JSON`.
const SENSITIVE_WORDS: &[&str] = &[
    "TOKEN",
    "TOKENS",
    "SECRET",
    "SECRETS",
    "PASSWORD",
    "PASSWORDS",
    "PASSWD",
    "CREDENTIAL",
    "CREDENTIALS",
    "PRIVATE",
];

/// Whether `name` says it holds a credential. The match is on whole words,
/// not a substring: `MONKEY`, `KEY_COUNT` and `SORT_KEYS` are ordinary names.
fn credential_name(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    let word = |w: &str| {
        upper == w || upper.ends_with(&format!("_{w}")) || upper.contains(&format!("_{w}_"))
    };
    upper == "KEY"
        || upper.ends_with("_KEY")
        || upper.contains("_KEY_")
        || SENSITIVE_WORDS.iter().any(|w| word(w))
}

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
/// the pieces came from, read to see where each command's output goes.
fn pieces(pieces_: &[Piece], text: &str, depth: usize, captured: bool) -> Option<&'static str> {
    let after = after_text(pieces_, text);
    let mut quiet: Vec<Option<bool>> = vec![None; pieces_.len()];
    for (index, piece) in pieces_.iter().enumerate() {
        let found = match piece {
            Piece::Cmd(simple) => simple_command(
                simple,
                depth,
                false,
                captured || pipeline_is_quiet(index, pieces_, &after, &mut quiet),
            ),
            Piece::Sub(inner) => pieces(inner, &flatten(inner).join("\n"), depth, true),
            Piece::Open | Piece::Close => None,
        };
        if found.is_some() {
            return found;
        }
    }
    None
}

/// The text after each command in `text`, read in order. A command's output
/// is piped when this starts with a single `|` (`||` is not a pipe).
fn after_text<'a>(pieces_: &[Piece], text: &'a str) -> Vec<Option<&'a str>> {
    let mut after = vec![None; pieces_.len()];
    let mut cursor = 0;
    for (index, piece) in pieces_.iter().enumerate() {
        if let Piece::Cmd(simple) = piece {
            if let Some(at) = text[cursor..].find(simple.as_str()) {
                cursor += at + simple.len();
                after[index] = Some(text[cursor..].trim_start());
            }
        }
    }
    after
}

/// Whether a command's output never reaches the terminal: it is captured by
/// a substitution, or the pipeline it feeds ends in a program that consumes
/// the bytes (a login, a checksum, a line count) or writes them to a regular
/// file. A pipe to `cat`, `tee`, `xxd` or `grep` still shows the input.
fn pipeline_is_quiet(
    index: usize,
    pieces_: &[Piece],
    after: &[Option<&str>],
    memo: &mut [Option<bool>],
) -> bool {
    if let Some(known) = memo[index] {
        return known;
    }
    let quiet = match (after[index], pieces_.get(index + 1)) {
        (Some(rest), Some(Piece::Cmd(next)))
            if rest.starts_with('|') && !rest.starts_with("||") =>
        {
            quiet_sink(next) || pipeline_is_quiet(index + 1, pieces_, after, memo)
        }
        _ => false,
    };
    memo[index] = Some(quiet);
    quiet
}

/// Whether `simple`, as the last stage of a pipe, does not show its input on
/// the terminal.
fn quiet_sink(simple: &str) -> bool {
    let raw = shell_words(simple);
    let (words, _, _) = command_words_assigning(simple);
    let program = words.first().map(|w| base(w)).unwrap_or_default();
    let args = words.get(1..).unwrap_or(&[]);
    if matches!(program, "docker" | "podman" | "nerdctl")
        && args.iter().any(|w| w == "login")
        && args.iter().any(|w| w == "--password-stdin")
    {
        return true;
    }
    if matches!(
        program,
        "wc" | "cksum" | "md5sum" | "sha1sum" | "sha256sum" | "sha512sum"
    ) {
        return true;
    }
    if program == "grep"
        && args
            .iter()
            .any(|w| matches!(w.as_str(), "-q" | "--quiet" | "--silent" | "-c" | "--count"))
    {
        return true;
    }
    redirects_to_file(&raw) && !redirects_to_terminal(&raw)
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
    // A here-string of a credential is printed by whatever reads it, unless
    // that reader consumes the bytes or writes them to a file.
    if simple.contains("<<<")
        && credential_expansion(simple).is_some()
        && !quiet
        && !quiet_sink(simple)
    {
        return Some("a here-string of a credential-named variable");
    }
    // Another language reading the environment inside its own program.
    if let Some(reason) = interpreter(program, args) {
        return Some(reason);
    }
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
                .any(|w| credential_name(w))
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
                    .any(|w| credential_name(w) || w.contains(['$', '`'])))
            .then_some("`declare -p` of a credential-named variable")
        }
        "ps" => args
            .iter()
            .any(|w| {
                !w.starts_with('-') && w.chars().all(|c| c.is_ascii_alphabetic()) && w.contains('e')
            })
            .then_some("`ps` with BSD `e`, which prints every process's environment"),
        "echo" | "print" | "printf" => {
            let to_file = redirects_to_file(&raw);
            // A file is quiet unless it is the terminal in disguise.
            if quiet || (to_file && !redirects_to_terminal(&raw)) {
                None
            } else {
                credential_expansion(simple)
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

/// A Python one-liner that prints the environment or reads a credential.
static PY_ENVIRON_WHOLE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?x)
        (print|pprint|dumps|dict|list|sorted|repr|str)\s*\(\s*(os\.)?environ\s*\)
        | (os\.)?environ\.(items|values|copy)\s*\(
        ",
    )
    .expect("static regex")
});

/// A Python read of one named environment variable.
static PY_ENVIRON_NAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?x)
        (?:os\.)?environ\.get\s*\(\s*['"]?([A-Za-z_][A-Za-z0-9_]*)
        | (?:os\.)?getenv\s*\(\s*['"]?([A-Za-z_][A-Za-z0-9_]*)
        | (?:os\.)?environ\s*\[\s*['"]?([A-Za-z_][A-Za-z0-9_]*)
        "#,
    )
    .expect("static regex")
});

/// A Node read of one named environment variable.
static NODE_ENVIRON_NAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?x)
        process\.env\.([A-Za-z_][A-Za-z0-9_]*)
        | process\.env\s*\[\s*['"]?([A-Za-z_][A-Za-z0-9_]*)
        "#,
    )
    .expect("static regex")
});

/// A Node whole-object read: `process.env` not narrowed by a property.
static NODE_ENVIRON_WHOLE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"process\.env\s*(?:$|[;,\)\]}\s])").expect("static regex"));

/// A Perl read of one named environment variable.
static PERL_ENVIRON_NAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"\$ENV\s*\{\s*['"]?([A-Za-z_][A-Za-z0-9_]*)"#).expect("static regex")
});

fn any_credential(captures: regex::Captures<'_>) -> bool {
    captures
        .iter()
        .skip(1)
        .flatten()
        .any(|m| credential_name(m.as_str()))
}

/// A read of the environment inside another language's inline program. The
/// shell does not expand here; the program itself looks the environment up.
fn interpreter(program: &str, args: &[String]) -> Option<&'static str> {
    let script = args.join(" ");
    match program {
        "python" | "python2" | "python3" => {
            if PY_ENVIRON_WHOLE.is_match(&script)
                || PY_ENVIRON_NAME.captures_iter(&script).any(any_credential)
            {
                Some("a Python program that reads the environment")
            } else {
                None
            }
        }
        "node" | "nodejs" => {
            if NODE_ENVIRON_WHOLE.is_match(&script)
                || NODE_ENVIRON_NAME.captures_iter(&script).any(any_credential)
            {
                Some("a Node program that reads the environment")
            } else {
                None
            }
        }
        "perl" => {
            if script.contains("%ENV")
                || PERL_ENVIRON_NAME.captures_iter(&script).any(any_credential)
            {
                Some("a Perl program that reads the environment")
            } else {
                None
            }
        }
        "ruby" | "irb" => script
            .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .any(|w| w == "ENV")
            .then_some("a Ruby program that reads the environment"),
        "awk" | "gawk" | "mawk" | "nawk" => script
            .contains("ENVIRON")
            .then_some("an awk program that reads `ENVIRON`"),
        "jq" => args
            .iter()
            .any(|a| a.trim() == "env" || a.contains("$ENV"))
            .then_some("a jq program that reads the environment"),
        _ => None,
    }
}

/// Whether `words` holds an `env` that runs no program and so prints the
/// environment: no `-i`/`--ignore-environment`/`-`, and only options and
/// assignments after it. Only an `env` in program position counts: leading
/// assignments and the wrappers the policy steps over (`sudo`, `nice`,
/// `timeout`, `stdbuf`, `busybox`, …), with their options and values, may
/// come first.
fn bare_env(words: &[String]) -> bool {
    let mut i = 0;
    while i < words.len() {
        let word = base(&words[i]);
        if is_assignment(&words[i]) || word.starts_with('-') {
            i += 1;
            continue;
        }
        match word {
            "sudo" | "doas" | "command" | "exec" | "nohup" | "builtin" | "setsid" | "chronic"
            | "busybox" | "time" => {
                i += 1;
            }
            "nice" | "ionice" | "stdbuf" => {
                i += 1;
                while words.get(i).is_some_and(|w| w.starts_with('-')) {
                    let value = matches!(
                        words[i].as_str(),
                        "-n" | "--adjustment" | "-c" | "--class" | "-p" | "-t" | "-i" | "-o" | "-e"
                    );
                    i += 1;
                    if value && words.get(i).is_some_and(|w| !w.starts_with('-')) {
                        i += 1;
                    }
                }
            }
            "timeout" => {
                i += 1;
                while words.get(i).is_some_and(|w| w.starts_with('-')) {
                    let value =
                        matches!(words[i].as_str(), "-s" | "--signal" | "-k" | "--kill-after");
                    i += 1;
                    if value {
                        i += 1;
                    }
                }
                // The duration (`timeout 5 env`).
                if words.get(i).is_some_and(|w| !w.starts_with('-')) {
                    i += 1;
                }
            }
            _ => break,
        }
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

/// Whether `words` redirect `>`/`>>` to a file (rather than duplicating a
/// descriptor with `>&2`).
fn redirects_to_file(words: &[String]) -> bool {
    words.iter().enumerate().any(|(i, w)| {
        let r = w.trim_start_matches('1');
        (r == ">" || r == ">>") && words.get(i + 1).is_some_and(|t| !t.starts_with('&'))
            || (r.starts_with('>') && r.len() > 1 && !r[1..].starts_with(['&', '>']))
            || (r.starts_with(">>") && r.len() > 2 && !r[2..].starts_with('&'))
    })
}

/// Whether a redirect target is the terminal (or a descriptor naming it).
fn terminal_target(target: &str) -> bool {
    let target = target.trim_matches(['\'', '"']);
    matches!(
        target,
        "/dev/stdout" | "/dev/stderr" | "/dev/tty" | "/dev/console"
    ) || target.starts_with("/dev/fd/1")
        || target.starts_with("/dev/fd/2")
        || target.starts_with("/proc/self/fd/1")
        || target.starts_with("/proc/self/fd/2")
}

/// Whether `words` redirect to the terminal: `> /dev/stdout`, `>/dev/tty`,
/// `> /proc/self/fd/1`. Such a redirect is not the quiet file the guard
/// otherwise allows.
fn redirects_to_terminal(words: &[String]) -> bool {
    words.iter().enumerate().any(|(i, w)| {
        let r = w.trim_start_matches(|c: char| c.is_ascii_digit());
        let attached = r.trim_start_matches(['<', '>', '&']);
        if attached.is_empty() {
            words.get(i + 1).is_some_and(|t| terminal_target(t))
        } else {
            terminal_target(attached)
        }
    })
}

/// A credential-named expansion, or an indirect `${!name}` expansion, on the
/// line: what `echo`, `printf` or a here-string would print.
fn credential_expansion(simple: &str) -> Option<&'static str> {
    EXPANSION.captures_iter(simple).find_map(|caps| {
        if &caps[1] == "!" {
            Some("`echo` of an indirect expansion")
        } else if credential_name(&caps[2]) {
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

    /// WI-4061: the bypasses a delta review of WI-4046 found. A pipe or a
    /// device redirect still reaches the terminal, a here-string is printed
    /// by its reader, and an interpreter reads the environment itself.
    #[test]
    fn wi_4061_refuses_the_reviewed_bypasses() {
        for command in [
            // A display program downstream still shows the token.
            "echo \"$VOGT_HTTP_TOKEN\" | cat",
            "echo \"$VOGT_HTTP_TOKEN\" | tee /tmp/x",
            "echo \"$VOGT_HTTP_TOKEN\" | xxd",
            "echo \"$VOGT_HTTP_TOKEN\" | grep -i token",
            "printf '%s\\n' \"$VOGT_HTTP_TOKEN\" | base64",
            "env | cat",
            "printenv GITHUB_TOKEN > /tmp/x",
            // A redirect to a device that is the terminal.
            "echo \"$VOGT_HTTP_TOKEN\" > /dev/stdout",
            "echo \"$VOGT_HTTP_TOKEN\" >/dev/tty",
            "echo \"$VOGT_HTTP_TOKEN\" > /proc/self/fd/1",
            "echo \"$VOGT_HTTP_TOKEN\" > /dev/console",
            // A here-string is printed by a reader.
            "cat <<< \"$VOGT_HTTP_TOKEN\"",
            "base64 <<< \"$VOGT_HTTP_TOKEN\"",
            // Another language reads the environment itself.
            "awk 'BEGIN{for (k in ENVIRON) print k, ENVIRON[k]}'",
            "gawk 'BEGIN{print ENVIRON[\"VOGT_HTTP_TOKEN\"]}'",
            "perl -e 'print $ENV{VOGT_HTTP_TOKEN}'",
            "ruby -e 'puts ENV.to_h'",
            "node -p process.env",
            "node -e 'console.log(process.env.VOGT_HTTP_TOKEN)'",
            "jq -n env",
            "jq -n '$ENV'",
            // More Python forms.
            "python3 -c 'from os import environ; print(dict(environ))'",
            "python3 -c 'import os; print(os.environ[\"VOGT_HTTP_TOKEN\"])'",
            "python3 -c 'import os; print(os.getenv(\"VOGT_HTTP_TOKEN\"))'",
            // Bare `env` behind a wrapper.
            "nice env",
            "timeout 5 env",
            "time env",
            "stdbuf -oL env",
            "busybox env",
        ] {
            let reason = check(command).unwrap_or_else(|| panic!("allowed: {command}"));
            assert!(reason.contains("environment guard"), "{command}: {reason}");
            assert!(reason.contains("test -n"), "{command}: {reason}");
        }
    }

    /// WI-4061: a credential word is matched as a whole name, and ordinary
    /// commands must not be refused.
    #[test]
    fn wi_4061_allows_names_that_merely_contain_a_credential_word() {
        for command in [
            "echo $MONKEY",
            "printenv MONKEY",
            "printenv KEY_COUNT",
            "printenv SORT_KEYS",
            "echo $KEY_COUNT",
            "echo $SORT_KEYS",
            "env VAR=1 cargo test",
            "git log --grep=environment",
            "cargo test environment",
            "echo $PATH",
            "grep -r env src/",
            "echo \"$TOKEN\" | wc -c",
            "echo \"$TOKEN\" > /dev/null",
        ] {
            assert_eq!(check(command), None, "{command}");
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
