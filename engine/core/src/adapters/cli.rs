//! CLI adapter. Ports `src/vogt/adapters/cli/`.
//!
//! Commands, flags and help text are generated from the operation registry, so
//! the CLI cannot drift from the other surfaces without the parity tests
//! noticing. Flags come from the recorded parameter schemas (the same table
//! MCP advertises as `inputSchema`), not from a second hand-written list.
//!
//! Exit codes match the Python adapter: `0` success, `1` a domain error,
//! `2` usage. A field that holds a secret (`password`, or a name ending in
//! `_password`) never becomes a plain flag: it takes `--<name>-file` and
//! `--<name>-stdin` instead.

use std::collections::BTreeMap;
use std::io::{self, IsTerminal, Write};
use std::path::Path;

use serde_json::{Map, Value};

use crate::errors::VogtError;
use crate::registry::{self, Operation, OperationRegistry};

pub const EXIT_OK: i32 = 0;
pub const EXIT_ERROR: i32 = 1;
pub const EXIT_USAGE: i32 = 2;

const DESCRIPTION: &str = "Vogt — per-repo and estate-wide product development state, \
with provenance and freshness on every answer.";

/// The outcome of one CLI invocation, so tests need not capture streams.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// One parsed invocation: which operation, and the parameter object built from
/// the flags the caller actually set. A flag left off is omitted, so the
/// operation's own default applies rather than `null`.
#[derive(Debug, Clone, PartialEq)]
pub struct Invocation {
    pub operation: &'static str,
    pub params: Value,
    pub json: bool,
    pub data_dir: Option<String>,
}

/// Execute one invocation.
///
/// `dispatch` runs the operation. The adapter does not know which handlers
/// exist: a `not_ported` handler comes back as a domain error (exit 1), the
/// same way any other `VogtError` does. `version` is the string `--version`
/// prints; the Python adapter prints `vogt <version>`.
pub fn run(
    argv: &[String],
    registry: &OperationRegistry,
    version: &str,
    dispatch: &mut dyn FnMut(&Operation, Value) -> Result<Value, VogtError>,
) -> CliResult {
    match parse(argv, registry, version) {
        ParseOutcome::Result(result) => result,
        ParseOutcome::Ready(invocation) => {
            let operation = match registry.get(invocation.operation) {
                Ok(operation) => operation,
                Err(_) => {
                    return usage(format!(
                        "error: unknown operation {}\n",
                        invocation.operation
                    ));
                }
            };
            // Validated here, before the service, so a bad argument is a usage
            // error (exit 2) as argparse-then-pydantic makes it, rather than the
            // domain error `Operation::run` would report. The service validates
            // again for HTTP and MCP, which never pass through here.
            let params = match registry::validate::prepare(operation.name, invocation.params) {
                Ok(params) => params,
                Err(error) => {
                    return CliResult {
                        exit_code: EXIT_USAGE,
                        stdout: String::new(),
                        stderr: format!("error: {}\n", error.message()),
                    };
                }
            };
            match dispatch(operation, params) {
                Ok(result) => {
                    let rendered = if invocation.json {
                        to_json(&result)
                    } else {
                        to_text(&result)
                    };
                    CliResult {
                        exit_code: EXIT_OK,
                        stdout: format!("{rendered}\n"),
                        stderr: String::new(),
                    }
                }
                Err(error) => CliResult {
                    exit_code: EXIT_ERROR,
                    stdout: String::new(),
                    stderr: format!("error: {}: {error}\n", error.code()),
                },
            }
        }
    }
}

enum ParseOutcome {
    Result(CliResult),
    Ready(Invocation),
}

fn ok_out(text: String) -> CliResult {
    CliResult {
        exit_code: EXIT_OK,
        stdout: text,
        stderr: String::new(),
    }
}

/// A usage error. argparse prints the help and the error on stdout, so a bare
/// invocation and a bare group do too. stderr stays empty.
fn usage(text: String) -> CliResult {
    CliResult {
        exit_code: EXIT_USAGE,
        stdout: text,
        stderr: String::new(),
    }
}

