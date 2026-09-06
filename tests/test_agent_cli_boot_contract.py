"""The agent-CLI boot contract, driven through the real scripts (#590).

`engine/deploy/agent-cli-install.sh` applies a runtime pin at container start
and `engine/deploy/verify-agent-clis.sh` then judges the result — under the
entrypoint's `set -euo pipefail`, so its exit status decides whether the pod
boots. These tests pin exactly what may and may not stop a pod, with real npm
and a local tarball (an `npm` shim rewrites only the package spec, so the run
is offline while `npm list`, the check that verify relies on, stays real):

- a pin, satisfied or not, never fails the boot;
- a manifest/active mismatch does (exit 78);
- a persisted-home shadow is quarantined (moved aside, warned) and the pod
  boots; `VOGT_AGENT_SHADOW_POLICY=fail` restores the strict refusal and
  `warn` acknowledges a deliberate override.

Characterised empirically on 2026-09-05 after a dev deploy failure was first
misattributed to the verify step; this keeps that reasoning honest in CI.
"""

from __future__ import annotations

import os
import shutil
import subprocess
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[1]
INSTALL = REPO_ROOT / "engine" / "deploy" / "agent-cli-install.sh"
VERIFY = REPO_ROOT / "engine" / "deploy" / "verify-agent-clis.sh"
BAKED = "2.1.258"
PINNED = "9.9.9"

pytestmark = pytest.mark.skipif(
    not (INSTALL.is_file() and VERIFY.is_file()) or shutil.which("npm") is None,
    reason="needs engine/deploy (absent in the core-alone job) and npm",
)


class Sandbox:
    """An image-shaped root: baked CLI, resolved manifest, tools table, a
    runtime root, a persisted home, and an npm shim serving one local tarball."""

    def __init__(self, base: Path) -> None:
        self.base = base
        self.root = base / "root"
        self.image_bin = base / "imagebin"
        self.home = base / "home"
        self.share = base / "share"
        self.shim = base / "shim"
        for path in (self.root, self.image_bin, self.home, self.share, self.shim):
            path.mkdir(parents=True)
        self._baked_tool()
        (self.share / "agent-versions.resolved").write_text(
            f"claude-code={BAKED}\n", encoding="utf-8"
        )
        (self.share / "agent-clis.tools").write_text(
            "claude-code\t@acme/tool\ttool\tVOGT_CLAUDE_CODE_VERSION\n",
            encoding="utf-8",
        )
        self._npm_shim(self._tarball())

    def _baked_tool(self) -> None:
        tool = self.image_bin / "tool"
        tool.write_text(f"#!/bin/sh\necho {BAKED}\n", encoding="utf-8")
        tool.chmod(0o755)

    def _tarball(self) -> Path:
        pkg = self.base / "pkg"
        (pkg / "bin").mkdir(parents=True)
        (pkg / "package.json").write_text(
            f'{{"name":"@acme/tool","version":"{PINNED}","bin":{{"tool":"bin/tool"}}}}',
            encoding="utf-8",
        )
        binary = pkg / "bin" / "tool"
        binary.write_text(f"#!/bin/sh\necho {PINNED}\n", encoding="utf-8")
        binary.chmod(0o755)
        subprocess.run(
            ["npm", "pack", "--silent"], cwd=pkg, check=True, capture_output=True
        )
        return next(pkg.glob("*.tgz"))

    def _npm_shim(self, tarball: Path) -> None:
        real = shutil.which("npm")
        assert real is not None
        shim = self.shim / "npm"
        shim.write_text(
            "#!/bin/sh\n"
            'if [ "$1" = install ] && [ "$2" = -g ]; then\n'
            f'  case "$5" in @acme/tool@{PINNED}) exec "{real}" install -g '
            f'--prefix "$4" "{tarball}" ;; *) echo "no such package $5" >&2; '
            "exit 1 ;; esac\n"
            "fi\n"
            f'exec "{real}" "$@"\n',
            encoding="utf-8",
        )
        shim.chmod(0o755)

    def env(self, **extra: str) -> dict[str, str]:
        env = dict(os.environ)
        env.update(
            {
                "VOGT_AGENT_CLI_ROOT": str(self.root),
                "VOGT_AGENT_CLI_BAKED_MANIFEST": str(
                    self.share / "agent-versions.resolved"
                ),
                "VOGT_AGENT_CLI_TOOLS": str(self.share / "agent-clis.tools"),
                "VOGT_AGENT_CLI_IMAGE_BIN": str(self.image_bin),
                "VOGT_AGENT_CLI_HOME_BIN": str(self.home / ".npm-global" / "bin"),
                "HOME": str(self.home),
                "PATH": os.pathsep.join(
                    [
                        str(self.shim),
                        str(self.root / "bin"),
                        str(self.image_bin),
                        os.environ.get("PATH", ""),
                    ]
                ),
            }
        )
        env.update(extra)
        return env

    def install(self, version: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            ["bash", str(INSTALL), "claude-code", version],
            env=self.env(),
            capture_output=True,
            text=True,
            check=False,
        )

    def verify(self, **extra: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            ["bash", str(VERIFY)],
            env=self.env(**extra),
            capture_output=True,
            text=True,
            check=False,
        )

    def manifest(self) -> str:
        path = self.root / "manifest"
        return path.read_text(encoding="utf-8") if path.exists() else ""

    def current(self) -> str:
        link = self.root / "claude-code" / "current"
        return str(link.readlink()) if link.is_symlink() else ""


