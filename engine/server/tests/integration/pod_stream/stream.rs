use super::*;

#[tokio::test]
async fn an_unreachable_mint_still_starts_the_pod_without_a_token() {
    let mut h = core_harness(false).await;
    let minter = FakeMinter::new(Err("graft could not be reached to mint a session token"));
    install_minter(&h, minter.clone());
    let id = h
        .start_with(
            json!({ "source": graft_source(), "session_ref": "ses_00000000000000000000000002" }),
        )
        .await;
    // The pod starts anyway: a graft outage must not make sessions
    // unavailable. The token is absent and the session says why.
    let (created_id, token) = h.created.recv().await.expect("a pod is created");
    assert_eq!(created_id, id);
    let body: Value = h.bootstrap(id, &token).await.json().await.unwrap();
    assert!(body.get("source_token").is_none(), "{body}");
    assert!(body.get("graft_session").is_none(), "{body}");
    let out = String::from_utf8_lossy(&h.scrollback(id).await).into_owned();
    assert!(out.contains("session graft token unavailable"), "{out}");
    assert!(out.contains("graft could not be reached"), "{out}");
    assert_eq!(minter.grants.lock().unwrap().len(), 1);
    assert_eq!(h.detail(id).await.summary.exit_code, None);
}

#[tokio::test]
async fn a_project_without_a_graft_source_earns_no_graft_token() {
    // No project source: the deployment default only names what to clone,
    // it is not a mapping that earns a repo-scoped landing token.
    let mut h = harness(false).await;
    let minter = FakeMinter::new(Ok("minted-graft-token"));
    install_minter(&h, minter.clone());
    let id = h.start().await;
    let (created_id, token) = h.created.recv().await.expect("a pod is created");
    assert_eq!(created_id, id);
    let body: Value = h.bootstrap(id, &token).await.json().await.unwrap();
    assert!(body.get("source_token").is_none(), "{body}");
    assert!(minter.grants.lock().unwrap().is_empty());
    let out = String::from_utf8_lossy(&h.scrollback(id).await).into_owned();
    assert!(out.contains("names no graft source"), "{out}");
}

#[tokio::test]
async fn a_git_project_neither_mints_nor_earns_a_graft_denial_note() {
    let mut h = core_harness(false).await;
    let minter = FakeMinter::new(Ok("minted-graft-token"));
    install_minter(&h, minter.clone());
    let id = h
            .start_with(json!({
                "source": {
                    "location": {"kind": "git", "url": "https://example.invalid/vogt.git", "repo": "vogt"},
                    "ref": "main",
                    "landing": "open_pr"
                }
            }))
            .await;
    let (_, token) = h.created.recv().await.expect("a pod is created");
    let body: Value = h.bootstrap(id, &token).await.json().await.unwrap();
    assert!(body.get("source_token").is_none(), "{body}");
    assert!(minter.grants.lock().unwrap().is_empty());
    let out = String::from_utf8_lossy(&h.scrollback(id).await).into_owned();
    assert!(!out.contains("session graft token unavailable"), "{out}");
}

#[tokio::test]
async fn sibling_sessions_each_get_their_own_graft_token() {
    // Oversight starts children through the same session.start path, so
    // each one mints its own token rather than sharing the parent's.
    let mut h = core_harness(false).await;
    let minter = FakeMinter::new(Ok("minted-graft-token"));
    install_minter(&h, minter.clone());
    let first = h
        .start_with(
            json!({ "source": graft_source(), "session_ref": "ses_00000000000000000000000003" }),
        )
        .await;
    let (_, first_token) = h.created.recv().await.expect("first pod");
    let second = h
        .start_with(
            json!({ "source": graft_source(), "session_ref": "ses_00000000000000000000000004" }),
        )
        .await;
    let (_, second_token) = h.created.recv().await.expect("second pod");
    assert_ne!(first_token, second_token);
    let sessions: Vec<String> = minter
        .grants
        .lock()
        .unwrap()
        .iter()
        .map(|g| g.session.clone())
        .collect();
    assert_eq!(
        sessions,
        [
            "ses_00000000000000000000000003",
            "ses_00000000000000000000000004"
        ]
    );
    for (id, token) in [(first, first_token), (second, second_token)] {
        let body: Value = h.bootstrap(id, &token).await.json().await.unwrap();
        assert_eq!(
            body["graft_session"],
            if id == first {
                "ses_00000000000000000000000003"
            } else {
                "ses_00000000000000000000000004"
            }
        );
    }
}

