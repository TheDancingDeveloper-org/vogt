"""Check a signed Android App Bundle against what Google Play will reject.

Run by `release-mobile.yml` and `release-mobile-dev.yml` after the AAB is
built and before `fastlane supply` uploads it, so a bundle Play would refuse
fails the job here, with a summary saying why, instead of as a rejection
e-mail nobody reads. Every check this makes has cost a release:

- **minSdk / targetSdk floors** — v0.7.1 and v0.7.2 went up with minSdk 23
  and Play's automatic protection refused both; nobody noticed for two
  releases.
- **launcher icon** — the application must name an icon that the bundle
  actually carries.
- **signing certificate** — the bundle must be signed by the registered
  upload key; a different key is a different app identity to Play.
- **versionCode** — Play refuses any code it has already seen (6002 was
  reused on 2026-09-10). Compared against every track *and* every bundle the
  app has ever had uploaded, via an androidpublisher edit that is inserted,
  read and always deleted — never committed.
- **service-account access** — the same edit call proves the account can
  act on this app; a 403 is reported as the missing Play Console grant.

The bundle checks run on every build, dry run included. The Play checks run
only when a service-account file is given, i.e. when upload is armed.

Standard library plus the tools a signing job already has: `java` (for
bundletool, downloaded once at a pinned version and sha256), `keytool`, and
`openssl` to sign the service-account JWT.
"""

from __future__ import annotations

import argparse
import base64
import hashlib
import json
import os
import re
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request
import zipfile
from collections.abc import Callable, Iterable, Sequence
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Protocol
from xml.etree import ElementTree

#: Google Play's target-API floor for new apps and app updates. It moves every
#: August: from 2025-08-31 it was 35, from 2026-08-31 it is 36 (Android 16).
#: Bump it here, once a year, when Play announces the next one.
PLAY_TARGET_SDK_FLOOR = 36

#: Play's automatic app protection refuses a bundle whose minSdk is below 24
#: (the v0.7.1/v0.7.2 rejections). `variables.gradle` carries 24 since v0.7.3.
MIN_SDK_FLOOR = 24

BUNDLETOOL_VERSION = "1.18.3"
BUNDLETOOL_URL = (
    "https://github.com/google/bundletool/releases/download/"
    f"{BUNDLETOOL_VERSION}/bundletool-all-{BUNDLETOOL_VERSION}.jar"
)
BUNDLETOOL_SHA256 = "a099cfa1543f55593bc2ed16a70a7c67fe54b1747bb7301f37fdfd6d91028e29"

PLAY_API = "https://androidpublisher.googleapis.com/androidpublisher/v3"
PLAY_SCOPE = "https://www.googleapis.com/auth/androidpublisher"
DEFAULT_TOKEN_URI = "https://oauth2.googleapis.com/token"

ANDROID_NS = "{http://schemas.android.com/apk/res/android}"
_ICON_REF = re.compile(r"^@(?:[\w.]+:)?(?P<type>[a-z]+)/(?P<name>\w+)$")
_FINGERPRINT = re.compile(r"SHA-?256:\s*([0-9A-Fa-f:]{64,95})")

PASS, FAIL, WARN, SKIP = "pass", "fail", "warn", "skip"


@dataclass(frozen=True)
class Check:
    name: str
    status: str
    detail: str


@dataclass(frozen=True)
class Manifest:
    package: str | None
    version_code: int | None
    min_sdk: int | None
    target_sdk: int | None
    icon: str | None
    has_launcher_activity: bool


# --------------------------------------------------------------------------
# Pure checks
# --------------------------------------------------------------------------


def _int(value: str | None) -> int | None:
    try:
        return int(value) if value is not None else None
    except ValueError:
        return None


