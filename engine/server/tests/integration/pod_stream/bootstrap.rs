use super::*;

#[tokio::test]
async fn pod_env_and_bootstrap_carry_the_contract_fields() {
    let mut h = harness(false).await;
    let id = h.start().await;
    let (_, token) = h.created.recv().await.unwrap();
    let token = String::from_utf8_lossy(&token).into_owned();

    let pod = h.launcher.pods.lock().unwrap().pop().expect("pod created");
    let spec_env: std::collections::BTreeMap<String, String> = pod.spec.unwrap().containers[0]
        .env
        .clone()
        .unwrap()
        .into_iter()
        .filter_map(|v| v.value.map(|value| (v.name, value)))
        .collect();

    let body: Value = h
        .bootstrap(id, token.as_bytes())
        .await
        .json()
        .await
        .unwrap();
    assert!(body["session_token"]
        .as_str()
        .is_some_and(|t| !t.is_empty()));
    assert!(body["brief"].is_string());
    assert!(body["bundle"].is_object());
    for name in [
        "VOGT_ENGINE_URL",
        "VOGT_SESSION_ID",
        "VOGT_BOOTSTRAP_TOKEN_FILE",
        "VOGT_SOURCE_JSON",
        "VOGT_CLI",
    ] {
        assert!(spec_env.contains_key(name), "pod spec.env lacks {name}");
        if name == "VOGT_BOOTSTRAP_TOKEN_FILE" {
            continue;
        }
        assert!(
            body["env"][name].as_str().is_some_and(|v| !v.is_empty()),
            "bootstrap env lacks {name}"
        );
    }
    assert_eq!(body["env"]["VOGT_ENGINE_URL"], spec_env["VOGT_ENGINE_URL"]);
    assert_eq!(spec_env["VOGT_SESSION_ID"], id.to_string());
    assert_eq!(body["env"]["VOGT_SESSION_ID"], id.to_string());
    // The credential is not in the pod spec at all, only the path of the
    // file the per-session Secret is mounted at.
    assert_eq!(
        spec_env["VOGT_BOOTSTRAP_TOKEN_FILE"],
        "/run/vogt-bootstrap/token"
    );
    assert!(!spec_env.contains_key("VOGT_BOOTSTRAP_TOKEN"));
    assert!(!spec_env.contains_key("VOGT_AGENT_BOOTSTRAP_TOKEN"));
    assert!(
        !spec_env.values().any(|v| v.contains(&token)),
        "bootstrap token in the pod env"
    );
    assert!(body["env"].get("VOGT_BOOTSTRAP_TOKEN").is_none());
    assert!(!body.to_string().contains(&token));
    // The pod spec and the bootstrap carry the same source descriptor:
    // the deployment's default, as this session's project names none.
    let spec_source: Value = serde_json::from_str(&spec_env["VOGT_SOURCE_JSON"]).unwrap();
    let boot_source: Value =
        serde_json::from_str(body["env"]["VOGT_SOURCE_JSON"].as_str().unwrap()).unwrap();
    assert_eq!(spec_source, boot_source);
    assert_eq!(body["source"], spec_source);
    assert_eq!(spec_source["location"]["repo"], "vogt");
    assert_eq!(spec_source["ref"], "rust-2");
}

/// WI-3884: the bootstrap env names the brokered model and the names the
/// deployment declared and this session holds — never a value.
#[tokio::test]
async fn pod_bootstrap_carries_the_agent_auth_breadcrumbs() {
    let tmp = tempfile::tempdir().unwrap();
    let mut h = harness_cfg(false, None, |cfg| {
        cfg.agent_auth_helper = fake_broker_helper(tmp.path());
        cfg.agent_auth_secrets = vogt_engine_server::secret_broker::parse_manifest(
            "F3_ONDEMAND acct f3-ondemand ondemand\n\
                 F3_WRITABLE acct f3-writable ondemand,writable\n",
        )
        .unwrap();
    })
    .await;
    let id = h.start().await;
    let now = OffsetDateTime::now_utc();
    h.state
        .sessions
        .secret_broker()
        .apply_grant(
            id,
            vogt_engine_contract::SessionGrant {
                grant_id: "grt_f3_1".into(),
                var: "F3_GRANTED".into(),
                project_id: "acct".into(),
                secret_name: "f3-granted".into(),
                uses: vogt_engine_contract::GrantUses::Ttl,
                expires_at: (now + time::Duration::hours(1))
                    .format(&time::format_description::well_known::Rfc3339)
                    .unwrap(),
                reason: "the pod needs it".into(),
            },
            now,
        )
        .unwrap();
    let (_, boot) = h.created.recv().await.unwrap();
    let body: Value = h.bootstrap(id, boot.as_slice()).await.json().await.unwrap();
    assert_eq!(body["env"]["AGENT_AUTH_MODE"], "brokered");
    assert_eq!(body["env"]["AGENT_AUTH_GRANTED"], "F3_GRANTED");
    // `writable` is orthogonal to the read policy, so the
    // `ondemand,writable` entry is listed by both names, as on the host.
    assert_eq!(
        body["env"]["AGENT_AUTH_ONDEMAND"],
        "F3_ONDEMAND F3_WRITABLE"
    );
    assert_eq!(body["env"]["AGENT_AUTH_WRITABLE"], "F3_WRITABLE");
    // Names only: the fake helper's values are not in the response.
    assert!(!body.to_string().contains("value-of-"), "{body}");
}

