// ---- Session-scoped file routes over the pod stream (WI-4869, WI-4863 D) ----
//
// The test plays the in-pod agent: it dials the real stream endpoint and
// answers every `FileOp` frame with the real `vogt_session_agent::file_ops`
// handler over a temp directory standing in for `/work`. The engine's
// `/api/sessions/{id}/files*` routes are then driven over HTTP.

use super::*;
use vogt_session_agent::file_ops;

/// A pod session with an attached fake agent serving `work`.
struct PodFiles {
    h: Harness,
    id: Uuid,
    work: tempfile::TempDir,
    agent: tokio::task::JoinHandle<()>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
}

/// Answers `FileOp` frames over `work` until told to stop, then reports the
/// child's exit as a real agent does, so the session ends and the test's
/// runtime can drop.
fn serve_work(
    mut ws: Agent,
    work: std::path::PathBuf,
    mut stop: tokio::sync::oneshot::Receiver<()>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let message = tokio::select! {
                _ = &mut stop => {
                    put(&mut ws, Frame::Exited { status: Some(0) }).await;
                    return;
                }
                next = ws.next() => match next {
                    Some(Ok(message)) => message,
                    _ => return,
                },
            };
            let Message::Text(text) = message else {
                continue;
            };
            match serde_json::from_str(&text) {
                Ok(Frame::FileOp { id, op }) => {
                    let result = file_ops::handle(&work, &op);
                    put(&mut ws, Frame::FileResult { id, result }).await;
                }
                Ok(Frame::Salvage) => answer_salvage(&mut ws).await,
                _ => {}
            }
        }
    })
}

async fn pod_files(tweak: impl FnOnce(&mut vogt_engine_server::config::Config)) -> PodFiles {
    let mut h = harness_cfg(false, None, tweak).await;
    let id = h.start().await;
    let token = h.session_token(id).await;
    // The agent is registered once the engine has acked the resume; the
    // frames before it (Resize) need no answer.
    let mut ws = h.dial(id, &token, 0).await;
    assert_eq!(take_skip_resize(&mut ws).await, Frame::Ack { offset: 0 });
    let work = tempfile::tempdir().unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let agent = serve_work(ws, work.path().to_path_buf(), stopped);
    PodFiles {
        h,
        id,
        work,
        agent,
        stop: Some(stop),
    }
}

impl PodFiles {
    fn url(&self, tail: &str) -> String {
        format!("{}/api/sessions/{}/{tail}", self.h.base, self.id)
    }

    /// The agent reports the child's exit and the session ends.
    async fn finish(mut self) {
        let _ = self.stop.take().unwrap().send(());
        self.agent.await.unwrap();
        ended(&self.h.state.sessions, self.id).await;
    }
}

