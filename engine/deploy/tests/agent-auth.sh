#!/usr/bin/env bash
# WI-3885: vogt-agent-auth on the paths that do not have an engine broker
# token, and the rule that no secret ever reaches a command's argv.
#
# A pod session holds VOGT_SESSION_TOKEN_FILE + VOGT_ENGINE_URL, not the
# engine-side broker pair. `fetch`, `store` and `grants` must use those the way
# `policy-hook.sh` already does, and must still refuse with no credential at
# all. The fake engine below receives the credential through a header read
# from a file descriptor and records what it saw.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
script="$root/agent-auth.sh"
dockerfile_pod="$(cd "$root/../.." && pwd)/engine/Dockerfile.pod"
testdir="$(mktemp -d)"
server_pid=""
cleanup() {
    [[ -z "$server_pid" ]] || kill "$server_pid" 2>/dev/null || true
    rm -rf "$testdir"
}
trap cleanup EXIT

fail() { echo "FAIL: $*" >&2; exit 1; }

cat >"$testdir/server.py" <<'PY'
import json, sys
from http.server import BaseHTTPRequestHandler, HTTPServer

log_path, port_path = sys.argv[1:]


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def record(self, body):
        with open(log_path, "a", encoding="utf-8") as fh:
            fh.write("%s|%s|%s\n" % (
                self.path, self.headers.get("Authorization", ""), body))

    def send(self, code, body):
        data = body.encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_POST(self):
        length = int(self.headers.get("Content-Length") or 0)
        body = self.rfile.read(length).decode("utf-8", "replace") if length else ""
        self.record(body)
        if self.path == "/api/agent-auth/fetch/DEMO":
            self.send(200, "demo-value")
        elif self.path == "/api/agent-auth/store/WRITABLE":
            self.send(200, "stored")
        elif self.path == "/api/agent-auth/grants":
            self.send(200, "[]")
        elif self.path == "/api/agent-auth/launch-report":
            self.send(200, "{}")
        elif self.path == "/api/mcp":
            self.send(200, '{"jsonrpc":"2.0","id":1,"result":{}}')
        elif self.path == "/api/v1/auth/universal-auth/login":
            try:
                payload = json.loads(body)
            except ValueError:
                self.send(400, "not json")
                return
            if payload.get("clientSecret") != "client-secret-value":
                self.send(401, "bad identity")
                return
            if payload.get("clientId") == "no-token-id":
                # A 200 that carries no token is a failed login, not a
                # credential with an empty value.
                self.send(200, "{}")
                return
            if payload.get("clientId") != "client-id":
                self.send(401, "bad identity")
                return
            self.send(200, '{"accessToken":"access-token-value"}')
        else:
            self.send(404, "not found")

    def do_GET(self):
        self.record("")
        if self.path == "/api/agent-auth/grants":
            self.send(200, "[]")
        else:
            self.send(404, "not found")


server = HTTPServer(("127.0.0.1", 0), Handler)
with open(port_path, "w", encoding="utf-8") as fh:
    fh.write(str(server.server_address[1]))
server.serve_forever()
PY

python3 "$testdir/server.py" "$testdir/requests.log" "$testdir/port" &
server_pid=$!
for _ in $(seq 1 100); do
    [[ -s "$testdir/port" ]] && break
    sleep 0.05
done
[[ -s "$testdir/port" ]] || fail "fake engine did not start"
port="$(cat "$testdir/port")"
base="http://127.0.0.1:$port"

token_file="$testdir/session-token"
printf 'session-token\n' >"$token_file"
chmod 600 "$token_file"

# 1. A pod session (no broker pair) fetches through VOGT_ENGINE_URL +
#    VOGT_SESSION_TOKEN_FILE. The engine sees that credential, not an empty one.
out="$(env -u VOGT_ENGINE_BROKER_URL -u VOGT_ENGINE_BROKER_TOKEN \
    -u VOGT_ENGINE_BROKER_TOKEN_FILE \
    VOGT_ENGINE_URL="$base" VOGT_SESSION_TOKEN_FILE="$token_file" \
    bash "$script" fetch DEMO)" || fail "pod fallback fetch failed"