def parse_manifest(xml_text: str) -> Manifest:
    """Facts from `bundletool dump manifest` output."""
    root = ElementTree.fromstring(xml_text.strip())
    uses_sdk = root.find("uses-sdk")
    min_sdk = target_sdk = None
    if uses_sdk is not None:
        min_sdk = _int(uses_sdk.get(f"{ANDROID_NS}minSdkVersion"))
        target_sdk = _int(uses_sdk.get(f"{ANDROID_NS}targetSdkVersion"))
    application = root.find("application")
    icon = None
    launcher = False
    if application is not None:
        icon = application.get(f"{ANDROID_NS}icon")
        for component in [
            *application.findall("activity"),
            *application.findall("activity-alias"),
        ]:
            for intent_filter in component.findall("intent-filter"):
                actions = {
                    a.get(f"{ANDROID_NS}name") for a in intent_filter.findall("action")
                }
                categories = {
                    c.get(f"{ANDROID_NS}name")
                    for c in intent_filter.findall("category")
                }
                if (
                    "android.intent.action.MAIN" in actions
                    and "android.intent.category.LAUNCHER" in categories
                ):
                    launcher = True
    return Manifest(
        package=root.get("package"),
        version_code=_int(root.get(f"{ANDROID_NS}versionCode")),
        min_sdk=min_sdk,
        # Android's own rule: an absent targetSdkVersion means minSdkVersion.
        target_sdk=target_sdk if target_sdk is not None else min_sdk,
        icon=icon,
        has_launcher_activity=launcher,
    )


def check_package(manifest: Manifest, expected: str) -> Check:
    if manifest.package == expected:
        return Check("package", PASS, expected)
    return Check(
        "package",
        FAIL,
        f"bundle is `{manifest.package}`, the workflow uploads to `{expected}`",
    )


def check_sdk_floors(
    manifest: Manifest, target_floor: int, min_floor: int
) -> list[Check]:
    out = []
    if manifest.min_sdk is None:
        out.append(Check("minSdk", FAIL, "the manifest declares no minSdkVersion"))
    elif manifest.min_sdk < min_floor:
        out.append(
            Check(
                "minSdk",
                FAIL,
                f"minSdk {manifest.min_sdk} < {min_floor}: Play automatic "
                "protection rejects it (raise `minSdkVersion` in "
                "mobile/android/variables.gradle)",
            )
        )
    else:
        out.append(Check("minSdk", PASS, f"{manifest.min_sdk} ≥ {min_floor}"))
    if manifest.target_sdk is None:
        out.append(Check("targetSdk", FAIL, "the manifest declares no targetSdk"))
    elif manifest.target_sdk < target_floor:
        out.append(
            Check(
                "targetSdk",
                FAIL,
                f"targetSdk {manifest.target_sdk} < Play's floor {target_floor} "
                "(raise `targetSdkVersion` in mobile/android/variables.gradle)",
            )
        )
    else:
        out.append(Check("targetSdk", PASS, f"{manifest.target_sdk} ≥ {target_floor}"))
    return out


def check_launcher_icon(manifest: Manifest, bundle_entries: Iterable[str]) -> Check:
    if not manifest.has_launcher_activity:
        return Check("launcher icon", FAIL, "no MAIN/LAUNCHER activity")
    if not manifest.icon:
        return Check("launcher icon", FAIL, "<application> declares no android:icon")
    match = _ICON_REF.match(manifest.icon)
    if not match:
        # A compiled reference (`@0x7f…`) cannot be resolved by name here; the
        # attribute being present is what this check can honestly claim.
        return Check("launcher icon", PASS, f"declared as {manifest.icon}")
    pattern = re.compile(
        rf"^base/res/{match['type']}(?:-[^/]+)?/{match['name']}\.(?:png|webp|xml)$"
    )
    found = sorted(e for e in bundle_entries if pattern.match(e))
    if not found:
        return Check(
            "launcher icon",
            FAIL,
            f"{manifest.icon} is declared but the bundle carries no such resource",
        )
    return Check("launcher icon", PASS, f"{manifest.icon} ({len(found)} densities)")


def normalise_fingerprint(value: str) -> str:
    return re.sub(r"[^0-9A-F]", "", value.upper())


def signer_fingerprints(keytool_output: str) -> list[str]:
    """SHA-256 fingerprints from `keytool -printcert -jarfile` output."""
    return [normalise_fingerprint(m) for m in _FINGERPRINT.findall(keytool_output)]


def check_signing(actual: Sequence[str], expected: str | None) -> Check:
    if not actual:
        return Check("signing certificate", FAIL, "the bundle carries no signer")
    shown = ", ".join(_colons(f) for f in actual)
    if not expected or not normalise_fingerprint(expected):
        return Check(
            "signing certificate",
            WARN,
            f"signed by {shown}; no expected upload-key fingerprint is configured, "
            "so the signing identity was not compared",
        )
    want = normalise_fingerprint(expected)
    if want in actual:
        return Check("signing certificate", PASS, f"upload key {_colons(want)}")
    return Check(
        "signing certificate",
        FAIL,
        f"signed by {shown}, expected the registered upload key {_colons(want)}",
    )


