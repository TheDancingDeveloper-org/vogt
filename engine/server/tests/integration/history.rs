use super::*;

#[tokio::test]
async fn exited_sessions_are_archived_searchable_and_deletable() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = test_config();
    cfg.default_cwd = tmp.path().to_path_buf();
    cfg.workspace_root = tmp.path().canonicalize().unwrap();

    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let id: String = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "archive-me",
            "command": [
                "/bin/sh",
                "-lc",
                "printf '<img src=x onerror=globalThis.__vogt_xss=1> <svg onload=1> <broken history-needle path/with &lt;script&gt;alert(1)&lt;/script&gt; punctuation\\n'; exit 7",
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
        .to_string();

    // Wait for the *finalized* row, not merely the provisional one written at
    // spawn. The provisional row appears first with a NULL exit code;
    // the exit waiter later upserts the real outcome. Requiring exit_code == 7
    // in the predicate settles that race deterministically.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let archived = loop {
        let sessions: Vec<Value> = client
            .get(format!("{base}/api/history/sessions?limit=20"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if let Some(session) = sessions
            .iter()
            .find(|s| s["id"] == id && s["exit_code"] == json!(7))
        {
            break session.clone();
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "session was not archived with its final exit code; got {sessions:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };

    assert_eq!(archived["name"], "archive-me");
    assert_eq!(archived["exit_code"], 7);
    assert!(
        archived["scrollback_bytes"].as_i64().unwrap_or_default() > 0,
        "archive should record output bytes: {archived:?}"
    );

    // The session record appearing in /history/sessions does not imply its
    // output is searchable: archival and indexing complete independently, so
    // the poll above settles only the first of them. A bare read here passes
    // on an idle machine and loses under load — a CI pipeline failed exactly
    // this way while the commit under test touched only .dockerignore. Same
    // deadline discipline as the loop above.
    let search_deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let hits: Vec<Value> = client
            .get(format!(
                "{base}/api/history/search?q=history-needle%20path%2Fwith"
            ))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if hits.iter().any(|hit| hit["session_id"] == id) {
            let hit = hits
                .iter()
                .find(|hit| hit["session_id"] == id)
                .expect("matching history result");
            let snippet = hit["match_snippet"].as_str().unwrap_or_default();
            assert!(
                !snippet.contains("<mark>"),
                "history API must return plain text snippets: {hit:?}"
            );
            assert!(
                snippet.contains("<img src=x onerror=globalThis.__vogt_xss=1>"),
                "history API must preserve hostile output as data: {hit:?}"
            );
            assert!(
                snippet.contains("<svg onload=1>"),
                "history API must preserve SVG payloads as data: {hit:?}"
            );
            assert!(
                snippet.contains("<broken"),
                "history API must preserve malformed markup as data: {hit:?}"
            );
            assert!(
                snippet.contains("&lt;script&gt;alert(1)&lt;/script&gt;"),
                "history API must preserve encoded payloads as data: {hit:?}"
            );
            break;
        }
        assert!(
            tokio::time::Instant::now() < search_deadline,
            "history search should find archived output; got {hits:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let del = client
        .delete(format!("{base}/api/history/{id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(del.status(), StatusCode::OK);

    let after_delete = client
        .get(format!("{base}/api/history/{id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(after_delete.status(), StatusCode::NOT_FOUND);
}

/// A long-lived session that never runs `exit` is still archived when the
/// engine shuts down, via the graceful-shutdown drain — it appears in history
/// with `ended_at` set and no one had to type `exit`.
#[tokio::test]
async fn shutdown_drain_archives_a_live_session_that_never_exited() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = test_config();
    cfg.default_cwd = tmp.path().to_path_buf();
    cfg.workspace_root = tmp.path().canonicalize().unwrap();

    let (base, state, _h) = boot_with_state(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    // `exec cat` never exits on its own: the child blocks forever on stdin,
    // exactly like a long-lived agent shell on prod.
    let id: String = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "never-exits",
            "command": ["/bin/sh", "-lc", "printf 'drain-me\\n'; exec cat"],
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

    // Give the reader thread a moment to flush the banner into the raw log so
    // the drain has output to archive.
    tokio::time::sleep(Duration::from_millis(200)).await;

    // The graceful-shutdown drain, invoked directly (the drain is what the
    // SIGTERM handler in `serve_forever` calls). The child is still running.
    state.sessions.drain_to_history().await;

    let sessions: Vec<Value> = client
        .get(format!("{base}/api/history/sessions?limit=20"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let archived = sessions
        .iter()
        .find(|s| s["id"] == id)
        .unwrap_or_else(|| panic!("live session was not archived by drain; got {sessions:?}"));
    assert_eq!(archived["name"], "never-exits");
    assert!(
        archived["ended_at"].is_string(),
        "drained session must have ended_at set: {archived:?}"
    );
    // The child never exited, so its outcome is unknown: exit_code stays NULL,
    // which is what the `unfinished` history filter selects.
    assert!(
        archived["exit_code"].is_null(),
        "a drained-but-unexited session has an unknown exit code: {archived:?}"
    );

    // Reap the never-exiting child so the exit waiter's blocking `child.wait()`
    // returns; otherwise it pins the test runtime open past the end of the test.
    let _ = client
        .post(format!("{base}/api/sessions/{id}/kill"))
        .send()
        .await;
}

/// A provisional row exists at spawn with a NULL exit code, and a later
/// provisional (re)write never clobbers the real outcome once the session has
/// finalized on exit.
#[tokio::test]
async fn provisional_history_row_is_written_at_spawn_and_not_clobbered() {
    use vogt_engine_server::history::ArchiveRecord;

    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = test_config();
    cfg.default_cwd = tmp.path().to_path_buf();
    cfg.workspace_root = tmp.path().canonicalize().unwrap();

    let (base, state, _h) = boot_with_state(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    // Stays alive for ~2s, giving a wide window to observe the provisional row
    // before the exit waiter finalizes it with exit code 5.
    let id: String = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "provisional",
            "command": ["/bin/sh", "-lc", "printf 'ready\\n'; sleep 2; exit 5"],
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

    // The provisional row is written (asynchronously) at spawn: poll for it and
    // assert it is present with no exit code and no end time.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    let provisional = loop {
        let resp = client
            .get(format!("{base}/api/history/{id}"))
            .send()
            .await
            .unwrap();
        if resp.status() == StatusCode::OK {
            let row: Value = resp.json().await.unwrap();
            if row["exit_code"].is_null() {
                break row;
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "provisional history row never appeared before finalize"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert!(
        provisional["ended_at"].is_null(),
        "a provisional row has no end time yet: {provisional:?}"
    );

    // After the child exits, the exit waiter upserts the real outcome over the
    // provisional NULLs.
    let finalize_deadline = tokio::time::Instant::now() + Duration::from_secs(6);
    loop {
        let row: Value = client
            .get(format!("{base}/api/history/{id}"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if row["exit_code"] == json!(5) {
            assert!(
                row["ended_at"].is_string(),
                "finalized row must have ended_at: {row:?}"
            );
            break;
        }
        assert!(
            tokio::time::Instant::now() < finalize_deadline,
            "session never finalized with its real exit code"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // A late provisional (re)write — the race the COALESCE guard defends
    // against — must not NULL the finalized outcome.
    let history = state.history.as_ref().expect("history enabled");
    history
        .archive_session(ArchiveRecord {
            id: uuid::Uuid::parse_str(&id).unwrap(),
            name: "provisional".to_string(),
            created_at: OffsetDateTime::now_utc(),
            ended_at: None,
            exit_code: None,
            cwd: None,
            command: None,
            scrollback_bytes: 0,
            end_reason: None,
            identity: None,
            economics: None,
        })
        .await
        .unwrap();

    let after: Value = client
        .get(format!("{base}/api/history/{id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        after["exit_code"],
        json!(5),
        "a late provisional write must not clobber a completed exit code: {after:?}"
    );
    assert!(
        after["ended_at"].is_string(),
        "a late provisional write must not clobber ended_at: {after:?}"
    );
}

/// A raw session log that predates any index row (survived a hard
/// restart) is recovered on startup with a NULL exit code and made searchable,
/// and an already-indexed session is never duplicated.
#[tokio::test]
async fn startup_backfill_indexes_orphaned_logs_without_duplicating() {
    use vogt_engine_server::history::SessionHistory;

    let dir = tempfile::tempdir().unwrap();
    let history = SessionHistory::new(dir.path()).await.unwrap();

    // An orphaned raw log with no matching row, as a prod redeploy would leave.
    let orphan = uuid::Uuid::new_v4();
    std::fs::write(
        history.log_dir().join(format!("{orphan}.log")),
        b"orphan-backfill-needle in the transcript\n",
    )
    .unwrap();

    let recovered = history.backfill_orphaned_logs().await.unwrap();
    assert_eq!(recovered, 1, "the single orphaned log should be recovered");

    let row = history.get_session(orphan).await.unwrap();
    assert!(
        row.exit_code.is_none(),
        "a recovered log has an unknown outcome (NULL exit code)"
    );
    assert!(
        row.scrollback_bytes > 0,
        "the recovered row records the log size"
    );

    // The recovered transcript is searchable.
    let hits = history.search("orphan-backfill-needle", 10).await.unwrap();
    assert!(
        hits.iter().any(|h| h.session_id == orphan.to_string()),
        "recovered output should be searchable; got {hits:?}"
    );

    // Running again is idempotent: nothing new, and no duplicate row.
    let again = history.backfill_orphaned_logs().await.unwrap();
    assert_eq!(again, 0, "a second backfill recovers nothing");
    assert_eq!(history.count_sessions().await.unwrap(), 1);
}

#[tokio::test]
async fn archived_history_log_preview_and_download_work() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = test_config();
    cfg.default_cwd = tmp.path().to_path_buf();
    cfg.workspace_root = tmp.path().canonicalize().unwrap();

    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let id: String = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "replay-me",
            "command": [
                "/bin/sh",
                "-lc",
                "printf 'first line\\nWIN_SSH_KEY=abcd1234\\nsecond line\\nthird line\\n'; exit 0",
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
        .to_string();

    // Wait for the finalized row, not the provisional one written at spawn
    // Only after the exit waiter finalizes (non-NULL exit_code) is the
    // reader thread guaranteed to have flushed the full transcript to the raw
    // log, so the tail preview below can rely on "third line" being present.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let session = client
            .get(format!("{base}/api/history/{id}"))
            .send()
            .await
            .unwrap();
        if session.status() == StatusCode::OK {
            let row: Value = session.json().await.unwrap();
            if !row["exit_code"].is_null() {
                break;
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "session was not archived in time"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let preview: Value = client
        .get(format!("{base}/api/history/{id}/log?tail_bytes=12"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(preview["session_id"], id);
    assert_eq!(preview["truncated"], true);
    assert_eq!(preview["bytes"], 12);
    assert!(
        preview["text"]
            .as_str()
            .unwrap_or("")
            .contains("third line"),
        "preview should contain tail output: {preview:?}"
    );

    // WI-3988: the log tail is a transcript; a secret printed into it is
    // masked before it is served.
    let preview: Value = client
        .get(format!("{base}/api/history/{id}/log?tail_bytes=4096"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let text = preview["text"].as_str().unwrap_or("");
    assert!(!text.contains("abcd1234"), "a printed key survived: {text}");
    assert!(text.contains("[redacted]"), "{text}");

    let download = client
        .get(format!("{base}/api/history/{id}/download"))
        .send()
        .await
        .unwrap();
    assert_eq!(download.status(), StatusCode::OK);
    let content_disposition = download
        .headers()
        .get(reqwest::header::CONTENT_DISPOSITION)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    assert!(
        content_disposition.contains("attachment"),
        "download should set attachment disposition: {content_disposition}"
    );
    let body = download.text().await.unwrap();
    assert!(body.contains("first line"));
    assert!(body.contains("third line"));
}

#[tokio::test]
async fn archived_history_cleanup_removes_old_sessions_and_logs() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = test_config();
    cfg.default_cwd = tmp.path().to_path_buf();
    cfg.workspace_root = tmp.path().canonicalize().unwrap();
    cfg.state_dir = tmp.path().join("state");

    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let id: String = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "cleanup-me",
            "command": ["/bin/sh", "-lc", "printf 'cleanup-history\\n'; exit 0"],
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

    // Wait for the finalized row, not the provisional one written at spawn
    // If cleanup deleted the provisional row before the exit waiter
    // finalized, the finalize's upsert would re-insert it and the 404 assert
    // below would race. Requiring a non-NULL exit_code means no further write
    // is pending when cleanup runs.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let session = client
            .get(format!("{base}/api/history/{id}"))
            .send()
            .await
            .unwrap();
        if session.status() == StatusCode::OK {
            let row: Value = session.json().await.unwrap();
            if !row["exit_code"].is_null() {
                break;
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "session was not archived in time"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let cleanup: Value = client
        .post(format!("{base}/api/history/cleanup"))
        .json(&json!({ "retention_days": 0 }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(cleanup["ok"], true);
    assert_eq!(cleanup["removed_sessions"], 1);

    let after = client
        .get(format!("{base}/api/history/{id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(after.status(), StatusCode::NOT_FOUND);
}

/// The log-preview endpoint strips ANSI on request, so an agent reading
/// over MCP gets plain text; the default preserves the raw escape stream for
/// callers that render it themselves.
#[tokio::test]
async fn history_log_preview_strips_ansi_when_requested() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = test_config();
    cfg.default_cwd = tmp.path().to_path_buf();
    cfg.workspace_root = tmp.path().canonicalize().unwrap();

    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let id: String = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "ansi-me",
            "command": [
                "/bin/sh",
                "-lc",
                "printf '\\033[31mred-needle\\033[0m plain-tail\\n'; exit 0",
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
        .to_string();

    // Wait for the finalized row so the raw log is fully flushed.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let session = client
            .get(format!("{base}/api/history/{id}"))
            .send()
            .await
            .unwrap();
        if session.status() == StatusCode::OK {
            let row: Value = session.json().await.unwrap();
            if !row["exit_code"].is_null() {
                break;
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "session was not archived in time"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Default: raw stream, escapes preserved.
    let raw: Value = client
        .get(format!("{base}/api/history/{id}/log"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let raw_text = raw["text"].as_str().unwrap_or_default();
    assert!(
        raw_text.contains('\u{1b}'),
        "default preview must keep the raw escape stream: {raw:?}"
    );

    // strip_ansi=true: readable plain text, no escape bytes.
    let stripped: Value = client
        .get(format!("{base}/api/history/{id}/log?strip_ansi=true"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let text = stripped["text"].as_str().unwrap_or_default();
    assert!(
        text.contains("red-needle") && text.contains("plain-tail"),
        "stripped preview must keep the visible text: {stripped:?}"
    );
    assert!(
        !text.contains('\u{1b}'),
        "stripped preview must contain no escape bytes: {stripped:?}"
    );
}

/// WI-877: a permission dialog on screen reads `awaiting-approval`, with the
/// command it asks about (even the part above the screen) and its countdown,
/// on the screen, the list and the event; `ready` is false. WI-875: turn
/// timing is reported. The screen can carry scrollback on request.
#[tokio::test]
async fn a_permission_dialog_reads_awaiting_approval_with_its_command() {
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    // Ten rows: the dialog's title and the first line of the command scroll
    // off the top, the question and its menu stay on screen.
    let script = "printf 'earlier output\\r\\n────────────\\r\\nBash command\\r\\n\\r\\n  rm -rf /tmp/vogt-approval-test &&\\r\\n  docker rm -f approval-test\\r\\n  clean up\\r\\n\\r\\n'; \
                  for i in 1 2 3 4 5; do printf 'context line %s\\r\\n' $i; done; \
                  printf 'Do you want to proceed?\\r\\n❯ 1. Yes\\r\\n  2. No\\r\\nClaude will automatically deny this request in 90s'; sleep 30";
    let id = create_session_with(
        &client,
        &base,
        json!({
            "name": "approval",
            "cols": 60,
            "rows": 10,
            "command": ["/bin/sh", "-c", script],
        }),
    )
    .await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let screen = loop {
        let screen: Value = client
            .get(format!(
                "{base}/api/sessions/{id}/screen?scrollback_lines=50"
            ))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if screen["activity"] == "awaiting-approval" {
            break screen;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "never awaiting-approval: {screen:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(
        screen["ready"], false,
        "a dialog is not a prompt to type at"
    );
    let approval = &screen["approval"];
    assert_eq!(approval["question"], "Do you want to proceed?");
    let excerpt = approval["command_excerpt"].as_str().unwrap();
    assert!(
        excerpt.contains("rm -rf /tmp/vogt-approval-test"),
        "the command above the screen is read from the scrollback: {excerpt}"
    );
    assert!(excerpt.contains("docker rm -f approval-test"), "{excerpt}");
    assert!(!excerpt.contains("earlier output"), "{excerpt}");
    let secs = approval["deadline_seconds"].as_u64().unwrap();
    assert!((80..=90).contains(&secs), "{approval:?}");
    assert!(approval["deadline_at"].is_string(), "{approval:?}");
    assert!(screen["turn_started_at"].is_string(), "{screen:?}");
    assert!(screen["last_output_at"].is_string(), "{screen:?}");
    let scrollback = screen["scrollback"].as_array().unwrap();
    assert!(
        scrollback.iter().any(|l| l == "earlier output"),
        "{scrollback:?}"
    );

    // The list says the same.
    let list: Value = client
        .get(format!("{base}/api/sessions"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let list: Vec<Value> = serde_json::from_value(list["sessions"].clone()).unwrap();
    let row = list.iter().find(|s| s["id"] == id).unwrap();
    assert_eq!(row["activity"], "awaiting-approval");
    assert_eq!(row["approval"]["question"], "Do you want to proceed?");

    // An over-large scrollback request is refused, not truncated silently.
    let too_many = client
        .get(format!(
            "{base}/api/sessions/{id}/screen?scrollback_lines=5000"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(too_many.status(), StatusCode::BAD_REQUEST);

    client
        .delete(format!("{base}/api/sessions/{id}"))
        .send()
        .await
        .unwrap();
}
