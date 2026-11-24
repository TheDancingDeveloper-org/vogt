#!/usr/bin/env python3
"""A stand-in for `codex app-server --stdio`.

It speaks the few JSON-RPC requests and notifications the quick-chat runtime
depends on, and enforces the ordering the fork does: a `turn/start` for a
thread the server has not started fails with `thread not found` (WI-1641), and
`thread/start` answers after a short delay while the session is built.

The turn text selects the behaviour:

- `run:<cmd>`   a command execution, put to a person for approval;
- `read:<path>` a tool call that runs unasked inside the working directory
                and asks for a person's approval outside it;
- `fetch:<url>` a tool call that a person must approve;
- `hostask`     an approval for a host change;
- `fail`        a turn that ends in a provider error (WI-1007);
- `slow`        a turn that waits until it is interrupted;
- anything else is echoed as `<model>: <text>`.

Every launch appends its argv to `launches.jsonl`, its environment variable
names to `env.jsonl`, each JSON-RPC request it receives to `requests.jsonl` and
each decision to `responses.jsonl`. The chosen model is kept in `model` so a
resume, which does not repeat it, answers on the model the person picked.
"""

import json
import os
import sys
import time
from pathlib import Path
from typing import Any

threads: set[str] = set()
# request id -> how to answer it once the engine sends a decision
waiting: dict[str, dict[str, Any]] = {}
# thread id whose turn is waiting for an interrupt
slow: set[str] = set()
model = "unknown"


def append(name: str, value: object) -> None:
    with Path(name).open("a") as f:
        f.write(json.dumps(value) + "\n")


def send(msg: dict[str, object]) -> None:
    sys.stdout.write(json.dumps(msg) + "\n")
    sys.stdout.flush()


def read_model() -> str:
    try:
        return Path("model").read_text().strip() or "unknown"
    except OSError:
        return "unknown"


def write_model(value: str) -> None:
    Path("model").write_text(value)


def agent(text: str) -> None:
    send(
        {
            "jsonrpc": "2.0",
            "method": "item/agentMessage/completed",
            "params": {"text": text},
        }
    )


def turn_end(thread_id: str, error: str | None = None) -> None:
    params: dict[str, Any] = {"threadId": thread_id, "turn": {"id": "turn-1"}}
    if error is not None:
        params["error"] = {"message": error}
    send({"jsonrpc": "2.0", "method": "turn/completed", "params": params})


def started(method: str, item_id: str, command: str) -> None:
    send(
        {
            "jsonrpc": "2.0",
            "method": f"item/{method}/started",
            "params": {"itemId": item_id, "command": command},
        }
    )


def completed(method: str, item_id: str, output: str, ok: bool) -> None:
    send(
        {
            "jsonrpc": "2.0",
            "method": f"item/{method}/completed",
            "params": {
                "itemId": item_id,
                "status": "completed" if ok else "failed",
                "output": output,
            },
        }
    )


def ask(request_id: str, method: str, command: str, then: dict[str, Any]) -> None:
    waiting[request_id] = then
    send(
        {
            "jsonrpc": "2.0",
            "id": request_id,
            "method": method,
            "params": {"command": command},
        }
    )


def answer(decision: str, step: dict[str, Any]) -> None:
    allowed = decision == "accept"
    completed(step["method"], step["item"], step["output"] if allowed else "", allowed)
    agent("done")
    turn_end(step["thread"])


def turn(thread_id: str, text: str) -> None:
    global model
    item = f"item-{abs(hash(text)) % 100000}"
    if text.startswith("run:"):
        command = text.split(":", 1)[1]
        started("commandExecution", item, command)
        ask(
            f"approval-{item}",
            "item/commandExecution/requestApproval",
            command,
            {
                "method": "commandExecution",
                "item": item,
                "output": f"ran {command}",
                "thread": thread_id,
            },
        )
    elif text.startswith("read:"):
        path = text.split(":", 1)[1]
        started("tool", item, path)
        if path.startswith("/"):
            ask(
                f"approval-{item}",
                "item/tool/requestUserInput",
                path,
                {
                    "method": "tool",
                    "item": item,
                    "output": f"ran {path}",
                    "thread": thread_id,
                },
            )
        else:
            completed("tool", item, f"ran {path}", True)
            agent("done")
            turn_end(thread_id)
    elif text.startswith("fetch:"):
        url = text.split(":", 1)[1]
        started("tool", item, url)
        ask(
            f"approval-{item}",
            "item/tool/requestUserInput",
            url,
            {
                "method": "tool",
                "item": item,
                "output": f"fetched {url}",
                "thread": thread_id,
            },
        )
    elif text == "hostask":
        command = "systemctl restart nginx"
        started("commandExecution", item, command)
        ask(
            f"approval-{item}",
            "item/commandExecution/requestApproval",
            command,
            {
                "method": "commandExecution",
                "item": item,
                "output": f"ran {command}",
                "thread": thread_id,
            },
        )
    elif text == "fail":
        turn_end(thread_id, "400 invalid request")
    elif text == "slow":
        slow.add(thread_id)
    elif text == "ping":
        agent("pong")
        turn_end(thread_id)
    else:
        agent(f"{model}: {text}")
        turn_end(thread_id)


argv = sys.argv[1:]
append("launches.jsonl", argv)
append("env.jsonl", sorted(os.environ))
model = read_model()

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    try:
        msg = json.loads(line)
    except ValueError:
        continue
    method, rid, params = msg.get("method"), msg.get("id"), msg.get("params") or {}
    if method is None:
        # The engine's answer to an approval we asked for.
        append("responses.jsonl", msg)
        step = waiting.pop(str(rid), None)
        if step is not None:
            decision = (msg.get("result") or {}).get("decision", "decline")
            answer(decision, step)
        continue
    append("requests.jsonl", {"method": method, "params": params})
    if method == "initialize":
        send({"jsonrpc": "2.0", "id": rid, "result": {"userAgent": "fake-codex/0"}})
    elif method == "thread/start":
        time.sleep(0.3)
        thread_id = params.get("threadId", "t")
        threads.add(thread_id)
        if params.get("model"):
            model = str(params["model"])
            write_model(model)
        send({"jsonrpc": "2.0", "id": rid, "result": {"thread": {"id": thread_id}}})
    elif method == "thread/resume":
        time.sleep(0.3)
        thread_id = params.get("threadId", "t")
        threads.add(thread_id)
        send({"jsonrpc": "2.0", "id": rid, "result": {"thread": {"id": thread_id}}})
    elif method == "thread/settings/update":
        if params.get("model"):
            model = str(params["model"])
            write_model(model)
        send({"jsonrpc": "2.0", "id": rid, "result": {}})
    elif method == "turn/start":
        thread_id = params.get("threadId")
        if thread_id not in threads:
            send(
                {
                    "jsonrpc": "2.0",
                    "id": rid,
                    "error": {
                        "code": -32600,
                        "message": f"thread not found: {thread_id}",
                    },
                }
            )
            continue
        send({"jsonrpc": "2.0", "id": rid, "result": {"turn": {"id": "turn-1"}}})
        blocks = params.get("input") or []
        text = blocks[0].get("text", "") if blocks else ""
        turn(str(thread_id), str(text))
    elif method == "turn/interrupt":
        thread_id = params.get("threadId")
        send({"jsonrpc": "2.0", "id": rid, "result": {}})
        if thread_id in slow:
            slow.discard(thread_id)
            turn_end(str(thread_id), "interrupted")
