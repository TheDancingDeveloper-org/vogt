"""Group 6 — LLM providers and the refusals around a bad launch."""

from __future__ import annotations

from ..harness import (
    Case,
    Check,
    Context,
    HttpStatusError,
    ToolError,
    failed,
    passed,
    skipped,
)
from ..policy import SESSION_MODEL, session_start_arguments

#: Provider states the engine reports as usable. The Rust registry currently
#: emits `active` (and `dropped` when an env key clashes with the manifest),
#: while older/other surfaces used `ready`/`ok`.
_READY_STATES = frozenset({None, "active", "ready", "ok"})
#: States that report a secret the launch path would have to fetch.
_BROKEN_SECRET_STATES = frozenset(
    {
        "missing-secret",
        "missing_secret",
        "secret-missing",
        "secret_missing",
        "broken",
        "failed",
    }
)


def deepseek_flash_registered(ctx: Context) -> Check:
    payload = ctx.client.tool("llm_provider_list")
    providers = [row for row in payload.get("providers") or [] if isinstance(row, dict)]
    if not providers:
        return failed(f"llm_provider_list is empty (engine={payload.get('engine')!r})")
    with_model = [
        row for row in providers if SESSION_MODEL in (row.get("models") or [])
    ]
    if not with_model:
        return failed(
            f"no provider lists {SESSION_MODEL}: "
            f"{[(row.get('name'), row.get('models')) for row in providers]}"
        )
    row = with_model[0]
    if row.get("state") not in _READY_STATES:
        return failed(
            f"{row.get('name')} is {row.get('state')!r}: {row.get('detail')!r}"
        )
    return passed(
        f"{row.get('name')} serves {SESSION_MODEL} (state={row.get('state')!r})"
    )


def unknown_model_refused(ctx: Context) -> Check:
    ctx.require_sessions()
    arguments = session_start_arguments(ctx.tag, task="This launch should be refused.")
    arguments["model"] = "acceptance/definitely-not-a-model"
    try:
        payload = ctx.client.tool("session_start", arguments)
    except (ToolError, HttpStatusError) as exc:
        text = str(exc)
        if "definitely-not-a-model" not in text:
            return failed(f"the refusal does not name the model: {text}")
        if "internal error" in text.lower():
            return failed(f"an unknown model produced a server error: {text}")
        return passed(f"unknown model refused as data: {text[:200]}")
    session = (
        payload.get("session") if isinstance(payload.get("session"), dict) else payload
    )
    identifier = str(session.get("id") or session.get("session_id") or "")
    if identifier:
        ctx.stop_session(identifier)
    return failed(
        "session_start accepted the unknown model 'acceptance/definitely-not-a-"
        f"model' and started {identifier or 'a session'}; the provider registry "
        "lists `models` as informational (docs/ENGINE.md), so nothing refuses a "
        "model id the instance cannot serve at launch — the acceptance intent "
        "(matrix S11) is a 4xx naming the model before a pod is created"
    )


def missing_secret_names_the_secret(ctx: Context) -> Check:
    payload = ctx.client.tool("llm_provider_list")
    providers = [row for row in payload.get("providers") or [] if isinstance(row, dict)]
    broken = [row for row in providers if row.get("state") in _BROKEN_SECRET_STATES]
    if not broken:
        states = {str(row.get("state")) for row in providers}
        return skipped(
            "no provider reports a missing/broken secret "
            f"(states={sorted(states)}), so the launch refusal cannot be "
            "exercised without removing a secret"
        )
    candidate = broken[0]
    secret_name = str(candidate.get("secret_name") or "")
    models = candidate.get("models") or []
    if not secret_name or not models:
        return skipped(
            "the broken provider does not state a secret name and model to launch"
        )
    ctx.require_sessions()
    arguments = session_start_arguments(ctx.tag, task="This launch should be refused.")
    arguments["model"] = models[0]
    try:
        result = ctx.client.tool("session_start", arguments)
    except (ToolError, HttpStatusError) as exc:
        text = str(exc)
        if secret_name not in text:
            return failed(f"the refusal does not name {secret_name}: {text}")
        if "internal error" in text.lower():
            return failed(f"a missing secret produced a server error: {text}")
        return passed(f"{candidate.get('name')} refused launch naming {secret_name}")
    session = (
        result.get("session") if isinstance(result.get("session"), dict) else result
    )
    identifier = str(session.get("id") or session.get("session_id") or "")
    if identifier:
        ctx.stop_session(identifier)
    return failed(f"a provider with a missing secret ({secret_name}) started a session")


def provider_failure_visible(ctx: Context) -> Check:
    return skipped("provider-failure visibility is WI-4131, which has not landed")


CASES = [
    Case(
        "P1",
        "providers",
        "S11",
        "critical",
        "a provider serves deepseek-v4-flash",
        deepseek_flash_registered,
    ),
    Case(
        "P2",
        "providers",
        "S11",
        "high",
        "an unknown model is refused, naming it",
        unknown_model_refused,
    ),
    Case(
        "P3",
        "providers",
        "S11",
        "high",
        "a missing secret refuses launch by name",
        missing_secret_names_the_secret,
    ),
    Case(
        "P4",
        "providers",
        "S11",
        "medium",
        "provider failures are visible to the caller",
        provider_failure_visible,
    ),
]
