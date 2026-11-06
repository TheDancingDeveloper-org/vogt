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
    assert '"$VOGT_KLAUDIA_SOURCE" "$VOGT_KLAUDIA_VERSION"' in dockerfile
    assert 'echo "$go_sum  /tmp/go.tar.gz" | sha256sum -c -' in dockerfile
    assert "deploy/codex-full-access.sh /usr/local/bin/codex" in dockerfile
    assert (
        "deploy/driven-session-codex-requirements.toml /etc/codex/requirements.toml"
        in dockerfile
    )
    assert "chmod 0755 /usr/local/bin/klaudia /etc/codex" in dockerfile
    assert "0 755 regular file\\n0 755 directory" in dockerfile
    assert "0 755\\n0 644" in dockerfile
