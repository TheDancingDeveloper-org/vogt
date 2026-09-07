"""The dev and prod shells are separate apps, and stay separate.

Android refuses to install two APKs sharing an `applicationId`, so validating
a mobile change against a dev stack would mean uninstalling the working app
first. Three files have to agree for the two shells to coexist:

1. `mobile/android/app/build.gradle` takes the id from an environment variable.
2. `mobile/capacitor.config.ts` takes it from the same variable.
3. `mobile/android/app/google-services.json` carries a Firebase client entry
   for the dev id, because FCM registration is keyed to the package name — an
   APK whose id has no entry installs and cannot receive a notification.

Nothing in the toolchains asserts they agree: the two build files are read by
different tools, the disagreement appears at install or at first push, and the
symptom (no notifications on the dev build) looks exactly like a broken push
service. These tests are that check.
"""

from __future__ import annotations

import json
import re
import subprocess
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[1]
MOBILE = REPO_ROOT / "mobile"
GRADLE = MOBILE / "android" / "app" / "build.gradle"
CAPACITOR = MOBILE / "capacitor.config.ts"
#: The real ``google-services.json`` is operator-supplied and git-ignored so a
#: live Firebase key stays out of the public tree; the committed,
#: sanitized placeholder is ``google-services.json.example`` and carries the
#: package_name client entries the build assembles under. Prefer the real file
#: when an operator has dropped one in, and fall back to the placeholder — that
#: is what CI and a fresh checkout assemble against.
_SERVICES_REAL = MOBILE / "android" / "app" / "google-services.json"
_SERVICES_EXAMPLE = MOBILE / "android" / "app" / "google-services.json.example"
SERVICES = _SERVICES_REAL if _SERVICES_REAL.is_file() else _SERVICES_EXAMPLE
#: Every workflow that can build an APK: whatever CI builds an APK under has to
#: be an identity FCM knows.
WORKFLOWS = REPO_ROOT / ".github" / "workflows"
CI_WORKFLOW = WORKFLOWS / "ci.yml"
RELEASE_WORKFLOW = WORKFLOWS / "release.yml"
RELEASE_MOBILE_WORKFLOW = WORKFLOWS / "release-mobile.yml"
FIREBASE_WRITE = REPO_ROOT / "scripts" / "write_firebase_config.sh"

#: The variable both build files read. Named once here so a rename shows up as
#: one failure rather than as a silent divergence.
APP_ID_VAR = "VOGT_ANDROID_APP_ID"
DEFAULT_APP_ID = "com.thedancingdeveloper.vogt"

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
    """Push delivery's load-bearing conjunct, and the one with no loud failure.

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
    `ci.yml`'s `android` job is what sets it.
    """
    ci_ids = _ci_app_ids()
    assert ci_ids, "no workflow job sets an application id"
    assert ci_ids != {DEFAULT_APP_ID}, (
        "every APK CI builds carries the prod application id, so a dev build "
        "still cannot install beside prod"
    )


#: Every credential the Android workflows consume is a plain GitHub Actions
#: secret written by ``scripts/write_firebase_config.sh`` or straight from the
#: environment — no secret broker and no CLI, so the jobs run on any
#: self-hosted runner and a fork can supply its own. These markers are what a
#: broker would bring back.
BROKER_MARKERS = ("INFISICAL", "infisical", "fetch_infisical")


def test_ci_android_writes_firebase_from_a_github_secret() -> None:
    """CI's dev APK build reads the dev Firebase config from a plain secret.

    The config is the ``VOGT_FIREBASE_DEV_JSON`` secret written by the generic
    helper, and the built package is matched to the dev id.
    """
    ci = CI_WORKFLOW.read_text(encoding="utf-8")

    assert "secrets.VOGT_FIREBASE_DEV_JSON" in ci
    assert "VOGT_ANDROID_EXPECTED_PACKAGE: com.thedancingdeveloper.vogt.dev" in ci
    assert "scripts/write_firebase_config.sh" in ci
    assert ci.count("remove Firebase config") == 1
    for marker in BROKER_MARKERS:
        assert marker not in ci


def test_release_android_writes_firebase_from_a_github_secret() -> None:
    """The release APK build reads its prod Firebase config the same way.

    The ``VOGT_FIREBASE_PROD_JSON`` secret is written by the generic helper,
    validated to carry the prod Android client, and removed after Gradle
    consumes it. The keystore arrives as the four ``VOGT_ANDROID_KEY*`` secrets.
    """
    release = RELEASE_WORKFLOW.read_text(encoding="utf-8")

    assert "secrets.VOGT_FIREBASE_PROD_JSON" in release
    assert "VOGT_ANDROID_EXPECTED_PACKAGE: com.thedancingdeveloper.vogt" in release
    assert "scripts/write_firebase_config.sh" in release
    assert release.count("remove Firebase config") == 1
    for secret in (
        "VOGT_ANDROID_KEYSTORE_B64",
        "VOGT_ANDROID_KEYSTORE_PASSWORD",
        "VOGT_ANDROID_KEY_ALIAS",
        "VOGT_ANDROID_KEY_PASSWORD",
    ):
        assert f"secrets.{secret}" in release
    for marker in BROKER_MARKERS:
        assert marker not in release


