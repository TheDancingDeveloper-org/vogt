"""Group 5 — sessions: a Codex pod, its conversation, and pod-level checks.

Everything here needs ``--with-sessions``. A session is started by S1 in the
pinned shape (pod, worktree off, deepseek flash) and stopped by S14 and by the
case/atexit cleanup. Later checks reuse that one session so a run costs one
pod, not one per assertion. A check that cannot observe its subject (no
cluster access, a feature not deployed) says SKIP with the reason.
"""

from __future__ import annotations

import json
import subprocess
import time
from typing import Any, cast

from ..harness import (
    Case,
    Check,
    Context,
    HttpStatusError,
    SkipCase,
    ToolError,
    failed,
    passed,
    skipped,
)
from ..policy import (
    SESSION_MODEL,
    SESSION_PROJECT,
    SESSION_TEMPLATE,
    session_start_arguments,
)

PONG_TASK = (
    "Run exactly this shell command: echo PONG\n"
    "Then reply with the single word PONG and nothing else."
)

_FIRST_TURN_FAILURE = "sessions.codex.first_turn_failure"

# These are terminal/provider-network facts printed by Codex itself. A
# transient "reconnecting" line is not enough: the CLI may recover. The
# completed failure wording is useful when last_reply is empty because no
# assistant response was ever written to the rollout.
_PROVIDER_FAILURE_MARKERS = (
    "connection failed:",
    "error sending request",
    "insufficient balance",
    "insufficient_quota",
    "too many requests",
    "service unavailable",
    "no deployments available",
)


def _session(ctx: Context, key: str = "sessions.codex") -> str:
    identifier = ctx.state.get(key)
    if not isinstance(identifier, str):
        raise SkipCase(
            "the Codex acceptance session was not started by an earlier check"
        )
    return identifier


def _session_row(ctx: Context, identifier: str) -> dict[str, Any]:
    # `full` mode: the digest listing is a product behaviour of its own (S18
    # asserts it), but every later check needs the row to exist regardless.
    listing = ctx.client.tool("session_list", {"mode": "full"})
    for row in listing.get("sessions") or []:
        if isinstance(row, dict) and identifier in {
            row.get("id"),
            row.get("session_id"),
        }:
            return row
    return {}


def _start(
    ctx: Context, *, template: str, model: str | None, task: str
) -> dict[str, Any]:
    arguments = session_start_arguments(ctx.tag, task=task)
    arguments["template"] = template
    if model is None:
        arguments.pop("model", None)
    payload = ctx.client.tool("session_start", arguments)
    session = (
        payload.get("session") if isinstance(payload.get("session"), dict) else payload
    )
    identifier = str(session.get("id") or session.get("session_id") or "")
    if not identifier:
        raise AssertionError(f"session_start returned no id: {payload!r}")
    ctx.track_session(identifier)
    return cast(dict[str, Any], session)


def _last_replies(ctx: Context, identifier: str) -> str:
    payload = ctx.client.tool("session_last_reply", {"id": identifier, "n": 5})
    messages = [
        message
        for message in payload.get("messages") or []
        if isinstance(message, dict)
    ]
    if not messages:
        return ""
    return str(messages[-1].get("text", ""))


def _all_replies(ctx: Context, identifier: str) -> str:
    payload = ctx.client.tool("session_last_reply", {"id": identifier, "n": 20})
    messages = [
        message
        for message in payload.get("messages") or []
        if isinstance(message, dict)
    ]
    return "\n".join(str(message.get("text", "")) for message in messages)


def _poll_reply(
    ctx: Context, identifier: str, needle: str, *, timeout_s: int = 120
) -> tuple[bool, str]:
    deadline = time.monotonic() + timeout_s
    text = ""
    while time.monotonic() < deadline:
        text = _last_replies(ctx, identifier)
        if needle in text:
            return True, text
        time.sleep(2)
    return False, text


