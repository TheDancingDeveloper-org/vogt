use super::*;

#[tokio::test]
async fn cold_attach_tail_hint_caps_the_snapshot() {
    // A cold attach (no resume_from) that sends a tail hint must get a
    // full reset snapshot bounded to at most that many bytes, not the entire
    // scrollback ring.
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let id: String = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({ "name": "tail", "command": ["/bin/cat"] }))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    const TAIL: usize = 512;
    fill_scrollback(&base, &id, 4 * TAIL).await;

    // Cold reattach with a small tail hint: a reset snapshot, capped.
    let mut cold = ws_attach_with_auth(
        &base,
        &id,
        json!({ "type": "auth", "token": TEST_TOKEN, "snapshot_tail_bytes": TAIL }),
    )
    .await;
    let (reset, len, _head) = read_snapshot(&mut cold).await;
    assert!(reset, "a cold attach is always a full reset");
    assert!(
        len > 0 && len <= TAIL,
        "cold snapshot should be capped at the tail hint ({TAIL}); got {len}"
    );
    cold.close(None).await.ok();
    kill_session(&client, &base, &id).await;
}

#[tokio::test]
async fn warm_attach_within_budget_stays_a_byte_exact_delta() {
    // The companion to the bounded-reset case: a warm reattach whose delta fits
    // the budget is still sent byte-for-byte with reset:false, so an ordinary
    // switch-away/switch-back appends without clearing the terminal.
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let id: String = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "warm-small",
            "command": ["/bin/sh", "-c", "stty -echo; exec /bin/cat"]
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

    // Prime the ring, record the cursor, then produce a small delta.
    let mut ws = ws_attach(&base, &id).await;
    read_snapshot(&mut ws).await;
    ws.send(Message::Binary(b"before-cursor\n".to_vec().into()))
        .await
        .unwrap();
    let _ = collect_binary_until(&mut ws, b"before-cursor", Duration::from_secs(2)).await;
    // The bytes arriving on the socket is not the ring having absorbed them:
    // the reader commits the scrollback after the broadcast, so a cursor read
    // in that gap still points before "before-cursor" and the resume then
    // includes it (seen failing in CI on an otherwise unrelated change). Wait
    // until the recorded position has moved past the priming output.
    let cursor = {
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            let detail: Value = client
                .get(format!("{base}/api/sessions/{id}"))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            let pos = detail["scrollback_pos"].as_u64().unwrap();
            if pos > 0 || std::time::Instant::now() >= deadline {
                break pos;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };
    assert!(
        cursor > 0,
        "the priming output never reached the scrollback"
    );
    ws.send(Message::Binary(b"after-cursor\n".to_vec().into()))
        .await
        .unwrap();
    let _ = collect_binary_until(&mut ws, b"after-cursor", Duration::from_secs(2)).await;
    ws.close(None).await.ok();

    // A generous budget the tiny delta fits inside: the resume stays a delta.
    let mut warm = ws_attach_with_auth(
        &base,
        &id,
        json!({
            "type": "auth",
            "token": TEST_TOKEN,
            "resume_from": cursor,
            "snapshot_tail_bytes": 1024 * 1024,
        }),
    )
    .await;
    let (reset, delta) = read_snapshot_payload(&mut warm).await;
    assert!(!reset, "a delta within budget must not reset the terminal");
    assert!(
        delta
            .windows(b"after-cursor".len())
            .any(|w| w == b"after-cursor"),
        "the delta must carry the post-cursor output"
    );
    assert!(
        !delta
            .windows(b"before-cursor".len())
            .any(|w| w == b"before-cursor"),
        "the delta must exclude pre-cursor output (byte-exact from the cursor)"
    );
    warm.close(None).await.ok();
    kill_session(&client, &base, &id).await;
}

#[tokio::test]
async fn retained_cursor_deltas_are_byte_exact_under_load() {
    // H1(b) — byte-exactness half. Two warm reattaches from retained cursors
    // Ca < Cb into the same idle 4 MiB ring: the later delta must equal the
    // earlier delta with its first (Cb-Ca) bytes removed, and its SEQ spine
    // must be contiguous. Proves `snapshot_since` is byte-exact against bytes
    // that really flowed through the PTY reader and broadcast path, with no gap
    // or duplicate at the resume boundary. This assertion survives F1.
    const CAP: usize = 4 * 1024 * 1024;
    let (base, _h) = boot_with(CAP).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let id = create_command_session(&client, &base, "delta", load_session_command(80_000)).await;
    // Let it settle so both cursors resolve against the same stable ring.
    let _ = poll_scrollback_at_least(&client, &base, &id, CAP as u64 + 256 * 1024).await;
    let total = wait_until_scrollback_stable(&client, &base, &id).await;

    // Both cursors sit inside the retained 4 MiB window.
    let ca = total - 2 * 1024 * 1024;
    let cb = ca + 512 * 1024;

    let mut a = ws_attach_with_token_and_cursor(&base, &id, TEST_TOKEN, Some(ca)).await;
    let (reset_a, delta_a) = read_snapshot_payload(&mut a).await;
    let mut b = ws_attach_with_token_and_cursor(&base, &id, TEST_TOKEN, Some(cb)).await;
    let (reset_b, delta_b) = read_snapshot_payload(&mut b).await;

    assert!(!reset_a && !reset_b, "retained cursors resume as deltas");
    let skip = (cb - ca) as usize;
    assert!(
        delta_a.len() >= skip && delta_a.len() == skip + delta_b.len(),
        "delta lengths inconsistent: |A|={} skip={} |B|={}",
        delta_a.len(),
        skip,
        delta_b.len()
    );
    assert_eq!(
        &delta_a[skip..],
        &delta_b[..],
        "the later delta must equal the earlier delta past the (Cb-Ca) boundary"
    );
    assert_seq_spine_contiguous(&delta_b);

    a.close(None).await.ok();
    b.close(None).await.ok();
    kill_session(&client, &base, &id).await;
}

#[tokio::test]
async fn pong_never_reports_the_server_ahead_of_this_socket() {
    // F2 (WI-126). Under load the liveness pong must carry the position actually
    // streamed to THIS socket, never `total_written` (which includes queued but
    // unsent output). Before the fix the inbound task answered with
    // scrollback_position() and the pong could be delivered ahead of the chunks
    // it referenced, so the client saw the server "ahead" and recycled the
    // socket. Now the outbound task answers with sent_pos after flushing, so the
    // pong's pos is never greater than what this socket has received.
    let (base, _h) = boot_with(4 * 1024 * 1024).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let id = create_command_session(&client, &base, "pong", load_session_command(50_000)).await;

    let mut ws = ws_attach(&base, &id).await;

    // Drain the initial snapshot, recording the absolute position it ended at.
    let mut snap_end: u64 = 0;
    let mut in_snapshot = false;
    loop {
        let m = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("snapshot frame arrives")
            .unwrap()
            .unwrap();
        match m {
            Message::Text(s) => {
                let v: Value = serde_json::from_str(&s).unwrap();
                match v["type"].as_str() {
                    Some("snapshot-start") => {
                        in_snapshot = true;
                        snap_end = v["scrollback_pos"].as_u64().unwrap();
                    }
                    Some("snapshot-done") => break,
                    _ => {}
                }
            }
            Message::Binary(_) if in_snapshot => {}
            _ => {}
        }
    }

    // Probe while output is still flowing.
    ws.send(Message::Text(
        json!({ "type": "ping", "id": 1 }).to_string().into(),
    ))
    .await
    .unwrap();

    // Count live bytes received on this socket until the pong arrives. The pong
    // is ordered after any flushed chunks, so by the time we read it we have
    // received every byte up to its position.
    let mut live: u64 = 0;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let m = tokio::time::timeout(remaining, ws.next())
            .await
            .expect("a pong should arrive within the deadline")
            .unwrap()
            .unwrap();
        match m {
            Message::Binary(b) => live += b.len() as u64,
            Message::Text(s) => {
                let v: Value = serde_json::from_str(&s).unwrap();
                if v["type"] == "pong" {
                    assert_eq!(v["id"].as_u64(), Some(1));
                    let pos = v["pos"].as_u64().unwrap();
                    let received = snap_end + live;
                    assert!(
                        pos <= received,
                        "pong pos {pos} is ahead of what this socket received \
                         ({received} = snap_end {snap_end} + live {live})"
                    );
                    break;
                }
                // A resync snapshot-start would only add to `received`; keep going.
            }
            _ => {}
        }
    }

    ws.close(None).await.ok();
    kill_session(&client, &base, &id).await;
}

