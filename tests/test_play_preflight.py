"""Tests for the pure parts of `scripts/play_preflight.py`."""

from __future__ import annotations

import base64
import json
import shutil
import subprocess
from pathlib import Path
from typing import Any

import pytest

from play_preflight import (
    FAIL,
    PASS,
    SKIP,
    WARN,
    Check,
    Manifest,
    PlayHTTPError,
    _openssl_sign,
    check_launcher_icon,
    check_package,
    check_sdk_floors,
    check_signing,
    check_version_code,
    explain_play_error,
    jwt_signing_input,
    max_version_code,
    parse_manifest,
    play_checks,
    play_max_version_code,
    render_summary,
    signer_fingerprints,
)

#: Trimmed from `bundletool dump manifest` on the real v0.7.3 AAB.
MANIFEST_XML = """
<manifest xmlns:android="http://schemas.android.com/apk/res/android"
    android:versionCode="7003" android:versionName="0.7.3"
    package="com.thedancingdeveloper.vogt">
  <uses-sdk android:minSdkVersion="{min_sdk}" android:targetSdkVersion="36"/>
  <application android:icon="@mipmap/ic_launcher" android:label="@string/app_name">
    <activity android:name="com.thedancingdeveloper.vogt.ServerActivity">
      <intent-filter>
        <action android:name="android.intent.action.MAIN"/>
        <category android:name="android.intent.category.LAUNCHER"/>
      </intent-filter>
    </activity>
    <activity android:name="com.thedancingdeveloper.vogt.MainActivity"/>
  </application>
</manifest>
"""

ICON_ENTRIES = [
    "base/manifest/AndroidManifest.xml",
    "base/res/mipmap-hdpi-v4/ic_launcher.png",
    "base/res/mipmap-anydpi-v26/ic_launcher.xml",
    "base/res/mipmap-hdpi-v4/ic_launcher_round.png",
]

FINGERPRINT = (
    "E0:28:A9:EC:A0:20:7A:5B:B2:8E:BE:88:A6:C0:86:69:"
    "C7:92:01:14:DA:72:B3:8E:AC:CE:5E:05:3F:3B:8E:70"
)


def manifest(min_sdk: int = 24) -> Manifest:
    return parse_manifest(MANIFEST_XML.format(min_sdk=min_sdk))


def test_manifest_facts() -> None:
    facts = manifest()
    assert facts == Manifest(
        package="com.thedancingdeveloper.vogt",
        version_code=7003,
        min_sdk=24,
        target_sdk=36,
        icon="@mipmap/ic_launcher",
        has_launcher_activity=True,
    )


def test_absent_target_sdk_means_min_sdk() -> None:
    xml = (
        '<manifest xmlns:android="http://schemas.android.com/apk/res/android" '
        'package="p"><uses-sdk android:minSdkVersion="24"/></manifest>'
    )
    assert parse_manifest(xml).target_sdk == 24


def test_the_v071_bundle_fails_on_min_sdk() -> None:
    """v0.7.1/v0.7.2 shipped minSdk 23; Play refused both."""
    checks = check_sdk_floors(manifest(min_sdk=23), target_floor=36, min_floor=24)
    assert [c.status for c in checks] == [FAIL, PASS]
    assert "minSdk 23 < 24" in checks[0].detail


def test_target_sdk_below_the_floor_fails() -> None:
    checks = check_sdk_floors(manifest(), target_floor=37, min_floor=24)
    assert [c.status for c in checks] == [PASS, FAIL]


def test_package_mismatch_fails() -> None:
    assert check_package(manifest(), "com.thedancingdeveloper.vogt").status == PASS
    assert check_package(manifest(), "com.thedancingdeveloper.vogt.dev").status == FAIL


def test_launcher_icon_must_exist_in_the_bundle() -> None:
    assert check_launcher_icon(manifest(), ICON_ENTRIES).status == PASS
    missing = check_launcher_icon(manifest(), ["base/res/mipmap-hdpi-v4/other.png"])
    assert missing.status == FAIL


def test_launcher_icon_needs_a_launcher_activity_and_an_icon() -> None:
    facts = manifest()
    no_launcher = Manifest(**{**facts.__dict__, "has_launcher_activity": False})
    no_icon = Manifest(**{**facts.__dict__, "icon": None})
    assert check_launcher_icon(no_launcher, ICON_ENTRIES).status == FAIL
    assert check_launcher_icon(no_icon, ICON_ENTRIES).status == FAIL


def test_signer_fingerprints_from_keytool() -> None:
    printed = f"Certificate fingerprints:\n\t SHA1: D8:14\n\t SHA256: {FINGERPRINT}\n"
    assert signer_fingerprints(printed) == [FINGERPRINT.replace(":", "")]


def test_signing_compares_against_the_upload_key() -> None:
    actual = [FINGERPRINT.replace(":", "")]
    assert check_signing(actual, FINGERPRINT.lower()).status == PASS
    assert check_signing(actual, FINGERPRINT.replace(":", "")).status == PASS
    assert check_signing(actual, "AB:CD").status == FAIL
    assert check_signing(actual, "").status == WARN
    assert check_signing(actual, None).status == WARN
    assert check_signing([], FINGERPRINT).status == FAIL