@pytest.fixture
def sandbox(tmp_path: Path) -> Sandbox:
    return Sandbox(tmp_path)


def test_a_satisfied_pin_installs_and_verifies(sandbox: Sandbox) -> None:
    done = sandbox.install(PINNED)
    assert done.returncode == 0, done.stderr
    assert sandbox.manifest() == f"claude-code={PINNED}\n"
    assert sandbox.current() == PINNED
    checked = sandbox.verify()
    assert checked.returncode == 0, checked.stderr


def test_an_unsatisfiable_pin_is_tolerated_and_verify_still_passes(
    sandbox: Sandbox,
) -> None:
    """The entrypoint ignores an installer failure on purpose ("a request for
    a version, not a reason to refuse to boot"); what must also hold is that
    the failure leaves nothing behind for verify to trip over."""
    assert sandbox.install(PINNED).returncode == 0
    failed = sandbox.install("8.8.8")
    assert failed.returncode == 1
    assert "stays on" in failed.stderr
    assert sandbox.manifest() == f"claude-code={PINNED}\n", "state untouched"
    assert sandbox.current() == PINNED
    assert sandbox.verify().returncode == 0


def test_a_manifest_the_active_copy_does_not_match_is_fatal(sandbox: Sandbox) -> None:
    """The installer cannot produce this (it rewrites the manifest from the
    `current` link), so it means something else wrote there — and the pod
    refuses to boot rather than run a CLI the manifest lies about."""
    assert sandbox.install(PINNED).returncode == 0
    (sandbox.root / "manifest").write_text("claude-code=7.7.7\n", encoding="utf-8")
    checked = sandbox.verify()
    assert checked.returncode == 78
    assert "expected pin 7.7.7" in checked.stderr


def test_a_persisted_home_shadow_is_quarantined_and_the_pod_boots(
    sandbox: Sandbox,
) -> None:
    """#196 kept the managed CLI authoritative by refusing to boot when a stray
    copy sat in the persisted home — which took vogt-dev down on 2026-09-05
    after a Codex "Update now" wrote ~/.npm-global/bin/codex. The default now
    quarantines the stray (moved aside, warned) and boots; the managed copy is
    still what PATH resolves."""
    assert sandbox.install(PINNED).returncode == 0
    stray = sandbox.home / ".npm-global" / "bin" / "tool"
    stray.parent.mkdir(parents=True)
    stray.write_text("#!/bin/sh\necho stale\n", encoding="utf-8")
    stray.chmod(0o755)
    quarantined = sandbox.verify()
    assert quarantined.returncode == 0, quarantined.stderr
    assert "quarantined to" in quarantined.stderr
    assert not stray.exists(), "the stray is no longer where it could shadow"
    assert list(stray.parent.glob("tool.shadowed-*")), "moved aside, not deleted"
    # A second boot finds nothing to quarantine and stays clean.
    assert sandbox.verify().returncode == 0


def test_the_strict_shadow_policy_still_refuses_and_warn_still_acknowledges(
    sandbox: Sandbox,
) -> None:
    """`fail` is the old gate, for operators who want a stray to stop the pod;
    `warn` leaves the stray in place and only says so."""
    assert sandbox.install(PINNED).returncode == 0
    stray = sandbox.home / ".npm-global" / "bin" / "tool"
    stray.parent.mkdir(parents=True)
    stray.write_text("#!/bin/sh\necho stale\n", encoding="utf-8")
    stray.chmod(0o755)
    strict = sandbox.verify(VOGT_AGENT_SHADOW_POLICY="fail")
    assert strict.returncode == 78
    assert "would shadow" in strict.stderr
    assert stray.exists(), "fail refuses but touches nothing"
    acknowledged = sandbox.verify(VOGT_AGENT_SHADOW_POLICY="warn")
    assert acknowledged.returncode == 0
    assert "warning" in acknowledged.stderr
    assert stray.exists(), "warn leaves the deliberate override in place"