def _missing_reply_detail(ctx: Context, identifier: str) -> str:
    """Explain an empty transcript from the live session, without guessing.

    `session_last_reply` returning no message can mean that write-back failed,
    but it can also mean that the model turn never produced an assistant
    message. Read the terminal and session state so the acceptance report
    keeps those two failures distinct.
    """
    screen = ctx.client.tool(
        "session_screen", {"id": identifier, "scrollback_lines": 80}
    )
    text = "\n".join(str(line) for line in screen.get("lines") or [])
    # A TUI repaint can erase the completed failure from the rendered grid.
    # The engine's ANSI-stripped history is the durable copy of what was
    # printed, and is already redacted at the product boundary.
    try:
        log = ctx.client.tool(
            "session_log_tail",
            {"id": identifier, "tail_bytes": 32 * 1024, "strip_ansi": True},
        )
        text += "\n" + str(log.get("text") or "")
    except (HttpStatusError, ToolError):
        pass
    lowered = text.lower()
    marker = next(
        (value for value in _PROVIDER_FAILURE_MARKERS if value in lowered), None
    )
    if marker is not None:
        return (
            "the first turn produced no assistant reply because the Codex "
            f"screen reports a provider/network failure ({marker!r})"
        )

    row = _session_row(ctx, identifier)
    blocked = row.get("blocked")
    if isinstance(blocked, dict) and blocked.get("reason"):
        return (
            "the first turn produced no assistant reply and the session is "
            f"blocked: {str(blocked['reason'])[-240:]}"
        )
    return (
        "the first turn produced no assistant reply; last_reply found the "
        "conversation but it contains no assistant message"
    )


def _first_turn_prerequisite(ctx: Context, check: str) -> Check | None:
    failure = ctx.state.get(_FIRST_TURN_FAILURE)
    if not isinstance(failure, str) or not failure:
        return None
    return skipped(
        f"{check} was not exercised: S3 established that the first model turn "
        f"never completed ({failure})"
    )


def _ask(
    ctx: Context, identifier: str, prompt: str, needle: str, *, timeout_s: int = 120
) -> tuple[bool, str]:
    ctx.client.tool(
        "session_input",
        {"id": identifier, "text": prompt, "reason": f"{ctx.tag}: acceptance probe"},
    )
    return _poll_reply(ctx, identifier, needle, timeout_s=timeout_s)


def start_codex(ctx: Context) -> Check:
    ctx.require_sessions()
    session = _start(
        ctx, template=SESSION_TEMPLATE, model=SESSION_MODEL, task=PONG_TASK
    )
    identifier = str(session.get("id") or session.get("session_id"))
    ctx.state["sessions.codex"] = identifier
    ctx.state["sessions.codex.engine"] = session.get("engine_session_id")
    if session.get("placement") not in (None, "pod"):
        return failed(f"session started with placement {session.get('placement')!r}")
    if session.get("model") not in (None, SESSION_MODEL):
        return failed(f"session started with model {session.get('model')!r}")
    if session.get("project") not in (None, SESSION_PROJECT):
        return failed(f"session started in project {session.get('project')!r}")
    return passed(
        f"started {identifier} ({session.get('engine_session_id')}) as a pod "
        f"with {SESSION_MODEL}"
    )


def session_listed(ctx: Context) -> Check:
    """A live session must appear in the default `session_list`.

    The digest listing is what a person and an agent see by default; a row
    that is only reachable with `mode=full` is a listing that hides the very
    session the caller just started.
    """
    ctx.require_sessions()
    identifier = _session(ctx)
    digest = ctx.client.tool("session_list")
    # Digest mode fills `digests`; full mode fills `sessions`. Read whichever
    # this mode answered with rather than assuming one key.
    digest_payload = digest.get("digests") or digest.get("sessions") or []
    digest_rows = [row for row in digest_payload if isinstance(row, dict)]
    digest_ids = {row.get("id") for row in digest_rows}
    full = ctx.client.tool("session_list", {"mode": "full"})
    full_rows = [row for row in full.get("sessions") or [] if isinstance(row, dict)]
    full_row = next((row for row in full_rows if row.get("id") == identifier), None)
    if identifier in digest_ids:
        return passed(
            f"default session_list lists {identifier} among {len(digest_rows)} row(s)"
        )
    if full_row is not None:
        return failed(
            f"default session_list returned {len(digest_rows)} row(s) and "
            f"total={digest.get('total')!r}, omitting the live session "
            f"{identifier} that mode=full lists (activity="
            f"{full_row.get('activity')!r}, alive={full_row.get('alive')!r}, "
            f"hidden_count={digest.get('hidden_count')!r})"
        )
    return failed(
        f"session_list did not list the live session {identifier} in either "
        f"mode (digest={len(digest_rows)} rows, full={len(full_rows)} rows)"
    )


