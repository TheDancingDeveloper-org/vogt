//! The real Codex fork, loading the shipped requirements file, refuses git
//! that discards pre-existing work and still lets a session commit its own
//! (WI-1356).
//!
//! The fork reads only `/etc/codex/requirements.toml` (a release build has no
//! override), so this test runs when that file is byte-for-byte the shipped
//! `engine/deploy/driven-session-codex-requirements.toml` and both `codex` and
//! `klaudia` are installed — the engine image, or a box where the file was
//! copied into place. Anywhere else it prints why and passes, unless
//! `VOGT_CODEX_GITGUARD_E2E=1`, which turns a missing prerequisite into a
//! failure.
//!
//! It repeats the run under every way a session could switch the `hooks`
//! feature off (`-c`, `--disable`, `$CODEX_HOME/config.toml`, a project
//! `.codex/config.toml`); the shipped `[features] hooks = true` pin must win.
//! A nested `codex exec --disable hooks` from a session's shell is the
//! `--disable` case.
//!
//! The model is a scripted Responses API on loopback: each turn it asks for
//! the next shell command, and records what Codex sent back for the last one.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

const SHIPPED: &str = include_str!("../../deploy/driven-session-codex-requirements.toml");
const REQUIREMENTS: &str = "/etc/codex/requirements.toml";

fn prerequisites() -> Result<PathBuf, String> {
    match std::fs::read_to_string(REQUIREMENTS) {
        Ok(text) if text == SHIPPED => {}
        Ok(_) => {
            return Err(format!(
                "{REQUIREMENTS} is not the shipped requirements file"
            ))
        }
        Err(e) => return Err(format!("{REQUIREMENTS}: {e}")),
    }
    let on_path = |name: &str| {
        std::env::var_os("PATH").and_then(|paths| {
            std::env::split_paths(&paths)
                .map(|dir| dir.join(name))
                .find(|p| p.is_file())
        })
    };
    on_path("klaudia").ok_or("klaudia is not on PATH")?;
    std::env::var_os("VOGT_CODEX_BIN")
        .map(PathBuf::from)
        .or_else(|| Some(PathBuf::from("/usr/local/libexec/codex-real")).filter(|p| p.is_file()))
        .or_else(|| on_path("codex"))
        .ok_or_else(|| "no codex binary (set VOGT_CODEX_BIN)".to_string())
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
    String::from_utf8(out.stdout).unwrap()
}

/// A repo with one committed file, then a modified tracked file and an
/// untracked file that exist before the session starts.
fn dirty_repo(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    git(dir, &["init", "-q"]);
    std::fs::write(dir.join("f.txt"), "base\n").unwrap();
    git(dir, &["add", "f.txt"]);
    git(dir, &["commit", "-q", "-m", "base"]);
    std::fs::write(dir.join("f.txt"), "base\npre-existing edit\n").unwrap();
    std::fs::write(dir.join("hello.txt"), "untracked, not the session's\n").unwrap();
}

fn assert_still_dirty(dir: &Path) {
    assert_eq!(
        std::fs::read_to_string(dir.join("f.txt")).unwrap(),
        "base\npre-existing edit\n",
        "{} lost its tracked edit",
        dir.display()
    );
    assert!(
        dir.join("hello.txt").is_file(),
        "{} lost hello.txt",
        dir.display()
    );
    assert_eq!(
        git(dir, &["stash", "list"]),
        "",
        "{} was stashed",
        dir.display()
    );
}

/// One SSE response: a shell call for `cmd`, or a final message.
fn sse(turn: usize, tools: &[String], cmd: Option<&str>) -> String {
    let item = match cmd {
        Some(cmd) if tools.iter().any(|t| t == "exec_command") => serde_json::json!({
            "type": "function_call", "id": format!("fc{turn}"), "call_id": format!("call{turn}"),
            "name": "exec_command", "arguments": serde_json::json!({"cmd": cmd}).to_string(),
        }),
        Some(cmd) => serde_json::json!({
            "type": "function_call", "id": format!("fc{turn}"), "call_id": format!("call{turn}"),
            "name": "shell",
            "arguments": serde_json::json!({"command": ["bash", "-lc", cmd]}).to_string(),
        }),
        None => serde_json::json!({
            "type": "message", "role": "assistant", "id": "msg",
            "content": [{"type": "output_text", "text": "done"}],
        }),
    };
    let id = format!("resp{turn}");
    let usage = serde_json::json!({
        "input_tokens": 1, "input_tokens_details": null, "output_tokens": 1,
        "output_tokens_details": null, "total_tokens": 2,
    });
    [
        serde_json::json!({"type": "response.created", "response": {"id": id}}),
        serde_json::json!({"type": "response.output_item.done", "item": item}),
        serde_json::json!({"type": "response.completed", "response": {"id": id, "usage": usage}}),
    ]
    .iter()
    .map(|ev| format!("event: {}\ndata: {ev}\n\n", ev["type"].as_str().unwrap()))
    .collect()
}

