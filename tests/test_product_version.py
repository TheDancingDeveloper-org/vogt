from __future__ import annotations

import subprocess
import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]

pytestmark = pytest.mark.skipif(
    not (ROOT / "web/package.json").is_file(),
    reason="product surface manifests are absent from the core-only test image",
)


def test_product_version_contract_is_in_sync() -> None:
    result = subprocess.run(
        [sys.executable, "scripts/check_product_version.py", "0.5.4"],
        cwd=ROOT,
        capture_output=True,
        text=True,
        check=False,
    )
    assert result.returncode == 0, result.stderr or result.stdout


def test_android_version_name_is_derived_from_the_canonical_mobile_manifest() -> None:
    package = (ROOT / "mobile/package.json").read_text(encoding="utf-8")
    gradle = (ROOT / "mobile/android/app/build.gradle").read_text(encoding="utf-8")
    assert '"version": "0.5.4"' in package
    assert "new JsonSlurper().parse(file('../../package.json'))" in gradle
    assert "versionName androidVersionName" in gradle