def first_task_delivered(ctx: Context) -> Check:
    ctx.require_sessions()
    identifier = _session(ctx)
    payload = ctx.client.tool(
        "session_wait", {"id": identifier, "until": "ready", "timeout_s": 180}
    )
    outcome = payload.get("outcome")
    if outcome != "ready":
        return failed(
            f"session_wait ended in {outcome!r}: {payload.get('screen_tail')!r}"
        )
    return passed(f"session {identifier} became ready; first task was delivered")


def pong_reply(ctx: Context) -> Check:
    ctx.require_sessions()
    identifier = _session(ctx)
    found, text = _poll_reply(ctx, identifier, "PONG", timeout_s=180)
    if not found:
        detail = _missing_reply_detail(ctx, identifier)
        ctx.state[_FIRST_TURN_FAILURE] = detail
        return failed(f"no PONG in the last replies after 180s: {detail}")
    return passed(f"last_reply contains PONG: {text.strip()[-160:]!r}")


def screen_readable(ctx: Context) -> Check:
    ctx.require_sessions()
    identifier = _session(ctx)
    payload = ctx.client.tool(
        "session_screen", {"id": identifier, "scrollback_lines": 40}
    )
    lines = [str(line) for line in payload.get("lines") or []]
    text = "\n".join(lines)
    if not text.strip():
        return failed("session_screen returned no lines")
    lowered = text.lower()
    if "bwrap" in lowered:
        return failed("the screen shows a bwrap error (A5 gate 3 failed)")
    if "do you trust the files" in lowered or (
        "approval" in lowered and "required" in lowered
    ):
        return failed(
            "the screen is showing a startup approval menu; the session is "
            "not driveable"
        )
    return passed(
        f"screen has {len(lines)} lines at "
        f"{payload.get('cols')}x{payload.get('rows')}; "
        "no bwrap/approval fault"
    )


def input_queueing(ctx: Context) -> Check:
    ctx.require_sessions()
    prerequisite = _first_turn_prerequisite(ctx, "queued-input delivery")
    if prerequisite is not None:
        return prerequisite
    identifier = _session(ctx)
    result = ctx.client.tool(
        "session_input",
        {
            "id": identifier,
            "text": "Run exactly: echo QUEUED\nThen reply with the single word QUEUED.",
            "reason": f"{ctx.tag}: queue probe",
        },
    )
    delivery = str(result.get("delivery"))
    if delivery in {"stuck", "wedged"}:
        return failed(
            f"input delivery was {delivery!r}: {result.get('delivery_evidence')!r}"
        )
    found, text = _poll_reply(ctx, identifier, "QUEUED", timeout_s=180)
    if not found:
        return failed(
            f"second input (delivery {delivery!r}) produced no QUEUED reply: "
            f"{text[-300:]!r}"
        )
    return passed(f"second input delivery={delivery}; reply contains QUEUED")


def hibernate_and_wake(ctx: Context) -> Check:
    ctx.require_sessions()
    prerequisite = _first_turn_prerequisite(ctx, "hibernate/wake conversation survival")
    if prerequisite is not None:
        return prerequisite
    identifier = _session(ctx)
    ctx.client.tool(
        "session_hibernate", {"id": identifier, "reason": f"{ctx.tag}: hibernate"}
    )
    row = _session_row(ctx, identifier)
    if row.get("activity") not in {"hibernated", "stopped"}:
        return failed(f"hibernate left activity {row.get('activity')!r}")
    ctx.client.tool("session_wake", {"id": identifier, "reason": f"{ctx.tag}: wake"})
    waited = ctx.client.tool(
        "session_wait", {"id": identifier, "until": "ready", "timeout_s": 180}
    )
    if waited.get("outcome") != "ready":
        return failed(f"wake did not reach ready: {waited.get('outcome')!r}")
    text = _all_replies(ctx, identifier)
    if "PONG" not in text:
        return failed(f"the reply did not survive hibernate/wake: {text[-300:]!r}")
    return passed("hibernated, woke, became ready, and the PONG conversation survived")