/// Parse `argv` (without the program name) against the registry.
fn parse(argv: &[String], registry: &OperationRegistry, version: &str) -> ParseOutcome {
    let argv_rest = argv;
    let mut json = false;
    let mut data_dir: Option<String> = None;
    let mut positional: Vec<String> = Vec::new();
    let mut flags: Vec<String> = Vec::new();

    // Global flags are accepted before the command and after it. Once the
    // command's own flags start, every remaining token belongs to them,
    // including a value that happens to look like a command word.
    let mut index = 0usize;
    let mut command_complete = false;
    while index < argv_rest.len() {
        let flag = &argv_rest[index];
        if !command_complete && (flag == "--help" || flag == "-h") && positional.is_empty() {
            return ParseOutcome::Result(ok_out(format_top(registry)));
        }
        if !command_complete && flag == "--version" && positional.is_empty() {
            return ParseOutcome::Result(ok_out(format!("vogt {version}\n")));
        }
        // A global flag only before the first command word. After `work`,
        // `--json` is that command's flag and a usage error, matching argparse.
        if flag == "--json" && positional.is_empty() {
            json = true;
            index += 1;
            continue;
        }
        if flag == "--data-dir" && positional.is_empty() {
            let Some(value) = argv_rest.get(index + 1) else {
                return ParseOutcome::Result(usage(
                    "error: --data-dir requires a value\n".to_string(),
                ));
            };
            data_dir = Some(value.clone());
            index += 2;
            continue;
        }
        if flag == "--help" || flag == "-h" {
            flags.push(flag.clone());
            index += 1;
            continue;
        }
        if positional.is_empty() && flag.starts_with('-') {
            return ParseOutcome::Result(usage(format!(
                "error: unrecognised argument {flag}\n{}",
                format_top(registry)
            )));
        }
        if !command_complete && !flag.starts_with('-') {
            positional.push(flag.clone());
            command_complete = command_is_complete(&positional, registry);
            index += 1;
            continue;
        }
        if !command_complete && flag.starts_with('-') {
            // A flag between the group word and the subcommand is a root
            // usage error, matching argparse: `work --json get` reports
            // "unrecognized arguments" against the top-level usage.
            return ParseOutcome::Result(usage(format!(
                "{}\nvogt: error: unrecognized arguments: {flag}\n",
                format_top(registry).trim_end()
            )));
        }
        flags.push(flag.clone());
        index += 1;
    }

    if positional.is_empty() {
        // A bare invocation prints the root help and exits 2, on stdout, as
        // argparse does.
        return ParseOutcome::Result(usage(format_top(registry)));
    }
    let operations = cli_operations(registry);
    let (operation, consumed) = match resolve_command(&positional, &operations) {
        CommandMatch::Operation { name, depth } => {
            let Some(operation) = registry.iter().find(|operation| operation.name == name) else {
                return ParseOutcome::Result(usage(format!("error: unknown operation {name}\n")));
            };
            (operation, depth)
        }
        CommandMatch::None => {
            return ParseOutcome::Result(usage(format!(
                "error: unknown command '{}'\n{}",
                positional[0],
                format_top(registry)
            )));
        }
        CommandMatch::Group { path } => {
            // `--help` on a group, or a bare group name, prints the group.
            let show = positional.len() == path.len()
                || flags
                    .first()
                    .is_some_and(|token| token == "--help" || token == "-h");
            if show && flags.is_empty() {
                // A bare group is a usage error (exit 2) and prints the root
                // help, which is what argparse does for an incomplete command.
                return ParseOutcome::Result(usage(format_top(registry)));
            }
            // A flag right after the group word is a root-level usage error,
            // not the operation's help: argparse reports "unrecognized
            // arguments" against the top-level usage.
            if let Some(flag) = flags.first().filter(|flag| flag.starts_with('-')) {
                return ParseOutcome::Result(usage(format!(
                    "{}\nvogt: error: unrecognized arguments: {flag}\n",
                    format_top(registry).trim_end()
                )));
            }
            let unknown = positional
                .get(path.len())
                .map(String::as_str)
                .unwrap_or(flags.first().map(String::as_str).unwrap_or(""));
            return ParseOutcome::Result(usage(format!(
                "error: unknown command '{unknown}'\n{}",
                format_group(registry, &path)
            )));
        }
    };

    if consumed < positional.len() {
        return ParseOutcome::Result(usage(format!(
            "error: unexpected argument {}\n{}",
            positional[consumed],
            format_operation(operation)
        )));
    }
    let flags = flags.as_slice();
    if flags.iter().any(|flag| flag == "--help" || flag == "-h") {
        return ParseOutcome::Result(ok_out(format_operation(operation)));
    }

    let schema = registry::params_schema_for(operation.name).cloned().unwrap_or_else(|| {
        serde_json::json!({"type": "object", "properties": {}, "additionalProperties": false})
    });
    match collect_params(flags, &schema) {
        Ok(params) => ParseOutcome::Ready(Invocation {
            operation: operation.name,
            params,
            json,
            data_dir,
        }),
        Err(message) if message.starts_with("root: ") => ParseOutcome::Result(usage(format!(
            "{}\nvogt: error: {}\n",
            format_top(registry).trim_end(),
            message.trim_start_matches("root: ")
        ))),
        Err(message) if message.starts_with("domain: ") => ParseOutcome::Result(CliResult {
            exit_code: EXIT_ERROR,
            stdout: String::new(),
            stderr: format!("{}\n", message.trim_start_matches("domain: ")),
        }),
        Err(message) => ParseOutcome::Result(usage(format!(
            "error: {message}\n{}",
            format_operation(operation)
        ))),
    }
}

/// True once `words` is itself an operation. A group word (`work`) is not
/// finished: the subcommand still follows, and a flag after it is a usage
/// error only once the whole command has been seen.
fn command_is_complete(words: &[String], registry: &OperationRegistry) -> bool {
    cli_operations(registry).iter().any(|op| op.path == words)
}

struct CliOp {
    path: Vec<String>,
    name: &'static str,
    summary: &'static str,
}

fn cli_operations(registry: &OperationRegistry) -> Vec<CliOp> {
    registry
        .for_transport(registry::Transport::Cli)
        .into_iter()
        .map(|operation| CliOp {
            path: operation
                .cli
                .path
                .iter()
                .map(|part| (*part).to_string())
                .collect(),
            name: operation.name,
            summary: operation.summary,
        })
        .collect()
}