/// WI-3884: the pod's stream credential is the broker credential for
/// `fetch` and `grants` — scoped to the manifest and to that session's own
/// approved grants, and every fetch is audited by name.
#[tokio::test]
async fn a_pod_stream_token_fetches_the_manifest_and_only_its_own_grant() {
    let logs = LogCapture::default();
    let _logging = tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .with_writer(logs.clone())
            .with_ansi(false)
            .with_max_level(tracing::Level::TRACE)
            .finish(),
    );
    let tmp = tempfile::tempdir().unwrap();
    let mut h = harness_cfg(false, None, |cfg| {
        cfg.agent_auth_helper = fake_broker_helper(tmp.path());
        cfg.agent_auth_secrets = vogt_engine_server::secret_broker::parse_manifest(
            "POD_ONDEMAND acct pod-ondemand ondemand\n",
        )
        .unwrap();
    })
    .await;
    let id = h.start().await;
    let token = h.session_token(id).await;
    let other = h.start().await;
    let other_token = h.session_token(other).await;

    // A person-approved grant for the *other* session only.
    let now = OffsetDateTime::now_utc();
    h.state
        .sessions
        .secret_broker()
        .apply_grant(
            other,
            vogt_engine_contract::SessionGrant {
                grant_id: "grt_pod_1".into(),
                var: "POD_GRANT".into(),
                project_id: "acct".into(),
                secret_name: "pod-grant".into(),
                uses: vogt_engine_contract::GrantUses::Ttl,
                expires_at: (now + time::Duration::hours(1))
                    .format(&time::format_description::well_known::Rfc3339)
                    .unwrap(),
                reason: "the other pod needs it".into(),
            },
            now,
        )
        .unwrap();

    // `set_default` is thread-local and does not rebuild the global
    // callsite interest cache; a sibling test that hits the broker
    // handlers first can cache them as "never" for this thread. Rebuild
    // it now that this thread's subscriber is installed.
    tracing::callsite::rebuild_interest_cache();
    let plain = reqwest::Client::new();
    let fetch = |token: &str, var: &str| {
        plain
            .post(format!("{}/api/agent-auth/fetch/{var}", h.base))
            .bearer_auth(token.to_string())
            .send()
    };

    // Its own manifest entry: the value, verbatim.
    let got = fetch(&token, "POD_ONDEMAND").await.unwrap();
    let status = got.status();
    let body = got.text().await.unwrap();
    assert_eq!(status, 200, "{body}");
    assert_eq!(body, "value-of-POD_ONDEMAND");

    // Another session's grant is not reachable through this stream token.
    let refused = fetch(&token, "POD_GRANT").await.unwrap();
    assert_eq!(refused.status(), 403);
    let error = refused.text().await.unwrap();
    assert!(error.contains("POD_GRANT"), "{error}");
    assert!(error.contains("ENGINE_AGENT_AUTH_SECRETS"), "{error}");

    // A name the deployment never declared is refused with the fix.
    assert_eq!(fetch(&token, "NOT_DECLARED").await.unwrap().status(), 403);

    // The owning session's stream token reaches its own grant.
    let got = fetch(&other_token, "POD_GRANT").await.unwrap();
    assert_eq!(got.status(), 200);
    assert_eq!(got.text().await.unwrap(), "value-of-POD_GRANT");

    // `grants` lists each session its own grant, never a value.
    let listed: Vec<Value> = plain
        .get(format!("{}/api/agent-auth/grants", h.base))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(listed.is_empty(), "{listed:?}");
    let listed: Vec<Value> = plain
        .get(format!("{}/api/agent-auth/grants", h.base))
        .bearer_auth(&other_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["var"], "POD_GRANT");
    assert!(!listed[0].to_string().contains("value-of-"));

    // The fetch is audited by session and name, never by value.
    let captured = || String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
    until("the fetch audit line", || {
        let logged = captured();
        logged.contains("vogt::audit")
            && logged.contains(&id.to_string())
            && logged.contains("POD_ONDEMAND")
    })
    .await;
    let logged = captured();
    assert!(!logged.contains("value-of-POD_ONDEMAND"), "{logged}");
    assert!(!logged.contains("value-of-POD_GRANT"), "{logged}");
}

/// WI-3883: a pod started from a template's own pod default (no explicit
/// placement) carries the caller's extra environment to the agent through
/// the bootstrap response's `agent_env`, never through the pod spec and
/// never through the response's own `env` (which the entrypoint exports
/// before it clones).
#[tokio::test]
async fn a_pod_session_carries_extra_env_through_the_bootstrap_response() {
    let mut h = harness(false).await;
    let id = h
        .start_body(json!({
            "name": "pod-env",
            "template": "pod",
            "env": [["CARGO_BUILD_JOBS", "4"], ["RUST_LOG", "info"]],
        }))
        .await;
    let (_, token) = h.created.recv().await.unwrap();
    let pod = h.launcher.pods.lock().unwrap().pop().expect("pod created");
    let spec_env: std::collections::BTreeMap<String, String> = pod.spec.unwrap().containers[0]
        .env
        .clone()
        .unwrap()
        .into_iter()
        .filter_map(|v| v.value.map(|value| (v.name, value)))
        .collect();
    assert!(
        !spec_env.contains_key("CARGO_BUILD_JOBS"),
        "a caller env value must not land in the pod spec"
    );
    let body: Value = h.bootstrap(id, &token).await.json().await.unwrap();
    assert_eq!(body["agent_env"]["CARGO_BUILD_JOBS"], "4");
    assert_eq!(body["agent_env"]["RUST_LOG"], "info");
    assert!(
        body["env"].get("CARGO_BUILD_JOBS").is_none(),
        "a caller value must not be in the env the entrypoint exports before cloning: {body}"
    );
    assert!(
        body["env"].get("RUST_LOG").is_none(),
        "a caller value must not be in the env the entrypoint exports before cloning: {body}"
    );
}

/// WI-3883: a cwd inside the project's checkout becomes the pod's work
/// directory, as a path under the clone root the entrypoint checks out.
#[tokio::test]
async fn a_pod_session_runs_in_the_cwd_inside_the_clone() {
    let clone = tempfile::tempdir().unwrap();
    let repo = clone.path().join("repo");
    let sub = repo.join("engine");
    std::fs::create_dir_all(&sub).unwrap();
    let init = std::process::Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .current_dir(&repo)
        .status()
        .unwrap();
    assert!(init.success(), "git init");
    let sub = sub.canonicalize().unwrap();

    let mut h = harness(false).await;
    let id = h
        .start_body(json!({
            "name": "pod-cwd",
            "template": "pod",
            "cwd": sub.to_string_lossy(),
        }))
        .await;
    let (_, token) = h.created.recv().await.unwrap();
    let body: Value = h.bootstrap(id, &token).await.json().await.unwrap();
    assert_eq!(body["env"]["VOGT_WORK_DIR"], "/work/engine");
}

/// WI-3883: a cwd outside every checkout is not something the pod can
/// honour, so the start is refused and the field is named.
#[tokio::test]
async fn a_pod_cwd_outside_the_clone_is_refused_and_names_the_field() {
    let outside = tempfile::tempdir().unwrap();
    let mut h = harness(false).await;
    let response = h
        .client
        .post(format!("{}/api/sessions", h.base))
        .json(&json!({
            "name": "pod-cwd-escape",
            "template": "pod",
            "cwd": outside.path().to_string_lossy(),
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = response.text().await.unwrap();
    assert!(body.contains("cwd"), "the refusal names the field: {body}");
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), h.created.recv())
            .await
            .is_err(),
        "a refused start must not create a pod"
    );
}

/// WI-3883: an environment name the engine or the pod owns is refused by
/// name rather than shadowed or dropped.
#[tokio::test]
async fn a_pod_session_refuses_a_reserved_env_name_and_names_the_field() {
    let mut h = harness(false).await;
    let response = h
        .client
        .post(format!("{}/api/sessions", h.base))
        .json(&json!({
            "name": "pod-env-reserved",
            "template": "pod",
            "env": [["VOGT_WORK_DIR", "/elsewhere"]],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = response.text().await.unwrap();
    assert!(
        body.contains("VOGT_WORK_DIR"),
        "the refusal names the variable: {body}"
    );
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), h.created.recv())
            .await
            .is_err(),
        "a refused start must not create a pod"
    );
}

