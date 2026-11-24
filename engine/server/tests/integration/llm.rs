use super::*;

#[tokio::test]
async fn the_assistant_answers_normally_for_a_model_this_transport_serves() {
    let mut cfg = test_config();
    cfg.assistant_api_key = Some("sk-test".into());
    let (base, _h) = boot_with_config(cfg).await;

    let res = reqwest::Client::new()
        .get(format!("{base}/api/assistant/history"))
        .headers(auth())
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), reqwest::StatusCode::OK);
}

#[tokio::test]
async fn a_chat_answers_switches_model_and_resumes_after_its_process_stops() {
    let Some((base, client, state, _guard, _bin)) = boot_chats().await else {
        return;
    };
    // Advertised, with the configured models.
    let config: Value = reqwest::get(format!("{base}/api/config"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(config["chat"]["drivers"][0]["name"], "codex", "{config}");
    assert_eq!(
        config["chat"]["drivers"][0]["models"][1]["id"],
        "deepseek-v4-pro"
    );

    let created: Value = client
        .post(format!("{base}/api/chats"))
        .json(&json!({ "work_item": "WI-1097" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["chat"]["id"].as_str().unwrap().to_string();
    assert_eq!(
        created["chat"]["model"], "grok-4.7",
        "the first model is the default"
    );
    assert_eq!(created["chat"]["creator"], "primary");

    let first = chat_send(&client, &base, &id, "is bedrock in sydney?").await;
    assert_eq!(first["finished"], true, "{first}");
    assert_eq!(
        texts(&first["entries"], "assistant"),
        ["grok-4.7: is bedrock in sydney?"]
    );
    assert_eq!(first["chat"]["title"], "is bedrock in sydney?");

    // Mid-chat switch: the live process is told, and says so on its next turn.
    let switched = client
        .post(format!("{base}/api/chats/{id}/model"))
        .json(&json!({ "model": "deepseek-v4-pro" }))
        .send()
        .await
        .unwrap();
    assert_eq!(switched.status(), StatusCode::OK);
    let second = chat_send(&client, &base, &id, "and now?").await;
    assert_eq!(
        texts(&second["entries"], "assistant"),
        ["deepseek-v4-pro: and now?"]
    );
    let refused = client
        .post(format!("{base}/api/chats/{id}/model"))
        .json(&json!({ "model": "gpt-unknown" }))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);

    // The process stops (an idle exit, a restart); the next message resumes
    // the same conversation by the chat's id, on the chosen model.
    let runtime = state.chats.clone().unwrap();
    let dir = state.config.state_dir.join("chats").join(&id);
    runtime_stop(&runtime, &id).await;
    let third = chat_send(&client, &base, &id, "still there?").await;
    assert_eq!(
        texts(&third["entries"], "assistant"),
        ["deepseek-v4-pro: still there?"]
    );
    let launches: Vec<Vec<String>> = std::fs::read_to_string(dir.join("launches.jsonl"))
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(launches.len(), 2, "{launches:?}");
    for argv in &launches {
        assert_eq!(
            argv.iter().rev().take(2).cloned().collect::<Vec<_>>(),
            ["--stdio", "app-server"],
            "{launches:?}"
        );
    }
    // The second launch resumed the thread rather than starting a new one.
    let requests: Vec<Value> = std::fs::read_to_string(dir.join("requests.jsonl"))
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let methods: Vec<&str> = requests
        .iter()
        .map(|r| r["method"].as_str().unwrap_or(""))
        .collect();
    assert!(methods.contains(&"thread/start"), "{requests:?}");
    assert!(methods.contains(&"thread/resume"), "{requests:?}");
    // The chat's own Codex home, regenerated at launch.
    assert!(std::fs::read_to_string(dir.join("codex-home/config.toml"))
        .unwrap()
        .contains("model_provider = \"litellm\""));
    assert!(dir.join("codex-home/vogt-model-catalog.json").exists());

    // Kept and found: listed, searchable by what was said, archivable.
    let found: Value = client
        .get(format!("{base}/api/chats?q=bedrock"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(found.as_array().unwrap().len(), 1, "{found}");
    assert_eq!(found[0]["message_count"], 6);
    client
        .post(format!("{base}/api/chats/{id}/archive"))
        .json(&json!({ "archived": true }))
        .send()
        .await
        .unwrap();
    let open: Value = client
        .get(format!("{base}/api/chats"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(open.as_array().unwrap().is_empty(), "{open}");
    let all: Value = client
        .get(format!("{base}/api/chats?archived=all"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(all.as_array().unwrap().len(), 1);
}

/// WI-3491/WI-3886: a provider registered after the engine started reaches the
/// next Codex pod launch — its tables in the pod bootstrap's `codex_config`
/// and its key name among the ondemand entries; removed, the next launch has
/// neither. The key value is never resolved into the bootstrap (WI-3886): the
/// session agent fetches it around the Codex child.
///
/// Codex runs only in a session pod (WI-3844), so this goes through the pod
/// harness's bootstrap rather than a host PTY.
#[tokio::test]
async fn a_registered_llm_provider_reaches_the_next_codex_launch_and_leaves_with_it() {
    let tmp = tempfile::tempdir().unwrap();
    let registry = tmp.path().join("llm-providers.toml");
    let template = SessionTemplate {
        name: "codex-pod".into(),
        description: "pod-backed Codex".into(),
        command: Some(vec!["codex".into()]),
        cwd: None,
        env: vec![],
        backend: "pod".into(),
        host_capable: false,
        pod_capable: false,
        account: None,
        default_name: None,
        match_repo_names: vec![],
        match_path_prefixes: vec![],
        tags: vec![],
    };
    let reg = registry.clone();
    let helper = tmp.path().join("agent-auth");
    std::fs::write(
        &helper,
        "#!/bin/sh\n[ \"$1\" = get ] || exit 64\nprintf 'value-for-%s' \"$2\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755)).unwrap();
    let helper_path = helper.clone();
    let mut h = pod_stream::harness_cfg(false, Some(vec![template]), move |cfg| {
        cfg.llm_providers_file = Some(reg);
        cfg.agent_auth_helper = helper_path;
        cfg.agent_auth_secrets = vogt_engine_server::secret_broker::parse_manifest(
            "MANIFEST_DS proj-a ds-secret optional",
        )
        .unwrap();
    })
    .await;

    async fn start(h: &mut pod_stream::Harness, name: &str) -> Value {
        let id = h
            .start_body(json!({
                "name": name,
                "template": "codex-pod",
                "placement": "pod",
            }))
            .await;
        let (created, token) = h.created.recv().await.expect("a pod is created");
        assert_eq!(created, id);
        reqwest::Client::new()
            .post(format!("{}/api/agent-auth/bootstrap", h.base))
            .bearer_auth(String::from_utf8_lossy(&token))
            .json(&json!({
                "session_id": id.to_string(),
                "graft_repo": "vogt",
                "graft_line": "rust-2",
                "cli": "codex",
            }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    }

    std::fs::write(
        &registry,
        "[[provider]]\nname = \"ds\"\nbase_url = \"http://litellm/v1\"\nwire_api = \"chat\"\nenv_key = \"DS_KEY\"\nsecret_project = \"proj-a\"\nsecret_name = \"ds-secret\"\n",
    )
    .unwrap();
    let with = start(&mut h, "registered").await;
    assert!(with["env"].get("DS_KEY").is_none(), "{with}");
    assert_eq!(
        with["env"]["AGENT_AUTH_ONDEMAND"].as_str(),
        Some("DS_KEY"),
        "{with}"
    );
    assert!(!with.to_string().contains("ds-value"), "{with}");
    let config: toml::Table = toml::from_str(with["codex_config"].as_str().unwrap()).unwrap();
    assert_eq!(
        config["model_providers"]["ds"]["env_key"].as_str(),
        Some("DS_KEY")
    );

    std::fs::write(&registry, "").unwrap();
    let without = start(&mut h, "removed").await;
    assert!(without["env"].get("DS_KEY").is_none(), "{without}");
    assert_eq!(
        without["env"]["AGENT_AUTH_ONDEMAND"].as_str(),
        Some(""),
        "{without}"
    );
    assert!(!without["codex_config"]
        .as_str()
        .unwrap_or_default()
        .contains("model_providers"));
}

/// A Codex launch picks the provider whose `models` list names the requested
/// model, not the first registered one (WI-4119), and resolves that provider's
/// secret at launch through the agent-auth broker.
#[tokio::test]
async fn a_codex_pod_launch_selects_the_provider_that_registers_its_model() {
    let tmp = tempfile::tempdir().unwrap();
    let registry = tmp.path().join("llm-providers.toml");
    let helper = tmp.path().join("agent-auth");
    std::fs::write(
        &helper,
        "#!/bin/sh\n[ \"$1\" = get ] || exit 64\nprintf 'value-for-%s' \"$2\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755)).unwrap();
    let template = SessionTemplate {
        name: "codex-pod".into(),
        description: "pod-backed Codex".into(),
        command: Some(vec!["codex".into()]),
        cwd: None,
        env: vec![],
        backend: "pod".into(),
        host_capable: false,
        pod_capable: false,
        account: None,
        default_name: None,
        match_repo_names: vec![],
        match_path_prefixes: vec![],
        tags: vec![],
    };
    let reg = registry.clone();
    let helper_path = helper.clone();
    let mut h = pod_stream::harness_cfg(false, Some(vec![template]), move |cfg| {
        cfg.llm_providers_file = Some(reg);
        cfg.agent_auth_helper = helper_path;
        cfg.agent_auth_secrets = vogt_engine_server::secret_broker::parse_manifest(
            "MANIFEST_DEEPSEEK proj-a DEEPSEEK_SECRET optional\nMANIFEST_CLAWBAY proj-a CLAWBAY_SECRET optional",
        )
        .unwrap();
    })
    .await;
    std::fs::write(
        &registry,
        r#"
[[provider]]
name = "litellm"
base_url = "http://litellm:4000/v1"
wire_api = "chat"
env_key = "DEEPSEEK_PROVIDER_KEY"
secret_project = "proj-a"
secret_name = "DEEPSEEK_SECRET"
models = ["deepseek/deepseek-v4-flash"]

[[provider]]
name = "grok"
base_url = "http://litellm:4000/v1"
wire_api = "chat"
env_key = "CLAWBAY_PROVIDER_KEY"
secret_project = "proj-a"
secret_name = "CLAWBAY_SECRET"
models = ["grok-4.7"]
"#,
    )
    .unwrap();

    let id = h
        .start_body(json!({
            "name": "grok-model",
            "template": "codex-pod",
            "placement": "pod",
            "model": "grok-4.7",
        }))
        .await;
    let (created, token) = h.created.recv().await.expect("a pod is created");
    assert_eq!(created, id);
    let bootstrap: Value = reqwest::Client::new()
        .post(format!("{}/api/agent-auth/bootstrap", h.base))
        .bearer_auth(String::from_utf8_lossy(&token))
        .json(&json!({
            "session_id": id.to_string(),
            "graft_repo": "vogt",
            "graft_line": "rust-2",
            "cli": "codex",
            "model": "grok-4.7",
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let config: toml::Table = toml::from_str(bootstrap["codex_config"].as_str().unwrap()).unwrap();
    assert_eq!(
        config["model_provider"].as_str(),
        Some("grok"),
        "{bootstrap}"
    );
    assert_eq!(config["model"].as_str(), Some("grok-4.7"));
    assert!(!bootstrap.to_string().contains("value-for-"), "{bootstrap}");
}

/// A provider whose secret cannot be resolved refuses the launch before a pod
/// exists, and the refusal names the secret — never a value (WI-4119).
#[tokio::test]
async fn a_codex_pod_launch_is_refused_when_the_provider_secret_is_missing() {
    let tmp = tempfile::tempdir().unwrap();
    let registry = tmp.path().join("llm-providers.toml");
    let helper = tmp.path().join("agent-auth");
    std::fs::write(
        &helper,
        "#!/bin/sh\n[ \"$1\" = get ] || exit 64\nprintf 'the secret %s is missing\\n' \"$2\" >&2\nexit 3\n",
    )
    .unwrap();
    std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755)).unwrap();
    let template = SessionTemplate {
        name: "codex-pod".into(),
        description: "pod-backed Codex".into(),
        command: Some(vec!["codex".into()]),
        cwd: None,
        env: vec![],
        backend: "pod".into(),
        host_capable: false,
        pod_capable: false,
        account: None,
        default_name: None,
        match_repo_names: vec![],
        match_path_prefixes: vec![],
        tags: vec![],
    };
    let reg = registry.clone();
    let helper_path = helper.clone();
    let mut h = pod_stream::harness_cfg(false, Some(vec![template]), move |cfg| {
        cfg.llm_providers_file = Some(reg);
        cfg.agent_auth_helper = helper_path;
        cfg.agent_auth_secrets = vogt_engine_server::secret_broker::parse_manifest(
            "MANIFEST_CLAWBAY proj-a CLAWBAY_SECRET optional",
        )
        .unwrap();
    })
    .await;
    std::fs::write(
        &registry,
        r#"
[[provider]]
name = "grok"
base_url = "http://litellm:4000/v1"
wire_api = "chat"
env_key = "CLAWBAY_PROVIDER_KEY"
secret_project = "proj-a"
secret_name = "CLAWBAY_SECRET"
models = ["grok-4.7"]
"#,
    )
    .unwrap();

    let response = reqwest::Client::new()
        .post(format!("{}/api/sessions", h.base))
        .headers(auth())
        .json(&json!({
            "name": "grok-missing-secret",
            "template": "codex-pod",
            "placement": "pod",
            "model": "grok-4.7",
        }))
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body = response.text().await.unwrap();
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.contains("CLAWBAY_SECRET"), "{body}");
    assert!(!body.contains("value-for-"), "{body}");
    assert!(
        h.created.try_recv().is_err(),
        "a refused launch made a pod: {body}"
    );
}

/// A requested model the provider registry does not list is refused before a
/// Codex pod exists, with a stable code that names the model (WI-4138).
#[tokio::test]
async fn an_unknown_model_is_refused_for_a_codex_launch() {
    let tmp = tempfile::tempdir().unwrap();
    let registry = tmp.path().join("llm-providers.toml");
    let helper = tmp.path().join("agent-auth");
    model_test_helper(&helper);
    let reg = registry.clone();
    let helper_path = helper.clone();
    let mut h = pod_stream::harness_cfg(
        false,
        Some(vec![model_test_template("codex-pod", "codex")]),
        move |cfg| {
            cfg.llm_providers_file = Some(reg);
            cfg.agent_auth_helper = helper_path;
            cfg.agent_auth_secrets = model_test_manifest();
        },
    )
    .await;
    write_model_registry(&registry);

    let response = reqwest::Client::new()
        .post(format!("{}/api/sessions", h.base))
        .headers(auth())
        .json(&json!({
            "name": "unknown-codex-model",
            "template": "codex-pod",
            "placement": "pod",
            "model": "acceptance/definitely-not-a-model",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["code"].as_str(), Some("unknown_model"), "{body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("acceptance/definitely-not-a-model"),
        "{body}"
    );
    assert!(
        h.created.try_recv().is_err(),
        "a refused launch made a pod: {body}"
    );
}

/// A name Claude Code would never resolve and no provider lists is still
/// refused for a Claude template, before the pod is created (WI-4138).
#[tokio::test]
async fn an_unknown_model_is_refused_for_a_claude_launch() {
    let tmp = tempfile::tempdir().unwrap();
    let registry = tmp.path().join("llm-providers.toml");
    let reg = registry.clone();
    let mut h = pod_stream::harness_cfg(
        false,
        Some(vec![model_test_template("claude-pod", "claude")]),
        move |cfg| {
            cfg.llm_providers_file = Some(reg);
        },
    )
    .await;
    write_model_registry(&registry);

    let response = reqwest::Client::new()
        .post(format!("{}/api/sessions", h.base))
        .headers(auth())
        .json(&json!({
            "name": "unknown-claude-model",
            "template": "claude-pod",
            "placement": "pod",
            "model": "not-a-model-xyz",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["code"].as_str(), Some("unknown_model"), "{body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("not-a-model-xyz"),
        "{body}"
    );
    assert!(
        h.created.try_recv().is_err(),
        "a refused launch made a pod: {body}"
    );
}

/// Vogt's own dispatch profiles send Claude Code's native names on the
/// `claude` template: the aliases `opus`, `sonnet` and `haiku`, and
/// `claude-…` ids, each optionally with Claude Code's `[1m]` context suffix.
/// Those are names Claude Code resolves itself and no provider registry ever
/// lists, so a registry that lists other models must not refuse them
/// (WI-4138).
#[tokio::test]
async fn claude_native_models_are_accepted_against_a_listing_registry() {
    let tmp = tempfile::tempdir().unwrap();
    let registry = tmp.path().join("llm-providers.toml");
    let reg = registry.clone();
    let mut h = pod_stream::harness_cfg(
        false,
        Some(vec![model_test_template("claude-pod", "claude")]),
        move |cfg| {
            cfg.llm_providers_file = Some(reg);
        },
    )
    .await;
    write_model_registry(&registry);

    for model in [
        "opus",
        "sonnet",
        "haiku",
        "fable",
        "claude-opus-5-5",
        "claude-sonnet-5-5",
        "claude-haiku-4-5",
        "opus[1m]",
        "sonnet[1m]",
        "claude-sonnet-4-5[1m]",
    ] {
        let id = h
            .start_body(json!({
                "name": "claude-native-model",
                "template": "claude-pod",
                "placement": "pod",
                "model": model,
            }))
            .await;
        let (created, _token) = h.created.recv().await.expect("the pod is created");
        assert_eq!(created, id, "{model}");
    }
}

/// Every model Vogt's own dispatch profiles and templates send is admitted by
/// a registry shaped the way `docs/DEPLOYMENT.md` recommends (WI-4131): the
/// Codex template's LiteLLM ids and the Claude template's `claude-org-*`
/// router groups (WI-4138).
#[tokio::test]
async fn models_vogt_sends_are_accepted_by_the_recommended_registry() {
    let tmp = tempfile::tempdir().unwrap();
    let registry = tmp.path().join("llm-providers.toml");
    let helper = tmp.path().join("agent-auth");
    model_test_helper(&helper);
    let reg = registry.clone();
    let helper_path = helper.clone();
    let mut h = pod_stream::harness_cfg(
        false,
        Some(vec![
            model_test_template("codex-pod", "codex"),
            model_test_template("claude-pod", "claude"),
        ]),
        move |cfg| {
            cfg.llm_providers_file = Some(reg);
            cfg.agent_auth_helper = helper_path;
            cfg.agent_auth_secrets = model_test_manifest();
        },
    )
    .await;
    write_model_registry(&registry);

    for (template, model) in [
        ("codex-pod", "deepseek/deepseek-v4-flash"),
        ("codex-pod", "grok-4.7"),
        ("codex-pod", "grok-4.6"),
        ("codex-pod", "claude-org-haiku"),
        ("codex-pod", "claude-org-sonnet"),
        ("codex-pod", "claude-org-opus"),
        ("claude-pod", "claude-org-haiku"),
        ("claude-pod", "claude-org-sonnet"),
        ("claude-pod", "claude-org-opus"),
    ] {
        let id = h
            .start_body(json!({
                "name": "vogt-model",
                "template": template,
                "placement": "pod",
                "model": model,
            }))
            .await;
        let (created, _token) = h.created.recv().await.expect("the pod is created");
        assert_eq!(created, id, "{template} {model}");
    }
}

/// A registered model and an omitted model still launch; the guard only
/// refuses a model the registry cannot serve (WI-4138).
#[tokio::test]
async fn known_and_default_models_still_launch() {
    let tmp = tempfile::tempdir().unwrap();
    let registry = tmp.path().join("llm-providers.toml");
    let helper = tmp.path().join("agent-auth");
    model_test_helper(&helper);
    let reg = registry.clone();
    let helper_path = helper.clone();
    let mut h = pod_stream::harness_cfg(
        false,
        Some(vec![
            model_test_template("codex-pod", "codex"),
            model_test_template("claude-pod", "claude"),
        ]),
        move |cfg| {
            cfg.llm_providers_file = Some(reg);
            cfg.agent_auth_helper = helper_path;
            cfg.agent_auth_secrets = model_test_manifest();
        },
    )
    .await;
    write_model_registry(&registry);

    for body in [
        json!({
            "name": "known-codex",
            "template": "codex-pod",
            "placement": "pod",
            "model": "deepseek/deepseek-v4-flash",
        }),
        json!({
            "name": "default-codex",
            "template": "codex-pod",
            "placement": "pod",
        }),
        json!({
            "name": "known-claude",
            "template": "claude-pod",
            "placement": "pod",
            "model": "claude-org-sonnet",
        }),
    ] {
        let id = h.start_body(body.clone()).await;
        let (created, _token) = h.created.recv().await.expect("the pod is created");
        assert_eq!(created, id, "{body}");
    }
}

/// The documented allowlist admits a model the registry entry does not name,
/// for a deployment whose provider serves it without listing it (WI-4138).
#[tokio::test]
async fn the_model_allowlist_admits_an_unregistered_model() {
    let tmp = tempfile::tempdir().unwrap();
    let registry = tmp.path().join("llm-providers.toml");
    let reg = registry.clone();
    let mut h = pod_stream::harness_cfg(
        false,
        Some(vec![model_test_template("claude-pod", "claude")]),
        move |cfg| {
            cfg.llm_providers_file = Some(reg);
            cfg.llm_model_allowlist = vec!["estate-private-model".into()];
        },
    )
    .await;
    write_model_registry(&registry);

    let id = h
        .start_body(json!({
            "name": "allowlisted-claude",
            "template": "claude-pod",
            "placement": "pod",
            "model": "estate-private-model",
        }))
        .await;
    let (created, _token) = h.created.recv().await.expect("the pod is created");
    assert_eq!(created, id);
}

#[tokio::test]
async fn llm_provider_handlers_write_and_remove_the_registry() {
    let tmp = tempfile::tempdir().unwrap();
    let registry = tmp.path().join("llm-providers.toml");
    let mut cfg = test_config();
    cfg.llm_providers_file = Some(registry.clone());
    cfg.agent_auth_secrets = vogt_engine_server::secret_broker::parse_manifest(
        "PROVIDER_KEY approved-project PROVIDER_KEY ondemand",
    )
    .unwrap();
    let (base, _state, _guard) = boot_with_state(cfg).await;
    let client = reqwest::Client::new();

    let response = client
        .post(format!("{base}/api/llm-providers"))
        .headers(auth())
        .json(&provider_request("test-provider", "TEST_PROVIDER_KEY"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let written = std::fs::read_to_string(&registry).unwrap();
    assert!(written.contains("test-provider"));
    assert!(written.contains("TEST_PROVIDER_KEY"));

    let response = client
        .post(format!("{base}/api/llm-providers/remove"))
        .headers(auth())
        .json(&json!({"name": "test-provider"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(!std::fs::read_to_string(&registry)
        .unwrap()
        .contains("test-provider"));
}

/// WI-3672: a panic while the registry lock was held poisoned it, and every
/// later `llm_provider_set` answered a bare, unlogged 500.
#[tokio::test]
async fn llm_provider_set_survives_a_poisoned_registry_lock() {
    let tmp = tempfile::tempdir().unwrap();
    let registry = tmp.path().join("llm-providers.toml");
    let mut cfg = test_config();
    cfg.llm_providers_file = Some(registry.clone());
    cfg.agent_auth_secrets = vogt_engine_server::secret_broker::parse_manifest(
        "PROVIDER_KEY approved-project PROVIDER_KEY ondemand",
    )
    .unwrap();
    let (base, _state, _guard) = boot_with_state(cfg).await;
    let _ = std::thread::spawn(|| {
        let _held = vogt_engine_server::llm_providers::registry_lock()
            .lock()
            .unwrap();
        panic!("poison the registry lock");
    })
    .join();
    let client = reqwest::Client::new();
    let ok = client
        .post(format!("{base}/api/llm-providers"))
        .headers(auth())
        .json(&provider_request("first", "FIRST_KEY"))
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status(), StatusCode::OK);
    // A second provider sharing the env_key is a clear 4xx, never a 500.
    let dup = client
        .post(format!("{base}/api/llm-providers"))
        .headers(auth())
        .json(&provider_request("second", "FIRST_KEY"))
        .send()
        .await
        .unwrap();
    assert_eq!(dup.status(), StatusCode::BAD_REQUEST);
    let body = dup.text().await.unwrap();
    assert!(body.contains("env_key"), "{body}");
}

#[tokio::test]
async fn llm_provider_handler_refuses_reserved_names_and_manifest_collisions() {
    let tmp = tempfile::tempdir().unwrap();
    let registry = tmp.path().join("llm-providers.toml");
    let mut cfg = test_config();
    cfg.llm_providers_file = Some(registry.clone());
    cfg.agent_auth_secrets = vogt_engine_server::secret_broker::parse_manifest(
        "MANIFEST_KEY approved-project PROVIDER_KEY ondemand",
    )
    .unwrap();
    let (base, _state, _guard) = boot_with_state(cfg).await;
    let client = reqwest::Client::new();

    for request in [
        provider_request("reserved", "HOME"),
        provider_request("collision", "MANIFEST_KEY"),
    ] {
        let response = client
            .post(format!("{base}/api/llm-providers"))
            .headers(auth())
            .json(&request)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
    assert!(!registry.exists());
}

#[tokio::test]
async fn llm_provider_handler_returns_bad_request_for_registry_collisions() {
    let tmp = tempfile::tempdir().unwrap();
    let registry = tmp.path().join("llm-providers.toml");
    let mut cfg = test_config();
    cfg.llm_providers_file = Some(registry.clone());
    cfg.agent_auth_secrets = vogt_engine_server::secret_broker::parse_manifest(
        "PROVIDER_KEY approved-project PROVIDER_KEY ondemand",
    )
    .unwrap();
    let (base, _state, _guard) = boot_with_state(cfg).await;
    let client = reqwest::Client::new();

    let first = client
        .post(format!("{base}/api/llm-providers"))
        .headers(auth())
        .json(&provider_request("first", "FIRST_PROVIDER_KEY"))
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);

    for (name, request) in [
        (
            "same-key",
            provider_request("same-key", "FIRST_PROVIDER_KEY"),
        ),
        (
            "same-secret",
            provider_request("same-secret", "SECOND_PROVIDER_KEY"),
        ),
    ] {
        let response = client
            .post(format!("{base}/api/llm-providers"))
            .headers(auth())
            .json(&request)
            .send()
            .await
            .unwrap();
        let status = response.status();
        let body: Value = response.json().await.unwrap();
        assert_eq!(status, StatusCode::BAD_REQUEST, "{name}: {body}");
        assert!(body["error"]
            .as_str()
            .unwrap()
            .contains("already another provider"));
    }
    // A refused request leaves the registry exactly as it was: a bad request
    // never half-writes a registry the engine would then refuse to load.
    let written = std::fs::read_to_string(&registry).unwrap();
    assert!(written.contains("first"));
    assert!(!written.contains("same-key") && !written.contains("same-secret"));
}

#[tokio::test]
async fn llm_provider_handler_registers_distinct_approved_manifest_secrets() {
    let tmp = tempfile::tempdir().unwrap();
    let registry = tmp.path().join("llm-providers.toml");
    let mut cfg = test_config();
    cfg.llm_providers_file = Some(registry.clone());
    cfg.agent_auth_secrets = vogt_engine_server::secret_broker::parse_manifest(
        "FIRST_KEY approved-project FIRST_SECRET ondemand\nSECOND_KEY approved-project SECOND_SECRET ondemand",
    )
    .unwrap();
    let (base, _state, _guard) = boot_with_state(cfg).await;
    let client = reqwest::Client::new();

    let first = client
        .post(format!("{base}/api/llm-providers"))
        .headers(auth())
        .json(&provider_request_with_secret(
            "first",
            "FIRST_PROVIDER_KEY",
            "FIRST_SECRET",
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);

    // The second provider may use another approved manifest secret while its
    // env_key remains its own namespace.  The manifest VARs are deliberately
    // different from both provider env_keys: a provider must not shadow a
    // deployment-owned variable.
    let second = client
        .post(format!("{base}/api/llm-providers"))
        .headers(auth())
        .json(&provider_request_with_secret(
            "second",
            "SECOND_PROVIDER_KEY",
            "SECOND_SECRET",
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), StatusCode::OK);

    let listed: Value = client
        .get(format!("{base}/api/llm-providers"))
        .headers(auth())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(listed["providers"].as_array().unwrap().len(), 2);
    assert!(listed["providers"]
        .as_array()
        .unwrap()
        .iter()
        .all(|provider| provider["state"] == "active"));

    let removed = client
        .post(format!("{base}/api/llm-providers/remove"))
        .headers(auth())
        .json(&json!({"name": "second"}))
        .send()
        .await
        .unwrap();
    assert_eq!(removed.status(), StatusCode::OK);
    assert_eq!(
        removed.json::<Value>().await.unwrap()["removed"],
        Value::Bool(true)
    );
}

#[tokio::test]
async fn llm_provider_handler_refuses_unlisted_and_shared_manifest_secrets() {
    let tmp = tempfile::tempdir().unwrap();
    let registry = tmp.path().join("llm-providers.toml");
    let mut cfg = test_config();
    cfg.llm_providers_file = Some(registry);
    cfg.agent_auth_secrets = vogt_engine_server::secret_broker::parse_manifest(
        "FIRST_KEY approved-project FIRST_SECRET ondemand\nSECOND_KEY approved-project SECOND_SECRET ondemand",
    )
    .unwrap();
    let (base, _state, _guard) = boot_with_state(cfg).await;
    let client = reqwest::Client::new();

    let first = client
        .post(format!("{base}/api/llm-providers"))
        .headers(auth())
        .json(&provider_request_with_secret(
            "first",
            "FIRST_PROVIDER_KEY",
            "FIRST_SECRET",
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);

    let shared = client
        .post(format!("{base}/api/llm-providers"))
        .headers(auth())
        .json(&provider_request_with_secret(
            "shared",
            "SHARED_PROVIDER_KEY",
            "FIRST_SECRET",
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(shared.status(), StatusCode::BAD_REQUEST);
    assert!(shared
        .text()
        .await
        .unwrap()
        .contains("already another provider"));

    let unlisted = client
        .post(format!("{base}/api/llm-providers"))
        .headers(auth())
        .json(&provider_request_with_secret(
            "unlisted",
            "UNLISTED_PROVIDER_KEY",
            "NOT_IN_MANIFEST",
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(unlisted.status(), StatusCode::BAD_REQUEST);
    assert!(unlisted
        .text()
        .await
        .unwrap()
        .contains("is not in the approved agent-auth manifest"));
}

/// WI-3699, A5 Step 1 shape: the deployment manifest holds several provider
/// secrets in one project, an existing provider already owns one of them, and
/// the second temporary provider must be able to take a *different* approved
/// secret under its own env_key.  The manifest VARs are deliberately not the
/// provider env_keys, as in the deployment.
#[tokio::test]
async fn llm_provider_handler_registers_an_a5_shaped_temporary_provider() {
    let tmp = tempfile::tempdir().unwrap();
    let registry = tmp.path().join("llm-providers.toml");
    let mut cfg = test_config();
    cfg.llm_providers_file = Some(registry);
    cfg.agent_auth_secrets = vogt_engine_server::secret_broker::parse_manifest(
        "HOMELAB_LITELLM_SESSION_KEY approved-project HOMELAB_LITELLM_SESSION_KEY\n\
         HOMELAB_LITELLM_CLAWBAY_KEY approved-project HOMELAB_LITELLM_CLAWBAY_KEY ondemand\n\
         HOMELAB_LITELLM_DEEPSEEK_KEY approved-project HOMELAB_LITELLM_DEEPSEEK_KEY ondemand",
    )
    .unwrap();
    let (base, _state, _guard) = boot_with_state(cfg).await;
    let client = reqwest::Client::new();

    let existing = client
        .post(format!("{base}/api/llm-providers"))
        .headers(auth())
        .json(&provider_request_with_secret(
            "litellm-deepseek",
            "LITELLM_DEEPSEEK_PROVIDER_KEY",
            "HOMELAB_LITELLM_DEEPSEEK_KEY",
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(existing.status(), StatusCode::OK);

    // The refused A5 registration: a distinct provider, a distinct env_key and
    // another approved manifest secret.  Nothing about the request shadows a
    // deployment variable, so it must be accepted.
    let temporary = client
        .post(format!("{base}/api/llm-providers"))
        .headers(auth())
        .json(&provider_request_with_secret(
            "a5-temp",
            "A5_TEMP_PROVIDER_KEY",
            "HOMELAB_LITELLM_CLAWBAY_KEY",
        ))
        .send()
        .await
        .unwrap();
    let temporary_status = temporary.status();
    let temporary_text = temporary.text().await.unwrap();
    assert_eq!(temporary_status, StatusCode::OK, "body: {temporary_text}");
    let body: Value = serde_json::from_str(&temporary_text).unwrap();
    assert_eq!(body["provider"]["state"], "active");
    assert_eq!(body["provider"]["env_key"], "A5_TEMP_PROVIDER_KEY");
    assert_eq!(
        body["provider"]["secret_name"],
        "HOMELAB_LITELLM_CLAWBAY_KEY"
    );

    // Using that secret's manifest VAR as the env_key is still refused: the
    // registry would add a second manifest line for the same variable.
    let shadowing = client
        .post(format!("{base}/api/llm-providers"))
        .headers(auth())
        .json(&provider_request_with_secret(
            "a5-shadow",
            "HOMELAB_LITELLM_CLAWBAY_KEY",
            "HOMELAB_LITELLM_CLAWBAY_KEY",
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(shadowing.status(), StatusCode::BAD_REQUEST);
    assert!(shadowing
        .text()
        .await
        .unwrap()
        .contains("clashes with manifest VAR"));

    // The existing provider's secret stays exclusive to it.
    let shared = client
        .post(format!("{base}/api/llm-providers"))
        .headers(auth())
        .json(&provider_request_with_secret(
            "a5-shared",
            "LITELLM_SHARED_PROVIDER_KEY",
            "HOMELAB_LITELLM_DEEPSEEK_KEY",
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(shared.status(), StatusCode::BAD_REQUEST);
    assert!(shared
        .text()
        .await
        .unwrap()
        .contains("already another provider's"));

    let listed: Value = client
        .get(format!("{base}/api/llm-providers"))
        .headers(auth())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(listed["providers"].as_array().unwrap().len(), 2);
    assert!(listed["providers"]
        .as_array()
        .unwrap()
        .iter()
        .all(|provider| provider["state"] == "active"));

    // The exact request behind the A5 report: the same approved secret name,
    // but with the project id from the compose-era `personal/vogt-dev`
    // overlay instead of the id in this deployment's mounted manifest.  The
    // refusal is correct policy, and it names the reference so an unapproved
    // project id cannot be mistaken for an env_key rule.
    let mut wrong_project = provider_request_with_secret(
        "a5-temp",
        "A5_TEMP_PROVIDER_KEY",
        "HOMELAB_LITELLM_CLAWBAY_KEY",
    );
    wrong_project["secret_project"] = json!("76b1ebe1-3656-4cef-952c-30d5d489c6e7");
    let refused = client
        .post(format!("{base}/api/llm-providers"))
        .headers(auth())
        .json(&wrong_project)
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
    let refused_body = refused.text().await.unwrap();
    assert!(
        refused_body.contains("a5-temp")
            && refused_body
                .contains("76b1ebe1-3656-4cef-952c-30d5d489c6e7/HOMELAB_LITELLM_CLAWBAY_KEY"),
        "body: {refused_body}"
    );

    let removed = client
        .post(format!("{base}/api/llm-providers/remove"))
        .headers(auth())
        .json(&json!({"name": "a5-temp"}))
        .send()
        .await
        .unwrap();
    assert_eq!(removed.status(), StatusCode::OK);
    assert_eq!(
        removed.json::<Value>().await.unwrap()["removed"],
        Value::Bool(true)
    );
}

#[tokio::test]
async fn llm_provider_handlers_refuse_a_non_admin_identity() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = test_config();
    cfg.token = None;
    cfg.vogt_core_url = Some(
        stand_in_core_knowing(vec![(TEST_TOKEN, "agent:provider-test", vec!["sessions"])]).await,
    );
    cfg.vogt_core_token = Some("stack-secret-provider-test".into());
    cfg.llm_providers_file = Some(tmp.path().join("llm-providers.toml"));
    let (base, _state, _guard) = boot_with_state(cfg).await;
    let response = reqwest::Client::new()
        .post(format!("{base}/api/llm-providers"))
        .headers(auth())
        .json(&provider_request("not-admin", "TEST_PROVIDER_KEY"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}
