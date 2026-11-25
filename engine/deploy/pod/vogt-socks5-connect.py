#!/usr/bin/env python3
"""Relay stdin/stdout to HOST:PORT through a SOCKS5 proxy (no auth).

Used by vogt-tailnet-connect as an ssh ProxyCommand. The destination name is
sent to the proxy unresolved (atyp 3), so the tailnet resolves it, not the pod.
Exit 255 on any failure, as ssh expects of a failed ProxyCommand.
"""

import os
import socket
import sys
import threading
from typing import NoReturn

HOST, PORT = sys.argv[1], int(sys.argv[2])
PROXY = (sys.argv[3], int(sys.argv[4])) if len(sys.argv) > 4 else ("127.0.0.1", 1055)


def die(msg: str) -> NoReturn:
    sys.stderr.write(f"vogt-tailnet-connect: {msg}\n")
    sys.exit(255)


def recv_exact(sock: socket.socket, n: int) -> bytes:
    buf = b""
    while len(buf) < n:
        chunk = sock.recv(n - len(buf))
        if not chunk:
            die("proxy closed the connection during the SOCKS5 handshake")
        buf += chunk
    return buf


try:
    s = socket.create_connection(PROXY, timeout=10)
except OSError as exc:
    die(f"cannot reach the tailnet proxy {PROXY[0]}:{PROXY[1]}: {exc}")

try:
    s.sendall(b"\x05\x01\x00")
    if recv_exact(s, 2) != b"\x05\x00":
        die("proxy refused the no-auth SOCKS5 method")
    name = HOST.encode()
    if len(name) > 255:
        die("host name too long")
    s.sendall(b"\x05\x01\x00\x03" + bytes([len(name)]) + name + PORT.to_bytes(2, "big"))
    head = recv_exact(s, 4)
    if head[0] != 5 or head[1] != 0:
        die(f"proxy could not connect to {HOST}:{PORT} (SOCKS5 reply {head[1]})")
    atyp = head[3]
    if atyp == 3:
        addr_len = recv_exact(s, 1)[0]
    else:
        addr_len = {1: 4, 4: 16}.get(atyp)
        if addr_len is None:
            die(f"proxy sent an unknown SOCKS5 address type {atyp}")
    recv_exact(s, addr_len + 2)
except OSError as exc:
    die(f"handshake with the tailnet proxy failed: {exc}")
s.settimeout(None)


def pump_out() -> None:
    try:
        while True:
            data = s.recv(65536)
            if not data:
                break
            sys.stdout.buffer.write(data)
            sys.stdout.buffer.flush()
    except OSError:
        pass
    finally:
        sys.stdout.flush()
        os._exit(0)


threading.Thread(target=pump_out, daemon=True).start()
try:
    while True:
        data = sys.stdin.buffer.read1(65536)
        if not data:
            break
        s.sendall(data)
    s.shutdown(socket.SHUT_WR)
except OSError:
    pass
# stdin is done: give the far side a moment to drain, then leave.
threading.Event().wait(2)
