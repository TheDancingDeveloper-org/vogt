use super::*;

#[test]
fn only_reads_of_the_chats_own_directory_and_inert_tools_run_free() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("notes.txt"), "x").unwrap();
    let here = dir.path();
    let free = |tool: &str, input: Value| classify(tool, &input, here) == ToolClass::Free;
    assert!(free("Read", json!({"file_path": "notes.txt"})));
    assert!(free(
        "Read",
        json!({"file_path": here.join("notes.txt").to_str().unwrap()})
    ));
    assert!(free("Glob", json!({"pattern": "**/*.md"})));
    assert!(free("Grep", json!({"pattern": "TODO"})));
    assert!(free(
        "Read",
        json!({"file_path": "notes.txt", "offset": 1, "limit": 5})
    ));
    assert!(free("ToolSearch", json!({"query": "anything"})));
    assert!(free("TodoWrite", json!({})));
    // Out of the directory, however it is spelled.
    for path in [
        "/proc/self/environ",
        "../../chats.db",
        "~/.ssh/id_rsa",
        "sub/../../x",
    ] {
        assert!(!free("Read", json!({ "file_path": path })), "{path}");
    }
    assert!(!free("Glob", json!({"pattern": "/proc/*/environ"})));
    assert!(!free("Grep", json!({"pattern": "x", "path": "/etc"})));
    // Through a symlink that leaves it.
    std::os::unix::fs::symlink("/etc", here.join("out")).unwrap();
    assert!(!free("Read", json!({"file_path": "out/passwd"})));
    // Anything that reaches the network or changes something asks.
    for tool in [
        "WebFetch",
        "WebSearch",
        "BrowserFetch",
        "BrowserSearch",
        "Bash",
        "Edit",
        "Write",
        "Agent",
    ] {
        assert!(
            !free(tool, json!({"url": "https://example.com", "query": "q"})),
            "{tool}"
        );
    }
    for tool in [
        "mcp__vogt__work_get",
        "mcp__github-ro__search_code",
        "mcp__cadastre__lookup",
    ] {
        assert!(!free(tool, json!({})), "{tool}");
    }
}

#[test]
fn the_free_path_is_an_allowlist_and_anything_it_cannot_classify_asks() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("main.go"), "package main").unwrap();
    let here = dir.path();
    let free = |tool: &str, input: Value| classify(tool, &input, here) == ToolClass::Free;
    // Klaudia's LSP tools name their path `file` (review round 2).
    for tool in ["Hover", "Diagnostics", "DocumentSymbols"] {
        if tool == "Hover" {
            assert!(free(
                tool,
                json!({"file": "main.go", "line": 1, "character": 1})
            ));
        }
        assert!(
            !free(
                tool,
                json!({"file": "/proc/self/environ", "line": 1, "character": 1})
            ),
            "{tool}"
        );
        assert!(!free(tool, json!({"file": "/proc/self/environ"})), "{tool}");
    }
    assert!(free("Diagnostics", json!({"file": "main.go"})));
    assert!(free("WorkspaceSymbol", json!({"query": "Pool"})));
    assert!(!free(
        "WorkspaceSymbol",
        json!({"query": "Pool", "file": "/etc/passwd"})
    ));
    // An unknown tool with a path argument asks, whatever the path.
    assert!(!free("ReadFile", json!({"file_path": "main.go"})));
    assert!(!free("Peek", json!({"file": "main.go"})));
    // An argument the table does not know asks, even beside a local path.
    assert!(!free(
        "Read",
        json!({"file_path": "main.go", "follow": "/etc/passwd"})
    ));
    // A required path that is missing, empty or not a string asks.
    assert!(!free("Read", json!({})));
    assert!(!free("Read", json!({"file_path": ""})));
    assert!(!free("Read", json!({"file_path": ["/etc/passwd"]})));
    assert!(!free("Hover", json!({"line": 1})));
    assert!(!free("Read", Value::Null));
    // A Grep pattern is a regex, not a path; its `path` and `glob` are.
    assert!(free("Grep", json!({"pattern": "^/etc/passwd", "-i": true})));
    assert!(!free("Grep", json!({"pattern": "x", "glob": "/etc/*"})));
    // Glob and Grep with no path search the working directory, the chat's.
    assert!(free("Glob", json!({"pattern": "**/*.go"})));
}

#[test]
fn redaction_masks_what_looks_like_a_credential() {
    let text = "401 from api: Authorization: Bearer abcdef0123456789 \
                api_key=sk-live-1234567890abcdef token: \"zzzzzzzzzzzz\" \
                ghp_ABCDEFGHIJKLMNOPQRSTUV and plain words stay";
    let out = redact(text);
    for secret in [
        "abcdef0123456789",
        "sk-live-1234567890abcdef",
        "zzzzzzzzzzzz",
        "ghp_ABCDEFGHIJKLMNOPQRSTUV",
    ] {
        assert!(!out.contains(secret), "{secret} survived: {out}");
    }
    assert!(out.contains("plain words stay"), "{out}");
    // A git SHA and a long single-case identifier are not secrets.
    let sha = "9faaef2c1d3b4e5f60718293a4b5c6d7e8f90a1b";
    assert!(redact(&format!("commit {sha}")).contains(sha));
    let ident = "a_very_long_snake_case_identifier_name_here";
    assert!(redact(ident).contains(ident));
    assert!(!redact("key AbCdEf0123456789GhIjKl0123456789MnOp").contains("AbCdEf0123456789"));
}