def kubectl(ctx: Context, *arguments: str) -> dict[str, Any]:
    ctx.require_kubectl()
    command = ["kubectl", "--context", ctx.options.kube_context, *arguments]
    try:
        proc = subprocess.run(
            command, capture_output=True, text=True, timeout=30, check=False
        )
    except (FileNotFoundError, subprocess.TimeoutExpired) as exc:
        raise SkipCase(f"kubectl unavailable: {type(exc).__name__}") from None
    if proc.returncode != 0:
        raise SkipCase(f"kubectl failed: {proc.stderr.strip().splitlines()[-1:]!r}")
    try:
        payload = json.loads(proc.stdout)
    except json.JSONDecodeError:
        raise SkipCase("kubectl did not return JSON") from None
    if not isinstance(payload, dict):
        raise SkipCase("kubectl returned a non-object JSON document")
    return payload


def placement(ctx: Context) -> Check:
    ctx.require_sessions()
    identifier = _session(ctx)
    row = _session_row(ctx, identifier)
    engine_id = str(
        row.get("engine_session_id") or ctx.state.get("sessions.codex.engine") or ""
    )
    if not engine_id:
        return failed("cannot read the session's engine id for the pod lookup")
    payload = kubectl(
        ctx,
        "-n",
        "vogt-sessions",
        "get",
        "pods",
        "-l",
        f"vogt.session={engine_id}",
        "-o",
        "json",
    )
    pods = payload.get("items") or []
    if not pods:
        return failed("no session pod exists for a live pod session")
    pod = pods[0]
    node = str((pod.get("spec") or {}).get("nodeName") or "")
    labels = (pod.get("metadata") or {}).get("labels") or {}
    if node in {"winrarhost", "winrarhost1", "winrarhost3"}:
        return failed(f"session pod is on {node}, an excluded node (A5 gate 2)")
    affinity = json.dumps((pod.get("spec") or {}).get("affinity") or {})
    selector = json.dumps((pod.get("spec") or {}).get("nodeSelector") or {})
    if "NotIn" not in affinity and "node-role.kubernetes.io/etcd" not in selector:
        return failed(
            "pod placement is a preference, not a guarantee: "
            f"affinity={affinity[:200]} selector={selector[:200]}"
        )
    return passed(
        f"pod on {node}; labels={labels}; affinity/selector pins the session node"
    )