def _colons(fingerprint: str) -> str:
    return ":".join(fingerprint[i : i + 2] for i in range(0, len(fingerprint), 2))


def max_version_code(tracks: dict[str, Any], bundles: dict[str, Any]) -> int | None:
    """The highest versionCode Play has seen, from edits.tracks/bundles.list."""
    codes: list[int] = []
    for track in tracks.get("tracks", []) or []:
        for release in track.get("releases", []) or []:
            codes += [int(c) for c in release.get("versionCodes", []) or []]
    for bundle in bundles.get("bundles", []) or []:
        if "versionCode" in bundle:
            codes.append(int(bundle["versionCode"]))
    return max(codes) if codes else None


def check_version_code(local: int | None, remote_max: int | None) -> Check:
    if local is None:
        return Check("versionCode", FAIL, "the manifest declares no versionCode")
    if remote_max is None:
        return Check("versionCode", PASS, f"{local}; Play has no earlier bundle")
    if local > remote_max:
        return Check("versionCode", PASS, f"{local} > {remote_max} (highest on Play)")
    return Check(
        "versionCode",
        FAIL,
        f"{local} ≤ {remote_max}, the highest code Play already has; Play "
        "refuses a reused or lower versionCode (bump the product version)",
    )


def explain_play_error(status: int, package: str, message: str) -> str:
    detail = f" Google said: {message}" if message else ""
    if status == 401:
        return (
            "the Play service account's credentials were rejected (401); "
            f"rotate VOGT_PLAY_SERVICE_ACCOUNT_JSON.{detail}"
        )
    if status == 403:
        return (
            f"the Play service account has no access to `{package}` (403). In Play "
            "Console → Users and permissions, grant the account release rights "
            "for this app, and check the Google Play Android Developer API is "
            f"enabled in its Cloud project.{detail}"
        )
    if status == 404:
        return (
            f"Play has no app `{package}` (404). Create the app in Play Console; "
            f"its first bundle must be uploaded there by hand.{detail}"
        )
    return f"Play API returned HTTP {status} for `{package}`.{detail}"


def render_summary(title: str, checks: Sequence[Check]) -> str:
    icon = {PASS: "✅", FAIL: "❌", WARN: "⚠️", SKIP: "⏭️"}
    failed = [c for c in checks if c.status == FAIL]
    verdict = (
        f"**{len(failed)} check(s) failed — the bundle was NOT uploaded.**"
        if failed
        else "All blocking checks passed."
    )
    rows = "\n".join(
        f"| {icon[c.status]} | {c.name} | {c.detail.replace('|', '/')} |"
        for c in checks
    )
    return f"## {title}\n\n{verdict}\n\n| | Check | Detail |\n|---|---|---|\n{rows}\n"


# --------------------------------------------------------------------------
# Google Play: an edit that is always deleted, never committed
# --------------------------------------------------------------------------


class PlayHTTPError(Exception):
    def __init__(self, status: int, message: str) -> None:
        super().__init__(f"HTTP {status}: {message}")
        self.status = status
        self.message = message


class PlayTransport(Protocol):
    def __call__(
        self, method: str, url: str, body: bytes | None
    ) -> dict[str, Any] | None: ...


def play_max_version_code(transport: PlayTransport, package: str) -> int | None:
    """Insert an edit, read tracks and bundles, and delete the edit.

    The delete is in a `finally`: an edit left open blocks the next one
    (fastlane's upload included) until it expires. Nothing here commits.
    """
    base = f"{PLAY_API}/applications/{urllib.parse.quote(package)}/edits"
    edit = transport("POST", base, b"{}")
    if not edit or "id" not in edit:
        raise PlayHTTPError(0, "edits.insert returned no edit id")
    edit_id = urllib.parse.quote(str(edit["id"]))
    try:
        tracks = transport("GET", f"{base}/{edit_id}/tracks", None)
        bundles = transport("GET", f"{base}/{edit_id}/bundles", None)
    finally:
        try:
            transport("DELETE", f"{base}/{edit_id}", None)
        except PlayHTTPError as exc:  # never mask the real outcome
            print(f"::warning::could not delete Play edit {edit_id}: {exc}")
    return max_version_code(tracks or {}, bundles or {})