/// WI-3883 review: names that would hijack the agent's own process (or,
/// before the split into `agent_env`, the entrypoint's clone step) are
/// refused by name: a caller's `GRAFT_ENDPOINT` would otherwise receive
/// the minted source token, and `LD_PRELOAD` would run caller code.
#[tokio::test]
async fn a_pod_session_refuses_hostile_env_names_and_names_the_field() {
    let h = harness(false).await;
    for (name, value) in [
        ("GRAFT_ENDPOINT", "http://attacker.invalid"),
        ("LD_PRELOAD", "/tmp/evil.so"),
        ("BASH_ENV", "/tmp/evil.sh"),
        ("NODE_OPTIONS", "--require /tmp/evil.js"),
        ("PYTHONPATH", "/tmp"),
        ("HTTPS_PROXY", "http://attacker.invalid"),
    ] {
        let response = h
            .client
            .post(format!("{}/api/sessions", h.base))
            .json(&json!({
                "name": "pod-env-hostile",
                "template": "pod",
                "env": [[name, value]],
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{name}");
        let body = response.text().await.unwrap();
        assert!(body.contains(name), "the refusal names {name}: {body}");
        assert!(body.contains("reserves env"), "{name}: {body}");
    }
}

/// WI-3883 review: a value carrying a newline (or tab, or NUL) would break
/// the entrypoint's one-row-per-variable hand-off, so it is refused.
#[tokio::test]
async fn a_pod_session_refuses_an_env_value_that_breaks_its_row() {
    let h = harness(false).await;
    for value in ["a\nb", "a\tb", "a\0b"] {
        let response = h
            .client
            .post(format!("{}/api/sessions", h.base))
            .json(&json!({
                "name": "pod-env-row",
                "template": "pod",
                "env": [["CARGO_BUILD_JOBS", value]],
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{value:?}");
        let body = response.text().await.unwrap();
        assert!(body.contains("CARGO_BUILD_JOBS"), "{body}");
        assert!(body.contains("NUL, newline or tab"), "{body}");
    }
}

/// WI-3335: a deployment with an invalid managed Codex policy refuses a
/// local Codex launch, while a pod-backed Codex template still starts (the
/// pod checks its own filesystem at boot). The local launch is now
/// refused earlier — Codex sessions are pod-only — so the placement gate
/// answers before the policy file is read; the pod must still start.
#[tokio::test]
async fn a_pod_codex_is_not_refused_by_the_engines_policy_check() {
    let template = |name: &str, backend: &str| SessionTemplate {
        name: name.into(),
        description: "codex".into(),
        command: Some(vec!["codex".into()]),
        cwd: None,
        env: Vec::new(),
        account: None,
        default_name: None,
        match_repo_names: Vec::new(),
        match_path_prefixes: Vec::new(),
        tags: Vec::new(),
        backend: backend.into(),
        host_capable: false,
        pod_capable: false,
    };
    let mut h = harness_cfg(
            false,
            Some(vec![
                template("codex-pod", "pod"),
                template("codex-local", "local"),
            ]),
            |cfg| {
                cfg.agent_onboarding.codex_policy = Err(
                    "Codex driven-session policy /etc/codex/requirements.toml is invalid (not TOML: expected `=`); refusing to start a Codex session".into(),
                );
            },
        )
        .await;
    let start = |name: &str| {
        h.client
            .post(format!("{}/api/sessions", h.base))
            .json(&json!({ "name": "codex", "template": name }))
            .send()
    };
    let pod = start("codex-pod").await.unwrap();
    assert!(pod.status().is_success(), "{}", pod.text().await.unwrap());
    let local = start("codex-local").await.unwrap();
    assert_eq!(local.status(), StatusCode::BAD_REQUEST);
    let body = local.text().await.unwrap();
    assert!(
        body.contains("Codex runs in a session pod"),
        "a host Codex launch is refused by placement: {body}"
    );
    let (_id, _token) = h.created.recv().await.expect("the pod is created");
}

/// WI-3243: a Claude pod is bootstrapped with the engine's MCP config and
/// its own core credential, as a host agent session is; the credential is
/// in the env, never in the config file. Another CLI gets neither.
#[tokio::test]
async fn a_claude_bootstrap_carries_the_mcp_config_and_the_sessions_credential() {
    let core = mock_core().await;
    let mut h = harness_cfg(false, None, |cfg| {
        cfg.vogt_core_url = Some(core);
        cfg.vogt_core_token = Some("stack-secret".into());
    })
    .await;
    let endpoint = h.state.sessions.pod_config().unwrap().engine_endpoint;

    let id = h.start().await;
    let (_, token) = h.created.recv().await.unwrap();
    let body: Value = h
        .bootstrap_as(id, &token, "claude")
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(body["env"]["VOGT_HTTP_TOKEN"], format!("minted-for-{id}"));
    assert_eq!(body["env"]["VOGT_ENGINE_SESSION_ID"], id.to_string());
    assert_eq!(
        body["mcp_config"],
        json!({"mcpServers": {"vogt": {
            "type": "http",
            "url": format!("{endpoint}/mcp"),
            "headers": {"Authorization": "Bearer ${VOGT_HTTP_TOKEN}"},
        }}})
    );
    assert!(!body["mcp_config"].to_string().contains("minted-for"));

    let other = h.start().await;
    let (_, token) = h.created.recv().await.unwrap();
    let body: Value = h
        .bootstrap_as(other, &token, "codex")
        .await
        .json()
        .await
        .unwrap();
    assert!(body.get("mcp_config").is_none(), "{body}");
    assert_eq!(
        body["env"]["VOGT_HTTP_TOKEN"],
        format!("minted-for-{other}")
    );
    let toml = body["codex_config"].as_str().unwrap();
    assert!(
        toml.contains(&format!("url = \"{endpoint}/mcp\"")),
        "{toml}"
    );
    assert!(
        toml.contains("bearer_token_env_var = \"VOGT_HTTP_TOKEN\""),
        "{toml}"
    );
    assert!(!toml.contains("minted-for"), "{toml}");

    let third = h.start().await;
    let (_, token) = h.created.recv().await.unwrap();
    let body: Value = h
        .bootstrap_as(third, &token, "opencode")
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(
        body["opencode_config"]["mcp"]["vogt"]["headers"]["Authorization"],
        "Bearer {env:VOGT_HTTP_TOKEN}"
    );
    assert!(body.get("codex_config").is_none(), "{body}");
}

/// WI-3491/WI-3886: a Codex pod is bootstrapped with the registered
/// providers' tables and their key *names* among the ondemand entries;
/// no key value enters the bootstrap response, because the session agent
/// fetches it around the Codex child. Once a provider is removed the next
/// pod has neither, and nothing unregistered is named.
#[tokio::test]
async fn a_codex_bootstrap_names_provider_keys_instead_of_resolving_them() {
    let core = mock_core().await;
    let dir = tempfile::tempdir().unwrap();
    let registry = dir.path().join("llm-providers.toml");
    let helper = fake_broker_helper(dir.path());
    let reg = registry.clone();
    let mut h = harness_cfg(false, None, move |cfg| {
        cfg.vogt_core_url = Some(core);
        cfg.vogt_core_token = Some("stack-secret".into());
        cfg.agent_auth_helper = helper;
        cfg.llm_providers_file = Some(reg);
    })
    .await;
    std::fs::write(
            &registry,
            "[[provider]]\nname = \"ds\"\nbase_url = \"http://litellm/v1\"\nwire_api = \"chat\"\nenv_key = \"DS_KEY\"\nsecret_project = \"p\"\nsecret_name = \"s\"\n",
        )
        .unwrap();

    let id = h.start().await;
    let (_, token) = h.created.recv().await.unwrap();
    let body: Value = h
        .bootstrap_as(id, &token, "codex")
        .await
        .json()
        .await
        .unwrap();
    // The fake helper answers `get VAR` with `value-of-VAR`; WI-3886
    // keeps that value out of the bootstrap response entirely.
    assert!(body["env"].get("DS_KEY").is_none(), "{body}");
    assert_eq!(
        body["env"]["AGENT_AUTH_ONDEMAND"].as_str(),
        Some("DS_KEY"),
        "{body}"
    );
    assert!(!body.to_string().contains("value-of-DS_KEY"), "{body}");
    let config: toml::Table = toml::from_str(body["codex_config"].as_str().unwrap()).unwrap();
    assert_eq!(
        config["model_providers"]["ds"]["env_key"].as_str(),
        Some("DS_KEY")
    );
    assert!(config.contains_key("mcp_servers"), "the MCP tables stay");

    std::fs::write(&registry, "").unwrap();
    let next = h.start().await;
    let (_, token) = h.created.recv().await.unwrap();
    let body: Value = h
        .bootstrap_as(next, &token, "codex")
        .await
        .json()
        .await
        .unwrap();
    assert!(body["env"].get("DS_KEY").is_none(), "{body}");
    assert_eq!(
        body["env"]["AGENT_AUTH_ONDEMAND"].as_str(),
        Some(""),
        "{body}"
    );
    assert!(!body["codex_config"]
        .as_str()
        .unwrap_or_default()
        .contains("model_providers"));
}

/// WI-4521: a shell pod (a person's shell or a raw command, no agent)
/// gets every agent's config whatever the deployment's CLI, so a
/// hand-typed `claude` has the engine's MCP servers and a hand-typed
/// `codex` has them and the providers. A provider key stays a name.
#[tokio::test]
async fn a_shell_bootstrap_carries_claude_and_codex_config() {
    let core = mock_core().await;
    let dir = tempfile::tempdir().unwrap();
    let registry = dir.path().join("llm-providers.toml");
    let helper = fake_broker_helper(dir.path());
    let reg = registry.clone();
    let mut h = harness_cfg(false, None, move |cfg| {
        cfg.vogt_core_url = Some(core);
        cfg.vogt_core_token = Some("stack-secret".into());
        cfg.agent_auth_helper = helper;
        cfg.llm_providers_file = Some(reg);
    })
    .await;
    std::fs::write(
            &registry,
            "[[provider]]\nname = \"ds\"\nbase_url = \"http://litellm/v1\"\nwire_api = \"chat\"\nenv_key = \"DS_KEY\"\nsecret_project = \"p\"\nsecret_name = \"s\"\n",
        )
        .unwrap();
    let id = h.start().await;
    let (_, token) = h.created.recv().await.unwrap();
    let body: Value = h
        .bootstrap_with(id, &token, "codex", true)
        .await
        .json()
        .await
        .unwrap();
    assert!(
        body["mcp_config"]["mcpServers"].is_object(),
        "a shell pod's claude gets MCP: {body}"
    );
    let config: toml::Table = toml::from_str(body["codex_config"].as_str().unwrap()).unwrap();
    assert_eq!(
        config["model_providers"]["ds"]["env_key"].as_str(),
        Some("DS_KEY")
    );
    assert!(config.contains_key("mcp_servers"), "{body}");
    assert!(!body.to_string().contains("value-of-DS_KEY"), "{body}");

    // An agent pod still gets only its own CLI's config.
    let next = h.start().await;
    let (_, token) = h.created.recv().await.unwrap();
    let body: Value = h
        .bootstrap_as(next, &token, "codex")
        .await
        .json()
        .await
        .unwrap();
    assert!(body.get("mcp_config").is_none_or(Value::is_null), "{body}");
}

/// WI-4296: an opencode pod runs a model only through a registered
/// provider. Its bootstrap carries each provider as an opencode
/// `provider` block with the key named by reference and the key name
/// among the ondemand entries, never the value; a model no provider
/// serves is refused before the pod exists, and a bare registered model
/// starts qualified with its provider.
#[tokio::test]
async fn an_opencode_pod_gets_the_registered_providers_and_refuses_unknown_models() {
    let core = mock_core().await;
    let dir = tempfile::tempdir().unwrap();
    let registry = dir.path().join("llm-providers.toml");
    let helper = fake_broker_helper(dir.path());
    let reg = registry.clone();
    let mut h = harness_cfg(
        false,
        Some(vec![super::model_test_template("opencode-pod", "opencode")]),
        move |cfg| {
            cfg.vogt_core_url = Some(core);
            cfg.vogt_core_token = Some("stack-secret".into());
            cfg.agent_auth_helper = helper;
            cfg.llm_providers_file = Some(reg);
        },
    )
    .await;
    std::fs::write(
            &registry,
            "[[provider]]\nname = \"litellm\"\nbase_url = \"http://100.92.54.45:4000/v1\"\nwire_api = \"chat\"\nenv_key = \"HOMELAB_LITELLM_SESSION_KEY\"\nsecret_project = \"p\"\nsecret_name = \"s\"\nmodels = [\"deepseek-chat\"]\n",
        )
        .unwrap();

    let response = h
        .client
        .post(format!("{}/api/sessions", h.base))
        .json(&json!({
            "name": "unknown-opencode-model",
            "template": "opencode-pod",
            "placement": "pod",
            "model": "nonexistent-model-probe",
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
            .contains("nonexistent-model-probe"),
        "{body}"
    );
    assert!(h.created.try_recv().is_err(), "a refused start made a pod");

    let id = h
        .start_body(json!({
            "name": "known-opencode-model",
            "template": "opencode-pod",
            "placement": "pod",
            "model": "deepseek-chat",
        }))
        .await;
    let (created, token) = h.created.recv().await.expect("the pod is created");
    assert_eq!(created, id);
    let pod = h.launcher.pods.lock().unwrap().pop().expect("pod created");
    let model = pod.spec.unwrap().containers[0]
        .env
        .clone()
        .unwrap()
        .into_iter()
        .find(|v| v.name == "VOGT_MODEL")
        .and_then(|v| v.value);
    assert_eq!(model.as_deref(), Some("litellm/deepseek-chat"));

    let body: Value = h
        .bootstrap_as(id, &token, "opencode")
        .await
        .json()
        .await
        .unwrap();
    let litellm = &body["opencode_config"]["provider"]["litellm"];
    assert_eq!(
        litellm["options"]["baseURL"], "http://100.92.54.45:4000/v1",
        "{body}"
    );
    assert_eq!(
        litellm["options"]["apiKey"], "{env:HOMELAB_LITELLM_SESSION_KEY}",
        "{body}"
    );
    assert!(litellm["models"]["deepseek-chat"].is_object(), "{body}");
    assert!(
        body["opencode_config"]["mcp"].is_object(),
        "the MCP block stays"
    );
    assert!(body["env"].get("HOMELAB_LITELLM_SESSION_KEY").is_none());
    assert!(body["env"]["AGENT_AUTH_ONDEMAND"]
        .as_str()
        .unwrap_or_default()
        .split(' ')
        .any(|name| name == "HOMELAB_LITELLM_SESSION_KEY"));
    assert!(
        !body
            .to_string()
            .contains("value-of-HOMELAB_LITELLM_SESSION_KEY"),
        "{body}"
    );
}

/// WI-4121: a pod's cadastre MCP entry authenticates with the bearer
/// variable the host registration names, and only when the agent-auth
/// manifest declares it. Each supported agent's bootstrap carries the
/// name, never a value; the session agent fetches it around the final
/// child. With no entry in the manifest the server is left out rather
/// than registered to 401.
#[tokio::test]
async fn a_cadastre_pod_entry_names_its_bearer_only_when_the_manifest_declares_it() {
    let dir = tempfile::tempdir().unwrap();
    let helper = fake_broker_helper(dir.path());
    let mut h = harness_cfg(false, None, move |cfg| {
        cfg.cadastre_mcp_url = Some("https://cadastre.example.test".into());
        cfg.agent_auth_helper = helper;
        cfg.agent_auth_secrets = vogt_engine_server::secret_broker::parse_manifest(
            "CADASTRE_HTTP_TOKEN acct cadastre-token ondemand\n",
        )
        .unwrap();
    })
    .await;
    let id = h.start().await;
    let (_, token) = h.created.recv().await.unwrap();
    let body: Value = h
        .bootstrap_as(id, &token, "codex")
        .await
        .json()
        .await
        .unwrap();
    let config = body["codex_config"].as_str().unwrap();
    assert!(config.contains("[mcp_servers.cadastre]"), "{config}");
    assert!(
        config.contains("bearer_token_env_var = \"CADASTRE_HTTP_TOKEN\""),
        "{config}"
    );
    assert!(
        config.contains("https://cadastre.example.test/mcp"),
        "{config}"
    );
    assert_eq!(
        body["env"]["AGENT_AUTH_ONDEMAND"].as_str(),
        Some("CADASTRE_HTTP_TOKEN"),
        "{body}"
    );
    assert!(body["env"].get("CADASTRE_HTTP_TOKEN").is_none(), "{body}");
    assert!(
        !body.to_string().contains("value-of-CADASTRE_HTTP_TOKEN"),
        "{body}"
    );

    for cli in ["claude", "opencode"] {
        let id = h.start().await;
        let (_, token) = h.created.recv().await.unwrap();
        let body: Value = h.bootstrap_as(id, &token, cli).await.json().await.unwrap();
        let config = if cli == "claude" {
            &body["mcp_config"]["mcpServers"]["cadastre"]
        } else {
            &body["opencode_config"]["mcp"]["cadastre"]
        };
        assert_eq!(
            config["url"], "https://cadastre.example.test/mcp",
            "{cli}: {body}"
        );
        assert!(
            config["headers"]["Authorization"]
                .as_str()
                .is_some_and(|value| value.contains("CADASTRE_HTTP_TOKEN")),
            "{cli}: {body}"
        );
        assert_eq!(
            body["env"]["AGENT_AUTH_ONDEMAND"].as_str(),
            Some("CADASTRE_HTTP_TOKEN"),
            "{cli}: {body}"
        );
        assert!(body["env"].get("CADASTRE_HTTP_TOKEN").is_none(), "{body}");
        assert!(
            !body.to_string().contains("value-of-CADASTRE_HTTP_TOKEN"),
            "{body}"
        );
    }

    // The same deployment URL, no manifest line: the entry is omitted.
    let dir2 = tempfile::tempdir().unwrap();
    let helper = fake_broker_helper(dir2.path());
    let mut h = harness_cfg(false, None, move |cfg| {
        cfg.cadastre_mcp_url = Some("https://cadastre.example.test".into());
        cfg.agent_auth_helper = helper;
        cfg.agent_auth_secrets = Vec::new();
    })
    .await;
    let id = h.start().await;
    let (_, token) = h.created.recv().await.unwrap();
    let body: Value = h
        .bootstrap_as(id, &token, "codex")
        .await
        .json()
        .await
        .unwrap();
    let config = body["codex_config"].as_str().unwrap_or_default();
    assert!(!config.contains("mcp_servers.cadastre"), "{config}");
    assert!(!body.to_string().contains("CADASTRE_HTTP_TOKEN"), "{body}");
}

/// WI-4289: a pod reaches a remote Vogt as `vogt-prod`, authenticated
/// with `VOGT_PROD_TOKEN` by reference, only when the engine names that
/// Vogt and the manifest declares the token. The token is never in the
/// bootstrap; with either half missing there is no entry.
#[tokio::test]
async fn a_vogt_prod_pod_entry_needs_a_url_and_a_manifest_token() {
    let dir = tempfile::tempdir().unwrap();
    let helper = fake_broker_helper(dir.path());
    let mut h = harness_cfg(false, None, move |cfg| {
        cfg.vogt_prod_mcp_url = Some("https://vogt-prod.example.test".into());
        cfg.agent_auth_helper = helper;
        cfg.agent_auth_secrets = vogt_engine_server::secret_broker::parse_manifest(
            "VOGT_PROD_TOKEN acct prod-bridge-token ondemand\n",
        )
        .unwrap();
    })
    .await;
    for cli in ["claude", "codex", "opencode"] {
        let id = h.start().await;
        let (_, token) = h.created.recv().await.unwrap();
        let body: Value = h.bootstrap_as(id, &token, cli).await.json().await.unwrap();
        match cli {
            "claude" => {
                let entry = &body["mcp_config"]["mcpServers"]["vogt-prod"];
                assert_eq!(entry["url"], "https://vogt-prod.example.test/mcp", "{body}");
                assert_eq!(
                    entry["headers"]["Authorization"], "Bearer ${VOGT_PROD_TOKEN}",
                    "{body}"
                );
            }
            "codex" => {
                let config = body["codex_config"].as_str().unwrap();
                assert!(config.contains("[mcp_servers.vogt-prod]"), "{config}");
                assert!(
                    config.contains("bearer_token_env_var = \"VOGT_PROD_TOKEN\""),
                    "{config}"
                );
            }
            _ => {
                let entry = &body["opencode_config"]["mcp"]["vogt-prod"];
                assert_eq!(entry["url"], "https://vogt-prod.example.test/mcp", "{body}");
                assert_eq!(
                    entry["headers"]["Authorization"], "Bearer {env:VOGT_PROD_TOKEN}",
                    "{body}"
                );
            }
        }
        assert_eq!(
            body["env"]["AGENT_AUTH_ONDEMAND"].as_str(),
            Some("VOGT_PROD_TOKEN"),
            "{cli}: {body}"
        );
        assert!(body["env"].get("VOGT_PROD_TOKEN").is_none(), "{body}");
        assert!(
            !body.to_string().contains("value-of-VOGT_PROD_TOKEN"),
            "{body}"
        );
    }

    // A URL with no manifest line, and a manifest line with no URL: no entry.
    for (url, manifest) in [
        (Some("https://vogt-prod.example.test"), ""),
        (None, "VOGT_PROD_TOKEN acct prod-bridge-token ondemand\n"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let helper = fake_broker_helper(dir.path());
        let mut h = harness_cfg(false, None, move |cfg| {
            cfg.vogt_prod_mcp_url = url.map(str::to_owned);
            cfg.agent_auth_helper = helper;
            cfg.agent_auth_secrets =
                vogt_engine_server::secret_broker::parse_manifest(manifest).unwrap();
        })
        .await;
        let id = h.start().await;
        let (_, token) = h.created.recv().await.unwrap();
        let body: Value = h
            .bootstrap_as(id, &token, "claude")
            .await
            .json()
            .await
            .unwrap();
        assert!(
            body["mcp_config"]["mcpServers"].get("vogt-prod").is_none(),
            "{url:?}: {body}"
        );
    }
}

/// WI-4489: a worker pod gets the worker set, as a host launch does: vogt
/// on its worker profile, cadastre and crw. vogt-prod is never handed to
/// it, nor is its token named, even with a URL and a manifest token.
#[tokio::test]
async fn a_worker_pod_gets_the_worker_set_with_crw_and_no_vogt_prod() {
    let core = mock_core().await;
    let dir = tempfile::tempdir().unwrap();
    let helper = fake_broker_helper(dir.path());
    let mut h = harness_cfg(false, None, move |cfg| {
        cfg.vogt_core_url = Some(core);
        cfg.vogt_core_token = Some("stack-secret".into());
        cfg.vogt_prod_mcp_url = Some("https://vogt-prod.example.test".into());
        cfg.crw_mcp_url = Some("http://crw.example.test:18190/mcp".into());
        cfg.agent_auth_helper = helper;
        cfg.agent_auth_secrets = vogt_engine_server::secret_broker::parse_manifest(
            "VOGT_PROD_TOKEN acct prod-bridge-token ondemand\n",
        )
        .unwrap();
    })
    .await;
    for cli in ["claude", "codex"] {
        let id = h
            .start_body(json!({
                "name": "pod-session",
                "template": "pod",
                "mcp_set": "worker",
                "role": "worker",
            }))
            .await;
        let (_, token) = h.created.recv().await.unwrap();
        let body: Value = h.bootstrap_as(id, &token, cli).await.json().await.unwrap();
        if cli == "claude" {
            let servers = &body["mcp_config"]["mcpServers"];
            assert_eq!(
                servers["crw"]["url"], "http://crw.example.test:18190/mcp",
                "{body}"
            );
            assert!(servers.get("vogt-prod").is_none(), "{body}");
            // The trim (for_mcp_set) puts vogt on its worker profile.
            let url = servers["vogt"]["url"].as_str().unwrap_or_default();
            assert!(url.ends_with("/mcp?profile=worker"), "{body}");
        } else {
            let config = body["codex_config"].as_str().unwrap();
            assert!(config.contains("[mcp_servers.crw]"), "{config}");
            assert!(!config.contains("vogt-prod"), "{config}");
            assert!(config.contains("/mcp?profile=worker"), "{config}");
        }
        assert!(!body.to_string().contains("VOGT_PROD_TOKEN"), "{body}");
        let summary: Value = h
            .client
            .get(format!("{}/api/sessions/{id}", h.base))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(summary["summary"]["mcp_set"], "worker", "{summary}");
        // A worker's role is the default, which the wire leaves out.
        assert!(summary["summary"].get("role").is_none(), "{summary}");
    }
    // A full-set start still gets vogt-prod and crw.
    let id = h.start().await;
    let (_, token) = h.created.recv().await.unwrap();
    let body: Value = h
        .bootstrap_as(id, &token, "claude")
        .await
        .json()
        .await
        .unwrap();
    let servers = &body["mcp_config"]["mcpServers"];
    assert!(servers.get("vogt-prod").is_some(), "{body}");
    assert!(servers.get("crw").is_some(), "{body}");
    let url = servers["vogt"]["url"].as_str().unwrap_or_default();
    assert!(!url.contains("profile="), "{body}");
}

/// WI-4489: a start another session or autopilot made, with no mcp_set
/// named, is a worker: its pod gets the worker set and no vogt-prod.
/// Oversight keeps the full set.
#[tokio::test]
async fn a_dispatched_pod_start_defaults_to_the_worker_set() {
    let dir = tempfile::tempdir().unwrap();
    let helper = fake_broker_helper(dir.path());
    let mut h = harness_cfg(false, None, move |cfg| {
        cfg.vogt_prod_mcp_url = Some("https://vogt-prod.example.test".into());
        cfg.agent_auth_helper = helper;
        cfg.agent_auth_secrets = vogt_engine_server::secret_broker::parse_manifest(
            "VOGT_PROD_TOKEN acct prod-bridge-token ondemand\n",
        )
        .unwrap();
    })
    .await;
    let mut supervisor: Option<uuid::Uuid> = None;
    for (start, want) in [
        (json!({"autopilot": true, "role": "oversight"}), "full"),
        (json!({"supervisor": "SUP"}), "worker"),
        (json!({"autopilot": true}), "worker"),
    ] {
        let mut body = json!({"name": "pod-session", "template": "pod"});
        for (k, v) in start.as_object().unwrap() {
            let v = if v == "SUP" {
                json!(supervisor.unwrap().to_string())
            } else {
                v.clone()
            };
            body[k] = v;
        }
        let id: Uuid = h.start_body(body).await;
        supervisor.get_or_insert(id);
        let (_, token) = h.created.recv().await.unwrap();
        let boot: Value = h
            .bootstrap_as(id, &token, "claude")
            .await
            .json()
            .await
            .unwrap();
        let has_prod = boot["mcp_config"]["mcpServers"].get("vogt-prod").is_some();
        assert_eq!(has_prod, want == "full", "{start}: {boot}");
        let summary: Value = h
            .client
            .get(format!("{}/api/sessions/{id}", h.base))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(summary["summary"]["mcp_set"], want, "{start}: {summary}");
    }
}

/// WI-4489: a pod a worker started (vogt-core names it `started_by`),
/// with no set named, gets the worker set and no vogt-prod, though no
/// supervisor or autopilot marks it as dispatched.
#[tokio::test]
async fn a_worker_started_pod_child_gets_the_worker_set() {
    let dir = tempfile::tempdir().unwrap();
    let helper = fake_broker_helper(dir.path());
    let mut h = harness_cfg(false, None, move |cfg| {
        cfg.vogt_prod_mcp_url = Some("https://vogt-prod.example.test".into());
        cfg.agent_auth_helper = helper;
        cfg.agent_auth_secrets = vogt_engine_server::secret_broker::parse_manifest(
            "VOGT_PROD_TOKEN acct prod-bridge-token ondemand\n",
        )
        .unwrap();
    })
    .await;
    let parent = h
        .start_body(json!({"name": "parent", "template": "pod", "mcp_set": "worker"}))
        .await;
    let _ = h.created.recv().await.unwrap();
    let child = h
        .start_body(json!({
            "name": "child", "template": "pod", "started_by": parent.to_string(),
        }))
        .await;
    let (_, token) = h.created.recv().await.unwrap();
    let boot: Value = h
        .bootstrap_as(child, &token, "claude")
        .await
        .json()
        .await
        .unwrap();
    assert!(
        boot["mcp_config"]["mcpServers"].get("vogt-prod").is_none(),
        "{boot}"
    );
    let summary: Value = h
        .client
        .get(format!("{}/api/sessions/{child}", h.base))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(summary["summary"]["mcp_set"], "worker", "{summary}");
}

/// WI-4489: on a dev lane no session reaches production. A full-set pod
/// gets no vogt-prod entry, and the broker refuses it VOGT_PROD_TOKEN,
/// though the URL and the manifest line are both configured.
#[tokio::test]
async fn a_dev_lane_never_hands_out_vogt_prod() {
    let dir = tempfile::tempdir().unwrap();
    let helper = fake_broker_helper(dir.path());
    let mut h = harness_cfg(false, None, move |cfg| {
        cfg.lane = Some("dev".into());
        cfg.vogt_prod_mcp_url = Some("https://vogt-prod.example.test".into());
        cfg.agent_auth_helper = helper;
        cfg.agent_auth_secrets = vogt_engine_server::secret_broker::parse_manifest(
            "VOGT_PROD_TOKEN acct prod-bridge-token ondemand\n",
        )
        .unwrap();
    })
    .await;
    let id = h
        .start_body(json!({"name": "full", "template": "pod", "mcp_set": "full"}))
        .await;
    let (_, token) = h.created.recv().await.unwrap();
    let boot: Value = h
        .bootstrap_as(id, &token, "claude")
        .await
        .json()
        .await
        .unwrap();
    assert!(
        boot["mcp_config"]["mcpServers"].get("vogt-prod").is_none(),
        "{boot}"
    );
    assert!(!boot.to_string().contains("VOGT_PROD_TOKEN"), "{boot}");
    let broker = h.state.sessions.secret_broker();
    let r = reqwest::Client::new()
        .post(format!("{}/api/agent-auth/fetch/VOGT_PROD_TOKEN", h.base))
        .bearer_auth(broker.issue(id))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);
}

/// WI-4601: with ENGINE_ALLOW_DEV_PROD_BRIDGE the dev lane keeps the
/// bridge. A full-set pod gets the vogt-prod entry and the broker hands it
/// VOGT_PROD_TOKEN, as on any other lane.
#[tokio::test]
async fn a_dev_lane_with_the_opt_in_hands_out_vogt_prod() {
    let dir = tempfile::tempdir().unwrap();
    let helper = fake_broker_helper(dir.path());
    let mut h = harness_cfg(false, None, move |cfg| {
        cfg.lane = Some("dev".into());
        cfg.allow_dev_prod_bridge = true;
        cfg.vogt_prod_mcp_url = Some("https://vogt-prod.example.test".into());
        cfg.agent_auth_helper = helper;
        cfg.agent_auth_secrets = vogt_engine_server::secret_broker::parse_manifest(
            "VOGT_PROD_TOKEN acct prod-bridge-token ondemand\n",
        )
        .unwrap();
    })
    .await;
    let id = h
        .start_body(json!({"name": "full", "template": "pod", "mcp_set": "full"}))
        .await;
    let (_, token) = h.created.recv().await.unwrap();
    let boot: Value = h
        .bootstrap_as(id, &token, "claude")
        .await
        .json()
        .await
        .unwrap();
    assert!(
        boot["mcp_config"]["mcpServers"].get("vogt-prod").is_some(),
        "{boot}"
    );
    let broker = h.state.sessions.secret_broker();
    let r = reqwest::Client::new()
        .post(format!("{}/api/agent-auth/fetch/VOGT_PROD_TOKEN", h.base))
        .bearer_auth(broker.issue(id))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
}

/// WI-4489: a bootstrap whose launch details are gone fails closed, on
/// the worker set, even for a full-set start.
#[tokio::test]
async fn a_bootstrap_without_its_launch_falls_back_to_the_worker_set() {
    let dir = tempfile::tempdir().unwrap();
    let helper = fake_broker_helper(dir.path());
    let mut h = harness_cfg(false, None, move |cfg| {
        cfg.vogt_prod_mcp_url = Some("https://vogt-prod.example.test".into());
        cfg.agent_auth_helper = helper;
        cfg.agent_auth_secrets = vogt_engine_server::secret_broker::parse_manifest(
            "VOGT_PROD_TOKEN acct prod-bridge-token ondemand\n",
        )
        .unwrap();
    })
    .await;
    let id = h.start().await;
    let (_, token) = h.created.recv().await.unwrap();
    assert!(h.state.agent_auth.take_launch(id).await.is_some());
    let body: Value = h
        .bootstrap_as(id, &token, "claude")
        .await
        .json()
        .await
        .unwrap();
    assert!(
        body["mcp_config"]["mcpServers"].get("vogt-prod").is_none(),
        "{body}"
    );
    assert!(!body.to_string().contains("VOGT_PROD_TOKEN"), "{body}");
}

/// WI-3491 review: a provider whose env_key is already a manifest VAR is
/// dropped whole. The pod gets no table pointing that VAR at the
/// registry's base_url, and the manifest's ondemand secret is not fetched.
#[tokio::test]
async fn a_codex_bootstrap_drops_a_provider_whose_key_is_a_manifest_var() {
    let core = mock_core().await;
    let dir = tempfile::tempdir().unwrap();
    let registry = dir.path().join("llm-providers.toml");
    let helper = fake_broker_helper(dir.path());
    let reg = registry.clone();
    let mut h = harness_cfg(false, None, move |cfg| {
        cfg.vogt_core_url = Some(core);
        cfg.vogt_core_token = Some("stack-secret".into());
        cfg.agent_auth_helper = helper;
        cfg.agent_auth_secrets = vogt_engine_server::secret_broker::parse_manifest(
            "DS_KEY manifest-proj manifest-secret ondemand\n",
        )
        .unwrap();
        cfg.llm_providers_file = Some(reg);
    })
    .await;
    std::fs::write(
            &registry,
            "[[provider]]\nname = \"ds\"\nbase_url = \"http://elsewhere/v1\"\nwire_api = \"chat\"\nenv_key = \"DS_KEY\"\nsecret_project = \"p\"\nsecret_name = \"s\"\n",
        )
        .unwrap();

    let id = h.start().await;
    let (_, token) = h.created.recv().await.unwrap();
    let body: Value = h
        .bootstrap_as(id, &token, "codex")
        .await
        .json()
        .await
        .unwrap();
    assert!(body["env"].get("DS_KEY").is_none(), "{body}");
    let config = body["codex_config"].as_str().unwrap_or_default();
    assert!(!config.contains("model_providers"), "{config}");
    assert!(!config.contains("elsewhere"), "{config}");
}

/// WI-3334 / WI-2000: `placement=pod` is accepted for the stock Claude
/// template. Its pod runs Claude, and its leased account is handed a login
/// with onboarding, folder trust and external imports pre-approved before
/// the checkout exists. It can write the bundle back with the only
/// credential it holds, its stream token.
#[tokio::test]
async fn a_claude_pod_gets_a_signed_in_bundle_and_can_write_it_back() {
    let tmp = tempfile::tempdir().unwrap();
    let acct = tmp.path().join("acct2");
    std::fs::create_dir_all(&acct).unwrap();
    std::fs::write(acct.join(".credentials.json"), br#"{"claudeAiOauth":{}}"#).unwrap();
    let dir = acct.clone();
    let mut h = harness_cfg(
        false,
        Some(SessionTemplate::default_templates()),
        move |cfg| {
            cfg.agent_onboarding = vogt_engine_server::claude_config::Onboarding {
                enabled: true,
                default_dir: None,
                settings_file: None,
                opencode_config: None,
                account_concurrency_cap: 8,
                default_account: None,
                accounts_root: None,
                accounts: vogt_engine_server::claude_config::parse_accounts(Some(&format!(
                    "acct2={}",
                    dir.display()
                ))),
                codex_policy: Ok(None),
            };
        },
    )
    .await;
    let created: Value = h
        .client
        .post(format!("{}/api/sessions", h.base))
        .json(&json!({
            "name": "claude-pod",
            "template": "Claude Code (protected)",
            "placement": "pod",
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id: Uuid = created["id"].as_str().unwrap().parse().unwrap();
    let (_, token) = h.created.recv().await.unwrap();
    let pod = h.launcher.pods.lock().unwrap()[0].clone();
    let env = pod.spec.unwrap().containers[0]
        .env
        .clone()
        .unwrap()
        .into_iter()
        .map(|item| (item.name, item.value.unwrap_or_default()))
        .collect::<std::collections::BTreeMap<_, _>>();
    assert_eq!(env["VOGT_CLI"], "claude");
    assert!(!env.contains_key("VOGT_SESSION_COMMAND"));
    let body: Value = h
        .bootstrap_as(id, &token, "claude")
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(body["env"]["VOGT_SESSION_ACCOUNT"], "acct2", "{body}");
    let bundle = body["bundle"].as_object().unwrap();
    assert!(bundle.contains_key(".credentials.json"), "{body}");
    let config = base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        bundle[".claude.json"].as_str().unwrap(),
    )
    .unwrap();
    let config: Value = serde_json::from_slice(&config).unwrap();
    assert_eq!(config["hasCompletedOnboarding"], true);
    assert_eq!(config["theme"], "dark");
    // The approval is for the pod's own workspace mount, not the engine's
    // workspace root, which the pod never sees (WI-2000).
    let engine_root = h.state.config.workspace_root.to_string_lossy().into_owned();
    assert_ne!(engine_root, "/home/vogt/Working", "{config}");
    let rootp = &config["projects"]["/home/vogt/Working"];
    assert_eq!(rootp["hasTrustDialogAccepted"], true, "{config}");
    assert_eq!(
        rootp["hasClaudeMdExternalIncludesApproved"], true,
        "{config}"
    );
    let project = &config["projects"]["/work"];
    assert_eq!(project["hasTrustDialogAccepted"], true, "{config}");
    assert_eq!(
        project["hasClaudeMdExternalIncludesApproved"], true,
        "{config}"
    );
    assert_eq!(
        project["hasClaudeMdExternalIncludesWarningShown"], true,
        "{config}"
    );

    let session_token = body["session_token"].as_str().unwrap();
    let conversation = body["env"]["VOGT_SESSION_CONVERSATION_ID"]
        .as_str()
        .unwrap_or("");
    let version = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
        - 1_000;
    let res = reqwest::Client::new()
        .post(format!(
            "{}/api/agent-auth/store/bundle/bundle-file",
            h.base
        ))
        .bearer_auth(session_token)
        .json(&json!({
            "account": "acct2",
            "conversation_id": conversation,
            "path": ".claude.json",
            "kind": "config",
            "bytes": base64::Engine::encode(
                &base64::engine::general_purpose::STANDARD,
                br#"{"hasCompletedOnboarding":true,"theme":"light"}"#,
            ),
            "version": version,
        }))
        .send()
        .await
        .unwrap();
    assert!(res.status().is_success(), "{}", res.status());
    assert!(std::fs::read_to_string(acct.join(".claude.json"))
        .unwrap()
        .contains("light"));
}

/// WI-4522: a shell pod is seeded with the default account's login, so
/// `claude` typed in it opens signed in. A pod running some other raw
/// command is not.
#[tokio::test]
async fn a_shell_pod_is_seeded_with_the_default_account() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let mut h = harness_cfg(false, Some(shell_pod_templates()), move |cfg| {
        cfg.agent_onboarding = default_account_onboarding(&root);
    })
    .await;

    let id = h
        .start_body(json!({ "name": "shell", "template": "Shell" }))
        .await;
    let (_, token) = h.created.recv().await.unwrap();
    let body: Value = h
        .bootstrap_with(id, &token, "claude", true)
        .await
        .json()
        .await
        .unwrap();
    let (account, login) = seeded_login(&body);
    assert_eq!(account, "acct1", "{body}");
    assert!(login.contains("acct1"), "{body}");
    assert!(body["bundle"].get(".claude.json").is_some(), "{body}");

    let id = h
        .start_body(json!({ "name": "raw", "template": "raw" }))
        .await;
    let (_, token) = h.created.recv().await.unwrap();
    let body: Value = h
        .bootstrap_with(id, &token, "claude", true)
        .await
        .json()
        .await
        .unwrap();
    let (account, login) = seeded_login(&body);
    assert_eq!(account, "", "{body}");
    assert_eq!(login, "", "{body}");
}
