"""Session images must ship the same guarded Codex as the stack image."""

from pathlib import Path

import pytest

ENGINE = Path(__file__).resolve().parents[1] / "engine"

pytestmark = pytest.mark.skipif(
    not ENGINE.is_dir(),
    reason="needs engine/ (absent in the core-alone job)",
)


def test_session_image_uses_the_stack_codex_policy_and_pins() -> None:
    dockerfile = (ENGINE / "Dockerfile.pod").read_text()
    assert "@openai/codex" not in dockerfile
    assert (
        "COPY agent-versions.env /usr/local/share/vogt/agent-versions.env" in dockerfile
    )
    assert "fork-${VOGT_CODEX_SHA}/${asset}" in dockerfile
    assert 'echo "$codex_sum  /tmp/${target}/codex" | sha256sum -c -' in dockerfile
    assert "go_sum" not in dockerfile
    assert "deploy/codex-full-access.sh /usr/local/bin/codex" in dockerfile
    assert (
        "deploy/driven-session-codex-requirements.toml /etc/codex/requirements.toml"
        in dockerfile
    )
    assert "klaudia" not in dockerfile.lower()


def test_session_image_installs_the_pinned_agent_clis_with_version_guards() -> None:
    """WI-4130: the pod image carries claude and opencode at the repo pins.

    The pins come from `agent-versions.env`, the same file the stack image
    reads, so the two images cannot drift apart. The `--version` guards are
    the stack image's: an npm install that resolves but leaves a stub must
    fail the build instead of producing a pod whose CLI cannot start.
    """
    dockerfile = (ENGINE / "Dockerfile.pod").read_text()
    assert ". /usr/local/share/vogt/agent-versions.env" in dockerfile
    assert "ARG CLAUDE_CODE_VERSION=" not in dockerfile
    assert "ARG OPENCODE_VERSION=" not in dockerfile
    assert '"@anthropic-ai/claude-code@${claude_code_version}"' in dockerfile
    assert '"opencode-ai@${opencode_version}"' in dockerfile
    assert "/usr/local/bin/claude --version" in dockerfile
    assert "/usr/local/bin/opencode --version" in dockerfile
    assert dockerfile.index("COPY agent-versions.env") < dockerfile.index(
        '"@anthropic-ai/claude-code@${claude_code_version}"'
    ), "the pins must be copied before the install reads them"


def test_session_image_codex_wires_the_shared_policy_hook() -> None:
    """The Codex launch spec runs the same policy hook a Claude session does.

    Codex reads `/etc/codex/requirements.toml` and no flag can point it
    elsewhere, so the image copy is the launch spec (WI-3755). A missing or
    rewritten hook refuses the launch in `engine/session-agent/src/
    codex_policy.rs`; this test keeps the shipped file and the image copy of
    the program from drifting apart.
    """
    requirements = (
        ENGINE / "deploy" / "driven-session-codex-requirements.toml"
    ).read_text()
    assert "[features]\nhooks = true" in requirements
    assert requirements.count('command = "/usr/local/bin/vogt-policy-hook codex"') == 2
    assert requirements.count("\nfailClosed = true") == 2
    matcher = 'matcher = "^(Bash|shell|exec_command|write_stdin|apply_patch)$"'
    assert matcher in requirements
    dockerfile = (ENGINE / "Dockerfile.pod").read_text()
    assert "deploy/policy-hook.sh /usr/local/bin/vogt-policy-hook" in dockerfile