#[tokio::test]
async fn the_engine_lists_and_reads_a_file_the_pod_wrote() {
    let p = pod_files(|_| {}).await;
    std::fs::create_dir_all(p.work.path().join("notes")).unwrap();
    std::fs::write(p.work.path().join("notes/a.txt"), "written in the pod\n").unwrap();

    let listed: Value =
        p.h.client
            .get(p.url("dir?path=notes"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
    assert!(
        listed.to_string().contains("a.txt"),
        "the pod's file is listed: {listed}"
    );

    let read =
        p.h.client
            .get(p.url("files?path=notes/a.txt"))
            .send()
            .await
            .unwrap();
    assert_eq!(read.status(), StatusCode::OK);
    let body: Value = read.json().await.unwrap();
    assert_eq!(body["content"], "written in the pod\n", "{body}");
    assert_eq!(body["size"], 19, "{body}");
    p.finish().await;
}

#[tokio::test]
async fn a_file_the_engine_uploads_is_at_the_same_path_in_the_pod() {
    let p = pod_files(|_| {}).await;

    // A JSON write …
    let written =
        p.h.client
            .put(p.url("files"))
            .json(&json!({ "path": "out/w.txt", "content": "via write", "create_parents": true }))
            .send()
            .await
            .unwrap();
    assert_eq!(
        written.status(),
        StatusCode::OK,
        "{:?}",
        written.text().await
    );
    assert_eq!(
        std::fs::read_to_string(p.work.path().join("out/w.txt")).unwrap(),
        "via write"
    );

    // … and a streaming upload.
    let payload: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
    let uploaded =
        p.h.client
            .put(p.url("files/upload?path=up/blob.bin&create_parents=true"))
            .body(payload.clone())
            .send()
            .await
            .unwrap();
    assert_eq!(
        uploaded.status(),
        StatusCode::OK,
        "{:?}",
        uploaded.text().await
    );
    assert_eq!(
        std::fs::read(p.work.path().join("up/blob.bin")).unwrap(),
        payload,
        "the pod reads exactly the bytes the engine uploaded"
    );
    p.finish().await;
}

#[tokio::test]
async fn a_token_for_another_session_is_refused_on_read_and_write() {
    // Resolves to `agent:session:ses_…`, a session token that holds the
    // interactive capabilities but belongs to no session of this engine.
    let core = stand_in_core_knowing(vec![(
        "other-session-token-1234567890",
        "agent:session:ses_00000000000000000000000009",
        vec!["work.write"],
    )])
    .await;
    let p = pod_files(|cfg| cfg.vogt_core_url = Some(core)).await;
    std::fs::write(p.work.path().join("plain.txt"), "mine").unwrap();
    let other = client_for("other-session-token-1234567890");

    let read = other
        .get(p.url("files?path=plain.txt"))
        .send()
        .await
        .unwrap();
    assert_eq!(read.status(), StatusCode::FORBIDDEN);
    let listed = other.get(p.url("dir")).send().await.unwrap();
    assert_eq!(listed.status(), StatusCode::FORBIDDEN);

    let write = other
        .put(p.url("files"))
        .json(&json!({ "path": "evil.txt", "content": "x" }))
        .send()
        .await
        .unwrap();
    assert_eq!(write.status(), StatusCode::FORBIDDEN);
    let upload = other
        .put(p.url("files/upload?path=evil2.txt"))
        .body("x")
        .send()
        .await
        .unwrap();
    assert_eq!(upload.status(), StatusCode::FORBIDDEN);

    assert!(!p.work.path().join("evil.txt").exists());
    assert!(!p.work.path().join("evil2.txt").exists());
    // The refusal is about the token, not the session: the engine's own
    // token still reads it.
    let ok =
        p.h.client
            .get(p.url("files?path=plain.txt"))
            .send()
            .await
            .unwrap();
    let status = ok.status();
    assert_eq!(status, StatusCode::OK, "{:?}", ok.text().await);
    p.finish().await;
}

#[tokio::test]
async fn a_detached_agent_is_a_409_session_hibernated() {
    let p = pod_files(|_| {}).await;
    let ok = p.h.client.get(p.url("dir")).send().await.unwrap();
    assert_eq!(ok.status(), StatusCode::OK);

    // The agent goes away: dropping the task drops the stream.
    p.agent.abort();
    let mut last = None;
    for _ in 0..100 {
        let res = p.h.client.get(p.url("dir")).send().await.unwrap();
        if res.status() == StatusCode::CONFLICT {
            last = Some(res.json::<Value>().await.unwrap());
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let body = last.expect("a detached agent becomes a 409");
    assert_eq!(body["code"], "session_hibernated", "{body}");
    for (method, tail) in [("GET", "files?path=a"), ("PUT", "files")] {
        let req = match method {
            "GET" => p.h.client.get(p.url(tail)),
            _ => {
                p.h.client
                    .put(p.url(tail))
                    .json(&json!({ "path": "a", "content": "x" }))
            }
        };
        let res = req.send().await.unwrap();
        assert_eq!(res.status(), StatusCode::CONFLICT, "{method} {tail}");
        let body: Value = res.json().await.unwrap();
        assert_eq!(body["code"], "session_hibernated", "{body}");
    }
}

#[tokio::test]
async fn a_hibernated_session_is_a_409_session_hibernated() {
    let (mut h, _accounts) = claude_harness().await;
    let id = h.start().await;
    let conversation = conversation_of(&h, id).await;
    let token = h.session_token(id).await;
    let mut agent = h.dial(id, &token, 0).await;
    assert_eq!(take_skip_resize(&mut agent).await, Frame::Ack { offset: 0 });
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
    assert!(h.state.sessions.is_hibernated(id));

    for res in [
        h.client
            .get(format!("{}/api/sessions/{id}/files?path=a", h.base))
            .send()
            .await
            .unwrap(),
        h.client
            .put(format!("{}/api/sessions/{id}/files", h.base))
            .json(&json!({ "path": "a", "content": "x" }))
            .send()
            .await
            .unwrap(),
    ] {
        assert_eq!(res.status(), StatusCode::CONFLICT);
        let body: Value = res.json().await.unwrap();
        assert_eq!(body["code"], "session_hibernated", "{body}");
        assert!(body["hint"].as_str().unwrap().contains("wake"), "{body}");
    }
}