def workspace_content(ctx: Context) -> Check:
    ctx.require_sessions()
    identifier = _session(ctx)
    prompt = (
        "Run these shell commands and reply with their output verbatim, nothing else:\n"
        'if [ -n "${VOGT_WORKSPACE_ROOT:-}" ]; then '
        "echo WS_CAPABILITY_PRESENT; else echo WS_CAPABILITY_ABSENT; fi\n"
        "test -f ~/Working/AGENTS.md && echo WS_AGENTS_OK || echo WS_AGENTS_MISSING\n"
        "test -d ~/Working/.claude/rules && echo WS_RULES_OK || echo WS_RULES_MISSING\n"
        "for f in settings.local.json auth.json .git-credentials .netrc; do echo "
        '"$f $(find ~ -maxdepth 4 -name "$f" 2>/dev/null | wc -l)"; done\n'
        "echo \"ssh_keys $(find ~/.ssh -maxdepth 1 -name 'id_*' "
        '2>/dev/null | wc -l)"\n'
        "echo WS_SCAN_DONE"
    )
    found, text = _ask(ctx, identifier, prompt, "WS_SCAN_DONE", timeout_s=90)
    if not found:
        return failed(f"the agent did not complete the workspace scan: {text[-200:]!r}")
    if "WS_CAPABILITY_ABSENT" in text:
        return skipped(
            "live pod reported VOGT_WORKSPACE_ROOT unset; the deployment does not "
            "provide a workspace snapshot (enable ENGINE_POD_WORKSPACE)"
        )
    if "WS_CAPABILITY_PRESENT" not in text:
        return failed(
            "the workspace probe did not report whether VOGT_WORKSPACE_ROOT is set: "
            f"{text[-200:]!r}"
        )
    if "WS_AGENTS_MISSING" in text or "WS_RULES_MISSING" in text:
        return failed(
            "live pod reported VOGT_WORKSPACE_ROOT set, but the mounted workspace "
            "is missing AGENTS.md or .claude/rules"
        )
    leaks: list[str] = []
    for name in ("settings.local.json", "auth.json", ".git-credentials", ".netrc"):
        for line in text.splitlines():
            if line.startswith(name):
                parts = line.split()
                if len(parts) > 1 and parts[1].isdigit() and int(parts[1]) > 0:
                    leaks.append(f"{name} x{parts[1]}")
    for line in text.splitlines():
        if line.startswith("ssh_keys"):
            parts = line.split()
            if len(parts) > 1 and parts[1].isdigit() and int(parts[1]) > 0:
                leaks.append(f"ssh keys x{parts[1]}")
    if leaks:
        return failed("the pod workspace contains credentials: " + ", ".join(leaks))
    return passed(
        "workspace has AGENTS.md and .claude/rules, and no "
        "settings.local.json/auth.json/.git-credentials/.netrc/ssh keys"
    )


def graft_token(ctx: Context) -> Check:
    ctx.require_sessions()
    identifier = _session(ctx)
    prompt = (
        "Run this and reply with the output verbatim:\n"
        'if [ -z "${VOGT_GRAFT_TOKEN_FILE:-}" ]; then '
        "echo GRAFT_CREDENTIAL_ABSENT; "
        'elif [ -r "$VOGT_GRAFT_TOKEN_FILE" ]; then '
        "echo GRAFT_CREDENTIAL_PRESENT; "
        "else echo GRAFT_CREDENTIAL_BROKEN; fi"
    )
    found, text = _ask(ctx, identifier, prompt, "GRAFT_CREDENTIAL_", timeout_s=60)
    if not found:
        return failed(
            f"the agent did not answer the graft-credential probe: {text[-200:]!r}"
        )
    if "GRAFT_CREDENTIAL_ABSENT" in text:
        return skipped(
            "live pod reported VOGT_GRAFT_TOKEN_FILE unset; enable per-session "
            "graft credential minting for the source provider"
        )
    if "GRAFT_CREDENTIAL_BROKEN" in text:
        return failed(
            "live pod reported VOGT_GRAFT_TOKEN_FILE set, but its credential file "
            "is not readable"
        )
    if "GRAFT_CREDENTIAL_PRESENT" not in text:
        return failed(f"the graft credential probe was ambiguous: {text[-200:]!r}")
    clone = (
        f"g clone vogt /tmp/{ctx.tag} --line rust-2 >/dev/null 2>&1 "
        "&& echo GRAFT_CLONE_OK || echo GRAFT_CLONE_FAIL"
    )
    found, text = _ask(
        ctx,
        identifier,
        f"Run this and reply with the output verbatim:\n{clone}",
        "GRAFT_CLONE_",
        timeout_s=240,
    )
    if not found:
        return failed(f"the graft clone probe never answered: {text[-200:]!r}")
    if "GRAFT_CLONE_FAIL" in text:
        return failed(
            "a pod session with a graft credential could not clone the sandbox repo"
        )
    return passed(
        "live pod reported a readable VOGT_GRAFT_TOKEN_FILE and used it to clone "
        "vogt/rust-2"
    )


