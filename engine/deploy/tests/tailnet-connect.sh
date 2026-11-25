#!/usr/bin/env bash
# WI-4716: vogt-tailnet-connect and tcurl, against a fake SOCKS5 listener.
#
#   - with no listener the helper exits 255 within 3s and says why
#   - with a listener it relays bytes both ways to the requested HOST:PORT,
#     sending the name unresolved (socks5h behaviour)
#   - tcurl asks curl for socks5h://127.0.0.1:1055 unless overridden
#   - the ssh_config snippet parses and selects the proxy for tailnet hosts only
set -euo pipefail

root="$(cd "$(dirname "$0")/../pod" && pwd)"
testdir="$(mktemp -d)"
server_pid=""
cleanup() {
    [[ -z "$server_pid" ]] || kill "$server_pid" 2>/dev/null || true
    rm -rf "$testdir"
}
trap cleanup EXIT
fail() { echo "FAIL: $*" >&2; exit 1; }

export VOGT_TAILNET_RELAY="$root/vogt-socks5-connect.py"

# A port with no listener: bind to get a free one, then close it.
free_port="$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1]); s.close()')"

# 1. no listener: 255, under 3s, a clear message.
start=$SECONDS
set +e
VOGT_TAILNET_SOCKS5="127.0.0.1:$free_port" "$root/vogt-tailnet-connect" 100.92.4.57 22 </dev/null 2>"$testdir/err"
rc=$?
set -e
elapsed=$((SECONDS - start))
[[ $rc -eq 255 ]] || fail "no listener: exit $rc, want 255"
[[ $elapsed -le 3 ]] || fail "no listener: took ${elapsed}s, want <= 3s"
grep -q "not answering" "$testdir/err" || fail "no listener: message missing: $(cat "$testdir/err")"

# 2. fake SOCKS5 proxy that records the request and echoes upper-cased data.
cat >"$testdir/socks.py" <<'PY'
import socket, sys
log, portfile = sys.argv[1:]
srv = socket.socket()
srv.bind(("127.0.0.1", 0))
srv.listen(8)
open(portfile, "w").write(str(srv.getsockname()[1]))
while True:
    c, _ = srv.accept()
    c.settimeout(5)
    try:
        hello = c.recv(3)
        if not hello:  # the helper's liveness probe
            continue
        c.sendall(b"\x05\x00")
        req = c.recv(262)
        n = req[4]
        host = req[5:5 + n].decode()
        port = int.from_bytes(req[5 + n:7 + n], "big")
        open(log, "a").write("atyp=%d host=%s port=%d\n" % (req[3], host, port))
        c.sendall(b"\x05\x00\x00\x01\x00\x00\x00\x00\x00\x00")
        while True:
            d = c.recv(4096)
            if not d:
                break
            c.sendall(d.upper())
    except OSError:
        pass
    finally:
        c.close()
PY
python3 "$testdir/socks.py" "$testdir/req.log" "$testdir/port" &
server_pid=$!
for _ in $(seq 50); do [[ -s "$testdir/port" ]] && break; sleep 0.1; done
[[ -s "$testdir/port" ]] || fail "fake SOCKS listener did not start"
port="$(cat "$testdir/port")"

out="$(printf 'hello tailnet' | VOGT_TAILNET_SOCKS5="127.0.0.1:$port" timeout 10 "$root/vogt-tailnet-connect" edgerouter.example.ts.net 22)" \
    || fail "relay: helper exited $?"
[[ "$out" == "HELLO TAILNET" ]] || fail "relay: got '$out'"
grep -qx 'atyp=3 host=edgerouter.example.ts.net port=22' "$testdir/req.log" \
    || fail "relay: proxy saw: $(cat "$testdir/req.log")"

# 3. tcurl passes the socks5h proxy to curl.
mkdir "$testdir/bin"
printf '#!/bin/sh\nprintf "%%s\\n" "$@"\n' >"$testdir/bin/curl"
chmod +x "$testdir/bin/curl"
args="$(PATH="$testdir/bin:$PATH" "$root/tcurl" -sS http://node-e:8120/)"
grep -qx 'socks5h://127.0.0.1:1055' <<<"$args" || fail "tcurl default proxy: $args"
args="$(VOGT_TAILNET_SOCKS5=127.0.0.1:9 PATH="$testdir/bin:$PATH" "$root/tcurl" http://x/)"
grep -qx 'socks5h://127.0.0.1:9' <<<"$args" || fail "tcurl override: $args"

# 4. the ssh_config snippet: proxy for tailnet hosts, none for others.
if command -v ssh >/dev/null; then
    cfg="$testdir/ssh_config"
    cp "$root/ssh_config-vogt-tailnet.conf" "$cfg"
    ssh_g() { ssh -F "$cfg" -G "$1" 2>/dev/null; }
    for h in edgerouter 100.92.4.57 100.127.1.1 box.tail1234.ts.net; do
        ssh_g "$h" | grep -q '^proxycommand /usr/local/bin/vogt-tailnet-connect ' || fail "ssh_config: $h has no tailnet proxy"
        ssh_g "$h" | grep -qx 'batchmode yes' || fail "ssh_config: $h not BatchMode"
        ssh_g "$h" | grep -qx 'connecttimeout 10' || fail "ssh_config: $h ConnectTimeout"
    done
    ssh_g edgerouter | grep -qx 'hostname 100.92.4.57' || fail "ssh_config: edgerouter HostName"
    for h in github.com 100.63.0.1 100.128.0.1 192.168.1.75 example.com; do
        ssh_g "$h" | grep -q '^proxycommand' && fail "ssh_config: $h must not use the tailnet proxy"
    done
else
    echo "note: ssh not installed; skipped the ssh_config check" >&2
fi

echo "ok: tailnet connect helpers"
