"""Group 8 — security spot checks that report exposure without changing it."""

from __future__ import annotations

from ..harness import Case, Check, Context, failed, passed


def exposure_report(ctx: Context) -> Check:
    metrics = ctx.client.http.request("GET", "/metrics", token=False)
    status = ctx.client.http.request("GET", "/api/status", token=False)
    config = ctx.client.http.request("GET", "/api/config", token=False)
    facts = {
        "metrics_public": metrics.status == 200,
        "status_unauthenticated": status.status,
        "config_public": config.status == 200,
    }
    if status.status not in (401, 403):
        return failed(f"/api/status answered {status.status} without a bearer", **facts)
    note = (
        f"metrics public={facts['metrics_public']} "
        f"/api/status no-token={status.status} "
        f"/api/config public={facts['config_public']} "
        "(report-only; /metrics exposure is a deferred hardening item)"
    )
    return passed(note, **facts)


def cors_allowlist(ctx: Context) -> Check:
    evil = "https://acceptance.invalid"
    response = ctx.client.http.request(
        "GET", "/healthz", headers={"Origin": evil}, token=False
    )
    allow = response.headers.get("access-control-allow-origin")
    if allow == "*":
        return failed("CORS answers Access-Control-Allow-Origin: *")
    if allow == evil:
        return failed(f"CORS reflects an unlisted origin {evil}")
    preflight = ctx.client.http.request(
        "OPTIONS",
        "/api/status",
        headers={
            "Origin": evil,
            "Access-Control-Request-Method": "GET",
            "Access-Control-Request-Headers": "authorization",
        },
        token=False,
    )
    preflight_allow = preflight.headers.get("access-control-allow-origin")
    if preflight_allow == "*" or preflight_allow == evil:
        return failed(f"CORS preflight allows an unlisted origin: {preflight_allow!r}")
    return passed(
        "evil origin gets no Access-Control-Allow-Origin "
        f"(GET={allow!r}, preflight={preflight_allow!r})"
    )


def response_headers(ctx: Context) -> Check:
    missing: list[str] = []
    present: dict[str, str] = {}
    for path in ("/healthz", "/api/config", "/api/status"):
        response = ctx.client.http.request("GET", path)
        request_id = response.headers.get("x-request-id")
        if not request_id:
            missing.append(f"{path} has no X-Request-Id")
        else:
            present[path] = request_id
    if missing:
        return failed("; ".join(missing))
    security_headers = {
        name: value
        for name, value in ctx.client.http.request("GET", "/api/config").headers.items()
        if name.startswith("x-")
        or name in {"strict-transport-security", "content-security-policy"}
    }
    note = (
        "X-Request-Id on every checked route; other headers "
        f"seen: {sorted(security_headers)}"
    )
    return passed(note, headers=security_headers)


CASES = [
    Case(
        "X1",
        "security",
        "O3",
        "high",
        "public exposure is reported, not silently accepted",
        exposure_report,
    ),
    Case(
        "X2",
        "security",
        "O3",
        "high",
        "CORS does not allow an arbitrary origin",
        cors_allowlist,
    ),
    Case(
        "X3",
        "security",
        "O3",
        "medium",
        "every answer carries a request id",
        response_headers,
    ),
]