[[ "$out" == "demo-value" ]] || fail "pod fallback fetch printed '$out'"

# 2. The same fallback powers `store` and `grants` (they share broker_curl).
out="$(printf 'written-value' | env -u VOGT_ENGINE_BROKER_URL \
    -u VOGT_ENGINE_BROKER_TOKEN -u VOGT_ENGINE_BROKER_TOKEN_FILE \
    VOGT_ENGINE_URL="$base" VOGT_SESSION_TOKEN_FILE="$token_file" \
    bash "$script" store WRITABLE)" || fail "pod fallback store failed"
[[ "$out" == "stored" ]] || fail "pod fallback store printed '$out'"

out="$(env -u VOGT_ENGINE_BROKER_URL -u VOGT_ENGINE_BROKER_TOKEN \
    -u VOGT_ENGINE_BROKER_TOKEN_FILE \
    VOGT_ENGINE_URL="$base" VOGT_SESSION_TOKEN_FILE="$token_file" \
    bash "$script" grants)" || fail "pod fallback grants failed"
[[ "$out" == "no approved grants for this session" ]] || fail "grants printed '$out'"

grep -q '^/api/agent-auth/fetch/DEMO|Bearer session-token|$' "$testdir/requests.log" \
    || fail "fetch did not present the stream credential"
grep -q '^/api/agent-auth/store/WRITABLE|Bearer session-token|written-value$' \
    "$testdir/requests.log" || fail "store did not present the stream credential"

# 3. Without any credential the helper still refuses, naming what is missing.
if env -u VOGT_ENGINE_BROKER_URL -u VOGT_ENGINE_BROKER_TOKEN \
    -u VOGT_ENGINE_BROKER_TOKEN_FILE -u VOGT_ENGINE_URL \
    -u VOGT_SESSION_TOKEN_FILE bash "$script" fetch DEMO \
    >/dev/null 2>"$testdir/noauth.err"; then
    fail "fetch without any credential should fail"
fi
grep -q 'no secret broker in this environment' "$testdir/noauth.err" \
    || fail "missing-broker message: $(cat "$testdir/noauth.err")"

# 4. The universal-auth login carries the client secret in the body, not argv,
#    and still yields the access token. A jq shim records jq's real argv, the
#    way the reviewer found the `--arg secret "$..."` leak with `pgrep -a jq`.
shimdir="$testdir/shim"
mkdir -p "$shimdir"
real_jq="$(command -v jq)"
cat >"$shimdir/jq" <<EOF
#!/usr/bin/env bash
printf '%s\n' "\$*" >>"$testdir/jq-argv.log"
exec "$real_jq" "\$@"
EOF
chmod +x "$shimdir/jq"

# shellcheck disable=SC2016 # the sourced shell expands $1, not this one
out="$(env -u VOGT_ENGINE_BROKER_URL -u VOGT_ENGINE_BROKER_TOKEN \
    -u VOGT_ENGINE_BROKER_TOKEN_FILE -u VOGT_SESSION_TOKEN_FILE \
    PATH="$shimdir:$PATH" \
    INFISICAL_API_URL="$base/api" INFISICAL_CLIENT_ID=client-id \
    INFISICAL_CLIENT_SECRET=client-secret-value \
    bash -c 'source "$1"; mint_access_token' _ "$script")" \
    || fail "mint_access_token failed"
[[ "$out" == "access-token-value" ]] || fail "mint_access_token printed '$out'"
grep -q '^/api/v1/auth/universal-auth/login|.*client-secret-value' \
    "$testdir/requests.log" || fail "login body did not carry the client secret"
