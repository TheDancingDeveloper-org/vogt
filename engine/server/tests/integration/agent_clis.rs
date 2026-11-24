// ── Runtime-pinned agent CLIs ───────────────────────────────────────
//
// Driven through the *real* installer script (`engine/deploy/agent-cli-
// install.sh`) with a stand-in `npm` on its PATH, so what is asserted is the
// contract the entrypoint relies on too — not a mock of it.

use super::*;
use vogt_engine_server::agent_clis::AgentCliPaths;

const FAKE_NPM: &str = r#"#!/usr/bin/env bash
set -euo pipefail
case "$1" in
  install)
    prefix=""; spec=""
    shift
    while (( $# )); do
      case "$1" in
        --prefix) prefix="$2"; shift ;;
        -g|--global) ;;
        *) spec="$1" ;;
      esac
      shift
    done
    pkg="${spec%@*}"; ver="${spec##*@}"
    if [[ "$ver" == "${FAKE_NPM_FAIL_VERSION:-}" ]]; then
      echo "npm ERR! 404 Not Found - $spec" >&2
      exit 1
    fi
    case "$pkg" in
      @anthropic-ai/claude-code) bin=claude ;;
      @openai/codex) bin=codex ;;
      *) echo "unexpected package $pkg" >&2; exit 2 ;;
    esac
    mkdir -p "$prefix/bin" "$prefix/lib/node_modules/$pkg"
    printf '#!/usr/bin/env bash\necho "%s %s"\n' "$bin" "$ver" > "$prefix/bin/$bin"
    chmod +x "$prefix/bin/$bin"
    printf '{"name":"%s","version":"%s"}\n' "$pkg" "$ver" > "$prefix/lib/node_modules/$pkg/package.json"
    ;;
  view)
    echo "${FAKE_NPM_LATEST:-9.9.9}"
    ;;
  *)
    echo "fake npm: unsupported $*" >&2
    exit 2
    ;;
esac
"#;

/// A root, a tool table, a baked manifest and an installer wrapper that
/// runs the real script with the fake npm first on PATH.
fn sandbox() -> (tempfile::TempDir, AgentCliPaths) {
    let tmp = tempfile::tempdir().unwrap();
    let tools_dir = tmp.path().join("tools");
    std::fs::create_dir_all(&tools_dir).unwrap();
    let npm = tools_dir.join("npm");
    std::fs::write(&npm, FAKE_NPM).unwrap();
    std::fs::set_permissions(&npm, std::fs::Permissions::from_mode(0o755)).unwrap();

    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("deploy")
        .join("agent-cli-install.sh");
    let installer = tmp.path().join("vogt-agent-cli-install");
    std::fs::write(
        &installer,
        format!(
            "#!/usr/bin/env bash\nexport PATH=\"{}:$PATH\"\nexec bash {} \"$@\"\n",
            tools_dir.display(),
            script.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&installer, std::fs::Permissions::from_mode(0o755)).unwrap();

    let table = tmp.path().join("agent-clis.tools");
    std::fs::write(
        &table,
        "codex\t@openai/codex\tcodex\tVOGT_CODEX_VERSION\n\
             claude-code\t@anthropic-ai/claude-code\tclaude\tVOGT_CLAUDE_CODE_VERSION\n",
    )
    .unwrap();
    let baked = tmp.path().join("agent-versions.resolved");
    std::fs::write(&baked, "codex=0.149.1\nclaude-code=2.1.258\n").unwrap();
    let image_bin = tmp.path().join("image-bin");
    std::fs::create_dir_all(&image_bin).unwrap();
    std::fs::write(
        image_bin.join("claude"),
        "#!/usr/bin/env bash\necho image\n",
    )
    .unwrap();

    let paths = AgentCliPaths {
        root: tmp.path().join("agent-clis"),
        installer,
        tools: table,
        baked,
        image_bin,
    };
    (tmp, paths)
}

use std::os::unix::fs::PermissionsExt;

async fn boot_sandboxed() -> (tempfile::TempDir, String, ServerGuard) {
    let (tmp, paths) = sandbox();
    let mut cfg = test_config();
    cfg.agent_clis = paths;
    // A caller the core resolves as a writer: sessions, files, git — but
    // not the operator grant that moves a CLI pin.
    let core = stand_in_core_knowing(vec![(
        "sessions-only-token-1234567890",
        "human:writer",
        vec!["read", "work.write"],
    )])
    .await;
    cfg.vogt_core_url = Some(core);
    let (base, handle) = boot_with_config(cfg).await;
    (tmp, base, handle)
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap()
}

fn tool<'a>(report: &'a Value, name: &str) -> &'a Value {
    report["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["tool"] == name)
        .unwrap_or_else(|| panic!("no tool {name} in {report}"))
}

