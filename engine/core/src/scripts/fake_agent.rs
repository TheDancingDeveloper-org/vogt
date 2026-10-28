//! `scripts/fake_agent_core.py` in Rust.
//!
//! Deterministic stand-in for the agent CLI an agent-task run drives. It
//! accepts the same invocation shape the engine produces and plays a chosen
//! scenario: no network, no model, and no wall-clock dependence beyond the
//! small delays a scenario opts into.
//!
//! The engine watches stdout for the sentinels this prints (`VOGT_NOTIFY:`,
//! `VOGT_SKIP:`, `VOGT_COST:`, and a fenced `json` block). Lines are flushed.

use std::env;
use std::fs::{self, OpenOptions};
use std::io::{self, BufRead, Write};
use std::process::Command;
use std::thread;
use std::time::Duration;

const ENV_PROMPT_FILE: &str = "VOGT_ENGINE_AGENT_TASK_PROMPT_FILE";
const ENV_CONTEXT_FILE: &str = "VOGT_ENGINE_AGENT_TASK_CONTEXT_FILE";
const ENV_TASK_ID: &str = "VOGT_ENGINE_AGENT_TASK_ID";
const ENV_RUN_ID: &str = "VOGT_ENGINE_AGENT_TASK_RUN_ID";
const ENV_VOGT_PROJECT: &str = "VOGT_PROJECT";
const ENV_VOGT_WORK_ITEM: &str = "VOGT_WORK_ITEM";

const DEFAULT_NOTIFY_PHRASE: &str = "VOGT_NOTIFY:";
const SKIP_SENTINEL: &str = "VOGT_SKIP:";
const COST_SENTINEL: &str = "VOGT_COST:";
const TRAILER_TASK: &str = "Vogt-Task";
const TRAILER_RUN: &str = "Vogt-Run";

const KNOWN_STEPS: &[&str] = &[
    "edit", "commit", "findings", "idle", "outcome", "stall", "skip", "cost", "schema",
];

/// A step that ends the process, carrying the exit code the caller should use.
struct Exit(i32);

fn env_or(name: &str, default: &str) -> String {
    match env::var(name) {
        Ok(value) => value,
        Err(_) => default.to_string(),
    }
}

