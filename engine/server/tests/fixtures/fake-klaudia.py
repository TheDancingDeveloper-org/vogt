#!/usr/bin/env python3
"""A scripted stand-in for `klaudia` in stream-json mode (WI-1097 tests).

It speaks the embedding contract (msp-klaudia docs/embedding.md) for the few
behaviours the chat runtime depends on, chosen by the message text:

- `run:<cmd>`   a Bash call, put to the approval gate the way the chat's
                PreToolUse hook does (POST $VOGT_CHAT_GATE_URL);
- `sneak:<cmd>` a Bash call that runs WITHOUT asking the gate;
- `read:<path>` a Read call, and `fetch:<url>` a WebFetch call, both gated;
- `hostask`     a `can_use_tool` control request for a host change;
- `hooks`       Klaudia's question whether the project's hooks may run;
- `fail`        a turn that ends in a provider error (WI-1007);
- `slow`        a turn that waits until it is interrupted;
- anything else is echoed back as `<model>: <text>`.

Every launch appends its argv to `launches.jsonl` in the working directory,
and the names of its environment variables to `env.jsonl`; every control
response it receives goes to `responses.jsonl`.
"""

from __future__ import annotations

import json
import os
import sys
import urllib.request
from pathlib import Path
from typing import Any

ARGS = sys.argv[1:]


def flag(name: str) -> str | None:
    for i, arg in enumerate(ARGS):
        if arg == name and i + 1 < len(ARGS):
            return ARGS[i + 1]
    return None


def append(name: str, value: object) -> None:
    with Path(name).open("a") as f:
        f.write(json.dumps(value) + "\n")


def out(obj: dict[str, Any]) -> None:
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()


def say(text: str) -> None:
    content = [{"type": "text", "text": text}]
    out({"type": "assistant", "message": {"role": "assistant", "content": content}})


def result(text: str, *, error: bool = False) -> None:
    out(
        {
            "type": "result",
            "subtype": "error_during_execution" if error else "success",
            "is_error": error,
            "result": text,
            "session_id": SESSION,
        }
    )


def ask(request_id: str, request: dict[str, Any]) -> None:
    out({"type": "control_request", "request_id": request_id, "request": request})


def gate(tool: str, tool_input: dict[str, Any]) -> dict[str, Any]:
    body: dict[str, Any] = {"hook_event_name": "PreToolUse", "tool_name": tool}
    body["tool_input"] = tool_input
    token = os.environ["VOGT_CHAT_GATE_TOKEN"]
    request = urllib.request.Request(
        os.environ["VOGT_CHAT_GATE_URL"],
        data=json.dumps(body).encode(),
        headers={
            "Authorization": f"Bearer {token}",
            "Content-Type": "application/json",
        },
        method="POST",
    )
    with urllib.request.urlopen(request, timeout=60) as answer:
        verdict: dict[str, Any] = json.loads(answer.read() or b"{}")
    return verdict


TOOLS = {
    "run": ("Bash", "command"),
    "sneak": ("Bash", "command"),
    "read": ("Read", "file_path"),
    "fetch": ("WebFetch", "url"),
}


def tool_call(text: str) -> None:
    verb, arg = text.split(":", 1)
    tool, field = TOOLS[verb]
    tool_input = {field: arg}
    use_id = f"toolu_{abs(hash(text)) % 100000}"
    call = {"type": "tool_use", "id": use_id, "name": tool, "input": tool_input}
    out({"type": "assistant", "message": {"role": "assistant", "content": [call]}})
    blocked, said = False, f"ran {arg}"
    if verb != "sneak":
        verdict = gate(tool, tool_input)
        if verdict.get("decision") == "block":
            blocked, said = True, str(verdict.get("reason", "blocked"))
    block = {
        "type": "tool_result",
        "tool_use_id": use_id,
        "content": said,
        "is_error": blocked,
    }
    out({"type": "user", "message": {"role": "user", "content": [block]}})
    say("done")
    result("done")


SESSION = flag("--session-id") or flag("--resume") or "unknown"
model = flag("--model") or "default"
append("launches.jsonl", ARGS)
append("env.jsonl", sorted(os.environ))
print("banner from a wrapper, not JSON", flush=True)
out(
    {
        "type": "system",
        "subtype": "init",
        "session_id": SESSION,
        "model": model,
        "permissionMode": "autonomous",
        "resumed": "--resume" in ARGS,
    }
)

waiting: dict[str, str] = {}
for line in iter(sys.stdin.readline, ""):
    try:
        msg = json.loads(line)
    except ValueError:
        continue
    kind = msg.get("type")
    if kind == "control_response":
        append("responses.jsonl", msg)
        rid = msg.get("response", {}).get("request_id")
        if rid in waiting:
            text = waiting.pop(rid)
            say(text)
            result(text)
        continue
    if kind == "control_request":
        sub = msg["request"]["subtype"]
        if sub == "set_model":
            model = msg["request"].get("model") or "default"
        response = {"subtype": "success", "request_id": msg["request_id"]}
        out({"type": "control_response", "response": {**response, "response": {}}})
        if sub == "interrupt" and "slow" in waiting.values():
            for rid in [k for k, v in waiting.items() if v == "slow"]:
                waiting.pop(rid)
            result("Error: interrupted", error=True)
        continue
    if kind != "user":
        continue
    text = msg["message"]["content"]
    echo = {"role": "user", "content": text}
    out({"type": "user", "message": echo, "session_id": SESSION})
    if text.split(":", 1)[0] in TOOLS and ":" in text:
        tool_call(text)
    elif text == "hostask":
        waiting["ask-1"] = "host change answered"
        ask(
            "ask-1",
            {
                "subtype": "can_use_tool",
                "tool_name": "Bash",
                "input": {"command": "systemctl restart nginx"},
                "host_change": {"summary": "restart nginx"},
            },
        )
    elif text == "hooks":
        cfg = str(Path.cwd() / ".klaudia" / "config.toml")
        waiting["hooks-1"] = "hooks answered"
        ask(
            "hooks-1",
            {
                "subtype": "can_use_tool",
                "tool_name": "Hooks",
                "specifier": cfg,
                "host_change": {"hooks": True, "paths": [cfg]},
            },
        )
    elif text == "fail":
        result("Error: 400 invalid request", error=True)
    elif text == "slow":
        waiting["slow-1"] = "slow"
    else:
        reply = f"{model}: {text}"
        say(reply)
        result(reply)
