"""Group 1 — health, version identity, and deploy diagnostics."""

from __future__ import annotations

from typing import Any

from ..harness import Case, Check, Context, HttpStatusError, failed, passed, skipped

_EMPTY_IDENTITIES = {"", "local", "dev", "local/dev", "unknown", "none"}

#: The shortest accepted short digest. `--expect-digest f09829df` is a
#: deliberate convenience for a deployed prefix; a one-character "match"
#: would prove nothing.
_MIN_DIGEST_PREFIX = 7


def _digest_matches(actual: str, expected: str) -> tuple[bool, str]:
    """Compare an image digest, accepting a short deployed prefix.

    The engine reports the full `sha256:…` digest. Operators routinely pass
    the short source prefix printed by the rollout (`f09829df`), so compare
    the hex bodies and require an exact match or a prefix of at least seven
    characters. A bare `sha256:` or a shorter prefix is not evidence.
    """
    actual_hex = actual.strip().lower().removeprefix("sha256:")
    expected_hex = expected.strip().lower().removeprefix("sha256:")
    if not actual_hex or not expected_hex:
        return False, "empty digest"
    if actual_hex == expected_hex:
        return True, "exact"
    if len(expected_hex) < _MIN_DIGEST_PREFIX:
        return (
            False,
            f"expected prefix {expected_hex!r} is shorter than {_MIN_DIGEST_PREFIX}",
        )
    if actual_hex.startswith(expected_hex):
        return True, "prefix"
    if len(actual_hex) >= _MIN_DIGEST_PREFIX and expected_hex.startswith(actual_hex):
        return True, "prefix"
    return False, "mismatch"


def _get_json(ctx: Context, path: str) -> dict[str, Any]:
    response = ctx.client.http.request("GET", path)
    if response.status != 200:
        raise HttpStatusError(response.status, "GET", path, response.text()[:200])
    payload = response.json()
    if not isinstance(payload, dict):
        raise HttpStatusError(200, "GET", path, "body is not a JSON object")
    return payload


def healthz(ctx: Context) -> Check:
    payload = _get_json(ctx, "/healthz")
    if payload.get("ok") is not True:
        return failed(f"/healthz answered {payload!r}")
    return passed('/healthz -> 200 {"ok": true}')


def readyz(ctx: Context) -> Check:
    payload = _get_json(ctx, "/readyz")
    checks = payload.get("checks") or []
    fatal_bad = [
        str(row.get("name"))
        for row in checks
        if isinstance(row, dict) and row.get("fatal") and row.get("ok") is not True
    ]
    if payload.get("ok") is not True or fatal_bad:
        return failed(
            f"/readyz ok={payload.get('ok')} failing fatal checks={fatal_bad}",
            checks=checks,
        )
    return passed(f"/readyz -> 200, {len(checks)} checks, no fatal failure")


def status_identity(ctx: Context) -> Check:
    payload = _get_json(ctx, "/api/status")
    fields = {
        name: str(payload.get(name) or "")
        for name in ("source_ref", "source_sha", "version")
    }
    bad = [
        name
        for name, value in fields.items()
        if value.strip().lower() in _EMPTY_IDENTITIES
    ]
    if bad:
        return failed(f"missing/degraded identity: {fields}", **fields)
    if not fields["source_sha"] or len(fields["source_sha"]) < 7:
        return failed(
            f"source_sha is not a commit id: {fields['source_sha']!r}", **fields
        )
    return passed(
        f"source_ref={fields['source_ref']} "
        f"source_sha={fields['source_sha'][:12]} version={fields['version']}",
        **fields,
    )


def diagnostics_engine(ctx: Context) -> Check:
    payload = ctx.client.tool("instance_diagnostics")
    status = payload.get("status")
    checks = payload.get("checks") or []
    engine = [
        row for row in checks if isinstance(row, dict) and row.get("name") == "engine"
    ]
    if not engine:
        return failed(
            f"diagnostics has no engine check: {payload.get('status')!r}", checks=checks
        )
    row = engine[0]
    if row.get("status") != "ok":
        return failed(
            f"engine check is {row.get('status')!r}: {row.get('detail')!r}", check=row
        )
    return passed(
        f"diagnostics={status}; engine check ok ({row.get('detail')!r})", check=row
    )


def diagnostics_digest(ctx: Context) -> Check:
    expected = ctx.options.expect_digest
    if not expected:
        return skipped("--expect-digest was not given")
    payload = ctx.client.tool("instance_diagnostics")
    actual = str(payload.get("image_digest") or "")
    matched, how = _digest_matches(actual, expected)
    if not matched:
        return failed(
            f"image_digest is {actual!r}, expected {expected!r} ({how})",
            actual=actual,
            expected=expected,
        )
    qualifier = "" if how == "exact" else " (prefix match)"
    return passed(
        f"image_digest={actual} matches --expect-digest {expected!r}{qualifier}"
    )


def config_identity(ctx: Context) -> Check:
    payload = _get_json(ctx, "/api/config")
    version = str(payload.get("version") or "")
    product = str(payload.get("product_version") or "")
    source_ref = str(payload.get("source_ref") or "")
    if not version or not product or not source_ref:
        return failed(
            f"/api/config identity incomplete: version={version!r} "
            f"product={product!r} ref={source_ref!r}"
        )
    return passed(
        f"/api/config version={version} product={product} source_ref={source_ref}"
    )


CASES = [
    Case("H1", "health", "O1", "critical", "/healthz answers ok", healthz),
    Case(
        "H2",
        "health",
        "O1",
        "critical",
        "/api/status carries a real source identity",
        status_identity,
    ),
    Case("H3", "health", "O1", "high", "/readyz has no fatal failing check", readyz),
    Case(
        "H4",
        "health",
        "O1",
        "high",
        "instance diagnostics reports the engine healthy",
        diagnostics_engine,
    ),
    Case(
        "H5",
        "health",
        "R1",
        "high",
        "deployed image digest matches --expect-digest",
        diagnostics_digest,
    ),
    Case(
        "H6",
        "health",
        "O1",
        "medium",
        "/api/config exposes the build identity",
        config_identity,
    ),
]