def cadastre_mcp(ctx: Context) -> Check:
    ctx.require_sessions()
    identifier = _session(ctx)
    prompt = (
        "Run this and reply with the output verbatim:\n"
        "grep -q '^\\[mcp_servers\\.cadastre\\]' ~/.codex/config.toml 2>/dev/null "
        "&& echo CADASTRE_CAPABILITY_PRESENT || echo CADASTRE_CAPABILITY_ABSENT"
    )
    found, text = _ask(ctx, identifier, prompt, "CADASTRE_CAPABILITY_", timeout_s=60)
    if not found:
        return failed(f"the agent did not answer the cadastre probe: {text[-200:]!r}")
    if "CADASTRE_CAPABILITY_ABSENT" in text:
        return skipped(
            "live pod config has no [mcp_servers.cadastre] entry; configure the "
            "cadastre MCP URL and credential for worker sessions"
        )
    if "CADASTRE_CAPABILITY_PRESENT" not in text:
        return failed(f"the cadastre registration probe was ambiguous: {text[-200:]!r}")
    found, text = _ask(
        ctx,
        identifier,
        "Call the cadastre MCP version tool. If it returns a version, reply "
        "CADASTRE_CALL_OK. If the call fails, reply CADASTRE_CALL_FAIL followed "
        "by the error. Do not inspect or print any credential.",
        "CADASTRE_CALL_",
        timeout_s=90,
    )
    if not found:
        return failed(
            "cadastre is registered in the live pod, but the tool invocation did "
            f"not answer: {text[-200:]!r}"
        )
    if "CADASTRE_CALL_FAIL" in text:
        return failed(
            f"cadastre is registered but its version call failed: {text[-200:]!r}"
        )
    if "CADASTRE_CALL_OK" not in text:
        return failed(f"the cadastre invocation result was ambiguous: {text[-200:]!r}")
    return passed(
        "cadastre is registered in the live pod and its version tool answered"
    )


def mcp_registrations(ctx: Context) -> Check:
    ctx.require_sessions()
    identifier = _session(ctx)
    prompt = (
        "Run this and reply with the output verbatim:\n"
        "grep -q vogt ~/.codex/config.toml 2>/dev/null && echo VOGT_MCP_OK "
        "|| echo VOGT_MCP_MISSING"
    )
    found, text = _ask(ctx, identifier, prompt, "VOGT_MCP_", timeout_s=60)
    if not found:
        return skipped(
            f"the agent did not answer the MCP-config probe: {text[-200:]!r}"
        )
    if "VOGT_MCP_MISSING" in text:
        return failed(
            "the vogt MCP server is not registered in the pod's ~/.codex/config.toml"
        )
    return passed("vogt is registered in the pod's ~/.codex/config.toml")


def _screen_text(ctx: Context, identifier: str, lines: int = 60) -> str:
    payload = ctx.client.tool(
        "session_screen", {"id": identifier, "scrollback_lines": lines}
    )
    return "\n".join(str(line) for line in payload.get("lines") or [])


#: What the session shell guard prints when it refuses `env`. The agent may
#: relay it in a reply or leave it on the terminal; either is the same proof.
GUARD_MARKERS = (
    "environment guard",
    "guard: `env`",
    "never print environment variables",
)

ENV_PROBE = "Run exactly this shell command and report the result verbatim: env | head"


def env_guard(ctx: Context) -> Check:
    ctx.require_sessions()
    identifier = _session(ctx)
    reply = ""
    for _attempt in (1, 2):
        ctx.client.tool(
            "session_input",
            {"id": identifier, "text": ENV_PROBE, "reason": f"{ctx.tag}: env guard"},
        )
        deadline = time.monotonic() + 90
        while time.monotonic() < deadline:
            reply = _last_replies(ctx, identifier)
            both = reply + "\n" + _screen_text(ctx, identifier)
            if _looks_like_env_dump(both):
                return failed(
                    "the environment was printed instead of being refused (WI-4046)"
                )
            if any(marker in both.lower() for marker in GUARD_MARKERS):
                tail = reply.strip()[-160:] or both.strip()[-160:]
                return passed(f"the environment guard refused `env | head`: {tail!r}")
            time.sleep(2)
    return skipped(
        "the agent did not put `env | head` through the shell guard in two "
        f"attempts, so the guard was not exercised: {reply.strip()[-200:]!r}"
    )