def _b64url(data: bytes) -> str:
    return base64.urlsafe_b64encode(data).rstrip(b"=").decode("ascii")


def jwt_signing_input(account: dict[str, Any], now: int) -> bytes:
    header = {"alg": "RS256", "typ": "JWT"}
    claims = {
        "iss": account["client_email"],
        "scope": PLAY_SCOPE,
        "aud": account.get("token_uri") or DEFAULT_TOKEN_URI,
        "iat": now,
        "exp": now + 600,
    }
    return (
        f"{_b64url(json.dumps(header).encode())}.{_b64url(json.dumps(claims).encode())}"
    ).encode("ascii")


def _openssl_sign(private_key_pem: str, data: bytes) -> bytes:
    # The key goes to a file only for openssl to read, in an owner-only
    # temporary directory removed on the way out.
    with tempfile.TemporaryDirectory() as directory:
        key = Path(directory) / "key.pem"
        key.touch(mode=0o600)
        key.write_text(private_key_pem, encoding="utf-8")
        return subprocess.run(
            ["openssl", "dgst", "-sha256", "-sign", str(key)],
            input=data,
            capture_output=True,
            check=True,
        ).stdout


def access_token(
    account: dict[str, Any],
    sign: Callable[[str, bytes], bytes] = _openssl_sign,
    now: Callable[[], float] = time.time,
) -> str:
    signing_input = jwt_signing_input(account, int(now()))
    assertion = (
        signing_input
        + b"."
        + _b64url(sign(account["private_key"], signing_input)).encode("ascii")
    )
    token_uri = account.get("token_uri") or DEFAULT_TOKEN_URI
    if not token_uri.startswith("https://"):
        raise ValueError("the service account's token_uri is not https")
    form = urllib.parse.urlencode(
        {
            "grant_type": "urn:ietf:params:oauth:grant-type:jwt-bearer",
            "assertion": assertion.decode("ascii"),
        }
    ).encode("ascii")
    request = urllib.request.Request(token_uri, data=form, method="POST")
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            return str(json.load(response)["access_token"])
    except urllib.error.HTTPError as exc:
        raise PlayHTTPError(exc.code, _google_message(exc)) from None


def _google_message(exc: urllib.error.HTTPError) -> str:
    try:
        body = json.loads(exc.read() or b"{}")
    except ValueError:
        return ""
    error = body.get("error")
    if isinstance(error, dict):
        return str(error.get("message", ""))
    return str(body.get("error_description") or error or "")


def urllib_transport(token: str) -> PlayTransport:
    def call(method: str, url: str, body: bytes | None) -> dict[str, Any] | None:
        request = urllib.request.Request(url, data=body, method=method)
        request.add_header("Authorization", f"Bearer {token}")
        if body is not None:
            request.add_header("Content-Type", "application/json")
        try:
            with urllib.request.urlopen(request, timeout=60) as response:
                raw = response.read()
        except urllib.error.HTTPError as exc:
            raise PlayHTTPError(exc.code, _google_message(exc)) from None
        parsed = json.loads(raw) if raw else None
        return parsed if isinstance(parsed, dict) else None

    return call


def play_checks(
    manifest: Manifest,
    package: str,
    service_account: Path | None,
    transport_factory: Callable[[dict[str, Any]], PlayTransport] | None = None,
) -> list[Check]:
    if service_account is None or not service_account.is_file():
        return [
            Check(
                "Play access + versionCode",
                SKIP,
                "no service account (publishing not armed); not compared with Play",
            )
        ]
    account = json.loads(service_account.read_text(encoding="utf-8"))
    factory = transport_factory or (lambda a: urllib_transport(access_token(a)))
    try:
        remote_max = play_max_version_code(factory(account), package)
    except PlayHTTPError as exc:
        return [
            Check(
                "Play service-account access",
                FAIL,
                explain_play_error(exc.status, package, exc.message),
            )
        ]
    except (OSError, ValueError, KeyError, subprocess.CalledProcessError) as exc:
        return [
            Check(
                "Play service-account access",
                FAIL,
                f"could not reach the Play API: {type(exc).__name__}: {exc}",
            )
        ]
    return [
        Check(
            "Play service-account access",
            PASS,
            f"{account.get('client_email', 'service account')} can edit `{package}`",
        ),
        check_version_code(manifest.version_code, remote_max),
    ]


