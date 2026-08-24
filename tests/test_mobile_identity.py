"""The dev and prod shells are separate apps, and stay separate (FR-M4).

Android refuses to install two APKs sharing an `applicationId`, so validating
a mobile change against the dev stack meant uninstalling the working app first
— which is why FR-M2's push routing and FR-T5's voice pass both sat unverified
on hardware. The fix is three files agreeing:

1. `mobile/android/app/build.gradle` takes the id from an environment variable.
2. `mobile/capacitor.config.ts` takes it from the same variable.
3. `mobile/android/app/google-services.json` carries a Firebase client entry
   for the dev id, because FCM registration is keyed to the package name — an
   APK whose id has no entry installs and cannot receive a notification.

All three were built and **nothing asserted they agreed**. `build.gradle`
carries the comment "Keep in sync with capacitor.config.ts", which is the
shape of a rule nobody can enforce: the two files are read by different
toolchains, the disagreement appears at install or at first push, and the
symptom (no notifications on the dev build) looks exactly like a broken push
service.

These tests are the check that comment was asking for.
"""

from __future__ import annotations

import json
import re
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[1]
MOBILE = REPO_ROOT / "mobile"
GRADLE = MOBILE / "android" / "app" / "build.gradle"
CAPACITOR = MOBILE / "capacitor.config.ts"
#: The real ``google-services.json`` is operator-supplied and git-ignored so a
#: live Firebase key stays out of the public tree (#265); the committed,
#: sanitized placeholder is ``google-services.json.example`` and carries the
#: package_name client entries the build assembles under. Prefer the real file
#: when an operator has dropped one in, and fall back to the placeholder — that
#: is what CI and a fresh checkout assemble against.
_SERVICES_REAL = MOBILE / "android" / "app" / "google-services.json"
_SERVICES_EXAMPLE = MOBILE / "android" / "app" / "google-services.json.example"
SERVICES = _SERVICES_REAL if _SERVICES_REAL.is_file() else _SERVICES_EXAMPLE
#: Every workflow that can build an APK. Was `engine/.woodpecker/server.yml`
#: until that vendored pipeline was deleted — it was inert here, since
#: Woodpecker builds the pre-merge Forgejo repository and this one is on
#: GitHub, so a test reading it was asserting about a file that never ran.
#: The property it protected is real, so it moved with the builds rather than
#: being dropped: whatever CI builds an APK under has to be an identity FCM
#: knows.
WORKFLOWS = REPO_ROOT / ".github" / "workflows"

#: The variable both build files read. Named once here so a rename shows up as
#: one failure rather than as a silent divergence.
APP_ID_VAR = "MYDEVENV2_ANDROID_APP_ID"
DEFAULT_APP_ID = "com.sprooty.mydevenv2"

pytestmark = pytest.mark.skipif(
    not SERVICES.is_file(),
    reason="the merged tree carries the Android shell; a core-only checkout does not",
)


def _packages() -> set[str]:
    manifest = json.loads(SERVICES.read_text(encoding="utf-8"))
    return {
        client["client_info"]["android_client_info"]["package_name"]
        for client in manifest.get("client", [])
    }


def _ci_app_ids() -> set[str]:
    """Every application id a GitHub Actions job builds an APK under.

    Read out of the workflows rather than named here, because the point is to
    fail when somebody adds a build stream — not when somebody updates a list
    that the builds no longer match. A job that sets nothing builds under
    `DEFAULT_APP_ID`, which the workflow cannot state and the two build files'
    fallbacks do; that case is covered by the test above this one.
    """
    found: set[str] = set()
    for path in sorted(WORKFLOWS.glob("*.yml")):
        text = path.read_text(encoding="utf-8")
        # Both quoting styles YAML allows for a scalar, and the bare form.
        found |= set(re.findall(rf'{APP_ID_VAR}:\s*"([^"\s]+)"', text))
        found |= set(re.findall(rf"{APP_ID_VAR}:\s*'([^'\s]+)'", text))
        found |= set(re.findall(rf"{APP_ID_VAR}:\s*([A-Za-z][\w.]*)\s*$", text, re.M))
    return found


def test_both_build_files_read_the_same_variable() -> None:
    """A comment asking two toolchains to agree is not a mechanism.

    Gradle produces the installed package; Capacitor produces the config the
    WebView and the push plugin are built against. If they diverge, the APK
    installs under one id and registers for push under another, and nothing
    fails until a notification does not arrive.
    """
    assert APP_ID_VAR in GRADLE.read_text(encoding="utf-8")
    assert APP_ID_VAR in CAPACITOR.read_text(encoding="utf-8")


def test_both_build_files_fall_back_to_the_same_id() -> None:
    """An unset variable must mean prod in both places, not prod in one.

    This is the failure that would ship silently: a local build with no
    variable set producing a Capacitor config for one app and a Gradle package
    for another.
    """
    gradle_default = re.search(
        rf"getenv\('{APP_ID_VAR}'\)\s*\?:\s*'([^']+)'", GRADLE.read_text("utf-8")
    )
    capacitor_default = re.search(
        rf'process\.env\.{APP_ID_VAR}\s*\|\|\s*"([^"]+)"', CAPACITOR.read_text("utf-8")
    )
    assert gradle_default, "gradle's fallback application id was not found"
    assert capacitor_default, "capacitor's fallback application id was not found"
    assert gradle_default.group(1) == capacitor_default.group(1) == DEFAULT_APP_ID


def test_every_application_id_ci_builds_can_receive_push() -> None:
    """FR-M4's load-bearing conjunct, and the one with no loud failure.

    `google-services.json` is keyed by package name. An APK built under an id
    with no client entry installs happily, runs happily, and silently cannot
    register for FCM — so the dev build looks like a push outage rather than a
    misconfiguration. Asserted against what the workflows actually set, so
    adding a third build stream without a Firebase entry fails here rather
    than on somebody's phone.
    """
    packages = _packages()
    assert DEFAULT_APP_ID in packages, "prod has no Firebase client entry"

    for app_id in _ci_app_ids():
        assert app_id in packages, (
            f"CI builds an APK as {app_id}, which has no client entry in "
            "google-services.json — it would install and never receive a push"
        )


def test_the_dev_stream_builds_under_its_own_id() -> None:
    """Two APKs side by side is the whole point of the requirement.

    Asserted rather than assumed because a workflow could set the variable to
    the prod id and everything above would still pass — the ids would agree,
    the entry would exist, and the two builds would still refuse to coexist.

    This is the assertion that caught the deletion of the vendored Woodpecker
    pipeline: it was the only file in the tree that set the variable, and
    removing it left the id built by nothing. `ci.yml`'s `android` job sets it
    now.
    """
    ci_ids = _ci_app_ids()
    assert ci_ids, "no workflow job sets an application id"
    assert ci_ids != {DEFAULT_APP_ID}, (
        "every APK CI builds carries the prod application id, so a dev build "
        "still cannot install beside prod"
    )


def test_the_source_package_stays_put() -> None:
    """`namespace` is the Java package, not the app identity.

    Moving it alongside `applicationId` would rename `MainActivity` and break
    the manifest, and it is the obvious wrong fix for somebody making dev and
    prod distinct. The comment in `build.gradle` says so; this is the check.
    """
    assert f'namespace "{DEFAULT_APP_ID}"' in GRADLE.read_text(encoding="utf-8")