#[tokio::test]
async fn a_start_the_cluster_quota_refuses_is_a_conflict_and_leaves_no_session() {
    let h = harness(false).await;
    *h.launcher.refuse.lock().unwrap() = Some(
        "the cluster refused the session pod: exceeded quota: vogt-sessions, \
             requested: limits.cpu=4, used: limits.cpu=32, limited: limits.cpu=32"
            .into(),
    );
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let response = client
        .post(format!("{}/api/sessions", h.base))
        .json(&json!({ "name": "ninth", "template": "pod" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let body = response.text().await.unwrap();
    assert!(
        body.contains("vogt-sessions") && body.contains("limited: limits.cpu=32"),
        "{body}"
    );
    let listed: Value = client
        .get(format!("{}/api/sessions", h.base))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        !listed.to_string().contains("ninth"),
        "no session row is left: {listed}"
    );
}

#[tokio::test]
async fn a_pod_create_that_fails_after_the_mint_leaves_no_token_behind() {
    let h = core_harness(true).await;
    install_minter(&h, FakeMinter::new(Ok("minted-token")));
    let id = h
        .start_with(
            json!({ "source": graft_source(), "session_ref": "ses_00000000000000000000000005" }),
        )
        .await;
    tokio::time::timeout(Duration::from_secs(10), async {
        while h.detail(id).await.summary.exit_code != Some(1) {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("a failed pod creation ends the session");
    assert!(
        !vogt_engine_server::agent_stream::global()
            .bootstrap_pending(id)
            .await
    );
}

#[tokio::test]
async fn without_a_mint_token_the_pod_gets_no_minted_token() {
    // No minter installed and no ENGINE_GRAFT_MINT_TOKEN_FILE: the
    // fallback path. (The shared-file half is covered by the
    // resolve_graft_token unit tests.)
    let mut h = core_harness(false).await;
    let id = h.start_with(json!({ "source": graft_source() })).await;
    let (_, token) = h.created.recv().await.unwrap();
    let body: Value = h.bootstrap(id, &token).await.json().await.unwrap();
    assert!(body.get("source_token").is_none(), "{body}");
}

#[tokio::test]
async fn the_revoke_hook_runs_when_the_session_ends() {
    let mut h = core_harness(false).await;
    let minter = FakeMinter::new(Ok("minted-token"));
    install_minter(&h, minter.clone());
    let id = h.start_with(json!({ "source": graft_source() })).await;
    let _ = h.created.recv().await.unwrap();
    let kill = h
        .client
        .post(format!("{}/api/sessions/{id}/kill", h.base))
        .send()
        .await
        .unwrap();
    assert_eq!(kill.status(), StatusCode::OK);
    tokio::time::timeout(Duration::from_secs(10), async {
        while minter.revoked.lock().unwrap().is_empty() {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("the hook runs at teardown");
    assert_eq!(*minter.revoked.lock().unwrap(), [id]);
}

#[tokio::test]
async fn a_host_session_gets_a_graft_token_file_and_never_the_value() {
    let out = tempfile::tempdir().unwrap();
    let out = out.path().join("env");
    let mut h = host_harness(&out, None).await;
    let minter = FakeMinter::new(Ok("host-minted-SECRET"));
    install_minter(&h, minter.clone());
    let _id = h
        .start_with(json!({
            "source": graft_source(),
            "placement": "host",
            "session_ref": "ses_00000000000000000000000006",
            "work_item": "WI-3986",
        }))
        .await;
    let env = host_env(&out).await;
    let path = host_token_path("ses_00000000000000000000000006");
    assert!(env.contains(&format!("VOGT_GRAFT_TOKEN_FILE={}", path.display())));
    assert!(env.contains("GRAFT_SESSION_ID=ses_00000000000000000000000006"));
    assert!(!env.contains("host-minted-SECRET"), "token value in env");
    assert!(!env.contains("VOGT_GRAFT_TOKEN_UNAVAILABLE"));
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "host-minted-SECRET"
    );
    let grants = minter.grants.lock().unwrap();
    assert_eq!(grants.len(), 1);
    assert_eq!(grants[0].work_item.as_deref(), Some("WI-3986"));
    assert!(h.created.try_recv().is_err(), "a host session makes no pod");
}

/// A project's graft source that leaves `landing` out reads as
/// `push_branch`, the schema default. A graft source only ever lands on
/// its line, and pods mint for it, so a host worker oversight starts must
/// get the same token: read, write and land on the line, TTL bounded.
#[tokio::test]
async fn a_host_session_on_a_graft_source_without_landing_gets_a_land_scoped_token() {
    let out = tempfile::tempdir().unwrap();
    let out = out.path().join("env");
    let h = host_harness(&out, None).await;
    let minter = FakeMinter::new(Ok("host-no-landing"));
    install_minter(&h, minter.clone());
    let mut source = graft_source();
    source.as_object_mut().unwrap().remove("landing");
    let _id = h
        .start_with(json!({
            "source": source,
            "placement": "host",
            "session_ref": "ses_0000000000000000000000000A",
            "work_item": "WI-3986",
        }))
        .await;
    let env = host_env(&out).await;
    let path = host_token_path("ses_0000000000000000000000000A");
    assert!(
        env.contains(&format!("VOGT_GRAFT_TOKEN_FILE={}", path.display())),
        "a graft source without landing got no host token file"
    );
    assert!(path.exists());
    let grants = minter.grants.lock().unwrap();
    assert_eq!(grants.len(), 1);
    let scopes = grants[0].scopes();
    for scope in ["repo:vogt:read", "repo:vogt:write", "repo:vogt:land:rust-2"] {
        assert!(scopes.iter().any(|s| s == scope), "{scope} in {scopes:?}");
    }
    assert!(grants[0].ttl_s > 0);
    assert!(grants[0].ttl_s <= vogt_engine_server::graft_mint::MAX_TTL_S);
}

#[tokio::test]
async fn a_pod_placed_session_gets_no_host_graft_file() {
    let out = tempfile::tempdir().unwrap();
    let mut h = host_harness(&out.path().join("env"), None).await;
    install_minter(&h, FakeMinter::new(Ok("pod-minted")));
    let _id = h
        .start_with(json!({
            "source": graft_source(),
            "placement": "pod",
            "session_ref": "ses_00000000000000000000000007",
        }))
        .await;
    let _ = h.created.recv().await.expect("the pod is created");
    assert!(!host_token_path("ses_00000000000000000000000007").exists());
}

#[tokio::test]
async fn a_host_mint_failure_starts_the_session_without_a_token_and_says_why() {
    let out = tempfile::tempdir().unwrap();
    let out = out.path().join("env");
    let h = host_harness(&out, None).await;
    install_minter(&h, FakeMinter::new(Err("graft is down")));
    let _id = h
        .start_with(json!({
            "source": graft_source(),
            "placement": "host",
            "session_ref": "ses_00000000000000000000000008",
        }))
        .await;
    let env = host_env(&out).await;
    assert!(!env.contains("VOGT_GRAFT_TOKEN_FILE"), "{env}");
    assert!(env.contains("VOGT_GRAFT_TOKEN_UNAVAILABLE="), "{env}");
    assert!(env.contains("graft is down"), "{env}");
    assert!(!host_token_path("ses_00000000000000000000000008").exists());
}

#[tokio::test]
async fn a_host_create_that_fails_revokes_and_removes_the_credential() {
    let out = tempfile::tempdir().unwrap();
    let h = host_harness(&out.path().join("env"), Some("/nonexistent/vogt-test-bin")).await;
    let minter = FakeMinter::new(Ok("host-create-fail"));
    install_minter(&h, minter.clone());
    let response = h
        .client
        .post(format!("{}/api/sessions", h.base))
        .json(&json!({
            "name": "x", "template": "pod", "placement": "host",
            "source": graft_source(), "session_ref": "ses_00000000000000000000000009",
        }))
        .send()
        .await
        .unwrap();
    assert!(!response.status().is_success(), "{}", response.status());
    assert!(!host_token_path("ses_00000000000000000000000009").exists());
    assert_eq!(minter.revoked.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn host_exit_revokes_and_hibernate_then_wake_mints_anew() {
    let out = tempfile::tempdir().unwrap();
    let out = out.path().join("env");
    let h = host_harness(&out, None).await;
    let minter = FakeMinter::new(Ok("host-lifecycle"));
    install_minter(&h, minter.clone());
    let start = |r: &'static str| {
        h.start_with(json!({
            "source": graft_source(), "placement": "host", "session_ref": r,
        }))
    };

    // Hibernate revokes and removes; wake mints a fresh credential.
    let id = start("ses_00000000000000000000000000").await;
    host_env(&out).await;
    let path = host_token_path("ses_00000000000000000000000000");
    assert!(path.exists());
    h.state
        .sessions
        .hibernate(
            id,
            Some("test".into()),
            vogt_engine_contract::HibernateTrigger::Manual,
            true,
            true,
            None,
        )
        .await
        .unwrap();
    until("hibernate revoke", || {
        !minter.revoked.lock().unwrap().is_empty()
    })
    .await;
    assert!(!path.exists(), "the file goes with the process");
    let woke = h
        .client
        .post(format!("{}/api/sessions/{id}/wake", h.base))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(woke.status(), StatusCode::OK);
    assert!(path.exists(), "wake delivers a fresh file");
    assert_eq!(minter.grants.lock().unwrap().len(), 2);

    // Exit revokes and removes.
    let kill = h
        .client
        .post(format!("{}/api/sessions/{id}/kill", h.base))
        .send()
        .await
        .unwrap();
    assert_eq!(kill.status(), StatusCode::OK);
    until("exit revoke", || minter.revoked.lock().unwrap().len() == 2).await;
    assert!(!path.exists());
}

#[tokio::test]
async fn a_pod_that_cannot_be_created_ends_the_session() {
    let h = harness(true).await;
    let id = h.start().await;
    // A failed create reports the same way as a failed mint: the exit
    // code can be visible before the note is, so wait for the text
    // (WI-3289).
    let sid = id.to_string();
    let exited = super::session_output_after_exit(
        &h.client,
        &h.base,
        &sid,
        &["session pod could not be created"],
    )
    .await;
    let out = &exited.printed;
    assert_eq!(
        exited.exit_code,
        Some(1),
        "a failed pod creation ends the session with exit 1; got {out:?}"
    );
    assert!(out.contains("session pod could not be created"), "{out}");
}

#[tokio::test]
async fn reconcile_ends_a_session_whose_pod_died_and_reports_orphans() {
    use k8s_openapi::api::core::v1::{
        ContainerState, ContainerStateTerminated, ContainerStatus, Pod, PodStatus,
    };
    let mut h = harness(false).await;
    let id = h.start().await;
    let (_, _token) = h.created.recv().await.expect("a pod is created");

    let labelled = |id: Uuid, status: PodStatus| {
        let mut pod = Pod::default();
        pod.metadata.labels = Some(
            [
                ("vogt.session".to_string(), id.to_string()),
                (
                    vogt_engine_server::pod_backend::ENGINE_LABEL.to_string(),
                    vogt_engine_server::pod_backend::DEFAULT_ENGINE_ID.to_string(),
                ),
            ]
            .into_iter()
            .collect(),
        );
        pod.metadata.uid = Some(format!("uid-{id}"));
        pod.status = Some(status);
        pod
    };
    let orphan = Uuid::new_v4();
    *h.launcher.listed.lock().unwrap() = vec![
        labelled(
            id,
            PodStatus {
                phase: Some("Failed".into()),
                container_statuses: Some(vec![ContainerStatus {
                    state: Some(ContainerState {
                        terminated: Some(ContainerStateTerminated {
                            exit_code: 137,
                            message: Some("OOMKilled".into()),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }),
                    ..Default::default()
                }]),
                ..Default::default()
            },
        ),
        labelled(
            orphan,
            PodStatus {
                phase: Some("Running".into()),
                ..Default::default()
            },
        ),
    ];

    // The agent stream registry is process-wide and other tests' sessions
    // are live in it: a long grace keeps this sweep to the pods it lists.
    let cfg = vogt_engine_server::pod_backend::PodConfig {
        connect_timeout: Duration::from_secs(3600),
        ..Default::default()
    };
    let report = vogt_engine_server::pod_reconcile::sweep(
        h.launcher.as_ref(),
        &h.state.agent_auth,
        &cfg,
        &|_| true,
    )
    .await
    .unwrap();
    assert_eq!(report.ended, vec![id]);
    assert_eq!(
        report
            .reported
            .iter()
            .map(|(id, _)| *id)
            .collect::<Vec<_>>(),
        vec![orphan]
    );

    tokio::time::timeout(Duration::from_secs(10), async {
        while h.detail(id).await.summary.exit_code != Some(137) {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("the dead pod ends the session");
    // The exit can be seen a moment before the reader thread has drained
    // the note into the scrollback.
    tokio::time::timeout(Duration::from_secs(10), async {
        while !String::from_utf8_lossy(&h.scrollback(id).await).contains("OOMKilled") {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("the reason reaches the scrollback");
    // The session's start task removes its own pod once the session ends.
    tokio::time::timeout(Duration::from_secs(10), async {
        while !h.launcher.deleted.lock().unwrap().contains(&id) {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("the ended session's pod is deleted");
    assert!(
        !h.launcher.deleted.lock().unwrap().contains(&orphan),
        "an orphan pod is reported, never deleted: its workspace is the only copy"
    );
}

/// WI-3878: a Running pod whose attached agent stops sending heartbeats
/// is ended by the reconciler with the heartbeat-specific reason, and the
/// deadline comes from the environment-backed setting.
#[tokio::test]
async fn a_running_pods_silent_agent_ends_at_the_heartbeat_timeout() {
    let mut h = harness(false).await;
    let id = h.start().await;
    let token = h.session_token(id).await;
    let mut agent = h.dial(id, &token, 0).await;
    assert_eq!(take(&mut agent).await, Frame::Ack { offset: 0 });

    *h.launcher.listed.lock().unwrap() = vec![running_pod(id)];
    let env = [("ENGINE_POD_HEARTBEAT_TIMEOUT", "1")];
    let cfg = vogt_engine_server::pod_backend::PodConfig::from_lookup(
        |name| {
            env.iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_string())
        },
        None,
    )
    .unwrap();
    assert_eq!(cfg.heartbeat_timeout, Duration::from_secs(1));

    tokio::time::sleep(Duration::from_millis(1_100)).await;
    let report = vogt_engine_server::pod_reconcile::sweep(
        h.launcher.as_ref(),
        &h.state.agent_auth,
        &cfg,
        &|_| true,
    )
    .await
    .unwrap();
    assert_eq!(report.ended, vec![id]);
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let detail = h.detail(id).await;
            if detail.summary.exit_code == Some(1)
                && String::from_utf8_lossy(&h.scrollback(id).await)
                    .contains("agent heartbeat silent for more than 1s")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("the heartbeat timeout ends the session with its own reason");
    drop(agent);
    ended(&h.state.sessions, id).await;
}

#[tokio::test]
async fn hibernate_flushes_the_transcript_deletes_the_pod_and_wake_resumes_it_on_the_same_account()
{
    let (mut h, _accounts) = claude_harness().await;
    let id = h.start().await;
    let conversation = conversation_of(&h, id).await;
    let token = h.session_token(id).await;
    let mut agent = h.dial(id, &token, 0).await;
    assert_eq!(take(&mut agent).await, Frame::Ack { offset: 0 });
    let session = h.state.sessions.get(id).unwrap();
    let (_, initial_rows, initial_cols) = session.screen_source(0);
    session.resize(101, 37).unwrap();
    assert_eq!(
        take(&mut agent).await,
        Frame::Resize {
            cols: 101,
            rows: 37
        }
    );
    let broker = h.state.sessions.secret_broker();
    broker
        .append_transcript(
            "acct",
            &conversation,
            &transcript_path(&conversation),
            0,
            b"turn one\n",
        )
        .unwrap();

    let pod_uid = h.launcher.cluster.lock().unwrap().get(&id).cloned();
    let hibernate = tokio::spawn({
        let client = h.client.clone();
        let url = format!("{}/api/sessions/{id}/hibernate", h.base);
        async move { client.post(url).json(&json!({})).send().await.unwrap() }
    });
    agent_hibernates(&h, &mut agent, "acct", &conversation, 9, b"turn two\n").await;
    let res = hibernate.await.unwrap();
    assert_eq!(res.status(), StatusCode::OK, "{:?}", res.text().await);
    assert!(
        h.launcher.deleted.lock().unwrap().contains(&id),
        "the pod is deleted"
    );
    assert!(pod_uid.is_some());
    assert_eq!(
        *h.launcher.delete_uids.lock().unwrap(),
        vec![(id, pod_uid)],
        "the teardown names the uid of the pod it created, never just the name"
    );
    assert!(h.state.sessions.is_hibernated(id));
    assert_eq!((initial_rows, initial_cols), (24, 80));
    let (_, rows, cols, _) = h.state.sessions.hibernated_screen(id).unwrap();
    assert_eq!((rows, cols), (37, 101));
    assert_eq!(
        h.detail(id).await.summary.activity,
        ActivityState::Hibernated
    );

    // Wake: a new pod on the same account, whose bootstrap carries the
    // newest transcript and tells the agent to resume it.
    let woken = h
        .client
        .post(format!("{}/api/sessions/{id}/wake", h.base))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(woken.status(), StatusCode::OK);
    let (created_id, bootstrap) = h.created.recv().await.expect("a new pod is created");
    assert_eq!(created_id, id);
    let pod = h.launcher.pods.lock().unwrap().pop().expect("pod");
    assert_eq!(
        pod.metadata.labels.as_ref().unwrap().get("vogt.account"),
        Some(&"acct".to_string()),
        "the wake keeps the account"
    );
    let body: Value = h.bootstrap(id, &bootstrap).await.json().await.unwrap();
    let seeded = body["bundle"][transcript_path(&conversation)]
        .as_str()
        .expect("the transcript is seeded into the new pod");
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(seeded)
            .unwrap(),
        b"turn one\nturn two\n"
    );
    assert_eq!(body["env"]["VOGT_SESSION_RESUME"], "1");
    assert_eq!(body["env"]["VOGT_SESSION_ACCOUNT"], "acct");
    assert_eq!(body["env"]["VOGT_SESSION_CONVERSATION_ID"], conversation);
    let summary = h.state.sessions.summary_of(id).unwrap();
    assert_eq!(summary.account.as_deref(), Some("acct"));
    assert_ne!(summary.activity, ActivityState::Hibernated);
    let (_, rows, cols) = h.state.sessions.get(id).unwrap().screen_source(0);
    assert_eq!((rows, cols), (37, 101));
}

#[tokio::test]
async fn a_wake_right_after_hibernate_waits_for_the_terminating_pod() {
    let (mut h, _accounts) = claude_harness().await;
    // The old pod lingers in Terminating long after the delete returned.
    *h.launcher.terminating.lock().unwrap() = Duration::from_millis(1500);
    let id = h.start().await;
    let conversation = conversation_of(&h, id).await;
    let token = h.session_token(id).await;
    let old_uid = h
        .launcher
        .cluster
        .lock()
        .unwrap()
        .get(&id)
        .cloned()
        .unwrap();
    let mut agent = h.dial(id, &token, 0).await;
    assert_eq!(take(&mut agent).await, Frame::Ack { offset: 0 });
    h.state
        .sessions
        .secret_broker()
        .append_transcript(
            "acct",
            &conversation,
            &transcript_path(&conversation),
            0,
            b"turn one\n",
        )
        .unwrap();
    let hibernate = tokio::spawn({
        let client = h.client.clone();
        let url = format!("{}/api/sessions/{id}/hibernate", h.base);
        async move { client.post(url).json(&json!({})).send().await.unwrap() }
    });
    agent_hibernates(&h, &mut agent, "acct", &conversation, 9, b"turn two\n").await;
    assert_eq!(hibernate.await.unwrap().status(), StatusCode::OK);

    // Wake straight away: the hibernate has returned, so the old pod must
    // be gone from the cluster already, not still Terminating.
    let woken = h
        .client
        .post(format!("{}/api/sessions/{id}/wake", h.base))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(woken.status(), StatusCode::OK);
    let (created_id, _) = tokio::time::timeout(Duration::from_secs(10), h.created.recv())
        .await
        .expect("the wake creates its pod instead of meeting the old one")
        .expect("a new pod is created");
    assert_eq!(created_id, id);
    let new_uid = h
        .launcher
        .cluster
        .lock()
        .unwrap()
        .get(&id)
        .cloned()
        .unwrap();
    assert_ne!(new_uid, old_uid, "a replacement, not the old pod");
    assert_ne!(
        h.detail(id).await.summary.activity,
        ActivityState::Hibernated
    );
    assert_eq!(
        *h.launcher.delete_uids.lock().unwrap(),
        vec![(id, Some(old_uid))]
    );
}

#[tokio::test]
async fn hibernate_is_refused_when_an_attached_agent_cannot_write_back() {
    let (mut h, _accounts) = claude_harness().await;
    let id = h.start().await;
    let token = h.session_token(id).await;
    let mut agent = h.dial(id, &token, 0).await;
    assert_eq!(take(&mut agent).await, Frame::Ack { offset: 0 });
    let hibernate = tokio::spawn({
        let client = h.client.clone();
        let url = format!("{}/api/sessions/{id}/hibernate", h.base);
        async move { client.post(url).json(&json!({})).send().await.unwrap() }
    });
    // The agent hears the flush and cannot store the transcript: the engine
    // refuses rather than delete a pod holding unsaved turns.
    assert_eq!(take(&mut agent).await, Frame::Flush);
    put(&mut agent, Frame::Flushed { ok: false }).await;
    let res = hibernate.await.unwrap();
    assert_eq!(res.status(), StatusCode::CONFLICT);
    assert!(h.launcher.deleted.lock().unwrap().is_empty());
    assert!(!h.state.sessions.is_hibernated(id));
    // Stop the session so its reader ends with the test.
    h.state.sessions.get(id).unwrap().kill().unwrap();
    put(&mut agent, Frame::Exited { status: Some(0) }).await;
    ended(&h.state.sessions, id).await;
}

#[tokio::test]
async fn restart_adopts_a_pod_before_it_reports_a_conversation() {
    let mut h = harness(false).await;
    let id = h.start().await;
    let token = h.session_token(id).await;
    assert!(h
        .state
        .sessions
        .summary_of(id)
        .unwrap()
        .conversation
        .is_none());
    h.state.sessions.hibernate_for_shutdown().await;
    assert!(
        h.state.sessions.get(id).is_ok_and(|s| s.is_alive()),
        "shutdown keeps a pod session live"
    );
    assert!(
        h.launcher.deleted.lock().unwrap().is_empty(),
        "shutdown never deletes a session pod"
    );
    // The record is persisted without a hibernation marker, so boot sees
    // a session that was running and can re-adopt its pod.
    let record_path = h
        .state
        .config
        .state_dir
        .join("sessions")
        .join(format!("{id}.json"));
    let record: Value =
        serde_json::from_str(&std::fs::read_to_string(&record_path).unwrap()).unwrap();
    assert!(
        record["hibernation"].is_null(),
        "a pod session's shutdown record must stay live: {record}"
    );
    let old_link = h.state.agent_auth.pod(id).unwrap();
    h.state.agent_auth.forget_pod(id);
    let launcher = fake_launcher(vec![running_pod(id)]);
    let registry = restarted(&h, launcher.clone());
    let adopted = registry.readopt_pods().await;
    old_link.finish(137);
    assert_eq!(adopted, vec![id]);
    let sweep = vogt_engine_server::pod_reconcile::sweep(
        launcher.as_ref(),
        &h.state.agent_auth,
        &registry.pod_config().unwrap(),
        &|_| true,
    )
    .await
    .unwrap();
    assert!(sweep.reported.is_empty());
    assert!(launcher.deleted.lock().unwrap().is_empty());
    assert!(registry.get(id).unwrap().is_alive());
    let mut agent = h.dial(id, &token, 0).await;
    assert_eq!(take(&mut agent).await, Frame::Ack { offset: 0 });
    put(&mut agent, Frame::Exited { status: Some(0) }).await;
    ended(&registry, id).await;
}

/// The old engine's link can end while it is shutting down (its stream
/// drops with the process). That must not delete the pod or its stream
/// credential: the next engine adopts both (WI-4126).
#[tokio::test]
async fn a_link_that_ends_during_shutdown_leaves_the_pod_and_credential() {
    let mut h = harness(false).await;
    let id = h.start().await;
    let _token = h.session_token(id).await;
    let launcher = Arc::clone(&h.launcher);
    let state_dir = h.state.config.state_dir.clone();
    let credential = state_dir.join("pod-tokens").join(id.to_string());
    assert!(credential.exists(), "bootstrap keeps the stream credential");
    h.state.sessions.hibernate_for_shutdown().await;
    let link = h.state.agent_auth.pod(id).unwrap();
    link.finish(137);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        launcher.deleted.lock().unwrap().is_empty(),
        "a link ending during shutdown must not delete the pod"
    );
    assert!(
        credential.exists(),
        "the stream credential is kept for the next engine"
    );
    let record = state_dir.join("sessions").join(format!("{id}.json"));
    assert!(record.exists(), "the record is kept for the next engine");
}

#[tokio::test]
async fn a_restarted_engine_adopts_the_running_pod_and_replays_exactly() {
    let (mut h, _accounts) = claude_harness().await;
    let id = h.start().await;
    let token = h.session_token(id).await;
    let mut agent = h.dial(id, &token, 0).await;
    assert_eq!(take(&mut agent).await, Frame::Ack { offset: 0 });
    put(
        &mut agent,
        Frame::Output {
            offset: 0,
            data: b"hello world".to_vec(),
        },
    )
    .await;
    assert_eq!(take(&mut agent).await, Frame::Ack { offset: 11 });
    log_len(&h, id, 11).await;

    // The engine dies. Its streams and credentials go with its memory; the
    // pod and the agent inside it do not.
    let old_link = h.state.agent_auth.pod(id).unwrap();
    h.state.agent_auth.forget_pod(id);
    drop(agent);
    let launcher = fake_launcher(vec![running_pod(id)]);
    let registry = restarted(&h, launcher.clone());
    let adopted = registry.readopt_pods().await;
    // The dead process's session ends with it.
    old_link.finish(137);
    assert_eq!(adopted, vec![id]);
    assert!(!registry.is_hibernated(id), "adopted, no longer recovered");
    assert!(
        launcher.deleted.lock().unwrap().is_empty(),
        "the pod is kept"
    );

    // The agent redials with the credential it already holds, claims the
    // output it last had acknowledged, and replays an overlap plus new
    // bytes. Nothing is dropped or doubled.
    let mut agent = h.dial(id, &token, 6).await;
    assert_eq!(take(&mut agent).await, Frame::Ack { offset: 11 });
    put(
        &mut agent,
        Frame::Output {
            offset: 6,
            data: b"worldXYZ!!".to_vec(),
        },
    )
    .await;
    assert_eq!(take(&mut agent).await, Frame::Ack { offset: 16 });
    // The ack means the bytes are in the link's channel, not yet in the
    // scrollback: the session's reader thread moves them there after.
    // Wait for it (WI-3286), and end the session before asserting, so a
    // mismatch fails instead of hanging on the reader thread at runtime
    // drop.
    let session = registry.get(id).unwrap();
    let settled = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let (bytes, _) = session.snapshot();
            if bytes.len() >= 5 {
                return bytes;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    put(&mut agent, Frame::Exited { status: Some(0) }).await;
    ended(&registry, id).await;
    let bytes = settled.expect("the replayed output reaches the scrollback");
    assert_eq!(String::from_utf8_lossy(&bytes), "XYZ!!");
}

/// WI-4499 / WI-4497: the agent's last OSC 7501 report survives an engine
/// restart that adopts its pod, and boot never writes the pod's record as
/// hibernated on its way to adopting it.
#[tokio::test]
async fn a_restarted_engine_keeps_the_adopted_pods_reported_status() {
    let (mut h, _accounts) = claude_harness().await;
    let id = h.start().await;
    let token = h.session_token(id).await;
    let mut agent = h.dial(id, &token, 0).await;
    assert_eq!(take(&mut agent).await, Frame::Ack { offset: 0 });
    let report = b"\x1b]7501;state=done:app=claude-code\x1b\\".to_vec();
    let len = report.len() as u64;
    put(
        &mut agent,
        Frame::Output {
            offset: 0,
            data: report,
        },
    )
    .await;
    assert_eq!(take(&mut agent).await, Frame::Ack { offset: len });
    log_len(&h, id, len).await;
    let before = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(status) = h.state.sessions.summary_of(id).unwrap().reported_status {
                return status;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the report reaches the summary");
    assert_eq!(before.app.as_deref(), Some("claude-code"));

    let old_link = h.state.agent_auth.pod(id).unwrap();
    h.state.agent_auth.forget_pod(id);
    drop(agent);
    let launcher = fake_launcher(vec![running_pod(id)]);
    let registry = restarted(&h, launcher.clone());
    let record_path = h
        .state
        .config
        .state_dir
        .join("sessions")
        .join(format!("{id}.json"));
    let record: Value =
        serde_json::from_str(&std::fs::read_to_string(&record_path).unwrap()).unwrap();
    assert!(
        record["hibernation"].is_null(),
        "boot must not write an adoptable pod's record as hibernated: {record}"
    );
    let adopted = registry.readopt_pods().await;
    old_link.finish(137);
    assert_eq!(adopted, vec![id]);
    let after = registry.summary_of(id).unwrap().reported_status;

    let mut agent = h.dial(id, &token, len).await;
    assert_eq!(take(&mut agent).await, Frame::Ack { offset: len });
    put(&mut agent, Frame::Exited { status: Some(0) }).await;
    ended(&registry, id).await;
    assert_eq!(after, Some(before), "the report survives the restart");
}

/// WI-4497: a recovered record boot held back for adoption whose pod is
/// gone is written as hibernated once the readopt finds nothing.
#[tokio::test]
async fn a_held_record_whose_pod_is_gone_is_kept_as_hibernated() {
    let mut h = harness(false).await;
    let id = h.start().await;
    let _token = h.session_token(id).await;
    let old_link = h.state.agent_auth.pod(id).unwrap();
    h.state.agent_auth.forget_pod(id);
    let registry = restarted(&h, fake_launcher(Vec::new()));
    assert!(registry.readopt_pods().await.is_empty());
    old_link.finish(137);
    let record_path = h
        .state
        .config
        .state_dir
        .join("sessions")
        .join(format!("{id}.json"));
    let record: Value =
        serde_json::from_str(&std::fs::read_to_string(&record_path).unwrap()).unwrap();
    assert_eq!(record["hibernation"]["trigger"], "recovered", "{record}");
    assert!(registry.is_hibernated(id));
}

/// WI-4442: the engine dies with no shutdown hook while the agent is
/// mid-output. The dead engine took a chunk from the agent and never
/// wrote it, and never acknowledged it. The next engine adopts the pod at
/// the log's end, the agent resumes from its last ack and resends, and the
/// log holds every byte exactly once: no gap, no duplicate.
#[tokio::test]
async fn a_forced_kill_mid_output_adopts_the_pod_with_no_gap_or_duplicate() {
    let (mut h, _accounts) = claude_harness().await;
    let id = h.start().await;
    let token = h.session_token(id).await;
    let mut agent = h.dial(id, &token, 0).await;
    assert_eq!(take(&mut agent).await, Frame::Ack { offset: 0 });
    put(
        &mut agent,
        Frame::Output {
            offset: 0,
            data: b"hello world".to_vec(),
        },
    )
    .await;
    assert_eq!(take(&mut agent).await, Frame::Ack { offset: 11 });
    put(
        &mut agent,
        Frame::Output {
            offset: 11,
            data: b" more-1".to_vec(),
        },
    )
    .await;
    assert_eq!(take(&mut agent).await, Frame::Ack { offset: 18 });
    let log_path = h.state.history.as_ref().unwrap().log_path(id);
    log_len(&h, id, 18).await;

    // The engine dies: its session reader writes nothing it takes from
    // here on. The agent, mid-output, sends its next chunk. The dead engine
    // takes it and never writes it, so it is never acknowledged.
    let old_link = h.state.agent_auth.pod(id).unwrap();
    old_link.crash();
    put(
        &mut agent,
        Frame::Output {
            offset: 18,
            data: b"MID".to_vec(),
        },
    )
    .await;
    // What the dead engine's socket says about it: no ack past 18.
    let mut heard = Vec::new();
    while let Ok(Some(Ok(message))) =
        tokio::time::timeout(Duration::from_millis(500), agent.next()).await
    {
        if let Message::Text(text) = message {
            heard.push(serde_json::from_str::<Frame>(&text).unwrap());
        }
    }
    assert!(
        !heard
            .iter()
            .any(|f| matches!(f, Frame::Ack { offset } if *offset > 18)),
        "the agent was acked for bytes the dead engine never wrote: {heard:?}"
    );
    assert_eq!(
        std::fs::metadata(&log_path).unwrap().len(),
        18,
        "the crashed engine wrote nothing further"
    );
    drop(agent);
    h.state.agent_auth.forget_pod(id);

    let launcher = fake_launcher(vec![running_pod(id)]);
    let registry = restarted(&h, launcher.clone());
    let adopted = registry.readopt_pods().await;
    old_link.finish(137);
    assert_eq!(adopted, vec![id]);
    assert!(
        launcher.deleted.lock().unwrap().is_empty(),
        "the pod is kept"
    );

    // The agent's last ack was 18, so it resumes there. The engine answers
    // with exactly what its log holds, and the agent resends from there.
    let mut agent = h.dial(id, &token, 18).await;
    assert_eq!(take(&mut agent).await, Frame::Ack { offset: 18 });
    put(
        &mut agent,
        Frame::Output {
            offset: 18,
            data: b"MID".to_vec(),
        },
    )
    .await;
    assert_eq!(take(&mut agent).await, Frame::Ack { offset: 21 });
    log_len(&h, id, 21).await;
    put(
        &mut agent,
        Frame::Output {
            offset: 21,
            data: b"END".to_vec(),
        },
    )
    .await;
    assert_eq!(take(&mut agent).await, Frame::Ack { offset: 24 });
    log_len(&h, id, 24).await;
    put(&mut agent, Frame::Exited { status: Some(0) }).await;
    ended(&registry, id).await;

    assert_eq!(
        std::fs::read(&log_path).unwrap(),
        b"hello world more-1MIDEND".to_vec(),
        "the client's output is continuous across the kill"
    );
}

/// WI-4442: an adopted link's silence is judged by the heartbeat timeout
/// only from the moment its agent reconnects. Before that the reconnect
/// timeout owns it, and the redial starts a full heartbeat window.
#[tokio::test]
async fn an_adopted_link_gets_a_full_heartbeat_timeout_from_its_reconnect() {
    let (mut h, _accounts) = claude_harness().await;
    let id = h.start().await;
    let token = h.session_token(id).await;
    let mut agent = h.dial(id, &token, 0).await;
    assert_eq!(take(&mut agent).await, Frame::Ack { offset: 0 });
    let old_link = h.state.agent_auth.pod(id).unwrap();
    h.state.agent_auth.forget_pod(id);
    drop(agent);

    let launcher = fake_launcher(vec![running_pod(id)]);
    let registry = restarted(&h, launcher.clone());
    let adopted = registry.readopt_pods().await;
    old_link.finish(137);
    assert_eq!(adopted, vec![id]);
    let link = h.state.agent_auth.pod(id).unwrap();
    // The agent is away for longer than the heartbeat timeout.
    tokio::time::sleep(Duration::from_millis(1_200)).await;
    let cfg = vogt_engine_server::pod_backend::PodConfig {
        heartbeat_timeout: Duration::from_secs(1),
        reconnect_timeout: Duration::from_secs(3600),
        connect_timeout: Duration::from_secs(3600),
        ..Default::default()
    };
    let pod = running_pod(id);
    assert!(
        matches!(
            vogt_engine_server::pod_reconcile::judge(&pod, Some(&link), &cfg, None),
            vogt_engine_server::pod_reconcile::Verdict::Keep
        ),
        "an away agent is the reconnect timeout's to judge, not the heartbeat's"
    );

    let mut agent = h.dial(id, &token, 0).await;
    assert_eq!(take(&mut agent).await, Frame::Ack { offset: 0 });
    let attached = tokio::time::timeout(Duration::from_secs(5), async {
        while !link.is_connected() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(attached.is_ok(), "the redial attaches");
    assert!(
        link.heartbeat_age() < cfg.heartbeat_timeout,
        "the reconnect starts a full heartbeat window"
    );
    assert!(
        matches!(
            vogt_engine_server::pod_reconcile::judge(&pod, Some(&link), &cfg, None),
            vogt_engine_server::pod_reconcile::Verdict::Keep
        ),
        "a freshly reconnected agent is not judged silent"
    );

    // Once the window passes with no heartbeat, the silence is judged.
    tokio::time::sleep(Duration::from_millis(1_100)).await;
    assert!(
        matches!(
            vogt_engine_server::pod_reconcile::judge(&pod, Some(&link), &cfg, None),
            vogt_engine_server::pod_reconcile::Verdict::End { .. }
        ),
        "a reconnected agent silent past the heartbeat timeout is ended"
    );
    put(&mut agent, Frame::Exited { status: Some(0) }).await;
    ended(&registry, id).await;
}

#[tokio::test]
async fn an_adopted_pod_whose_agent_never_returns_is_reported_and_left_running() {
    let (mut h, _accounts) = claude_harness().await;
    let id = h.start().await;
    let token = h.session_token(id).await;
    let mut agent = h.dial(id, &token, 0).await;
    // Only the adopted link matters here; the first ack's offset is not
    // the subject of this test.
    let _ = take(&mut agent).await;
    let old_link = h.state.agent_auth.pod(id).unwrap();
    h.state.agent_auth.forget_pod(id);
    drop(agent);
    let launcher = fake_launcher(vec![running_pod(id)]);
    let registry = restarted(&h, launcher.clone());
    let adopted = registry.readopt_pods().await;
    old_link.finish(137);
    assert_eq!(adopted, vec![id]);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !registry.is_hibernated(id),
        "a restart never hibernates (and so never deletes) a surviving pod"
    );
    assert!(
        launcher.deleted.lock().unwrap().is_empty(),
        "a pod whose agent has not dialled back is left running"
    );

    // The agent stream registry is process-wide; a long connect grace
    // keeps this sweep from judging other tests' sessions as disappeared.
    let cfg = vogt_engine_server::pod_backend::PodConfig {
        reconnect_timeout: Duration::from_millis(50),
        connect_timeout: Duration::from_secs(3600),
        ..Default::default()
    };
    let sweep = vogt_engine_server::pod_reconcile::sweep(
        launcher.as_ref(),
        &h.state.agent_auth,
        &cfg,
        &|_| true,
    )
    .await
    .unwrap();
    assert_eq!(
        sweep.reported.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
        vec![id]
    );
    assert!(launcher.deleted.lock().unwrap().is_empty());
    assert!(
        registry.get(id).is_ok_and(|s| s.is_alive()),
        "the adopted session stays live so a late agent can still take it over"
    );

    if let Some(link) = h.state.agent_auth.pod(id) {
        link.finish(0);
    }
    ended(&registry, id).await;
}

/// WI-3878: `ENGINE_POD_RECONNECT_TIMEOUT` (read through the same lookup
/// `PodConfig::from_env` uses) is what decides how long an adopted pod's
/// agent may take to redial before the reconciler reports it.
#[tokio::test]
async fn an_env_overridden_reconnect_timeout_reports_an_unreturning_pod() {
    let (mut h, _accounts) = claude_harness().await;
    let id = h.start().await;
    let token = h.session_token(id).await;
    let mut agent = h.dial(id, &token, 0).await;
    // Only the adopted link matters here; the first ack's offset is not
    // the subject of this test.
    let _ = take(&mut agent).await;
    let old_link = h.state.agent_auth.pod(id).unwrap();
    h.state.agent_auth.forget_pod(id);
    drop(agent);
    let launcher = fake_launcher(vec![running_pod(id)]);
    let registry = restarted(&h, launcher.clone());
    let env = [("ENGINE_POD_RECONNECT_TIMEOUT", "1")];
    let cfg = vogt_engine_server::pod_backend::PodConfig::from_lookup(
        |name| {
            env.iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_string())
        },
        None,
    )
    .unwrap();
    assert_eq!(cfg.reconnect_timeout, Duration::from_secs(1));
    // The agent stream registry is process-wide; a long connect grace
    // keeps this sweep from judging other tests' sessions as disappeared.
    let cfg = vogt_engine_server::pod_backend::PodConfig {
        connect_timeout: Duration::from_secs(3600),
        ..cfg
    };

    let adopted = registry.readopt_pods().await;
    old_link.finish(137);
    assert_eq!(adopted, vec![id]);
    tokio::time::sleep(Duration::from_millis(1_100)).await;
    let sweep = vogt_engine_server::pod_reconcile::sweep(
        launcher.as_ref(),
        &h.state.agent_auth,
        &cfg,
        &|_| true,
    )
    .await
    .unwrap();
    assert_eq!(
        sweep.reported.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
        vec![id],
        "the one-second reconnect timeout, not the 120 s default, reports the pod"
    );
    assert!(launcher.deleted.lock().unwrap().is_empty());
    assert!(!registry.is_hibernated(id));

    if let Some(link) = h.state.agent_auth.pod(id) {
        link.finish(0);
    }
    ended(&registry, id).await;
}

#[tokio::test]
async fn a_restart_reports_a_pod_whose_session_record_is_missing() {
    let (h, _accounts) = claude_harness().await;
    let orphan = Uuid::new_v4();
    let launcher = fake_launcher(vec![running_pod(orphan)]);
    let registry = restarted(&h, launcher.clone());
    assert!(
        registry.readopt_pods().await.is_empty(),
        "a pod with no session record cannot be adopted"
    );
    // The agent stream registry is process-wide; a long connect grace
    // keeps this sweep from judging other tests' sessions as disappeared.
    let cfg = vogt_engine_server::pod_backend::PodConfig {
        connect_timeout: Duration::from_secs(3600),
        ..Default::default()
    };
    let sweep = vogt_engine_server::pod_reconcile::sweep(
        launcher.as_ref(),
        &h.state.agent_auth,
        &cfg,
        &|_| true,
    )
    .await
    .unwrap();
    assert_eq!(
        sweep.reported.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
        vec![orphan]
    );
    assert!(
        launcher.deleted.lock().unwrap().is_empty(),
        "a pod with no session record is reported, not deleted"
    );
}

#[tokio::test]
async fn another_engines_pod_is_not_adopted() {
    let (mut h, _accounts) = claude_harness().await;
    let id = h.start().await;
    let _ = h.session_token(id).await;
    let old_link = h.state.agent_auth.pod(id).unwrap();
    h.state.agent_auth.forget_pod(id);
    let registry = restarted(&h, fake_launcher(vec![running_pod_of(id, "someone-else")]));
    old_link.finish(137);
    assert!(registry.readopt_pods().await.is_empty());
    assert!(registry.is_hibernated(id));
}

#[tokio::test]
async fn a_pod_without_a_kept_credential_is_not_adopted() {
    let (mut h, _accounts) = claude_harness().await;
    let id = h.start().await;
    let _ = h.session_token(id).await;
    let old_link = h.state.agent_auth.pod(id).unwrap();
    h.state.agent_auth.forget_pod(id);
    let _ = std::fs::remove_file(
        h.state
            .config
            .state_dir
            .join("pod-tokens")
            .join(id.to_string()),
    );
    let registry = restarted(&h, fake_launcher(vec![running_pod(id)]));
    old_link.finish(137);
    assert!(registry.readopt_pods().await.is_empty());
    assert!(registry.is_hibernated(id));
}