enum CommandMatch {
    None,
    Group { path: Vec<String> },
    Operation { name: &'static str, depth: usize },
}

/// Longest command path that is a prefix of `argv`. A path that is a strict
/// prefix of some operation and not itself an operation is a group.
fn resolve_command(argv: &[String], operations: &[CliOp]) -> CommandMatch {
    if argv.is_empty() {
        return CommandMatch::None;
    }
    let mut best: Option<&CliOp> = None;
    let mut best_depth = 0usize;
    let mut group: Option<Vec<String>> = None;
    for op in operations {
        if argv.len() < op.path.len()
            && argv
                .iter()
                .zip(op.path.iter())
                .all(|(got, want)| got == want)
            && group.is_none()
        {
            group = Some(argv.to_vec());
        }
        if argv.len() >= op.path.len()
            && argv[..op.path.len()] == op.path[..]
            && op.path.len() > best_depth
        {
            best = Some(op);
            best_depth = op.path.len();
        }
    }
    if let Some(op) = best {
        return CommandMatch::Operation {
            name: op.name,
            depth: best_depth,
        };
    }
    // A prefix of a longer command, with nothing left that matches, is still a
    // group when every token matched some operation's leading path.
    if let Some(path) = group {
        return CommandMatch::Group { path };
    }
    // The first token matches no command at all.
    let head = &argv[0];
    if operations
        .iter()
        .any(|op| op.path.first().is_some_and(|part| part == head))
    {
        return CommandMatch::Group {
            path: vec![head.clone()],
        };
    }
    CommandMatch::None
}

/// Read flags into the parameter object. Unknown flags and missing required
/// fields are usage errors. A secret field is read from `--<name>-file` or
/// `--<name>-stdin`, never from a plain flag.
fn collect_params(argv: &[String], schema: &Value) -> Result<Value, String> {
    let properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let required: Vec<String> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();

    let mut values: Map<String, Value> = Map::new();
    let mut secret_files: BTreeMap<String, String> = BTreeMap::new();
    let mut secret_sources: BTreeMap<String, String> = BTreeMap::new();
    let mut index = 0usize;
    while index < argv.len() {
        let token = &argv[index];
        if token == "--" {
            return Err("unexpected argument".to_string());
        }
        if !token.starts_with("--") {
            return Err(format!("root: unrecognized arguments: {token}"));
        }
        let (name, inline) = match token.split_once('=') {
            Some((flag, value)) => (flag.trim_start_matches("--"), Some(value.to_string())),
            None => (token.trim_start_matches("--"), None),
        };
        let field = name.replace('-', "_");

        if is_secret(&field) && properties.contains_key(&field) {
            return Err(format!(
                "refusing --{name}: pass --{name}-file or --{name}-stdin, never the value in argv"
            ));
        }
        if let Some(secret) = secret_name(&field) {
            if !properties.contains_key(secret) {
                return Err(format!("root: unrecognized arguments: --{name}"));
            }
            let source = if field.ends_with("_stdin") {
                "stdin"
            } else {
                "file"
            };
            if let Some(previous) = secret_sources.get(secret) {
                if previous != source {
                    let (first, second) = if source == "stdin" {
                        (format!("--{secret}-stdin"), format!("--{secret}-file"))
                    } else {
                        (format!("--{secret}-file"), format!("--{secret}-stdin"))
                    };
                    return Err(format!(
                        "root: argument {first}: not allowed with argument {second}"
                    ));
                }
            }
            secret_sources.insert(secret.to_string(), source.to_string());
            if field.ends_with("_stdin") {
                if inline.is_some() {
                    return Err(format!("--{name} takes no value"));
                }
                index += 1;
                // A repeated secret flag is the last one, matching argparse.
                values.insert(secret.to_string(), Value::String(read_secret_stdin()?));
                continue;
            }
            let value = take_value(argv, &mut index, inline)?;
            // Only the last file is read. An earlier missing file is ignored,
            // matching argparse, which keeps the final value.
            secret_files.insert(secret.to_string(), value);
            continue;
        }

        // `--no-<flag>` is the off switch for a boolean that is not already
        // phrased as a negative, matching argparse's BooleanOptionalAction.
        let (property_name, forced_bool) = if let Some(base) = field.strip_prefix("no_") {
            if !properties.contains_key(&field)
                && properties.contains_key(base)
                && is_bool(&properties[base])
            {
                (base.to_string(), Some(false))
            } else {
                (field.clone(), None)
            }
        } else {
            (field.clone(), None)
        };
        let Some(property) = properties.get(&property_name) else {
            return Err(format!("unrecognised argument --{name}"));
        };
        if is_bool(property) {
            let value = match (forced_bool, inline) {
                (Some(value), None) => value,
                (Some(_), Some(_)) => {
                    return Err(format!("--{name} takes no value"));
                }
                (None, Some(text)) => parse_bool_text(&text)?,
                (None, None) => true,
            };
            values.insert(property_name, Value::Bool(value));
            index += 1;
            continue;
        }
        let raw = take_value(argv, &mut index, inline)?;
        let parsed =
            coerce(&raw, property).map_err(|message| format!("argument --{name}: {message}"))?;
        if is_list(property) {
            let entry = values
                .entry(field)
                .or_insert_with(|| Value::Array(Vec::new()));
            entry.as_array_mut().expect("list value").push(parsed);
        } else {
            values.insert(field, parsed);
        }
    }
    for (secret, path) in &secret_files {
        values.insert(secret.clone(), Value::String(read_secret_file(path)?));
    }

    for name in &required {
        if is_secret(name) {
            if !values.contains_key(name) {
                let dashed = name.replace('_', "-");
                // Off a terminal there is no prompt, so a missing secret is a
                // domain error (exit 1), not a usage error.
                if !io::stdin().is_terminal() {
                    return Err(format!(
                        "domain: --{dashed}-file or --{dashed}-stdin is required when not on a terminal"
                    ));
                }
                return Err(format!("--{dashed}-file or --{dashed}-stdin is required"));
            }
            continue;
        }
        if !values.contains_key(name) {
            return Err(format!("--{} is required", name.replace('_', "-")));
        }
    }
    Ok(Value::Object(values))
}

fn take_value(
    argv: &[String],
    index: &mut usize,
    inline: Option<String>,
) -> Result<String, String> {
    if let Some(value) = inline {
        *index += 1;
        return Ok(value);
    }
    let next = argv.get(*index + 1).ok_or_else(|| {
        format!(
            "--{} requires a value",
            argv[*index].trim_start_matches("--")
        )
    })?;
    // A negative number is a value, not a flag. argparse's
    // `_negative_number_matcher` does the same when no option looks like a
    // negative number, which none of ours do. `-5` and `-5.5` are values;
    // `--foo` and `-h` are not.
    if next.starts_with('-') && next != "-" && !is_negative_number(next) {
        return Err(format!(
            "--{} requires a value",
            argv[*index].trim_start_matches("--")
        ));
    }
    *index += 2;
    Ok(next.clone())
}

fn is_negative_number(token: &str) -> bool {
    let rest = token.strip_prefix('-').unwrap_or("");
    let mut seen_dot = false;
    let mut digits = 0usize;
    for ch in rest.chars() {
        match ch {
            '0'..='9' => digits += 1,
            '.' if !seen_dot => seen_dot = true,
            _ => return false,
        }
    }
    digits > 0
}

fn secret_name(field: &str) -> Option<&str> {
    for suffix in ["_file", "_stdin"] {
        if let Some(base) = field.strip_suffix(suffix) {
            if is_secret(base) {
                return Some(base);
            }
        }
    }
    None
}

fn is_secret(name: &str) -> bool {
    name == "password" || name.ends_with("_password")
}

fn read_secret_file(path: &str) -> Result<String, String> {
    let text =
        std::fs::read_to_string(path).map_err(|_| format!("cannot read secret file {path}"))?;
    Ok(text.trim_end_matches(['\r', '\n']).to_string())
}

fn read_secret_stdin() -> Result<String, String> {
    let mut line = String::new();
    io::stdin()
        .read_line(&mut line)
        .map_err(|_| "cannot read secret from stdin".to_string())?;
    Ok(line.trim_end_matches(['\r', '\n']).to_string())
}

fn is_bool(property: &Value) -> bool {
    schema_types(property).iter().any(|kind| kind == "boolean")
        && !schema_types(property).iter().any(|kind| kind == "array")
}

fn is_list(property: &Value) -> bool {
    schema_types(property).iter().any(|kind| kind == "array")
}

fn schema_types(property: &Value) -> Vec<String> {
    match property.get("type") {
        Some(Value::String(kind)) => vec![kind.clone()],
        Some(Value::Array(kinds)) => kinds
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
        _ => {
            // anyOf: string | null, and friends.
            property
                .get("anyOf")
                .and_then(Value::as_array)
                .map(|options| {
                    options
                        .iter()
                        .filter_map(|option| option.get("type").and_then(Value::as_str))
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default()
        }
    }
}

fn parse_bool_text(text: &str) -> Result<bool, String> {
    match text {
        "true" | "True" | "1" | "yes" => Ok(true),
        "false" | "False" | "0" | "no" => Ok(false),
        _ => Err(format!("expected a boolean, got {text}")),
    }
}

fn coerce(raw: &str, property: &Value) -> Result<Value, String> {
    if let Some(allowed) = enum_choices(property) {
        if !allowed.iter().any(|choice| choice == raw) {
            return Err(format!(
                "invalid choice {raw}; choose from {}",
                allowed.join(", ")
            ));
        }
        return Ok(Value::String(raw.to_string()));
    }
    let types = schema_types(property);
    if types.iter().any(|kind| kind == "integer") {
        let number: i64 = raw
            .parse()
            .map_err(|_| format!("invalid int value: '{raw}'"))?;
        return Ok(Value::from(number));
    }
    if types.iter().any(|kind| kind == "number") {
        let number: f64 = raw
            .parse()
            .map_err(|_| format!("expected a number, got {raw}"))?;
        return Ok(Value::from(number));
    }
    if types.iter().any(|kind| kind == "array") {
        // A list of objects is one JSON value per repeat, matching the Python
        // CLI. A list of scalars is one element per repeat.
        let items = property.get("items").cloned().unwrap_or(Value::Null);
        if is_json_value(&items) {
            return serde_json::from_str(raw).map_err(|_| format!("expected JSON, got {raw}"));
        }
        return coerce(raw, &items);
    }
    // Only an object-typed field is JSON. A string that happens to start with
    // `{` (`--title '{wip} fix'`) stays a string.
    if is_json_value(property) {
        return serde_json::from_str(raw).map_err(|_| format!("expected JSON, got {raw}"));
    }
    Ok(Value::String(raw.to_string()))
}

/// Choices declared as `enum` or, for `Optional[Literal]`, inside `anyOf`.
fn enum_choices(property: &Value) -> Option<Vec<String>> {
    if let Some(choices) = property.get("enum").and_then(Value::as_array) {
        let allowed: Vec<String> = choices
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect();
        if !allowed.is_empty() {
            return Some(allowed);
        }
    }
    let options = property.get("anyOf").and_then(Value::as_array)?;
    for option in options {
        if let Some(choices) = option.get("enum").and_then(Value::as_array) {
            let allowed: Vec<String> = choices
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect();
            if !allowed.is_empty() {
                return Some(allowed);
            }
        }
    }
    None
}

fn is_json_value(property: &Value) -> bool {
    let types = schema_types(property);
    types.iter().any(|kind| kind == "object" || kind == "array")
}

fn format_top(registry: &OperationRegistry) -> String {
    let mut out = String::new();
    out.push_str("usage: vogt [-h] [--version] [--data-dir DATA_DIR] [--json] <command> ...\n\n");
    out.push_str(DESCRIPTION);
    out.push_str("\n\npositional arguments:\n");
    for (name, summary) in top_commands(registry) {
        out.push_str(&format!("  {name:<22}{summary}\n"));
    }
    out.push_str("\noptions:\n");
    out.push_str("  -h, --help           show this help message and exit\n");
    out.push_str("  --version            show program's version number and exit\n");
    out.push_str("  --data-dir DATA_DIR  Instance data directory (overrides VOGT_DATA_DIR).\n");
    out.push_str("  --json               Emit the raw result as JSON instead of formatted text.\n");
    out
}

fn top_commands(registry: &OperationRegistry) -> Vec<(String, String)> {
    let mut seen = BTreeMap::<String, String>::new();
    let mut order: Vec<String> = Vec::new();
    for op in cli_operations(registry) {
        let head = op.path[0].clone();
        if seen.contains_key(&head) {
            continue;
        }
        let summary = if op.path.len() == 1 {
            op.summary.to_string()
        } else {
            format!("{head} operations")
        };
        seen.insert(head.clone(), summary);
        order.push(head);
    }
    order
        .into_iter()
        .map(|name| {
            let summary = seen.remove(&name).unwrap_or_default();
            (name, summary)
        })
        .collect()
}

fn format_group(registry: &OperationRegistry, path: &[String]) -> String {
    let mut out = String::new();
    let joined = path.join(" ");
    out.push_str(&format!("usage: vogt {joined} <subcommand> ...\n\n"));
    out.push_str(&format!("{joined} operations\n\nsubcommands:\n"));
    let mut shown = BTreeMap::<String, String>::new();
    let mut order = Vec::new();
    for op in cli_operations(registry) {
        if op.path.len() <= path.len() || op.path[..path.len()] != *path {
            continue;
        }
        let name = op.path[path.len()].clone();
        if shown.contains_key(&name) {
            continue;
        }
        let summary = if op.path.len() == path.len() + 1 {
            op.summary.to_string()
        } else {
            format!("{name} operations")
        };
        shown.insert(name.clone(), summary);
        order.push(name);
    }
    for name in order {
        let summary = &shown[&name];
        out.push_str(&format!("  {name:<22}{summary}\n"));
    }
    out
}

fn format_operation(operation: &Operation) -> String {
    let path = operation.cli.path.join(" ");
    let schema = registry::params_schema_for(operation.name);
    let mut out = String::new();
    out.push_str(&format!("usage: vogt {path}"));
    if let Some(schema) = schema {
        for flag in usage_flags(schema) {
            out.push_str(&format!(" {flag}"));
        }
    }
    out.push_str("\n\n");
    if !operation.summary.is_empty() {
        out.push_str(operation.summary);
        out.push_str("\n\n");
    }
    out.push_str("options:\n");
    out.push_str("  -h, --help            show this help message and exit\n");
    if let Some(schema) = schema {
        for (flag, help) in flag_help(schema) {
            if help.is_empty() {
                out.push_str(&format!("  {flag}\n"));
            } else if flag.len() > 20 {
                // argparse wraps a flag whose column is already full onto the
                // next line rather than running the text into the description.
                out.push_str(&format!("  {flag}\n                        {help}\n"));
            } else {
                out.push_str(&format!("  {flag:<22}{help}\n"));
            }
        }
    }
    out
}

fn usage_flags(schema: &Value) -> Vec<String> {
    let required = required_fields(schema);
    flag_help(schema)
        .into_iter()
        .map(|(flag, _)| {
            let name = flag.split_whitespace().next().unwrap_or("");
            let field = name.trim_start_matches("--").replace('-', "_");
            let base = field
                .strip_suffix("_file")
                .or_else(|| field.strip_suffix("_stdin"))
                .unwrap_or(&field);
            if required.iter().any(|item| item == base) {
                flag
            } else {
                format!("[{flag}]")
            }
        })
        .collect()
}

fn required_fields(schema: &Value) -> Vec<String> {
    schema
        .get("required")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn flag_help(schema: &Value) -> Vec<(String, String)> {
    let properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut lines = Vec::new();
    for (name, property) in &properties {
        let dashed = name.replace('_', "-");
        if is_secret(name) {
            lines.push((
                format!("--{dashed}-file PATH"),
                format!("Read the {dashed} from this file (never pass it in argv)."),
            ));
            lines.push((
                format!("--{dashed}-stdin"),
                format!("Read the {dashed} from standard input."),
            ));
            continue;
        }
        let help = property
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if is_bool(property) {
            if dashed.starts_with("no-") {
                lines.push((format!("--{dashed}"), help));
            } else {
                lines.push((format!("--{dashed}, --no-{dashed}"), help));
            }
            continue;
        }
        let metavar = flag_metavar(name, property);
        lines.push((format!("--{dashed} {metavar}"), help));
    }
    lines
}

/// The metavar argparse prints: `{a,b}` for a choice, the uppercased field
/// name otherwise. A choice is what pushes `--order {started,rss}` past the
/// help column.
fn flag_metavar(name: &str, property: &Value) -> String {
    if let Some(choices) = enum_choices(property) {
        return format!("{{{}}}", choices.join(","));
    }
    name.to_uppercase()
}
/// Human rendering, porting `render.py`: a mapping is `key: value` lines, an
/// empty list is `(none)`, a boolean is `yes`/`no`, and `null` is `-`.
fn to_text(value: &Value) -> String {
    let mut lines = Vec::new();
    render_value(value, &mut lines, 0);
    lines.join("\n")
}

fn render_value(value: &Value, lines: &mut Vec<String>, indent: usize) {
    let pad = "  ".repeat(indent);
    match value {
        Value::Object(map) => {
            for (key, item) in map {
                match item {
                    Value::Object(_) => {
                        lines.push(format!("{pad}{key}:"));
                        render_value(item, lines, indent + 1);
                    }
                    Value::Array(items) if items.is_empty() => {
                        lines.push(format!("{pad}{key}: (none)"));
                    }
                    Value::Array(_) => {
                        lines.push(format!("{pad}{key}:"));
                        render_value(item, lines, indent + 1);
                    }
                    other => lines.push(format!("{pad}{key}: {}", scalar(other))),
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                if item.is_object() {
                    lines.push(format!("{pad}-"));
                    render_value(item, lines, indent + 1);
                } else {
                    lines.push(format!("{pad}- {}", scalar(item)));
                }
            }
        }
        other => lines.push(format!("{pad}{}", scalar(other))),
    }
}

fn scalar(value: &Value) -> String {
    match value {
        Value::Null => "-".to_string(),
        Value::Bool(true) => "yes".to_string(),
        Value::Bool(false) => "no".to_string(),
        Value::String(text) => text.clone(),
        Value::Number(number) => number.to_string(),
        other => other.to_string(),
    }
}

fn to_json(value: &Value) -> String {
    // Python `json.dumps(..., indent=2)`: two-space indent, `": "` and
    // `ensure_ascii`. Kept here rather than in `decisions` so the digest
    // renderer and this layout can change independently.
    python_json_indent(value)
}

fn python_json_indent(value: &Value) -> String {
    let mut out = String::new();
    write_indent(&mut out, value, 0);
    out
}

fn write_indent(out: &mut String, value: &Value, indent: usize) {
    let pad = "  ".repeat(indent);
    let inner = "  ".repeat(indent + 1);
    match value {
        Value::Array(items) if !items.is_empty() => {
            out.push_str("[\n");
            for (index, item) in items.iter().enumerate() {
                out.push_str(&inner);
                write_indent(out, item, indent + 1);
                if index + 1 != items.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            out.push_str(&pad);
            out.push(']');
        }
        Value::Object(map) if !map.is_empty() => {
            out.push_str("{\n");
            for (index, (key, item)) in map.iter().enumerate() {
                out.push_str(&inner);
                out.push_str(&crate::decisions::python_json_dumps(
                    &Value::String(key.clone()),
                    false,
                ));
                out.push_str(": ");
                write_indent(out, item, indent + 1);
                if index + 1 != map.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            out.push_str(&pad);
            out.push('}');
        }
        other => out.push_str(&crate::decisions::python_json_dumps(other, false)),
    }
}

/// Console entry used by `main`. Writes the result and returns the exit code.
pub fn main_cli(
    argv: &[String],
    registry: &OperationRegistry,
    version: &str,
    dispatch: &mut dyn FnMut(&Operation, Value) -> Result<Value, VogtError>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> i32 {
    let result = run(argv, registry, version, dispatch);
    let _ = stdout.write_all(result.stdout.as_bytes());
    let _ = stderr.write_all(result.stderr.as_bytes());
    result.exit_code
}

/// `--data-dir` from a parsed invocation, for the binary to thread into config.
pub fn data_dir_of(invocation_argv: &[String]) -> Option<String> {
    let mut rest = invocation_argv;
    while let [flag, value, tail @ ..] = rest {
        if flag == "--data-dir" {
            return Some(value.clone());
        }
        if flag == "--json" || flag == "--help" || flag == "-h" || flag == "--version" {
            rest = &rest[1..];
            continue;
        }
        if flag.starts_with('-') {
            rest = if value.starts_with('-') {
                &rest[1..]
            } else {
                tail
            };
            continue;
        }
        break;
    }
    None
}

/// Refuse a path that escapes the process, used by tests that point
/// `--password-file` at a fixture. Not a sandbox: the operator owns the box.
#[allow(dead_code)]
pub fn secret_path_is_file(path: &str) -> bool {
    Path::new(path).is_file()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::default_registry;

    fn no_dispatch(operation: &Operation, _params: Value) -> Result<Value, VogtError> {
        // The registry's own answer for a service that has not landed.
        operation.run(None, Value::Null)?;
        Ok(Value::Null)
    }

    fn argv(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| (*arg).to_string()).collect()
    }

    #[test]
    fn help_lists_every_cli_command_group() {
        let registry = default_registry();
        let result = run(&argv(&["--help"]), &registry, "test", &mut no_dispatch);
        assert_eq!(result.exit_code, EXIT_OK);
        assert!(result.stdout.contains("usage: vogt"));
        assert!(result.stdout.contains("positional arguments"));
        assert!(result.stdout.contains("--json"));
        assert!(result.stdout.contains("--data-dir"));
    }

    #[test]
    fn version_exits_zero() {
        let registry = default_registry();
        let result = run(&argv(&["--version"]), &registry, "9.9.9", &mut no_dispatch);
        assert_eq!(result.exit_code, EXIT_OK);
        assert_eq!(result.stdout, "vogt 9.9.9\n");
    }

    #[test]
    fn unknown_command_is_usage() {
        let registry = default_registry();
        let result = run(&argv(&["nope"]), &registry, "test", &mut no_dispatch);
        assert_eq!(result.exit_code, EXIT_USAGE);
        assert!(result.stdout.contains("unknown command"));
    }

    #[test]
    fn operation_help_shows_schema_flags() {
        let registry = default_registry();
        let result = run(
            &argv(&["work", "get", "--help"]),
            &registry,
            "test",
            &mut no_dispatch,
        );
        assert_eq!(result.exit_code, EXIT_OK, "{}", result.stderr);
        assert!(result.stdout.contains("--ref"), "{}", result.stdout);
    }

    #[test]
    fn missing_required_flag_is_usage() {
        let registry = default_registry();
        let result = run(&argv(&["work", "get"]), &registry, "test", &mut no_dispatch);
        assert_eq!(
            result.exit_code, EXIT_USAGE,
            "STDERR={:?} STDOUT={}",
            result.stderr, result.stdout
        );
        assert!(
            result.stdout.contains("required"),
            "STDERR={:?} STDOUT={}",
            result.stderr,
            result.stdout
        );
    }

    #[test]
    fn parsed_params_omit_unset_flags_and_coerce_types() {
        let registry = default_registry();
        let mut seen: Option<Value> = None;
        let mut dispatch = |_operation: &Operation, params: Value| {
            seen = Some(params);
            Ok(serde_json::json!({"ok": true}))
        };
        let result = run(
            &argv(&[
                "--json",
                "work",
                "get",
                "--ref",
                "WI-7",
                "--comment-limit",
                "3",
            ]),
            &registry,
            "test",
            &mut dispatch,
        );
        assert_eq!(result.exit_code, EXIT_OK, "{}", result.stderr);
        assert!(result.stdout.contains("\"ok\": true"), "{}", result.stdout);
        assert!(
            result.stdout.contains('\n'),
            "indent=2, got {}",
            result.stdout
        );
        let params = seen.expect("dispatched");
        assert_eq!(params["ref"], "WI-7");
        assert_eq!(params["comment_limit"], 3);
        assert!(params.get("comment_offset").is_none());
    }

    #[test]
    fn a_domain_error_exits_one() {
        let registry = default_registry();
        let result = run(
            &argv(&["work", "get", "--ref", "WI-7"]),
            &registry,
            "test",
            &mut no_dispatch,
        );
        assert_eq!(result.exit_code, EXIT_ERROR);
        assert!(
            result.stderr.contains("invalid_request"),
            "{}",
            result.stderr
        );
        assert!(result.stderr.contains("not been ported"));
    }

    #[test]
    fn secret_file_is_read_and_a_plain_password_flag_is_refused() {
        let registry = default_registry();
        let help = run(
            &argv(&["user", "create", "--help"]),
            &registry,
            "test",
            &mut no_dispatch,
        );
        assert_eq!(help.exit_code, EXIT_OK, "{}", help.stderr);
        assert!(help.stdout.contains("--password-file"), "{}", help.stdout);
        assert!(help.stdout.contains("--password-stdin"), "{}", help.stdout);
        assert!(!help.stdout.contains("--password PASSWORD"));

        let refused = run(
            &argv(&[
                "user",
                "create",
                "--username",
                "ada",
                "--password",
                "secret",
                "--reason",
                "because",
            ]),
            &registry,
            "test",
            &mut no_dispatch,
        );
        assert_eq!(refused.exit_code, EXIT_USAGE, "{}", refused.stderr);

        let path = std::env::temp_dir().join("vogt-cli-secret-test");
        std::fs::write(&path, "s3cret\n").unwrap();
        let mut seen: Option<Value> = None;
        let mut dispatch = |_operation: &Operation, params: Value| {
            seen = Some(params);
            Ok(Value::Null)
        };
        let result = run(
            &argv(&[
                "user",
                "create",
                "--username",
                "ada",
                "--password-file",
                path.to_str().unwrap(),
                "--reason",
                "because",
            ]),
            &registry,
            "test",
            &mut dispatch,
        );
        let _ = std::fs::remove_file(&path);
        assert_eq!(result.exit_code, EXIT_OK, "{}", result.stderr);
        assert_eq!(seen.expect("dispatched")["password"], "s3cret");
    }

    #[test]
    fn bool_and_enum_flags_follow_the_schema() {
        let registry = default_registry();
        let mut seen: Option<Value> = None;
        let mut dispatch = |_operation: &Operation, params: Value| {
            seen = Some(params);
            Ok(Value::Null)
        };
        let result = run(
            &argv(&[
                "work",
                "create",
                "--kind",
                "bug",
                "--title",
                "x",
                "--reason",
                "because",
                "--local-only",
            ]),
            &registry,
            "test",
            &mut dispatch,
        );
        assert_eq!(result.exit_code, EXIT_OK, "{}", result.stderr);
        let params = seen.expect("dispatched");
        assert_eq!(params["kind"], "bug");
        assert_eq!(params["local_only"], true);
    }

    #[test]
    fn a_brace_in_a_string_field_stays_a_string() {
        let registry = default_registry();
        let mut seen: Option<Value> = None;
        let mut dispatch = |_operation: &Operation, params: Value| {
            seen = Some(params);
            Ok(Value::Null)
        };
        let result = run(
            &argv(&[
                "work",
                "create",
                "--kind",
                "bug",
                "--title",
                "{wip} fix",
                "--reason",
                "because",
            ]),
            &registry,
            "test",
            &mut dispatch,
        );
        assert_eq!(result.exit_code, EXIT_OK, "{}", result.stderr);
        assert_eq!(seen.expect("dispatched")["title"], "{wip} fix");
    }

    #[test]
    fn an_optional_literal_rejects_a_value_outside_its_choices() {
        let registry = default_registry();
        let result = run(
            &argv(&[
                "work", "update", "--ref", "WI-7", "--effort", "huge", "--reason", "because",
            ]),
            &registry,
            "test",
            &mut no_dispatch,
        );
        assert_eq!(result.exit_code, EXIT_USAGE, "{}", result.stdout);
        assert!(
            result.stdout.contains("invalid choice"),
            "{}",
            result.stderr
        );
    }

    #[test]
    fn a_missing_secret_off_a_terminal_exits_one() {
        let registry = default_registry();
        let result = run(
            &argv(&["user", "create", "--username", "ada", "--reason", "because"]),
            &registry,
            "test",
            &mut no_dispatch,
        );
        assert_eq!(result.exit_code, EXIT_ERROR, "{}", result.stderr);
        assert!(
            result.stderr.contains("when not on a terminal"),
            "{}",
            result.stderr
        );
        assert!(!result.stderr.starts_with("error:"), "{}", result.stderr);
    }

    #[test]
    fn a_global_flag_after_a_group_word_is_usage() {
        let registry = default_registry();
        let result = run(
            &argv(&["work", "--json", "get", "--ref", "WI-7"]),
            &registry,
            "test",
            &mut no_dispatch,
        );
        assert_eq!(result.exit_code, EXIT_USAGE, "{}", result.stdout);
        assert!(
            result.stdout.contains("unrecognized arguments: --json"),
            "{}",
            result.stdout
        );
    }

    #[test]
    fn a_repeated_secret_file_keeps_the_last_value() {
        let registry = default_registry();
        let first = std::env::temp_dir().join("vogt-cli-secret-a");
        let second = std::env::temp_dir().join("vogt-cli-secret-b");
        std::fs::write(&first, "one\n").unwrap();
        std::fs::write(&second, "two\n").unwrap();
        let mut seen: Option<Value> = None;
        let mut dispatch = |_operation: &Operation, params: Value| {
            seen = Some(params);
            Ok(Value::Null)
        };
        let result = run(
            &argv(&[
                "user",
                "create",
                "--username",
                "ada",
                "--password-file",
                first.to_str().unwrap(),
                "--password-file",
                second.to_str().unwrap(),
                "--reason",
                "because",
            ]),
            &registry,
            "test",
            &mut dispatch,
        );
        let _ = std::fs::remove_file(&first);
        let _ = std::fs::remove_file(&second);
        assert_eq!(result.exit_code, EXIT_OK, "{}", result.stderr);
        assert_eq!(seen.expect("dispatched")["password"], "two");
    }

    #[test]
    fn an_earlier_missing_secret_file_is_ignored_when_a_later_one_exists() {
        let registry = default_registry();
        let second = std::env::temp_dir().join("vogt-cli-secret-last");
        std::fs::write(&second, "kept\n").unwrap();
        let mut seen: Option<Value> = None;
        let mut dispatch = |_operation: &Operation, params: Value| {
            seen = Some(params);
            Ok(Value::Null)
        };
        let result = run(
            &argv(&[
                "user",
                "create",
                "--username",
                "ada",
                "--password-file",
                "/nonexistent/secret",
                "--password-file",
                second.to_str().unwrap(),
                "--reason",
                "because",
            ]),
            &registry,
            "test",
            &mut dispatch,
        );
        let _ = std::fs::remove_file(&second);
        assert_eq!(result.exit_code, EXIT_OK, "{}", result.stderr);
        assert_eq!(seen.expect("dispatched")["password"], "kept");
    }

    #[test]
    fn a_secret_file_and_stdin_together_are_usage() {
        let registry = default_registry();
        let result = run(
            &argv(&[
                "user",
                "create",
                "--username",
                "ada",
                "--password-file",
                "/tmp/x",
                "--password-stdin",
                "--reason",
                "because",
            ]),
            &registry,
            "test",
            &mut no_dispatch,
        );
        assert_eq!(result.exit_code, EXIT_USAGE, "{}", result.stdout);
        assert!(
            result.stdout.contains("not allowed with"),
            "{}",
            result.stderr
        );
    }

    #[test]
    fn text_mode_renders_like_python() {
        let registry = default_registry();
        let mut dispatch = |_operation: &Operation, _params: Value| {
            Ok(serde_json::json!({"name": "ada", "active": true, "items": []}))
        };
        let result = run(
            &argv(&["registry", "dump"]),
            &registry,
            "test",
            &mut dispatch,
        );
        assert_eq!(result.exit_code, EXIT_OK, "{}", result.stderr);
        assert!(result.stdout.contains("name: ada"), "{}", result.stdout);
        assert!(result.stdout.contains("active: yes"), "{}", result.stdout);
        assert!(result.stdout.contains("items: (none)"), "{}", result.stdout);
    }

    #[test]
    fn optional_flags_are_bracketed_and_a_bad_int_names_the_flag() {
        let registry = default_registry();
        let help = run(
            &argv(&["token", "issue", "--help"]),
            &registry,
            "test",
            &mut no_dispatch,
        );
        assert!(help.stdout.contains("[--scopes"), "{}", help.stdout);
        assert!(
            help.stdout.contains("[--expires-in-days"),
            "{}",
            help.stdout
        );
        assert!(help.stdout.contains("--actor"), "{}", help.stdout);
        let bad = run(
            &argv(&[
                "token",
                "issue",
                "--actor",
                "a",
                "--name",
                "t",
                "--reason",
                "r",
                "--expires-in-days",
                "-1.5",
            ]),
            &registry,
            "test",
            &mut no_dispatch,
        );
        assert_eq!(bad.exit_code, EXIT_USAGE, "{}", bad.stdout);
        assert!(
            bad.stdout.contains("invalid int value: '-1.5'"),
            "{}",
            bad.stdout
        );
    }

    #[test]
    fn a_negative_number_is_a_value_not_a_flag() {
        let registry = default_registry();
        let mut seen: Option<Value> = None;
        let mut dispatch = |_operation: &Operation, params: Value| {
            seen = Some(params);
            Ok(Value::Null)
        };
        let result = run(
            &argv(&[
                "token",
                "issue",
                "--actor",
                "local:a",
                "--name",
                "t",
                "--reason",
                "because",
                "--expires-in-days",
                "-5",
            ]),
            &registry,
            "test",
            &mut dispatch,
        );
        assert_eq!(result.exit_code, EXIT_OK, "{}", result.stdout);
        assert_eq!(seen.expect("dispatched")["expires_in_days"], -5);
    }

    #[test]
    fn data_dir_before_the_command_is_not_the_command() {
        let registry = default_registry();
        let mut seen = false;
        let mut dispatch = |_operation: &Operation, _params: Value| {
            seen = true;
            Ok(Value::Null)
        };
        let result = run(
            &argv(&["--data-dir", "/tmp/x9", "registry", "dump"]),
            &registry,
            "test",
            &mut dispatch,
        );
        assert_eq!(result.exit_code, EXIT_OK, "{}", result.stdout);
        assert!(seen, "the command ran");
        assert!(!result.stdout.contains("unknown command"));
    }

    #[test]
    fn a_global_flag_after_the_command_is_usage() {
        let registry = default_registry();
        let result = run(
            &argv(&["registry", "dump", "--data-dir", "/tmp/nope"]),
            &registry,
            "test",
            &mut no_dispatch,
        );
        assert_eq!(result.exit_code, EXIT_USAGE, "{}", result.stdout);
    }

    #[test]
    fn a_bare_group_is_usage() {
        let registry = default_registry();
        let result = run(&argv(&["work"]), &registry, "test", &mut no_dispatch);
        assert_eq!(result.exit_code, EXIT_USAGE, "{}", result.stdout);
        assert!(result.stdout.contains("usage: vogt"), "{}", result.stdout);
    }

    #[test]
    fn bad_enum_is_usage() {
        let registry = default_registry();
        let result = run(
            &argv(&[
                "work", "create", "--kind", "nope", "--title", "x", "--reason", "because",
            ]),
            &registry,
            "test",
            &mut no_dispatch,
        );
        assert_eq!(result.exit_code, EXIT_USAGE);
        assert!(result.stdout.contains("invalid choice"));
    }
}