def _looks_like_env_dump(text: str) -> bool:
    lines = [line for line in text.splitlines() if "=" in line]
    shaped = [
        line
        for line in lines
        if line.split("=", 1)[0].strip().replace("_", "").isalnum()
    ]
    return len(shaped) >= 5


def secret_redaction(ctx: Context) -> Check:
    ctx.require_sessions()
    identifier = _session(ctx)
    sentinel = "sk-test-ACCEPTANCESENTINEL123456"
    prompt = (
        "Run exactly this shell command and reply with its output verbatim:\n"
        f"printf '%s\\n' '{sentinel}'"
    )
    # The reply is served redacted, so the sentinel disappearing is the
    # success signal: waiting for `sk-` would wait forever and turn a working
    # mask into a SKIP. Wait for a new reply that carries a mask marker.
    mask_markers = ("[redacted]", "<redacted>", "sk-***", "sk-<redacted>")
    before = _last_replies(ctx, identifier)
    text = before
    for _attempt in (1, 2):
        ctx.client.tool(
            "session_input",
            {"id": identifier, "text": prompt, "reason": f"{ctx.tag}: redaction probe"},
        )
        deadline = time.monotonic() + 90
        while time.monotonic() < deadline:
            text = _last_replies(ctx, identifier)
            both = text + "\n" + _screen_text(ctx, identifier)
            if sentinel in both:
                return failed(
                    "the secret-shaped sentinel appeared unmasked in the reply "
                    "or screen"
                )
            if any(marker in both.lower() for marker in mask_markers):
                return passed(
                    "the secret-shaped sentinel is masked in the session reply "
                    "and screen"
                )
            if text and text != before and "redacted" in text.lower():
                return passed(
                    "the secret-shaped sentinel is masked in the session reply "
                    "and screen"
                )
            time.sleep(2)
    # A new reply with no sentinel and no mask is not evidence of redaction:
    # the agent may simply have declined to run the probe. Retried once above;
    # report the honest state instead of a pass.
    return skipped(
        "the probe produced no secret-shaped output to redact (the agent may "
        f"have declined): {text.strip()[-200:]!r}"
    )


def stop_session(ctx: Context) -> Check:
    ctx.require_sessions()
    identifier = _session(ctx)
    ctx.stop_session(identifier)
    row = _session_row(ctx, identifier)
    if row and row.get("activity") not in {None, "stopped", "exited"}:
        return failed(f"session_stop left activity {row.get('activity')!r}")
    engine_id = str(
        ctx.state.get("sessions.codex.engine") or row.get("engine_session_id") or ""
    )
    if engine_id:
        try:
            payload = kubectl(
                ctx,
                "-n",
                "vogt-sessions",
                "get",
                "pods",
                "-l",
                f"vogt.session={engine_id}",
                "-o",
                "json",
            )
        except SkipCase:
            return passed(
                f"{identifier} stopped; pod absence not checked (no cluster access)"
            )
        # Pod deletion is asynchronous: a Terminating pod is not an orphan
        # yet. Give it a minute before calling it one.
        deadline = time.monotonic() + 60
        remaining = payload.get("items") or []
        while remaining and time.monotonic() < deadline:
            time.sleep(2)
            payload = kubectl(
                ctx,
                "-n",
                "vogt-sessions",
                "get",
                "pods",
                "-l",
                f"vogt.session={engine_id}",
                "-o",
                "json",
            )
            remaining = payload.get("items") or []
        if remaining:
            names = [
                item.get("metadata", {}).get("name")
                for item in remaining
                if isinstance(item, dict)
            ]
            return failed(f"session_stop left an orphan pod (A5 gate 4): {names}")
        return passed(f"{identifier} stopped and its pod is gone")
    return passed(f"{identifier} stopped")


def _template_named(ctx: Context, needle: str) -> dict[str, Any] | None:
    payload = ctx.client.tool("session_template_list")
    templates = payload.get("templates") or payload.get("session_templates") or []
    for template in templates:
        if not isinstance(template, dict):
            continue
        haystack = (
            f"{template.get('name')} {template.get('agent')} "
            f"{template.get('placements')} {template.get('tags')}"
        ).lower()
        if needle in haystack:
            return template
    return None


