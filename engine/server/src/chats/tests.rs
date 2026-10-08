use super::*;

#[test]
fn reads_run_and_everything_else_waits_for_a_person() {
    for read in [
        "Read",
        "Grep",
        "Glob",
        "BrowserFetch",
        "BrowserSearch",
        "ToolSearch",
    ] {
        assert_eq!(tool_class(read), ToolClass::Read, "{read}");
    }
    for write in [
        "Bash",
        "Edit",
        "Write",
        "NotebookEdit",
        "Agent",
        "Memory",
        "SomethingNew",
    ] {
        assert_eq!(tool_class(write), ToolClass::Write, "{write}");
    }
}

#[test]
fn an_mcp_tool_reads_by_its_verbs_and_unknown_ones_ask() {
    for read in [
        "mcp__vogt__work_get",
        "mcp__vogt__session_list",
        "mcp__vogt__session_screen",
        "mcp__cadastre__lookup",
        "mcp__cadastre__brief",
        "mcp__vogt__status",
        // A server named read-only is read-only whatever the tool is called.
        "mcp__github-ro__pull_request_review_write",
    ] {
        assert_eq!(tool_class(read), ToolClass::Read, "{read}");
    }
    for write in [
        "mcp__vogt__work_create",
        "mcp__vogt__work_comment",
        "mcp__vogt__session_start",
        "mcp__vogt__session_input",
        "mcp__komodo__deploy_stack",
        "mcp__komodo__write_stack_file",
        // A read verb beside a write verb is a write.
        "mcp__vogt__drift_list_and_resolve",
        // Nothing recognisable: ask.
        "mcp__vogt__frobnicate",
        "mcp__malformed",
    ] {
        assert_eq!(tool_class(write), ToolClass::Write, "{write}");
    }
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
