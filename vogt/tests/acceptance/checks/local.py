"""Group 0 — the suite's own plumbing, proved without a live instance."""

from __future__ import annotations

from ..harness import (
    Case,
    Check,
    Context,
    Options,
    failed,
    passed,
    redact,
    run_shared_cleanup,
)
from ..policy import (
    SESSION_MODEL,
    SESSION_PROJECT,
    PolicyRefusal,
    session_start_arguments,
    validate_base_url,
    validate_instance,
)


def prod_url_refused(ctx: Context) -> Check:
    refusals: list[str] = []
    for url in ("https://vogt.sprooty.com", "https://example.invalid"):
        try:
            validate_base_url(url)
        except PolicyRefusal as exc:
            refusals.append(str(exc))
        else:
            return failed(f"{url} was not refused")
    try:
        validate_instance(instance_id="prod-1", prod_instance_id="prod-1")
    except PolicyRefusal as exc:
        refusals.append(str(exc))
    else:
        return failed("a target with the production instance id was not refused")
    return passed(
        "prod host, unknown host and prod instance id all refused "
        f"({len(refusals)} rules)"
    )


def session_arguments_pinned(ctx: Context) -> Check:
    arguments = session_start_arguments("acceptance-test", task="x")
    expected = {
        "project": SESSION_PROJECT,
        "placement": "pod",
        "worktree": False,
        "model": SESSION_MODEL,
    }
    wrong = {
        key: arguments.get(key)
        for key, value in expected.items()
        if arguments.get(key) != value
    }
    if wrong:
        return failed(f"throwaway session arguments are not pinned: {wrong}")
    for forbidden in ("placement",):
        if arguments.get(forbidden) == "host":
            return failed("a throwaway session could be placed on the host")
    if "acceptance-test" not in str(arguments.get("reason")):
        return failed("the session reason does not carry the run tag")
    return passed(f"pinned {expected} and a tagged reason")


class _RecordingClient:
    def __init__(self) -> None:
        self.secrets: tuple[str, ...] = ()
        self.stopped: list[str] = []

    def tool(
        self, name: str, arguments: dict[str, object] | None = None
    ) -> dict[str, object]:
        if name == "session_stop" and arguments:
            self.stopped.append(str(arguments.get("id")))
        return {}


def cleanup_runs_after_failure(ctx: Context) -> Check:
    client = _RecordingClient()
    local = Context(client, Options(fake=True), "acceptance-cleanup-test")
    order: list[str] = []
    local.on_cleanup("first", lambda: order.append("first"))
    local.on_cleanup("second", lambda: order.append("second"))
    local.track_session("ses_fake_cleanup")
    try:
        raise RuntimeError("the case failed after registering cleanup")
    except RuntimeError:
        pass
    problems = local.run_cleanup()
    if problems:
        return failed(f"cleanup reported problems: {problems}")
    if order != ["second", "first"]:
        return failed(f"cleanup did not run in reverse order: {order}")
    if client.stopped:
        return failed(
            f"the case cleanup stopped a run-scoped session: {client.stopped}"
        )
    shared = run_shared_cleanup()
    if shared:
        return failed(f"shared cleanup reported problems: {shared}")
    if client.stopped != ["ses_fake_cleanup"]:
        return failed(f"the run cleanup did not stop the session: {client.stopped}")
    return passed(
        "case hooks ran in reverse order and the run-scoped session was stopped"
    )


def evidence_is_redacted(ctx: Context) -> Check:
    token = ctx.client.http.token
    if token is None:
        return failed("the run has no token to test redaction with")
    leaked = f"Authorization: Bearer {token.value}"
    cleaned = redact(leaked, secrets=ctx.client.secrets)
    if token.value in cleaned:
        return failed("redaction left the bearer in evidence")
    shaped = redact("api_key=sk-live-0123456789abcdef", secrets=())
    if "sk-live-0123456789abcdef" in shaped:
        return failed("redaction left a secret-shaped string in evidence")
    return passed(
        "the bearer and a secret-shaped string are both masked before printing"
    )


CASES = [
    Case(
        "L1",
        "local",
        "WI-4135",
        "critical",
        "the suite refuses production and unknown hosts",
        prod_url_refused,
    ),
    Case(
        "L2",
        "local",
        "WI-4135",
        "critical",
        "throwaway sessions are pinned to pod/deepseek",
        session_arguments_pinned,
    ),
    Case(
        "L3",
        "local",
        "WI-4135",
        "high",
        "cleanup runs after a failed case",
        cleanup_runs_after_failure,
    ),
    Case(
        "L4",
        "local",
        "WI-4135",
        "critical",
        "evidence is redacted before it is printed",
        evidence_is_redacted,
    ),
]