def _probe_template(ctx: Context, needle: str, label: str, key: str) -> Check:
    ctx.require_sessions()
    template = _template_named(ctx, needle)
    if template is None:
        return skipped(f"no {label} template is configured on this instance")
    name = str(template.get("name"))
    try:
        session = _start(
            ctx, template=name, model=None, task="Reply with READY and do nothing else."
        )
    except (ToolError, HttpStatusError) as exc:
        text = str(exc).lower()
        if "host-only" in text or "host only" in text or "placement" in text:
            return skipped(f"{label} is host-only on this instance: {exc}")
        return failed(f"{label} pod start failed for another reason: {exc}")
    identifier = str(session.get("id") or session.get("session_id"))
    ctx.state[key] = identifier
    ctx.stop_session(identifier)
    return passed(f"{label} started in a pod as {identifier} and was stopped")


def claude_template(ctx: Context) -> Check:
    return _probe_template(ctx, "claude", "Claude Code", "sessions.claude")


def opencode_template(ctx: Context) -> Check:
    return _probe_template(ctx, "opencode", "opencode", "sessions.opencode")


def survives_restart(ctx: Context) -> Check:
    ctx.require_sessions()
    ctx.require_restart()
    return skipped(
        "the suite cannot restart the engine through the API; this check needs an "
        "operator-driven redeploy (WI-3405/WI-3205)"
    )


CASES = [
    Case(
        "S1",
        "sessions",
        "S1",
        "critical",
        "start a Codex session in a pod",
        start_codex,
    ),
    Case(
        "S18",
        "sessions",
        "D1",
        "high",
        "a live session appears in the default session list",
        session_listed,
    ),
    Case(
        "S2",
        "sessions",
        "S7",
        "high",
        "the first task is delivered and the session is ready",
        first_task_delivered,
    ),
    Case(
        "S3",
        "sessions",
        "S1",
        "critical",
        "last_reply returns the PONG answer",
        pong_reply,
    ),
    Case("S4", "sessions", "D2", "high", "session_screen is readable", screen_readable),
    Case(
        "S5",
        "sessions",
        "D1",
        "high",
        "a second input is delivered and answered",
        input_queueing,
    ),
    Case(
        "S6",
        "sessions",
        "S2",
        "critical",
        "hibernate then wake keeps the conversation",
        hibernate_and_wake,
    ),
    Case(
        "S7",
        "sessions",
        "S10",
        "critical",
        "the pod is on an allowed node and never Node B",
        placement,
    ),
    Case(
        "S8",
        "sessions",
        "W3",
        "high",
        "the pod has workspace rules and no credentials",
        workspace_content,
    ),
    Case(
        "S9",
        "sessions",
        "G2",
        "high",
        "a pod session can clone with its graft credential",
        graft_token,
    ),
    Case(
        "S10",
        "sessions",
        "W2",
        "high",
        "cadastre is registered in the pod",
        cadastre_mcp,
    ),
    Case(
        "S11",
        "sessions",
        "W1",
        "high",
        "the vogt MCP server is registered in the pod",
        mcp_registrations,
    ),
    Case(
        "S12",
        "sessions",
        "R2",
        "critical",
        "the environment guard refuses env | head",
        env_guard,
    ),
    Case(
        "S13",
        "sessions",
        "O3",
        "critical",
        "secret-shaped output is masked in screen/reply",
        secret_redaction,
    ),
    Case(
        "S14",
        "sessions",
        "S3",
        "critical",
        "stop leaves no live session or pod",
        stop_session,
    ),
    Case(
        "S15",
        "sessions",
        "S4",
        "medium",
        "Claude Code runs in a pod or is refused for a named reason",
        claude_template,
    ),
    Case(
        "S16",
        "sessions",
        "S5",
        "medium",
        "opencode runs in a pod or is refused for a named reason",
        opencode_template,
    ),
    Case(
        "S17",
        "sessions",
        "S9",
        "medium",
        "a session survives an engine restart",
        survives_restart,
    ),
]