#[tokio::test]
async fn the_report_names_the_baked_baseline_as_active_until_a_pin_is_applied() {
    let (_tmp, base, _h) = boot_sandboxed().await;
    let unauth = reqwest::get(format!("{base}/api/agent-clis"))
        .await
        .unwrap();
    assert_eq!(unauth.status(), StatusCode::UNAUTHORIZED);

    let report: Value = client()
        .get(format!("{base}/api/agent-clis"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(report["installer_present"], true);
    let claude = tool(&report, "claude-code");
    assert_eq!(claude["source"], "image");
    assert_eq!(claude["active_version"], "2.1.258");
    assert_eq!(claude["baked_version"], "2.1.258");
    assert_eq!(claude["env_var"], "VOGT_CLAUDE_CODE_VERSION");
    assert!(claude.get("upstream_latest").is_none(), "not asked for");
    // Codex has no image binary in this sandbox: the honest word is absent.
    assert_eq!(tool(&report, "codex")["source"], "absent");
}

#[tokio::test]
async fn a_post_installs_the_version_and_the_report_follows() {
    let (tmp, base, _h) = boot_sandboxed().await;
    let report: Value = client()
        .post(format!("{base}/api/agent-clis/claude-code"))
        .json(&json!({ "version": "2.1.261" }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let claude = tool(&report, "claude-code");
    assert_eq!(claude["source"], "runtime");
    assert_eq!(claude["active_version"], "2.1.261");
    assert_eq!(claude["baked_version"], "2.1.258");
    assert_eq!(claude["installed_versions"], json!(["2.1.261"]));
    // The installer's own artefacts are what the report read.
    let root = tmp.path().join("agent-clis");
    assert!(root
        .join("claude-code")
        .join("2.1.261")
        .join("bin")
        .join("claude")
        .exists());
    assert_eq!(
        std::fs::read_to_string(root.join("manifest")).unwrap(),
        "codex=0.149.1\nclaude-code=2.1.261\n"
    );

    // Back to the image copy: offline, and the report says image again.
    let report: Value = client()
        .post(format!("{base}/api/agent-clis/claude-code"))
        .json(&json!({ "version": "image" }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let claude = tool(&report, "claude-code");
    assert_eq!(claude["source"], "image");
    assert_eq!(claude["active_version"], "2.1.258");
    // The prefix stays for an offline switch later.
    assert_eq!(claude["installed_versions"], json!(["2.1.261"]));
}

#[tokio::test]
async fn a_failed_install_is_a_conflict_and_changes_nothing() {
    let (tmp, base, _h) = boot_sandboxed().await;
    // The installer inherits the engine's environment; the failing version
    // is named through the fake npm's own switch, set in the wrapper.
    let installer = tmp.path().join("vogt-agent-cli-install");
    let wrapper = std::fs::read_to_string(&installer).unwrap();
    std::fs::write(
        &installer,
        wrapper.replace(
            "exec bash",
            "export FAKE_NPM_FAIL_VERSION=2.1.999\nexec bash",
        ),
    )
    .unwrap();

    let response = client()
        .post(format!("{base}/api/agent-clis/claude-code"))
        .json(&json!({ "version": "2.1.999" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let said = response.text().await.unwrap();
    assert!(said.contains("previous version stays"), "{said}");
    assert!(
        said.contains("npm install failed") || said.contains("stays on"),
        "{said}"
    );

    let report: Value = client()
        .get(format!("{base}/api/agent-clis"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(tool(&report, "claude-code")["source"], "image");
    assert!(!tmp
        .path()
        .join("agent-clis")
        .join("claude-code")
        .join("2.1.999")
        .exists());
}

#[tokio::test]
async fn malformed_requests_are_refused_before_the_installer_runs() {
    let (tmp, base, _h) = boot_sandboxed().await;
    for bad in ["2.1", "v2.1.261", "2.1.261; rm -rf /", "--flag", ""] {
        let response = client()
            .post(format!("{base}/api/agent-clis/claude-code"))
            .json(&json!({ "version": bad }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{bad:?}");
    }
    // A dist-tag reaches the installer, which refuses it without the opt-in
    // — the installer's EX_USAGE is a bad request here too.
    let response = client()
        .post(format!("{base}/api/agent-clis/claude-code"))
        .json(&json!({ "version": "latest" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(response
        .text()
        .await
        .unwrap()
        .contains("VOGT_AGENT_CLI_ALLOW_DIST_TAGS"));

    let response = client()
        .post(format!("{base}/api/agent-clis/opencode"))
        .json(&json!({ "version": "1.0.0" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert!(!tmp.path().join("agent-clis").join("opencode").exists());
}

#[tokio::test]
async fn changing_a_pin_needs_the_agent_clis_write_capability() {
    let (_tmp, base, _h) = boot_sandboxed().await;
    let scoped = reqwest::Client::builder()
        .default_headers(auth_for("sessions-only-token-1234567890"))
        .build()
        .unwrap();
    // Reading is any valid token's.
    let read = scoped
        .get(format!("{base}/api/agent-clis"))
        .send()
        .await
        .unwrap();
    assert_eq!(read.status(), StatusCode::OK);
    // Moving the pin is not.
    let write = scoped
        .post(format!("{base}/api/agent-clis/claude-code"))
        .json(&json!({ "version": "2.1.261" }))
        .send()
        .await
        .unwrap();
    assert_eq!(write.status(), StatusCode::FORBIDDEN);
}