#[test]
fn a_wrapper_in_front_of_the_driver_is_dropped_for_a_chat() {
    let argv: Vec<String> = ["vogt-agent-auth", "run", "--", "klaudia", "--x"]
        .map(String::from)
        .to_vec();
    assert_eq!(driver_of(&argv), ["klaudia", "--x"]);
    let bare: Vec<String> = vec!["/opt/bin/klaudia".into()];
    assert_eq!(driver_of(&bare), bare);
}

#[test]
fn an_orphaned_chat_process_group_is_ended_at_boot_and_a_reused_pid_is_not() {
    let chats = tempfile::tempdir().unwrap();
    let dir = chats.path().join(Uuid::new_v4().to_string());
    std::fs::create_dir_all(&dir).unwrap();
    let mut child = std::process::Command::new("sleep");
    child.arg("300");
    std::os::unix::process::CommandExt::process_group(&mut child, 0);
    let mut child = child.spawn().unwrap();
    write_pidfile(&dir, child.id());
    // A record whose start time does not match is someone else's process.
    let stranger = chats.path().join("stranger");
    std::fs::create_dir_all(&stranger).unwrap();
    std::fs::write(
        stranger.join(PIDFILE),
        format!("{} 1\n", std::process::id()),
    )
    .unwrap();
    sweep_orphans(chats.path());
    let status = child.wait().unwrap();
    assert!(!status.success(), "the orphan was killed");
    assert!(!dir.join(PIDFILE).exists());
    assert!(!stranger.join(PIDFILE).exists());
}

#[test]
fn the_hook_blocks_on_any_failure_and_outlives_the_engines_own_deadline() {
    let config = hook_config(Duration::from_secs(600));
    // Parses as the TOML Klaudia reads.
    let parsed: toml::Value = toml::from_str(&config).unwrap();
    let hook = &parsed["hooks"][0];
    assert_eq!(hook["event"].as_str(), Some("PreToolUse"));
    let command = hook["command"].as_str().unwrap();
    // Exit 2 is Klaudia's "blocked"; any other failure would read as a
    // broken hook and let the call through.
    assert!(command.contains("exit 2"), "{command}");
    assert!(command.contains("curl -sS -f"), "{command}");
    assert!(command.contains("$VOGT_CHAT_GATE_URL"), "{command}");
    // The engine answers at 600 s; curl gives up later, and Klaudia (which
    // lets a timed-out hook's call through) later still.
    assert!(command.contains("--max-time 660"), "{command}");
    assert_eq!(hook["timeout"].as_str(), Some("720s"));
}

#[test]
fn a_call_is_described_by_what_it_would_do() {
    assert_eq!(
        describe_call("Bash", &json!({"command": "make test"})),
        "make test"
    );
    assert_eq!(
        describe_call("Edit", &json!({"file_path": "/x/y.rs", "old_string": "a"})),
        "/x/y.rs"
    );
    assert_eq!(
        describe_call("mcp__vogt__work_create", &json!({"title": "T"})),
        r#"{"title":"T"}"#
    );
    assert_eq!(describe_call("Agent", &Value::Null), "Agent");
    let long = "x".repeat(2_000);
    assert!(
        describe_call("Bash", &json!({ "command": long }))
            .chars()
            .count()
            <= INPUT_CHARS + 1
    );
}

#[test]
fn the_launch_is_the_klaudia_template_unless_a_command_is_named() {
    let enabled = ChatPolicy {
        enabled: true,
        ..Default::default()
    };
    let templates = crate::config::SessionTemplate::default_templates();
    let launch = resolve_launch(&enabled, &templates).unwrap();
    assert_eq!(launch.template.as_deref(), Some("Klaudia (protected)"));
    assert_eq!(launch.argv, ["vogt-agent-auth", "run", "--", "klaudia"]);

    let named = ChatPolicy {
        command: Some(vec!["/opt/bin/klaudia".into()]),
        ..enabled.clone()
    };
    let launch = resolve_launch(&named, &templates).unwrap();
    assert_eq!(launch.template, None);
    assert_eq!(launch.argv, ["/opt/bin/klaudia"]);

    // Not Klaudia: the engine cannot speak its protocol, so no chats.
    let other = ChatPolicy {
        command: Some(vec!["claude".into()]),
        ..enabled.clone()
    };
    assert!(resolve_launch(&other, &templates).is_none());
    // Off, or no template that runs Klaudia: no chats.
    assert!(resolve_launch(&ChatPolicy::default(), &templates).is_none());
    assert!(resolve_launch(&enabled, &[]).is_none());
}

#[test]
fn a_policy_refuses_models_and_timeouts_that_would_misbehave() {
    let ok = ChatPolicy {
        enabled: true,
        models: vec![ChatModel {
            id: "grok-4.7".into(),
            label: "Grok".into(),
        }],
        ..Default::default()
    };
    assert!(ok.validate().is_ok());
    for bad in [
        ChatPolicy {
            models: vec![ChatModel {
                id: "--dangerously-skip-permissions".into(),
                label: "x".into(),
            }],
            ..ok.clone()
        },
        ChatPolicy {
            models: vec![ChatModel {
                id: "default".into(),
                label: "x".into(),
            }],
            ..ok.clone()
        },
        ChatPolicy {
            approval_timeout: Duration::from_secs(2),
            ..ok.clone()
        },
    ] {
        assert!(bad.validate().is_err(), "{bad:?}");
    }
}
