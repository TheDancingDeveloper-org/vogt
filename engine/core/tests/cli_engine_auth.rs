//! The pod's `vogt` CLI must fail loudly when the engine refuses its token.
//!
//! `llm-provider list` surfaced this (WI-4315): the engine answered 401, the
//! shared engine client mapped it to `EngineUnavailable`, the list operation
//! degraded that to an empty result, and the CLI printed `(none)` with exit 0.
//! A refusal is not an outage — it is the credential or its scopes — so the
//! command must exit non-zero and name the refusal.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Command, Output};

fn binary() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_vogt-core"));
    // The same deterministic hooks the other binary tests set, so ids and
    // clock stamps do not depend on the machine the test runs on.
    command
        .env("VOGT_TEST_CLOCK_START", "2026-01-02T03:04:05+00:00")
        .env("VOGT_TEST_IDS", "sequential")
        .env("USER", "test")
        .env("LOGNAME", "test");
    command
}

/// A fresh root with an initialised instance under `instance/`, and a token
/// file the CLI will send to the fake engine.
fn fresh(name: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!(
        "vogt-cli-engine-auth-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let data = root.join("instance");
    let init = binary()
        .arg("--json")
        .arg("--data-dir")
        .arg(&data)
        .arg("init")
        .output()
        .expect("run init");
    assert!(
        init.status.success(),
        "init: {}",
        String::from_utf8_lossy(&init.stderr)
    );
    let token = root.join("engine.token");
    std::fs::write(&token, "test-token").unwrap();
    (root, data, token)
}

/// One engine answer: bind a listener, serve exactly one request, and hand
/// back the base URL plus the thread that will serve it.
fn engine(status: u16, body: &'static str) -> (String, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        // A GET has no body; the request headers are enough for the client's
        // call to complete, and the response closes the connection.
        let mut request = [0u8; 8192];
        let _ = stream.read(&mut request);
        let reason = match status {
            401 => "Unauthorized",
            403 => "Forbidden",
            _ => "OK",
        };
        let answer = format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(answer.as_bytes()).unwrap();
        let _ = stream.flush();
    });
    (format!("http://{address}"), handle)
}

fn run_list(data: &PathBuf, token: &PathBuf, url: &str) -> Output {
    binary()
        .arg("--data-dir")
        .arg(data)
        .args(["llm-provider", "list"])
        .env("VOGT_ENGINE_URL", url)
        .env("VOGT_ENGINE_TOKEN_FILE", token)
        .output()
        .expect("run llm-provider list")
}

#[test]
fn llm_provider_list_fails_loudly_on_401_and_403_and_never_prints_none() {
    let cases = [
        (
            401u16,
            r#"{"error":"no bearer token presented"}"#,
            "unauthenticated",
            "401",
        ),
        (
            403u16,
            r#"{"error":"forbidden: this credential is the stack secret (vogt-core's own identity), which may drive sessions and read history but never writes to Vogt through the front door; it lacks the AgentClisWrite capability, which the admin scope grants"}"#,
            "forbidden",
            "AgentClisWrite",
        ),
    ];
    for (status, body, code, said) in cases {
        let (root, data, token) = fresh(&status.to_string());
        let (url, server) = engine(status, body);
        let output = run_list(&data, &token, &url);
        server.join().unwrap();

        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert_eq!(
            output.status.code(),
            Some(1),
            "HTTP {status} must exit non-zero; stdout={stdout:?} stderr={stderr:?}"
        );
        assert!(
            stderr.contains(&format!("error: {code}:")),
            "HTTP {status} must name the refusal as {code}: {stderr:?}"
        );
        assert!(
            stderr.contains(said),
            "HTTP {status} must carry the reason {said:?}: {stderr:?}"
        );
        assert!(
            !stdout.contains("(none)"),
            "HTTP {status} must not render an empty list: {stdout:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