#[tokio::test]
async fn ws_attach_echoes_input_and_replays_on_reattach() {
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    // `cat` echoes whatever we send it on stdin to stdout.
    let id: String = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({ "name": "echo", "command": ["/bin/cat"] }))
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

    // 1) snapshot-start text frame
    let m = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .expect("snapshot-start arrives")
        .unwrap()
        .unwrap();
    let s = match m {
        Message::Text(s) => s,
        other => panic!("expected text snapshot-start, got {other:?}"),
    };
    let v: Value = serde_json::from_str(&s).unwrap();
    assert_eq!(v["type"], "snapshot-start");

    // 2) snapshot-done. A live session's cold attach sends the grid's frame
    // (WI-121), so binary frames may precede it even before any input.
    loop {
        let m = tokio::time::timeout(Duration::from_secs(2), ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        if let Message::Text(s) = m {
            if serde_json::from_str::<Value>(&s).unwrap()["type"] == "snapshot-done" {
                break;
            }
        }
    }

    // 3) Write input → expect to see it echoed back.
    ws.send(Message::Binary(b"hello-mydevenv\n".to_vec().into()))
        .await
        .unwrap();

    let echoed = collect_binary_until(&mut ws, b"hello-mydevenv", Duration::from_secs(2)).await;
    assert!(
        echoed
            .windows(b"hello-mydevenv".len())
            .any(|w| w == b"hello-mydevenv"),
        "echo not seen; got {:?}",
        String::from_utf8_lossy(&echoed)
    );

    // Close first client.
    ws.close(None).await.ok();
    drop(ws);

    // 4) Reattach — snapshot must contain what we just echoed.
    let mut ws2 = ws_attach(&base, &id).await;
    let _start = ws2.next().await.unwrap().unwrap();
    let mut accumulated = Vec::new();
    loop {
        let m = tokio::time::timeout(Duration::from_secs(2), ws2.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        match m {
            Message::Binary(b) => accumulated.extend_from_slice(&b),
            Message::Text(s) => {
                let v: Value = serde_json::from_str(&s).unwrap();
                if v["type"] == "snapshot-done" {
                    break;
                }
            }
            _ => {}
        }
    }
    assert!(
        accumulated
            .windows(b"hello-mydevenv".len())
            .any(|w| w == b"hello-mydevenv"),
        "scrollback replay missing previous output; got {:?}",
        String::from_utf8_lossy(&accumulated)
    );

    let _ = client
        .delete(format!("{base}/api/sessions/{id}"))
        .send()
        .await;
}

#[tokio::test]
async fn ws_ping_returns_the_session_output_position() {
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let id: String = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "ping",
            "command": ["/bin/sh", "-c", "stty -echo; exec /bin/cat"]
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

    let mut ws = ws_attach(&base, &id).await;
    while let Some(Ok(message)) = ws.next().await {
        if let Message::Text(text) = message {
            if serde_json::from_str::<Value>(&text).unwrap()["type"] == "snapshot-done" {
                break;
            }
        }
    }

    ws.send(Message::Binary(b"ping-output\n".to_vec().into()))
        .await
        .unwrap();
    let _ = collect_binary_until(&mut ws, b"ping-output", Duration::from_secs(2)).await;
    let detail: Value = client
        .get(format!("{base}/api/sessions/{id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let expected_pos = detail["scrollback_pos"].as_u64().unwrap();

    ws.send(Message::Text(
        json!({"type": "ping", "id": 19}).to_string().into(),
    ))
    .await
    .unwrap();
    let pong = loop {
        let message = tokio::time::timeout(Duration::from_secs(2), ws.next())
            .await
            .expect("pong arrives")
            .unwrap()
            .unwrap();
        if let Message::Text(text) = message {
            let value: Value = serde_json::from_str(&text).unwrap();
            if value["type"] == "pong" {
                break value;
            }
        }
    };
    assert_eq!(pong["id"], 19);
    assert_eq!(pong["pos"], expected_pos);

    let _ = client
        .delete(format!("{base}/api/sessions/{id}"))
        .send()
        .await;
}

#[tokio::test]
async fn ws_reattach_replays_only_output_after_a_valid_cursor() {
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let id: String = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "resume",
            "command": ["/bin/sh", "-c", "stty -echo; exec /bin/cat"]
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

    let mut ws = ws_attach(&base, &id).await;
    while let Some(Ok(message)) = ws.next().await {
        if let Message::Text(text) = message {
            if serde_json::from_str::<Value>(&text).unwrap()["type"] == "snapshot-done" {
                break;
            }
        }
    }
    ws.send(Message::Binary(b"before-cursor\n".to_vec().into()))
        .await
        .unwrap();
    let _ = collect_binary_until(&mut ws, b"before-cursor", Duration::from_secs(2)).await;

    let detail: Value = client
        .get(format!("{base}/api/sessions/{id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let cursor = detail["scrollback_pos"].as_u64().unwrap();

    ws.send(Message::Binary(b"after-cursor\n".to_vec().into()))
        .await
        .unwrap();
    let _ = collect_binary_until(&mut ws, b"after-cursor", Duration::from_secs(2)).await;
    ws.close(None).await.ok();

    let mut resumed = ws_attach_with_token_and_cursor(&base, &id, TEST_TOKEN, Some(cursor)).await;
    let start = resumed.next().await.unwrap().unwrap();
    let start = match start {
        Message::Text(text) => serde_json::from_str::<Value>(&text).unwrap(),
        other => panic!("expected snapshot-start, got {other:?}"),
    };
    assert_eq!(start["type"], "snapshot-start");
    assert_eq!(start["reset"], false);

    let mut delta = Vec::new();
    loop {
        match resumed.next().await.unwrap().unwrap() {
            Message::Binary(bytes) => delta.extend_from_slice(&bytes),
            Message::Text(text)
                if serde_json::from_str::<Value>(&text).unwrap()["type"] == "snapshot-done" =>
            {
                break;
            }
            _ => {}
        }
    }
    assert!(delta
        .windows(b"after-cursor".len())
        .any(|w| w == b"after-cursor"));
    assert!(!delta
        .windows(b"before-cursor".len())
        .any(|w| w == b"before-cursor"));

    let _ = client
        .delete(format!("{base}/api/sessions/{id}"))
        .send()
        .await;
}

#[tokio::test]
async fn ws_rejects_oversized_input_without_closing_the_session() {
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let id: String = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "bounded-input",
            "command": ["/bin/sh", "-c", "stty -echo; exec /bin/cat"]
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

    let mut ws = ws_attach(&base, &id).await;
    while let Some(Ok(message)) = ws.next().await {
        if let Message::Text(text) = message {
            if serde_json::from_str::<Value>(&text).unwrap()["type"] == "snapshot-done" {
                break;
            }
        }
    }

    ws.send(Message::Binary(vec![b'x'; 64 * 1024 + 1].into()))
        .await
        .unwrap();
    ws.send(Message::Binary(b"still-responsive\n".to_vec().into()))
        .await
        .unwrap();
    let output = collect_binary_until(&mut ws, b"still-responsive", Duration::from_secs(2)).await;
    assert!(
        output
            .windows(b"still-responsive".len())
            .any(|window| window == b"still-responsive"),
        "normal input was not accepted after oversized frame"
    );
    assert!(
        !output
            .windows(128)
            .any(|window| window.iter().all(|byte| *byte == b'x')),
        "oversized input reached the PTY"
    );

    let _ = client
        .delete(format!("{base}/api/sessions/{id}"))
        .send()
        .await;
}

/// WI-920: a client that falls behind the event stream is told so in band,
/// the stream keeps going, and the lag is on record in `/api/status`.
#[tokio::test]
async fn a_lagging_event_stream_is_told_and_keeps_going() {
    let (base, state, _h) = boot_with_state(test_config()).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let mut response = client
        .get(format!("{base}/api/events"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    // Large events, published while nothing reads the response: the socket
    // fills, the stream stops being polled, and its receiver falls more than
    // the bus's capacity behind.
    let padding = "x".repeat(16 * 1024);
    for seq in 0..2000 {
        state.bus.publish(ServerEvent::VogtChanged {
            kind: "test.burst".into(),
            entity_kind: "work_item".into(),
            entity_id: "WI-1".into(),
            seq,
            summary: json!({ "padding": padding }),
        });
    }
    let mut seen = String::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while !seen.contains("\"type\":\"lagged\"") {
        let chunk = tokio::time::timeout_at(deadline, response.chunk())
            .await
            .expect("the lagged notice never arrived")
            .unwrap()
            .expect("the stream ended");
        // Keep only a tail: the burst is tens of megabytes.
        seen.push_str(&String::from_utf8_lossy(&chunk));
        if seen.len() > 64 * 1024 {
            seen = seen[seen.len() - 64 * 1024..].to_string();
        }
    }
    // The stream is still alive after the lag.
    state.bus.publish(ServerEvent::SessionRenamed {
        id: uuid::Uuid::nil(),
        name: "after the lag".into(),
    });
    let mut after = String::new();
    while !after.contains("after the lag") {
        let chunk = tokio::time::timeout_at(deadline, response.chunk())
            .await
            .expect("nothing arrived after the lag")
            .unwrap()
            .expect("the stream ended");
        after.push_str(&String::from_utf8_lossy(&chunk));
        if after.len() > 64 * 1024 {
            after = after[after.len() - 64 * 1024..].to_string();
        }
    }
    let status: Value = client
        .get(format!("{base}/api/status"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        status["event_lag"]["sse-events"]["events_skipped"]
            .as_u64()
            .unwrap()
            > 0,
        "{status}"
    );
}

#[tokio::test]
async fn every_attached_client_is_told_the_pty_size() {
    // WI-1089: two clients on one PTY with different pane sizes. The snapshot
    // names the size it is drawn at, and when one client resizes the PTY the
    // other is told, so it renders the stream at the program's width instead
    // of its own (which wraps a diff-painting TUI into stacked ghost frames).
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let id: String = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({ "name": "sizes", "command": ["/bin/cat"] }))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let mut desk = ws_attach(&base, &id).await;
    let start = next_control(&mut desk, "snapshot-start").await;
    assert!(
        start["cols"].as_u64().is_some(),
        "snapshot-start carries cols: {start}"
    );
    assert!(
        start["rows"].as_u64().is_some(),
        "snapshot-start carries rows: {start}"
    );
    next_control(&mut desk, "snapshot-done").await;

    let mut phone = ws_attach(&base, &id).await;
    next_control(&mut phone, "snapshot-done").await;

    desk.send(Message::Text(
        json!({ "type": "resize", "cols": 178, "rows": 32 })
            .to_string()
            .into(),
    ))
    .await
    .unwrap();
    let told = next_control(&mut phone, "resize").await;
    assert_eq!(
        (told["cols"].as_u64(), told["rows"].as_u64()),
        (Some(178), Some(32))
    );
    let echoed = next_control(&mut desk, "resize").await;
    assert_eq!(echoed["cols"].as_u64(), Some(178));

    phone
        .send(Message::Text(
            json!({ "type": "resize", "cols": 66, "rows": 40 })
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    let told = next_control(&mut desk, "resize").await;
    assert_eq!(
        (told["cols"].as_u64(), told["rows"].as_u64()),
        (Some(66), Some(40))
    );

    // A later attach is handed the size its snapshot was drawn at.
    let mut late = ws_attach(&base, &id).await;
    let start = next_control(&mut late, "snapshot-start").await;
    assert_eq!(
        (start["cols"].as_u64(), start["rows"].as_u64()),
        (Some(66), Some(40))
    );

    desk.close(None).await.ok();
    phone.close(None).await.ok();
    late.close(None).await.ok();
    kill_session(&client, &base, &id).await;
}

#[tokio::test]
async fn a_live_session_accepts_input_resize_and_stop_without_blocking() {
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let id = create_session_with(
        &client,
        &base,
        json!({ "name": "backend-lock", "command": ["/bin/cat"] }),
    )
    .await;
    let mut ws = ws_attach(&base, &id).await;
    next_control(&mut ws, "snapshot-done").await;

    tokio::time::timeout(Duration::from_secs(2), async {
        ws.send(Message::Binary(b"live\n".to_vec().into()))
            .await
            .unwrap();
        ws.send(Message::Text(
            json!({ "type": "resize", "cols": 120, "rows": 40 })
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
        next_control(&mut ws, "resize").await;
        client
            .post(format!("{base}/api/sessions/{id}/kill"))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
        wait_for_session_row(&client, &base, &id, |s| !s["exit_code"].is_null()).await;
    })
    .await
    .expect("input, resize and stop remain responsive while the child exits");
    ws.close(None).await.ok();
}

/// WI-4068: vogt-core sends `text`, then each named key as its own `/input`
/// write, so `keys: ["enter"]` arrives here as `text: "\r"` and
/// `keys: ["tab"]` as `text: "\t"`. A program that asked for bracketed paste
/// must still receive those as keys, not as pasted newline/tab text; only
/// text that is actually pasted (and the Enter after or appended to it) is
/// wrapped. A wrapper around a key would reach the PTY literally, which
/// `cat -v` shows.
#[tokio::test]
async fn session_input_keys_reach_a_bracketed_paste_program_as_keys() {
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let id = create_session_with(
        &client,
        &base,
        json!({
            "name": "paste-mode-cat",
            "command": [
                "/bin/sh",
                "-c",
                // Turn bracketed paste on before `cat`, then say so, so the
                // first input below is sent while the mode is live.
                "printf '\\033[?2004h'; printf 'ready\\n'; exec cat -v",
            ],
        }),
    )
    .await;
    live_output_containing(&client, &base, &id, &["ready"]).await;
    // The scrollback is appended just before the grid parses the same bytes,
    // and the grid is where bracketed paste is tracked. Give the parse that
    // follows "ready" a moment to land before relying on the mode.
    tokio::time::sleep(Duration::from_millis(250)).await;

    // `keys: ["enter"]` is one `text: "\r"` write. It must reach the PTY as
    // a key: the terminal echo shows a pasted marker as `^[[200~`.
    let before = session_scrollback(&client, &base, &id).await;
    client
        .post(format!("{base}/api/sessions/{id}/input"))
        .json(&json!({ "text": "\r" }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let printed = wait_for_scrollback_growth(&client, &base, &id, before.len()).await;
    let typed = &printed[before.len()..];
    assert!(
        typed.contains('\n'),
        "Enter did not reach the PTY: {typed:?}"
    );
    assert!(
        !typed.contains("^[[200~") && !typed.contains("^[[201~"),
        "Enter was written as pasted text: {typed:?}"
    );

    // `keys: ["tab"]` is one `text: "\t"` write, for completion menus.
    let before = session_scrollback(&client, &base, &id).await;
    client
        .post(format!("{base}/api/sessions/{id}/input"))
        .json(&json!({ "text": "\t" }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let printed = wait_for_scrollback_growth(&client, &base, &id, before.len()).await;
    let typed = &printed[before.len()..];
    assert!(typed.contains('\t'), "Tab did not reach the PTY: {typed:?}");
    assert!(
        !typed.contains("^[[200~") && !typed.contains("^[[201~"),
        "Tab was written as pasted text: {typed:?}"
    );

    // The text+Enter shape is still one paste (WI-4068), so Codex's paste
    // burst window ends before the Enter that follows it.
    let before = session_scrollback(&client, &base, &id).await;
    client
        .post(format!("{base}/api/sessions/{id}/input"))
        .json(&json!({ "text": "hello", "submit": true }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let printed = wait_for_scrollback_growth(&client, &base, &id, before.len()).await;
    let typed = &printed[before.len()..];
    assert!(
        typed.contains("^[[200~hello^[[201~"),
        "the text was not one paste: {typed:?}"
    );
    assert!(
        typed.contains('\n'),
        "the Enter after the paste did not reach the PTY: {typed:?}"
    );
    kill_session(&client, &base, &id).await;
}

#[tokio::test]
async fn an_omitted_placement_follows_the_template_backend() {
    let mut h = pod_stream::harness(false).await;
    let id = h.start().await;
    let (_created, _token) = h.created.recv().await.expect("the pod is created");
    h.state.sessions.remove(id).unwrap();
}

/// WI-1314 (a): input after the report, then a full turn, clears it; a turn
/// of unrelated output with no input does not.
#[tokio::test]
async fn a_blocked_report_clears_after_input_and_the_turn_it_starts() {
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let go = dir.path().join("go");
    let id = create_session_with(
        &client,
        &base,
        json!({
            "name": "blocked-input",
            "command": turn_script(&format!(
                "while [ ! -e {go} ]; do sleep 0.05; done; \
                 printf 'unrelated build output\\r\\n'; \
                 read l; printf 'pushed after %s\\r\\n' \"$l\"",
                go = go.display()
            )),
        }),
    )
    .await;
    wait_for_activity(&client, &base, &id, RESTING).await;
    report_blocked(&client, &base, &id).await;
    assert_eq!(
        blocked_report(&client, &base, &id).await["reason"],
        "push refused"
    );

    // Unrelated output: a turn runs and ends, nobody addressed the block.
    std::fs::write(&go, b"").unwrap();
    live_output_containing(&client, &base, &id, &["unrelated build output"]).await;
    wait_for_activity(&client, &base, &id, RESTING).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        blocked_report(&client, &base, &id).await["reason"],
        "push refused",
        "output nobody asked for must not clear a block"
    );

    // Input, then the turn it starts: cleared.
    let resp = client
        .post(format!("{base}/api/sessions/{id}/input"))
        .json(&json!({ "text": "approved", "submit": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    live_output_containing(&client, &base, &id, &["pushed after approved"]).await;
    wait_for_unblocked(&client, &base, &id).await;
}

/// WI-1314: the engine's own writes (autopilot nudge, budget warning,
/// hibernate resume, task steering) go through `write_input` and do not
/// address a block; only `note_addressed` (the API and a person's keys) do.
#[tokio::test]
async fn engine_input_does_not_clear_a_blocked_report() {
    let (base, state, _h) = boot_with_state(test_config()).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let id = create_session_with(
        &client,
        &base,
        json!({
            "name": "blocked-nudged",
            "command": turn_script("read l; printf 'got %s\\r\\n' \"$l\""),
        }),
    )
    .await;
    wait_for_activity(&client, &base, &id, RESTING).await;
    report_blocked(&client, &base, &id).await;
    assert_eq!(
        blocked_report(&client, &base, &id).await["reason"],
        "push refused"
    );
    let session = state.sessions.get(id.parse().unwrap()).unwrap();
    session
        .write_input(vogt_engine_server::autopilot::NUDGE.as_bytes())
        .unwrap();
    session.write_input(b"\r").unwrap();
    live_output_containing(&client, &base, &id, &["got Autopilot"]).await;
    wait_for_activity(&client, &base, &id, RESTING).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        blocked_report(&client, &base, &id).await["reason"],
        "push refused",
        "the engine's own input is not a person addressing the block"
    );
}

/// WI-1314: input typed while the reporting turn still runs waits for a
/// turn of its own; the end of the turn it arrived in does not clear.
#[tokio::test]
async fn input_during_the_reporting_turn_stays_blocked_until_its_own_turn() {
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let go = dir.path().join("go");
    let id = create_session_with(
        &client,
        &base,
        json!({
            "name": "blocked-early-input",
            "command": turn_script(&format!(
                "while [ ! -e {go} ]; do printf .; sleep 0.05; done; \
                 printf 'reporting turn over\\r\\n'; read l; sleep 1; \
                 printf 'acted on %s\\r\\n' \"$l\"",
                go = go.display()
            )),
        }),
    )
    .await;
    wait_for_activity(&client, &base, &id, &["running"]).await;
    report_blocked(&client, &base, &id).await;
    assert_eq!(
        blocked_report(&client, &base, &id).await["reason"],
        "push refused"
    );
    let resp = client
        .post(format!("{base}/api/sessions/{id}/input"))
        .json(&json!({ "text": "fixed", "submit": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    std::fs::write(&go, b"").unwrap();
    live_output_containing(&client, &base, &id, &["reporting turn over"]).await;
    wait_for_activity(&client, &base, &id, RESTING).await;
    assert_eq!(
        blocked_report(&client, &base, &id).await["reason"],
        "push refused",
        "the turn the input arrived in is not the turn that acted on it"
    );
    live_output_containing(&client, &base, &id, &["acted on fixed"]).await;
    wait_for_unblocked(&client, &base, &id).await;
}

/// WI-1314 (c): answering the dialog a report was about clears it when the
/// resumed turn ends.
#[tokio::test]
async fn answering_the_dialog_clears_a_blocked_report() {
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let gate = dir.path().join("gate.py");
    std::fs::write(
        &gate,
        r#"import sys, tty, os, time
opts = ["Yes, I trust this folder", "No, exit"]
sel = 0
def draw():
    sys.stdout.write("\x1b[2J\x1b[H")
    sys.stdout.write("Quick safety check: Is this a project you created or one you trust?\r\n\r\n")
    for i, o in enumerate(opts):
        sys.stdout.write(("❯ " if i == sel else "  ") + f"{i+1}. {o}\r\n")
    sys.stdout.flush()
fd = sys.stdin.fileno()
tty.setraw(fd)
draw()
buf = b""
while True:
    buf += os.read(fd, 16)
    while buf:
        if buf.startswith(b"\x1b[B"):
            sel = min(sel + 1, len(opts) - 1); buf = buf[3:]; draw()
        elif buf.startswith(b"\x1b[A"):
            sel = max(sel - 1, 0); buf = buf[3:]; draw()
        elif buf.startswith(b"\r"):
            sys.stdout.write("\x1b[2J\x1b[H" + f"chose {sel+1}\r\n> ")
            sys.stdout.flush()
            time.sleep(60)
        elif buf.startswith(b"\x1b") and len(buf) < 3:
            break
        else:
            buf = buf[1:]
"#,
    )
    .unwrap();
    let id = create_session_with(
        &client,
        &base,
        json!({ "name": "blocked-dialog", "command": ["python3", gate.to_string_lossy()] }),
    )
    .await;
    wait_for_activity(&client, &base, &id, &["awaiting-approval"]).await;
    report_blocked(&client, &base, &id).await;
    assert_eq!(
        blocked_report(&client, &base, &id).await["reason"],
        "push refused"
    );
    let answered: Value = client
        .post(format!("{base}/api/sessions/{id}/answer"))
        .json(&json!({ "option": 1 }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(answered["dismissed"], true, "{answered}");
    live_output_containing(&client, &base, &id, &["chose 1"]).await;
    wait_for_unblocked(&client, &base, &id).await;
}
