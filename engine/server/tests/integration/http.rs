use super::*;

#[tokio::test]
async fn healthz_is_public() {
    let (base, _h) = boot().await;
    let res = reqwest::get(format!("{base}/healthz")).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["ok"], true);
}

#[tokio::test]
async fn config_endpoint_is_public_and_returns_shape() {
    let (base, _h) = boot().await;
    let res = reqwest::get(format!("{base}/api/config")).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body: Value = res.json().await.unwrap();
    assert!(
        body.get("gui_stream_url").is_some(),
        "missing gui_stream_url"
    );
    assert_eq!(
        body["gui_stream_available"], false,
        "unverified test config must withdraw the GUI surface"
    );
    assert!(body["version"].as_str().is_some(), "missing version");
    assert!(
        body["product_version"].as_str().is_some(),
        "missing product version"
    );
    assert!(body["source_ref"].as_str().is_some(), "missing source ref");
    assert!(body["source_sha"].as_str().is_some(), "missing source sha");
    assert!(body["git_sha"].as_str().is_some(), "missing git sha");
}

#[tokio::test]
async fn finite_pwa_and_api_responses_negotiate_gzip_without_changing_cache_policy() {
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .no_gzip()
        .no_brotli()
        .build()
        .unwrap();

    let identity = client
        .get(format!("{base}/"))
        .header(reqwest::header::ACCEPT_ENCODING, "identity")
        .send()
        .await
        .unwrap();
    let identity_body = identity.bytes().await.unwrap();

    let compressed = client
        .get(format!("{base}/"))
        .header(reqwest::header::ACCEPT_ENCODING, "gzip")
        .send()
        .await
        .unwrap();
    assert_eq!(
        compressed.headers()[reqwest::header::CONTENT_ENCODING],
        "gzip"
    );
    assert_eq!(
        compressed.headers()[reqwest::header::VARY]
            .to_str()
            .unwrap()
            .to_ascii_lowercase(),
        "accept-encoding"
    );
    assert_eq!(
        compressed.headers()[reqwest::header::CACHE_CONTROL],
        "no-store, must-revalidate"
    );
    let compressed_body = compressed.bytes().await.unwrap();
    let mut decoder = GzDecoder::new(&compressed_body[..]);
    let mut decoded = Vec::new();
    std::io::Read::read_to_end(&mut decoder, &mut decoded).unwrap();
    assert_eq!(decoded, identity_body);

    let api = client
        .get(format!("{base}/api/config"))
        .header(reqwest::header::ACCEPT_ENCODING, "gzip")
        .send()
        .await
        .unwrap();
    assert_eq!(api.headers()[reqwest::header::CONTENT_ENCODING], "gzip");
    assert_eq!(
        api.headers()[reqwest::header::VARY]
            .to_str()
            .unwrap()
            .to_ascii_lowercase(),
        "accept-encoding"
    );
    let api_body = api.bytes().await.unwrap();
    let mut decoder = GzDecoder::new(&api_body[..]);
    let mut decoded = Vec::new();
    std::io::Read::read_to_end(&mut decoder, &mut decoded).unwrap();
    serde_json::from_slice::<Value>(&decoded).unwrap();
}

