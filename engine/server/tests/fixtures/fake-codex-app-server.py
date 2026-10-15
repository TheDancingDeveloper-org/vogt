#!/usr/bin/env python3
"""A stand-in for `codex app-server --stdio` that enforces the ordering the
fork does: a `turn/start` for a thread the server has not started fails with
`thread not found` (WI-1641). `thread/start` answers after a short delay, as
the real server does while it builds the session, so a client that sends the
turn before the thread result reproduces the failure."""
import json
import sys
import threading
import time

lock = threading.Lock()
threads: set[str] = set()


def send(msg: dict) -> None:
    with lock:
        sys.stdout.write(json.dumps(msg) + "\n")
        sys.stdout.flush()


def start_thread(rid, params: dict) -> None:
    time.sleep(0.3)
    thread_id = params.get("threadId", "t")
    threads.add(thread_id)
    send({"jsonrpc": "2.0", "id": rid, "result": {"thread": {"id": thread_id}}})


for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    msg = json.loads(line)
    method, rid, params = msg.get("method"), msg.get("id"), msg.get("params") or {}
    if method == "initialize":
        send({"jsonrpc": "2.0", "id": rid, "result": {"userAgent": "fake-codex/0"}})
    elif method in ("thread/start", "thread/resume"):
        threading.Thread(target=start_thread, args=(rid, params), daemon=True).start()
    elif method == "turn/start":
        thread_id = params.get("threadId")
        if thread_id not in threads:
            send({"jsonrpc": "2.0", "id": rid,
                  "error": {"code": -32600, "message": f"thread not found: {thread_id}"}})
            continue
        send({"jsonrpc": "2.0", "id": rid, "result": {"turn": {"id": "turn-1"}}})
        send({"jsonrpc": "2.0", "method": "item/agentMessage/completed",
              "params": {"threadId": thread_id, "text": "pong"}})
        send({"jsonrpc": "2.0", "method": "turn/completed",
              "params": {"threadId": thread_id, "turn": {"id": "turn-1"}}})