/// `repr()` of a Python `str`, so a steer echo reads as the Python original's
/// `{steer!r}` did: single quotes unless the text holds a `'` and no `"`.
fn py_repr(text: &str) -> String {
    let quote = if text.contains('\'') && !text.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::with_capacity(text.len() + 2);
    out.push(quote);
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

fn emit(line: &str) {
    let mut out = io::stdout().lock();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}

fn read_prompt(prompt_file: Option<&str>) -> String {
    let from_env = env::var(ENV_PROMPT_FILE).ok();
    let path = prompt_file
        .map(str::to_string)
        .filter(|s| !s.is_empty())
        .or_else(|| from_env.filter(|s| !s.is_empty()));
    let Some(path) = path else {
        return String::new();
    };
    fs::read_to_string(path).unwrap_or_default()
}

/// A `FAKE_AGENT_SCENARIO: <scenario>` marker line in the prompt, if any.
pub fn scenario_from_prompt(prompt: &str) -> Option<String> {
    for raw in prompt.lines() {
        let line = raw.trim();
        if let Some(rest) = line.strip_prefix("FAKE_AGENT_SCENARIO:") {
            let value = rest.trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

/// CLI arg, else `FAKE_AGENT_SCENARIO`, else a prompt marker, else `outcome`.
pub fn resolve_scenario(cli_scenario: Option<&str>, prompt: &str) -> String {
    if let Some(cli) = cli_scenario.map(str::trim).filter(|s| !s.is_empty()) {
        return cli.to_string();
    }
    if let Ok(from_env) = env::var("FAKE_AGENT_SCENARIO") {
        if !from_env.is_empty() {
            return from_env;
        }
    }
    if let Some(from_prompt) = scenario_from_prompt(prompt) {
        return from_prompt;
    }
    "outcome".to_string()
}

/// Split a `+`-separated scenario into known steps.
pub fn split_steps(scenario: &str) -> Result<Vec<String>, String> {
    let steps: Vec<String> = scenario
        .split('+')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect();
    let unknown: Vec<&str> = steps
        .iter()
        .filter(|step| !KNOWN_STEPS.contains(&step.as_str()))
        .map(String::as_str)
        .collect();
    if !unknown.is_empty() {
        let shown = format!(
            "[{}]",
            unknown
                .iter()
                .map(|s| format!("'{s}'"))
                .collect::<Vec<_>>()
                .join(", ")
        );
        let known = format!(
            "[{}]",
            KNOWN_STEPS
                .iter()
                .map(|s| format!("'{s}'"))
                .collect::<Vec<_>>()
                .join(", ")
        );
        return Err(format!(
            "fake-agent: unknown scenario step(s) {shown}; known steps are {known}"
        ));
    }
    if steps.is_empty() {
        Ok(vec!["outcome".to_string()])
    } else {
        Ok(steps)
    }
}

fn parse_f64(name: &str, default: &str) -> Result<f64, String> {
    let raw = env_or(name, default);
    raw.parse::<f64>()
        .map_err(|_| format!("fake-agent: {name} is not a number: {raw}"))
}

fn parse_i32(name: &str, default: &str) -> Result<i32, String> {
    let raw = env_or(name, default);
    raw.parse::<i32>()
        .map_err(|_| format!("fake-agent: {name} is not an integer: {raw}"))
}

fn sleep_secs(seconds: f64) {
    if seconds > 0.0 {
        thread::sleep(Duration::from_secs_f64(seconds));
    }
}

fn step_edit() -> Result<(), String> {
    let target = env_or("FAKE_AGENT_EDIT_FILE", "fake-agent-edit.txt");
    let run_id = env_or(ENV_RUN_ID, "no-run");
    let default = format!("edited by fake-agent for run {run_id}\n");
    let mut text = env_or("FAKE_AGENT_EDIT_TEXT", &default);
    if !text.ends_with('\n') {
        text.push('\n');
    }
    let mut handle = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&target)
        .map_err(|err| format!("fake-agent: edit failed: {err}"))?;
    handle
        .write_all(text.as_bytes())
        .map_err(|err| format!("fake-agent: edit failed: {err}"))?;
    emit(&format!("fake-agent: edited {target}"));
    Ok(())
}

fn git(args: &[&str]) -> Result<std::process::Output, String> {
    Command::new("git")
        .args(args)
        .output()
        .map_err(|err| format!("fake-agent: git failed: {err}"))
}

fn step_commit() -> Result<(), String> {
    let task_id = env_or(ENV_TASK_ID, "");
    let run_id = env_or(ENV_RUN_ID, "");
    let subject = env_or("FAKE_AGENT_COMMIT_MESSAGE", "chore: fake-agent checkpoint");
    let mut trailers = Vec::new();
    if !task_id.is_empty() {
        trailers.push(format!("{TRAILER_TASK}: {task_id}"));
    }
    if !run_id.is_empty() {
        trailers.push(format!("{TRAILER_RUN}: {run_id}"));
    }
    let message = if trailers.is_empty() {
        subject
    } else {
        format!("{subject}\n\n{}\n", trailers.join("\n"))
    };

    let add = git(&["add", "-A"])?;
    if !add.status.success() {
        let stderr = String::from_utf8_lossy(&add.stderr);
        emit(&format!("fake-agent: git add failed: {}", stderr.trim()));
        return Err(Exit(1).into_err());
    }
    let commit = git(&[
        "-c",
        "user.email=fake-agent@vogt.invalid",
        "-c",
        "user.name=fake-agent",
        "commit",
        "--no-gpg-sign",
        "--allow-empty",
        "-m",
        &message,
    ])?;
    if !commit.status.success() {
        let stderr = String::from_utf8_lossy(&commit.stderr);
        emit(&format!("fake-agent: git commit failed: {}", stderr.trim()));
        return Err(Exit(1).into_err());
    }
    let head = git(&["rev-parse", "HEAD"])?;
    let sha = String::from_utf8_lossy(&head.stdout);
    emit(&format!("fake-agent: committed {}", sha.trim()));
    Ok(())
}

/// Marker so a step can ask `run` to exit with a code without printing twice.
impl Exit {
    fn into_err(self) -> String {
        format!("\u{0}exit:{}", self.0)
    }
}

fn step_findings() -> Result<(), String> {
    let phrase = env_or("FAKE_AGENT_NOTIFY_PHRASE", DEFAULT_NOTIFY_PHRASE);
    let text = env_or("FAKE_AGENT_NOTIFY_TEXT", "fake-agent synthetic finding");
    let delay = parse_f64("FAKE_AGENT_NOTIFY_DELAY", "0.3")?;
    sleep_secs(delay);
    emit(&findings_line(&phrase, &text));

    let dump = env_or("FAKE_AGENT_FINDINGS_FILE", "");
    if !dump.is_empty() {
        let payload = findings_json(&text, &phrase);
        fs::write(&dump, payload)
            .map_err(|err| format!("fake-agent: findings dump failed: {err}"))?;
    }
    Ok(())
}

pub fn findings_line(phrase: &str, text: &str) -> String {
    format!("{phrase} {text}")
}

pub fn skip_line(reason: &str) -> String {
    format!("{SKIP_SENTINEL} {reason}")
}

pub fn cost_line(payload: &str) -> String {
    format!("{COST_SENTINEL} {payload}")
}

fn findings_json(text: &str, phrase: &str) -> String {
    let task_id = env_or(ENV_TASK_ID, "");
    let run_id = env_or(ENV_RUN_ID, "");
    let project = env_or(ENV_VOGT_PROJECT, "");
    let work_item = env_or(ENV_VOGT_WORK_ITEM, "");
    let payload = serde_json::json!({
        "task_id": task_id,
        "run_id": run_id,
        "vogt_project": if project.is_empty() { serde_json::Value::Null } else { serde_json::Value::String(project) },
        "vogt_work_item": if work_item.is_empty() { serde_json::Value::Null } else { serde_json::Value::String(work_item) },
        "findings": [{
            "text": text,
            "source": "fake-agent",
            "phrase": phrase,
        }],
    });
    format!(
        "{}\n",
        serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "{}".to_string())
    )
}

fn step_idle() -> Result<(), String> {
    let prompt = env_or("FAKE_AGENT_IDLE_PROMPT", "fake-agent idle> ");
    let seconds = parse_f64("FAKE_AGENT_IDLE_SECONDS", "")
        .ok()
        .filter(|s| *s >= 0.0);
    let mut out = io::stdout().lock();
    let _ = write!(out, "{prompt}");
    let _ = out.flush();
    drop(out);

    let line = read_idle_line(seconds);
    match line {
        Some(raw) => {
            let steer = raw.trim();
            if !steer.is_empty() {
                emit(&format!("fake-agent: steered with {}", py_repr(steer)));
            } else {
                emit("fake-agent: idle continued (eof)");
            }
        }
        None => emit("fake-agent: idle continued (eof)"),
    }
    Ok(())
}

/// Read one stdin line. When `FAKE_AGENT_IDLE_SECONDS` is set, stop after that
/// many seconds and treat the timeout as EOF so a test cannot hang.
fn read_idle_line(bound: Option<f64>) -> Option<String> {
    if let Some(seconds) = bound.filter(|s| *s > 0.0) {
        let (tx, rx) = std::sync::mpsc::channel();
        thread::spawn(move || {
            let stdin = io::stdin();
            let mut line = String::new();
            let read = stdin.lock().read_line(&mut line);
            let _ = tx.send(read.ok().filter(|n| *n > 0).map(|_| line));
        });
        rx.recv_timeout(Duration::from_secs_f64(seconds))
            .unwrap_or_default()
    } else {
        let stdin = io::stdin();
        let mut line = String::new();
        match stdin.lock().read_line(&mut line) {
            Ok(0) | Err(_) => None,
            Ok(_) => Some(line),
        }
    }
}

fn step_stall() -> Result<(), String> {
    let seconds = parse_f64("FAKE_AGENT_STALL_SECONDS", "3600")?;
    emit(&format!("fake-agent: stalling for {seconds}s"));
    sleep_secs(seconds);
    Ok(())
}

fn step_outcome() -> Result<(), String> {
    let code = parse_i32("FAKE_AGENT_EXIT_CODE", "0")?;
    emit(&format!("fake-agent: exiting {code}"));
    Err(Exit(code).into_err())
}

fn step_skip() -> Result<(), String> {
    let reason = env_or("FAKE_AGENT_SKIP_REASON", "nothing to do");
    emit(&skip_line(&reason));
    Ok(())
}

fn step_cost() -> Result<(), String> {
    let payload = env_or(
        "FAKE_AGENT_COST",
        r#"{"total_usd": 0.42, "input_tokens": 1200, "output_tokens": 340}"#,
    );
    emit(&cost_line(&payload));
    Ok(())
}

fn step_schema() -> Result<(), String> {
    let pass_on = parse_i32("FAKE_AGENT_SCHEMA_PASS_ON", "1")?;
    let delay = parse_f64("FAKE_AGENT_SCHEMA_DELAY", "0.3")?;
    let good = env_or(
        "FAKE_AGENT_SCHEMA_GOOD",
        r#"{"summary": "did the thing", "risk": "low"}"#,
    );
    let bad = env_or("FAKE_AGENT_SCHEMA_BAD", r#"{"summary": "did the thing"}"#);
    sleep_secs(delay);

    let mut attempt = 1;
    loop {
        let payload = if attempt >= pass_on { &good } else { &bad };
        for line in schema_block(payload).lines() {
            emit(line);
        }
        if attempt >= pass_on {
            return Err(Exit(0).into_err());
        }
        // A wrong block: wait for the engine's correction before trying again.
        let stdin = io::stdin();
        let mut line = String::new();
        match stdin.lock().read_line(&mut line) {
            Ok(0) | Err(_) => return Err(Exit(0).into_err()),
            Ok(_) => {}
        }
        attempt += 1;
    }
}

/// A fenced findings block, exactly three lines, trailing newline included by
/// the caller that emits each line.
pub fn schema_block(payload: &str) -> String {
    format!("```json\n{payload}\n```")
}

fn run_step(step: &str) -> Result<(), String> {
    match step {
        "edit" => step_edit(),
        "commit" => step_commit(),
        "findings" => step_findings(),
        "idle" => step_idle(),
        "stall" => step_stall(),
        "outcome" => step_outcome(),
        "skip" => step_skip(),
        "cost" => step_cost(),
        "schema" => step_schema(),
        other => Err(format!("fake-agent: unknown scenario step(s) ['{other}']")),
    }
}

fn decode_exit(err: &str) -> Result<i32, String> {
    err.strip_prefix("\u{0}exit:")
        .and_then(|code| code.parse().ok())
        .map(Ok)
        .unwrap_or_else(|| Err(err.to_string()))
}

/// Machine-readable contract, key order matching `json.dumps(..., indent=2)`.
pub fn contract_json() -> String {
    let payload = serde_json::json!({
        "scenario_default": "outcome",
        "steps": KNOWN_STEPS,
        "reads_env": [
            ENV_PROMPT_FILE,
            ENV_CONTEXT_FILE,
            ENV_TASK_ID,
            ENV_RUN_ID,
            ENV_VOGT_PROJECT,
            ENV_VOGT_WORK_ITEM,
        ],
        "config_env": {
            "FAKE_AGENT_SCENARIO": "scenario, '+'-separated steps",
            "FAKE_AGENT_NOTIFY_PHRASE": DEFAULT_NOTIFY_PHRASE,
            "FAKE_AGENT_NOTIFY_TEXT": "fake-agent synthetic finding",
            "FAKE_AGENT_NOTIFY_DELAY": "0.3",
            "FAKE_AGENT_FINDINGS_FILE": "optional path for a JSON dump",
            "FAKE_AGENT_EXIT_CODE": "0",
            "FAKE_AGENT_EDIT_FILE": "fake-agent-edit.txt",
            "FAKE_AGENT_EDIT_TEXT": "edited by fake-agent for run <run_id>",
            "FAKE_AGENT_COMMIT_MESSAGE": "chore: fake-agent checkpoint",
            "FAKE_AGENT_STALL_SECONDS": "3600",
            "FAKE_AGENT_IDLE_PROMPT": "fake-agent idle> ",
            "FAKE_AGENT_SKIP_REASON": "nothing to do",
            "FAKE_AGENT_COST": "{\"total_usd\": 0.42, ...}",
            "FAKE_AGENT_SCHEMA_PASS_ON": "1",
            "FAKE_AGENT_SCHEMA_DELAY": "0.3",
            "FAKE_AGENT_SCHEMA_GOOD": "a findings block that passes the schema",
            "FAKE_AGENT_SCHEMA_BAD": "a findings block that fails the schema",
        },
        "commit_trailers": [TRAILER_TASK, TRAILER_RUN],
        "notify_phrase_default": DEFAULT_NOTIFY_PHRASE,
        "skip_sentinel": SKIP_SENTINEL,
        "cost_sentinel": COST_SENTINEL,
    });
    serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "{}".to_string())
}

struct Cli {
    scenario: Option<String>,
    prompt_file: Option<String>,
    print_contract: bool,
}

fn parse_args(args: &[String]) -> Result<Cli, String> {
    let mut cli = Cli {
        scenario: None,
        prompt_file: None,
        print_contract: false,
    };
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if arg == "--contract" || arg == "--print-contract" {
            cli.print_contract = true;
            return Ok(cli);
        } else if arg == "--prompt-file" {
            i += 1;
            let path = args
                .get(i)
                .ok_or_else(|| "fake-agent: --prompt-file needs a path".to_string())?;
            cli.prompt_file = Some(path.clone());
        } else if let Some(path) = arg.strip_prefix("--prompt-file=") {
            cli.prompt_file = Some(path.to_string());
        } else if arg == "--scenario" {
            i += 1;
            let value = args
                .get(i)
                .ok_or_else(|| "fake-agent: --scenario needs a value".to_string())?;
            cli.scenario = Some(value.clone());
        } else if let Some(value) = arg.strip_prefix("--scenario=") {
            cli.scenario = Some(value.to_string());
        } else if !arg.starts_with('-') && cli.scenario.is_none() {
            cli.scenario = Some(arg.clone());
        }
        i += 1;
    }
    Ok(cli)
}

/// Play the scenario. The `Ok` value is the process exit code.
pub fn run(args: &[String]) -> Result<i32, String> {
    let cli = parse_args(args)?;
    if cli.print_contract {
        emit(&contract_json());
        return Ok(0);
    }
    let prompt = read_prompt(cli.prompt_file.as_deref());
    let scenario = resolve_scenario(cli.scenario.as_deref(), &prompt);
    let steps = split_steps(&scenario)?;
    emit(&format!(
        "fake-agent: scenario {scenario} (steps: {})",
        steps.join("+")
    ));
    for step in &steps {
        if let Err(err) = run_step(step) {
            return decode_exit(&err);
        }
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_steer_echo_quotes_like_python_repr() {
        assert_eq!(super::py_repr("continue please"), "'continue please'");
        assert_eq!(super::py_repr("it's"), "\"it's\"");
        assert_eq!(super::py_repr("it's \"x\""), "'it\\'s \"x\"'");
        assert_eq!(super::py_repr("a\\b\tc"), "'a\\\\b\\tc'");
        assert_eq!(super::py_repr("\u{1}"), "'\\x01'");
    }

    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// The same lock `config` tests hold while they rewrite the process
    /// environment. A separate lock let the two modules interleave `set_var`.
    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        crate::config::env_lock()
    }

    #[test]
    fn scenario_from_prompt_marker() {
        let prompt = "# Task\n\nFAKE_AGENT_SCENARIO: edit+commit\nmore\n";
        assert_eq!(scenario_from_prompt(prompt).as_deref(), Some("edit+commit"));
        assert_eq!(scenario_from_prompt("no marker\n"), None);
        assert_eq!(scenario_from_prompt("FAKE_AGENT_SCENARIO:   \n"), None);
    }

    #[test]
    fn resolve_prefers_cli_then_env_then_prompt() {
        let _guard = env_lock();
        env::remove_var("FAKE_AGENT_SCENARIO");
        assert_eq!(
            resolve_scenario(Some("skip"), "FAKE_AGENT_SCENARIO: cost\n"),
            "skip"
        );
        env::set_var("FAKE_AGENT_SCENARIO", "idle");
        assert_eq!(
            resolve_scenario(None, "FAKE_AGENT_SCENARIO: cost\n"),
            "idle"
        );
        env::remove_var("FAKE_AGENT_SCENARIO");
        assert_eq!(
            resolve_scenario(None, "FAKE_AGENT_SCENARIO: cost\n"),
            "cost"
        );
        assert_eq!(resolve_scenario(None, ""), "outcome");
    }

    #[test]
    fn step_splitting_and_unknown() {
        assert_eq!(
            split_steps("edit+commit").expect("steps"),
            vec!["edit".to_string(), "commit".to_string()]
        );
        assert_eq!(split_steps("  ").expect("default"), vec!["outcome"]);
        let err = split_steps("edit+nope").expect_err("unknown");
        assert!(err.contains("unknown scenario step"), "{err}");
        assert!(err.contains("nope"), "{err}");
    }

    #[test]
    fn edit_step_appends_file() {
        let _guard = env_lock();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let dir = env::temp_dir().join(format!("fake-agent-{nanos}"));
        fs::create_dir_all(&dir).expect("dir");
        let file = dir.join("note.txt");
        let prev = env::current_dir().expect("cwd");
        env::set_current_dir(&dir).expect("chdir");
        env::set_var("FAKE_AGENT_EDIT_FILE", "note.txt");
        env::set_var("FAKE_AGENT_EDIT_TEXT", "hello");
        env::remove_var(ENV_RUN_ID);
        let result = step_edit();
        env::set_current_dir(&prev).expect("restore");
        env::remove_var("FAKE_AGENT_EDIT_FILE");
        env::remove_var("FAKE_AGENT_EDIT_TEXT");
        result.expect("edit");
        let body = fs::read_to_string(&file).expect("read");
        assert_eq!(body, "hello\n");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn sentinel_lines() {
        assert_eq!(skip_line("nothing to do"), "VOGT_SKIP: nothing to do");
        assert_eq!(
            cost_line(r#"{"total_usd": 0.42}"#),
            "VOGT_COST: {\"total_usd\": 0.42}"
        );
        assert_eq!(
            findings_line("VOGT_NOTIFY:", "the price dropped"),
            "VOGT_NOTIFY: the price dropped"
        );
        assert!(schema_block(r#"{"a": 1}"#).starts_with("```json\n"));
        assert!(schema_block("{}").ends_with("\n```"));
    }

    #[test]
    fn outcome_exit_code() {
        let _guard = env_lock();
        env::set_var("FAKE_AGENT_EXIT_CODE", "7");
        let err = step_outcome().expect_err("exits");
        env::remove_var("FAKE_AGENT_EXIT_CODE");
        assert_eq!(decode_exit(&err).expect("code"), 7);
        env::set_var("FAKE_AGENT_EXIT_CODE", "0");
        let err = step_outcome().expect_err("exits");
        env::remove_var("FAKE_AGENT_EXIT_CODE");
        assert_eq!(decode_exit(&err).expect("code"), 0);
    }
}