def test_release_mobile_builds_a_gated_play_aab() -> None:
    """The Play pipeline builds a signed AAB and only uploads when armed.

    A release is a `v*` tag. The AAB is signed with the upload key (the same
    keystore secrets the APK job uses) and the Play upload is gated on the
    repo variable VOGT_PLAY_PUBLISH — until it is 'true' the job is a dry run
    that keeps the signed AAB as an artifact and never touches Play. That
    explicit gate is what stops a half-configured pipeline making a bad first
    upload (Play App Signing binds to whatever signs the first upload). The
    service account is the ``VOGT_PLAY_SERVICE_ACCOUNT_JSON`` secret, written
    to the runner's temp directory only when publishing is armed. It builds
    the prod applicationId (the build.gradle default), so it must NOT pin a
    dev/non-prod id here.
    """
    wf = RELEASE_MOBILE_WORKFLOW.read_text(encoding="utf-8")

    assert "tags: ['v*']" in wf
    # Every job self-hosted: the Android SDK is not on a GitHub-hosted runner.
    assert "runs-on: [self-hosted]" in wf
    assert "runs-on: ubuntu-latest" not in wf
    assert "./gradlew bundleRelease" in wf
    assert "--track internal" in wf
    assert "PACKAGE_NAME: com.thedancingdeveloper.vogt" in wf
    # The upload is gated on an explicit repo variable — unset/not 'true' is a
    # dry run with no Play call, both halves asserted.
    assert "vars.VOGT_PLAY_PUBLISH == 'true'" in wf
    assert "vars.VOGT_PLAY_PUBLISH != 'true'" in wf
    # The Play service account is a plain secret, written only when armed.
    assert "secrets.VOGT_PLAY_SERVICE_ACCOUNT_JSON" in wf
    # A store build carries the real prod Firebase, validated to carry the
    # prod Android client.
    assert "secrets.VOGT_FIREBASE_PROD_JSON" in wf
    assert "VOGT_ANDROID_EXPECTED_PACKAGE: com.thedancingdeveloper.vogt" in wf
    assert "scripts/write_firebase_config.sh" in wf
    for marker in BROKER_MARKERS:
        assert marker not in wf
    # It builds under the prod default id — no non-prod override pinned here.
    assert f"{APP_ID_VAR}:" not in wf
    assert wf.count("remove Firebase config") == 1


def test_firebase_writer_never_prints_the_secret_and_checks_package() -> None:
    script = FIREBASE_WRITE.read_text(encoding="utf-8")
    # The value arrives through the environment and is written without echo.
    assert 'printf \'%s\' "$VOGT_FIREBASE_JSON" >"$temp_output"' in script
    assert "json.loads" in script
    assert "VOGT_ANDROID_EXPECTED_PACKAGE" in script
    assert 'mv -- "$temp_output" "$VOGT_FIREBASE_OUTPUT"' in script


def test_live_firebase_config_is_not_tracked() -> None:
    """Firebase credentials stay operator/CI supplied, not repository state."""
    tracked = subprocess.run(
        ["git", "ls-files", "--", str(_SERVICES_REAL.relative_to(REPO_ROOT))],
        cwd=REPO_ROOT,
        check=True,
        capture_output=True,
        text=True,
    ).stdout.splitlines()
    assert not tracked


def test_namespace_matches_the_source_package() -> None:
    """`namespace` is the Java package, and it tracks the app-id family.

    The manifest names its components relatively (`.MainActivity`,
    `.VogtApplication`, ...), so `namespace` must equal the package the source
    actually declares under `com/thedancingdeveloper/vogt/`. The source tree,
    `namespace` and the applicationId default move together, so `namespace`
    and `DEFAULT_APP_ID` share a value. A divergence here means the manifest
    resolves to a class that is not there.
    """
    assert f'namespace "{DEFAULT_APP_ID}"' in GRADLE.read_text(encoding="utf-8")


# ── a touch over a terminal is the page's, never the WebView's ───────────────
#
# Back-scroll did not work in a terminal on the Android app. `touch-action:
# pan-y pinch-zoom` on the terminal host let the WebView start a native pan on
# the first unprevented move and cancel the touch for the scrollers that would
# have worked, and page zoom let a pinch hijack the drag that followed. The
# stylesheet is not applied under jsdom and the Capacitor config is read by no
# JavaScript test, so the two halves the fix depends on are pinned here.

STYLES = REPO_ROOT / "web" / "src" / "styles.css"


def _css_block(css: str, selector: str) -> str:
    # The rule of its own, not the tail of a comma-separated selector list
    # that happens to end with the same selector.
    for match in re.finditer(r"\n" + re.escape(selector) + r" \{", css):
        if css[match.start() - 1] != ",":
            return css[match.start() + 1 : css.index("}", match.end())]
    raise AssertionError(f"no rule for {selector}")


def test_the_terminal_host_leaves_the_browser_no_touch_action() -> None:
    css = STYLES.read_text(encoding="utf-8")
    for selector in (".terminal-host", ".terminal-host .xterm-screen"):
        block = _css_block(css, selector)
        assert re.search(r"touch-action:\s*none", block), selector
        assert "pinch-zoom" not in block, selector


def test_the_android_shell_does_not_zoom_the_page() -> None:
    config = CAPACITOR.read_text(encoding="utf-8")
    assert "zoomEnabled: true" not in config
    assert config.count("zoomEnabled: false") == 2, (
        "both the top-level and the android block must turn page zoom off"
    )