/// Serve the script; return the tool output Codex reported for each call.
fn scripted_model(cmds: Vec<String>) -> (u16, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let outputs = Arc::new(Mutex::new(Vec::new()));
    let seen = outputs.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request_line = String::new();
            if reader.read_line(&mut request_line).is_err() {
                continue;
            }
            let mut len = 0usize;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
                if let Some(v) = line
                    .split_once(':')
                    .filter(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                    .map(|(_, v)| v)
                {
                    len = v.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0; len];
            if reader.read_exact(&mut body).is_err() {
                continue;
            }
            if !request_line.starts_with("POST") {
                let body = r#"{"models":[],"data":[]}"#;
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                continue;
            }
            let req: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
            let tools: Vec<String> = req["tools"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|t| t["name"].as_str().map(str::to_string))
                .collect();
            let done: Vec<String> = req["input"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|i| i["type"] == "function_call_output")
                .map(|i| match &i["output"] {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                })
                .collect();
            let turn = done.len();
            *seen.lock().unwrap() = done;
            let body = sse(turn, &tools, cmds.get(turn).map(String::as_str));
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    (port, outputs)
}

/// How a session might try to switch the `hooks` feature off. The shipped
/// file pins `[features] hooks = true`, so every case must still be refused.
struct Override {
    name: &'static str,
    args: &'static [&'static str],
    codex_home_config: Option<&'static str>,
    project_config: Option<&'static str>,
}

const OVERRIDES: &[Override] = &[
    Override {
        name: "none",
        args: &[],
        codex_home_config: None,
        project_config: None,
    },
    Override {
        name: "-c features.hooks=false",
        args: &["-c", "features.hooks=false"],
        codex_home_config: None,
        project_config: None,
    },
    Override {
        name: "--disable hooks",
        args: &["--disable", "hooks"],
        codex_home_config: None,
        project_config: None,
    },
    Override {
        name: "$CODEX_HOME/config.toml",
        args: &[],
        codex_home_config: Some("[features]\nhooks = false\n"),
        project_config: None,
    },
    Override {
        name: "project .codex/config.toml",
        args: &[],
        codex_home_config: None,
        project_config: Some("[features]\nhooks = false\n"),
    },
];

#[test]
fn codex_with_the_shipped_requirements_refuses_destructive_git() {
    let codex = match prerequisites() {
        Ok(codex) => codex,
        Err(why) if std::env::var("VOGT_CODEX_GITGUARD_E2E").as_deref() == Ok("1") => {
            panic!("VOGT_CODEX_GITGUARD_E2E=1 but {why}")
        }
        Err(why) => {
            eprintln!("skipping the Codex gitguard e2e: {why}");
            return;
        }
    };
    for case in OVERRIDES {
        refuses_destructive_git(&codex, case);
    }
}

fn refuses_destructive_git(codex: &Path, case: &Override) {
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("repo");
    let other = root.path().join("other");
    let home = root.path().join("codex-home");
    std::fs::create_dir_all(&home).unwrap();
    dirty_repo(&repo);
    dirty_repo(&other);
    let mut home_config = case.codex_home_config.unwrap_or_default().to_string();
    if let Some(project) = case.project_config {
        // Committed, so it is not part of the work gitguard protects, and the
        // project trusted, so Codex loads its config layer.
        std::fs::create_dir_all(repo.join(".codex")).unwrap();
        std::fs::write(repo.join(".codex/config.toml"), project).unwrap();
        git(&repo, &["add", ".codex/config.toml"]);
        git(&repo, &["commit", "-q", "-m", "project config"]);
        home_config.push_str(&format!(
            "\n[projects.{:?}]\ntrust_level = \"trusted\"\n",
            repo.display().to_string()
        ));
    }
    std::fs::write(home.join("config.toml"), home_config).unwrap();

    let refused = [
        "git stash".to_string(),
        "git checkout -- .".to_string(),
        "git clean -f".to_string(),
        "git reset --hard".to_string(),
        format!("git -C {} restore f.txt", other.display()),
    ];
    let own = [
        "printf 'mine\\n' > own.txt".to_string(),
        "git add own.txt && git -c user.name=t -c user.email=t@example.invalid commit -q -m own"
            .to_string(),
    ];
    let script: Vec<String> = refused.iter().chain(own.iter()).cloned().collect();
    let (port, outputs) = scripted_model(script.clone());

    let provider = format!(
        "model_providers.mock={{name=\"mock\",base_url=\"http://127.0.0.1:{port}/v1\",wire_api=\"responses\",env_key=\"VOGT_E2E_MOCK_KEY\"}}"
    );
    let out = Command::new(codex)
        .current_dir(&repo)
        .env("CODEX_HOME", &home)
        .env("VOGT_E2E_MOCK_KEY", "unused")
        .args(["exec", "--skip-git-repo-check", "--json"])
        .arg("--dangerously-bypass-approvals-and-sandbox")
        .args(case.args)
        .args(["-c", "model_provider=mock", "-c", &provider, "-m", "gpt-5"])
        .arg("run the script")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    let outputs = outputs.lock().unwrap().clone();
    let context = format!(
        "override {}\ncodex exit {:?}\nstderr tail: {}\ntool outputs: {outputs:#?}",
        case.name,
        out.status,
        String::from_utf8_lossy(&out.stderr)
            .lines()
            .filter(|l| !l.contains(" WARN ") && !l.contains(" INFO "))
            .collect::<Vec<_>>()
            .join("\n")
    );
    assert!(out.status.success(), "{context}");
    assert_eq!(outputs.len(), script.len(), "{context}");

    for (cmd, output) in refused.iter().zip(&outputs) {
        assert!(
            output.contains("blocked by PreToolUse hook") && output.contains("Refused"),
            "`{cmd}` was not refused by gitguard: {output}\n{context}"
        );
    }
    assert_still_dirty(&repo);
    assert_still_dirty(&other);

    // The session's own work commits; the pre-existing work is not swept in.
    let log = git(&repo, &["log", "--format=%s", "-1", "--name-only"]);
    assert_eq!(
        log.split_whitespace().collect::<Vec<_>>(),
        ["own", "own.txt"],
        "{context}"
    );
}
