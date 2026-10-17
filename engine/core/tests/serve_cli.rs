//! The served process as an operator runs it: the listen address is required,
//! every surface answers on the one port, and a quick start from the stack
//! secret reaches a first operator, with the CLI fallback closing the door too.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// The stack secret a Docker quick start writes before `docker compose up`.
const CORE_SECRET: &str = "vogt_stack_secret_that_is_long_enough_0123456789";
const PASSWORD: &str = "correct horse battery staple";

fn binary() -> Command {
    Command::new(env!("CARGO_BIN_EXE_vogt-core"))
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vogt-serve-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

/// A served process. Dropping it stops the process.
struct Served {
    child: Child,
    port: u16,
}

impl Drop for Served {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Starts `serve` on a free port and waits until it answers liveness.
fn serve(data_dir: &Path, config_file: Option<&Path>) -> Served {
    let port = free_port();
    let mut command = binary();
    command
        .arg("--data-dir")
        .arg(data_dir)
        .args(["serve", "--host", "127.0.0.1", "--port"])
        .arg(port.to_string())
        .arg("--no-schedule")
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(file) = config_file {
        command.env("VOGT_CONFIG_FILE", file);
    }
    let child = command.spawn().expect("start serve");
    let served = Served { child, port };
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if TcpStream::connect(("127.0.0.1", port)).is_ok()
            && send(port, "GET", "/health/live", None, None).0 == 200
        {
            return served;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("serve never answered /health/live on port {port}");
}

/// One request on a fresh connection, the way the HTTP tests make them.
fn send(
    port: u16,
    method: &str,
    path: &str,
    bearer: Option<&str>,
    body: Option<&Value>,
) -> (u16, String) {
    let mut stream = TcpStream::connect(SocketAddr::from(([127, 0, 0, 1], port))).unwrap();
    let auth = bearer
        .map(|token| format!("Authorization: Bearer {token}\r\n"))
        .unwrap_or_default();
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    let typed = if body.is_some() {
        "Content-Type: application/json\r\n"
    } else {
        ""
    };
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\n{auth}{typed}Content-Length: {}\r\nConnection: close\r\n\r\n{payload}",
        payload.len()
    )
    .unwrap();
    let mut buf = String::new();
    stream.read_to_string(&mut buf).unwrap();
    let (head, response) = buf.split_once("\r\n\r\n").unwrap();
    let status = head.split_whitespace().nth(1).unwrap().parse().unwrap();
    (status, response.to_string())
}

fn json_of(body: &str) -> Value {
    serde_json::from_str(body).unwrap_or_else(|err| panic!("{err}: {body}"))
}

#[test]
fn serve_refuses_to_start_without_a_listen_address() {
    let dir = scratch("listen");
    let no_host = binary()
        .arg("--data-dir")
        .arg(&dir)
        .args(["serve", "--port", "1"])
        .output()
        .unwrap();
    assert!(!no_host.status.success());
    let stderr = String::from_utf8_lossy(&no_host.stderr);
    assert!(stderr.contains("--host"), "{stderr}");

    let no_port = binary()
        .arg("--data-dir")
        .arg(&dir)
        .args(["serve", "--host", "127.0.0.1"])
        .output()
        .unwrap();
    assert!(!no_port.status.success());
    let stderr = String::from_utf8_lossy(&no_port.stderr);
    assert!(stderr.contains("--port"), "{stderr}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn every_surface_answers_on_the_one_port() {
    let dir = scratch("one-port");
    let init = binary()
        .arg("--data-dir")
        .arg(&dir)
        .arg("init")
        .output()
        .unwrap();
    assert!(init.status.success(), "{init:?}");
    let served = serve(&dir, None);
    for path in [
        "/health/live",
        "/health/ready",
        "/version",
        "/connection-info",
        "/openapi.json",
    ] {
        let (status, body) = send(served.port, "GET", path, None, None);
        assert_eq!(status, 200, "{path}: {body}");
    }
    // The API and MCP share the port too; they answer, with the gate in front.
    let (status, _) = send(served.port, "GET", "/api/status", None, None);
    assert_eq!(status, 401);
    let (status, _) = send(
        served.port,
        "POST",
        "/mcp",
        None,
        Some(&json!({"jsonrpc": "2.0", "id": 1, "method": "ping"})),
    );
    assert_eq!(status, 401);
    drop(served);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The Docker quick start: the stack secret is adopted at `init`, no person
/// exists yet, and the wizard's bootstrap creates the first login over HTTP.
#[test]
fn a_quick_start_reaches_a_first_operator_end_to_end() {
    let dir = scratch("quick-start");
    let config_dir = scratch("quick-start-config");
    let secret_file = config_dir.join("vogt-core-token");
    std::fs::write(&secret_file, CORE_SECRET).unwrap();
    let config_file = config_dir.join("vogt.toml");
    std::fs::write(
        &config_file,
        format!(
            "bootstrap_core_token_file = \"{}\"\n",
            secret_file.display()
        ),
    )
    .unwrap();

    let init = binary()
        .env("VOGT_CONFIG_FILE", &config_file)
        .arg("--data-dir")
        .arg(&dir)
        .arg("init")
        .output()
        .unwrap();
    assert!(init.status.success(), "{init:?}");

    let served = serve(&dir, Some(&config_file));
    let (status, body) = send(served.port, "GET", "/api/install/status", None, None);
    assert_eq!(status, 200, "{body}");
    assert_eq!(json_of(&body), json!({"install_mode": true}));

    let (status, body) = send(
        served.port,
        "POST",
        "/api/install/bootstrap",
        None,
        Some(&json!({"display_name": "Ada", "username": "ada", "password": PASSWORD})),
    );
    assert_eq!(status, 200, "{body}");
    assert_eq!(json_of(&body)["username"], "ada");

    let (status, body) = send(
        served.port,
        "POST",
        "/api/auth/login",
        None,
        Some(&json!({"username": "ada", "password": PASSWORD})),
    );
    assert_eq!(status, 200, "{body}");
    let session = json_of(&body)["secret"].as_str().unwrap().to_string();
    let (status, body) = send(served.port, "GET", "/api/auth/whoami", Some(&session), None);
    assert_eq!(status, 200, "{body}");
    assert_eq!(json_of(&body)["identity_ref"], "human:ada");

    let (status, body) = send(served.port, "GET", "/api/install/status", None, None);
    assert_eq!(status, 200, "{body}");
    assert_eq!(json_of(&body), json!({"install_mode": false}));
    let (status, body) = send(
        served.port,
        "POST",
        "/api/install/bootstrap",
        None,
        Some(&json!({"display_name": "Eve", "username": "eve", "password": PASSWORD})),
    );
    assert_eq!(status, 409, "{body}");
    assert_eq!(json_of(&body)["error"]["code"], "install_closed");

    // The adopted core token keeps the identity and scopes it was given.
    let (status, body) = send(
        served.port,
        "GET",
        "/api/auth/whoami",
        Some(CORE_SECRET),
        None,
    );
    assert_eq!(status, 200, "{body}");
    let me = json_of(&body);
    assert_eq!(me["identity_ref"], "agent:vogt-engine");
    assert_eq!(me["kind"], "agent");
    let mut scopes: Vec<String> = me["scopes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|scope| scope.as_str().unwrap().to_string())
        .collect();
    scopes.sort();
    assert_eq!(scopes, ["project.write", "read", "work.write"]);

    drop(served);
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&config_dir);
}

/// `vogt user create` inside the container is the documented alternative to the
/// wizard, and it closes the door the same way.
#[test]
fn the_cli_fallback_also_closes_a_quick_start() {
    let dir = scratch("cli-fallback");
    let init = binary()
        .arg("--data-dir")
        .arg(&dir)
        .arg("init")
        .output()
        .unwrap();
    assert!(init.status.success(), "{init:?}");
    let password_file = dir.join("password");
    std::fs::write(&password_file, PASSWORD).unwrap();

    let created = binary()
        .arg("--data-dir")
        .arg(&dir)
        .args([
            "user",
            "create",
            "--username",
            "ada",
            "--scopes",
            "admin",
            "--reason",
            "quick start",
        ])
        .arg("--password-file")
        .arg(&password_file)
        .output()
        .unwrap();
    assert!(created.status.success(), "{created:?}");

    let served = serve(&dir, None);
    let (status, body) = send(served.port, "GET", "/api/install/status", None, None);
    assert_eq!(status, 200, "{body}");
    assert_eq!(json_of(&body), json!({"install_mode": false}));
    let (status, body) = send(
        served.port,
        "POST",
        "/api/install/bootstrap",
        None,
        Some(&json!({"display_name": "Eve"})),
    );
    assert_eq!(status, 409, "{body}");
    drop(served);
    let _ = std::fs::remove_dir_all(&dir);
}
