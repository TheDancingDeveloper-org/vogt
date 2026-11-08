"""Session images must ship the same guarded Codex as the stack image."""

from pathlib import Path


def test_session_image_uses_the_stack_codex_policy_and_pins() -> None:
    engine = Path(__file__).resolve().parents[1] / "engine"
    dockerfile = (engine / "Dockerfile.pod").read_text()
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


def test_session_image_codex_wires_the_shared_policy_hook() -> None:
    """The Codex launch spec runs the same policy hook a Claude session does.

    Codex reads `/etc/codex/requirements.toml` and no flag can point it
    elsewhere, so the image copy is the launch spec (WI-3755). A missing or
    rewritten hook refuses the launch in `engine/session-agent/src/
    codex_policy.rs`; this test keeps the shipped file and the image copy of
    the program from drifting apart.
    """
    engine = Path(__file__).resolve().parents[1] / "engine"
    requirements = (
        engine / "deploy" / "driven-session-codex-requirements.toml"
    ).read_text()
    assert "[features]\nhooks = true" in requirements
    assert requirements.count('command = "/usr/local/bin/vogt-policy-hook codex"') == 2
    assert requirements.count("\nfailClosed = true") == 2
    matcher = 'matcher = "^(Bash|shell|exec_command|write_stdin|apply_patch)$"'
    assert matcher in requirements
    dockerfile = (engine / "Dockerfile.pod").read_text()
    assert "deploy/policy-hook.sh /usr/local/bin/vogt-policy-hook" in dockerfile