#[tokio::test]
async fn compression_does_not_modify_sse_or_identity_responses() {
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .no_gzip()
        .no_brotli()
        .build()
        .unwrap();

    let sse = client
        .get(format!("{base}/api/events"))
        .header(reqwest::header::ACCEPT_ENCODING, "gzip")
        .header(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {TEST_TOKEN}"),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(sse.status(), StatusCode::OK);
    assert!(!sse
        .headers()
        .contains_key(reqwest::header::CONTENT_ENCODING));
    assert_eq!(
        sse.headers()[reqwest::header::CONTENT_TYPE],
        "text/event-stream"
    );

    let identity = client
        .get(format!("{base}/api/config"))
        .header(reqwest::header::ACCEPT_ENCODING, "identity")
        .send()
        .await
        .unwrap();
    assert!(!identity
        .headers()
        .contains_key(reqwest::header::CONTENT_ENCODING));
}

#[tokio::test]
async fn auth_check_is_cheap_and_requires_auth() {
    let (base, _h) = boot().await;

    let unauth = reqwest::get(format!("{base}/api/auth/check"))
        .await
        .unwrap();
    assert_eq!(unauth.status(), StatusCode::UNAUTHORIZED);

    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let body: Value = client
        .get(format!("{base}/api/auth/check"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(body["ok"], true);
    assert!(body["version"].as_str().is_some(), "missing version");
    assert!(body["storage"]["workspace_root"].as_str().is_some());
    assert!(body.get("history").is_none());
    assert!(body.get("agent_tasks").is_none());
}

#[tokio::test]
async fn status_endpoint_requires_auth_and_returns_shape() {
    let (base, _h) = boot().await;

    let unauth = reqwest::get(format!("{base}/api/status")).await.unwrap();
    assert_eq!(unauth.status(), StatusCode::UNAUTHORIZED);

    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let body: Value = client
        .get(format!("{base}/api/status"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert!(body["version"].as_str().is_some(), "missing version");
    assert!(
        body["product_version"].as_str().is_some(),
        "missing product version"
    );
    assert!(
        body["session_count"].as_u64().is_some(),
        "missing session_count"
    );
    assert!(
        body["push_subscription_count"].as_u64().is_some(),
        "missing push_subscription_count"
    );
    assert!(
        body["gui_process_count"].as_u64().is_some(),
        "missing gui_process_count"
    );
    assert!(
        body["auth_broker"]["auto_agent_auth"].is_boolean(),
        "missing auth_broker.auto_agent_auth"
    );
    assert!(
        body["storage"]["workspace_root"].as_str().is_some(),
        "missing storage.workspace_root"
    );
}

#[tokio::test]
async fn list_sessions_rejects_missing_auth() {
    let (base, _h) = boot().await;
    let res = reqwest::get(format!("{base}/api/sessions")).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn default_session_uses_agent_auth_helper_when_enabled() {
    let tmp = tempfile::tempdir().unwrap();
    let helper = tmp.path().join("agent-auth");
    std::fs::write(
        &helper,
        "#!/bin/sh\n[ \"$1\" = shell ] || exit 64\nprintf 'agent-wrapper-ok\\n'\nexec /bin/cat\n",
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&helper).unwrap().permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&helper, permissions).unwrap();

    let mut cfg = test_config();
    cfg.default_cwd = tmp.path().to_path_buf();
    cfg.workspace_root = tmp.path().canonicalize().unwrap();
    cfg.auto_agent_auth = true;
    cfg.agent_auth_helper = helper;

    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let id = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({ "name": "authenticated-shell" }))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let mut ws = ws_attach(&base, &id).await;
    let output = collect_binary_until(&mut ws, b"agent-wrapper-ok", Duration::from_secs(2)).await;
    assert!(
        output
            .windows(b"agent-wrapper-ok".len())
            .any(|w| w == b"agent-wrapper-ok"),
        "agent auth helper output not seen; got {:?}",
        String::from_utf8_lossy(&output)
    );

    let _ = client
        .delete(format!("{base}/api/sessions/{id}"))
        .send()
        .await;
}

// ── unprovisioned means invisible, not broken ─────────────────────────────

#[tokio::test]
async fn without_an_api_key_every_assistant_route_is_absent() {
    // The feature is invisible unless provisioned: a 404 rather than a 500 or
    // an empty transcript, so a deployment that never configured an assistant
    // does not look like one whose assistant is broken. Asserted here because
    // every other test in this file boots with `assistant_api_key: None` and
    // simply never asks.
    let (base, _h) = boot().await;
    let client = reqwest::Client::new();
    for (method, path) in [
        ("GET", "/api/assistant/history"),
        ("POST", "/api/assistant/message"),
        ("POST", "/api/assistant/reset"),
    ] {
        let req = match method {
            "GET" => client.get(format!("{base}{path}")),
            _ => client
                .post(format!("{base}{path}"))
                .json(&serde_json::json!({"text": "hello"})),
        };
        let res = req.headers(auth()).send().await.unwrap();
        assert_eq!(
            res.status(),
            reqwest::StatusCode::NOT_FOUND,
            "{method} {path} should be absent, not broken"
        );
    }
}

// ── sessions do not depend on the core ────────────────────────────────────

#[tokio::test]
async fn sessions_work_with_no_vogt_core_configured() {
    // This is exercised by every session test in this file, because they all
    // boot with `vogt_core_url: None` — and that is exactly why it needed
    // naming. A reader looking for the requirement found nothing, and the day
    // somebody gives this fixture a core, the coverage would vanish without a
    // single test turning red.
    let cfg = test_config();
    assert!(
        cfg.vogt_core_url.is_none(),
        "this test is about the core being absent; if the fixture gains one, \
         core independence needs its own fixture rather than this assertion deleted"
    );
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::new();

    let created = client
        .post(format!("{base}/api/sessions"))
        .headers(auth())
        .json(&serde_json::json!({"name": "no-core"}))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), reqwest::StatusCode::OK);
    let id = created.json::<serde_json::Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let listed = client
        .get(format!("{base}/api/sessions"))
        .headers(auth())
        .send()
        .await
        .unwrap();
    assert_eq!(listed.status(), reqwest::StatusCode::OK);

    // And the container is ready, so a healthcheck does not restart the
    // engine — which could not revive a core and would kill every live PTY
    // trying.
    let ready = client.get(format!("{base}/readyz")).send().await.unwrap();
    assert!(ready.status().is_success());

    let killed = client
        .post(format!("{base}/api/sessions/{id}/kill"))
        .headers(auth())
        .send()
        .await
        .unwrap();
    assert_eq!(killed.status(), reqwest::StatusCode::OK);
}

// -- Server-side speech --------------------------------------------

/// Unconfigured, both speech routes 404 so the client falls back. The
/// request is well-formed — a real multipart upload and a real JSON body — so
/// the 404 is the handler's "this half is not provisioned", not an extractor
/// rejecting a malformed request.
#[tokio::test]
async fn server_speech_routes_404_when_unconfigured() {
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let form = reqwest::multipart::Form::new()
        .text("model", "whisper-1")
        .part(
            "file",
            reqwest::multipart::Part::bytes(vec![0u8, 1, 2, 3])
                .file_name("take.webm")
                .mime_str("audio/webm")
                .unwrap(),
        );
    let stt = client
        .post(format!("{base}/api/assistant/stt"))
        .multipart(form)
        .send()
        .await
        .unwrap();
    assert_eq!(stt.status(), StatusCode::NOT_FOUND);

    let tts = client
        .post(format!("{base}/api/assistant/tts"))
        .json(&json!({ "text": "hello" }))
        .send()
        .await
        .unwrap();
    assert_eq!(tts.status(), StatusCode::NOT_FOUND);
}

/// Both speech routes require the `assistant` capability, enforced by the
/// `starts_with("/api/assistant") && != GET` rule in `auth::required_capability`
/// — a token without it is refused before the handler runs, so a speech route
/// is never an ungated door into the assistant. Asserted end-to-end here in
/// addition to the unit test in `auth.rs`.
#[tokio::test]
async fn server_speech_routes_require_the_assistant_capability() {
    // A caller the core resolves with `read` only: may not drive the
    // assistant. The front door learns that from the core's scopes.
    let core = stand_in_core_knowing(vec![(
        "reader-token-1234567890abcdef",
        "human:reader",
        vec!["read"],
    )])
    .await;
    let mut cfg = test_config();
    cfg.vogt_core_url = Some(core);
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth_for("reader-token-1234567890abcdef"))
        .build()
        .unwrap();

    let stt = client
        .post(format!("{base}/api/assistant/stt"))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(stt.status(), StatusCode::FORBIDDEN);

    let tts = client
        .post(format!("{base}/api/assistant/tts"))
        .json(&json!({ "text": "hello" }))
        .send()
        .await
        .unwrap();
    assert_eq!(tts.status(), StatusCode::FORBIDDEN);
}

/// `/api/config` advertises each configured speech half by presence only — never
/// a key or a base URL — so a client can pick the server pipeline by capability
/// rather than by probing for a 404.
#[tokio::test]
async fn config_advertises_configured_server_speech() {
    let (base, _h) = boot().await;
    let unconfigured: Value = reqwest::get(format!("{base}/api/config"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(unconfigured["assistant_stt_enabled"], false);
    assert_eq!(unconfigured["assistant_tts_enabled"], false);

    let mut cfg = test_config();
    cfg.assistant_stt_base_urls = vec!["https://audio.invalid/v1".into()];
    cfg.assistant_stt_api_key = Some("sk-stt-123".into());
    // TTS left unset: the two halves are independent.
    let (base, _h) = boot_with_config(cfg).await;
    let configured: Value = reqwest::get(format!("{base}/api/config"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(configured["assistant_stt_enabled"], true);
    assert_eq!(configured["assistant_tts_enabled"], false);
    // Presence only — the key and base URL never appear.
    let rendered = configured.to_string();
    assert!(!rendered.contains("sk-stt-123"), "the key must never leak");
    assert!(
        !rendered.contains("audio.invalid"),
        "the base URL is an exposure value and must never leak"
    );
}

#[tokio::test]
async fn session_template_list_uses_the_real_engine_route_and_summary() {
    let mut cfg = test_config();
    cfg.session_templates = vec![SessionTemplate {
        name: "Pod Claude".into(),
        description: String::new(),
        command: Some(vec!["claude".into()]),
        cwd: None,
        env: vec![],
        backend: "pod".into(),
        host_capable: true,
        pod_capable: false,
        account: None,
        default_name: None,
        match_repo_names: vec![],
        match_path_prefixes: vec![],
        tags: vec![],
    }];
    cfg.pod_backends_enabled = true;
    let (base, _state, _guard) = boot_with_state(cfg).await;
    let response = reqwest::Client::new()
        .get(format!("{base}/api/session-templates"))
        .headers(auth())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let rows: Vec<Value> = response.json().await.unwrap();
    assert_eq!(
        rows,
        vec![json!({
            "name": "Pod Claude",
            "agent": "claude",
            "placements": ["host", "pod"],
            "default_placement": "pod"
        })]
    );
}

#[tokio::test]
async fn a_host_placement_overrides_a_pod_template_default() {
    let mut h = pod_stream::harness_cfg(false, None, |_| {}).await;
    let id = h
        .start_body(json!({
            "name": "host-session",
            "template": "pod",
            "placement": "host"
        }))
        .await;
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), h.created.recv())
            .await
            .is_err(),
        "host placement unexpectedly created a pod"
    );
    h.state.sessions.remove(id).unwrap();
}