[[ -s "$testdir/jq-argv.log" ]] || fail "jq shim observed no invocation"
if grep -q 'client-secret-value' "$testdir/jq-argv.log"; then
    fail "client secret appeared in jq argv: $(cat "$testdir/jq-argv.log")"
fi

# 4b. A login response with a 200 and no access token is a failure, so the
#     callers fall back or die instead of using an empty credential.
# shellcheck disable=SC2016 # the sourced shell expands $1, not this one
if env -u VOGT_ENGINE_BROKER_URL -u VOGT_ENGINE_BROKER_TOKEN \
    -u VOGT_ENGINE_BROKER_TOKEN_FILE -u VOGT_SESSION_TOKEN_FILE \
    INFISICAL_API_URL="$base/api" INFISICAL_CLIENT_ID=no-token-id \
    INFISICAL_CLIENT_SECRET=client-secret-value \
    bash -c 'source "$1"; mint_access_token' _ "$script" \
    >"$testdir/no-token.out" 2>&1; then
    fail "mint_access_token succeeded without an access token: $(cat "$testdir/no-token.out")"
fi
if [[ -s "$testdir/no-token.out" ]]; then
    fail "mint_access_token printed an empty token: $(cat "$testdir/no-token.out")"
fi

# 5. A probe's bearer reaches curl through a descriptor, never argv.
# shellcheck disable=SC2016 # the sourced shell expands $1 and $2, not this one
out="$(env -u VOGT_ENGINE_BROKER_URL -u VOGT_ENGINE_BROKER_TOKEN \
    -u VOGT_ENGINE_BROKER_TOKEN_FILE -u VOGT_SESSION_TOKEN_FILE \
    bash -c 'source "$1"
        response="$(mktemp)"; error="$(mktemp)"
        probe_mcp "Demo MCP" "$2/api/mcp" probe-token "Demo token" "$response" "$error" ""
        rm -f "$response" "$error"' _ "$script" "$base")" \
    || fail "probe_mcp failed"
[[ "$out" == "ok: Demo MCP ($base/api/mcp)" ]] || fail "probe printed '$out'"
grep -q '^/api/mcp|Bearer probe-token|' "$testdir/requests.log" \
    || fail "probe did not present its token"

# 6. No secret variable is expanded into a command's argv anywhere in the
#    helper. Comments may name the retired flags; command lines may not use
#    them. That includes `jq --arg name "$SECRET"` (the reviewer's `pgrep -a
#    jq` leak) and a secret interpolated into a single-quoted jq program.
#    curl headers go through @<(printf ...).
secret_var='[A-Za-z_][A-Za-z0-9_]*(secret|token|password|passwd|credential|api_?key|private_key)'
leaks="$(grep -vE '^[[:space:]]*#' "$script" \
    | grep -niE -- '--client-secret|--token([= ]|$)|-H "Authorization|=[$]value|--arg(json)?[[:space:]]+[A-Za-z_][A-Za-z0-9_]*[[:space:]]+"\$\{?'"$secret_var" || true)"
[[ -z "$leaks" ]] || fail "secret expanded into argv:
$leaks"
jq_leaks="$(grep -vE '^[[:space:]]*#' "$script" \
    | grep -nE "jq[^']*'[^']*\\\$\\{?${secret_var}" || true)"
[[ -z "$jq_leaks" ]] || fail "secret interpolated into a jq program:
$jq_leaks"
grep -q -- '-H @<(printf .Authorization: Bearer' "$script" \
    || fail "curl no longer reads the bearer from a descriptor"

# 7. The pod image ships the helper the session needs (WI-3243 did the COPY;
#    WI-3885 keeps it from regressing).
[[ -f "$dockerfile_pod" ]] || fail "missing $dockerfile_pod"
grep -q '^COPY --chmod=0755 deploy/agent-auth.sh /usr/local/bin/vogt-agent-auth$' \
    "$dockerfile_pod" || fail "Dockerfile.pod does not install vogt-agent-auth"

echo "agent-auth: ok"
