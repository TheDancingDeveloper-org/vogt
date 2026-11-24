use super::*;

#[tokio::test]
async fn readyz_fails_when_workspace_root_disappears() {
    let workspace = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let mut cfg = test_config();
    cfg.default_cwd = workspace.path().to_path_buf();
    cfg.workspace_root = workspace.path().to_path_buf();
    cfg.state_dir = state_dir.path().to_path_buf();

    let (base, _h) = boot_with_config(cfg).await;
    workspace.close().unwrap();

    let res = reqwest::get(format!("{base}/readyz")).await.unwrap();
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["ok"], false);
    let checks = body["checks"].as_array().expect("missing checks");
    let workspace_check = checks
        .iter()
        .find(|check| check["name"] == "workspace_root")
        .expect("missing workspace_root check");
    assert_eq!(workspace_check["ok"], false);
}

#[tokio::test]
async fn session_cwd_must_stay_under_workspace_root() {
    let tmp = tempfile::tempdir().unwrap();
    let workspace = tmp.path().join("workspace");
    let nested = workspace.join("nested");
    std::fs::create_dir_all(&nested).unwrap();
    let outside = tmp.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();

    let mut cfg = test_config();
    cfg.default_cwd = workspace.clone();
    cfg.workspace_root = workspace.canonicalize().unwrap();

    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let create: Value = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "cwd-test",
            "command": ["/bin/cat"],
            "cwd": nested.canonicalize().unwrap().to_string_lossy().into_owned()
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        create["cwd"],
        nested.canonicalize().unwrap().to_string_lossy().as_ref()
    );
    let id = create["id"].as_str().unwrap();
    let _ = client
        .delete(format!("{base}/api/sessions/{id}"))
        .send()
        .await;

    let rejected = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "bad-cwd",
            "command": ["/bin/cat"],
            "cwd": outside.canonicalize().unwrap().to_string_lossy().into_owned()
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(rejected.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn file_api_round_trip() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("hello.txt"), "first").unwrap();
    std::fs::create_dir(tmp.path().join("sub")).unwrap();
    std::fs::write(tmp.path().join("sub/nested.md"), "# nested").unwrap();

    let cfg = Config {
        default_cwd: tmp.path().to_path_buf(),
        workspace_root: tmp.path().canonicalize().unwrap(),
        ..test_config()
    };
    let (router, _state) = router(cfg).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    // List root
    let dir: Vec<Value> = client
        .get(format!("{base}/api/dir?path="))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let names: Vec<&str> = dir.iter().filter_map(|e| e["name"].as_str()).collect();
    assert!(names.contains(&"hello.txt"));
    assert!(names.contains(&"sub"));

    // Read existing file
    let r: Value = client
        .get(format!("{base}/api/files?path=hello.txt"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(r["content"], "first");

    // Write a new file
    let w = client
        .put(format!("{base}/api/files"))
        .json(&json!({ "path": "new.txt", "content": "fresh" }))
        .send()
        .await
        .unwrap();
    assert_eq!(w.status(), StatusCode::OK);
    let bytes_on_disk = std::fs::read_to_string(tmp.path().join("new.txt")).unwrap();
    assert_eq!(bytes_on_disk, "fresh");

    // Create a directory via higher-level file ops.
    let mkdir = client
        .post(format!("{base}/api/files/op"))
        .json(&json!({ "op": "mkdir", "path": "ops/deeper", "parents": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(mkdir.status(), StatusCode::OK);
    assert!(tmp.path().join("ops/deeper").is_dir());

    // Upload binary bytes via content_base64 (native client upload path).
    use base64::Engine as _;
    let raw: &[u8] = &[0x00, 0x01, 0xff, 0xfe, b'h', b'i'];
    let b64 = base64::engine::general_purpose::STANDARD.encode(raw);
    let wb = client
        .put(format!("{base}/api/files"))
        .json(&json!({ "path": "up/bin.dat", "content_base64": b64, "create_parents": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(wb.status(), StatusCode::OK);
    let on_disk = std::fs::read(tmp.path().join("up/bin.dat")).unwrap();
    assert_eq!(on_disk, raw);

    // Duplicate both a file and a directory tree.
    let dup_file = client
        .post(format!("{base}/api/files/op"))
        .json(&json!({
            "op": "duplicate",
            "from": "hello.txt",
            "to": "ops/hello-copy.txt",
            "create_parents": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(dup_file.status(), StatusCode::OK);
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("ops/hello-copy.txt")).unwrap(),
        "first"
    );

    let dup_dir = client
        .post(format!("{base}/api/files/op"))
        .json(&json!({
            "op": "duplicate",
            "from": "sub",
            "to": "sub-copy"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(dup_dir.status(), StatusCode::OK);
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("sub-copy/nested.md")).unwrap(),
        "# nested"
    );

    // Move/rename a file.
    let mv = client
        .post(format!("{base}/api/files/op"))
        .json(&json!({
            "op": "move",
            "from": "new.txt",
            "to": "ops/renamed.txt",
            "create_parents": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(mv.status(), StatusCode::OK);
    assert!(!tmp.path().join("new.txt").exists());
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("ops/renamed.txt")).unwrap(),
        "fresh"
    );

    // Delete a copied file and directory tree.
    let del_file = client
        .post(format!("{base}/api/files/op"))
        .json(&json!({ "op": "delete", "path": "ops/hello-copy.txt" }))
        .send()
        .await
        .unwrap();
    assert_eq!(del_file.status(), StatusCode::OK);
    assert!(!tmp.path().join("ops/hello-copy.txt").exists());

    let del_dir = client
        .post(format!("{base}/api/files/op"))
        .json(&json!({ "op": "delete", "path": "sub-copy", "recursive": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(del_dir.status(), StatusCode::OK);
    assert!(!tmp.path().join("sub-copy").exists());

    // A malformed base64 body is a 400, not a 500.
    let bad = client
        .put(format!("{base}/api/files"))
        .json(&json!({ "path": "bad.dat", "content_base64": "!!!notbase64!!!" }))
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), StatusCode::BAD_REQUEST);

    // Path-traversal rejected
    let escape = client
        .get(format!("{base}/api/files?path=../escape"))
        .send()
        .await
        .unwrap();
    assert_eq!(escape.status(), StatusCode::BAD_REQUEST);

    // Tree with depth 1 includes nested.md
    let tree: Vec<Value> = client
        .get(format!("{base}/api/tree?depth=1"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let sub = tree
        .iter()
        .find(|n| n["name"] == "sub")
        .expect("sub node present");
    let kids: Vec<&str> = sub["children"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|c| c["name"].as_str())
        .collect();
    assert!(kids.contains(&"nested.md"));

    // Search (skip if rg isn't installed; just don't fail the suite)
    let s = client
        .get(format!("{base}/api/search?q=nested"))
        .send()
        .await
        .unwrap();
    if s.status() == StatusCode::OK {
        let hits: Vec<Value> = s.json().await.unwrap();
        assert!(
            hits.iter().any(|h| h["path"]
                .as_str()
                .map(|p| p.ends_with("nested.md"))
                .unwrap_or(false)),
            "rg search should find nested; got {hits:?}"
        );
    }
}

#[tokio::test]
async fn file_upload_streams_to_disk() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = Config {
        default_cwd: tmp.path().to_path_buf(),
        workspace_root: tmp.path().canonicalize().unwrap(),
        ..test_config()
    };
    let (router, _state) = router(cfg).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    // A payload comfortably larger than the 2 MiB JSON default, to prove the
    // streaming route carries what the buffered one would reject.
    let payload: Vec<u8> = (0..(5 * 1024 * 1024u32)).map(|i| (i % 251) as u8).collect();

    let r = client
        .put(format!(
            "{base}/api/files/upload?path=up/big.bin&create_parents=true"
        ))
        .body(payload.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let body: Value = r.json().await.unwrap();
    assert_eq!(body["bytes"].as_u64().unwrap(), payload.len() as u64);

    // Bytes landed exactly, and the returned hash is the content SHA-256.
    let on_disk = std::fs::read(tmp.path().join("up/big.bin")).unwrap();
    assert_eq!(on_disk, payload);
    let mut hasher = <sha2::Sha256 as sha2::Digest>::new();
    sha2::Digest::update(&mut hasher, &payload);
    let expect_hash = sha2::Digest::finalize(hasher)
        .iter()
        .fold(String::new(), |mut s, b| {
            use std::fmt::Write as _;
            let _ = write!(s, "{b:02x}");
            s
        });
    assert_eq!(body["hash"].as_str().unwrap(), expect_hash);

    // No spooled temp file is left behind in the target directory.
    let leftovers: Vec<String> = std::fs::read_dir(tmp.path().join("up"))
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        leftovers,
        vec!["big.bin".to_string()],
        "temp file leaked: {leftovers:?}"
    );

    // if_match guards a streaming overwrite just like the JSON write does: a
    // stale baseline is a 409, not a clobber.
    let stale = client
        .put(format!(
            "{base}/api/files/upload?path=up/big.bin&if_match=deadbeef"
        ))
        .body(vec![b'x'; 16])
        .send()
        .await
        .unwrap();
    assert_eq!(stale.status(), StatusCode::CONFLICT);
    assert_eq!(
        std::fs::read(tmp.path().join("up/big.bin")).unwrap(),
        payload
    );
}

#[tokio::test]
async fn read_returns_hash_and_mtime_and_if_match_guards_writes() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("note.txt"), "original").unwrap();

    let cfg = Config {
        default_cwd: tmp.path().to_path_buf(),
        workspace_root: tmp.path().canonicalize().unwrap(),
        ..test_config()
    };
    let (router, _state) = router(cfg).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    // Read exposes a content hash and an mtime.
    let r: Value = client
        .get(format!("{base}/api/files?path=note.txt"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(r["content"], "original");
    let hash = r["hash"].as_str().expect("read returns a hash").to_string();
    assert_eq!(hash.len(), 64, "sha-256 hex is 64 chars");
    assert!(r["mtime"].as_u64().unwrap() > 0, "read returns an mtime");

    // Happy path: writing with the matching if_match succeeds and hands back a
    // fresh baseline hash/mtime.
    let w = client
        .put(format!("{base}/api/files"))
        .json(&json!({ "path": "note.txt", "content": "edited by me", "if_match": hash }))
        .send()
        .await
        .unwrap();
    assert_eq!(w.status(), StatusCode::OK);
    let wbody: Value = w.json().await.unwrap();
    let new_hash = wbody["hash"].as_str().expect("write returns new hash");
    assert_ne!(new_hash, hash, "baseline advanced after the write");
    assert!(wbody["mtime"].as_u64().unwrap() > 0);
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("note.txt")).unwrap(),
        "edited by me"
    );

    // Simulate an external change on disk, then attempt to save against the now
    // stale baseline: the write is refused with 409 Conflict and the file is
    // left untouched.
    std::fs::write(tmp.path().join("note.txt"), "changed underfoot").unwrap();
    let stale = client
        .put(format!("{base}/api/files"))
        .json(&json!({ "path": "note.txt", "content": "my clobber", "if_match": new_hash }))
        .send()
        .await
        .unwrap();
    assert_eq!(stale.status(), StatusCode::CONFLICT);
    let body: Value = stale.json().await.unwrap();
    assert!(
        body["error"].as_str().unwrap().contains("changed on disk"),
        "conflict body should explain the staleness; got {body:?}"
    );
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("note.txt")).unwrap(),
        "changed underfoot",
        "a conflicting write must not clobber the newer on-disk content"
    );

    // Without if_match the write still wins unconditionally (legacy behaviour).
    let force = client
        .put(format!("{base}/api/files"))
        .json(&json!({ "path": "note.txt", "content": "forced" }))
        .send()
        .await
        .unwrap();
    assert_eq!(force.status(), StatusCode::OK);
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("note.txt")).unwrap(),
        "forced"
    );
}

#[tokio::test]
async fn file_name_search_returns_matching_paths() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir(tmp.path().join("docs")).unwrap();
    std::fs::write(tmp.path().join("README.md"), "root").unwrap();
    std::fs::write(tmp.path().join("docs").join("readme-notes.txt"), "nested").unwrap();
    std::fs::write(tmp.path().join("docs").join("other.txt"), "other").unwrap();

    let mut cfg = test_config();
    cfg.default_cwd = tmp.path().to_path_buf();
    cfg.workspace_root = tmp.path().canonicalize().unwrap();

    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let hits: Vec<Value> = client
        .get(format!("{base}/api/search/files?q=read"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert!(hits.iter().any(|hit| hit["path"] == "README.md"));
    assert!(hits
        .iter()
        .any(|hit| hit["path"] == "docs/readme-notes.txt"));
}

#[tokio::test]
async fn git_status_log_branch() {
    // Spin up a fresh git repo in a tempdir as the workspace, then drive
    // the git API across status, diff, staging, commit, and branch workflow.
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
    std::fs::write(repo.join("a.txt"), "one\n").unwrap();
    sh("git add a.txt && git commit -q -m 'init'");
    std::fs::write(repo.join("a.txt"), "one\ntwo\n").unwrap();
    std::fs::write(repo.join("b.txt"), "untracked\n").unwrap();

    let cfg = Config {
        default_cwd: repo.to_path_buf(),
        workspace_root: repo.canonicalize().unwrap(),
        ..test_config()
    };
    let (router, _state) = router(cfg).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let st: Value = client
        .get(format!("{base}/api/git/status"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(st["is_repo"], true);
    assert_eq!(st["branch"], "main");
    let entries = st["entries"].as_array().unwrap();
    let paths: Vec<&str> = entries.iter().filter_map(|e| e["path"].as_str()).collect();
    assert!(paths.contains(&"a.txt"));
    assert!(paths.contains(&"b.txt"));

    let log: Vec<Value> = client
        .get(format!("{base}/api/git/log?n=10"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(log.len(), 1);
    assert_eq!(log[0]["subject"], "init");

    let br: Value = client
        .get(format!("{base}/api/git/branch"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(br["current"], "main");

    let diff: Value = client
        .get(format!("{base}/api/git/diff?path=a.txt"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(diff["head"], "one\n");
    assert_eq!(diff["current"], "one\ntwo\n");

    let stage_a: Value = client
        .post(format!("{base}/api/git/op"))
        .json(&json!({
            "op": "stage",
            "path": "a.txt",
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(stage_a["ok"], true);

    let staged_status: Value = client
        .get(format!("{base}/api/git/status"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let staged_a = staged_status["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["path"] == "a.txt")
        .unwrap();
    assert_eq!(staged_a["kind"], "staged");
    assert_eq!(staged_a["index"], "M");
    assert_eq!(staged_a["worktree"], " ");

    let unstage_a: Value = client
        .post(format!("{base}/api/git/op"))
        .json(&json!({
            "op": "unstage",
            "path": "a.txt",
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(unstage_a["ok"], true);

    let discard_a: Value = client
        .post(format!("{base}/api/git/op"))
        .json(&json!({
            "op": "discard",
            "path": "a.txt",
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(discard_a["ok"], true);
    assert_eq!(
        std::fs::read_to_string(repo.join("a.txt")).unwrap(),
        "one\n"
    );

    let stage_b: Value = client
        .post(format!("{base}/api/git/op"))
        .json(&json!({
            "op": "stage",
            "path": "b.txt",
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(stage_b["ok"], true);

    let unstage_b: Value = client
        .post(format!("{base}/api/git/op"))
        .json(&json!({
            "op": "unstage",
            "path": "b.txt",
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(unstage_b["ok"], true);

    let discard_b: Value = client
        .post(format!("{base}/api/git/op"))
        .json(&json!({
            "op": "discard",
            "path": "b.txt",
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(discard_b["ok"], true);
    assert!(!repo.join("b.txt").exists());

    std::fs::write(repo.join("b.txt"), "tracked now\n").unwrap();
    let stage_commit_target: Value = client
        .post(format!("{base}/api/git/op"))
        .json(&json!({
            "op": "stage",
            "path": "b.txt",
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(stage_commit_target["ok"], true);

    let commit: Value = client
        .post(format!("{base}/api/git/op"))
        .json(&json!({
            "op": "commit",
            "message": "add b",
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(commit["ok"], true);
    assert!(commit["commit"].as_str().unwrap().len() >= 7);

    let clean_status: Value = client
        .get(format!("{base}/api/git/status"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(clean_status["entries"].as_array().unwrap().is_empty());

    let updated_log: Vec<Value> = client
        .get(format!("{base}/api/git/log?n=10"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(updated_log.len(), 2);
    assert_eq!(updated_log[0]["subject"], "add b");

    let create_branch: Value = client
        .post(format!("{base}/api/git/op"))
        .json(&json!({
            "op": "checkout",
            "branch": "feature/git-workflow",
            "create": true,
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(create_branch["ok"], true);
    assert_eq!(create_branch["branch"], "feature/git-workflow");

    let branch_after_create: Value = client
        .get(format!("{base}/api/git/branch"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(branch_after_create["current"], "feature/git-workflow");
    let all_branches = branch_after_create["all"].as_array().unwrap();
    assert!(all_branches
        .iter()
        .any(|branch| branch.as_str() == Some("feature/git-workflow")));

    let checkout_main: Value = client
        .post(format!("{base}/api/git/op"))
        .json(&json!({
            "op": "checkout",
            "branch": "main",
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(checkout_main["ok"], true);
    assert_eq!(checkout_main["branch"], "main");

    let branch_after_checkout: Value = client
        .get(format!("{base}/api/git/branch"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(branch_after_checkout["current"], "main");
}

#[tokio::test]
async fn git_endpoints_return_empty_for_non_repo_workspace() {
    let tmp = tempfile::tempdir().unwrap();
    // Reproduce the deployed workspace shape from vogt#53: an empty `.git`
    // placeholder is not a repository and must not make status return 500.
    std::fs::create_dir(tmp.path().join(".git")).unwrap();
    std::fs::create_dir(tmp.path().join("plain-dir")).unwrap();

    let mut cfg = test_config();
    cfg.default_cwd = tmp.path().to_path_buf();
    cfg.workspace_root = tmp.path().canonicalize().unwrap();

    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let response = client
        .get(format!("{base}/api/git/status"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let st: Value = response.json().await.unwrap();
    assert_eq!(st["is_repo"], false);
    assert_eq!(st["branch"], "");
    assert!(st["entries"].as_array().unwrap().is_empty());

    let nested_status: Value = client
        .get(format!("{base}/api/git/status?repo=plain-dir"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(nested_status["repo"], "plain-dir");
    assert_eq!(nested_status["is_repo"], false);
    assert!(nested_status["entries"].as_array().unwrap().is_empty());

    let nested_log: Vec<Value> = client
        .get(format!("{base}/api/git/log?repo=plain-dir&n=10"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(nested_log.is_empty());

    let nested_branch: Value = client
        .get(format!("{base}/api/git/branch?repo=plain-dir"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(nested_branch["current"], "");
    assert!(nested_branch["all"].as_array().unwrap().is_empty());

    let log: Vec<Value> = client
        .get(format!("{base}/api/git/log?n=10"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(log.is_empty());

    let br: Value = client
        .get(format!("{base}/api/git/branch"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(br["current"], "");
    assert!(br["all"].as_array().unwrap().is_empty());

    // Read endpoints deliberately render an empty non-repository state.
    // Mutation must still refuse: returning success here would claim a Git
    // effect happened in an ordinary directory.
    let operation = client
        .post(format!("{base}/api/git/operate"))
        .json(&serde_json::json!({
            "op": "stage",
            "repo": "plain-dir",
            "path": "nothing.txt"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(operation.status(), StatusCode::NOT_FOUND);
}

/// WI-1020: reading the workspace — the viewer, the download, a listing, the
/// tree, both searches and every git read — needs the `sessions` capability.
/// A `read`-only device token and a zero-scope credential are refused before
/// the handler runs; a writer and the stack secret still read.
#[tokio::test]
async fn workspace_reads_need_the_sessions_capability() {
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
    std::fs::write(repo.join("ok.txt"), "needle-ok\n").unwrap();
    sh("git add ok.txt && git commit -q -m init");
    std::fs::write(repo.join("ok.txt"), "needle-ok\nmore\n").unwrap();

    let core = stand_in_core_knowing(vec![
        (
            "reader-token-1234567890abcdef",
            "human:reader",
            vec!["read"],
        ),
        ("noscope-token-1234567890abcdef", "human:nobody", vec![]),
        (
            "writer-token-1234567890abcdef",
            "human:writer",
            vec!["read", "work.write"],
        ),
    ])
    .await;
    let mut cfg = test_config();
    cfg.default_cwd = repo.to_path_buf();
    cfg.workspace_root = repo.canonicalize().unwrap();
    cfg.vogt_core_url = Some(core);
    let (base, _h) = boot_with_config(cfg).await;

    let routes = [
        "files?path=ok.txt",
        "files/download?path=ok.txt",
        "dir",
        "tree?depth=1",
        "search?q=needle",
        "search/files?q=ok",
        "git/status",
        "git/diff?path=ok.txt",
        "git/log?n=5",
        "git/branch",
    ];
    let status = |token: &'static str, route: &'static str| {
        let url = format!("{base}/api/{route}");
        async move {
            reqwest::Client::new()
                .get(url)
                .headers(auth_for(token))
                .send()
                .await
                .unwrap()
                .status()
        }
    };
    for route in routes {
        for token in [
            "reader-token-1234567890abcdef",
            "noscope-token-1234567890abcdef",
        ] {
            assert_eq!(
                status(token, route).await,
                StatusCode::FORBIDDEN,
                "{token} must not read /api/{route}"
            );
        }
        for token in ["writer-token-1234567890abcdef", TEST_TOKEN] {
            assert_eq!(
                status(token, route).await,
                StatusCode::OK,
                "{token} must still read /api/{route}"
            );
        }
    }
}

/// The streaming upload needs `filesystem-write`, like every other write of
/// the tree: a read-only token is refused and nothing lands on disk, while a
/// writer's upload succeeds.
#[tokio::test]
async fn streaming_upload_needs_filesystem_write() {
    let tmp = tempfile::tempdir().unwrap();
    let core = stand_in_core_knowing(vec![
        (
            "reader-token-1234567890abcdef",
            "human:reader",
            vec!["read"],
        ),
        (
            "writer-token-1234567890abcdef",
            "human:writer",
            vec!["read", "work.write"],
        ),
    ])
    .await;
    let mut cfg = test_config();
    cfg.default_cwd = tmp.path().to_path_buf();
    cfg.workspace_root = tmp.path().canonicalize().unwrap();
    cfg.vogt_core_url = Some(core);
    let (base, _h) = boot_with_config(cfg).await;

    let upload = |token: &'static str, name: &'static str| {
        let url = format!("{base}/api/files/upload?path={name}");
        async move {
            reqwest::Client::new()
                .put(url)
                .headers(auth_for(token))
                .body("uploaded\n")
                .send()
                .await
                .unwrap()
                .status()
        }
    };
    assert_eq!(
        upload("reader-token-1234567890abcdef", "reader.txt").await,
        StatusCode::FORBIDDEN
    );
    assert!(!tmp.path().join("reader.txt").exists());
    assert_eq!(
        upload("writer-token-1234567890abcdef", "writer.txt").await,
        StatusCode::OK
    );
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("writer.txt")).unwrap(),
        "uploaded\n"
    );
}

#[tokio::test]
async fn a_refused_worktree_session_leaves_no_worktree_or_branch() {
    let (tmp, mut cfg, stub) = hibernation_sandbox();
    let repo = tmp.path().join("repo");
    worktree_checkout(&repo);
    let codex = tmp.path().join("bin").join("codex");
    std::fs::copy(&stub, &codex).unwrap();
    cfg.agent_onboarding.codex_policy =
        Err("Codex driven-session policy /no/such/requirements.toml cannot be read".into());
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let refused = |name: &str, body: Value| {
        let client = client.clone();
        let base = base.clone();
        let name = name.to_string();
        async move {
            let res = client
                .post(format!("{base}/api/sessions"))
                .json(&body)
                .send()
                .await
                .unwrap();
            let status = res.status();
            let text = res.text().await.unwrap();
            assert!(
                status.is_client_error() || status.is_server_error(),
                "{name}: {status} {text}"
            );
        }
    };
    let worktree = |branch: &str| json!({ "branch": branch, "base_ref": "main" });
    let cases = [
        // The codex policy refuses the launch.
        (
            "wt-codex",
            json!({ "name": "c", "command": [codex.to_string_lossy()],
                    "cwd": repo.to_string_lossy(), "worktree": worktree("wt-codex") }),
        ),
        // The agent CLI refuses the posture.
        (
            "wt-launch",
            json!({ "name": "l", "command": [stub.to_string_lossy()],
                    "permission_mode": "nonsense",
                    "cwd": repo.to_string_lossy(), "worktree": worktree("wt-launch") }),
        ),
        // Resume plus worktree is refused outright.
        (
            "wt-resume",
            json!({ "name": "r", "command": [stub.to_string_lossy()],
                    "resume": "00000000-0000-4000-8000-000000000000",
                    "cwd": repo.to_string_lossy(), "worktree": worktree("wt-resume") }),
        ),
        // The PTY cannot spawn a command that does not exist — after the cut.
        (
            "wt-spawn",
            json!({ "name": "s", "command": [tmp.path().join("no-such-bin").to_string_lossy()],
                    "cwd": repo.to_string_lossy(), "worktree": worktree("wt-spawn") }),
        ),
    ];
    for (branch, body) in cases {
        refused(branch, body).await;
        assert!(
            !tmp.path().join(branch).exists(),
            "{branch}: directory left"
        );
        assert!(!has_branch(&repo, branch), "{branch}: branch left");
    }
    let listed = worktree_git(&repo, &["worktree", "list", "--porcelain"]);
    assert_eq!(listed.matches("worktree ").count(), 1, "{listed}");
}

#[tokio::test]
async fn a_worktree_outside_the_workspace_is_refused_before_it_is_cut() {
    let tmp = tempfile::tempdir().unwrap();
    let workspace = tmp.path().join("ws");
    worktree_checkout(&workspace);
    let outside = tmp.path().join("other");
    worktree_checkout(&outside);
    let mut cfg = test_config();
    cfg.default_cwd = workspace.clone();
    cfg.workspace_root = workspace.canonicalize().unwrap();
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    // A checkout outside the workspace, and the workspace root itself, whose
    // sibling is outside it.
    for (checkout, branch) in [(&outside, "wt-outside"), (&workspace, "wt-root")] {
        let res = client
            .post(format!("{base}/api/sessions"))
            .json(&json!({
                "name": "escape",
                "command": ["/bin/cat"],
                "cwd": checkout.to_string_lossy(),
                "worktree": { "branch": branch, "base_ref": "main" },
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST, "{branch}");
        assert!(
            !tmp.path().join(branch).exists(),
            "{branch}: directory left"
        );
        assert!(!has_branch(checkout, branch), "{branch}: branch left");
    }
}

/// WI-1304: with cleanup on, stopping a session in a linked worktree removes
/// its build output, but not while another running session shares the
/// worktree; the main checkout is never touched.
#[tokio::test]
async fn worktree_cleanup_waits_for_every_session_in_the_worktree() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    let git = |dir: &std::path::Path, args: &[&str]| {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    };
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "seed@vogt.invalid"]);
    git(&repo, &["config", "user.name", "seed"]);
    std::fs::write(repo.join("README"), "r").unwrap();
    git(&repo, &["add", "README"]);
    git(&repo, &["commit", "-qm", "init"]);
    let linked = tmp.path().join("linked");
    git(&repo, &["worktree", "add", "-q", linked.to_str().unwrap()]);
    std::fs::write(linked.join("Cargo.toml"), "[package]\nname=\"t\"\n").unwrap();
    let target = linked.join("target");
    std::fs::create_dir(&target).unwrap();
    std::fs::write(target.join("out"), "built").unwrap();

    let mut cfg = test_config();
    cfg.default_cwd = tmp.path().to_path_buf();
    cfg.workspace_root = tmp.path().canonicalize().unwrap();
    cfg.state_dir = tmp.path().join("state");
    cfg.worktree_clean_on_stop = true;
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let cwd = linked.to_string_lossy().into_owned();
    let a = create_session_with(
        &client,
        &base,
        json!({ "name": "a", "command": ["/bin/cat"], "cwd": cwd }),
    )
    .await;
    let b = create_session_with(
        &client,
        &base,
        json!({ "name": "b", "command": ["/bin/cat"], "cwd": cwd }),
    )
    .await;

    let kill = client
        .post(format!("{base}/api/sessions/{a}/kill"))
        .send()
        .await
        .unwrap();
    assert_eq!(kill.status(), StatusCode::OK);
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(
        target.join("out").exists(),
        "session b still works in the worktree"
    );

    let kill = client
        .post(format!("{base}/api/sessions/{b}/kill"))
        .send()
        .await
        .unwrap();
    assert_eq!(kill.status(), StatusCode::OK);
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    while target.exists() && std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(!target.exists(), "the last session out cleans the worktree");
    assert!(linked.join("Cargo.toml").exists());
    assert!(repo.join("README").exists());
}

#[tokio::test]
async fn a_pod_placement_refuses_a_worktree_and_names_the_field() {
    let mut h = pod_stream::harness(false).await;
    let response = reqwest::Client::new()
        .post(format!("{}/api/sessions", h.base))
        .headers(auth())
        .json(&json!({
            "name": "pod-worktree",
            "template": "pod",
            "placement": "pod",
            "worktree": { "branch": "wi-3883-worker", "base_ref": "main" }
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = response.text().await.unwrap();
    assert!(
        body.contains("worktree"),
        "the refusal names the field: {body}"
    );
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), h.created.recv())
            .await
            .is_err(),
        "a refused start must not create a pod"
    );
}

/// WI-1325: `POST /api/workspaces/clean` drops build output and stamps
/// `cleaned_at`, and removes a worktree only when no live or hibernated
/// session uses it, the tree is clean (`force` or not), and its branch is
/// merged (unless forced) — deleting the merged branch with it.
#[tokio::test]
async fn workspace_clean_refuses_live_dirty_and_unmerged_trees() {
    let (tmp, cfg, stub) = hibernation_sandbox();
    std::fs::write(&stub, "#!/bin/sh\nprintf '> '\nsleep 300\n").unwrap();
    let repo = tmp.path().join("repo");
    worktree_checkout(&repo);
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let created: Value = client
        .post(format!("{base}/api/sessions"))
        .json(
            &json!({ "name": "wc worktree", "command": [stub.to_string_lossy()],
                       "cwd": repo.to_string_lossy(),
                       "worktree": { "branch": "wi-1325-worker", "base_ref": "main" } }),
        )
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap().to_string();
    let listed: Value = client
        .get(format!("{base}/api/workspaces"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let path = listed["workspaces"][0]["path"]
        .as_str()
        .unwrap()
        .to_string();
    let tree = std::path::PathBuf::from(&path);
    std::fs::create_dir_all(tree.join("node_modules/pkg")).unwrap();
    std::fs::write(tree.join("node_modules/pkg/index.js"), "x".repeat(4096)).unwrap();
    let clean = |mode: &'static str, force: bool| {
        let client = client.clone();
        let url = format!("{base}/api/workspaces/clean");
        let path = path.clone();
        async move {
            client
                .post(url)
                .json(&json!({ "path": path, "mode": mode, "force": force }))
                .send()
                .await
                .unwrap()
        }
    };

    // A running session's tree: neither mode touches it.
    assert_eq!(
        clean("build_output", false).await.status(),
        StatusCode::CONFLICT
    );
    assert_eq!(clean("remove", true).await.status(), StatusCode::CONFLICT);
    assert!(tree.join("node_modules").is_dir());

    // Hibernated: build output goes and is stamped; removal still waits.
    let hibernated = client
        .post(format!("{base}/api/sessions/{id}/hibernate"))
        .json(&json!({ "reason": "test" }))
        .send()
        .await
        .unwrap();
    assert_eq!(hibernated.status(), StatusCode::OK);
    let resp = clean("build_output", false).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    assert!(body["bytes_freed"].as_u64().unwrap() >= 4096, "{body}");
    assert!(!tree.join("node_modules").exists());
    let listed: Value = client
        .get(format!("{base}/api/workspaces"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(listed["workspaces"][0]["cleaned_at"], body["cleaned_at"]);
    assert_eq!(clean("remove", false).await.status(), StatusCode::CONFLICT);
    let _ = client
        .delete(format!("{base}/api/sessions/{id}"))
        .send()
        .await;

    // Dirty: refused even when forced.
    std::fs::write(tree.join("g.txt"), "work\n").unwrap();
    assert_eq!(clean("remove", true).await.status(), StatusCode::CONFLICT);
    // Committed but unmerged: refused without force.
    worktree_git(&tree, &["add", "g.txt"]);
    worktree_git(&tree, &["commit", "-q", "-m", "work"]);
    assert_eq!(clean("remove", false).await.status(), StatusCode::CONFLICT);
    assert!(tree.is_dir());

    // Landed on origin/main: the worktree and the merged branch both go.
    worktree_git(&tree, &["push", "-q", "origin", "HEAD:main"]);
    worktree_git(&tree, &["fetch", "-q", "origin"]);
    let resp = clean("remove", false).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["removed"], true, "{body}");
    assert_eq!(body["branch_deleted"], "wi-1325-worker");
    assert!(!tree.exists());
    assert!(!has_branch(&repo, "wi-1325-worker"));

    // A linked worktree outside the workspace root that no session recorded
    // is refused, directly or through a symlink inside the root.
    let outside = tempfile::tempdir().unwrap();
    let foreign = outside.path().join("repo");
    worktree_checkout(&foreign);
    let foreign_tree = outside.path().join("foreign-wt");
    worktree_git(
        &foreign,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "foreign",
            &foreign_tree.to_string_lossy(),
            "main",
        ],
    );
    std::fs::create_dir_all(foreign_tree.join("target")).unwrap();
    let link = tmp.path().join("innocent");
    std::os::unix::fs::symlink(&foreign_tree, &link).unwrap();
    for target in [&foreign_tree, &link] {
        for mode in ["build_output", "remove"] {
            let resp = client
                .post(format!("{base}/api/workspaces/clean"))
                .json(&json!({ "path": target.to_string_lossy(), "mode": mode, "force": true }))
                .send()
                .await
                .unwrap();
            assert_eq!(
                resp.status(),
                StatusCode::FORBIDDEN,
                "{mode} {}",
                target.display()
            );
        }
    }
    assert!(foreign_tree.join("target").is_dir());
}

/// WI-1341: a land fetches, checks the pinned tip, rebases or refuses, checks
/// history and messages, and pushes the base forward without a merge commit
/// or `--force` — all in a temporary worktree that is gone afterwards. Only
/// vogt-core's own credential may ask.
#[tokio::test]
async fn land_fast_forwards_or_rebases_and_refuses_by_name() {
    let door = wi1255_front_door().await;
    let root = door.repo.parent().unwrap().to_path_buf();
    let origin = root.join("origin.git");
    let dev = root.join("dev");
    worktree_git(&root, &["init", "-q", "--bare", "-b", "main", "origin.git"]);
    worktree_git(
        &door.repo,
        &["remote", "add", "origin", &origin.to_string_lossy()],
    );
    worktree_git(&door.repo, &["push", "-q", "origin", "main"]);
    worktree_git(
        &root,
        &[
            "clone",
            "-q",
            &origin.to_string_lossy(),
            &dev.to_string_lossy(),
        ],
    );
    worktree_git(&dev, &["config", "user.email", "t@t"]);
    worktree_git(&dev, &["config", "user.name", "t"]);
    let init = worktree_git(&dev, &["rev-parse", "HEAD"])
        .trim()
        .to_string();
    // A branch cut from `from`, one commit writing `file`, pushed.
    let branch = |name: &str, from: &str, file: &str, body: &str, message: &str| -> String {
        worktree_git(&dev, &["fetch", "-q", "origin"]);
        worktree_git(&dev, &["checkout", "-q", "-b", name, from]);
        std::fs::write(dev.join(file), body).unwrap();
        worktree_git(&dev, &["add", file]);
        worktree_git(&dev, &["commit", "-q", "-m", message]);
        worktree_git(&dev, &["push", "-q", "origin", name]);
        worktree_git(&dev, &["rev-parse", "HEAD"])
            .trim()
            .to_string()
    };
    let on_origin = |name: &str| -> String {
        worktree_git(&origin, &["rev-parse", &format!("refs/heads/{name}")])
            .trim()
            .to_string()
    };
    let land = |branch: &str, head: &str, mode: &str| {
        json!({
            "op": "land", "repo": "repo", "branch": branch, "head_sha": head,
            "onto": "main", "mode": mode, "linear_history": true,
            "forbidden_text": ["grok"],
        })
    };
    let worktrees = || {
        worktree_git(&door.repo, &["worktree", "list"])
            .lines()
            .count()
    };
    let worktrees_before = worktrees();
    let refused = |said: &Value, needle: &str| {
        assert!(
            said["error"].as_str().unwrap_or("").contains(needle),
            "wanted {needle:?}: {said}"
        );
    };

    let f1 = branch("f1", &init, "a.txt", "f1\n", "f1 change");
    // Wrong caller: a person, an overseer, break-glass.
    for token in [WI1255_PERSON, WI1255_OVERSEER, TEST_TOKEN] {
        let (status, said) = door.git_op(token, land("f1", &f1, "fast_forward")).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{token} {said}");
        refused(&said, "only vogt-core lands");
    }
    assert_eq!(on_origin("main"), init);

    // Fast-forward.
    let (status, said) = door
        .git_op(WI1255_STACK, land("f1", &f1, "fast_forward"))
        .await;
    assert_eq!(status, StatusCode::OK, "{said}");
    assert_eq!(said["onto_before"], init.as_str());
    assert_eq!(said["onto_after"], f1.as_str());
    assert_eq!(said["commits"], json!([f1]));
    assert_eq!(said["rebased"], false);
    assert_eq!(on_origin("main"), f1);

    // Stale tip.
    let f2 = branch("f2", &f1, "c.txt", "c\n", "f2 change");
    let (status, said) = door
        .git_op(WI1255_STACK, land("f2", &init, "fast_forward"))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{said}");
    refused(&said, "stale tip");
    assert_eq!(on_origin("f2"), f2);
    assert_eq!(on_origin("main"), f1);

    // Behind: fast_forward refuses, rebase lands.
    let f3 = branch("f3", &init, "b.txt", "b\n", "f3 change");
    let (status, said) = door
        .git_op(WI1255_STACK, land("f3", &f3, "fast_forward"))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{said}");
    refused(&said, "rebase first");
    assert_eq!(on_origin("f3"), f3);
    let (status, said) = door.git_op(WI1255_STACK, land("f3", &f3, "rebase")).await;
    assert_eq!(status, StatusCode::OK, "{said}");
    assert_eq!(said["rebased"], true);
    assert_eq!(said["onto_before"], f1.as_str());
    let tip = on_origin("main");
    assert_eq!(said["onto_after"], tip.as_str());
    assert_eq!(on_origin("f3"), tip, "the branch is rebased too");
    assert_eq!(
        worktree_git(&origin, &["rev-parse", "main^"]).trim(),
        f1,
        "one commit on top of the old base, no merge"
    );
    assert!(worktree_git(&origin, &["rev-list", "--merges", "main"])
        .trim()
        .is_empty());

    // Rebase conflict.
    let f4 = branch("f4", &init, "a.txt", "f4\n", "f4 change");
    let (status, said) = door.git_op(WI1255_STACK, land("f4", &f4, "rebase")).await;
    assert_eq!(status, StatusCode::CONFLICT, "{said}");
    refused(&said, "rebase conflict");
    refused(&said, "a.txt");
    assert_eq!(on_origin("f4"), f4);
    assert_eq!(on_origin("main"), tip);

    // Merge commit.
    branch("f5", &tip, "d.txt", "d\n", "f5 change");
    worktree_git(&dev, &["checkout", "-q", "-b", "f5side", &tip]);
    std::fs::write(dev.join("e.txt"), "e\n").unwrap();
    worktree_git(&dev, &["add", "e.txt"]);
    worktree_git(&dev, &["commit", "-q", "-m", "side"]);
    worktree_git(&dev, &["checkout", "-q", "f5"]);
    worktree_git(
        &dev,
        &["merge", "-q", "--no-ff", "-m", "merge side", "f5side"],
    );
    worktree_git(&dev, &["push", "-q", "origin", "f5"]);
    let f5 = worktree_git(&dev, &["rev-parse", "HEAD"])
        .trim()
        .to_string();
    let (status, said) = door
        .git_op(WI1255_STACK, land("f5", &f5, "fast_forward"))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{said}");
    refused(&said, "merge commits");
    refused(&said, &f5);
    assert_eq!(on_origin("main"), tip);

    // Forbidden text.
    let f6 = branch("f6", &tip, "g.txt", "g\n", "written by grok");
    let (status, said) = door
        .git_op(WI1255_STACK, land("f6", &f6, "fast_forward"))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{said}");
    refused(&said, "forbidden text");
    assert_eq!(on_origin("main"), tip);

    // An https GitHub origin with no agent-auth helper entry cannot push.
    worktree_git(
        &door.repo,
        &[
            "remote",
            "set-url",
            "origin",
            "https://github.com/example/none.git",
        ],
    );
    let (status, said) = door
        .git_op(WI1255_STACK, land("f6", &f6, "fast_forward"))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{said}");
    refused(&said, "agent-auth helper");

    assert_eq!(worktrees(), worktrees_before, "every temp worktree is gone");
}

/// WI-1340: `GET /api/workspaces` lists every session with a workspace
/// block — a worktree and a checkout — with its size and liveness, and
/// leaves out a session in no repo.
#[tokio::test]
async fn the_workspaces_route_lists_each_session_tree_with_its_size() {
    let (tmp, cfg, stub) = hibernation_sandbox();
    std::fs::write(&stub, "#!/bin/sh\nprintf '> '\nsleep 300\n").unwrap();
    let repo = tmp.path().join("repo");
    worktree_checkout(&repo);
    let plain = tmp.path().join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let command = json!([stub.to_string_lossy()]);
    let mut ids = Vec::new();
    for body in [
        json!({ "name": "wl worktree", "command": command, "cwd": repo.to_string_lossy(),
                "worktree": { "branch": "wi-1340-worker", "base_ref": "main" } }),
        json!({ "name": "wl checkout", "command": command, "cwd": repo.to_string_lossy() }),
        json!({ "name": "wl none", "command": command, "cwd": plain.to_string_lossy() }),
    ] {
        let created: Value = client
            .post(format!("{base}/api/sessions"))
            .json(&body)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        ids.push(created["id"].as_str().unwrap().to_string());
    }

    let resp = client
        .get(format!("{base}/api/workspaces"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let listed: Value = resp.json().await.unwrap();
    let rows = listed["workspaces"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "{listed}");
    for (id, kind) in [(&ids[0], "worktree"), (&ids[1], "checkout")] {
        let row = rows
            .iter()
            .find(|r| r["session_id"] == id.as_str())
            .unwrap_or_else(|| panic!("{id} missing from {listed}"));
        assert_eq!(row["kind"], kind);
        assert_eq!(row["alive"], true);
        assert!(row["size_bytes"].as_u64().is_some_and(|n| n > 0), "{row}");
        assert!(row["path"].as_str().is_some());
    }

    for id in ids {
        let _ = client
            .delete(format!("{base}/api/sessions/{id}"))
            .send()
            .await;
    }
}