# --------------------------------------------------------------------------
# Tools
# --------------------------------------------------------------------------


def ensure_bundletool(cache_dir: Path) -> Path:
    jar = cache_dir / f"bundletool-all-{BUNDLETOOL_VERSION}.jar"
    if jar.is_file() and _sha256(jar) == BUNDLETOOL_SHA256:
        return jar
    cache_dir.mkdir(parents=True, exist_ok=True)
    partial = jar.with_suffix(".part")
    with urllib.request.urlopen(BUNDLETOOL_URL, timeout=120) as response:
        partial.write_bytes(response.read())
    digest = _sha256(partial)
    if digest != BUNDLETOOL_SHA256:
        partial.unlink()
        raise RuntimeError(
            f"bundletool {BUNDLETOOL_VERSION} sha256 {digest} does not match "
            f"the pinned {BUNDLETOOL_SHA256}"
        )
    partial.replace(jar)
    return jar


def _sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def _run(argv: Sequence[str]) -> str:
    return subprocess.run(argv, capture_output=True, text=True, check=True).stdout


def bundle_checks(
    aab: Path,
    package: str,
    bundletool: Path,
    target_floor: int,
    min_floor: int,
    expected_cert: str | None,
) -> tuple[Manifest | None, list[Check]]:
    try:
        manifest_xml = _run(
            ["java", "-jar", str(bundletool), "dump", "manifest", "--bundle", str(aab)]
        )
        manifest = parse_manifest(manifest_xml)
    except (subprocess.CalledProcessError, ElementTree.ParseError) as exc:
        return None, [Check("manifest", FAIL, f"could not read the manifest: {exc}")]
    with zipfile.ZipFile(aab) as bundle:
        entries = bundle.namelist()
    checks = [
        check_package(manifest, package),
        *check_sdk_floors(manifest, target_floor, min_floor),
        check_launcher_icon(manifest, entries),
    ]
    try:
        printed = _run(["keytool", "-printcert", "-jarfile", str(aab)])
        checks.append(check_signing(signer_fingerprints(printed), expected_cert))
    except subprocess.CalledProcessError as exc:
        checks.append(
            Check("signing certificate", FAIL, f"keytool failed: {exc.stderr}")
        )
    return manifest, checks


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--aab", type=Path, required=True)
    parser.add_argument("--package", required=True)
    parser.add_argument("--target-sdk-floor", type=int, default=PLAY_TARGET_SDK_FLOOR)
    parser.add_argument("--min-sdk-floor", type=int, default=MIN_SDK_FLOOR)
    parser.add_argument(
        "--expected-cert-sha256",
        default=os.environ.get("PLAY_UPLOAD_CERT_SHA256", ""),
        help="the registered upload key's SHA-256 (colons optional); "
        "empty skips the comparison with a warning",
    )
    parser.add_argument(
        "--service-account",
        type=Path,
        help="Play service-account JSON; absent skips the Play API checks",
    )
    parser.add_argument("--bundletool", type=Path, help="bundletool jar to use")
    parser.add_argument(
        "--cache-dir",
        type=Path,
        default=Path(os.environ.get("RUNNER_TEMP", tempfile.gettempdir())),
    )
    parser.add_argument("--summary", default=os.environ.get("GITHUB_STEP_SUMMARY"))
    parser.add_argument("--title", default="Play preflight")
    args = parser.parse_args(argv)

    bundletool = args.bundletool or ensure_bundletool(args.cache_dir)
    manifest, checks = bundle_checks(
        args.aab,
        args.package,
        bundletool,
        args.target_sdk_floor,
        args.min_sdk_floor,
        args.expected_cert_sha256,
    )
    if manifest is not None:
        checks += play_checks(manifest, args.package, args.service_account)

    for check in checks:
        prefix = {FAIL: "::error::", WARN: "::warning::"}.get(check.status, "")
        print(f"{prefix}{check.name}: {check.status} — {check.detail}")
    summary = render_summary(f"{args.title}: `{args.package}`", checks)
    if args.summary:
        with Path(args.summary).open("a", encoding="utf-8") as handle:
            handle.write(summary)
    return 1 if any(c.status == FAIL for c in checks) else 0


if __name__ == "__main__":
    sys.exit(main())