def test_max_version_code_spans_tracks_and_bundles() -> None:
    tracks = {
        "tracks": [
            {"track": "internal", "releases": [{"versionCodes": ["7002"]}]},
            {"track": "production", "releases": [{"status": "draft"}]},
            {"track": "alpha"},
        ]
    }
    bundles = {"bundles": [{"versionCode": 7003}, {"versionCode": 6002}]}
    assert max_version_code(tracks, bundles) == 7003
    assert max_version_code({}, {}) is None


def test_a_reused_version_code_fails() -> None:
    assert check_version_code(6002, 6002).status == FAIL
    assert check_version_code(6001, 6002).status == FAIL
    assert check_version_code(7003, 7002).status == PASS
    assert check_version_code(7003, None).status == PASS
    assert check_version_code(None, 1).status == FAIL


class FakePlay:
    def __init__(self, fail_on: str | None = None, status: int = 500) -> None:
        self.calls: list[tuple[str, str]] = []
        self.fail_on = fail_on
        self.status = status

    def __call__(self, method: str, url: str, body: bytes | None) -> Any:
        self.calls.append((method, url.rsplit("/applications/", 1)[1]))
        if self.fail_on and url.endswith(self.fail_on):
            raise PlayHTTPError(self.status, "nope")
        if method == "POST":
            return {"id": "e1"}
        if url.endswith("/tracks"):
            return {"tracks": [{"releases": [{"versionCodes": ["7002"]}]}]}
        if url.endswith("/bundles"):
            return {"bundles": [{"versionCode": 7001}]}
        return None


def test_the_edit_is_read_and_deleted_never_committed() -> None:
    play = FakePlay()
    assert play_max_version_code(play, "p") == 7002
    assert play.calls == [
        ("POST", "p/edits"),
        ("GET", "p/edits/e1/tracks"),
        ("GET", "p/edits/e1/bundles"),
        ("DELETE", "p/edits/e1"),
    ]
    assert not any(":commit" in url for _m, url in play.calls)


def test_the_edit_is_deleted_even_when_a_read_fails() -> None:
    play = FakePlay(fail_on="/tracks")
    with pytest.raises(PlayHTTPError):
        play_max_version_code(play, "p")
    assert play.calls[-1] == ("DELETE", "p/edits/e1")


def _account(tmp_path: Path) -> Path:
    path = tmp_path / "sa.json"
    path.write_text(json.dumps({"client_email": "ci@x.iam", "private_key": "k"}))
    return path


def test_a_403_is_explained_as_missing_app_access(tmp_path: Path) -> None:
    play = FakePlay(fail_on="/edits", status=403)
    checks = play_checks(manifest(), "p", _account(tmp_path), lambda _a: play)
    assert [c.status for c in checks] == [FAIL]
    assert "Users and permissions" in checks[0].detail


def test_play_checks_pass_and_compare_version_code(tmp_path: Path) -> None:
    checks = play_checks(manifest(), "p", _account(tmp_path), lambda _a: FakePlay())
    assert [(c.name, c.status) for c in checks] == [
        ("Play service-account access", PASS),
        ("versionCode", PASS),
    ]


def test_play_checks_skip_without_a_service_account(tmp_path: Path) -> None:
    assert play_checks(manifest(), "p", None)[0].status == SKIP
    assert play_checks(manifest(), "p", tmp_path / "absent.json")[0].status == SKIP


@pytest.mark.parametrize(
    ("status", "needle"),
    [(401, "rejected"), (403, "no access"), (404, "no app"), (500, "HTTP 500")],
)
def test_play_errors_are_explained(status: int, needle: str) -> None:
    assert needle in explain_play_error(status, "p", "")


def test_jwt_signing_input_claims() -> None:
    signing_input = jwt_signing_input({"client_email": "ci@x.iam"}, now=1000)
    _header, claims = signing_input.split(b".")
    decoded = json.loads(base64.urlsafe_b64decode(claims + b"=" * (-len(claims) % 4)))
    assert decoded == {
        "iss": "ci@x.iam",
        "scope": "https://www.googleapis.com/auth/androidpublisher",
        "aud": "https://oauth2.googleapis.com/token",
        "iat": 1000,
        "exp": 1600,
    }


@pytest.mark.skipif(shutil.which("openssl") is None, reason="needs openssl")
def test_openssl_signature_verifies(tmp_path: Path) -> None:
    key = tmp_path / "key.pem"
    subprocess.run(
        ["openssl", "genpkey", "-algorithm", "RSA", "-out", str(key)],
        check=True,
        capture_output=True,
    )
    pub = tmp_path / "pub.pem"
    subprocess.run(
        ["openssl", "pkey", "-in", str(key), "-pubout", "-out", str(pub)],
        check=True,
        capture_output=True,
    )
    signature = tmp_path / "sig"
    signature.write_bytes(_openssl_sign(key.read_text(), b"payload"))
    data = tmp_path / "data"
    data.write_bytes(b"payload")
    verified = subprocess.run(
        [
            *("openssl", "dgst", "-sha256", "-verify", str(pub)),
            *("-signature", str(signature), str(data)),
        ],
        capture_output=True,
        text=True,
    )
    assert "Verified OK" in verified.stdout


def test_summary_states_the_verdict() -> None:
    passing = render_summary("t", [Check("a", PASS, "ok")])
    assert "All blocking checks passed." in passing
    failing = render_summary("t", [Check("a", FAIL, "x|y"), Check("b", SKIP, "")])
    assert "1 check(s) failed" in failing
    assert "x/y" in failing
