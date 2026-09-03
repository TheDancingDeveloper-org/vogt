"""One user-facing Vogt identity, with legacy compatibility names contained."""

from __future__ import annotations

import json
import re
import struct
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
WEB = ROOT / "web"

pytestmark = pytest.mark.skipif(
    not WEB.is_dir(),
    reason=(
        "the merged tree carries presentation identity; a core-only checkout does not"
    ),
)

VISIBLE_WEB_FILES = (
    WEB / "index.html",
    WEB / "public" / "manifest.webmanifest",
    WEB / "public" / "sw.js",
    WEB / "src" / "App.tsx",
    WEB / "src" / "Settings.tsx",
    WEB / "src" / "TemplateEditor.tsx",
    WEB / "src" / "index.tsx",
)


def test_static_user_facing_surfaces_do_not_regress_to_legacy_branding() -> None:
    offenders: list[str] = []
    compatibility_literals = {
        WEB / "public" / "sw.js": ('"mydevenv2-default"',),
        # `viewport-resize` and `terminal-*` events were renamed to `vogt:*`
        # (#271); only `native-insets` keeps the historic name, because the
        # human-gated Android shell still dispatches it (renamed under #265).
        WEB / "src" / "index.tsx": ('"mydevenv2:native-insets"',),
    }
    for path in VISIBLE_WEB_FILES:
        text = path.read_text(encoding="utf-8")
        for literal in compatibility_literals.get(path, ()):
            text = text.replace(literal, "")
        if re.search(r"mydevenv2", text, re.I):
            offenders.append(path.relative_to(ROOT).as_posix())
    assert not offenders, f"legacy user-facing branding returned in {offenders}"


def test_manifest_keeps_the_existing_install_identity_while_renaming_it() -> None:
    manifest = json.loads((WEB / "public" / "manifest.webmanifest").read_text())
    assert manifest["id"] == manifest["start_url"] == manifest["scope"] == "/"
    assert manifest["name"] == manifest["short_name"] == "Vogt"
    assert "product development environment" in manifest["description"]


def test_install_and_notification_icons_have_declared_real_dimensions() -> None:
    manifest = json.loads((WEB / "public" / "manifest.webmanifest").read_text())
    pngs = [icon for icon in manifest["icons"] if icon["type"] == "image/png"]
    assert {icon["purpose"] for icon in pngs} == {"any", "maskable"}
    for icon in pngs:
        width, height = map(int, icon["sizes"].split("x", 1))
        data = (WEB / "public" / icon["src"].removeprefix("/")).read_bytes()
        assert data.startswith(b"\x89PNG\r\n\x1a\n")
        assert struct.unpack(">II", data[16:24]) == (width, height)

    service_worker = (WEB / "public" / "sw.js").read_text()
    assert 'icon: "/icon-192.png"' in service_worker
    assert 'badge: "/notification-badge.svg"' in service_worker


def test_service_worker_upgrade_is_eager_and_drops_stale_brand_assets() -> None:
    client = (WEB / "src" / "push.ts").read_text()
    service_worker = (WEB / "public" / "sw.js").read_text()

    assert 'updateViaCache: "none"' in client
    assert "await reg.update()" in client
    assert "self.skipWaiting()" in service_worker
    assert "await self.clients.claim()" in service_worker
    assert "caches.delete(key)" in service_worker


def test_push_metadata_uses_vogt_labels_with_the_stable_android_channel() -> None:
    push = (WEB / "src" / "push.ts").read_text()
    assert 'id: "mydevenv2-alerts"' in push
    assert 'name: "Vogt alerts"' in push
    assert 'description: "Session, work, and assistant alerts from Vogt"' in push
    assert 'pushTest: (title = "Vogt test"' in (WEB / "src" / "api.ts").read_text()
    assert (
        '"Vogt digest".to_string()'
        in (ROOT / "engine" / "server" / "src" / "push.rs").read_text()
    )
    assert (
        '"Tap to open the session in Vogt."'
        in (ROOT / "engine" / "server" / "src" / "push_api.rs").read_text()
    )


def test_compatibility_names_remain_explicitly_stable() -> None:
    """Renaming these values would lose state or make an Android sibling app.

    Browser-storage keys are NOT in this set any more: #271 renamed them to
    `vogt.*` with a one-shot migration (`web/src/storageMigration.ts`), so state
    is carried across the rename rather than pinned to the historic name. The
    Android *applicationId* is no longer in this set either: #271's mobile half
    renamed it (to `com.sprooty.vogt`), and it later moved to
    `com.thedancingdeveloper.vogt` for the owned domain — each rename accepting
    the cost it carries (a new app — users reinstall, no in-place upgrade — and
    FCM re-registration against the new id). The names that remain are the ones a
    rename could not migrate at all — an OS-level Android notification channel,
    an IndexedDB database, and a wire prefix a client may still emit.
    """
    api = (WEB / "src" / "api.ts").read_text()
    push = (WEB / "src" / "push.ts").read_text()
    capacitor = (ROOT / "mobile" / "capacitor.config.ts").read_text()
    android = ROOT / "mobile" / "android" / "app" / "src" / "main"
    agent_tasks = (ROOT / "engine" / "server" / "src" / "agent_tasks.rs").read_text()

    # The credential key was renamed to `vogt.token`; the migration preserves the
    # session, so nobody is signed out (#271).
    assert 'const TOKEN_KEY = "vogt.token"' in api
    assert 'id: "mydevenv2-alerts"' in push
    # The Android applicationId moved to `com.thedancingdeveloper.vogt` (owned
    # domain; #271 first took it to com.sprooty.vogt). Unlike the channel id
    # above, this one accepts the migration cost rather than staying stable: the
    # operator adds the new id to Firebase and users reinstall.
    assert '"com.thedancingdeveloper.vogt"' in capacitor
    # The default notify prefix was renamed to `VOGT_NOTIFY:` (#203), but the
    # legacy `MYDEVENV2_NOTIFY:` must remain an accepted prefix so existing task
    # definitions and any client still emitting it keep working.
    assert 'DEFAULT_NOTIFY_PHRASE: &str = "VOGT_NOTIFY:"' in agent_tasks
    assert 'LEGACY_NOTIFY_PHRASE: &str = "MYDEVENV2_NOTIFY:"' in agent_tasks
    assert (
        "#7EE787"
        in (android / "res" / "drawable-v24" / "ic_launcher_foreground.xml").read_text()
    )
    assert (
        "#0D1117"
        in (android / "res" / "values" / "ic_launcher_background.xml").read_text()
    )
