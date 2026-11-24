use super::*;

#[test]
fn redaction_masks_what_looks_like_a_credential() {
    let text = "401 from api: Authorization: Bearer abcdef0123456789 \
                api_key=sk-live-1234567890abcdef token: \"zzzzzzzzzzzz\" \
                ghp_ABCDEFGHIJKLMNOPQRSTUV and plain words stay";
    let out = redact(text);
    // The assertion messages name the sample by position only: a test must
    // not write a credential-shaped value into its output.
    for (i, sample) in [
        "abcdef0123456789",
        "sk-live-1234567890abcdef",
        "zzzzzzzzzzzz",
        "ghp_ABCDEFGHIJKLMNOPQRSTUV",
    ]
    .into_iter()
    .enumerate()
    {
        assert!(!out.contains(sample), "sample {i} survived redaction");
    }
    assert!(
        out.contains("plain words stay"),
        "plain text was redacted too"
    );
    // A git SHA and a long single-case identifier are not secrets.
    let sha = "9faaef2c1d3b4e5f60718293a4b5c6d7e8f90a1b";
    assert!(redact(&format!("commit {sha}")).contains(sha));
    let ident = "a_very_long_snake_case_identifier_name_here";
    assert!(redact(ident).contains(ident));
    assert!(!redact("key AbCdEf0123456789GhIjKl0123456789MnOp").contains("AbCdEf0123456789"));
}

#[test]
fn a_wrapper_in_front_of_the_driver_is_dropped_for_a_chat() {
    let argv: Vec<String> = ["vogt-agent-auth", "run", "--", "codex", "--x"]
        .map(String::from)
        .to_vec();
    assert_eq!(driver_of(&argv), ["codex", "--x"]);
    let bare: Vec<String> = vec!["/opt/bin/codex".into()];
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
fn the_launch_is_a_codex_command_the_operator_names() {
    let enabled = ChatPolicy {
        enabled: true,
        ..Default::default()
    };
    let templates = crate::config::SessionTemplate::default_templates();
    // The shipped Codex template runs the driver through the credential
    // wrapper; the chat runs the driver itself.
    let launch = resolve_codex(&enabled, &templates).unwrap();
    assert_eq!(launch.template.as_deref(), Some("Codex (protected)"));
    assert_eq!(launch.driver, ["codex"]);

    let named = ChatPolicy {
        command: Some(vec!["/opt/bin/codex".into()]),
        ..enabled.clone()
    };
    let launch = resolve_codex(&named, &templates).unwrap();
    assert_eq!(launch.template, None);
    assert_eq!(launch.driver, ["/opt/bin/codex"]);

    // The engine speaks only the Codex app-server protocol: anything else
    // means no chats.
    let other = ChatPolicy {
        command: Some(vec!["claude".into()]),
        ..enabled.clone()
    };
    assert!(resolve_codex(&other, &templates).is_none());
    // Off, or no template that runs Codex: no chats.
    assert!(resolve_codex(&ChatPolicy::default(), &templates).is_none());
    assert!(resolve_codex(&enabled, &[]).is_none());
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
